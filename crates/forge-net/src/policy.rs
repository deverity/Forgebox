//! How Windows reaches the Forgebox resolver (docs/06 §5.1, ADR-0004, ADR-0033): the async
//! façade over [`forge_core::platform::DnsPolicy`].
//!
//! Order of preference:
//! 1. **NRPT** (`nrpt.add` through the helper, one UAC prompt per TLD): `.test` goes to
//!    the resolver wherever it listens on port 53 (`127.0.0.1` or `127.0.0.53`). The rule
//!    only counts once the OS resolver **really follows it**: a random name under the TLD
//!    (`forgebox-probe-<ulid>.test`, answered only by the Forgebox resolver) must resolve
//!    to loopback through the OS resolver within [`PolicyTimeouts::probe_window`], with
//!    the cache flushed before every try.
//! 2. **`hosts` fallback** (`hosts.set_block`): when the resolver could not get port 53,
//!    when the OS has no NRPT, when the Group Policy NRPT table exists (even empty, the
//!    local rules are then ignored; no `nrpt.add` is attempted) or when the probe fails
//!    (VPN or security agent intercepting DNS). One line per host and alias, `127.0.0.1`
//!    and `::1`, no wildcard; kept in step with the sites by [`DnsPolicyManager::sync_sites`].
//!    The mode is persisted (`state/dns-policy.json`, [`DnsPolicyManager::restore`]) so it
//!    survives restarts even before the first site. That file is the **only** thing that
//!    arms it: the `hosts` file is shared by every root of the machine and its block does
//!    not say which root wrote it, so a block found there never arms the mode (a second
//!    root, a portable one or the smoke, would otherwise rewrite the block of another
//!    root, with a UAC prompt nobody asked for).
//! 3. Nothing: the diagnosis says why.
//!
//! Every change is followed by a resolver cache flush and published as
//! `dns.policy_applied {v, tld, policy, mode, reason, probe_ok}`; every helper call as
//! `helper.op {v, name, ok, error, effective, reason}` (same shapes as
//! `forge_ipc::api::events::{DnsPolicyPayload, HelperOpPayload}`).
//!
//! `enable`, `disable` and the site sync take turns (one-permit semaphore with a deadline):
//! a sync waiting for its prompt never writes the block back after a `disable`.
//!
//! Blocking calls run in `spawn_blocking` with timeouts: reads within
//! [`PolicyTimeouts::read`], each probe within [`PolicyTimeouts::probe_attempt`], helper
//! calls within [`PolicyTimeouts::helper`] (the UAC prompt waits for a human). Long
//! operations are meant to run inside a job of the daemon (`net.dns_enable` is a job,
//! docs/09).

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use forge_core::dns_name;
use forge_core::error::{ForgeError, Result};
use forge_core::event::{EventBus, EventKind};
use forge_core::helper::HostsEntry;
use forge_core::helper::hosts::read_hosts_block;
use forge_core::platform::{
    DnsPolicy, DnsPolicyStatus, NrptApplied, NrptIneffective, NrptSource, NrptTable, policy_status,
};
use forge_core::runtime::spawn_blocking_timeout;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Version of the `dns.policy_applied` and `helper.op` payloads (fields added since are
/// optional, ADR-0033).
pub const POLICY_EVENT_VERSION: u32 = 1;

/// Prefix of the probe names (`forgebox-probe-<ulid>.test`).
pub const PROBE_PREFIX: &str = "forgebox-probe-";

/// How a TLD reaches the resolver (wire values of `forge_ipc` `DnsPolicyState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyState {
    /// An NRPT rule the OS resolver follows sends the TLD to the resolver.
    Applied,
    /// The sites are in the `hosts` managed block.
    HostsFallback,
    NotApplied,
}

impl PolicyState {
    /// The effective mode (wire values of `forge_ipc` `DnsMode`).
    pub const fn mode(self) -> PolicyMode {
        match self {
            Self::Applied => PolicyMode::Nrpt,
            Self::HostsFallback => PolicyMode::Hosts,
            Self::NotApplied => PolicyMode::None,
        }
    }
}

/// Which path really routes a TLD (wire values of `forge_ipc` `DnsMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    Nrpt,
    Hosts,
    None,
}

/// Why a TLD is not routed by NRPT (wire values of `forge_ipc` `DnsPolicyReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyReason {
    /// The Group Policy NRPT table exists, even empty: Windows ignores local rules.
    GroupPolicyTablePresent,
    /// The rule is in the registry, but the OS resolver did not answer a probe name of
    /// the TLD with loopback.
    ResolverProbeFailed,
    /// The Forgebox resolver is not on port 53.
    ResolverNotOnPort53,
    /// The OS has no NRPT.
    NrptUnsupported,
    /// An effective rule sends the TLD to another server.
    NrptPointsElsewhere,
}

impl PolicyReason {
    /// Wire value (`group_policy_table_present`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GroupPolicyTablePresent => "group_policy_table_present",
            Self::ResolverProbeFailed => "resolver_probe_failed",
            Self::ResolverNotOnPort53 => "resolver_not_on_port53",
            Self::NrptUnsupported => "nrpt_unsupported",
            Self::NrptPointsElsewhere => "nrpt_points_elsewhere",
        }
    }
}

impl From<NrptIneffective> for PolicyReason {
    fn from(reason: NrptIneffective) -> Self {
        match reason {
            NrptIneffective::GroupPolicyTablePresent => Self::GroupPolicyTablePresent,
        }
    }
}

/// One verification of a TLD through the OS resolver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeResult {
    /// Name of the last try (`forgebox-probe-<ulid>.test`).
    pub host: String,
    /// Resolved, and only to loopback.
    pub ok: bool,
    /// What the last try returned.
    pub resolved: Vec<IpAddr>,
    /// Last error of the OS resolver.
    pub error: Option<String>,
    pub attempts: u32,
    pub elapsed: Duration,
}

/// State of one TLD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TldPolicy {
    /// `.test`.
    pub tld: String,
    pub state: PolicyState,
    /// Where the NRPT rule points, when there is one (effective or not).
    pub nameserver: Option<IpAddr>,
    /// Why the TLD is not routed by NRPT (`None` when it is, or when nothing was set up).
    pub reason: Option<PolicyReason>,
    /// The Group Policy NRPT table exists on the machine.
    pub group_policy_table: bool,
    /// Verification through the OS resolver, when there was a rule to verify.
    pub probe: Option<ProbeResult>,
}

impl TldPolicy {
    fn new(tld: &str, state: PolicyState) -> Self {
        Self {
            tld: tld.to_owned(),
            state,
            nameserver: None,
            reason: None,
            group_policy_table: false,
            probe: None,
        }
    }
}

/// What [`DnsPolicyManager::enable`] should set up.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnableRequest {
    /// `[".test"]`.
    pub tlds: Vec<String>,
    /// Address the resolver listens on **when it is port 53** (`DnsStats::nrpt_capable`);
    /// `None` forces the `hosts` fallback.
    pub nameserver: Option<IpAddr>,
    /// Every site and alias for the `hosts` fallback (`loja.test`, `api.loja.test`).
    pub hosts: Vec<String>,
}

/// Timeouts of the blocking calls and of the verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyTimeouts {
    /// Registry and `hosts` reads, cache flush.
    pub read: Duration,
    /// A helper run, UAC prompt included.
    pub helper: Duration,
    /// One query to the OS resolver.
    pub probe_attempt: Duration,
    /// How long `enable` keeps probing a fresh rule before falling back to `hosts`.
    pub probe_window: Duration,
    /// Pause between two probes.
    pub probe_interval: Duration,
    /// How long `enable`, `disable` and the site sync wait for each other (one at a time;
    /// each may hold a UAC prompt or two).
    pub turn: Duration,
}

impl Default for PolicyTimeouts {
    fn default() -> Self {
        Self {
            read: Duration::from_secs(5),
            helper: Duration::from_secs(15 * 60),
            probe_attempt: Duration::from_secs(3),
            probe_window: Duration::from_secs(8),
            probe_interval: Duration::from_secs(1),
            turn: Duration::from_secs(31 * 60),
        }
    }
}

#[derive(Serialize)]
struct DnsPolicyApplied<'a> {
    v: u32,
    tld: &'a str,
    policy: PolicyState,
    mode: PolicyMode,
    reason: Option<PolicyReason>,
    probe_ok: Option<bool>,
}

#[derive(Serialize)]
struct HelperOpEvent<'a> {
    v: u32,
    name: &'a str,
    ok: bool,
    error: Option<String>,
    effective: Option<bool>,
    reason: Option<&'static str>,
}

/// Version of `state/dns-policy.json`.
const STATE_VERSION: u32 = 1;

/// `state/dns-policy.json`: the `hosts` mode survives restarts even with no site (and so
/// no block) yet (ADR-0033).
#[derive(Debug, Default, Serialize, Deserialize)]
struct SavedState {
    v: u32,
    /// TLDs routed by the `hosts` block (`[".test"]`).
    #[serde(default)]
    hosts_mode: Vec<String>,
}

fn read_state(path: &Path) -> Result<SavedState> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|err| ForgeError::InvalidInput(format!("{}: {err}", path.display()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(SavedState::default()),
        Err(err) => Err(ForgeError::io_at("reading", path, err)),
    }
}

fn write_state(path: &Path, state: &SavedState) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|err| ForgeError::io_at("creating", dir, err))?;
    }
    let mut bytes = serde_json::to_vec_pretty(state)
        .map_err(|err| ForgeError::Internal(format!("dns policy state: {err}")))?;
    bytes.push(b'\n');
    forge_core::paths::write_atomic(path, &bytes)
}

/// Takes the single permit of `turn` within `timeout`.
async fn acquire(
    turn: &Arc<Semaphore>,
    timeout: Duration,
    what: &str,
) -> Result<OwnedSemaphorePermit> {
    tokio::time::timeout(timeout, Arc::clone(turn).acquire_owned())
        .await
        .map_err(|_| ForgeError::Timeout {
            operation: what.to_owned(),
            after: timeout,
        })?
        .map_err(|_| ForgeError::Internal("dns policy turn closed".into()))
}

/// Async façade over the OS resolver policy. Cheap to clone.
#[derive(Debug, Clone)]
pub struct DnsPolicyManager {
    policy: Arc<dyn DnsPolicy>,
    events: Option<EventBus>,
    timeouts: PolicyTimeouts,
    /// TLDs routed by the `hosts` block: set by `enable` (fallback) and by
    /// [`Self::restore`] (this root's state file), cleared by `disable` and by an `enable`
    /// that verified NRPT. Never by a block found in `hosts` (it may be another root's).
    /// [`Self::sync_sites`] keeps their lines in step. Never held across `.await`.
    hosts_mode: Arc<Mutex<BTreeSet<String>>>,
    /// Where `hosts_mode` is persisted (`state/dns-policy.json`); `None` keeps it in
    /// memory only.
    state_file: Option<PathBuf>,
    /// One `enable`/`disable`/site sync at a time (docs/03 §7.2 "fila de vez"): a sync
    /// waiting for its UAC prompt never writes the block back after a `disable`.
    turn: Arc<Semaphore>,
    /// One write of `state_file` at a time, each with the latest set.
    persist_turn: Arc<Semaphore>,
}

/// The `hosts` entries for `hosts` under `tlds`: `127.0.0.1` and `::1` for each valid
/// name below a TLD; the rest is skipped with a warning (the helper would refuse the
/// whole block).
pub fn hosts_entries(hosts: &[String], tlds: &[String]) -> Vec<HostsEntry> {
    let suffixes: Vec<&str> = tlds
        .iter()
        .filter_map(|t| dns_name::parse_suffix(t).ok())
        .collect();
    let mut names = BTreeSet::new();
    for host in hosts {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        let valid = dns_name::check_name(&host).is_ok()
            && suffixes
                .iter()
                .any(|s| host != *s && dns_name::is_within(&host, s));
        if valid {
            names.insert(host);
        } else {
            tracing::warn!(%host, "dns: host left out of the hosts block (not below a TLD)");
        }
    }
    names
        .into_iter()
        .flat_map(|host| {
            [
                HostsEntry::new(IpAddr::V4(Ipv4Addr::LOCALHOST), host.clone()),
                HostsEntry::new(IpAddr::V6(Ipv6Addr::LOCALHOST), host),
            ]
        })
        .collect()
}

/// A fresh probe name under `tld`: `forgebox-probe-<ulid>.test` (lowercase). Never cached,
/// never in `hosts`; the Forgebox resolver answers any name of its zones with loopback.
pub fn probe_name(tld: &str) -> String {
    format!(
        "{PROBE_PREFIX}{}.{}",
        ulid::Ulid::generate().to_string().to_ascii_lowercase(),
        tld.trim_start_matches('.').to_ascii_lowercase()
    )
}

/// What a helper call reported, for `helper.op`.
#[derive(Debug, Clone, Copy, Default)]
struct HelperNote {
    effective: Option<bool>,
    reason: Option<NrptIneffective>,
}

impl From<NrptApplied> for HelperNote {
    fn from(applied: NrptApplied) -> Self {
        Self {
            effective: applied.effective,
            reason: applied.reason,
        }
    }
}

impl DnsPolicyManager {
    pub fn new(policy: Arc<dyn DnsPolicy>) -> Self {
        Self {
            policy,
            events: None,
            timeouts: PolicyTimeouts::default(),
            hosts_mode: Arc::default(),
            state_file: None,
            turn: Arc::new(Semaphore::new(1)),
            persist_turn: Arc::new(Semaphore::new(1)),
        }
    }

    /// Persists the `hosts` mode in `path` (`state/dns-policy.json`); load it with
    /// [`Self::restore`].
    pub fn with_state_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_file = Some(path.into());
        self
    }

    /// Publishes `dns.policy_applied` and `helper.op` on `bus`.
    pub fn with_events(mut self, bus: EventBus) -> Self {
        self.events = Some(bus);
        self
    }

    pub fn with_timeouts(mut self, timeouts: PolicyTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// TLDs currently routed by the `hosts` block.
    pub fn hosts_mode_tlds(&self) -> Vec<String> {
        self.hosts_mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// Arms or clears the `hosts` mode of `tld`; returns whether it changed.
    fn set_hosts_mode(&self, tld: &str, on: bool) -> bool {
        let mut mode = self
            .hosts_mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let tld = tld.to_ascii_lowercase();
        if on {
            mode.insert(tld)
        } else {
            mode.remove(&tld)
        }
    }

    /// Clears `tlds` (`disable`).
    fn clear_hosts_mode(&self, tlds: &[String]) {
        let mut mode = self
            .hosts_mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for tld in tlds {
            mode.remove(&tld.to_ascii_lowercase());
        }
    }

    /// Waits for the turn of `enable`/`disable`/site sync.
    async fn turn(&self) -> Result<OwnedSemaphorePermit> {
        acquire(
            &self.turn,
            self.timeouts.turn,
            "waiting for another DNS policy change",
        )
        .await
    }

    /// Loads the `hosts` mode saved by an earlier run (boot, before the first `status`):
    /// a TLD that fell back to `hosts` keeps following the sites after a restart even when
    /// no block was written yet (no site) or the last site went away. Returns the TLDs
    /// armed. No state file (or none configured): nothing.
    pub async fn restore(&self) -> Result<Vec<String>> {
        let Some(path) = self.state_file.clone() else {
            return Ok(Vec::new());
        };
        let saved = spawn_blocking_timeout(
            "reading the DNS policy state",
            self.timeouts.read,
            move || read_state(&path),
        )
        .await?;
        let tlds: Vec<String> = saved
            .hosts_mode
            .iter()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        for tld in &tlds {
            self.set_hosts_mode(tld, true);
        }
        if !tlds.is_empty() {
            tracing::info!(?tlds, "dns: hosts mode restored");
        }
        Ok(tlds)
    }

    /// Saves the current `hosts` mode (when a state file is configured); a failure is only
    /// logged (the mode stays right in memory until the next restart).
    async fn persist(&self) {
        let Some(path) = self.state_file.clone() else {
            return;
        };
        let result = async {
            let _permit = acquire(
                &self.persist_turn,
                self.timeouts.read,
                "saving the DNS policy state",
            )
            .await?;
            // The snapshot is taken inside the turn: the last writer saves the latest set.
            let state = SavedState {
                v: STATE_VERSION,
                hosts_mode: self.hosts_mode_tlds(),
            };
            spawn_blocking_timeout(
                "saving the DNS policy state",
                self.timeouts.read,
                move || write_state(&path, &state),
            )
            .await
        }
        .await;
        if let Err(err) = result {
            tracing::warn!(%err, "dns: hosts mode not saved");
        }
    }

    async fn read<T: Send + 'static>(
        &self,
        what: &str,
        f: impl FnOnce(&dyn DnsPolicy) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let policy = Arc::clone(&self.policy);
        spawn_blocking_timeout(what, self.timeouts.read, move || f(policy.as_ref())).await
    }

    async fn table(&self) -> Result<NrptTable> {
        self.read("reading NRPT rules", |p| p.nrpt_table()).await
    }

    /// Runs one helper operation and publishes `helper.op`.
    async fn helper<T: Into<HelperNote> + Copy + Send + 'static>(
        &self,
        op: &'static str,
        f: impl FnOnce(&dyn DnsPolicy) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let policy = Arc::clone(&self.policy);
        let result =
            spawn_blocking_timeout(op, self.timeouts.helper, move || f(policy.as_ref())).await;
        if let Some(bus) = &self.events {
            let note: HelperNote = result.as_ref().map(|t| (*t).into()).unwrap_or_default();
            let payload = HelperOpEvent {
                v: POLICY_EVENT_VERSION,
                name: op,
                ok: result.is_ok(),
                error: result.as_ref().err().map(ToString::to_string),
                effective: note.effective,
                reason: note.reason.map(NrptIneffective::as_str),
            };
            bus.publish(
                EventKind::HelperOp,
                serde_json::to_value(payload).unwrap_or_default(),
            );
        }
        result
    }

    fn publish_state(&self, state: &TldPolicy) {
        if let Some(bus) = &self.events {
            let payload = DnsPolicyApplied {
                v: POLICY_EVENT_VERSION,
                tld: &state.tld,
                policy: state.state,
                mode: state.state.mode(),
                reason: state.reason,
                probe_ok: state.probe.as_ref().map(|p| p.ok),
            };
            bus.publish(
                EventKind::DnsPolicyApplied,
                serde_json::to_value(payload).unwrap_or_default(),
            );
        }
    }

    async fn current_block(&self) -> Result<Option<Vec<HostsEntry>>> {
        let text = self.read("reading hosts", |p| p.read_hosts()).await?;
        Ok(read_hosts_block(&text)?.map(|b| b.entries))
    }

    /// Asks the OS resolver for fresh probe names under `tld` until one resolves to
    /// loopback or `window` is over (at least one try), flushing the cache between tries.
    pub async fn probe(&self, tld: &str, window: Duration) -> ProbeResult {
        let started = Instant::now();
        let mut result = ProbeResult {
            host: String::new(),
            ok: false,
            resolved: Vec::new(),
            error: None,
            attempts: 0,
            elapsed: Duration::ZERO,
        };
        loop {
            if result.attempts > 0 {
                // Between tries: drop whatever the DNS Client cached meanwhile (negative
                // answers of the site names included). The first try needs none: every
                // probe name is new.
                self.flush().await;
            }
            let host = probe_name(tld);
            let policy = Arc::clone(&self.policy);
            let query = host.clone();
            let answer = spawn_blocking_timeout(
                "asking the OS resolver",
                self.timeouts.probe_attempt,
                move || policy.query_system(&query),
            )
            .await;
            result.attempts += 1;
            result.host = host;
            match answer {
                Ok(ips) => {
                    result.ok = !ips.is_empty() && ips.iter().all(IpAddr::is_loopback);
                    result.error = if result.ok {
                        None
                    } else if ips.is_empty() {
                        Some("no address".to_owned())
                    } else {
                        Some("answered with addresses other than loopback".to_owned())
                    };
                    result.resolved = ips;
                }
                Err(err) => {
                    result.resolved = Vec::new();
                    result.error = Some(err.to_string());
                }
            }
            if result.ok || started.elapsed() + self.timeouts.probe_interval >= window {
                break;
            }
            tokio::time::sleep(self.timeouts.probe_interval).await;
        }
        result.elapsed = started.elapsed();
        if result.ok {
            tracing::info!(%tld, attempts = result.attempts, "dns: the OS resolver follows the NRPT rule");
        } else {
            tracing::warn!(%tld, host = %result.host, error = ?result.error, resolved = ?result.resolved, attempts = result.attempts, "dns: the OS resolver does not follow the NRPT rule");
        }
        result
    }

    /// State of each TLD. `nameserver` is where the resolver listens now on port 53
    /// (`None`: not running or not on 53, nothing to verify against). A rule that points
    /// elsewhere, that Windows ignores (Group Policy table) or that the OS resolver does
    /// not follow (one probe) does not count as applied. `hosts` is reported only for a
    /// TLD this root armed (state file or `enable`); read-only: it never arms, writes or
    /// prompts.
    pub async fn status(
        &self,
        tlds: &[String],
        nameserver: Option<IpAddr>,
    ) -> Result<Vec<TldPolicy>> {
        let armed = self.hosts_mode_tlds();
        let table = self.table().await?;
        let mut out = Vec::new();
        for tld in tlds {
            let mut policy = TldPolicy::new(tld, PolicyState::NotApplied);
            policy.group_policy_table = table.group_policy_table;
            let mut nrpt = false;
            match policy_status(&table, tld) {
                DnsPolicyStatus::Applied { nameserver: ns } => {
                    policy.nameserver = Some(ns);
                    match nameserver {
                        Some(want) if want != ns => {
                            policy.reason = Some(PolicyReason::NrptPointsElsewhere);
                        }
                        Some(_) => {
                            let probe = self.probe(tld, Duration::ZERO).await;
                            nrpt = probe.ok;
                            if !nrpt {
                                policy.reason = Some(PolicyReason::ResolverProbeFailed);
                            }
                            policy.probe = Some(probe);
                        }
                        None => nrpt = true,
                    }
                }
                DnsPolicyStatus::Ignored {
                    nameserver: ns,
                    reason,
                } => {
                    policy.nameserver = Some(ns);
                    policy.reason = Some(reason.into());
                }
                DnsPolicyStatus::NotApplied => {
                    policy.reason = table.local_rules_ignored().map(PolicyReason::from);
                }
            }
            // Only this root's own mode: a block in the shared `hosts` file may belong to
            // another root and is not ours to follow.
            let is_armed = armed.contains(&tld.to_ascii_lowercase());
            policy.state = if nrpt {
                PolicyState::Applied
            } else if is_armed {
                PolicyState::HostsFallback
            } else {
                PolicyState::NotApplied
            };
            if nrpt {
                policy.reason = None;
            } else if policy.state == PolicyState::HostsFallback
                && policy.reason.is_none()
                && nameserver.is_none()
            {
                policy.reason = Some(PolicyReason::ResolverNotOnPort53);
            }
            out.push(policy);
        }
        Ok(out)
    }

    /// Routes the TLDs to the resolver (`forge dns enable`): NRPT when Windows follows it
    /// (checked by probing the OS resolver), the `hosts` block otherwise; then flushes the
    /// cache. Already correct TLDs cost no prompt; with the Group Policy table present no
    /// NRPT rule is attempted (it would be ignored).
    ///
    /// Errors: `needs_elevation` when the user declines a prompt (nothing else is tried:
    /// the fallback needs the same consent), the helper's refusals, timeouts.
    pub async fn enable(&self, req: EnableRequest) -> Result<Vec<TldPolicy>> {
        let _turn = self.turn().await?;
        let mut states: Vec<TldPolicy> = Vec::new();
        let mut needs_hosts: Vec<TldPolicy> = Vec::new();
        for tld in &req.tlds {
            let table = self.table().await?;
            let mut policy = TldPolicy::new(tld, PolicyState::Applied);
            policy.group_policy_table = table.group_policy_table;
            let Some(nameserver) = req.nameserver else {
                policy.reason = Some(PolicyReason::ResolverNotOnPort53);
                needs_hosts.push(policy);
                continue;
            };
            let current = policy_status(&table, tld);
            if current != (DnsPolicyStatus::Applied { nameserver }) {
                if let Some(reason) = table.local_rules_ignored() {
                    // A local rule would be ignored: no prompt for it.
                    tracing::warn!(%tld, %reason, "dns: local NRPT rules are ignored on this machine; using hosts");
                    policy.reason = Some(reason.into());
                    if let DnsPolicyStatus::Ignored { nameserver: ns, .. } = current {
                        policy.nameserver = Some(ns);
                    }
                    needs_hosts.push(policy);
                    continue;
                }
                let t = tld.clone();
                let applied = match self
                    .helper("nrpt.add", move |p| p.apply(&t, nameserver))
                    .await
                {
                    Ok(applied) => applied,
                    Err(err) if err.code() == "unsupported_platform" => {
                        policy.reason = Some(PolicyReason::NrptUnsupported);
                        needs_hosts.push(policy);
                        continue;
                    }
                    Err(err) => return Err(err),
                };
                policy.nameserver = Some(nameserver);
                let after = policy_status(&self.table().await?, tld);
                let ignored = match after {
                    DnsPolicyStatus::Ignored { reason, .. } => Some(reason),
                    _ if applied.effective == Some(false) => applied.reason,
                    _ => None,
                };
                if let Some(reason) = ignored {
                    tracing::warn!(%tld, %reason, "dns: NRPT rule added but ignored by Windows; using hosts");
                    policy.reason = Some(reason.into());
                    needs_hosts.push(policy);
                    continue;
                }
            }
            policy.nameserver = Some(nameserver);
            let probe = self.probe(tld, self.timeouts.probe_window).await;
            let ok = probe.ok;
            policy.probe = Some(probe);
            if ok {
                states.push(policy);
            } else {
                policy.reason = Some(PolicyReason::ResolverProbeFailed);
                needs_hosts.push(policy);
            }
        }
        let mut changed = false;
        for routed in &states {
            changed |= self.set_hosts_mode(&routed.tld, false);
        }
        if changed && needs_hosts.is_empty() && self.hosts_mode_tlds().is_empty() {
            // This root just left hosts mode for a verified NRPT (the VPN went away): its
            // block would otherwise stay, stale, until `disable`, since `sync_sites` no
            // longer touches it. One prompt, only when a block is there. The mode is saved
            // first: a declined prompt must not bring hosts mode back at the next boot.
            self.persist().await;
            changed = false;
            if self.write_hosts_if_changed(&[], &[]).await? {
                tracing::info!("dns: NRPT verified; hosts block removed");
            }
        }
        if !needs_hosts.is_empty() {
            // Every TLD that stays in hosts mode, not only the new ones: the block is
            // rewritten as a whole.
            let mut tlds: BTreeSet<String> = self.hosts_mode_tlds().into_iter().collect();
            tlds.extend(needs_hosts.iter().map(|p| p.tld.to_ascii_lowercase()));
            let tlds: Vec<String> = tlds.into_iter().collect();
            if hosts_entries(&req.hosts, &tlds).is_empty() {
                // No site yet: the block is written when the first one arrives
                // (`sync_sites`; the mode is persisted, so also after a restart).
                tracing::info!(?tlds, "dns: hosts mode with no site yet");
            }
            // Only a different block costs a prompt (running `enable` again does not).
            self.write_hosts_if_changed(&req.hosts, &tlds).await?;
            for mut policy in needs_hosts {
                changed |= self.set_hosts_mode(&policy.tld, true);
                policy.state = PolicyState::HostsFallback;
                states.push(policy);
            }
        }
        if changed {
            self.persist().await;
        }
        self.flush().await;
        for state in &states {
            self.publish_state(state);
        }
        Ok(states)
    }

    /// Removes the routing (`forge dns disable`): the Forgebox NRPT rules of the TLDs and
    /// the `hosts` block, then flushes the cache.
    pub async fn disable(&self, tlds: &[String]) -> Result<()> {
        let _turn = self.turn().await?;
        for tld in tlds {
            let table = self.table().await?;
            let wanted = tld.to_ascii_lowercase();
            let present = table
                .rules
                .iter()
                .any(|r| r.source == NrptSource::Local && r.namespaces.contains(&wanted));
            if present {
                let t = tld.clone();
                self.helper("nrpt.remove", move |p| p.remove(&t)).await?;
            }
        }
        if self.current_block().await?.is_some() {
            self.helper("hosts.set_block", |p| p.set_hosts_block(&[]))
                .await?;
        }
        self.clear_hosts_mode(tlds);
        self.persist().await;
        self.flush().await;
        for tld in tlds {
            self.publish_state(&TldPolicy::new(tld, PolicyState::NotApplied));
        }
        Ok(())
    }

    /// Keeps the `hosts` block in step with `hosts` (every site and alias) for the TLDs in
    /// `hosts` mode; nothing to do (no prompt) when no TLD uses it or the lines did not
    /// change. Returns whether it wrote.
    pub async fn sync_sites(&self, hosts: &[String]) -> Result<bool> {
        if self.hosts_mode_tlds().is_empty() {
            return Ok(false);
        }
        let _turn = self.turn().await?;
        // Read again inside the turn: a `disable` that ran meanwhile cleared the mode,
        // and the block must not come back.
        let tlds = self.hosts_mode_tlds();
        if tlds.is_empty() {
            return Ok(false);
        }
        let wrote = self.write_hosts_if_changed(hosts, &tlds).await?;
        if wrote {
            self.flush().await;
        }
        Ok(wrote)
    }

    /// Rewrites the `hosts` block for `hosts` under `tlds` (one prompt) only when the
    /// entries change; no entry left removes the block. Returns whether it wrote.
    pub async fn sync_hosts(&self, hosts: &[String], tlds: &[String]) -> Result<bool> {
        let _turn = self.turn().await?;
        let wrote = self.write_hosts_if_changed(hosts, tlds).await?;
        if wrote {
            self.flush().await;
        }
        Ok(wrote)
    }

    /// The block for `hosts` under `tlds`, written (one prompt) only when it differs from
    /// the current one; no entry left removes the block. No flush. Returns whether it
    /// wrote.
    async fn write_hosts_if_changed(&self, hosts: &[String], tlds: &[String]) -> Result<bool> {
        let wanted = hosts_entries(hosts, tlds);
        let current = self.current_block().await?.unwrap_or_default();
        let as_set = |v: &[HostsEntry]| v.iter().cloned().collect::<BTreeSet<_>>();
        if as_set(&wanted) == as_set(&current) {
            return Ok(false);
        }
        if wanted.is_empty() {
            self.helper("hosts.set_block", |p| p.set_hosts_block(&[]))
                .await?;
        } else {
            self.write_block(wanted).await?;
        }
        Ok(true)
    }

    async fn write_block(&self, entries: Vec<HostsEntry>) -> Result<()> {
        if entries.is_empty() {
            return Err(ForgeError::InvalidInput(
                "no site below the TLDs to write to hosts".to_owned(),
            ));
        }
        self.helper("hosts.set_block", move |p| p.set_hosts_block(&entries))
            .await
    }

    /// Flushes the OS resolver cache; a failure is only logged.
    pub async fn flush(&self) {
        if let Err(err) = self
            .read("flushing the DNS cache", |p| p.flush_cache())
            .await
        {
            tracing::warn!(%err, "dns: could not flush the resolver cache");
        }
    }
}

impl From<()> for HelperNote {
    fn from((): ()) -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_names_are_fresh_lowercase_and_under_the_tld() {
        let a = probe_name(".Test");
        let b = probe_name("test");
        assert_ne!(a, b);
        for name in [&a, &b] {
            assert!(name.starts_with(PROBE_PREFIX), "{name}");
            assert!(name.ends_with(".test"), "{name}");
            assert_eq!(*name, name.to_ascii_lowercase());
            dns_name::check_name(name).unwrap();
        }
    }

    #[test]
    fn modes_and_reasons_have_stable_wire_values() {
        assert_eq!(PolicyState::Applied.mode(), PolicyMode::Nrpt);
        assert_eq!(PolicyState::HostsFallback.mode(), PolicyMode::Hosts);
        assert_eq!(PolicyState::NotApplied.mode(), PolicyMode::None);
        for reason in [
            PolicyReason::GroupPolicyTablePresent,
            PolicyReason::ResolverProbeFailed,
            PolicyReason::ResolverNotOnPort53,
            PolicyReason::NrptUnsupported,
            PolicyReason::NrptPointsElsewhere,
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), reason.as_str());
        }
        assert_eq!(
            PolicyReason::from(NrptIneffective::GroupPolicyTablePresent),
            PolicyReason::GroupPolicyTablePresent
        );
    }

    #[test]
    fn hosts_entries_cover_both_families_and_skip_strangers() {
        let entries = hosts_entries(
            &[
                "Loja.test".into(),
                "api.loja.test.".into(),
                "loja.test".into(),
                "test".into(),
                "google.com".into(),
                "bad_name.test".into(),
            ],
            &[".test".into()],
        );
        let lines: Vec<String> = entries
            .iter()
            .map(|e| format!("{} {}", e.ip, e.host))
            .collect();
        assert_eq!(
            lines,
            vec![
                "127.0.0.1 api.loja.test",
                "::1 api.loja.test",
                "127.0.0.1 loja.test",
                "::1 loja.test",
            ]
        );
    }
}
