//! The resolver: hickory-server on UDP and TCP, driven by an actor (docs/03 §7.2).
//!
//! * [`DnsServer::spawn`] starts the actor `dns-server` under an [`ActorSupervisor`];
//!   nothing listens until [`DnsServer::start`].
//! * `start` binds the first free address of [`DnsServerConfig::listen`]: by default
//!   `127.0.0.1:53`, then `127.0.0.53:53` when the first is taken (ADR-0004: an NRPT rule
//!   takes an IP, never a port, so the fallback is another loopback address). UDP and TCP
//!   must both bind on the same address. A candidate with port 0 takes an ephemeral port
//!   (tests).
//! * `reload` swaps the zones at once ([`tokio::sync::watch`]): queries in flight finish
//!   with the old set, the next ones see the new one. No socket is touched.
//! * `stats` reports the address, whether an NRPT rule can point at it (port 53) and
//!   query counters.
//! * `stop` cancels the hickory tasks and waits for them (bounded).
//!
//! Zones, counters and the "should be running" flag live outside the actor, so a panic
//! (caught by the supervisor) loses nothing: the new instance re-binds when the server was
//! running. Every request to the actor goes through `ask` with a timeout.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use forge_core::actor::{
    Actor, ActorHandle, ActorRef, ActorSpec, ActorSupervisor, Context, Reply, RestartInfo,
};
use forge_core::error::{ForgeError, Result};
use hickory_server::Server;
use hickory_server::net::runtime::Time;
use hickory_server::proto::op::{
    Edns, Header, HeaderCounts, MessageType, Metadata, OpCode, ResponseCode,
};
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::zone_handler::MessageResponseBuilder;
use serde::Serialize;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::zones::ZoneSet;

/// Timeout of every request to the actor (ADR-0011).
const ASK_TIMEOUT: Duration = Duration::from_secs(5);
/// `start` binds sockets and may wait for a previous stop: a little more room.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// `127.0.0.1:53` then `127.0.0.53:53` (ADR-0004).
pub const STANDARD_LISTEN: [SocketAddr; 2] = [
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 53),
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 53)), 53),
];

/// Where and how the resolver listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsServerConfig {
    /// Candidate addresses, in order of preference.
    pub listen: Vec<SocketAddr>,
    /// A TCP client that sends nothing for this long is dropped.
    pub tcp_timeout: Duration,
    /// Budget of a stop (hickory tasks winding down).
    pub stop_timeout: Duration,
}

impl Default for DnsServerConfig {
    fn default() -> Self {
        Self::on(STANDARD_LISTEN.to_vec())
    }
}

impl DnsServerConfig {
    /// Listens on the first free address of `listen`.
    pub fn on(listen: Vec<SocketAddr>) -> Self {
        Self {
            listen,
            tcp_timeout: Duration::from_secs(5),
            stop_timeout: Duration::from_secs(3),
        }
    }
}

/// What the resolver is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DnsStats {
    pub running: bool,
    /// Bound address while running.
    pub listen: Option<SocketAddr>,
    /// The resolver listens on port 53 of a loopback address: an NRPT rule can send the
    /// zones to it. Otherwise only the `hosts` fallback works.
    pub nrpt_capable: bool,
    /// Zone names, most specific first.
    pub zones: Vec<String>,
    /// Questions received since the actor was created.
    pub queries: u64,
    /// NOERROR with at least one record.
    pub answered: u64,
    /// NOERROR without records (type not served in the zone).
    pub nodata: u64,
    /// Names outside the zones.
    pub nxdomain: u64,
    /// FORMERR, NOTIMP, REFUSED and failures to send.
    pub errors: u64,
    /// Why the last start failed or the last run ended, if so.
    pub last_error: Option<String>,
}

#[derive(Debug, Default)]
struct Counters {
    queries: AtomicU64,
    answered: AtomicU64,
    nodata: AtomicU64,
    nxdomain: AtomicU64,
    errors: AtomicU64,
}

/// State that survives actor restarts.
struct Shared {
    config: DnsServerConfig,
    zones: watch::Sender<Arc<ZoneSet>>,
    counters: Arc<Counters>,
    want_running: AtomicBool,
    /// Numbers each run, across actor instances (a late `Ended` never matches a newer run).
    generation: AtomicU64,
}

/// The hickory side: answers from the current zones.
struct Handler {
    zones: watch::Receiver<Arc<ZoneSet>>,
    counters: Arc<Counters>,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

#[async_trait::async_trait]
impl RequestHandler for Handler {
    async fn handle_request<R: ResponseHandler, T: Time>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        bump(&self.counters.queries);
        let edns = request.edns.as_ref().map(|req| {
            let mut edns = Edns::new();
            edns.set_max_payload(req.max_payload().max(512));
            edns.set_version(0);
            edns
        });
        let builder = MessageResponseBuilder::new(&request.queries, edns.as_ref());
        let mut metadata = Metadata::response_from_request(&request.metadata);
        let questions = request.queries.queries();
        let refusal = if request.metadata.message_type != MessageType::Query {
            Some(ResponseCode::FormErr)
        } else if request.metadata.op_code != OpCode::Query {
            Some(ResponseCode::NotImp)
        } else if questions.len() != 1 {
            Some(ResponseCode::FormErr)
        } else {
            None
        };
        let sent = if let Some(code) = refusal {
            bump(&self.counters.errors);
            let response = builder.error_msg(&request.metadata, code);
            response_handle.send_response(response).await
        } else {
            // Clone the Arc and release the watch guard before any await.
            let zones = Arc::clone(&self.zones.borrow());
            let answer = zones.answer(questions[0].original());
            match answer.code {
                ResponseCode::NoError if answer.answers.is_empty() => bump(&self.counters.nodata),
                ResponseCode::NoError => bump(&self.counters.answered),
                ResponseCode::NXDomain => bump(&self.counters.nxdomain),
                _ => bump(&self.counters.errors),
            }
            metadata.authoritative = answer.authoritative;
            metadata.response_code = answer.code;
            let response = builder.build(
                metadata,
                answer.answers.iter(),
                std::iter::empty(),
                answer.soa.iter(),
                std::iter::empty(),
            );
            response_handle.send_response(response).await
        };
        match sent {
            Ok(info) => info,
            Err(err) => {
                bump(&self.counters.errors);
                tracing::debug!(%err, "dns: could not send a response");
                metadata.response_code = ResponseCode::ServFail;
                ResponseInfo::from(Header {
                    metadata,
                    counts: HeaderCounts::default(),
                })
            }
        }
    }
}

/// A bound, serving resolver.
struct Running {
    addr: SocketAddr,
    token: CancellationToken,
    /// `None` once a graceful stop took it to wait on it.
    task: Option<JoinHandle<()>>,
    generation: u64,
}

impl Drop for Running {
    /// An instance dropped without a graceful stop (panic caught by the supervisor)
    /// must not leave the hickory tasks serving and holding the sockets: cancel them and
    /// abort the task that owns the server (its `JoinSet` aborts the listeners).
    fn drop(&mut self) {
        self.token.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

enum Msg {
    Start {
        reply: Reply<Result<SocketAddr>>,
    },
    Stop {
        reply: Reply<()>,
    },
    Reload {
        zones: ZoneSet,
        reply: Reply<()>,
    },
    Stats {
        reply: Reply<DnsStats>,
    },
    /// The hickory tasks of `generation` ended on their own.
    Ended {
        generation: u64,
        error: Option<String>,
    },
    /// Test only: makes the actor panic (supervisor restart path).
    #[cfg(test)]
    Panic,
}

struct DnsActor {
    shared: Arc<Shared>,
    running: Option<Running>,
    last_error: Option<String>,
}

/// Binds UDP and TCP on `addr` (same port on both; port 0 picks one free for both).
async fn bind(addr: SocketAddr) -> std::io::Result<(UdpSocket, TcpListener)> {
    let attempts = if addr.port() == 0 { 16 } else { 1 };
    let mut last = None;
    for _ in 0..attempts {
        let udp = UdpSocket::bind(addr).await?;
        let port = udp.local_addr()?.port();
        match TcpListener::bind(SocketAddr::new(addr.ip(), port)).await {
            Ok(tcp) => return Ok((udp, tcp)),
            Err(err) => last = Some(err),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free port")))
}

impl DnsActor {
    async fn start(&mut self, ctx: &mut Context<Self>) -> Result<SocketAddr> {
        if let Some(running) = &self.running {
            return Ok(running.addr);
        }
        let mut failures = Vec::new();
        for candidate in &self.shared.config.listen {
            match bind(*candidate).await {
                Ok((udp, tcp)) => {
                    let addr = udp
                        .local_addr()
                        .map_err(|e| ForgeError::io("dns socket", e))?;
                    self.serve(ctx, udp, tcp, addr);
                    self.shared.want_running.store(true, Ordering::Relaxed);
                    self.last_error = None;
                    if !failures.is_empty() {
                        tracing::info!(%addr, skipped = ?failures, "dns: preferred address taken, using fallback");
                    }
                    tracing::info!(%addr, zones = ?self.shared.zones.borrow().names(), "dns: listening");
                    return Ok(addr);
                }
                Err(err) => failures.push(format!("{candidate}: {err}")),
            }
        }
        let message = format!("no address to listen on: {}", failures.join("; "));
        self.last_error = Some(message.clone());
        let port = self
            .shared
            .config
            .listen
            .first()
            .map_or(53, SocketAddr::port);
        Err(ForgeError::PortInUse {
            port,
            owner: Some(message),
            pid: None,
            exe: None,
        })
    }

    fn serve(
        &mut self,
        ctx: &mut Context<Self>,
        udp: UdpSocket,
        tcp: TcpListener,
        addr: SocketAddr,
    ) {
        let handler = Handler {
            zones: self.shared.zones.subscribe(),
            counters: Arc::clone(&self.shared.counters),
        };
        let mut server = Server::new(handler);
        server.register_socket(udp);
        server.register_listener(tcp, self.shared.config.tcp_timeout, 64);
        let token = server.shutdown_token().clone();
        let generation = self.shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let myself = ctx.myself_weak();
        let task = tokio::spawn(async move {
            let error = server.block_until_done().await.err().map(|e| e.to_string());
            if let Some(actor) = myself.upgrade() {
                let _ = actor.tell(Msg::Ended { generation, error });
            }
        });
        self.running = Some(Running {
            addr,
            token,
            task: Some(task),
            generation,
        });
    }

    async fn stop(&mut self) {
        let Some(mut running) = self.running.take() else {
            return;
        };
        running.token.cancel();
        if let Some(task) = running.task.take() {
            let abort = task.abort_handle();
            if tokio::time::timeout(self.shared.config.stop_timeout, task)
                .await
                .is_err()
            {
                tracing::warn!(addr = %running.addr, "dns: server did not stop in time; aborted");
                abort.abort();
            }
        }
        tracing::info!(addr = %running.addr, "dns: stopped");
    }

    fn stats(&self) -> DnsStats {
        let c = &self.shared.counters;
        let listen = self.running.as_ref().map(|r| r.addr);
        DnsStats {
            running: listen.is_some(),
            listen,
            nrpt_capable: listen.is_some_and(|a| a.port() == 53 && a.ip().is_loopback()),
            zones: self.shared.zones.borrow().names(),
            queries: c.queries.load(Ordering::Relaxed),
            answered: c.answered.load(Ordering::Relaxed),
            nodata: c.nodata.load(Ordering::Relaxed),
            nxdomain: c.nxdomain.load(Ordering::Relaxed),
            errors: c.errors.load(Ordering::Relaxed),
            last_error: self.last_error.clone(),
        }
    }
}

impl Actor for DnsActor {
    type Msg = Msg;

    async fn handle(&mut self, msg: Msg, ctx: &mut Context<Self>) {
        match msg {
            Msg::Start { reply } => {
                let result = self.start(ctx).await;
                let _ = reply.send(result);
            }
            Msg::Stop { reply } => {
                self.shared.want_running.store(false, Ordering::Relaxed);
                self.stop().await;
                let _ = reply.send(());
            }
            Msg::Reload { zones, reply } => {
                tracing::info!(zones = ?zones.names(), "dns: zones reloaded");
                self.shared.zones.send_replace(Arc::new(zones));
                let _ = reply.send(());
            }
            Msg::Stats { reply } => {
                let _ = reply.send(self.stats());
            }
            #[cfg(test)]
            Msg::Panic => panic!("dns actor panic requested by a test"),
            Msg::Ended { generation, error } => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|r| r.generation == generation)
                {
                    let message = error.unwrap_or_else(|| "the server tasks ended".to_owned());
                    tracing::warn!(error = %message, "dns: server ended unexpectedly");
                    self.running = None;
                    self.last_error = Some(message);
                }
            }
        }
    }

    async fn on_restart(&mut self, ctx: &mut Context<Self>, info: &RestartInfo) {
        if self.shared.want_running.load(Ordering::Relaxed) {
            tracing::warn!(reason = %info.reason, "dns: actor restarted, listening again");
            if let Err(err) = self.start(ctx).await {
                tracing::error!(%err, "dns: could not listen again after a restart");
            }
        }
    }

    async fn on_stop(&mut self, _ctx: &mut Context<Self>) {
        self.stop().await;
    }
}

/// Handle of the resolver actor. Cheap to clone.
#[derive(Clone)]
pub struct DnsServer {
    actor: ActorRef<Msg>,
    handle: Arc<ActorHandle<Msg>>,
    zones: watch::Receiver<Arc<ZoneSet>>,
}

impl std::fmt::Debug for DnsServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DnsServer")
            .field("zones", &self.zones.borrow().names())
            .finish_non_exhaustive()
    }
}

impl DnsServer {
    /// Starts the actor `dns-server` (not listening yet) with `zones`.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn spawn(supervisor: &ActorSupervisor, config: DnsServerConfig, zones: ZoneSet) -> Self {
        let (zones_tx, zones_rx) = watch::channel(Arc::new(zones));
        let shared = Arc::new(Shared {
            config,
            zones: zones_tx,
            counters: Arc::new(Counters::default()),
            want_running: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        });
        let stop_timeout = shared.config.stop_timeout + Duration::from_secs(1);
        let handle = supervisor.spawn(
            ActorSpec::new("dns-server")
                .mailbox(64)
                .stop_timeout(stop_timeout),
            move || DnsActor {
                shared: Arc::clone(&shared),
                running: None,
                last_error: None,
            },
        );
        Self {
            actor: handle.actor_ref().clone(),
            handle: Arc::new(handle),
            zones: zones_rx,
        }
    }

    /// Listens on the first free candidate address and returns it (the current address
    /// when already listening). `port_in_use` when every candidate is taken.
    pub async fn start(&self) -> Result<SocketAddr> {
        self.actor
            .ask(|reply| Msg::Start { reply }, START_TIMEOUT)
            .await?
    }

    /// Stops listening (the actor stays, ready to start again).
    pub async fn stop(&self) -> Result<()> {
        let budget = START_TIMEOUT;
        Ok(self.actor.ask(|reply| Msg::Stop { reply }, budget).await?)
    }

    /// Replaces the zones; the next question already sees them.
    pub async fn reload(&self, zones: ZoneSet) -> Result<()> {
        Ok(self
            .actor
            .ask(|reply| Msg::Reload { zones, reply }, ASK_TIMEOUT)
            .await?)
    }

    pub async fn stats(&self) -> Result<DnsStats> {
        Ok(self
            .actor
            .ask(|reply| Msg::Stats { reply }, ASK_TIMEOUT)
            .await?)
    }

    /// The zones being answered now (no round trip to the actor).
    pub fn zones(&self) -> Arc<ZoneSet> {
        Arc::clone(&self.zones.borrow())
    }

    /// Stops the actor for good (graceful, bounded).
    pub async fn shutdown(&self) {
        self.handle.stop().await;
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use forge_core::actor::ActorStatus;
    use forge_core::event::EventBus;

    use super::*;
    use crate::dns::client::resolve;

    #[tokio::test]
    async fn a_restarted_actor_listens_again_on_the_same_address() {
        // A port usable for UDP and TCP right now (an ephemeral UDP port may fall in a TCP
        // range Hyper-V excludes): found by a throwaway server on port 0.
        let supervisor = ActorSupervisor::new(EventBus::new());
        let probe = DnsServer::spawn(
            &supervisor,
            DnsServerConfig::on(vec![(Ipv4Addr::LOCALHOST, 0).into()]),
            ZoneSet::new(),
        );
        let addr = probe.start().await.unwrap();
        probe.shutdown().await;
        let server = DnsServer::spawn(
            &supervisor,
            DnsServerConfig::on(vec![addr]),
            ZoneSet::local(&[".test".into()]).unwrap(),
        );
        assert_eq!(server.start().await.unwrap(), addr);
        server.actor.tell(Msg::Panic).ok().unwrap();
        // The old instance is dropped (its sockets released) and the new one re-binds.
        let mut stats = server.stats().await;
        for _ in 0..50 {
            if stats.as_ref().is_ok_and(|s| s.running) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            stats = server.stats().await;
        }
        let stats = stats.unwrap();
        assert!(stats.running, "{stats:?}");
        assert_eq!(
            stats.listen,
            Some(addr),
            "same address: the old sockets were released"
        );
        assert!(matches!(server.handle.status(), ActorStatus::Running));
        let ips = resolve(addr, "loja.test", Duration::from_secs(3))
            .await
            .unwrap();
        assert!(ips.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST)));

        // After an explicit stop, a restart does not listen again.
        server.stop().await.unwrap();
        server.actor.tell(Msg::Panic).ok().unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!server.stats().await.unwrap().running);
        server.shutdown().await;
    }
}
