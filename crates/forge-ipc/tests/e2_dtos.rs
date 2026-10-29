//! The E2 contract (ADR-0041): every new DTO survives a serde round trip with the wire
//! shape clients rely on, and every extended DTO still reads the JSON an API 1.0 peer sends
//! (missing fields take their defaults).

use std::collections::BTreeMap;
use std::fmt::Debug;

use pretty_assertions::assert_eq;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use forge_core::Timestamp;
use forge_ipc::api::*;

fn ts() -> Timestamp {
    "2026-10-29T12:00:00Z".parse().unwrap()
}

fn tool(id: &str) -> ToolId {
    ToolId::new(id).unwrap()
}

/// Serializes, checks that both the `Value` and the text forms read back equal, and
/// returns the JSON for shape assertions.
fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) -> Value {
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, value);
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value);
    json
}

fn from<T: DeserializeOwned>(json: Value) -> T {
    serde_json::from_value(json).unwrap()
}

fn variant(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

// --- catalog ------------------------------------------------------------------------------

fn catalog_entry_1_0() -> Value {
    json!({
        "id": "php",
        "name": "PHP",
        "category": "runtime",
        "description": "Linguagem PHP",
        "tags": ["php"],
        "registry": "forgebox",
        "versions": ["8.4.14"],
        "installed": true
    })
}

#[test]
fn catalog_entries_round_trip_with_the_1_1_metadata() {
    for (kind, wire) in [
        (CatalogKind::Tool, "tool"),
        (CatalogKind::Extension, "extension"),
        (CatalogKind::Stack, "stack"),
    ] {
        assert_eq!(round_trip(&kind), wire);
    }
    let mut entry: CatalogEntryDto = from(catalog_entry_1_0());
    entry.kind = CatalogKind::Extension;
    entry.summary = Some("Depurador".into());
    entry.color = Some("#777BB4".into());
    entry.icon_sha256 = Some("ab".repeat(32));
    entry.platforms = vec!["windows-x64".into()];
    entry.native = false;
    entry.official = true;
    entry.latest = Some("8.4.14".into());
    entry.update_available = Some("8.4.15".into());
    entry.size_estimate_bytes = Some(35 << 20);
    let json = round_trip(&entry);
    assert_eq!(json["kind"], "extension");
    assert_eq!(json["native"], false);
    assert_eq!(json["size_estimate_bytes"], 35u64 << 20);
    round_trip(&CatalogSearchDto {
        entries: vec![entry],
    });
}

#[test]
fn catalog_entries_read_api_1_0_json() {
    let entry: CatalogEntryDto = from(catalog_entry_1_0());
    assert_eq!(entry.kind, CatalogKind::Tool);
    // 1.0 daemons only offered native tools; nothing else is known.
    assert!(entry.native);
    assert!(!entry.official);
    assert_eq!(
        (
            &entry.summary,
            &entry.color,
            &entry.icon_sha256,
            &entry.latest,
            &entry.update_available,
            entry.size_estimate_bytes
        ),
        (&None, &None, &None, &None, &None, None)
    );
    assert!(entry.platforms.is_empty());
}

fn tool_dto_json() -> Value {
    json!({
        "id": "php",
        "name": "PHP",
        "category": "runtime",
        "description": null,
        "installed": [],
        "current": null,
        "policy": "auto-patch",
        "latest": "8.4.14",
        "update_available": null,
        "has_service": true
    })
}

#[test]
fn tool_info_round_trips_with_variants_and_stacks() {
    let mut info: ToolInfoDto = from(json!({
        "tool": tool_dto_json(),
        "available": [],
        "used_by": []
    }));
    info.kind = CatalogKind::Stack;
    info.description_md = Some("# Laravel\n\nPHP **8.4**".into());
    info.homepage = Some("https://laravel.com".into());
    info.docs = Some("https://laravel.com/docs".into());
    info.changelog_url = Some("https://github.com/laravel/framework/releases".into());
    info.license = Some("MIT".into());
    info.variants = vec![VariantDefDto {
        key: "ts".into(),
        values: vec!["nts".into(), "ts".into()],
        default: Some("nts".into()),
        labels: variant(&[("nts", "Non Thread Safe"), ("ts", "Thread Safe")]),
        derived: false,
    }];
    info.suggests = vec![tool("composer")];
    info.extends = Some(tool("php"));
    info.stack = Some(StackDto {
        tools: vec![
            StackToolDto {
                id: tool("php"),
                series: Some("8.4".into()),
                version: None,
                instance: None,
                extensions: vec![tool("xdebug")],
            },
            StackToolDto {
                id: tool("mysql"),
                series: Some("8.4".into()),
                version: None,
                instance: Some("default".into()),
                extensions: vec![],
            },
        ],
    });
    let json = round_trip(&info);
    assert_eq!(json["kind"], "stack");
    assert_eq!(json["variants"][0]["labels"]["nts"], "Non Thread Safe");
    assert_eq!(json["stack"]["tools"][0]["extensions"], json!(["xdebug"]));
    // A minimal variant and stack tool read with defaults.
    let def: VariantDefDto = from(json!({"key": "arch", "values": ["x64"]}));
    assert!(def.labels.is_empty() && !def.derived && def.default.is_none());
    let stack_tool: StackToolDto = from(json!({"id": "nginx"}));
    assert!(stack_tool.extensions.is_empty() && stack_tool.series.is_none());
}

#[test]
fn tool_info_reads_api_1_0_json() {
    let info: ToolInfoDto = from(json!({
        "tool": tool_dto_json(),
        "available": [{"version": "8.4.14", "variants": [], "channel": "stable"}],
        "used_by": []
    }));
    assert_eq!(info.kind, CatalogKind::Tool);
    assert!(info.variants.is_empty() && info.suggests.is_empty());
    assert!(info.description_md.is_none() && info.stack.is_none() && info.extends.is_none());
}

#[test]
fn install_and_remove_take_an_extension_target() {
    let params = ToolInstallParams {
        specs: vec![ToolSpec::from("xdebug")],
        variant: None,
        yes: false,
        target: Some(ToolSpec::from("php@8.4")),
    };
    assert_eq!(round_trip(&params)["target"], "php@8.4");
    let old: ToolInstallParams = from(json!({"specs": ["php@8.4"], "variant": null, "yes": true}));
    assert_eq!(old.target, None);
    let remove = ToolRemoveParams {
        spec: ToolSpec::from("xdebug@3.4.1"),
        keep_config: false,
        confirm: true,
        target: Some(ToolSpec::from("php@8.4")),
    };
    round_trip(&remove);
    let old: ToolRemoveParams = from(json!({"spec": "php@8.2.29", "confirm": true}));
    assert_eq!(old.target, None);
}

// --- releases, plan, history, icon ---------------------------------------------------------

#[test]
fn releases_round_trip() {
    let params = ToolReleasesParams {
        tool: tool("php"),
        channel: Some("preview".into()),
    };
    round_trip(&params);
    let minimal: ToolReleasesParams = from(json!({"tool": "php"}));
    assert_eq!(minimal.channel, None);
    let release = ToolReleaseDto {
        version: "8.4.14".into(),
        series: "8.4".into(),
        channel: Some("stable".into()),
        published_at: Some(ts()),
        size_bytes: Some(32 << 20),
        installed: true,
        variants: vec![
            ReleaseVariantDto {
                variant: variant(&[("arch", "x64"), ("compiler", "vs17"), ("ts", "nts")]),
                canonical_name: "8.4.14-nts-vs17-x64".into(),
                size_bytes: Some(32 << 20),
                installed: true,
            },
            ReleaseVariantDto {
                variant: variant(&[("arch", "x64"), ("compiler", "vs17"), ("ts", "ts")]),
                canonical_name: "8.4.14-ts-vs17-x64".into(),
                size_bytes: None,
                installed: false,
            },
        ],
    };
    let dto = ToolReleasesDto {
        tool: tool("php"),
        releases: vec![release],
        stale: true,
        fetched_at: Some(ts()),
    };
    let json = round_trip(&dto);
    assert_eq!(json["releases"][0]["variants"][0]["variant"]["ts"], "nts");
    assert_eq!(json["releases"][0]["published_at"], "2026-10-29T12:00:00Z");
    assert_eq!(json["stale"], true);
    // Optional parts may be missing.
    let bare: ToolReleasesDto = from(json!({
        "tool": "php",
        "releases": [{"version": "8.4.14", "series": "8.4"}]
    }));
    assert!(!bare.stale && bare.fetched_at.is_none());
    assert!(bare.releases[0].variants.is_empty() && !bare.releases[0].installed);
}

#[test]
fn plan_round_trips() {
    for (reason, wire) in [
        (ToolPlanReason::Requested, "requested"),
        (ToolPlanReason::Dependency, "dependency"),
        (ToolPlanReason::Stack, "stack"),
        (ToolPlanReason::Extension, "extension"),
    ] {
        assert_eq!(round_trip(&reason), wire);
    }
    let params = ToolPlanParams {
        specs: vec![ToolSpec::from("php@8.3")],
        stack: Some("laravel".into()),
        variant: Some("nts".into()),
        target: None,
    };
    round_trip(&params);
    let stack_only: ToolPlanParams = from(json!({"stack": "laravel"}));
    assert!(stack_only.specs.is_empty());
    assert_eq!(stack_only.stack.as_deref(), Some("laravel"));
    let plan = ToolPlanDto {
        items: vec![
            ToolPlanItemDto {
                tool: tool("php"),
                kind: CatalogKind::Tool,
                spec: ToolSpec::from("php@8.3"),
                version: "8.3.27".into(),
                variant: variant(&[("arch", "x64"), ("compiler", "vs16"), ("ts", "nts")]),
                canonical_name: Some("8.3.27-nts-vs16-x64".into()),
                size_bytes: Some(31 << 20),
                size_estimated: false,
                already_installed: false,
                reason: ToolPlanReason::Stack,
                target: None,
            },
            ToolPlanItemDto {
                tool: tool("xdebug"),
                kind: CatalogKind::Extension,
                spec: ToolSpec::from("xdebug"),
                version: "3.4.7".into(),
                variant: BTreeMap::new(),
                canonical_name: None,
                size_bytes: None,
                size_estimated: false,
                already_installed: false,
                reason: ToolPlanReason::Extension,
                target: Some(ToolSpec::from("php@8.3")),
            },
        ],
        total_bytes: 31 << 20,
        unknown_size: 1,
    };
    let json = round_trip(&plan);
    assert_eq!(json["items"][1]["reason"], "extension");
    assert_eq!(json["items"][1]["kind"], "extension");
    assert_eq!(json["unknown_size"], 1);
    let minimal: ToolPlanItemDto = from(json!({
        "tool": "nginx",
        "spec": "nginx",
        "version": "1.30.6",
        "reason": "requested"
    }));
    assert_eq!(minimal.kind, CatalogKind::Tool);
    assert!(!minimal.size_estimated && !minimal.already_installed);
}

#[test]
fn history_round_trips() {
    for (trigger, wire) in [
        (UpdateTriggerDto::Auto, "auto"),
        (UpdateTriggerDto::User, "user"),
        (UpdateTriggerDto::Rollback, "rollback"),
    ] {
        assert_eq!(round_trip(&trigger), wire);
    }
    for (status, wire) in [
        (UpdateStatusDto::Started, "started"),
        (UpdateStatusDto::Completed, "completed"),
        (UpdateStatusDto::Failed, "failed"),
        (UpdateStatusDto::RolledBack, "rolled_back"),
    ] {
        assert_eq!(round_trip(&status), wire);
    }
    round_trip(&ToolHistoryParams {
        tool: tool("php"),
        series: Some("8.3".into()),
        limit: Some(20),
    });
    let update = UpdateId::new();
    let history = UpdateHistoryDto {
        tool: tool("php"),
        entries: vec![
            UpdateHistoryEntryDto {
                id: UpdateId::new(),
                series: Some("8.3".into()),
                from: Some("8.3.37".into()),
                to: "8.3.36".into(),
                from_version_id: None,
                to_version_id: Some(VersionId::new()),
                policy: None,
                trigger: UpdateTriggerDto::Rollback,
                status: UpdateStatusDto::Completed,
                error: None,
                job_id: Some(JobId::new()),
                rollback_of: Some(update),
                started_at: ts(),
                finished_at: Some(ts()),
            },
            UpdateHistoryEntryDto {
                id: update,
                series: Some("8.3".into()),
                from: Some("8.3.36".into()),
                to: "8.3.37".into(),
                from_version_id: Some(VersionId::new()),
                to_version_id: None,
                policy: Some(UpdatePolicy::AutoPatch),
                trigger: UpdateTriggerDto::Auto,
                status: UpdateStatusDto::RolledBack,
                error: None,
                job_id: None,
                rollback_of: None,
                started_at: ts(),
                finished_at: Some(ts()),
            },
        ],
    };
    let json = round_trip(&history);
    assert_eq!(json["entries"][1]["policy"], "auto-patch");
    assert_eq!(json["entries"][0]["rollback_of"], json["entries"][1]["id"]);
}

#[test]
fn icons_round_trip() {
    let icon = IconDto {
        tool: tool("php"),
        svg: "<svg xmlns=\"http://www.w3.org/2000/svg\"/>".into(),
        sha256: "0f".repeat(32),
    };
    let json = round_trip(&icon);
    assert_eq!(json["svg"], icon.svg);
}

// --- services ----------------------------------------------------------------------------

fn instance_1_0() -> Value {
    json!({
        "id": InstanceId::new(),
        "tool": "mysql",
        "version_id": VersionId::new(),
        "version": "8.4.7",
        "name": "default",
        "state": "running",
        "pid": 4412,
        "ports": [],
        "autostart": true,
        "started_at": null,
        "cpu_pct": 0.5,
        "rss_bytes": 1048576,
        "restarts": 0,
        "last_error": null
    })
}

#[test]
fn instances_carry_their_data_dir_and_size() {
    let old: InstanceDto = from(instance_1_0());
    assert_eq!((old.data_dir.as_deref(), old.size_bytes), (None, None));
    let mut instance = old;
    instance.data_dir = Some("C:/Forgebox/data/mysql/default".into());
    instance.size_bytes = Some(120 << 20);
    let json = round_trip(&instance);
    assert_eq!(json["data_dir"], "C:/Forgebox/data/mysql/default");
    let create = ServiceCreateParams {
        tool: ToolSpec::from("mysql@8.4"),
        name: "legacy".into(),
        port: None,
        autostart: false,
        data_dir: Some("D:/dados/mysql".into()),
    };
    round_trip(&create);
    let old: ServiceCreateParams = from(json!({"tool": "mysql@8.4", "name": "legacy"}));
    assert_eq!(old.data_dir, None);
}

#[test]
fn setting_values_are_untagged_by_kind() {
    for (value, wire) in [
        (SettingValueDto::Bool(true), json!(true)),
        (SettingValueDto::Int(9003), json!(9003)),
        (SettingValueDto::Float(0.5), json!(0.5)),
        (SettingValueDto::Text("512M".into()), json!("512M")),
        (
            SettingValueDto::List(vec!["debug".into(), "develop".into()]),
            json!(["debug", "develop"]),
        ),
    ] {
        assert_eq!(round_trip(&value), wire);
    }
    // A whole number reads as an integer, a select of "-1" stays text.
    assert_eq!(
        from::<SettingValueDto>(json!(120)),
        SettingValueDto::Int(120)
    );
    assert_eq!(
        from::<SettingValueDto>(json!("-1")),
        SettingValueDto::Text("-1".into())
    );
}

fn memory_limit() -> QuickSettingDto {
    QuickSettingDto {
        key: "memory_limit".into(),
        label: Some("Memory limit".into()),
        kind: QuickSettingKind::Select,
        options: vec!["128M".into(), "512M".into(), "-1".into()],
        min: None,
        max: None,
        step: None,
        value: Some(SettingValueDto::Text("1G".into())),
        default: Some(SettingValueDto::Text("512M".into())),
        is_default: false,
        apply: SettingApply::Reload,
        file: Some("php_ini".into()),
        also_set: vec![],
        reserve: false,
    }
}

#[test]
fn service_settings_round_trip() {
    for (kind, wire) in [
        (QuickSettingKind::Toggle, "toggle"),
        (QuickSettingKind::Select, "select"),
        (QuickSettingKind::Number, "number"),
        (QuickSettingKind::Text, "text"),
        (QuickSettingKind::Path, "path"),
        (QuickSettingKind::Port, "port"),
        (QuickSettingKind::Slider, "slider"),
        (QuickSettingKind::Multi, "multi"),
    ] {
        assert_eq!(round_trip(&kind), wire);
    }
    for (apply, wire) in [
        (SettingApply::Reload, "reload"),
        (SettingApply::Restart, "restart"),
        (SettingApply::None, "none"),
    ] {
        assert_eq!(round_trip(&apply), wire);
    }
    let slider = QuickSettingDto {
        key: "max_connections".into(),
        label: None,
        kind: QuickSettingKind::Slider,
        options: vec![],
        min: Some(10.0),
        max: Some(500.0),
        step: Some(10.0),
        value: Some(SettingValueDto::Int(150)),
        default: Some(SettingValueDto::Int(151)),
        is_default: false,
        apply: SettingApply::Restart,
        file: Some("my_ini".into()),
        also_set: vec!["mysqlx_max_connections".into()],
        reserve: false,
    };
    let policy = RestartPolicyDto {
        on: RestartOn::Failure,
        max: 3,
        window_secs: 60,
    };
    let settings = ServiceSettingsDto {
        target: ServiceTarget::from("mysql/default"),
        tool: tool("mysql"),
        settings: vec![memory_limit(), slider],
        files: vec![FileRefDto {
            role: "my_ini".into(),
            path: "C:/Forgebox/etc/mysql/default/my.ini".into(),
            fs_path: Some("etc/mysql/default/my.ini".into()),
            managed: true,
        }],
        ports: vec![ServicePortSettingDto {
            name: "main".into(),
            port: Some(3307),
            default: Some(3306),
            protocol: PortProtocol::Tcp,
            range: Some("databases".into()),
        }],
        advanced: Some(InstanceAdvancedDto {
            autostart: true,
            args: vec!["--skip-name-resolve".into()],
            env: BTreeMap::from([("TZ".into(), "UTC".into())]),
            restart_policy: RestartPolicyDto {
                on: RestartOn::Always,
                ..policy
            },
            restart_policy_default: policy,
            versions: vec!["8.4.14".into(), "8.4.13".into()],
        }),
    };
    let json = round_trip(&settings);
    assert_eq!(json["settings"][0]["kind"], "select");
    assert_eq!(json["settings"][1]["value"], 150);
    assert_eq!(json["ports"][0]["protocol"], "tcp");
    assert_eq!(json["advanced"]["restart_policy"]["on"], "always");
    // Operators get no `advanced`; a minimal setting reads with defaults.
    let bare: ServiceSettingsDto = from(json!({
        "target": "php@8.3",
        "tool": "php",
        "settings": [{"key": "opcache", "kind": "toggle", "apply": "restart"}]
    }));
    assert!(bare.advanced.is_none() && bare.files.is_empty() && bare.ports.is_empty());
    assert_eq!(bare.settings[0].value, None);
    assert!(!bare.settings[0].is_default && bare.settings[0].options.is_empty());
}

#[test]
fn service_update_is_a_patch() {
    for (on, wire) in [
        (RestartOn::Never, "never"),
        (RestartOn::Failure, "failure"),
        (RestartOn::Always, "always"),
    ] {
        assert_eq!(round_trip(&on), wire);
    }
    let params = ServiceUpdateParams {
        instance: InstanceRef::from("mysql/default"),
        autostart: Some(false),
        launch: Some(LaunchPatchDto {
            args: Some(vec![]),
            env: None,
        }),
        restart_policy: Some(RestartPolicyPatchDto {
            on: Some(RestartOn::Always),
            max: None,
            window_secs: Some(120),
        }),
        version: Some("8.4.14".into()),
    };
    let json = round_trip(&params);
    // A cleared list is an empty list, not an absent one.
    assert_eq!(json["launch"]["args"], json!([]));
    assert_eq!(json["launch"]["env"], Value::Null);
    // Everything absent: nothing changes.
    let bare: ServiceUpdateParams = from(json!({"instance": "mysql/default"}));
    assert_eq!(
        (
            bare.autostart,
            bare.launch,
            bare.restart_policy,
            bare.version
        ),
        (None, None, None, None)
    );
    let patch: RestartPolicyPatchDto = from(json!({}));
    assert_eq!(patch, RestartPolicyPatchDto::default());
}

// --- sites, tls, events --------------------------------------------------------------------

#[test]
fn site_driver_and_process_correlation() {
    let params = SiteDriverParams {
        site: SiteRef::from("loja.test"),
        driver: Some("laravel".into()),
    };
    round_trip(&params);
    let auto: SiteDriverParams = from(json!({"site": "loja.test"}));
    assert_eq!(auto.driver, None);
    let process_1_0 = json!({
        "site_id": SiteId::new(),
        "host": "loja.test",
        "name": "web",
        "command": "npm run dev",
        "state": "running",
        "pid": 77,
        "port": 3000,
        "autostart": true
    });
    let mut process: SiteProcessDto = from(process_1_0);
    assert_eq!(process.instance_id, None);
    let id = InstanceId::new();
    process.instance_id = Some(id);
    assert_eq!(round_trip(&process)["instance_id"], json!(id));
}

#[test]
fn certificates_say_whether_they_are_trusted() {
    let cert_1_0 = json!({
        "host": "loja.test",
        "sans": ["loja.test", "*.loja.test"],
        "site_id": null,
        "not_before": "2026-10-01T00:00:00Z",
        "not_after": "2027-01-01T00:00:00Z",
        "thumbprint_sha256": "aa"
    });
    let mut cert: CertificateDto = from(cert_1_0);
    assert_eq!(
        (cert.trusted, cert.ca_thumbprint_sha256.as_deref()),
        (None, None)
    );
    cert.trusted = Some(false);
    cert.ca_thumbprint_sha256 = Some("bb".into());
    let list = CertificateListDto {
        certificates: vec![cert],
    };
    let json = round_trip(&list);
    assert_eq!(json["certificates"][0]["trusted"], false);
}

#[test]
fn service_events_name_the_site_of_an_app_instance() {
    let payload_1_0 = json!({
        "v": 1,
        "instance_id": InstanceId::new(),
        "name": "app/loja-test-web",
        "tool": "nodejs",
        "state": "running",
        "pid": 77,
        "strategy": null,
        "exit_code": null,
        "error": null
    });
    let mut payload: ServicePayload = from(payload_1_0);
    assert_eq!(payload.site_id, None);
    let site = SiteId::new();
    payload.site_id = Some(site);
    let json = round_trip(&payload);
    // The event filter of the site matches it.
    let filter = EventFilter {
        site_id: Some(site),
        ..EventFilter::default()
    };
    assert!(filter.matches("service.running", &json));
}
