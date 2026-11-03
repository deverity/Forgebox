//! The tray's model: what the daemon of the active context looks like, reduced from one
//! snapshot (`services.status`, `sites.list`, `php.list`, `hello`) and the events after it
//! (docs/07 §7). Pure: no Tauri, no I/O, no clock. The actor in [`super`] owns one and turns
//! it into a [`MenuSpec`], a tooltip and an icon after every change.
//!
//! Aggregate state, highest first (docs/07 §7): **error** (daemon unreachable, or every
//! enabled service failed) > **warning** (some service failed or crash-loops) >
//! **starting** (some service is starting) > **updating** (tool updates pending, an
//! install/update job running, an instance in maintenance, or an app update) > **running**
//! (something runs) > **stopped** (nothing runs). `base` before the first answer.
//! "Enabled" services are the instances with `autostart`; with none, every instance.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use forge_ipc::api::{
    HelloDto, InstanceDto, JobCancelledPayload, JobDonePayload, JobFailedPayload,
    JobStartedPayload, MailChangedPayload, PhpVersionDto, ServicePayload, ServiceState,
    ServiceStatusDto, SiteDto, ToolCurrentPayload, ToolInstallPayload, ToolUpdatesAvailablePayload,
    ToolVersionChangePayload,
};

use crate::bridge::{ConnectionStatus, ContextStateDto, LOCAL_CONTEXT};
use crate::i18n::{Language, Namespace, translate};

/// Aggregate state of the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrayState {
    /// No answer yet (the plain app icon).
    Base,
    Running,
    Starting,
    Stopped,
    Updating,
    Warning,
    Error,
}

impl TrayState {
    /// Highest first: the first state whose condition holds is shown (docs/07 §7).
    pub const PRECEDENCE: [TrayState; 6] = [
        TrayState::Error,
        TrayState::Warning,
        TrayState::Starting,
        TrayState::Updating,
        TrayState::Running,
        TrayState::Stopped,
    ];

    /// The state shown when every state of `holding` has its condition true.
    pub fn highest(holding: &[TrayState]) -> TrayState {
        Self::PRECEDENCE
            .into_iter()
            .find(|state| holding.contains(state))
            .unwrap_or(TrayState::Stopped)
    }

    /// File stem of the icon (`icons/tray/<stem>-<theme>.ico`).
    pub fn stem(self) -> &'static str {
        match self {
            TrayState::Base => "base",
            TrayState::Running => "running",
            TrayState::Starting => "starting",
            TrayState::Stopped => "stopped",
            TrayState::Updating => "updating",
            TrayState::Warning => "warning",
            TrayState::Error => "error",
        }
    }
}

/// Icon of a menu entry (see `icons::glyph`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    /// The app icon (header).
    App,
    /// A colored dot (status line).
    Dot(TrayState),
    Open,
    Services,
    Sites,
    Php,
    Play,
    Restart,
    Stop,
    Logs,
    Mail,
    Terminal,
    Settings,
    Quit,
}

/// Where the connection of the active context stands, as the tray sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Never connected yet (opening, first connection, onboarding).
    Pending,
    Up,
    /// Lost or stopped after having been up, or reconnecting.
    Down,
}

/// What the tray keeps of an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceView {
    /// `InstanceId` (ULID).
    pub id: String,
    pub tool: String,
    pub name: String,
    pub version: String,
    pub state: ServiceState,
    pub autostart: bool,
}

impl ServiceView {
    pub fn from_dto(dto: &InstanceDto) -> Self {
        Self {
            id: dto.id.to_string(),
            tool: dto.tool.to_string(),
            name: dto.name.clone(),
            version: dto.version.clone(),
            state: dto.state,
            autostart: dto.autostart,
        }
    }

    /// `mysql 8.4`, or `mysql 8.4 · legacy` for a named instance.
    pub fn label(&self) -> String {
        let mut label = self.tool.clone();
        if !self.version.is_empty() {
            label.push(' ');
            label.push_str(&self.version);
        }
        if self.name != "default" && !self.name.is_empty() {
            label.push_str(" · ");
            label.push_str(&self.name);
        }
        label
    }

    fn is_failing(&self) -> bool {
        matches!(self.state, ServiceState::Failed | ServiceState::Crashed)
    }
}

/// A site of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteView {
    pub host: String,
    /// Opened by the client (the daemon may be remote).
    pub url: String,
}

/// A PHP version of the CLI submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhpView {
    pub version: String,
    pub series: String,
    pub version_id: String,
    /// `bin/php/current` points at it (`forge use php`).
    pub current: bool,
}

/// What one fetch brought (`None`: not fetched, or failed: the previous value stays).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub services: Option<Vec<ServiceView>>,
    pub sites: Option<Vec<SiteView>>,
    pub php: Option<Vec<PhpView>>,
    /// `mail.status.unread`, when Mail is implemented and was fetched.
    pub mail_unread: Option<u32>,
}

impl Snapshot {
    pub fn services_from(dto: &ServiceStatusDto) -> Vec<ServiceView> {
        let mut services: Vec<ServiceView> =
            dto.instances.iter().map(ServiceView::from_dto).collect();
        sort_services(&mut services);
        services
    }

    pub fn sites_from(sites: &[SiteDto]) -> Vec<SiteView> {
        let mut out: Vec<SiteView> = sites
            .iter()
            .filter(|site| site.enabled)
            .map(|site| SiteView {
                host: site.host.clone(),
                url: site.url.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.host.cmp(&b.host));
        out
    }

    pub fn php_from(versions: &[PhpVersionDto]) -> Vec<PhpView> {
        versions
            .iter()
            .map(|v| PhpView {
                version: v.version.clone(),
                series: v.series.clone(),
                version_id: v.version_id.to_string(),
                current: v.is_current_cli,
            })
            .collect()
    }
}

fn sort_services(services: &mut [ServiceView]) {
    services.sort_by(|a, b| (&a.tool, &a.name, &a.id).cmp(&(&b.tool, &b.name, &b.id)));
}

/// Which parts of the snapshot must be fetched again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scope {
    pub services: bool,
    pub sites: bool,
    pub php: bool,
}

impl Scope {
    pub const ALL: Scope = Scope {
        services: true,
        sites: true,
        php: true,
    };
    pub const SITES: Scope = Scope {
        services: false,
        sites: true,
        php: false,
    };
    pub const PHP: Scope = Scope {
        services: false,
        sites: false,
        php: true,
    };
    pub const SERVICES: Scope = Scope {
        services: true,
        sites: false,
        php: false,
    };

    pub fn is_empty(self) -> bool {
        !(self.services || self.sites || self.php)
    }

    #[must_use]
    pub fn union(self, other: Scope) -> Scope {
        Scope {
            services: self.services || other.services,
            sites: self.sites || other.sites,
            php: self.php || other.php,
        }
    }
}

/// What an event (or a connection change) asks of the actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Nothing the tray shows changed.
    None,
    /// The model changed: rebuild (debounced).
    Changed,
    /// Fetch these parts again (the model may also have changed).
    Resync(Scope),
}

/// Jobs that put the icon in `updating` (docs/07 §7: an install/update job runs).
pub const UPDATING_JOBS: &[&str] = &[
    "tools.install",
    "tools.update",
    "tools.rollback",
    "tools.remove",
    "tools.add_local",
];

/// Event kinds the tray subscribes to (`events.subscribe` filter).
pub const EVENT_KINDS: &[&str] = &[
    "service.starting",
    "service.running",
    "service.stopping",
    "service.stopped",
    "service.crashed",
    "service.failed",
    "service.maintenance",
    "site.*",
    "tool.*",
    "job.started",
    "job.done",
    "job.failed",
    "job.cancelled",
    "mail.changed",
];

/// Method the Mail entry needs (hidden while the daemon answers `not_implemented`).
pub const MAIL_METHOD: &str = "mail.status";
/// Method the Terminal entry (and middle click) needs.
pub const TERMINAL_METHOD: &str = "terminal.open";

/// Counts behind the status line and the actions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub total: usize,
    pub running: usize,
    pub starting: usize,
    pub failed: usize,
    pub maintenance: usize,
    pub enabled: usize,
    pub enabled_failed: usize,
    /// Enabled instances that are stopped, failed or crashed ("Iniciar todos" applies).
    pub enabled_startable: usize,
}

/// The model of one context.
#[derive(Debug, Clone, PartialEq)]
pub struct TrayModel {
    pub context: String,
    /// Contexts the header submenu lists (only `local` in M2).
    pub contexts: Vec<String>,
    pub link: Link,
    /// `hello.local == false` (or a non-local context while unknown).
    pub remote: bool,
    pub not_implemented: BTreeSet<String>,
    /// A first snapshot of the services arrived.
    pub loaded: bool,
    pub services: Vec<ServiceView>,
    pub sites: Vec<SiteView>,
    pub php: Vec<PhpView>,
    /// `tool.updates_available.count`.
    pub updates_pending: u32,
    /// Install/update jobs running (`job_id`, or `tool@version` for jobless installs).
    pub busy_jobs: BTreeSet<String>,
    /// The app updater announced an update (`forge://updater`).
    pub app_update: bool,
    pub mail_unread: Option<u32>,
    /// Connection number of the last `Up` (a new one means a reconnection).
    generation: Option<u64>,
    ever_up: bool,
}

impl TrayModel {
    pub fn new(context: &str) -> Self {
        Self {
            context: context.to_owned(),
            contexts: vec![LOCAL_CONTEXT.to_owned()],
            link: Link::Pending,
            remote: context != LOCAL_CONTEXT,
            not_implemented: BTreeSet::new(),
            loaded: false,
            services: Vec::new(),
            sites: Vec::new(),
            php: Vec::new(),
            updates_pending: 0,
            busy_jobs: BTreeSet::new(),
            app_update: false,
            mail_unread: None,
            generation: None,
            ever_up: false,
        }
    }

    /// Whether the daemon implements `method` (`hello.not_implemented`). Unknown before
    /// the first hello: `false`.
    pub fn implemented(&self, method: &str) -> bool {
        self.link == Link::Up && !self.not_implemented.contains(method)
    }

    pub fn mail_visible(&self) -> bool {
        self.implemented(MAIL_METHOD)
    }

    pub fn terminal_visible(&self) -> bool {
        self.implemented(TERMINAL_METHOD)
    }

    /// Follows the bridge's state of the context. `Resync(ALL)` when a (new) connection
    /// is up: the first one, or one after a loss.
    pub fn set_connection(&mut self, state: &ContextStateDto) -> Effect {
        if state.context != self.context {
            return Effect::None;
        }
        let before = (self.link, self.remote, self.not_implemented.clone());
        let link = match state.status {
            ConnectionStatus::Connected => Link::Up,
            ConnectionStatus::Reconnecting
            | ConnectionStatus::Stopped
            | ConnectionStatus::Closed => Link::Down,
            ConnectionStatus::Idle
            | ConnectionStatus::Uninitialized
            | ConnectionStatus::Connecting
            | ConnectionStatus::Starting => {
                if self.ever_up {
                    Link::Down
                } else {
                    Link::Pending
                }
            }
        };
        self.link = link;
        if let Some(hello) = &state.hello {
            self.apply_hello(hello);
        }
        if link == Link::Up {
            self.ever_up = true;
            let new_connection = before.0 != Link::Up || state.generation != self.generation;
            self.generation = state.generation;
            if new_connection {
                return Effect::Resync(Scope::ALL);
            }
        }
        if (self.link, self.remote, &self.not_implemented) != (before.0, before.1, &before.2) {
            Effect::Changed
        } else {
            Effect::None
        }
    }

    fn apply_hello(&mut self, hello: &HelloDto) {
        self.remote = !hello.local;
        self.not_implemented = hello.not_implemented.iter().cloned().collect();
    }

    /// Replaces what the snapshot brought.
    pub fn apply_snapshot(&mut self, snapshot: Snapshot) {
        if let Some(services) = snapshot.services {
            self.services = services;
            self.loaded = true;
        }
        if let Some(sites) = snapshot.sites {
            self.sites = sites;
        }
        if let Some(php) = snapshot.php {
            self.php = php;
        }
        if snapshot.mail_unread.is_some() {
            self.mail_unread = snapshot.mail_unread;
        }
    }

    /// Reduces one event (`kind`, `payload`).
    pub fn apply_event(&mut self, kind: &str, payload: &Value) -> Effect {
        match kind {
            "service.starting"
            | "service.running"
            | "service.stopping"
            | "service.stopped"
            | "service.crashed"
            | "service.failed"
            | "service.maintenance" => self.on_service(payload),
            "site.created" | "site.updated" | "site.removed" | "site.php_changed" => {
                Effect::Resync(Scope::SITES)
            }
            "tool.current_changed" => self.on_current_changed(payload),
            "tool.updates_available" => match parse::<ToolUpdatesAvailablePayload>(payload) {
                Some(p) => self.set(|m| m.updates_pending = p.count),
                None => Effect::None,
            },
            "tool.install.started" => match parse::<ToolInstallPayload>(payload) {
                Some(p) if p.job_id.is_none() => {
                    let key = format!("{}@{}", p.tool, p.version);
                    self.set(|m| {
                        m.busy_jobs.insert(key);
                    })
                }
                _ => Effect::None,
            },
            "tool.install.completed" | "tool.install.failed" => {
                match parse::<ToolInstallPayload>(payload) {
                    Some(p) => {
                        let key = format!("{}@{}", p.tool, p.version);
                        let changed = self.set(|m| {
                            m.busy_jobs.remove(&key);
                            if let Some(job) = p.job_id {
                                m.busy_jobs.remove(&job.to_string());
                            }
                        });
                        if p.tool.as_str() == "php" && kind == "tool.install.completed" {
                            Effect::Resync(Scope::PHP)
                        } else {
                            changed
                        }
                    }
                    None => Effect::None,
                }
            }
            "tool.updated" | "tool.rollback" => match parse::<ToolVersionChangePayload>(payload) {
                Some(p) if p.tool.as_str() == "php" => Effect::Resync(Scope::PHP),
                _ => Effect::None,
            },
            "job.started" => match parse::<JobStartedPayload>(payload) {
                Some(p) if UPDATING_JOBS.contains(&p.name.as_str()) => self.set(|m| {
                    m.busy_jobs.insert(p.job_id.to_string());
                }),
                _ => Effect::None,
            },
            "job.done" => match parse::<JobDonePayload>(payload) {
                Some(p) => self.finish_job(&p.job_id.to_string(), &p.name),
                None => Effect::None,
            },
            "job.failed" => match parse::<JobFailedPayload>(payload) {
                Some(p) => self.finish_job(&p.job_id.to_string(), &p.name),
                None => Effect::None,
            },
            "job.cancelled" => match parse::<JobCancelledPayload>(payload) {
                Some(p) => self.finish_job(&p.job_id.to_string(), &p.name),
                None => Effect::None,
            },
            "mail.changed" => match parse::<MailChangedPayload>(payload) {
                Some(p) => self.set(|m| m.mail_unread = Some(p.unread)),
                None => Effect::None,
            },
            _ => Effect::None,
        }
    }

    /// Applies `change`; `Changed` when the model differs afterwards.
    fn set(&mut self, change: impl FnOnce(&mut Self)) -> Effect {
        let before = self.clone();
        change(self);
        if *self == before {
            Effect::None
        } else {
            Effect::Changed
        }
    }

    fn finish_job(&mut self, job_id: &str, name: &str) -> Effect {
        let changed = self.set(|m| {
            m.busy_jobs.remove(job_id);
        });
        if name == "tools.remove" || name == "tools.add_local" {
            // A PHP version may have come or gone.
            return Effect::Resync(Scope::PHP);
        }
        changed
    }

    fn on_service(&mut self, payload: &Value) -> Effect {
        let Some(p) = parse::<ServicePayload>(payload) else {
            return Effect::Resync(Scope::SERVICES);
        };
        let id = p.instance_id.to_string();
        match self.services.iter_mut().find(|s| s.id == id) {
            Some(service) if service.state == p.state => Effect::None,
            Some(service) => {
                service.state = p.state;
                Effect::Changed
            }
            // A new instance: its autostart and version come from `services.status`.
            None if self.loaded => Effect::Resync(Scope::SERVICES),
            None => Effect::None,
        }
    }

    fn on_current_changed(&mut self, payload: &Value) -> Effect {
        let Some(p) = parse::<ToolCurrentPayload>(payload) else {
            return Effect::None;
        };
        if p.tool.as_str() != "php" {
            return Effect::None;
        }
        let by_id = p.version_id.map(|id| id.to_string());
        // `to` is the folder name (`8.3.37-nts-vs17-x64`).
        let matches = |v: &PhpView| match &by_id {
            Some(id) => &v.version_id == id,
            None => p.to == v.version || p.to.starts_with(&format!("{}-", v.version)),
        };
        if !self.php.iter().any(matches) {
            return Effect::Resync(Scope::PHP);
        }
        self.set(|m| {
            for v in &mut m.php {
                v.current = matches(v);
            }
        })
    }

    /// Instances the "enabled" rules count: `autostart` ones, or all when none is.
    fn enabled(&self) -> impl Iterator<Item = &ServiceView> {
        let any_autostart = self.services.iter().any(|s| s.autostart);
        self.services
            .iter()
            .filter(move |s| !any_autostart || s.autostart)
    }

    pub fn summary(&self) -> Summary {
        let mut summary = Summary {
            total: self.services.len(),
            ..Summary::default()
        };
        for s in &self.services {
            match s.state {
                ServiceState::Running => summary.running += 1,
                ServiceState::Starting => summary.starting += 1,
                ServiceState::Failed | ServiceState::Crashed => summary.failed += 1,
                ServiceState::Maintenance => summary.maintenance += 1,
                ServiceState::Stopped | ServiceState::Stopping => {}
            }
        }
        for s in self.enabled() {
            summary.enabled += 1;
            if s.is_failing() {
                summary.enabled_failed += 1;
            }
            if matches!(
                s.state,
                ServiceState::Stopped | ServiceState::Failed | ServiceState::Crashed
            ) {
                summary.enabled_startable += 1;
            }
        }
        summary
    }

    /// The states whose condition holds now (before precedence).
    pub fn conditions(&self) -> Vec<TrayState> {
        let s = self.summary();
        let mut holding = Vec::new();
        if self.link == Link::Down || (s.enabled > 0 && s.enabled_failed == s.enabled) {
            holding.push(TrayState::Error);
        }
        if s.failed > 0 {
            holding.push(TrayState::Warning);
        }
        if s.starting > 0 {
            holding.push(TrayState::Starting);
        }
        if self.updates_pending > 0
            || !self.busy_jobs.is_empty()
            || s.maintenance > 0
            || self.app_update
        {
            holding.push(TrayState::Updating);
        }
        if s.running > 0 {
            holding.push(TrayState::Running);
        }
        holding.push(TrayState::Stopped);
        holding
    }

    /// The aggregate state of the icon.
    pub fn state(&self) -> TrayState {
        match self.link {
            Link::Pending => TrayState::Base,
            Link::Down => TrayState::Error,
            Link::Up if !self.loaded => TrayState::Base,
            Link::Up => TrayState::highest(&self.conditions()),
        }
    }

    /// The display name of the context (`Local`, or the remote's id).
    pub fn context_label(&self, lang: Language) -> String {
        context_label(&self.context, lang)
    }

    /// The status line (also the first line of the tooltip after the title).
    pub fn status_line(&self, lang: Language) -> String {
        let t = |key: &str, vars: &[(&str, &str)]| translate(lang, Namespace::Tray, key, vars);
        match self.link {
            Link::Down => return t("status.daemon_down", &[]),
            Link::Pending => return t("status.connecting", &[]),
            Link::Up if !self.loaded => return t("status.connecting", &[]),
            Link::Up => {}
        }
        let s = self.summary();
        if s.total == 0 {
            t("status.no_services", &[])
        } else if s.failed > 0 {
            t("status.failed", &[("count", &s.failed.to_string())])
        } else if s.running == s.total {
            t("status.all_running", &[])
        } else {
            t(
                "services_running",
                &[
                    ("running", &s.running.to_string()),
                    ("total", &s.total.to_string()),
                ],
            )
        }
    }

    /// The tooltip: title, status line, active PHP CLI (Windows keeps 127 characters).
    pub fn tooltip(&self, lang: Language) -> String {
        let t = |key: &str, vars: &[(&str, &str)]| translate(lang, Namespace::Tray, key, vars);
        let mut lines = vec![t("title", &[("context", &self.context_label(lang))])];
        if self.remote {
            lines.push(t("tooltip.remote", &[("context", &self.context)]));
        }
        lines.push(self.status_line(lang));
        if let Some(php) = self.php.iter().find(|v| v.current) {
            lines.push(t("tooltip.php", &[("version", &php.version)]));
        }
        truncate_chars(&lines.join("\n"), 127)
    }

    /// The native menu, as data (docs/07 §7, the reference mockup's structure).
    pub fn menu(&self, lang: Language) -> MenuSpec {
        let t = |key: &str, vars: &[(&str, &str)]| translate(lang, Namespace::Tray, key, vars);
        let up = self.link == Link::Up && self.loaded;
        let s = self.summary();
        let state = self.state();
        let mut items = Vec::new();

        // Header: context switcher.
        let mut contexts: Vec<MenuNode> = self
            .contexts
            .iter()
            .map(|id| {
                let name = context_label(id, lang);
                MenuNode::Item {
                    id: TrayAction::SwitchContext(id.clone()).id(),
                    label: if *id == self.context {
                        format!("● {name}")
                    } else {
                        name
                    },
                    enabled: *id != self.context,
                    glyph: None,
                }
            })
            .collect();
        contexts.push(MenuNode::Item {
            id: "context.add_remote".into(),
            label: t("context.add_remote", &[]),
            enabled: false,
            glyph: None,
        });
        items.push(MenuNode::Submenu {
            id: "header".into(),
            label: t("title", &[("context", &self.context_label(lang))]),
            enabled: true,
            glyph: Some(Glyph::App),
            items: contexts,
        });
        items.push(MenuNode::Item {
            id: "status".into(),
            label: self.status_line(lang),
            enabled: false,
            glyph: Some(Glyph::Dot(state)),
        });
        items.push(item(
            TrayAction::OpenWindow,
            t("open", &[]),
            true,
            Glyph::Open,
        ));

        // Services.
        let mut services: Vec<MenuNode> = self
            .services
            .iter()
            .map(|service| {
                let running = service.state == ServiceState::Running;
                let actionable = matches!(
                    service.state,
                    ServiceState::Running
                        | ServiceState::Stopped
                        | ServiceState::Failed
                        | ServiceState::Crashed
                );
                MenuNode::Check {
                    id: TrayAction::ToggleService {
                        id: service.id.clone(),
                        start: !running,
                    }
                    .id(),
                    label: service.label(),
                    enabled: up && actionable,
                    checked: running,
                }
            })
            .collect();
        if !services.is_empty() {
            services.push(MenuNode::Separator);
        }
        services.push(item(
            TrayAction::OpenServices,
            t("services.open", &[]),
            true,
            Glyph::Open,
        ));
        items.push(MenuNode::Submenu {
            id: "services".into(),
            label: t("services.menu", &[]),
            enabled: true,
            glyph: Some(Glyph::Services),
            items: services,
        });

        // Sites: opened by the client.
        let sites: Vec<MenuNode> = if self.sites.is_empty() {
            vec![MenuNode::Item {
                id: "sites.empty".into(),
                label: t("sites.empty", &[]),
                enabled: false,
                glyph: None,
            }]
        } else {
            self.sites
                .iter()
                .map(|site| MenuNode::Item {
                    id: TrayAction::OpenSite(site.url.clone()).id(),
                    label: site.host.clone(),
                    enabled: true,
                    glyph: None,
                })
                .collect()
        };
        items.push(MenuNode::Submenu {
            id: "sites".into(),
            label: t("sites.menu", &[]),
            enabled: up,
            glyph: Some(Glyph::Sites),
            items: sites,
        });

        // PHP CLI: radio (a check item per version, the current one checked).
        let php: Vec<MenuNode> = if self.php.is_empty() {
            vec![MenuNode::Item {
                id: "php.empty".into(),
                label: t("php.empty", &[]),
                enabled: false,
                glyph: None,
            }]
        } else {
            self.php
                .iter()
                .map(|v| MenuNode::Check {
                    id: TrayAction::UsePhp(v.version.clone()).id(),
                    label: t("php.version", &[("version", &v.version)]),
                    enabled: up && !v.current,
                    checked: v.current,
                })
                .collect()
        };
        items.push(MenuNode::Submenu {
            id: "php".into(),
            label: t("php.menu", &[]),
            enabled: up,
            glyph: Some(Glyph::Php),
            items: php,
        });

        // Stack actions.
        items.push(MenuNode::Separator);
        if up && s.enabled_startable > 0 {
            items.push(item(
                TrayAction::StartAll,
                t("start_all", &[]),
                true,
                Glyph::Play,
            ));
        }
        items.push(item(
            TrayAction::RestartAll,
            t("restart_all", &[]),
            up && s.running > 0,
            Glyph::Restart,
        ));
        items.push(item(
            TrayAction::StopAll,
            t("stop_all", &[]),
            up && s.running + s.starting > 0,
            Glyph::Stop,
        ));

        // Shortcuts into the window.
        items.push(MenuNode::Separator);
        items.push(item(TrayAction::Logs, t("logs", &[]), true, Glyph::Logs));
        if self.mail_visible() {
            let label = match self.mail_unread {
                Some(n) if n > 0 => t("mail_unread", &[("count", &n.to_string())]),
                _ => t("mail", &[]),
            };
            items.push(item(TrayAction::Mail, label, true, Glyph::Mail));
        }
        if self.terminal_visible() {
            items.push(item(
                TrayAction::Terminal,
                t("terminal", &[]),
                true,
                Glyph::Terminal,
            ));
        }
        items.push(MenuNode::Separator);
        items.push(item(
            TrayAction::Settings,
            t("settings", &[]),
            true,
            Glyph::Settings,
        ));
        items.push(item(TrayAction::Quit, t("quit", &[]), true, Glyph::Quit));
        MenuSpec { items }
    }
}

fn item(action: TrayAction, label: String, enabled: bool, glyph: Glyph) -> MenuNode {
    MenuNode::Item {
        id: action.id(),
        label,
        enabled,
        glyph: Some(glyph),
    }
}

fn context_label(context: &str, lang: Language) -> String {
    if context == LOCAL_CONTEXT {
        translate(lang, Namespace::Tray, "context.local", &[])
    } else {
        context.to_owned()
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn parse<T: DeserializeOwned>(payload: &Value) -> Option<T> {
    serde_json::from_value(payload.clone()).ok()
}

/// The native menu as data: what `menu::build` turns into a Tauri menu, and what the tests
/// read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MenuSpec {
    pub items: Vec<MenuNode>,
}

/// One entry of a [`MenuSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuNode {
    Item {
        id: String,
        label: String,
        enabled: bool,
        glyph: Option<Glyph>,
    },
    /// A check item (the per-instance toggles and the PHP "radio").
    Check {
        id: String,
        label: String,
        enabled: bool,
        checked: bool,
    },
    Submenu {
        id: String,
        label: String,
        enabled: bool,
        glyph: Option<Glyph>,
        items: Vec<MenuNode>,
    },
    Separator,
}

impl MenuNode {
    pub fn id(&self) -> Option<&str> {
        match self {
            MenuNode::Item { id, .. }
            | MenuNode::Check { id, .. }
            | MenuNode::Submenu { id, .. } => Some(id),
            MenuNode::Separator => None,
        }
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            MenuNode::Item { label, .. }
            | MenuNode::Check { label, .. }
            | MenuNode::Submenu { label, .. } => Some(label),
            MenuNode::Separator => None,
        }
    }

    pub fn enabled(&self) -> bool {
        match self {
            MenuNode::Item { enabled, .. }
            | MenuNode::Check { enabled, .. }
            | MenuNode::Submenu { enabled, .. } => *enabled,
            MenuNode::Separator => false,
        }
    }

    pub fn checked(&self) -> Option<bool> {
        match self {
            MenuNode::Check { checked, .. } => Some(*checked),
            _ => None,
        }
    }

    pub fn children(&self) -> &[MenuNode] {
        match self {
            MenuNode::Submenu { items, .. } => items,
            _ => &[],
        }
    }
}

impl MenuSpec {
    /// The entry with `id`, at any depth.
    pub fn find(&self, id: &str) -> Option<&MenuNode> {
        fn walk<'a>(nodes: &'a [MenuNode], id: &str) -> Option<&'a MenuNode> {
            nodes.iter().find_map(|node| {
                if node.id() == Some(id) {
                    Some(node)
                } else {
                    walk(node.children(), id)
                }
            })
        }
        walk(&self.items, id)
    }

    /// Ids of the top-level entries, separators as `"-"`.
    pub fn top_level(&self) -> Vec<&str> {
        self.items.iter().map(|n| n.id().unwrap_or("-")).collect()
    }
}

/// What a click on a menu entry (or the icon) asks for. The id of an entry is
/// [`TrayAction::id`]; [`TrayAction::parse`] reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    OpenWindow,
    SwitchContext(String),
    /// Start (`start`) or stop the instance `id`.
    ToggleService {
        id: String,
        start: bool,
    },
    OpenServices,
    OpenSite(String),
    UsePhp(String),
    StartAll,
    RestartAll,
    StopAll,
    Logs,
    Mail,
    Terminal,
    Settings,
    Quit,
}

impl TrayAction {
    pub fn id(&self) -> String {
        match self {
            TrayAction::OpenWindow => "open".into(),
            TrayAction::SwitchContext(id) => format!("context:{id}"),
            TrayAction::ToggleService { id, start: true } => format!("service:start:{id}"),
            TrayAction::ToggleService { id, start: false } => format!("service:stop:{id}"),
            TrayAction::OpenServices => "services.open".into(),
            TrayAction::OpenSite(url) => format!("site:{url}"),
            TrayAction::UsePhp(version) => format!("php:{version}"),
            TrayAction::StartAll => "start_all".into(),
            TrayAction::RestartAll => "restart_all".into(),
            TrayAction::StopAll => "stop_all".into(),
            TrayAction::Logs => "logs".into(),
            TrayAction::Mail => "mail".into(),
            TrayAction::Terminal => "terminal".into(),
            TrayAction::Settings => "settings".into(),
            TrayAction::Quit => "quit".into(),
        }
    }

    pub fn parse(id: &str) -> Option<TrayAction> {
        let simple = match id {
            "open" => Some(TrayAction::OpenWindow),
            "services.open" => Some(TrayAction::OpenServices),
            "start_all" => Some(TrayAction::StartAll),
            "restart_all" => Some(TrayAction::RestartAll),
            "stop_all" => Some(TrayAction::StopAll),
            "logs" => Some(TrayAction::Logs),
            "mail" => Some(TrayAction::Mail),
            "terminal" => Some(TrayAction::Terminal),
            "settings" => Some(TrayAction::Settings),
            "quit" => Some(TrayAction::Quit),
            _ => None,
        };
        if simple.is_some() {
            return simple;
        }
        let (prefix, rest) = id.split_once(':')?;
        if rest.is_empty() {
            return None;
        }
        match prefix {
            "context" => Some(TrayAction::SwitchContext(rest.to_owned())),
            "site" => Some(TrayAction::OpenSite(rest.to_owned())),
            "php" => Some(TrayAction::UsePhp(rest.to_owned())),
            "service" => {
                let (verb, instance) = rest.split_once(':')?;
                let start = match verb {
                    "start" => true,
                    "stop" => false,
                    _ => return None,
                };
                (!instance.is_empty()).then(|| TrayAction::ToggleService {
                    id: instance.to_owned(),
                    start,
                })
            }
            _ => None,
        }
    }

    /// The API call of an action that goes to the daemon: `(method, params)`. Client-side
    /// actions (window, navigation, URLs, quit) answer `None`.
    pub fn api_call(&self) -> Option<(&'static str, Value)> {
        match self {
            TrayAction::ToggleService { id, start } => Some((
                if *start {
                    "services.start"
                } else {
                    "services.stop"
                },
                json!({ "targets": [id] }),
            )),
            TrayAction::UsePhp(version) => {
                Some(("tools.use", json!({ "tool": "php", "version": version })))
            }
            // Empty targets mean `all` (ServiceTargetsParams).
            TrayAction::StartAll => Some(("services.start", json!({ "targets": [] }))),
            TrayAction::RestartAll => Some(("services.restart", json!({ "targets": [] }))),
            TrayAction::StopAll => Some(("services.stop", json!({ "targets": [] }))),
            _ => None,
        }
    }

    /// The window route of a navigation action.
    pub fn route(&self) -> Option<&'static str> {
        match self {
            TrayAction::OpenServices => Some("/services"),
            TrayAction::Logs => Some("/logs"),
            TrayAction::Mail => Some("/mail"),
            TrayAction::Settings => Some("/settings"),
            _ => None,
        }
    }
}

/// Instances by id, for tests and diagnostics.
pub fn services_by_id(model: &TrayModel) -> BTreeMap<&str, ServiceState> {
    model
        .services
        .iter()
        .map(|s| (s.id.as_str(), s.state))
        .collect()
}
