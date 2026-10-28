//! Port allocator (docs/03 §4): named ranges of `forgebox.toml`
//! ([`forge_core::config::PortsConfig`]), reservations persisted in the `ports` table
//! ([`forge_core::store::PortRepo`]) and the OS TCP table
//! ([`forge_core::platform::PortTable`]) to avoid ports taken outside Forgebox.
//!
//! * [`PortAllocator::allocate`]: the owner's port, allocating the first free port of a
//!   range the first time (idempotent: a port is never "guessed" twice). Skips ports
//!   reserved for other owners, the `[ports] reserved` list (Xdebug 9003, only handed out
//!   by an explicit [`PortAllocator::reserve`]), ports with a TCP listener and ports the
//!   OS refuses to bind (a quick bind of `127.0.0.1:<port>`; catches the ranges Hyper-V
//!   and WinNAT exclude, which have no listener).
//! * [`PortAllocator::reserve`]: an explicit port (user choice, "Trocar porta"); moves the
//!   owner's reservation atomically.
//! * [`PortAllocator::release`] / [`PortAllocator::release_owner`].
//! * [`forge_core::model::port::PortReservations`] is implemented by delegation, so crates
//!   that cannot depend on `forge-net` (the PHP pools of `forge-fcgi`) get their ports
//!   from the same allocator through an `Arc<dyn PortReservations>` given by the daemon.
//! * [`PortAllocator::conflicts`]: crosses the reservations with the TCP listeners,
//!   stores each reservation's state (`bound` when its owner listens, `conflict` with the
//!   external PID/exe, `reserved` otherwise) and returns the conflicts. The daemon
//!   publishes `port.conflict` for the [`PortConflict::newly_detected`] ones.
//!
//! **Concurrency.** The allocator is a cheap façade, not an actor: every decision that
//! must be atomic (owner already served? which port is free? insert) is **one** operation
//! of the store's writer thread, which serializes all writes (docs/03 §7.1). The TCP
//! table and the bind check are blocking calls made before/after that operation in
//! `spawn_blocking` with a timeout; no lock is held across an `.await`. Only TCP is
//! allocated (no Forgebox service declares UDP ports yet).

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use forge_core::config::PortsConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::model::port::{
    OwnerKind, PortFuture, PortHolder, PortProtocol, PortReservation, PortReservations, PortState,
    ReservationOwner,
};
use forge_core::platform::{PortOwner, PortTable};
use forge_core::runtime::spawn_blocking_timeout;
use forge_core::store::{PortRepo, Store, StoreReader, StoreWriter};

/// Tuning of a [`PortAllocator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocatorOptions {
    /// Budget of one read of the TCP table.
    pub table_timeout: Duration,
    /// Bind `127.0.0.1:<port>` before handing out a new port (off only in tests that
    /// use fake tables).
    pub bind_check: bool,
    /// Ports refused by the bind check before `allocate` gives up.
    pub max_bind_failures: usize,
}

impl Default for AllocatorOptions {
    /// 5 s table budget, bind check on, at most 64 refused ports.
    fn default() -> Self {
        Self {
            table_timeout: Duration::from_secs(5),
            bind_check: true,
            max_bind_failures: 64,
        }
    }
}

/// A reservation whose port is held by a process that is not its owner's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortConflict {
    /// The reservation, with `state == Conflict` and the holder stored.
    pub reservation: PortReservation,
    /// The listener seen on the port.
    pub holder: PortOwner,
    /// The conflict is new (or has a new holder) since the previous check.
    pub newly_detected: bool,
}

/// Port allocator over the store and the OS TCP table. Cheap to clone.
#[derive(Clone)]
pub struct PortAllocator {
    writer: StoreWriter,
    reader: StoreReader,
    table: Arc<dyn PortTable>,
    /// The `[ports]` section in use, shared by every clone and replaced live by
    /// [`PortAllocator::set_config`] (`forge config set ports.ranges.web ...`).
    config: Arc<watch::Sender<Arc<PortsConfig>>>,
    options: AllocatorOptions,
}

impl std::fmt::Debug for PortAllocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortAllocator")
            .field("table", &self.table)
            .field("ranges", &self.config().ranges.keys().collect::<Vec<_>>())
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

/// What the allocating write operation did.
enum Pick {
    /// The owner already had a port.
    Existing(PortReservation),
    /// A new reservation was inserted.
    New(PortReservation),
}

impl PortAllocator {
    /// Allocator over `store` (its writer and read pool), the TCP table `table` and the
    /// `[ports]` section of `forgebox.toml`.
    pub fn new(store: &Store, table: Arc<dyn PortTable>, config: PortsConfig) -> Self {
        Self {
            writer: store.writer().clone(),
            reader: store.reader().clone(),
            table,
            config: Arc::new(watch::Sender::new(Arc::new(config))),
            options: AllocatorOptions::default(),
        }
    }

    /// Replaces the tuning.
    pub fn with_options(mut self, options: AllocatorOptions) -> Self {
        self.options = options;
        self
    }

    /// The `[ports]` configuration in use (a snapshot).
    pub fn config(&self) -> Arc<PortsConfig> {
        Arc::clone(&self.config.borrow())
    }

    /// Replaces the `[ports]` configuration for every clone of this allocator (the
    /// preferences changed). Existing reservations are kept: an owner keeps its port
    /// even if the range no longer contains it; only new allocations see the new ranges.
    pub fn set_config(&self, config: PortsConfig) {
        self.config.send_replace(Arc::new(config));
    }

    /// Whether `port` belongs to the range `range` of the current configuration.
    pub fn in_range(&self, range: &str, port: u16) -> bool {
        self.config()
            .range(range)
            .is_some_and(|set| set.contains(port))
    }

    /// Current TCP listeners (blocking call off the async workers, bounded).
    async fn listeners(&self) -> Result<Vec<PortOwner>> {
        let table = Arc::clone(&self.table);
        spawn_blocking_timeout(
            "reading the TCP table",
            self.options.table_timeout,
            move || table.listeners(),
        )
        .await
    }

    /// Whether `127.0.0.1:<port>` can be bound right now (the socket is closed at once).
    async fn bindable(&self, port: u16) -> Result<bool> {
        if !self.options.bind_check {
            return Ok(true);
        }
        spawn_blocking_timeout("checking a port", self.options.table_timeout, move || {
            Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok())
        })
        .await
    }

    /// The reservation of `owner`, allocating the first free port of `range` the first
    /// time. Calling it again for the same owner returns the same port, even if the
    /// range changed meanwhile.
    ///
    /// Errors: `not_found` for an unknown range, `conflict` when the range has no free
    /// port, `invalid_input` for a malformed owner.
    pub async fn allocate(&self, range: &str, owner: ReservationOwner) -> Result<PortReservation> {
        owner.validate()?;
        let config = self.config();
        let set = config
            .range(range)
            .ok_or_else(|| ForgeError::NotFound(format!("port range {range:?}")))?
            .clone();
        if let Some(existing) = self.get(&owner).await? {
            return Ok(existing);
        }
        // Without the table the bind check still catches what is taken on 127.0.0.1.
        let mut busy: BTreeSet<u16> = match self.listeners().await {
            Ok(listeners) => listeners.iter().map(|l| l.port).collect(),
            Err(err) => {
                tracing::warn!(%err, "allocating without the TCP table");
                BTreeSet::new()
            }
        };
        let reserved: BTreeSet<u16> = config.reserved.iter().copied().collect();
        let mut refused = 0_usize;
        loop {
            let skip: BTreeSet<u16> = busy.union(&reserved).copied().collect();
            let (set, owner, range_name) = (set.clone(), owner.clone(), range.to_owned());
            let pick = self
                .writer
                .execute(move |conn| {
                    let repo = PortRepo::new(conn);
                    if let Some(existing) = repo.find_owner(&owner)? {
                        return Ok(Pick::Existing(existing));
                    }
                    let taken = repo.reserved_ports(PortProtocol::Tcp)?;
                    let port = set
                        .ports()
                        .find(|p| !taken.contains(p) && !skip.contains(p))
                        .ok_or_else(|| {
                            ForgeError::Conflict(format!(
                                "no free port left in range {range_name:?}"
                            ))
                        })?;
                    let reservation = PortReservation::new_tcp(port, Some(range_name), owner);
                    repo.insert(&reservation)?;
                    Ok(Pick::New(reservation))
                })
                .await?;
            let reservation = match pick {
                Pick::Existing(reservation) => return Ok(reservation),
                Pick::New(reservation) => reservation,
            };
            if self.bindable(reservation.port).await? {
                return Ok(reservation);
            }
            tracing::debug!(
                port = reservation.port,
                range,
                "port cannot be bound (taken or excluded by the system); trying the next one"
            );
            let id = reservation.id;
            self.writer
                .execute(move |conn| PortRepo::new(conn).delete(id))
                .await?;
            busy.insert(reservation.port);
            refused += 1;
            if refused >= self.options.max_bind_failures {
                return Err(ForgeError::Conflict(format!(
                    "{refused} ports of range {range:?} could not be bound"
                )));
            }
        }
    }

    /// Reserves exactly `port` for `owner` (an explicit choice: `services.create --port`,
    /// "Trocar porta", the Xdebug port). Ports of the `reserved` list are allowed. If the
    /// owner holds another port, the reservation moves (atomically). `range` is recorded
    /// for information.
    ///
    /// Errors: `port_in_use` when a process listens on the port (or the OS refuses to
    /// bind it), `conflict` when another owner holds it.
    pub async fn reserve(
        &self,
        port: u16,
        range: Option<&str>,
        owner: ReservationOwner,
    ) -> Result<PortReservation> {
        owner.validate()?;
        if port == 0 {
            return Err(ForgeError::InvalidInput("port 0 cannot be reserved".into()));
        }
        if let Some(existing) = self.get(&owner).await? {
            if existing.port == port {
                return Ok(existing);
            }
        }
        if let Some(holder) = self.listeners().await?.into_iter().find(|l| l.port == port) {
            return Err(ForgeError::PortInUse {
                port,
                owner: Some(describe(&holder)),
            });
        }
        if !self.bindable(port).await? {
            return Err(ForgeError::PortInUse {
                port,
                owner: Some("the system (the port cannot be bound)".into()),
            });
        }
        let range = range.map(str::to_owned);
        self.writer
            .execute(move |conn| {
                let repo = PortRepo::new(conn);
                if let Some(taken) = repo.find_port(PortProtocol::Tcp, port)? {
                    if taken.owner == owner {
                        return Ok(taken);
                    }
                    return Err(ForgeError::Conflict(format!(
                        "port {port} is reserved for {}",
                        taken.owner
                    )));
                }
                let mut reservation = PortReservation::new_tcp(port, range, owner);
                if let Some(previous) = repo.delete_owner_port(&reservation.owner)? {
                    reservation.created_at = previous.created_at;
                }
                repo.insert(&reservation)?;
                Ok(reservation)
            })
            .await
    }

    /// Frees the port of `owner`; returns the released reservation.
    pub async fn release(&self, owner: &ReservationOwner) -> Result<Option<PortReservation>> {
        let owner = owner.clone();
        self.writer
            .execute(move |conn| PortRepo::new(conn).delete_owner_port(&owner))
            .await
    }

    /// Frees every port of an owner (instance or site removed); returns how many.
    pub async fn release_owner(&self, kind: OwnerKind, id: &str) -> Result<usize> {
        let id = id.to_owned();
        self.writer
            .execute(move |conn| PortRepo::new(conn).delete_owner(kind, &id))
            .await
    }

    /// The reservation of `owner`, if any.
    pub async fn get(&self, owner: &ReservationOwner) -> Result<Option<PortReservation>> {
        let owner = owner.clone();
        self.reader
            .query(move |conn| PortRepo::new(conn).find_owner(&owner))
            .await
    }

    /// The reservation holding `port` (any owner), if any: `net.port_check`.
    pub async fn reservation_of(
        &self,
        protocol: PortProtocol,
        port: u16,
    ) -> Result<Option<PortReservation>> {
        self.reader
            .query(move |conn| PortRepo::new(conn).find_port(protocol, port))
            .await
    }

    /// Every reservation, by port.
    pub async fn list(&self) -> Result<Vec<PortReservation>> {
        self.reader.query(|conn| PortRepo::new(conn).list()).await
    }

    /// Crosses the TCP reservations with the listeners of the machine and stores each
    /// one's state: `bound` when a process of its owner listens on the port
    /// (`is_owner_pid(owner, pid)`, answered by the daemon from the jobs of its
    /// instances/pools/apps), `conflict` (with the holder) when only other processes do,
    /// `reserved` when nobody does. Returns the conflicts, by port.
    pub async fn conflicts<F>(&self, is_owner_pid: F) -> Result<Vec<PortConflict>>
    where
        F: Fn(&ReservationOwner, u32) -> bool,
    {
        let listeners = self.listeners().await?;
        let reservations = self.list().await?;
        let mut updates: Vec<(PortReservation, PortState, Option<PortHolder>)> = Vec::new();
        let mut conflicts = Vec::new();
        for reservation in reservations {
            if reservation.protocol != PortProtocol::Tcp {
                continue;
            }
            let on_port: Vec<&PortOwner> = listeners
                .iter()
                .filter(|l| l.port == reservation.port)
                .collect();
            let owned = on_port
                .iter()
                .any(|l| is_owner_pid(&reservation.owner, l.pid));
            let external = on_port
                .iter()
                .find(|l| !is_owner_pid(&reservation.owner, l.pid));
            let (state, holder) = match (owned, external) {
                (true, _) => (PortState::Bound, None),
                (false, Some(l)) => (
                    PortState::Conflict,
                    Some(PortHolder {
                        pid: l.pid,
                        exe: l.exe_display(),
                    }),
                ),
                (false, None) => (PortState::Reserved, None),
            };
            if let (PortState::Conflict, Some(l), Some(h)) = (state, external, &holder) {
                let newly_detected = reservation.state != PortState::Conflict
                    || reservation.conflict.as_ref().map(|c| c.pid) != Some(h.pid);
                let mut updated = reservation.clone();
                updated.state = state;
                updated.conflict = holder.clone();
                conflicts.push(PortConflict {
                    reservation: updated,
                    holder: (*l).clone(),
                    newly_detected,
                });
            }
            if reservation.state != state || reservation.conflict != holder {
                updates.push((reservation, state, holder));
            }
        }
        if !updates.is_empty() {
            let changes: Vec<_> = updates
                .into_iter()
                .map(|(r, state, holder)| (r.id, state, holder))
                .collect();
            self.writer
                .execute(move |conn| {
                    let repo = PortRepo::new(conn);
                    for (id, state, holder) in &changes {
                        // A row released meanwhile is simply gone.
                        repo.set_state(*id, *state, holder.as_ref())?;
                    }
                    Ok(())
                })
                .await?;
        }
        Ok(conflicts)
    }
}

impl PortReservations for PortAllocator {
    fn allocate<'a>(
        &'a self,
        range: &'a str,
        owner: ReservationOwner,
    ) -> PortFuture<'a, PortReservation> {
        Box::pin(PortAllocator::allocate(self, range, owner))
    }

    fn release<'a>(
        &'a self,
        owner: &'a ReservationOwner,
    ) -> PortFuture<'a, Option<PortReservation>> {
        Box::pin(PortAllocator::release(self, owner))
    }

    fn release_owner<'a>(&'a self, kind: OwnerKind, id: &'a str) -> PortFuture<'a, usize> {
        Box::pin(PortAllocator::release_owner(self, kind, id))
    }

    fn reserve<'a>(
        &'a self,
        port: u16,
        range: Option<&'a str>,
        owner: ReservationOwner,
    ) -> PortFuture<'a, PortReservation> {
        Box::pin(PortAllocator::reserve(self, port, range, owner))
    }
}

/// `C:\xampp\mysql\bin\mysqld.exe (PID 4412)` or `PID 4`.
fn describe(holder: &PortOwner) -> String {
    match holder.exe_display() {
        Some(exe) => format!("{exe} (PID {})", holder.pid),
        None => format!("PID {}", holder.pid),
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::path::PathBuf;

    use std::sync::Mutex;

    use forge_core::platform::PortProto;

    use super::*;

    /// TCP table with listeners set by the test.
    #[derive(Debug, Default)]
    struct FakeTable(Mutex<Vec<PortOwner>>);

    impl FakeTable {
        fn listen(&self, port: u16, pid: u32, exe: Option<&str>) {
            self.0.lock().unwrap().push(PortOwner {
                port,
                addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
                pid,
                exe_path: exe.map(PathBuf::from),
                proto: PortProto::Tcp,
            });
        }

        fn clear(&self) {
            self.0.lock().unwrap().clear();
        }
    }

    impl PortTable for FakeTable {
        fn listeners(&self) -> Result<Vec<PortOwner>> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn established_count(&self, _: u16) -> Result<u32> {
            Ok(0)
        }
    }

    fn config(ranges: &[(&str, &str)], reserved: &[u16]) -> PortsConfig {
        let mut config = PortsConfig {
            reserved: reserved.to_vec(),
            ..PortsConfig::default()
        };
        for (name, spec) in ranges {
            config
                .ranges
                .insert((*name).to_owned(), spec.parse().unwrap());
        }
        config
    }

    fn owner(id: &str) -> ReservationOwner {
        ReservationOwner::new(OwnerKind::Instance, id, "main").unwrap()
    }

    fn allocator(table: Arc<FakeTable>, config: PortsConfig) -> (Store, PortAllocator) {
        let store = Store::open_in_memory().unwrap();
        let allocator = PortAllocator::new(&store, table, config).with_options(AllocatorOptions {
            bind_check: false,
            ..AllocatorOptions::default()
        });
        (store, allocator)
    }

    #[tokio::test]
    async fn allocates_in_order_skipping_reserved_taken_and_listened_ports() {
        let table = Arc::new(FakeTable::default());
        table.listen(41002, 999, Some(r"C:\other\server.exe"));
        let (_store, ports) = allocator(
            Arc::clone(&table),
            config(&[("test", "41000-41005")], &[41001]),
        );
        let a = ports.allocate("test", owner("a")).await.unwrap();
        assert_eq!(a.port, 41000);
        assert_eq!(a.range.as_deref(), Some("test"));
        assert_eq!(a.state, PortState::Reserved);
        let b = ports.allocate("test", owner("b")).await.unwrap();
        assert_eq!(b.port, 41003, "41001 reserved, 41002 listened on");
        let again = ports.allocate("test", owner("a")).await.unwrap();
        assert_eq!(again, a, "idempotent per owner");
        ports.allocate("test", owner("c")).await.unwrap();
        ports.allocate("test", owner("d")).await.unwrap();
        let err = ports.allocate("test", owner("e")).await.unwrap_err();
        assert_eq!(err.code(), "conflict", "{err}");
        let err = ports.allocate("nope", owner("e")).await.unwrap_err();
        assert_eq!(err.code(), "not_found");
        assert_eq!(ports.list().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn new_ranges_apply_live_to_every_clone_and_keep_existing_reservations() {
        let table = Arc::new(FakeTable::default());
        let (_store, ports) = allocator(table, config(&[("web", "41080, 41443")], &[]));
        let clone = ports.clone();
        let a = ports.allocate("web", owner("a")).await.unwrap();
        assert_eq!(a.port, 41080);
        assert!(clone.in_range("web", 41443));
        assert!(!clone.in_range("web", 42080));
        assert!(!clone.in_range("nope", 41080));

        // `forge config set ports.ranges.web [42080, 42443]` while the daemon runs.
        ports.set_config(config(&[("web", "42080, 42443")], &[]));
        assert!(clone.in_range("web", 42080), "clones see the new ranges");
        assert!(!clone.in_range("web", 41080));
        let b = clone.allocate("web", owner("b")).await.unwrap();
        assert_eq!(b.port, 42080, "new allocations use the new range");
        let again = clone.allocate("web", owner("a")).await.unwrap();
        assert_eq!(again.port, 41080, "an owner keeps its port");
    }

    #[tokio::test]
    async fn serves_pools_through_the_core_trait() {
        let table = Arc::new(FakeTable::default());
        let (_store, ports) = allocator(table, config(&[("php-pools", "19000-19009")], &[]));
        let ports: Arc<dyn PortReservations> = Arc::new(ports);
        let pool = "01J0000000000000000000POOL";
        let worker = |slot: u32| {
            ReservationOwner::new(OwnerKind::Pool, pool, format!("worker-{slot}")).unwrap()
        };
        let a = ports.allocate("php-pools", worker(1)).await.unwrap();
        let b = ports.allocate("php-pools", worker(2)).await.unwrap();
        assert_eq!((a.port, b.port), (19000, 19001));
        let again = ports.allocate("php-pools", worker(1)).await.unwrap();
        assert_eq!(again.port, 19000);
        let released = ports.release(&worker(1)).await.unwrap().unwrap();
        assert_eq!(released.port, 19000);
        assert_eq!(ports.release(&worker(1)).await.unwrap(), None);
        ports.allocate("php-pools", worker(3)).await.unwrap();
        let removed = ports.release_owner(OwnerKind::Pool, pool).await.unwrap();
        assert_eq!(removed, 2);
    }

    #[tokio::test]
    async fn concurrent_allocations_never_share_a_port() {
        let table = Arc::new(FakeTable::default());
        let (_store, ports) = allocator(table, config(&[("test", "42000-42099")], &[]));
        let mut tasks = Vec::new();
        for i in 0..40 {
            let ports = ports.clone();
            tasks.push(tokio::spawn(async move {
                ports
                    .allocate("test", owner(&format!("o{i}")))
                    .await
                    .unwrap()
                    .port
            }));
        }
        let mut seen = BTreeSet::new();
        for task in tasks {
            assert!(seen.insert(task.await.unwrap()), "duplicate port");
        }
        assert_eq!(seen.len(), 40);
    }

    #[tokio::test]
    async fn reserve_moves_release_frees_and_others_conflict() {
        let table = Arc::new(FakeTable::default());
        let (_store, ports) = allocator(Arc::clone(&table), config(&[("xdebug", "9003")], &[9003]));
        let err = ports.allocate("xdebug", owner("x")).await.unwrap_err();
        assert_eq!(err.code(), "conflict", "reserved ports are never allocated");
        let daemon = ReservationOwner::new(OwnerKind::Daemon, "xdebug", "main").unwrap();
        let xdebug = ports
            .reserve(9003, Some("xdebug"), daemon.clone())
            .await
            .unwrap();
        assert_eq!(xdebug.port, 9003);
        assert_eq!(
            ports.reserve(9003, None, daemon.clone()).await.unwrap(),
            xdebug
        );
        let err = ports.reserve(9003, None, owner("a")).await.unwrap_err();
        assert_eq!(err.code(), "conflict");

        let first = ports.reserve(43000, None, owner("a")).await.unwrap();
        let moved = ports.reserve(43001, None, owner("a")).await.unwrap();
        assert_eq!(moved.port, 43001);
        assert_eq!(moved.created_at, first.created_at);
        assert_eq!(ports.get(&owner("a")).await.unwrap().unwrap().port, 43001);

        table.listen(43002, 4412, Some(r"C:\xampp\mysql\bin\mysqld.exe"));
        let err = ports.reserve(43002, None, owner("b")).await.unwrap_err();
        assert_eq!(err.code(), "port_in_use");
        assert!(err.to_string().contains("mysqld.exe (PID 4412)"), "{err}");

        assert_eq!(
            ports.release(&owner("a")).await.unwrap().unwrap().port,
            43001
        );
        assert_eq!(ports.release(&owner("a")).await.unwrap(), None);
        ports.reserve(43001, None, owner("b")).await.unwrap();
        assert_eq!(
            ports.release_owner(OwnerKind::Instance, "b").await.unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn conflicts_track_owner_external_and_free_ports() {
        let table = Arc::new(FakeTable::default());
        let (_store, ports) = allocator(Arc::clone(&table), config(&[], &[]));
        let mysql = ports.reserve(3306, None, owner("mysql")).await.unwrap();
        let redis = ports.reserve(6379, None, owner("redis")).await.unwrap();
        ports.reserve(8025, None, owner("mail")).await.unwrap();

        // mysql's own process (pid 10) listens; an external one sits on redis' port.
        table.listen(3306, 10, Some(r"C:\Forgebox\bin\mysql\8.4\bin\mysqld.exe"));
        table.listen(6379, 4242, Some(r"C:\Redis\redis-server.exe"));
        let is_owner = |o: &ReservationOwner, pid: u32| o.id == "mysql" && pid == 10;
        let found = ports.conflicts(is_owner).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].reservation.id, redis.id);
        assert_eq!(found[0].holder.pid, 4242);
        assert!(found[0].newly_detected);
        let stored = ports.get(&owner("redis")).await.unwrap().unwrap();
        assert_eq!(stored.state, PortState::Conflict);
        assert_eq!(
            stored.conflict,
            Some(PortHolder {
                pid: 4242,
                exe: Some(r"C:\Redis\redis-server.exe".into())
            })
        );
        let stored = ports.get(&owner("mysql")).await.unwrap().unwrap();
        assert_eq!((stored.id, stored.state), (mysql.id, PortState::Bound));

        let again = ports.conflicts(is_owner).await.unwrap();
        assert_eq!(again.len(), 1);
        assert!(!again[0].newly_detected, "same holder as before");

        table.clear();
        assert!(ports.conflicts(is_owner).await.unwrap().is_empty());
        for r in ports.list().await.unwrap() {
            assert_eq!(r.state, PortState::Reserved, "{r:?}");
            assert_eq!(r.conflict, None, "{r:?}");
        }
    }
}
