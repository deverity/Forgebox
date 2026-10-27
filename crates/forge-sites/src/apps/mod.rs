//! Processes of a site (Procfile 2.0, docs/06 §4.2): the `processes:` map of a site or of
//! `forgebox.yaml`, supervised as `App`s.
//!
//! * [`command`]: `$PORT`, Windows argument splitting, direct vs `cmd.exe` launch.
//! * [`runtime`]: `runtime: nodejs@22`, `.nvmrc`/`.node-version`/`.php-version` and the
//!   variables a runtime tool injects, all read from the tool manifests.
//! * [`spec`]: [`AppSite`] → [`AppPlan`] → [`AppSpec`] (deterministic `PATH`, env, ports,
//!   logs under `logs/apps/<host>/<name>.log`) and the supervisor spec.
//! * [`manager`]: the [`AppManager`] actor: one `InstanceActor` per process, one Job per
//!   site, `autostart: true | false | on-visit`, the `/_forgebox/starting` page.
//!
//! The vhost of an app site is built by [`crate::vhost::build`] from the same data: the
//! host proxies to `127.0.0.1:$PORT` of the process with `proxy: true` ([`crate::proxy`]).

pub mod command;
pub mod manager;
pub mod runtime;
pub mod spec;

pub use manager::{
    AppManager, AppTimeouts, EnsureOutcome, ProcessStatus, SiteAppsStatus, StartingPage,
};
pub use runtime::{ResolvedRuntime, RuntimeCatalog, RuntimeSource, RuntimeTool};
pub use spec::{
    APP_TOOL, APPS_RANGE, AppEnv, AppPlan, AppSite, AppSpec, PortRequest, port_request_for,
    processes_of, processes_to_json, proxied_process, validate_processes,
};
