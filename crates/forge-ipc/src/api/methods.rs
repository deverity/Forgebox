//! The method table: one row per API method (docs/09: every `forge` command maps to a
//! method; ADR-0009). This list is the single source of truth: [`METHODS`], the marker
//! types (`methods::sites_add`), the typed client (`Client::sites_add`) and
//! `methods.ts` are all generated from it, so they cannot drift.
//!
//! Row: `kind "group.action" fn_name(Params) -> Result [MinRole, flags...] "forge cmd";`
//! * `call`: answers with `Result` within ~1 s.
//! * `job`: answers at once with `JobDto`; `Result` is the type of `job.done.result`.
//! * `stream`: answers with `SubscriptionDto`; `Result` is the item notification type.
//!
//! Flags: `mutating` (idempotency cache by `request_id`), `local_only` (refused on remote
//! connections), `notify("item", "lagged")` (stream notification names). The last
//! string lists the CLI commands served (`; `-separated, empty for GUI/protocol-only).

use super::*;

macro_rules! api_methods {
    ($callback:ident) => {
        $callback! {
            // daemon
            call "daemon.status" daemon_status(NoParams) -> DaemonStatusDto [Viewer] "forge daemon status";
            call "daemon.stop" daemon_stop(DaemonStopParams) -> Ack [Admin, mutating] "forge daemon stop";
            call "daemon.restart" daemon_restart(NoParams) -> Ack [Admin, mutating] "forge daemon restart";
            job "daemon.doctor" daemon_doctor(DoctorParams) -> DoctorReportDto [Viewer] "forge doctor; forge daemon doctor";
            // tools / catalog
            call "tools.search" tools_search(ToolSearchParams) -> CatalogSearchDto [Viewer] "forge search";
            call "tools.list" tools_list(ToolListParams) -> ToolListDto [Viewer] "forge tool list";
            call "tools.info" tools_info(ToolParams) -> ToolInfoDto [Viewer] "forge tool info";
            job "tools.install" tools_install(ToolInstallParams) -> ToolInstallResultDto [Admin, mutating] "forge tool install";
            job "tools.remove" tools_remove(ToolRemoveParams) -> Ack [Admin, mutating] "forge tool remove";
            job "tools.update" tools_update(ToolUpdateParams) -> ToolUpdatesDto [Admin, mutating] "forge tool update";
            job "tools.check_updates" tools_check_updates(ToolUpdateParams) -> ToolUpdatesDto [Viewer] "forge tool update --check";
            job "tools.rollback" tools_rollback(ToolRollbackParams) -> ToolVersionDto [Admin, mutating] "forge tool rollback";
            call "tools.policy" tools_policy(ToolPolicyParams) -> ToolDto [Admin, mutating] "forge tool policy";
            job "tools.add_local" tools_add_local(ToolAddLocalParams) -> ToolVersionDto [Admin, mutating, local_only] "forge tool add-local";
            job "tools.discover" tools_discover(NoParams) -> ToolDiscoverDto [Admin, mutating] "forge tool discover";
            call "tools.use" tools_use(ToolUseParams) -> ToolVersionDto [Operator, mutating] "forge use";
            call "tools.which" tools_which(ToolWhichParams) -> WhichDto [Viewer] "forge which";
            // services / instances
            job "services.start" services_start(ServiceTargetsParams) -> ServiceStatusDto [Operator, mutating] "forge start";
            job "services.stop" services_stop(ServiceTargetsParams) -> ServiceStatusDto [Operator, mutating] "forge stop";
            job "services.restart" services_restart(ServiceTargetsParams) -> ServiceStatusDto [Operator, mutating] "forge restart";
            job "services.reload" services_reload(ServiceTargetParams) -> ServiceStatusDto [Operator, mutating] "forge reload";
            call "services.status" services_status(NoParams) -> ServiceStatusDto [Viewer] "forge status";
            job "services.create" services_create(ServiceCreateParams) -> InstanceDto [Admin, mutating] "forge service create";
            job "services.remove" services_remove(ServiceRemoveParams) -> Ack [Admin, mutating] "forge service remove";
            call "services.config" services_config(ServiceTargetParams) -> ConfigFilesDto [Operator, local_only] "forge service config";
            call "services.config_files" services_config_files(ServiceTargetParams) -> ConfigFilesDto [Viewer] "";
            job "services.set" services_set(ServiceSetParams) -> ServiceStatusDto [Admin, mutating] "forge service set";
            call "services.ports" services_ports(NoParams) -> PortMapDto [Viewer] "forge service ports";
            // logs
            call "logs.read" logs_read(LogsReadParams) -> LogChunkDto [Viewer] "forge logs; forge daemon logs";
            stream "logs.subscribe" logs_subscribe(LogsSubscribeParams) -> LogEnvelope [Viewer, notify("logs.line", "logs.lagged")] "forge logs -f; forge daemon logs -f";
            call "logs.unsubscribe" logs_unsubscribe(UnsubscribeParams) -> Ack [Viewer] "";
            // php
            call "php.list" php_list(NoParams) -> PhpVersionListDto [Viewer] "forge php list";
            call "php.pool_status" php_pool_status(PhpPoolStatusParams) -> PoolListDto [Viewer] "forge php pool status";
            call "php.ext_list" php_ext_list(PhpVersionParams) -> PhpExtListDto [Viewer] "forge php ext list";
            job "php.ext_enable" php_ext_enable(PhpExtParams) -> PhpExtListDto [Admin, mutating] "forge php ext enable";
            job "php.ext_disable" php_ext_disable(PhpExtParams) -> PhpExtListDto [Admin, mutating] "forge php ext disable";
            job "php.xdebug" php_xdebug(PhpXdebugParams) -> PoolDto [Operator, mutating] "forge php xdebug";
            call "php.ini" php_ini(PhpVersionParams) -> PhpIniDto [Viewer] "forge php ini";
            call "php.ini_open" php_ini_open(PhpVersionParams) -> PhpIniDto [Operator, local_only] "forge php ini --open";
            // sites, proxies, parked folders, projects, processes
            call "sites.list" sites_list(NoParams) -> SiteListDto [Viewer] "forge site list";
            job "sites.add" sites_add(SiteAddParams) -> SiteDto [Operator, mutating] "forge site add; forge link";
            job "sites.remove" sites_remove(SiteRemoveParams) -> Ack [Operator, mutating] "forge site remove; forge unlink; forge proxy remove";
            job "sites.php" sites_php(SitePhpParams) -> SiteDto [Operator, mutating] "forge site php";
            job "sites.secure" sites_secure(SiteSecureParams) -> SiteDto [Operator, mutating] "forge site secure; forge site unsecure";
            job "sites.server" sites_server(SiteServerParams) -> SiteDto [Operator, mutating] "forge site server";
            job "sites.alias" sites_alias(SiteAliasParams) -> SiteDto [Operator, mutating] "forge site alias";
            call "sites.open" sites_open(SiteOpenParams) -> Ack [Operator, local_only] "forge site open";
            call "sites.info" sites_info(SiteParams) -> SiteInfoDto [Viewer] "forge site info";
            call "sites.park" sites_park(ParkParams) -> ParkedDirDto [Operator, mutating] "forge park";
            call "sites.unpark" sites_unpark(ParkParams) -> Ack [Operator, mutating] "forge unpark";
            call "sites.parked" sites_parked(NoParams) -> ParkedListDto [Viewer] "forge parked";
            job "sites.proxy_add" sites_proxy_add(ProxyAddParams) -> SiteDto [Operator, mutating] "forge proxy add";
            job "sites.up" sites_up(ProjectParams) -> ProjectUpDto [Operator, mutating] "forge up";
            job "sites.down" sites_down(ProjectParams) -> Ack [Operator, mutating] "forge down";
            call "sites.ps" sites_ps(SitePsParams) -> SiteProcessListDto [Viewer] "forge ps";
            job "sites.run" sites_run(SiteRunParams) -> SiteProcessDto [Operator, mutating] "forge run";
            job "sites.share" sites_share(SiteShareParams) -> ShareDto [Operator, mutating] "forge share";
            job "sites.new" sites_new(SiteNewParams) -> SiteDto [Admin, mutating] "forge new";
            // databases
            call "db.list" db_list(DbListParams) -> DatabaseListDto [Viewer] "forge db list";
            call "db.create" db_create(DbCreateParams) -> DatabaseDto [Operator, mutating] "forge db create";
            call "db.drop" db_drop(DbDropParams) -> Ack [Operator, mutating] "forge db drop";
            job "db.export" db_export(DbExportParams) -> DbExportDto [Operator, mutating] "forge db export";
            job "db.import" db_import(DbImportParams) -> Ack [Operator, mutating] "forge db import";
            call "db.shell" db_shell(DbShellParams) -> Ack [Operator, local_only] "forge db shell";
            call "db.url" db_url(DbUrlParams) -> DbUrlDto [Operator] "forge db url";
            call "db.open" db_open(DbOpenParams) -> Ack [Operator, local_only] "forge db open";
            // net
            call "net.dns_status" net_dns_status(NoParams) -> DnsStatusDto [Viewer] "forge dns status";
            job "net.dns_enable" net_dns_enable(NoParams) -> DnsStatusDto [Admin, mutating] "forge dns enable";
            job "net.dns_disable" net_dns_disable(NoParams) -> DnsStatusDto [Admin, mutating] "forge dns disable";
            call "net.dns_test" net_dns_test(DnsTestParams) -> DnsTestDto [Viewer] "forge dns test";
            job "net.lan_expose" net_lan_expose(LanSiteParams) -> LanAccessDto [Admin, mutating] "forge lan expose";
            job "net.lan_unexpose" net_lan_unexpose(LanSiteParams) -> Ack [Admin, mutating] "forge lan unexpose";
            call "net.lan_qr" net_lan_qr(LanSiteParams) -> LanAccessDto [Viewer] "forge lan qr";
            // tls
            call "tls.ca_status" tls_ca_status(NoParams) -> CaStatusDto [Viewer] "forge tls ca status";
            job "tls.ca_install" tls_ca_install(CaInstallParams) -> CaStatusDto [Admin, mutating] "forge tls ca install";
            call "tls.ca_export" tls_ca_export(NoParams) -> PemDto [Viewer] "forge tls ca export";
            job "tls.ca_rotate" tls_ca_rotate(CaRotateParams) -> CaStatusDto [Admin, mutating] "forge tls ca rotate";
            job "tls.issue" tls_issue(TlsHostParams) -> CertificateDto [Operator, mutating] "forge tls issue";
            job "tls.renew" tls_renew(TlsRenewParams) -> CertificateListDto [Operator, mutating] "forge tls renew";
            call "tls.export" tls_export(TlsExportParams) -> CertExportDto [Admin] "forge tls export";
            // mail
            call "mail.status" mail_status(NoParams) -> MailStatusDto [Viewer] "forge mail status";
            call "mail.open" mail_open(NoParams) -> Ack [Viewer, local_only] "forge mail open";
            call "mail.list" mail_list(MailListParams) -> MailListDto [Viewer] "forge mail list";
            call "mail.show" mail_show(MailShowParams) -> MailMessageDto [Viewer] "forge mail show";
            call "mail.clear" mail_clear(MailClearParams) -> Ack [Operator, mutating] "forge mail clear";
            call "mail.env" mail_env(NoParams) -> EnvVarsDto [Viewer] "forge mail env";
            // shell / env
            call "shell.open" shell_open(ShellOpenParams) -> Ack [Operator, local_only] "forge shell";
            call "shell.env" shell_env(ShellEnvParams) -> ShellEnvDto [Viewer, local_only] "forge env; forge exec";
            call "shell.path_show" shell_path_show(NoParams) -> UserPathDto [Viewer] "forge path show";
            call "shell.path_add" shell_path_add(NoParams) -> UserPathDto [Admin, mutating] "forge path add";
            call "shell.path_remove" shell_path_remove(NoParams) -> UserPathDto [Admin, mutating] "forge path remove";
            // fs / Linux parity
            call "fs_parity.case_status" fs_parity_case_status(CaseDirParams) -> CaseSensitivityDto [Viewer] "forge fs case-sensitive status";
            call "fs_parity.case_enable" fs_parity_case_enable(CaseDirParams) -> CaseSensitivityDto [Admin, mutating] "forge fs case-sensitive enable";
            call "fs_parity.case_disable" fs_parity_case_disable(CaseDirParams) -> CaseSensitivityDto [Admin, mutating] "forge fs case-sensitive disable";
            job "fs_parity.case_convert" fs_parity_case_convert(CaseConvertParams) -> CaseConvertDto [Admin, mutating] "forge fs case-sensitive convert";
            job "fs_parity.site_case" fs_parity_site_case(SiteCaseParams) -> SiteDto [Operator, mutating] "forge site case-sensitive";
            job "fs_parity.lint_case" fs_parity_lint_case(LintCaseParams) -> CaseLintReportDto [Operator, mutating] "forge site lint-case";
            // jobs
            call "jobs.list" jobs_list(JobListParams) -> JobListDto [Viewer] "forge jobs";
            call "jobs.get" jobs_get(JobIdParams) -> JobDto [Viewer] "forge jobs watch";
            call "jobs.cancel" jobs_cancel(JobIdParams) -> JobCancelDto [Operator, mutating] "forge jobs cancel";
            // stats / metrics
            call "stats.get" stats_get(StatsParams) -> StatsDto [Viewer] "forge stats";
            call "stats.top" stats_top(NoParams) -> TopDto [Viewer] "forge top";
            job "stats.export" stats_export(StatsExportParams) -> StatsExportDto [Viewer] "forge stats export";
            stream "metrics.subscribe" metrics_subscribe(MetricsSubscribeParams) -> MetricsEnvelope [Viewer, notify("metrics.site", "metrics.lagged")] "forge stats --watch";
            call "metrics.unsubscribe" metrics_unsubscribe(UnsubscribeParams) -> Ack [Viewer] "";
            // remote (daemon side)
            call "remote.pair" remote_pair(NoParams) -> PairingCodeDto [Admin, mutating] "forge remote pair";
            job "remote.listen" remote_listen(RemoteListenParams) -> Ack [Admin, mutating] "forge remote listen";
            call "remote.clients" remote_clients(NoParams) -> RemoteClientListDto [Admin] "forge remote clients";
            call "remote.revoke" remote_revoke(RemoteRevokeParams) -> Ack [Admin, mutating] "forge remote revoke";
            // registry
            call "registry.list" registry_list(NoParams) -> RegistryListDto [Viewer] "forge registry list";
            call "registry.add" registry_add(RegistryAddParams) -> RegistryDto [Admin, mutating] "forge registry add";
            call "registry.remove" registry_remove(RegistryRemoveParams) -> Ack [Admin, mutating] "forge registry remove";
            job "registry.update" registry_update(NoParams) -> RegistryListDto [Admin, mutating] "forge registry update";
            // system
            call "system.hello" system_hello(HelloParams) -> HelloDto [Viewer] "";
            call "system.ping" system_ping(NoParams) -> PongDto [Viewer] "";
            job "system.import_laragon" system_import_laragon(ImportLaragonParams) -> ImportReportDto [Admin, mutating] "forge import laragon";
            job "system.bundle_export" system_bundle_export(BundleExportParams) -> ArtifactDto [Admin, mutating] "forge bundle export";
            job "system.bundle_import" system_bundle_import(BundleImportParams) -> ToolInstallResultDto [Admin, mutating] "forge bundle import";
            job "system.backup_now" system_backup_now(BackupNowParams) -> BackupReportDto [Operator, mutating] "forge backup now";
            call "system.backup_schedule" system_backup_schedule(BackupScheduleDto) -> BackupScheduleDto [Admin, mutating] "forge backup schedule";
            call "system.config_get" system_config_get(ConfigGetParams) -> ConfigValueDto [Viewer] "forge config get";
            call "system.config_set" system_config_set(ConfigSetParams) -> ConfigValueDto [Admin, mutating] "forge config set";
            job "system.self_update" system_self_update(SelfUpdateParams) -> SelfUpdateDto [Admin, mutating] "forge self-update";
            // fs (restricted to etc/, logs/, www/)
            call "fs.read" fs_read(FsReadParams) -> FsReadDto [Admin] "";
            call "fs.write" fs_write(FsWriteParams) -> FsWriteDto [Admin, mutating] "";
            call "fs.tail" fs_tail(FsTailParams) -> FsTailDto [Admin] "";
            call "fs.list" fs_list(FsListParams) -> FsListDto [Operator] "";
            // events
            stream "events.subscribe" events_subscribe(EventsSubscribeParams) -> EventEnvelope [Viewer, notify("events.event", "events.lagged")] "forge status --watch; forge jobs watch";
            call "events.unsubscribe" events_unsubscribe(UnsubscribeParams) -> Ack [Viewer] "";
            call "events.list" events_list(EventsListParams) -> EventPageDto [Viewer] "";
        }
    };
}
pub(crate) use api_methods;

/// One marker type per row.
macro_rules! method_item {
    (call $name:literal $fn:ident $params:ident $res:ident
        [$role:ident $(, $flag:ident $(($($arg:tt)*))?)*] $cli:literal) => {
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
            ) $(.$flag($($($arg)*)?))*;
            type Params = $params;
            type Result = $res;
        }
    };
    (job $name:literal $fn:ident $params:ident $res:ident
        [$role:ident $(, $flag:ident $(($($arg:tt)*))?)*] $cli:literal) => {
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
            .job_result(stringify!($res)) $(.$flag($($($arg)*)?))*;
            type Params = $params;
            type Result = JobDto;
        }
    };
    (stream $name:literal $fn:ident $params:ident $res:ident
        [$role:ident $(, $flag:ident $(($($arg:tt)*))?)*] $cli:literal) => {
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
            .item(stringify!($res)) $(.$flag($($($arg)*)?))*;
            type Params = $params;
            type Result = SubscriptionDto;
        }

        impl StreamMethod for $fn {
            type Item = $res;
        }
    };
}

macro_rules! define_methods {
    ($($kind:ident $name:literal $fn:ident ($params:ident) -> $res:ident [$($flags:tt)*] $cli:literal;)*) => {
        $(method_item!($kind $name $fn $params $res [$($flags)*] $cli);)*

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
