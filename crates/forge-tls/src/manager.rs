//! The `tls` actor and its handle [`TlsManager`] (docs/03 §6-§7, docs/06 §6, ADR-0023).
//!
//! The actor owns the CA (with its key, in memory only), the site certificates found in
//! `certs/sites/` and the trust ledger; it serializes every change to those files, so two
//! issues of the same host or an issue during a rotation never interleave. Key generation,
//! signing and file I/O run in `spawn_blocking` with a timeout; nothing inside the actor
//! waits for a human.
//!
//! Operations that wait for a human (the Windows confirmation dialog of `CurrentUser\Root`,
//! UAC for `LocalMachine\Root`) run in the caller's task, outside the actor, with
//! [`TlsConfig::trust_timeout`]: [`TlsManager::install_ca`], [`TlsManager::uninstall_ca`]
//! and the trust steps of [`TlsManager::rotate_ca`]. The daemon runs those as jobs
//! (`tls.ca_install`, `tls.ca_rotate` are `job` methods, docs/09).
//!
//! Rotation is two-phase so that declining the dialog changes nothing: the actor prepares
//! the new CA in memory, the caller trusts it where the old one was trusted, then the actor
//! commits (writes the CA, re-issues every leaf) and finally the caller removes the old
//! root. Removal failures are warnings; the ledger keeps the old thumbprint for a later
//! clean-up.
//!
//! A scheduler task (daily by default, ±10 % jitter, first run shortly after start) sends
//! "renew what is due": leaves with less than 30 days left, signed by another CA, or whose
//! key file does not match. Events: `tls.ca_created`, `tls.leaf_issued`, `tls.renewed`
//! (payload [`TlsEventPayload`], same shape as `forge_ipc::api::events::TlsPayload`), and
//! `helper.op` for `LocalMachine` changes. The web server reload after a renewal is the
//! daemon's reaction to `tls.renewed`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use forge_core::Timestamp;
use forge_core::actor::{Actor, ActorHandle, ActorRef, ActorSpec, ActorSupervisor, Context, Reply};
use forge_core::error::{ForgeError, Result};
use forge_core::event::{EventBus, EventKind};
use forge_core::ids::SiteId;
use forge_core::paths::ForgeRoot;
use forge_core::platform::{CertThumbprint, FileAcl, TrustScope, TrustStore};
use forge_core::runtime::spawn_blocking_timeout;
use jiff::SignedDuration;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::ca::{
    BACKDATE, CA_VALIDITY, CaInfo, CaOptions, LocalCa, default_common_name, machine_name,
};
use crate::crl::{CA_CRL_FILE, CRL_VALIDITY, CrlDocument, build_crl, crl_url};
use crate::leaf::{
    LEAF_VALIDITY, LeafInfo, LeafRequest, RENEW_BEFORE, StoredLeaf, check_against_ca, issue_leaf,
    scan_leaves,
};
use crate::messages;
use crate::trust::{TrustEntry, TrustLedger};

/// Version of the `tls.*` payloads.
pub const TLS_EVENT_VERSION: u32 = 1;
/// The UI warns this long before the CA expires (docs/06 §6.1).
pub const CA_WARN_BEFORE: SignedDuration = SignedDuration::from_hours(90 * 24);

/// Payload of `tls.ca_created`, `tls.leaf_issued` and `tls.renewed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsEventPayload {
    pub v: u32,
    /// Leaf host; `None` for the CA.
    pub host: Option<String>,
    pub site_id: Option<SiteId>,
    pub thumbprint_sha256: String,
    pub not_after: Timestamp,
}

#[derive(Serialize)]
struct HelperOpEvent<'a> {
    v: u32,
    name: &'a str,
    ok: bool,
    error: Option<String>,
}

/// Settings of the TLS manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsConfig {
    /// Configured TLDs (`ForgeConfig::tlds()`), for the CA name constraints.
    pub tlds: Vec<String>,
    /// Common name of new CAs; `None`: `Forgebox Local CA <hostname> <year>`.
    pub ca_common_name: Option<String>,
    pub ca_validity: SignedDuration,
    pub leaf_validity: SignedDuration,
    /// Leaves with less than this left are renewed.
    pub renew_before: SignedDuration,
    /// First automatic renewal check after the start.
    pub first_check_delay: Duration,
    /// Interval of the automatic renewal checks (±10 % jitter).
    pub check_interval: Duration,
    /// Budget of one blocking step (key generation, signing, a few files).
    pub io_timeout: Duration,
    /// Budget of the steps that write many files (rotation, renewing everything).
    pub bulk_timeout: Duration,
    /// Budget of a trust store change (dialog or UAC waiting for a human).
    pub trust_timeout: Duration,
    /// Whether the OS trust store may be changed (`false` in portable mode, docs/08 §2.2).
    pub os_trust: bool,
    /// Loopback address of the CRL responder (`crate::crl`). Leaves carry
    /// `http://<it>/forgebox/<CA SHA-1>.crl` as distribution point; `None`: no distribution
    /// point and no responder (Windows `curl.exe` then refuses the certificates unless
    /// told not to check revocation).
    pub crl_listen: Option<SocketAddr>,
    /// `nextUpdate` of the served CRL.
    pub crl_validity: SignedDuration,
}

/// Named port range (`forgebox.toml` `[ports.ranges]`) whose first port the CRL responder
/// uses.
pub const CRL_PORT_RANGE: &str = "tls-crl";

impl TlsConfig {
    /// Defaults of docs/06 §6 for `tlds`.
    pub fn new(tlds: Vec<String>) -> Self {
        Self {
            tlds,
            ca_common_name: None,
            ca_validity: CA_VALIDITY,
            leaf_validity: LEAF_VALIDITY,
            renew_before: RENEW_BEFORE,
            first_check_delay: Duration::from_secs(60),
            check_interval: Duration::from_secs(24 * 60 * 60),
            io_timeout: Duration::from_secs(10),
            bulk_timeout: Duration::from_secs(120),
            trust_timeout: Duration::from_secs(15 * 60),
            os_trust: true,
            crl_listen: None,
            crl_validity: CRL_VALIDITY,
        }
    }

    /// Defaults for `root` (portable mode: no OS trust store) and its `forgebox.toml`: the
    /// CRL responder on `127.0.0.1:<first port of [ports.ranges] tls-crl>`.
    pub fn for_root(root: &ForgeRoot, config: &forge_core::config::ForgeConfig) -> Self {
        let mut out = Self::new(config.tlds());
        out.os_trust = !root.is_portable();
        out.crl_listen = config
            .ports
            .range(CRL_PORT_RANGE)
            .and_then(|range| range.ports().next())
            .map(|port| SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        out
    }
}

/// Which leaves [`TlsManager::renew`] re-issues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenewScope {
    /// Less than `renew_before` left, another CA, or a key that does not match.
    Due,
    /// One host, whatever its state.
    Host(String),
    /// Every leaf.
    All,
}

/// A leaf and how it stands against the current CA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeafStatus {
    #[serde(flatten)]
    pub info: LeafInfo,
    /// Carries a valid signature of the current CA.
    pub signed_by_ca: bool,
    /// The key file exists and matches.
    pub key_ok: bool,
    /// Would be renewed by the next automatic check.
    pub due: bool,
}

/// Answer of [`TlsManager::status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TlsStatus {
    pub ca: Option<CaInfo>,
    /// Why the CA files could not be used (rotation recreates them).
    pub ca_error: Option<String>,
    pub trusted_current_user: bool,
    pub trusted_local_machine: bool,
    /// Why the trust store could not be read.
    pub trust_error: Option<String>,
    pub ca_expires_soon: bool,
    /// Configured TLDs the CA cannot sign for.
    pub missing_tlds: Vec<String>,
    /// Roots Forgebox added for previous CAs and still recorded (uninstall removes them).
    pub stale_roots: Vec<TrustEntry>,
    pub leaves: Vec<LeafStatus>,
    /// Unreadable files in `certs/sites/`.
    pub problems: Vec<String>,
    pub crl: CrlStatus,
    /// pt-BR hints for the user.
    pub warnings: Vec<String>,
}

/// The CRL responder (see [`crate::crl`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CrlStatus {
    /// Distribution point of the current CA's leaves.
    pub url: Option<String>,
    /// The responder is listening.
    pub serving: bool,
    pub next_update: Option<Timestamp>,
    /// Why it does not listen (port taken...).
    pub error: Option<String>,
}

/// Answer of [`TlsManager::install_ca`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallReport {
    pub ca: CaInfo,
    pub scope: TrustScope,
    /// `false` when it was already trusted (no dialog shown).
    pub added: bool,
}

/// Answer of [`TlsManager::rotate_ca`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RotationReport {
    pub ca: CaInfo,
    pub previous: Option<CaInfo>,
    /// Leaves re-issued under the new CA.
    pub leaves: Vec<LeafInfo>,
    /// Leaves that could not be re-issued (`host: reason`).
    pub skipped: Vec<String>,
    /// Scopes where the new CA was trusted (those where the previous one was).
    pub trusted: Vec<TrustScope>,
    /// Scopes the previous CA was removed from.
    pub removed_previous: Vec<TrustScope>,
    pub warnings: Vec<String>,
}

/// Certificate and key of a site (`tls.export`, admin only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafExport {
    pub host: String,
    pub cert_pem: String,
    pub key_pem: String,
}

/// What the actor knows, for [`TlsManager::status`].
#[derive(Debug, Clone)]
struct Snapshot {
    ca: Option<CaInfo>,
    ca_pem: Option<String>,
    ca_error: Option<String>,
    leaves: Vec<LeafStatus>,
    problems: Vec<String>,
    ledger: TrustLedger,
    crl: CrlStatus,
}

/// A CA prepared by phase one of a rotation.
#[derive(Debug, Clone)]
struct Prepared {
    token: u64,
    ca: CaInfo,
    der: Vec<u8>,
    previous: Option<CaInfo>,
}

/// Result of phase two.
#[derive(Debug, Clone)]
struct Committed {
    ca: CaInfo,
    leaves: Vec<LeafInfo>,
    skipped: Vec<String>,
}

/// The CA certificate (created when missing).
#[derive(Debug, Clone)]
struct CaMaterial {
    info: CaInfo,
    der: Vec<u8>,
}

enum LedgerChange {
    Record(TrustEntry),
    Forget(CertThumbprint, TrustScope),
}

enum Msg {
    Snapshot {
        reply: Reply<Snapshot>,
    },
    EnsureCa {
        reply: Reply<Result<CaMaterial>>,
    },
    Issue {
        request: LeafRequest,
        reply: Reply<Result<LeafInfo>>,
    },
    Renew {
        scope: RenewScope,
        reply: Option<Reply<Result<Vec<LeafInfo>>>>,
    },
    PrepareRotation {
        reply: Reply<Result<Prepared>>,
    },
    CommitRotation {
        token: u64,
        reply: Reply<Result<Committed>>,
    },
    AbortRotation {
        token: u64,
    },
    Ledger {
        change: LedgerChange,
        reply: Reply<Result<()>>,
    },
    ExportLeaf {
        host: String,
        reply: Reply<Result<LeafExport>>,
    },
    LeafChain {
        host: String,
        reply: Reply<Result<(Vec<u8>, Vec<u8>)>>,
    },
    Reload {
        reply: Reply<()>,
    },
    /// Test only: makes the actor panic (supervisor restart path).
    #[cfg(test)]
    Panic,
}

/// State shared by every instance of the actor and the handle.
struct Shared {
    certs_ca: PathBuf,
    certs_sites: PathBuf,
    config: TlsConfig,
    acl: Arc<dyn FileAcl>,
    rotations: AtomicU64,
    /// The CRL being served (survives actor restarts).
    crl: watch::Sender<Option<CrlDocument>>,
}

/// A leaf known to the actor.
#[derive(Debug, Clone)]
struct LeafEntry {
    stored: StoredLeaf,
    signed_by_ca: bool,
}

impl LeafEntry {
    /// `crl_url`: the distribution point leaves of the current CA must carry.
    fn status(
        &self,
        now: Timestamp,
        renew_before: SignedDuration,
        crl_url: Option<&str>,
    ) -> LeafStatus {
        let due = !self.signed_by_ca
            || !self.stored.key_ok
            || self.stored.info.crl_url.as_deref() != crl_url
            || self.stored.info.expires_within(now, renew_before);
        LeafStatus {
            info: self.stored.info.clone(),
            signed_by_ca: self.signed_by_ca,
            key_ok: self.stored.key_ok,
            due,
        }
    }
}

/// Aborts a task of the actor (scheduler, CRL responder) when the instance goes away (stop
/// or panic).
struct OwnedTask(JoinHandle<()>);

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct TlsActor {
    shared: Arc<Shared>,
    ca: Option<Arc<LocalCa>>,
    ca_error: Option<String>,
    leaves: BTreeMap<String, LeafEntry>,
    problems: Vec<String>,
    ledger: TrustLedger,
    pending: Option<(u64, Arc<LocalCa>)>,
    site_ids: HashMap<String, SiteId>,
    scheduler: Option<OwnedTask>,
    crl_server: Option<OwnedTask>,
    crl_error: Option<String>,
}

/// `d` ± 10 %.
fn jittered(d: Duration) -> Duration {
    let mut bytes = [0_u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        return d;
    }
    let unit = f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX); // 0..=1
    d.mul_f64(0.9 + 0.2 * unit)
}

impl TlsActor {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            ca: None,
            ca_error: None,
            leaves: BTreeMap::new(),
            problems: Vec::new(),
            ledger: TrustLedger::default(),
            pending: None,
            site_ids: HashMap::new(),
            scheduler: None,
            crl_server: None,
            crl_error: None,
        }
    }

    async fn blocking<T, F>(&self, what: &str, timeout: Duration, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Shared) -> Result<T> + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        spawn_blocking_timeout(what, timeout, move || f(&shared)).await
    }

    fn publish(
        &self,
        ctx: &Context<Self>,
        kind: EventKind,
        host: Option<&str>,
        thumbprint_sha256: &str,
        not_after: Timestamp,
    ) {
        let payload = TlsEventPayload {
            v: TLS_EVENT_VERSION,
            host: host.map(str::to_owned),
            site_id: host.and_then(|h| self.site_ids.get(h).copied()),
            thumbprint_sha256: thumbprint_sha256.to_owned(),
            not_after,
        };
        ctx.events()
            .publish(kind, serde_json::to_value(payload).unwrap_or_default());
    }

    fn ca_options(&self, now: Timestamp) -> CaOptions {
        let config = &self.shared.config;
        CaOptions {
            common_name: config
                .ca_common_name
                .clone()
                .unwrap_or_else(|| default_common_name(&machine_name(), now)),
            tlds: config.tlds.clone(),
            validity: config.ca_validity,
            now,
        }
    }

    /// Reads the CA, the leaves and the ledger from disk.
    async fn reload(&mut self) {
        let timeout = self.shared.config.bulk_timeout;
        let loaded = self
            .blocking("tls: loading certs/", timeout, |shared| {
                let ca = LocalCa::load(&shared.certs_ca);
                let (leaves, problems) = scan_leaves(&shared.certs_sites)?;
                let ca_der = ca
                    .as_ref()
                    .ok()
                    .and_then(|c| c.as_ref())
                    .map(|c| c.cert_der().to_vec());
                let leaves: Vec<LeafEntry> = leaves
                    .into_iter()
                    .map(|stored| LeafEntry {
                        signed_by_ca: ca_der.as_deref().is_some_and(|der| stored.signed_by(der)),
                        stored,
                    })
                    .collect();
                let ledger = TrustLedger::load(&shared.certs_ca).unwrap_or_else(|err| {
                    tracing::warn!(%err, "tls: trust ledger unreadable; starting a new one");
                    TrustLedger::default()
                });
                Ok((ca, leaves, problems, ledger))
            })
            .await;
        match loaded {
            Ok((ca, leaves, problems, ledger)) => {
                match ca {
                    Ok(ca) => {
                        self.ca = ca.map(Arc::new);
                        self.ca_error = None;
                    }
                    Err(err) => {
                        tracing::error!(%err, "tls: the local CA files cannot be used");
                        self.ca = None;
                        self.ca_error = Some(err.to_string());
                    }
                }
                self.leaves = leaves
                    .into_iter()
                    .map(|e| (e.stored.info.host.clone(), e))
                    .collect();
                self.problems = problems;
                self.ledger = ledger;
                tracing::info!(
                    ca = ?self.ca.as_ref().map(|c| c.info().thumbprint_sha1),
                    leaves = self.leaves.len(),
                    "tls: loaded"
                );
            }
            Err(err) => {
                tracing::error!(%err, "tls: could not read certs/");
                self.ca_error = Some(err.to_string());
            }
        }
    }

    /// The current CA, created (and published) when there is none.
    async fn ensure_ca(&mut self, ctx: &Context<Self>) -> Result<Arc<LocalCa>> {
        if let Some(ca) = &self.ca {
            return Ok(Arc::clone(ca));
        }
        if let Some(err) = &self.ca_error {
            return Err(ForgeError::ConfigInvalid(format!(
                "the local CA files cannot be used ({err}); rotate the CA to replace them"
            )));
        }
        let opts = self.ca_options(Timestamp::now());
        let ca = self
            .blocking(
                "tls: creating the local CA",
                self.shared.config.io_timeout,
                move |shared| {
                    let ca = LocalCa::generate(&opts)?;
                    ca.save(&shared.certs_ca, shared.acl.as_ref())?;
                    Ok(ca)
                },
            )
            .await?;
        let info = ca.info();
        tracing::info!(thumbprint = %info.thumbprint_sha1, subject = %info.subject, "tls: local CA created");
        self.publish(
            ctx,
            EventKind::TlsCaCreated,
            None,
            &info.thumbprint_sha256,
            info.not_after,
        );
        let ca = Arc::new(ca);
        self.ca = Some(Arc::clone(&ca));
        // Leaves of an older CA are now due.
        for entry in self.leaves.values_mut() {
            entry.signed_by_ca = false;
        }
        self.refresh_crl(true).await;
        Ok(ca)
    }

    /// Validity of a new leaf: the configured one, never past the CA's `notAfter`.
    fn leaf_validity(&self, ca: &LocalCa, now: Timestamp) -> Result<SignedDuration> {
        let left = ca.info().not_after.duration_since(now - BACKDATE);
        if left <= SignedDuration::ZERO {
            return Err(ForgeError::ConfigInvalid(
                "the local CA has expired; rotate it".to_owned(),
            ));
        }
        Ok(self.shared.config.leaf_validity.min(left))
    }

    /// Distribution point of the leaves of `ca` (`None` without a CRL responder).
    fn crl_url_for(&self, ca: &CaInfo) -> Option<String> {
        self.shared
            .config
            .crl_listen
            .map(|listen| crl_url(listen, &ca.thumbprint_sha1))
    }

    /// Distribution point the leaves of the current CA must carry.
    fn expected_crl_url(&self) -> Option<String> {
        self.ca.as_ref().and_then(|ca| self.crl_url_for(ca.info()))
    }

    /// Signs a new CRL for the current CA when `force`, when the served one belongs to
    /// another CA or when half of its validity is gone; writes `certs/ca/forgebox-ca.crl`
    /// and hands it to the responder. Without a CA nothing is served.
    async fn refresh_crl(&mut self, force: bool) {
        let Some(ca) = self.ca.clone() else {
            self.shared.crl.send_replace(None);
            return;
        };
        let now = Timestamp::now();
        let fresh = self.shared.crl.borrow().as_ref().is_some_and(|doc| {
            doc.path == crate::crl::crl_path(&ca.info().thumbprint_sha1) && !doc.needs_refresh(now)
        });
        if fresh && !force {
            return;
        }
        let validity = self.shared.config.crl_validity;
        let built = self
            .blocking(
                "tls: signing the CRL",
                self.shared.config.io_timeout,
                move |shared| {
                    let doc = build_crl(&ca, now, validity)?;
                    forge_core::paths::write_atomic(&shared.certs_ca.join(CA_CRL_FILE), &doc.der)?;
                    Ok(doc)
                },
            )
            .await;
        match built {
            Ok(doc) => {
                tracing::info!(path = %doc.path, next_update = %doc.next_update, "tls: CRL signed");
                self.shared.crl.send_replace(Some(doc));
            }
            Err(err) => tracing::error!(%err, "tls: could not sign the CRL"),
        }
    }

    /// Starts the CRL responder on `crl_listen` if it is not running.
    async fn start_crl_server(&mut self) {
        let Some(addr) = self.shared.config.crl_listen else {
            return;
        };
        if self.crl_server.is_some() {
            return;
        }
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "tls: CRL responder listening");
                let task = tokio::spawn(crate::crl::serve(listener, self.shared.crl.subscribe()));
                self.crl_server = Some(OwnedTask(task));
                self.crl_error = None;
            }
            Err(err) => {
                tracing::warn!(%addr, %err, "tls: CRL responder cannot listen");
                self.crl_error = Some(format!("{addr}: {err}"));
            }
        }
    }

    fn crl_status(&self) -> CrlStatus {
        CrlStatus {
            url: self.expected_crl_url(),
            serving: self.crl_server.is_some(),
            next_update: self.shared.crl.borrow().as_ref().map(|d| d.next_update),
            error: self.crl_error.clone(),
        }
    }

    /// Issues and stores leaves for `(host, sans)` in one blocking step. Returns the new
    /// entries and the hosts that failed.
    async fn write_leaves(
        &self,
        ca: Arc<LocalCa>,
        targets: Vec<(String, Vec<String>)>,
        timeout: Duration,
    ) -> Result<(Vec<LeafEntry>, Vec<String>)> {
        let now = Timestamp::now();
        let validity = self.leaf_validity(&ca, now)?;
        let distribution = self.crl_url_for(ca.info());
        self.blocking("tls: issuing site certificates", timeout, move |shared| {
            let mut done = Vec::new();
            let mut failed = Vec::new();
            for (host, sans) in targets {
                let result = issue_leaf(
                    &ca,
                    &shared.certs_sites,
                    &host,
                    &sans,
                    validity,
                    now,
                    distribution.as_deref(),
                )
                .and_then(|leaf| {
                    leaf.save(shared.acl.as_ref())?;
                    Ok(leaf)
                });
                match result {
                    Ok(leaf) => done.push(LeafEntry {
                        stored: StoredLeaf {
                            info: leaf.info().clone(),
                            cert_der: leaf.cert_der().to_vec(),
                            key_ok: true,
                        },
                        signed_by_ca: true,
                    }),
                    Err(err) => failed.push(format!("{host}: {err}")),
                }
            }
            Ok((done, failed))
        })
        .await
    }

    async fn issue(&mut self, ctx: &Context<Self>, request: LeafRequest) -> Result<LeafInfo> {
        let (host, sans) = request.sans()?;
        if let Some(site_id) = request.site_id {
            self.site_ids.insert(host.clone(), site_id);
        }
        let ca = self.ensure_ca(ctx).await?;
        check_against_ca(&ca, &host, &sans)?;
        let wanted: BTreeSet<&String> = sans.iter().collect();
        let kind = match self.leaves.get(&host) {
            Some(existing) => {
                let same_names =
                    existing.stored.info.sans.iter().collect::<BTreeSet<_>>() == wanted;
                let expected = self.expected_crl_url();
                let status = existing.status(
                    Timestamp::now(),
                    self.shared.config.renew_before,
                    expected.as_deref(),
                );
                if same_names && !status.due {
                    return Ok(existing.stored.info.clone());
                }
                if same_names {
                    EventKind::TlsRenewed
                } else {
                    EventKind::TlsLeafIssued
                }
            }
            None => EventKind::TlsLeafIssued,
        };
        let (mut done, failed) = self
            .write_leaves(
                ca,
                vec![(host.clone(), sans)],
                self.shared.config.io_timeout,
            )
            .await?;
        if let Some(err) = failed.into_iter().next() {
            return Err(ForgeError::Internal(err));
        }
        let entry = done
            .pop()
            .ok_or_else(|| ForgeError::Internal("no certificate issued".to_owned()))?;
        let info = entry.stored.info.clone();
        tracing::info!(%host, sans = ?info.sans, not_after = %info.not_after, "tls: site certificate issued");
        self.publish(
            ctx,
            kind,
            Some(&host),
            &info.thumbprint_sha256,
            info.not_after,
        );
        self.leaves.insert(host, entry);
        Ok(info)
    }

    async fn renew(&mut self, ctx: &Context<Self>, scope: RenewScope) -> Result<Vec<LeafInfo>> {
        let now = Timestamp::now();
        let renew_before = self.shared.config.renew_before;
        let expected = self.expected_crl_url();
        let hosts: Vec<String> = match &scope {
            RenewScope::Due => self
                .leaves
                .iter()
                .filter(|(_, e)| e.status(now, renew_before, expected.as_deref()).due)
                .map(|(h, _)| h.clone())
                .collect(),
            RenewScope::Host(host) => {
                let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
                if !self.leaves.contains_key(&host) {
                    return Err(ForgeError::NotFound(format!("certificate of {host}")));
                }
                vec![host]
            }
            RenewScope::All => self.leaves.keys().cloned().collect(),
        };
        if hosts.is_empty() {
            return Ok(Vec::new());
        }
        let ca = self.ensure_ca(ctx).await?;
        let targets = hosts
            .iter()
            .map(|h| (h.clone(), self.leaves[h].stored.info.sans.clone()))
            .collect();
        let (done, failed) = self
            .write_leaves(ca, targets, self.shared.config.bulk_timeout)
            .await?;
        for problem in &failed {
            tracing::warn!(%problem, "tls: certificate not renewed");
        }
        let mut out = Vec::with_capacity(done.len());
        for entry in done {
            let info = entry.stored.info.clone();
            tracing::info!(host = %info.host, not_after = %info.not_after, "tls: site certificate renewed");
            self.publish(
                ctx,
                EventKind::TlsRenewed,
                Some(&info.host),
                &info.thumbprint_sha256,
                info.not_after,
            );
            self.leaves.insert(info.host.clone(), entry);
            out.push(info);
        }
        if out.is_empty() && !failed.is_empty() {
            return Err(ForgeError::Internal(failed.join("; ")));
        }
        Ok(out)
    }

    async fn prepare_rotation(&mut self) -> Result<Prepared> {
        let opts = self.ca_options(Timestamp::now());
        let ca = self
            .blocking(
                "tls: generating a new CA",
                self.shared.config.io_timeout,
                move |_| LocalCa::generate(&opts),
            )
            .await?;
        let token = self.shared.rotations.fetch_add(1, Ordering::Relaxed) + 1;
        let prepared = Prepared {
            token,
            ca: ca.info().clone(),
            der: ca.cert_der().to_vec(),
            previous: self.ca.as_ref().map(|c| c.info().clone()),
        };
        self.pending = Some((token, Arc::new(ca)));
        Ok(prepared)
    }

    async fn commit_rotation(&mut self, ctx: &Context<Self>, token: u64) -> Result<Committed> {
        let ca = match self.pending.take() {
            Some((pending, ca)) if pending == token => ca,
            other => {
                self.pending = other;
                return Err(ForgeError::Conflict(
                    "no prepared CA rotation with this token".to_owned(),
                ));
            }
        };
        let save = Arc::clone(&ca);
        self.blocking(
            "tls: writing the new CA",
            self.shared.config.io_timeout,
            move |shared| save.save(&shared.certs_ca, shared.acl.as_ref()),
        )
        .await?;
        let info = ca.info().clone();
        tracing::info!(thumbprint = %info.thumbprint_sha1, "tls: local CA rotated");
        self.publish(
            ctx,
            EventKind::TlsCaCreated,
            None,
            &info.thumbprint_sha256,
            info.not_after,
        );
        self.ca = Some(Arc::clone(&ca));
        self.ca_error = None;
        self.refresh_crl(true).await;
        let targets: Vec<(String, Vec<String>)> = self
            .leaves
            .iter()
            .map(|(h, e)| (h.clone(), e.stored.info.sans.clone()))
            .collect();
        for entry in self.leaves.values_mut() {
            entry.signed_by_ca = false;
        }
        let (done, skipped) = self
            .write_leaves(ca, targets, self.shared.config.bulk_timeout)
            .await?;
        let mut leaves = Vec::with_capacity(done.len());
        for entry in done {
            let leaf = entry.stored.info.clone();
            self.publish(
                ctx,
                EventKind::TlsRenewed,
                Some(&leaf.host),
                &leaf.thumbprint_sha256,
                leaf.not_after,
            );
            self.leaves.insert(leaf.host.clone(), entry);
            leaves.push(leaf);
        }
        Ok(Committed {
            ca: info,
            leaves,
            skipped,
        })
    }

    async fn change_ledger(&mut self, change: LedgerChange) -> Result<()> {
        let mut ledger = self.ledger.clone();
        let changed = match change {
            LedgerChange::Record(entry) => {
                let same = ledger.entries.contains(&entry);
                ledger.record(entry);
                !same
            }
            LedgerChange::Forget(thumbprint, scope) => ledger.forget(&thumbprint, scope),
        };
        if !changed {
            return Ok(());
        }
        let to_save = ledger.clone();
        self.blocking(
            "tls: writing the trust ledger",
            self.shared.config.io_timeout,
            move |shared| to_save.save(&shared.certs_ca),
        )
        .await?;
        self.ledger = ledger;
        Ok(())
    }

    fn snapshot(&self) -> Snapshot {
        let now = Timestamp::now();
        let expected = self.expected_crl_url();
        Snapshot {
            ca: self.ca.as_ref().map(|c| c.info().clone()),
            ca_pem: self.ca.as_ref().map(|c| c.cert_pem().to_owned()),
            ca_error: self.ca_error.clone(),
            leaves: self
                .leaves
                .values()
                .map(|e| e.status(now, self.shared.config.renew_before, expected.as_deref()))
                .collect(),
            problems: self.problems.clone(),
            ledger: self.ledger.clone(),
            crl: self.crl_status(),
        }
    }

    fn start_scheduler(&mut self, ctx: &Context<Self>) {
        let actor = ctx.myself_weak();
        let first = self.shared.config.first_check_delay;
        let every = self.shared.config.check_interval;
        let task = tokio::spawn(async move {
            tokio::time::sleep(jittered(first)).await;
            loop {
                let Some(actor) = actor.upgrade() else { break };
                if let Err(err) = actor.tell(Msg::Renew {
                    scope: RenewScope::Due,
                    reply: None,
                }) {
                    tracing::warn!(error = %err, "tls: renewal check skipped");
                }
                drop(actor);
                tokio::time::sleep(jittered(every)).await;
            }
        });
        self.scheduler = Some(OwnedTask(task));
    }
}

impl Actor for TlsActor {
    type Msg = Msg;

    async fn on_start(&mut self, ctx: &mut Context<Self>) {
        self.reload().await;
        self.refresh_crl(false).await;
        self.start_crl_server().await;
        self.start_scheduler(ctx);
    }

    async fn handle(&mut self, msg: Msg, ctx: &mut Context<Self>) {
        match msg {
            Msg::Snapshot { reply } => {
                let _ = reply.send(self.snapshot());
            }
            Msg::EnsureCa { reply } => {
                let result = self.ensure_ca(ctx).await.map(|ca| CaMaterial {
                    info: ca.info().clone(),
                    der: ca.cert_der().to_vec(),
                });
                let _ = reply.send(result);
            }
            Msg::Issue { request, reply } => {
                let result = self.issue(ctx, request).await;
                let _ = reply.send(result);
            }
            Msg::Renew { scope, reply } => {
                if scope == RenewScope::Due {
                    // The daily check also keeps the CRL fresh and retries the responder.
                    self.refresh_crl(false).await;
                    self.start_crl_server().await;
                }
                let result = self.renew(ctx, scope).await;
                match reply {
                    Some(reply) => {
                        let _ = reply.send(result);
                    }
                    None => {
                        if let Err(err) = result {
                            tracing::warn!(%err, "tls: automatic renewal failed");
                        }
                    }
                }
            }
            Msg::PrepareRotation { reply } => {
                let result = self.prepare_rotation().await;
                let _ = reply.send(result);
            }
            Msg::CommitRotation { token, reply } => {
                let result = self.commit_rotation(ctx, token).await;
                let _ = reply.send(result);
            }
            Msg::AbortRotation { token } => {
                if self.pending.as_ref().is_some_and(|(t, _)| *t == token) {
                    self.pending = None;
                    tracing::info!("tls: CA rotation abandoned");
                }
            }
            Msg::Ledger { change, reply } => {
                let result = self.change_ledger(change).await;
                let _ = reply.send(result);
            }
            Msg::ExportLeaf { host, reply } => {
                let result = match self.leaves.get(&host) {
                    None => Err(ForgeError::NotFound(format!("certificate of {host}"))),
                    Some(entry) => {
                        let cert_path = entry.stored.info.cert_path.clone();
                        let key_path = entry.stored.info.key_path.clone();
                        self.blocking(
                            "tls: reading a site certificate",
                            self.shared.config.io_timeout,
                            move |_| {
                                Ok(LeafExport {
                                    host,
                                    cert_pem: crate::cert::read_text(&cert_path)?,
                                    key_pem: crate::cert::read_text(&key_path)?,
                                })
                            },
                        )
                        .await
                    }
                };
                let _ = reply.send(result);
            }
            Msg::LeafChain { host, reply } => {
                let result = match (self.leaves.get(&host), &self.ca) {
                    (None, _) => Err(ForgeError::NotFound(format!("certificate of {host}"))),
                    (_, None) => Err(ForgeError::NotFound("local CA".to_owned())),
                    (Some(entry), Some(ca)) => {
                        Ok((entry.stored.cert_der.clone(), ca.cert_der().to_vec()))
                    }
                };
                let _ = reply.send(result);
            }
            Msg::Reload { reply } => {
                self.reload().await;
                self.refresh_crl(false).await;
                self.start_crl_server().await;
                let _ = reply.send(());
            }
            #[cfg(test)]
            Msg::Panic => panic!("tls actor panic requested by a test"),
        }
    }
}

/// Handle of the `tls` actor. Cheap to clone.
#[derive(Clone)]
pub struct TlsManager {
    actor: ActorRef<Msg>,
    handle: Arc<ActorHandle<Msg>>,
    shared: Arc<Shared>,
    trust: Arc<dyn TrustStore>,
    events: EventBus,
}

impl std::fmt::Debug for TlsManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsManager")
            .field("certs_ca", &self.shared.certs_ca)
            .field("trust", &self.trust)
            .finish_non_exhaustive()
    }
}

impl TlsManager {
    /// Starts the actor `tls` for `root`: loads `certs/`, then checks renewals on the
    /// schedule of `config`. `acl` restricts the key files; `trust` is the OS trust store
    /// (`forge_core::platform::trust_store`).
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn spawn(
        supervisor: &ActorSupervisor,
        root: &ForgeRoot,
        config: TlsConfig,
        acl: Arc<dyn FileAcl>,
        trust: Arc<dyn TrustStore>,
    ) -> Self {
        let shared = Arc::new(Shared {
            certs_ca: root.certs_ca(),
            certs_sites: root.certs_sites(),
            config,
            acl,
            rotations: AtomicU64::new(0),
            crl: watch::channel(None).0,
        });
        let factory_shared = Arc::clone(&shared);
        let handle = supervisor.spawn(
            ActorSpec::new("tls")
                .mailbox(256)
                .stop_timeout(shared.config.io_timeout + Duration::from_secs(1)),
            move || TlsActor::new(Arc::clone(&factory_shared)),
        );
        Self {
            actor: handle.actor_ref().clone(),
            handle: Arc::new(handle),
            shared,
            trust,
            events: supervisor.events().clone(),
        }
    }

    fn config(&self) -> &TlsConfig {
        &self.shared.config
    }

    /// Ask budget of an operation whose blocking step has `inner`.
    fn budget(inner: Duration) -> Duration {
        inner + Duration::from_secs(5)
    }

    async fn ask<R>(&self, make: impl FnOnce(Reply<R>) -> Msg, inner: Duration) -> Result<R> {
        Ok(self.actor.ask(make, Self::budget(inner)).await?)
    }

    /// CA, trust and leaves. The trust store is read without elevation or dialog.
    pub async fn status(&self) -> Result<TlsStatus> {
        let snap = self
            .ask(|reply| Msg::Snapshot { reply }, self.config().io_timeout)
            .await?;
        let mut trusted = (false, false);
        let mut trust_error = None;
        if let Some(ca) = &snap.ca {
            let trust = Arc::clone(&self.trust);
            let thumbprint = ca.thumbprint_sha1;
            match spawn_blocking_timeout(
                "tls: reading the trust store",
                self.config().io_timeout,
                move || {
                    Ok((
                        trust.is_trusted(&thumbprint, TrustScope::CurrentUser)?,
                        trust.is_trusted(&thumbprint, TrustScope::LocalMachine)?,
                    ))
                },
            )
            .await
            {
                Ok(pair) => trusted = pair,
                Err(err) => trust_error = Some(err.to_string()),
            }
        }
        let now = Timestamp::now();
        let ca_expires_soon = snap
            .ca
            .as_ref()
            .is_some_and(|ca| ca.not_after.duration_since(now) < CA_WARN_BEFORE);
        let missing_tlds = snap
            .ca
            .as_ref()
            .map(|ca| ca.missing_tlds(&self.config().tlds))
            .unwrap_or_default();
        let current = snap.ca.as_ref().map(|c| c.thumbprint_sha1);
        let stale_roots = snap
            .ledger
            .entries
            .iter()
            .filter(|e| Some(e.thumbprint_sha1) != current)
            .cloned()
            .collect();
        let mut warnings = Vec::new();
        if ca_expires_soon {
            warnings.push(messages::CA_EXPIRES_SOON.to_owned());
        }
        if !missing_tlds.is_empty() {
            warnings.push(format!(
                "{} ({})",
                messages::CA_MISSING_TLDS,
                missing_tlds.join(", ")
            ));
        }
        if !self.config().os_trust {
            warnings.push(messages::PORTABLE_NO_TRUST.to_owned());
        }
        Ok(TlsStatus {
            ca: snap.ca,
            ca_error: snap.ca_error,
            trusted_current_user: trusted.0,
            trusted_local_machine: trusted.1,
            trust_error,
            ca_expires_soon,
            missing_tlds,
            stale_roots,
            leaves: snap.leaves,
            problems: snap.problems,
            crl: snap.crl,
            warnings,
        })
    }

    /// The CA, created (with `tls.ca_created`) when there is none.
    pub async fn ensure_ca(&self) -> Result<CaInfo> {
        Ok(self.ensure_ca_material().await?.info)
    }

    async fn ensure_ca_material(&self) -> Result<CaMaterial> {
        self.ask(|reply| Msg::EnsureCa { reply }, self.config().io_timeout)
            .await?
    }

    /// The certificate of a site: the existing one when it already covers the request and
    /// is not due, otherwise a new one (`tls.leaf_issued`, or `tls.renewed` when only its
    /// age or CA changed). Creates the CA first if needed.
    pub async fn issue(&self, request: LeafRequest) -> Result<LeafInfo> {
        self.ask(
            |reply| Msg::Issue { request, reply },
            self.config().io_timeout * 2,
        )
        .await?
    }

    /// Re-issues the leaves of `scope` (`tls.renewed` each) and returns them.
    pub async fn renew(&self, scope: RenewScope) -> Result<Vec<LeafInfo>> {
        self.ask(
            |reply| Msg::Renew {
                scope,
                reply: Some(reply),
            },
            self.config().bulk_timeout,
        )
        .await?
    }

    /// Re-reads `certs/` (files changed by hand).
    pub async fn reload(&self) -> Result<()> {
        self.ask(|reply| Msg::Reload { reply }, self.config().bulk_timeout)
            .await
    }

    /// The CA certificate in PEM (`tls.ca_export`). `not_found` without a CA.
    pub async fn export_ca(&self) -> Result<String> {
        let snap = self
            .ask(|reply| Msg::Snapshot { reply }, self.config().io_timeout)
            .await?;
        snap.ca_pem
            .ok_or_else(|| ForgeError::NotFound("local CA".to_owned()))
    }

    /// Certificate and private key of `host` (`tls.export`).
    pub async fn export_leaf(&self, host: &str) -> Result<LeafExport> {
        let host = host.to_ascii_lowercase();
        self.ask(
            |reply| Msg::ExportLeaf { host, reply },
            self.config().io_timeout,
        )
        .await?
    }

    /// Asks the OS TLS verifier whether the certificate of `host` validates for `host`
    /// with the local CA as the only root (name constraints included; revocation through
    /// the CRL responder when it is configured). Diagnostics.
    pub async fn verify_with_os(&self, host: &str) -> Result<()> {
        let host = host.to_ascii_lowercase();
        let probe = host.clone();
        // With the CRL responder, check revocation like Windows `curl.exe` does.
        let revocation = self.config().crl_listen.is_some();
        let (leaf, ca) = self
            .ask(
                |reply| Msg::LeafChain { host, reply },
                self.config().io_timeout,
            )
            .await??;
        let trust = Arc::clone(&self.trust);
        spawn_blocking_timeout(
            "tls: OS chain verification",
            self.config().io_timeout,
            move || trust.verify_server(&[leaf], &ca, &probe, revocation),
        )
        .await
    }

    async fn ledger(&self, change: LedgerChange) -> Result<()> {
        self.ask(
            |reply| Msg::Ledger { change, reply },
            self.config().io_timeout,
        )
        .await?
    }

    fn publish_helper_op(&self, name: &str, result: &Result<impl Sized>) {
        let payload = HelperOpEvent {
            v: TLS_EVENT_VERSION,
            name,
            ok: result.is_ok(),
            error: result.as_ref().err().map(ToString::to_string),
        };
        self.events.publish(
            EventKind::HelperOp,
            serde_json::to_value(payload).unwrap_or_default(),
        );
    }

    async fn trust_read(&self, thumbprint: CertThumbprint, scope: TrustScope) -> Result<bool> {
        let trust = Arc::clone(&self.trust);
        spawn_blocking_timeout(
            "tls: reading the trust store",
            self.config().io_timeout,
            move || trust.is_trusted(&thumbprint, scope),
        )
        .await
    }

    async fn trust_add(&self, der: Vec<u8>, scope: TrustScope) -> Result<()> {
        let trust = Arc::clone(&self.trust);
        let result = spawn_blocking_timeout(
            "tls: trusting the local CA",
            self.config().trust_timeout,
            move || trust.add_root(&der, scope),
        )
        .await;
        if scope == TrustScope::LocalMachine {
            self.publish_helper_op("trust.add_machine_root", &result);
        }
        result
    }

    async fn trust_remove(&self, thumbprint: CertThumbprint, scope: TrustScope) -> Result<bool> {
        let trust = Arc::clone(&self.trust);
        let result = spawn_blocking_timeout(
            "tls: removing a root",
            self.config().trust_timeout,
            move || trust.remove_root(&thumbprint, scope),
        )
        .await;
        if scope == TrustScope::LocalMachine {
            self.publish_helper_op("trust.remove_machine_root", &result);
        }
        result
    }

    fn entry(ca: &CaInfo, scope: TrustScope) -> TrustEntry {
        TrustEntry {
            thumbprint_sha1: ca.thumbprint_sha1,
            scope,
            subject: ca.subject.clone(),
            not_after: ca.not_after,
            added_at: Timestamp::now(),
        }
    }

    fn check_os_trust(&self) -> Result<()> {
        if self.config().os_trust {
            Ok(())
        } else {
            Err(ForgeError::Forbidden(
                messages::PORTABLE_NO_TRUST.to_owned(),
            ))
        }
    }

    /// Trusts the CA in `scope` (creating it if needed). `CurrentUser` shows the Windows
    /// confirmation dialog once (declined: `cancelled`); `LocalMachine` goes through
    /// `forgebox-helper`. Waits up to [`TlsConfig::trust_timeout`]: run it in a job.
    pub async fn install_ca(&self, scope: TrustScope) -> Result<InstallReport> {
        self.check_os_trust()?;
        let ca = self.ensure_ca_material().await?;
        let already = self.trust_read(ca.info.thumbprint_sha1, scope).await?;
        if !already {
            self.trust_add(ca.der.clone(), scope).await?;
            tracing::info!(%scope, thumbprint = %ca.info.thumbprint_sha1, "tls: local CA trusted");
        }
        self.ledger(LedgerChange::Record(Self::entry(&ca.info, scope)))
            .await?;
        Ok(InstallReport {
            ca: ca.info,
            scope,
            added: !already,
        })
    }

    /// Removes from `scope` the current CA and every root Forgebox recorded there (older
    /// CAs). Returns the thumbprints removed. Dialogs and prompts as in
    /// [`TlsManager::install_ca`].
    pub async fn uninstall_ca(&self, scope: TrustScope) -> Result<Vec<CertThumbprint>> {
        self.check_os_trust()?;
        let snap = self
            .ask(|reply| Msg::Snapshot { reply }, self.config().io_timeout)
            .await?;
        let mut targets: BTreeSet<CertThumbprint> = snap
            .ledger
            .in_scope(scope)
            .map(|e| e.thumbprint_sha1)
            .collect();
        if let Some(ca) = &snap.ca {
            targets.insert(ca.thumbprint_sha1);
        }
        let mut removed = Vec::new();
        for thumbprint in targets {
            if self.trust_remove(thumbprint, scope).await? {
                tracing::info!(%scope, %thumbprint, "tls: root removed");
                removed.push(thumbprint);
            }
            self.ledger(LedgerChange::Forget(thumbprint, scope)).await?;
        }
        Ok(removed)
    }

    /// Replaces the CA and re-issues every leaf (docs/06 §6.1, docs/11 §3). The new CA is
    /// trusted wherever the old one was **before** anything is written, so declining the
    /// dialog leaves everything as it was (`cancelled`). Run it in a job.
    pub async fn rotate_ca(&self) -> Result<RotationReport> {
        let prepared = self
            .ask(
                |reply| Msg::PrepareRotation { reply },
                self.config().io_timeout,
            )
            .await??;
        let token = prepared.token;
        let mut warnings = Vec::new();
        let mut old_scopes = Vec::new();
        if let (Some(old), true) = (&prepared.previous, self.config().os_trust) {
            for scope in [TrustScope::CurrentUser, TrustScope::LocalMachine] {
                match self.trust_read(old.thumbprint_sha1, scope).await {
                    Ok(true) => old_scopes.push(scope),
                    Ok(false) => {}
                    Err(err) => warnings.push(format!("{scope}: {err}")),
                }
            }
        }
        let mut trusted = Vec::new();
        if old_scopes.contains(&TrustScope::CurrentUser) {
            if let Err(err) = self
                .trust_add(prepared.der.clone(), TrustScope::CurrentUser)
                .await
            {
                let _ = self.actor.tell(Msg::AbortRotation { token });
                return Err(err);
            }
            trusted.push(TrustScope::CurrentUser);
        }
        let committed = match self
            .ask(
                |reply| Msg::CommitRotation { token, reply },
                self.config().bulk_timeout,
            )
            .await
            .and_then(|r| r)
        {
            Ok(committed) => committed,
            Err(err) => {
                let _ = self.actor.tell(Msg::AbortRotation { token });
                if trusted.contains(&TrustScope::CurrentUser) {
                    let new = prepared.ca.thumbprint_sha1;
                    if let Err(undo) = self.trust_remove(new, TrustScope::CurrentUser).await {
                        tracing::warn!(%undo, "tls: could not remove the unused new root");
                    }
                }
                return Err(err);
            }
        };
        if trusted.contains(&TrustScope::CurrentUser) {
            self.ledger(LedgerChange::Record(Self::entry(
                &committed.ca,
                TrustScope::CurrentUser,
            )))
            .await?;
        }
        if old_scopes.contains(&TrustScope::LocalMachine) {
            match self
                .trust_add(prepared.der.clone(), TrustScope::LocalMachine)
                .await
            {
                Ok(()) => {
                    trusted.push(TrustScope::LocalMachine);
                    self.ledger(LedgerChange::Record(Self::entry(
                        &committed.ca,
                        TrustScope::LocalMachine,
                    )))
                    .await?;
                }
                Err(err) => warnings.push(format!("local_machine: {err}")),
            }
        }
        let mut removed_previous = Vec::new();
        if let Some(old) = &prepared.previous {
            for scope in &old_scopes {
                match self.trust_remove(old.thumbprint_sha1, *scope).await {
                    Ok(_) => {
                        removed_previous.push(*scope);
                        self.ledger(LedgerChange::Forget(old.thumbprint_sha1, *scope))
                            .await?;
                    }
                    Err(err) => warnings.push(format!(
                        "{scope}: the previous CA {} is still trusted: {err}",
                        old.thumbprint_sha1
                    )),
                }
            }
        }
        Ok(RotationReport {
            ca: committed.ca,
            previous: prepared.previous,
            leaves: committed.leaves,
            skipped: committed.skipped,
            trusted,
            removed_previous,
            warnings,
        })
    }

    /// Stops the actor for good (graceful, bounded).
    pub async fn shutdown(&self) {
        self.handle.stop().await;
    }

    /// Test hook: makes the actor panic.
    #[cfg(test)]
    fn panic_actor(&self) {
        let _ = self.actor.tell(Msg::Panic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::platform::FileAcl;
    use std::path::Path;

    /// Grants every ACL request (unit tests do not care about DACLs).
    #[derive(Debug)]
    struct NoAcl;
    impl FileAcl for NoAcl {
        fn restrict_to_current_user(&self, _: &Path) -> Result<()> {
            Ok(())
        }
        fn is_restricted_to_current_user(&self, _: &Path) -> Result<bool> {
            Ok(true)
        }
    }

    /// Trust store that trusts nothing and refuses changes.
    #[derive(Debug)]
    struct NoTrust;
    impl TrustStore for NoTrust {
        fn is_trusted(&self, _: &CertThumbprint, _: TrustScope) -> Result<bool> {
            Ok(false)
        }
        fn add_root(&self, _: &[u8], _: TrustScope) -> Result<()> {
            Err(ForgeError::Cancelled("declined".into()))
        }
        fn remove_root(&self, _: &CertThumbprint, _: TrustScope) -> Result<bool> {
            Ok(false)
        }
        fn verify_server(&self, _: &[Vec<u8>], _: &[u8], _: &str, _: bool) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn for_root_uses_the_tls_crl_range_and_portable_mode() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let config = forge_core::config::ForgeConfig::default();
        let tls = TlsConfig::for_root(&root, &config);
        assert_eq!(tls.tlds, [".test"]);
        assert_eq!(
            tls.crl_listen,
            Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 47480)))
        );
        assert!(tls.os_trust);
        std::fs::write(root.portable_flag(), b"").unwrap();
        assert!(!TlsConfig::for_root(&root, &config).os_trust);
        assert_eq!(TlsConfig::new(vec![]).crl_listen, None);
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        for _ in 0..100 {
            let d = jittered(Duration::from_secs(100));
            assert!(
                d >= Duration::from_secs(90) && d <= Duration::from_secs(110),
                "{d:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_restarted_actor_reloads_the_ca_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let supervisor = ActorSupervisor::new(EventBus::new());
        let tls = TlsManager::spawn(
            &supervisor,
            &root,
            TlsConfig::new(vec![".test".into()]),
            Arc::new(NoAcl),
            Arc::new(NoTrust),
        );
        let ca = tls.ensure_ca().await.unwrap();
        tls.issue(LeafRequest::new("loja.test")).await.unwrap();
        tls.panic_actor();
        let mut status = tls.status().await;
        for _ in 0..50 {
            if status.as_ref().is_ok_and(|s| s.ca.is_some()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            status = tls.status().await;
        }
        let status = status.unwrap();
        assert_eq!(status.ca.unwrap().thumbprint_sha1, ca.thumbprint_sha1);
        assert_eq!(status.leaves.len(), 1);
        assert!(status.leaves[0].signed_by_ca && !status.leaves[0].due);
        tls.shutdown().await;
    }
}
