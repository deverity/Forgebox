//! The method table: one row per API method (docs/09: every `forge` command maps to a
//! method; ADR-0009). This list is the single source of truth: [`METHODS`], the marker
//! types (`methods::sites_add`), the typed client (`Client::sites_add`) and
//! `methods.ts` are all generated from it, so they cannot drift.
//!
//! Row: `kind "group.action" fn_name(Params) -> Result [MinRole, flags...] "forge cmd" => "template";`
//! * `call`: answers with `Result` within ~1 s.
//! * `job`: answers at once with `JobDto`; `Result` is the type of `job.done.result`.
//! * `stream`: answers with `SubscriptionDto`; `Result` is the item notification type.
//!
//! Flags: `mutating` (idempotency cache by `request_id`), `local_only` (refused on remote
//! connections), `notify("item", "lagged")` (stream notification names). The last
//! string lists the CLI commands served (`; `-separated, empty for GUI/protocol-only).
//! `=> "template"` is the `forge` command rendered from the params
//! ([`super::cli_template`], ADR-0037): required for every row with a non-empty CLI list,
//! absent otherwise (`MethodInfo::render_cli`).

use super::*;

macro_rules! api_methods {
    ($callback:ident) => {
        $callback! {
            // daemon
            call "daemon.status" daemon_status(NoParams) -> DaemonStatusDto [Viewer] "forge daemon status" => "forge daemon status";
            call "daemon.stop" daemon_stop(DaemonStopParams) -> Ack [Admin, mutating] "forge daemon stop" => "forge daemon stop";
            call "daemon.restart" daemon_restart(NoParams) -> Ack [Admin, mutating] "forge daemon restart" => "forge daemon restart";
            job "daemon.doctor" daemon_doctor(DoctorParams) -> DoctorReportDto [Viewer] "forge doctor; forge daemon doctor" => "forge doctor";
            // tools / catalog
            call "tools.search" tools_search(ToolSearchParams) -> CatalogSearchDto [Viewer] "forge search" => "forge search {text} {category=--category}";
            call "tools.list" tools_list(ToolListParams) -> ToolListDto [Viewer] "forge tool list" => "forge tool list {installed?--installed} {updates?--updates}";
            call "tools.info" tools_info(ToolParams) -> ToolInfoDto [Viewer] "forge tool info" => "forge tool info {tool}";
            job "tools.install" tools_install(ToolInstallParams) -> ToolInstallResultDto [Admin, mutating] "forge tool install" => "forge tool install {specs...} {variant=--variant}";
            job "tools.remove" tools_remove(ToolRemoveParams) -> Ack [Admin, mutating] "forge tool remove" => "forge tool remove {spec} {keep_config?--keep-config}";
            job "tools.update" tools_update(ToolUpdateParams) -> ToolUpdatesDto [Admin, mutating] "forge tool update" => "[tool] forge tool update {tool} | forge tool update --all";
            job "tools.check_updates" tools_check_updates(ToolUpdateParams) -> ToolUpdatesDto [Viewer] "forge tool update --check" => "[tool] forge tool update {tool} --check | forge tool update --check";
            job "tools.rollback" tools_rollback(ToolRollbackParams) -> ToolVersionDto [Admin, mutating] "forge tool rollback" => "forge tool rollback {spec}";
            call "tools.policy" tools_policy(ToolPolicyParams) -> ToolDto [Admin, mutating] "forge tool policy" => "forge tool policy {tool} {policy}";
            job "tools.add_local" tools_add_local(ToolAddLocalParams) -> ToolVersionDto [Admin, mutating, local_only] "forge tool add-local" => "forge tool add-local {source}";
            job "tools.discover" tools_discover(NoParams) -> ToolDiscoverDto [Admin, mutating] "forge tool discover" => "forge tool discover";
            call "tools.use" tools_use(ToolUseParams) -> ToolVersionDto [Operator, mutating] "forge use" => "forge use {tool} {version}";
            call "tools.which" tools_which(ToolWhichParams) -> WhichDto [Viewer] "forge which" => "forge which {tool}";
            // services / instances
            job "services.start" services_start(ServiceTargetsParams) -> ServiceStatusDto [Operator, mutating] "forge start" => "forge start {targets...}";
            job "services.stop" services_stop(ServiceTargetsParams) -> ServiceStatusDto [Operator, mutating] "forge stop" => "forge stop {targets...}";
            job "services.restart" services_restart(ServiceTargetsParams) -> ServiceStatusDto [Operator, mutating] "forge restart" => "forge restart {targets...}";
            job "services.reload" services_reload(ServiceTargetParams) -> ServiceStatusDto [Operator, mutating] "forge reload" => "forge reload {target}";
            call "services.status" services_status(NoParams) -> ServiceStatusDto [Viewer] "forge status" => "forge status";
            job "services.create" services_create(ServiceCreateParams) -> InstanceDto [Admin, mutating] "forge service create" => "forge service create {tool} {name} {port=--port} {autostart?--autostart}";
            job "services.remove" services_remove(ServiceRemoveParams) -> Ack [Admin, mutating] "forge service remove" => "forge service remove {instance} {delete_data?--delete-data}";
            call "services.config" services_config(ServiceTargetParams) -> ConfigFilesDto [Operator, local_only] "forge service config" => "forge service config {target}";
            call "services.config_files" services_config_files(ServiceTargetParams) -> ConfigFilesDto [Viewer] "";
            job "services.set" services_set(ServiceSetParams) -> ServiceStatusDto [Admin, mutating] "forge service set" => "forge service set {target} {settings...}";
            call "services.ports" services_ports(NoParams) -> PortMapDto [Viewer] "forge service ports" => "forge service ports";
            // logs
            call "logs.read" logs_read(LogsReadParams) -> LogChunkDto [Viewer] "forge logs; forge daemon logs" => "forge logs {target} {since_secs=--since} {grep=--grep} {limit=-n}";
            stream "logs.subscribe" logs_subscribe(LogsSubscribeParams) -> LogEnvelope [Viewer, notify("logs.line", "logs.lagged")] "forge logs -f; forge daemon logs -f" => "forge logs {target} -f {grep=--grep} {tail=-n}";
            call "logs.unsubscribe" logs_unsubscribe(UnsubscribeParams) -> Ack [Viewer] "";
            // php
            call "php.list" php_list(NoParams) -> PhpVersionListDto [Viewer] "forge php list" => "forge php list";
            call "php.pool_status" php_pool_status(PhpPoolStatusParams) -> PoolListDto [Viewer] "forge php pool status" => "forge php pool status {version}";
            call "php.ext_list" php_ext_list(PhpVersionParams) -> PhpExtListDto [Viewer] "forge php ext list" => "forge php ext list {version}";
            job "php.ext_enable" php_ext_enable(PhpExtParams) -> PhpExtListDto [Admin, mutating] "forge php ext enable" => "forge php ext enable {version} {extension}";
            job "php.ext_disable" php_ext_disable(PhpExtParams) -> PhpExtListDto [Admin, mutating] "forge php ext disable" => "forge php ext disable {version} {extension}";
            job "php.xdebug" php_xdebug(PhpXdebugParams) -> PoolDto [Operator, mutating] "forge php xdebug" => "forge php xdebug {version} {mode} {port=--port}";
            call "php.ini" php_ini(PhpVersionParams) -> PhpIniDto [Viewer] "forge php ini" => "forge php ini {version}";
            call "php.ini_open" php_ini_open(PhpVersionParams) -> PhpIniDto [Operator, local_only] "forge php ini --open" => "forge php ini {version} --open";
            // sites, proxies, parked folders, projects, processes
            call "sites.list" sites_list(NoParams) -> SiteListDto [Viewer] "forge site list" => "forge site list";
            job "sites.add" sites_add(SiteAddParams) -> SiteDto [Operator, mutating] "forge site add; forge link" => "forge site add {dir} {host=--host} {php=--php} {server=--server} {secure?--secure} {driver=--driver}";
            job "sites.remove" sites_remove(SiteRemoveParams) -> Ack [Operator, mutating] "forge site remove; forge unlink; forge proxy remove" => "forge site remove {site}";
            job "sites.php" sites_php(SitePhpParams) -> SiteDto [Operator, mutating] "forge site php" => "forge site php {site} {php}";
            job "sites.secure" sites_secure(SiteSecureParams) -> SiteDto [Operator, mutating] "forge site secure; forge site unsecure" => "forge site {secure?secure:unsecure} {site}";
            job "sites.server" sites_server(SiteServerParams) -> SiteDto [Operator, mutating] "forge site server" => "forge site server {site} {server}";
            job "sites.alias" sites_alias(SiteAliasParams) -> SiteDto [Operator, mutating] "forge site alias" => "forge site alias {site} {action} {alias}";
            call "sites.open" sites_open(SiteOpenParams) -> Ack [Operator, local_only] "forge site open" => "[with=editor] forge site open {site} --editor | [with=terminal] forge site open {site} --terminal | [with=explorer] forge site open {site} --explorer | forge site open {site}";
            call "sites.info" sites_info(SiteParams) -> SiteInfoDto [Viewer] "forge site info" => "forge site info {site}";
            call "sites.park" sites_park(ParkParams) -> ParkedDirDto [Operator, mutating] "forge park" => "forge park {dir} {tld=--tld}";
            call "sites.unpark" sites_unpark(ParkParams) -> Ack [Operator, mutating] "forge unpark" => "forge unpark {dir}";
            call "sites.parked" sites_parked(NoParams) -> ParkedListDto [Viewer] "forge parked" => "forge parked";
            job "sites.proxy_add" sites_proxy_add(ProxyAddParams) -> SiteDto [Operator, mutating] "forge proxy add" => "forge proxy add {host} {upstream} {websocket?--ws} {secure?--secure}";
            job "sites.up" sites_up(ProjectParams) -> ProjectUpDto [Operator, mutating] "forge up" => "forge up {dir}";
            job "sites.down" sites_down(ProjectParams) -> Ack [Operator, mutating] "forge down" => "forge down {dir}";
            call "sites.ps" sites_ps(SitePsParams) -> SiteProcessListDto [Viewer] "forge ps" => "forge ps {site}";
            job "sites.run" sites_run(SiteRunParams) -> SiteProcessDto [Operator, mutating] "forge run" => "forge run {site} {process} {action}";
            job "sites.share" sites_share(SiteShareParams) -> ShareDto [Operator, mutating] "forge share" => "forge share {site} {provider=--provider} {stop?--stop}";
            job "sites.new" sites_new(SiteNewParams) -> SiteDto [Admin, mutating] "forge new" => "forge new {stack} {name} {php=--php} {db=--db} {dir=--dir}";
            // databases
            call "db.list" db_list(DbListParams) -> DatabaseListDto [Viewer] "forge db list" => "forge db list {instance}";
            call "db.create" db_create(DbCreateParams) -> DatabaseDto [Operator, mutating] "forge db create" => "forge db create {name} {instance=--on} {charset=--charset}";
            call "db.drop" db_drop(DbDropParams) -> Ack [Operator, mutating] "forge db drop" => "forge db drop {name} {instance=--on}";
            job "db.export" db_export(DbExportParams) -> DbExportDto [Operator, mutating] "forge db export" => "forge db export {name} {instance=--on} {output=--output}";
            job "db.import" db_import(DbImportParams) -> Ack [Operator, mutating] "forge db import" => "forge db import {name} {file} {instance=--on}";
            call "db.shell" db_shell(DbShellParams) -> Ack [Operator, local_only] "forge db shell" => "forge db shell {instance} {database}";
            call "db.url" db_url(DbUrlParams) -> DbUrlDto [Operator] "forge db url" => "forge db url {instance} {database}";
            call "db.open" db_open(DbOpenParams) -> Ack [Operator, local_only] "forge db open" => "forge db open {instance} {with=--with} {database}";
            // net
            call "net.dns_status" net_dns_status(NoParams) -> DnsStatusDto [Viewer] "forge dns status" => "forge dns status";
            job "net.dns_enable" net_dns_enable(NoParams) -> DnsStatusDto [Admin, mutating] "forge dns enable" => "forge dns enable";
            job "net.dns_disable" net_dns_disable(NoParams) -> DnsStatusDto [Admin, mutating] "forge dns disable" => "forge dns disable";
            call "net.dns_test" net_dns_test(DnsTestParams) -> DnsTestDto [Viewer] "forge dns test" => "forge dns test {host}";
            job "net.lan_expose" net_lan_expose(LanSiteParams) -> LanAccessDto [Admin, mutating] "forge lan expose" => "forge lan expose {site}";
            job "net.lan_unexpose" net_lan_unexpose(LanSiteParams) -> Ack [Admin, mutating] "forge lan unexpose" => "forge lan unexpose {site}";
            call "net.lan_qr" net_lan_qr(LanSiteParams) -> LanAccessDto [Viewer] "forge lan qr" => "forge lan qr {site}";
            // tls
            call "tls.ca_status" tls_ca_status(NoParams) -> CaStatusDto [Viewer] "forge tls ca status" => "forge tls ca status";
            job "tls.ca_install" tls_ca_install(CaInstallParams) -> CaStatusDto [Admin, mutating] "forge tls ca install" => "[scope=local_machine] forge tls ca install --scope machine | forge tls ca install --scope user";
            call "tls.ca_export" tls_ca_export(NoParams) -> PemDto [Viewer] "forge tls ca export" => "forge tls ca export";
            job "tls.ca_rotate" tls_ca_rotate(CaRotateParams) -> CaStatusDto [Admin, mutating] "forge tls ca rotate" => "forge tls ca rotate";
            job "tls.issue" tls_issue(TlsHostParams) -> CertificateDto [Operator, mutating] "forge tls issue" => "forge tls issue {host}";
            job "tls.renew" tls_renew(TlsRenewParams) -> CertificateListDto [Operator, mutating] "forge tls renew" => "forge tls renew {host} {all?--all}";
            call "tls.export" tls_export(TlsExportParams) -> CertExportDto [Admin] "forge tls export" => "[format=pfx] forge tls export {host} --pfx | forge tls export {host}";
            // mail
            call "mail.status" mail_status(NoParams) -> MailStatusDto [Viewer] "forge mail status" => "forge mail status";
            call "mail.open" mail_open(NoParams) -> Ack [Viewer, local_only] "forge mail open" => "forge mail open";
            call "mail.list" mail_list(MailListParams) -> MailListDto [Viewer] "forge mail list" => "forge mail list {unread?--unread} {limit=--limit}";
            call "mail.show" mail_show(MailShowParams) -> MailMessageDto [Viewer] "forge mail show" => "forge mail show {id}";
            call "mail.clear" mail_clear(MailClearParams) -> Ack [Operator, mutating] "forge mail clear" => "forge mail clear";
            call "mail.env" mail_env(NoParams) -> EnvVarsDto [Viewer] "forge mail env" => "forge mail env";
            // shell / env
            call "shell.open" shell_open(ShellOpenParams) -> Ack [Operator, local_only] "forge shell" => "forge shell {dir}";
            call "shell.env" shell_env(ShellEnvParams) -> ShellEnvDto [Viewer, local_only] "forge env; forge exec" => "[format=cmd] forge env {dir} --cmd | [format=bash] forge env {dir} --bash | [format=json] forge env {dir} --json | forge env {dir} --powershell";
            call "shell.path_show" shell_path_show(NoParams) -> UserPathDto [Viewer] "forge path show" => "forge path show";
            call "shell.path_add" shell_path_add(NoParams) -> UserPathDto [Admin, mutating] "forge path add" => "forge path add";
            call "shell.path_remove" shell_path_remove(NoParams) -> UserPathDto [Admin, mutating] "forge path remove" => "forge path remove";
            // fs / Linux parity
            call "fs_parity.case_status" fs_parity_case_status(CaseDirParams) -> CaseSensitivityDto [Viewer] "forge fs case-sensitive status" => "forge fs case-sensitive status {dir}";
            call "fs_parity.case_enable" fs_parity_case_enable(CaseDirParams) -> CaseSensitivityDto [Admin, mutating] "forge fs case-sensitive enable" => "forge fs case-sensitive enable {dir}";
            call "fs_parity.case_disable" fs_parity_case_disable(CaseDirParams) -> CaseSensitivityDto [Admin, mutating] "forge fs case-sensitive disable" => "forge fs case-sensitive disable {dir}";
            job "fs_parity.case_convert" fs_parity_case_convert(CaseConvertParams) -> CaseConvertDto [Admin, mutating] "forge fs case-sensitive convert" => "forge fs case-sensitive convert {dir} {revert?--revert}";
            job "fs_parity.site_case" fs_parity_site_case(SiteCaseParams) -> SiteDto [Operator, mutating] "forge site case-sensitive" => "forge site case-sensitive {site} {enabled?on:off}";
            job "fs_parity.lint_case" fs_parity_lint_case(LintCaseParams) -> CaseLintReportDto [Operator, mutating] "forge site lint-case" => "forge site lint-case {site} {fix?--fix} {dry_run?--dry-run} {strategy=--strategy}";
            // jobs
            call "jobs.list" jobs_list(JobListParams) -> JobListDto [Viewer] "forge jobs" => "forge jobs {active?--active}";
            call "jobs.get" jobs_get(JobIdParams) -> JobDto [Viewer] "forge jobs watch" => "forge jobs watch {job_id}";
            call "jobs.cancel" jobs_cancel(JobIdParams) -> JobCancelDto [Operator, mutating] "forge jobs cancel" => "forge jobs cancel {job_id}";
            // stats / metrics
            call "stats.get" stats_get(StatsParams) -> StatsDto [Viewer] "forge stats" => "forge stats {site} {window=--window}";
            call "stats.top" stats_top(NoParams) -> TopDto [Viewer] "forge top" => "forge top";
            job "stats.export" stats_export(StatsExportParams) -> StatsExportDto [Viewer] "forge stats export" => "forge stats export {site} {from=--from} {to=--to} {format=--output}";
            stream "metrics.subscribe" metrics_subscribe(MetricsSubscribeParams) -> MetricsEnvelope [Viewer, notify("metrics.site", "metrics.lagged")] "forge stats --watch" => "forge stats {site} {window=--window} --watch";
            call "metrics.unsubscribe" metrics_unsubscribe(UnsubscribeParams) -> Ack [Viewer] "";
            // remote (daemon side)
            call "remote.pair" remote_pair(NoParams) -> PairingCodeDto [Admin, mutating] "forge remote pair" => "forge remote pair";
            job "remote.listen" remote_listen(RemoteListenParams) -> Ack [Admin, mutating] "forge remote listen" => "forge remote listen {addr=--addr}";
            call "remote.clients" remote_clients(NoParams) -> RemoteClientListDto [Admin] "forge remote clients" => "forge remote clients";
            call "remote.revoke" remote_revoke(RemoteRevokeParams) -> Ack [Admin, mutating] "forge remote revoke" => "forge remote revoke {client_id}";
            // registry
            call "registry.list" registry_list(NoParams) -> RegistryListDto [Viewer] "forge registry list" => "forge registry list";
            call "registry.add" registry_add(RegistryAddParams) -> RegistryDto [Admin, mutating] "forge registry add" => "forge registry add {url} {name=--name}";
            call "registry.remove" registry_remove(RegistryRemoveParams) -> Ack [Admin, mutating] "forge registry remove" => "forge registry remove {name}";
            job "registry.update" registry_update(NoParams) -> RegistryListDto [Admin, mutating] "forge registry update" => "forge registry update";
            // system
            call "system.hello" system_hello(HelloParams) -> HelloDto [Viewer] "";
            call "system.ping" system_ping(NoParams) -> PongDto [Viewer] "";
            job "system.import_laragon" system_import_laragon(ImportLaragonParams) -> ImportReportDto [Admin, mutating] "forge import laragon" => "forge import laragon {path} {dry_run?--dry-run} {copy_tools?--copy-tools} {move_data?--move-data} {convert_case_sensitive?--convert-case}";
            job "system.bundle_export" system_bundle_export(BundleExportParams) -> ArtifactDto [Admin, mutating] "forge bundle export" => "forge bundle export {specs...} {output=--output}";
            job "system.bundle_import" system_bundle_import(BundleImportParams) -> ToolInstallResultDto [Admin, mutating] "forge bundle import" => "forge bundle import {file}";
            job "system.backup_now" system_backup_now(BackupNowParams) -> BackupReportDto [Operator, mutating] "forge backup now" => "forge backup now {instance}";
            call "system.backup_schedule" system_backup_schedule(BackupScheduleDto) -> BackupScheduleDto [Admin, mutating] "forge backup schedule" => "forge backup schedule {frequency} {at}";
            call "system.config_get" system_config_get(ConfigGetParams) -> ConfigValueDto [Viewer] "forge config get" => "forge config get {key}";
            call "system.config_set" system_config_set(ConfigSetParams) -> ConfigValueDto [Admin, mutating] "forge config set" => "forge config set {key} {value}";
            job "system.self_update" system_self_update(SelfUpdateParams) -> SelfUpdateDto [Admin, mutating] "forge self-update" => "forge self-update {check?--check} {channel=--channel}";
            // fs (restricted to etc/, logs/, www/)
            call "fs.read" fs_read(FsReadParams) -> FsReadDto [Admin] "";
            call "fs.write" fs_write(FsWriteParams) -> FsWriteDto [Admin, mutating] "";
            call "fs.tail" fs_tail(FsTailParams) -> FsTailDto [Admin] "";
            call "fs.list" fs_list(FsListParams) -> FsListDto [Operator] "";
            // events
            stream "events.subscribe" events_subscribe(EventsSubscribeParams) -> EventEnvelope [Viewer, notify("events.event", "events.lagged")] "forge status --watch; forge jobs watch" => "forge status --watch";
            call "events.unsubscribe" events_unsubscribe(UnsubscribeParams) -> Ack [Viewer] "";
            call "events.list" events_list(EventsListParams) -> EventPageDto [Viewer] "";
        }
    };
}
pub(crate) use api_methods;

/// One marker type per row.
macro_rules! method_item {
    (call $name:literal $fn:ident $params:ident $res:ident
        [$role:ident $(, $flag:ident $(($($arg:tt)*))?)*] $cli:literal $(=> $tpl:literal)?) => {
        #[doc = concat!("`", $name, "`: answers with [`", stringify!($res), "`]. CLI: ", $cli)]
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy)]
        pub struct $fn;

        impl ApiMethod for $fn {
            const INFO: MethodInfo = MethodInfo::new(
                $name,
                MethodKind::Call,
                Role::$role,
                $cli,
                stringify!($params),
                stringify!($res),
            ) $(.$flag($($($arg)*)?))* $(.cli_template($tpl))?;
            type Params = $params;
            type Result = $res;
        }
    };
    (job $name:literal $fn:ident $params:ident $res:ident
        [$role:ident $(, $flag:ident $(($($arg:tt)*))?)*] $cli:literal $(=> $tpl:literal)?) => {
        #[doc = concat!("`", $name, "`: long-running, answers with a `JobDto`; the job result is [`", stringify!($res), "`]. CLI: ", $cli)]
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy)]
        pub struct $fn;

        impl ApiMethod for $fn {
            const INFO: MethodInfo = MethodInfo::new(
                $name,
                MethodKind::Job,
                Role::$role,
                $cli,
                stringify!($params),
                "JobDto",
            )
            .job_result(stringify!($res)) $(.$flag($($($arg)*)?))* $(.cli_template($tpl))?;
            type Params = $params;
            type Result = JobDto;
        }
    };
    (stream $name:literal $fn:ident $params:ident $res:ident
        [$role:ident $(, $flag:ident $(($($arg:tt)*))?)*] $cli:literal $(=> $tpl:literal)?) => {
        #[doc = concat!("`", $name, "`: opens a stream of [`", stringify!($res), "`]. CLI: ", $cli)]
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy)]
        pub struct $fn;

        impl ApiMethod for $fn {
            const INFO: MethodInfo = MethodInfo::new(
                $name,
                MethodKind::Stream,
                Role::$role,
                $cli,
                stringify!($params),
                "SubscriptionDto",
            )
            .item(stringify!($res)) $(.$flag($($($arg)*)?))* $(.cli_template($tpl))?;
            type Params = $params;
            type Result = SubscriptionDto;
        }

        impl StreamMethod for $fn {
            type Item = $res;
        }
    };
}

macro_rules! define_methods {
    ($($kind:ident $name:literal $fn:ident ($params:ident) -> $res:ident [$($flags:tt)*] $cli:literal $(=> $tpl:literal)?;)*) => {
        $(method_item!($kind $name $fn $params $res [$($flags)*] $cli $(=> $tpl)?);)*

        /// Every method of the API, grouped by `group` as in docs/09.
        pub const METHODS: &[MethodInfo] = &[$(<$fn as ApiMethod>::INFO,)*];

        /// Registers the params/result types of every method for the TypeScript export.
        pub(crate) fn register_method_types(types: specta::Types) -> specta::Types {
            types
                .register::<JobDto>()
                .register::<SubscriptionDto>()
                $(.register::<$params>().register::<$res>())*
        }
    };
}

api_methods!(define_methods);

/// Looks a method up by name.
pub fn method_info(name: &str) -> Option<&'static MethodInfo> {
    METHODS.iter().find(|m| m.name == name)
}

/// Every stream method's notification names: `(item, lagged)`.
pub fn notifications() -> impl Iterator<Item = (&'static MethodInfo, &'static str, &'static str)> {
    METHODS
        .iter()
        .filter_map(|m| Some((m, m.notification?, m.lagged_notification?)))
}
