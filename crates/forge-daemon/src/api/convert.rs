//! Domain types → API DTOs (`forge_ipc::api`). Paths become informative `/` strings;
//! identities stay ULIDs (ADR-0009).

use forge_core::model::port::{OwnerKind, PortReservation, PortState as DomainPortState};
use forge_core::store::{
    Site, SiteKind as DomainSiteKind, SiteXdebug as DomainXdebug, ToolVersionRow, VersionSource,
    VersionState,
};
use forge_fcgi::{PoolState, PoolStatus, WorkerPhase};
use forge_ipc::api::{
    ApiError, CaStatusDto, CatalogKind, CertificateDto, FailureReasonDto, InstanceDto, PoolDto,
    PortDto, PortHolderDto, PortOwnerKind, PortProtocol, PortState, RegistryDto, RegistryKind,
    ServiceState, SiteDto, SiteKind, SitePhpDto, SiteXdebug, ToolVersionDto, ToolVersionSource,
    ToolVersionState, UpdatePolicy, XdebugMode,
};
use forge_manifest::schema::Kind;
use forge_registry::RegistryInfo;
use forge_supervisor::{FailureReason, InstanceState, InstanceStatus};
use forge_tls::{LeafInfo, PermittedSubtree, TlsStatus};

use crate::services::Entry;
use crate::web::SitePhp;

pub fn version_state(state: VersionState) -> ToolVersionState {
    match state {
        VersionState::Downloading => ToolVersionState::Downloading,
        VersionState::Installed => ToolVersionState::Installed,
        VersionState::Broken => ToolVersionState::Broken,
        VersionState::Removing => ToolVersionState::Removing,
    }
}

pub fn version_source(source: VersionSource) -> ToolVersionSource {
    match source {
        VersionSource::Registry => ToolVersionSource::Registry,
        VersionSource::Manual => ToolVersionSource::Manual,
        VersionSource::Import => ToolVersionSource::Import,
    }
}

/// `kind` of a catalog manifest (only tools, extensions and stacks are catalog entries;
/// the other kinds never reach the catalog and read as tools).
pub fn catalog_kind(kind: Kind) -> CatalogKind {
    match kind {
        Kind::Extension => CatalogKind::Extension,
        Kind::Stack => CatalogKind::Stack,
        Kind::Tool | Kind::Project | Kind::Registry => CatalogKind::Tool,
    }
}

/// An installed version; `current` = behind the `current` junction.
pub fn tool_version(row: &ToolVersionRow, current: bool) -> ToolVersionDto {
    ToolVersionDto {
        id: row.id,
        tool: row.tool_id.clone(),
        version: row.version.clone(),
        canonical_name: row.canonical_name.clone(),
        variant: row.variant.clone(),
        state: version_state(row.state),
        source: version_source(row.source),
        path: row.path.replace('\\', "/"),
        size_bytes: None,
        installed_at: None,
        is_current: current,
    }
}

/// Stored policy text → DTO (`None` = the manifest default, given by the caller).
pub fn update_policy(text: &str) -> Option<UpdatePolicy> {
    Some(match text {
        "manual" => UpdatePolicy::Manual,
        "notify" => UpdatePolicy::Notify,
        "auto-patch" => UpdatePolicy::AutoPatch,
        "auto-minor" => UpdatePolicy::AutoMinor,
        "auto" => UpdatePolicy::Auto,
        _ => return None,
    })
}

/// DTO policy → stored text.
pub fn update_policy_text(policy: UpdatePolicy) -> &'static str {
    match policy {
        UpdatePolicy::Manual => "manual",
        UpdatePolicy::Notify => "notify",
        UpdatePolicy::AutoPatch => "auto-patch",
        UpdatePolicy::AutoMinor => "auto-minor",
        UpdatePolicy::Auto => "auto",
    }
}

pub fn service_state(state: &InstanceState) -> ServiceState {
    match state {
        InstanceState::Stopped => ServiceState::Stopped,
        InstanceState::Starting => ServiceState::Starting,
        InstanceState::Running => ServiceState::Running,
        InstanceState::Stopping { .. } => ServiceState::Stopping,
        InstanceState::Crashed { .. } => ServiceState::Crashed,
        InstanceState::Failed { .. } => ServiceState::Failed,
        InstanceState::Maintenance => ServiceState::Maintenance,
    }
}

/// Same shape on both sides (`kind`-tagged); converted through JSON.
pub fn failure_reason(reason: &FailureReason) -> Option<FailureReasonDto> {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

fn failure_error(reason: &FailureReason) -> ApiError {
    let dto = failure_reason(reason);
    let code = match reason {
        FailureReason::PortInUse { .. } => "port_in_use",
        FailureReason::MissingDll { .. } | FailureReason::MissingRuntime { .. } => "not_found",
        FailureReason::ConfigInvalid { .. } => "config_invalid",
        FailureReason::DependencyNotRunning { .. } => "conflict",
        FailureReason::Permission { .. } => "needs_elevation",
        FailureReason::HealthTimeout { .. } => "timeout",
        _ => "internal",
    };
    ApiError {
        code: code.to_owned(),
        message: serde_json::to_string(reason).unwrap_or_default(),
        details: dto.and_then(|d| failure_details(&d)),
    }
}

/// `details` of a service failure: the `kind`-tagged diagnosis (API 1.0 shape, kept) plus
/// the `{reason, args}` convention (ADR-0039): `reason` = the kind (`port_in_use`,
/// `missing_dll`...), `args` = its other fields.
fn failure_details(dto: &FailureReasonDto) -> Option<serde_json::Value> {
    let mut value = serde_json::to_value(dto).ok()?;
    let map = value.as_object_mut()?;
    let reason = map.get("kind")?.clone();
    let args: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .filter(|(key, _)| key.as_str() != "kind")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    map.insert("reason".into(), reason);
    map.insert("args".into(), serde_json::Value::Object(args));
    Some(value)
}

pub fn port_owner_kind(kind: OwnerKind) -> PortOwnerKind {
    match kind {
        OwnerKind::Instance => PortOwnerKind::Instance,
        OwnerKind::Pool => PortOwnerKind::Pool,
        OwnerKind::App => PortOwnerKind::App,
        OwnerKind::Site => PortOwnerKind::Site,
        OwnerKind::Daemon => PortOwnerKind::Daemon,
    }
}

pub fn port(reservation: &PortReservation, owner_name: String) -> PortDto {
    PortDto {
        id: reservation.id,
        port: reservation.port,
        protocol: match reservation.protocol {
            forge_core::model::port::PortProtocol::Tcp => PortProtocol::Tcp,
            forge_core::model::port::PortProtocol::Udp => PortProtocol::Udp,
        },
        owner_kind: port_owner_kind(reservation.owner.kind),
        owner_id: reservation.owner.id.clone(),
        owner_name,
        name: reservation.owner.port_name.clone(),
        state: match reservation.state {
            DomainPortState::Reserved => PortState::Reserved,
            DomainPortState::Bound => PortState::Bound,
            DomainPortState::Conflict => PortState::Conflict,
        },
        conflict: reservation.conflict.as_ref().map(|h| PortHolderDto {
            pid: h.pid,
            exe: h.exe.clone().unwrap_or_else(|| "?".into()),
        }),
    }
}

/// An instance with its live status (if its actor answered) and its ports.
pub fn instance(
    entry: &Entry,
    status: Option<&InstanceStatus>,
    ports: &[PortReservation],
    metrics: Option<&forge_supervisor::InstanceMetrics>,
) -> InstanceDto {
    let (state, pid, last_error, restarts, started_at) = match status {
        Some(s) => (
            service_state(&s.state),
            s.pid,
            match &s.state {
                InstanceState::Failed { reason } => Some(failure_error(reason)),
                InstanceState::Crashed { exit_code } => Some(ApiError::new(
                    "internal",
                    format!("exited with code {exit_code:#x}"),
                )),
                _ => None,
            },
            s.restarts,
            s.state.is_active().then_some(s.since),
        ),
        None => (
            ServiceState::Failed,
            None,
            entry
                .error
                .as_ref()
                .map(|e| ApiError::new("config_invalid", e.clone())),
            0,
            None,
        ),
    };
    let label = entry.label();
    InstanceDto {
        id: entry.row.id,
        tool: entry.row.tool_id.clone(),
        version_id: entry.row.version_id,
        version: entry
            .version
            .as_ref()
            .map(|v| v.version.clone())
            .unwrap_or_default(),
        name: entry.row.name.clone(),
        state,
        pid,
        ports: ports.iter().map(|r| port(r, label.clone())).collect(),
        autostart: entry.row.autostart,
        started_at,
        cpu_pct: metrics.map(|m| m.cpu_pct as f32),
        rss_bytes: metrics.map(|m| m.rss_bytes),
        restarts,
        last_error,
        data_dir: entry.row.data_dir.as_ref().map(|d| d.replace('\\', "/")),
        // The periodic data-dir scan is M2-E2-T10.
        size_bytes: None,
    }
}

pub fn pool_state(state: &PoolState) -> ServiceState {
    match state {
        PoolState::Stopped => ServiceState::Stopped,
        PoolState::Starting => ServiceState::Starting,
        PoolState::Running => ServiceState::Running,
        PoolState::Draining | PoolState::Stopping => ServiceState::Stopping,
        PoolState::Failed { .. } => ServiceState::Failed,
    }
}

/// A pool; `sites` = enabled sites it serves.
pub fn pool(status: &PoolStatus, sites: u32) -> PoolDto {
    let active = status.count(WorkerPhase::Active);
    PoolDto {
        series: status.series.clone(),
        version: status.version.clone(),
        version_id: status.version_id,
        state: pool_state(&status.state),
        upstream: status.upstream.name.clone(),
        workers: u32::try_from(active).unwrap_or(u32::MAX),
        busy_workers: u32::try_from(
            status
                .workers
                .iter()
                .filter(|w| w.phase == WorkerPhase::Active && w.established > 0)
                .count(),
        )
        .unwrap_or(u32::MAX),
        min_workers: status.min_workers,
        max_workers: status.max_workers,
        ports: status.workers.iter().filter_map(|w| w.port).collect(),
        sites,
        xdebug: false,
        requests_total: Some(status.requests_total),
        rss_bytes: Some(status.rss_bytes),
    }
}

pub fn site_kind(kind: DomainSiteKind) -> SiteKind {
    match kind {
        DomainSiteKind::Php => SiteKind::Php,
        DomainSiteKind::Static => SiteKind::Static,
        DomainSiteKind::Proxy => SiteKind::Proxy,
        DomainSiteKind::App => SiteKind::App,
    }
}

pub fn site_xdebug(x: DomainXdebug) -> SiteXdebug {
    match x {
        DomainXdebug::Inherit => SiteXdebug::Inherit,
        DomainXdebug::Off => SiteXdebug::Off,
        DomainXdebug::Trigger => SiteXdebug::Trigger,
        DomainXdebug::Always => SiteXdebug::Always,
    }
}

/// The URL a site is served at (`https://loja.test`).
pub fn site_url(site: &Site) -> String {
    let scheme = if site.secure { "https" } else { "http" };
    format!("{scheme}://{}", site.host)
}

/// A site; `php` = what the daemon resolved for it.
pub fn site(site: &Site, php: Option<&SitePhp>, default_server: &str) -> SiteDto {
    SiteDto {
        id: site.id,
        slug: site.slug.clone(),
        host: site.host.clone(),
        aliases: site.aliases.clone(),
        kind: site_kind(site.kind),
        root: site.root.as_ref().map(|r| r.replace('\\', "/")),
        driver: site.driver.clone(),
        server: site
            .server
            .as_deref()
            .or(Some(default_server))
            .and_then(|s| forge_core::ids::ToolId::new(s).ok()),
        php: (site.kind == DomainSiteKind::Php).then(|| SitePhpDto {
            series: site.php.series.clone(),
            exact: site.php.exact.clone(),
            resolved: php.map(|p| p.version.version.clone()),
            upstream: php.map(|p| p.upstream.clone()),
        }),
        upstream: site.upstream.clone(),
        websocket: site.kind == DomainSiteKind::Proxy,
        secure: site.secure,
        http_redirect: site.http_redirect,
        env: site.env.clone(),
        xdebug: site_xdebug(site.xdebug),
        parked: site.parked,
        enabled: site.enabled,
        case_sensitive: site.case_sensitive,
        url: site_url(site),
        processes: Vec::new(),
        created_at: site.created_at,
        updated_at: site.updated_at,
    }
}

pub fn certificate(leaf: &LeafInfo, site_id: Option<forge_core::ids::SiteId>) -> CertificateDto {
    CertificateDto {
        host: leaf.host.clone(),
        sans: leaf.sans.clone(),
        site_id,
        not_before: leaf.not_before,
        not_after: leaf.not_after,
        thumbprint_sha256: leaf.thumbprint_sha256.clone(),
        // Checked by `tls.list` (M2-E2-T6); `tls.issue`/`tls.renew` do not check.
        ca_thumbprint_sha256: None,
        trusted: None,
    }
}

fn subtree(s: &PermittedSubtree) -> String {
    match s {
        PermittedSubtree::Dns { name } => format!(".{}", name.trim_start_matches('.')),
        PermittedSubtree::Ip { network, prefix } => format!("{network}/{prefix}"),
    }
}

pub fn ca_status(status: &TlsStatus) -> CaStatusDto {
    let ca = status.ca.as_ref();
    CaStatusDto {
        exists: ca.is_some(),
        subject: ca.map(|c| c.subject.clone()),
        thumbprint_sha256: ca.map(|c| c.thumbprint_sha256.clone()),
        thumbprint_sha1: ca.map(|c| c.thumbprint_sha1.to_string()),
        not_after: ca.map(|c| c.not_after),
        trusted_current_user: status.trusted_current_user,
        trusted_local_machine: status.trusted_local_machine,
        name_constraints: ca
            .map(|c| c.permitted.iter().map(subtree).collect())
            .unwrap_or_default(),
    }
}

pub fn registry(info: &RegistryInfo) -> RegistryDto {
    RegistryDto {
        name: info.name.clone(),
        url: info.location.clone(),
        kind: match info.kind.as_str() {
            "git" => RegistryKind::Git,
            "http" => RegistryKind::Http,
            _ => RegistryKind::Dir,
        },
        official: info.official,
        enabled: info.enabled,
        priority: i32::try_from(info.priority).unwrap_or(i32::MAX),
        tools: u32::try_from(info.tools).unwrap_or(u32::MAX),
        last_refresh_at: info.last_refresh_at,
    }
}

pub fn xdebug_mode(enabled: bool) -> XdebugMode {
    if enabled {
        XdebugMode::On
    } else {
        XdebugMode::Off
    }
}
