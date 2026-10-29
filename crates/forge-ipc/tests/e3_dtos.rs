//! The E3 contract (ADR-0040): every new DTO survives a serde round trip with the wire
//! shape clients rely on, and every extended DTO still reads the JSON an API 1.0 peer sends
//! (missing fields take their defaults; absent optional output stays absent).

use std::collections::BTreeMap;
use std::fmt::Debug;

use pretty_assertions::assert_eq;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use forge_core::Timestamp;
use forge_core::event::EventKind;
use forge_ipc::api::*;

fn ts() -> Timestamp {
    "2026-10-28T12:00:00Z".parse().unwrap()
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

// --- db -------------------------------------------------------------------------------

fn db_instance() -> DbInstanceDto {
    DbInstanceDto {
        instance_id: InstanceId::new(),
        instance: "postgresql/default".into(),
        tool: tool("postgresql"),
        version: "17.6".into(),
        protocol: "postgresql".into(),
        state: ServiceState::Stopped,
        port: Some(5432),
        data_size_bytes: Some(48 << 20),
        capabilities: DbCapabilitiesDto {
            create: true,
            drop: true,
            export: true,
            import: true,
            shell: true,
            url: true,
            open_with: vec![tool("dbeaver")],
        },
    }
}

#[test]
fn db_dtos_round_trip() {
    let instance = db_instance();
    let json = round_trip(&instance);
    assert_eq!(json["protocol"], "postgresql");
    assert_eq!(json["state"], "stopped");
    assert_eq!(json["capabilities"]["open_with"], json!(["dbeaver"]));
    round_trip(&DbCapabilitiesDto::default());
    let list = DatabaseListDto {
        databases: vec![],
        instances: vec![instance],
    };
    round_trip(&list);
    let command = ShellCommandDto {
        program: "C:/Forgebox/bin/mysql/8.4.13/bin/mysql.exe".into(),
        args: vec![
            "--host=127.0.0.1".into(),
            "--port=3306".into(),
            "loja".into(),
        ],
        cwd: None,
        env: BTreeMap::from([("MYSQL_PWD".into(), "secret".into())]),
    };
    let json = round_trip(&command);
    assert_eq!(json["env"]["MYSQL_PWD"], "secret");
}

#[test]
fn db_dtos_read_api_1_0_json() {
    // 1.0 daemons sent no instances.
    let list: DatabaseListDto = from(json!({"databases": []}));
    assert!(list.instances.is_empty());
    // A minimal instance (capabilities and size missing).
    let instance: DbInstanceDto = from(json!({
        "instance_id": InstanceId::new(),
        "instance": "mysql/default",
        "tool": "mysql",
        "version": "8.4.13",
        "protocol": "mysql",
        "state": "running",
        "port": null
    }));
    assert_eq!(instance.capabilities, DbCapabilitiesDto::default());
    assert_eq!(instance.data_size_bytes, None);
    let command: ShellCommandDto = from(json!({"program": "psql.exe"}));
    assert!(command.args.is_empty() && command.env.is_empty() && command.cwd.is_none());
}

// --- mail -----------------------------------------------------------------------------

fn summary() -> MailSummaryDto {
    MailSummaryDto {
        id: "Ab3x".into(),
        from: "Loja <noreply@loja.test>".into(),
        to: vec!["cliente@example.test".into()],
        subject: "Pedido 42".into(),
        received_at: ts(),
        read: false,
        size_bytes: 2048,
        attachments: 1,
    }
}

#[test]
fn mail_dtos_round_trip() {
    let params = MailListParams {
        unread: true,
        limit: Some(20),
        query: Some("subject:pedido".into()),
        offset: Some(40),
    };
    let json = round_trip(&params);
    assert_eq!(json["query"], "subject:pedido");
    round_trip(&MailListDto {
        messages: vec![summary()],
        total: 61,
        unread: 3,
    });
    let message = MailMessageDto {
        summary: summary(),
        headers: BTreeMap::from([("X-Mailer".into(), vec!["Laravel".into()])]),
        text: Some("Olá".into()),
        html: Some("<p>Olá</p>".into()),
        cc: vec!["copia@example.test".into()],
        bcc: vec![],
        reply_to: vec!["vendas@loja.test".into()],
        attachments: vec![MailAttachmentDto {
            part_id: "2".into(),
            file_name: "nota.pdf".into(),
            content_type: "application/pdf".into(),
            size_bytes: 12_000,
            inline: false,
            content_id: None,
        }],
    };
    let json = round_trip(&message);
    assert_eq!(json["attachments"][0]["part_id"], "2");
    round_trip(&MailShowParams {
        id: "Ab3x".into(),
        mark_read: false,
    });
    round_trip(&MailMarkReadParams {
        ids: vec!["a".into(), "b".into()],
        read: false,
    });
    round_trip(&MailDeleteParams {
        ids: vec!["a".into()],
        confirm: true,
    });
    round_trip(&MailAttachmentParams {
        id: "Ab3x".into(),
        part_id: "2".into(),
    });
    round_trip(&MailAttachmentContentDto {
        file_name: "nota.pdf".into(),
        content_type: "application/pdf".into(),
        size_bytes: 3,
        content_base64: "JVBE".into(),
    });
    assert_eq!(MAIL_ATTACHMENT_MAX_BYTES, 4 * 1024 * 1024);
}

#[test]
fn mail_dtos_read_api_1_0_json() {
    let params: MailListParams = from(json!({"unread": true, "limit": 5}));
    assert_eq!((params.query, params.offset), (None, None));
    let list: MailListDto = from(json!({"messages": []}));
    assert_eq!((list.total, list.unread), (0, 0));
    // Showing always marked the message read: that stays the default.
    let show: MailShowParams = from(json!({"id": "x"}));
    assert!(show.mark_read);
    let message: MailMessageDto = from(json!({
        "summary": serde_json::to_value(summary()).unwrap(),
        "headers": {},
        "text": "hi",
        "html": null
    }));
    assert!(message.cc.is_empty() && message.attachments.is_empty());
    let read: MailMarkReadParams = from(json!({"ids": ["a"]}));
    assert!(read.read);
    let delete: MailDeleteParams = from(json!({"ids": ["a"]}));
    assert!(!delete.confirm, "confirm is never implied");
}

// --- logs -----------------------------------------------------------------------------

#[test]
fn log_dtos_round_trip() {
    let line = LogLineDto {
        seq: 7,
        ts: Some(ts()),
        source: LogSource::File,
        level: Some("warn".into()),
        text: "GET /checkout 404".into(),
        access: Some(AccessLineDto {
            host: "loja.test".into(),
            status: 404,
            method: "GET".into(),
            uri: "/checkout?step=2".into(),
            rt_ms: Some(1.25),
            upstream: Some("127.0.0.1:19083".into()),
        }),
    };
    let json = round_trip(&line);
    assert_eq!(json["access"]["rt_ms"], 1.25);
    round_trip(&LogsReadParams {
        target: "site:loja.test".into(),
        since_secs: None,
        grep: None,
        limit: Some(200),
        before_seq: Some(1_000),
    });
    let target = LogTargetDto {
        target: "nginx.error".into(),
        label: "nginx · error".into(),
        kind: LogTargetKind::InstanceFile,
        group: "nginx".into(),
        format: Some("nginx-error".into()),
        live: true,
        file: Some("logs/nginx/error.log".into()),
    };
    let json = round_trip(&target);
    assert_eq!(json["kind"], "instance_file");
    round_trip(&LogTargetListDto {
        targets: vec![target],
    });
    for (kind, wire) in [
        (LogTargetKind::Daemon, "daemon"),
        (LogTargetKind::Instance, "instance"),
        (LogTargetKind::Pool, "pool"),
        (LogTargetKind::Site, "site"),
        (LogTargetKind::App, "app"),
    ] {
        assert_eq!(round_trip(&kind), wire);
    }
}

#[test]
fn log_dtos_read_and_write_api_1_0_json() {
    let old = json!({"seq": 3, "ts": null, "source": "stdout", "level": null, "text": "ok"});
    let line: LogLineDto = from(old.clone());
    assert_eq!(line.access, None);
    // A line without access fields serializes exactly as in 1.0 (no `access: null`).
    assert_eq!(serde_json::to_value(&line).unwrap(), old);
    let read: LogsReadParams = from(json!({"target": "daemon", "limit": 10}));
    assert_eq!(read.before_seq, None);
    let target: LogTargetDto = from(json!({
        "target": "daemon", "label": "forgeboxd", "kind": "daemon", "group": "daemon"
    }));
    assert!(!target.live && target.format.is_none() && target.file.is_none());
}

// --- terminal -------------------------------------------------------------------------

fn terminal() -> TerminalDto {
    TerminalDto {
        id: TerminalId::new(),
        title: "mysql/default".into(),
        shell: "C:/Forgebox/bin/mysql/8.4.13/bin/mysql.exe".into(),
        cwd: "C:/Forgebox".into(),
        site_id: None,
        cols: 100,
        rows: 40,
        pid: None,
        started_at: ts(),
        exit_code: Some(-1_073_741_510),
        exited_at: Some(ts()),
        last_seq: 1 << 40,
    }
}

#[test]
fn terminal_dtos_round_trip() {
    let open = TerminalOpenParams {
        dir: Some("C:/Forgebox/www/loja".into()),
        site: Some(SiteRef::from("loja.test")),
        versions: BTreeMap::from([("php".into(), "8.2".into())]),
        shell: Some("cmd".into()),
        command: vec!["php".into(), "artisan".into(), "tinker".into()],
        env: BTreeMap::from([("APP_ENV".into(), "local".into())]),
        cols: 200,
        rows: 50,
        title: Some("tinker".into()),
    };
    round_trip(&open);
    let session = terminal();
    let json = round_trip(&session);
    assert!(json["id"].as_str().unwrap().parse::<TerminalId>().is_ok());
    assert_eq!(json["exit_code"], -1_073_741_510);
    round_trip(&TerminalListDto {
        terminals: vec![session.clone()],
    });
    round_trip(&TerminalIdParams { id: session.id });
    round_trip(&TerminalInputParams {
        id: session.id,
        data: "echo hi\r".into(),
        input_seq: 9,
    });
    round_trip(&TerminalResizeParams {
        id: session.id,
        cols: 80,
        rows: 24,
    });
    let mut subscribe = TerminalSubscribeParams {
        id: session.id,
        resume_from: None,
    };
    round_trip(&subscribe);
    subscribe.set_resume_from(Some(4096));
    assert_eq!(Resumable::resume_from(&subscribe), Some(4096));
    let output = TerminalEnvelope {
        subscription_id: SubscriptionId::new(),
        seq: 4,
        item: TerminalItemDto::Output {
            data: "hi\r\n".into(),
        },
    };
    let json = round_trip(&output);
    assert_eq!(json["item"], json!({"kind": "output", "data": "hi\r\n"}));
    let exited = TerminalEnvelope {
        item: TerminalItemDto::Exited { exit_code: None },
        seq: 5,
        ..output
    };
    let json = round_trip(&exited);
    assert_eq!(json["item"], json!({"kind": "exited", "exit_code": null}));
}

#[test]
fn terminal_open_defaults() {
    let open: TerminalOpenParams = from(json!({}));
    assert_eq!(
        (open.cols, open.rows),
        (TERMINAL_DEFAULT_COLS, TERMINAL_DEFAULT_ROWS)
    );
    assert!(open.command.is_empty() && open.site.is_none() && open.shell.is_none());
    let session: TerminalDto = from(json!({
        "id": TerminalId::new(), "title": "pwsh", "shell": "pwsh", "cwd": "C:/Forgebox",
        "cols": 80, "rows": 24, "pid": 1, "started_at": "2026-10-28T12:00:00Z", "last_seq": 0
    }));
    assert_eq!(
        (session.exit_code, session.exited_at, session.site_id),
        (None, None, None)
    );
    // Input without input_seq is refused: dedupe depends on it.
    assert!(
        serde_json::from_value::<TerminalInputParams>(
            json!({"id": TerminalId::new(), "data": "x"})
        )
        .is_err()
    );
}

// --- shell, system, doctor, security extras -------------------------------------------

#[test]
fn shell_system_and_security_dtos_round_trip() {
    round_trip(&ShellOpenEditorParams {
        path: "C:/Forgebox/www/loja/app/Http/Kernel.php".into(),
        line: Some(42),
        column: Some(3),
    });
    let editor: ShellOpenEditorParams = from(json!({"path": "C:/x.php"}));
    assert_eq!((editor.line, editor.column), (None, None));

    let value = ConfigValueDto {
        key: "tld".into(),
        value: json!(".local.test"),
        is_default: false,
        requires: vec!["tls.ca_rotate".into()],
    };
    round_trip(&value);
    let doc = ConfigDocumentDto {
        values: json!({"tld": ".test", "dns": {"enabled": true}}),
        defaults: json!({"tld": ".test", "dns": {"enabled": true}}),
        schema: json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object"}),
        set_keys: vec!["dns.enabled".into()],
    };
    round_trip(&doc);

    let check = CheckDto {
        id: "dns.policy".into(),
        status: CheckStatus::Warn,
        message: "NRPT rule missing".into(),
        fix: Some("forge dns enable".into()),
        fix_method: Some("net.dns_enable".into()),
        fix_params: Some(json!({})),
    };
    let json = round_trip(&check);
    assert_eq!(json["fix_method"], "net.dns_enable");
    assert!(method_info(check.fix_method.as_deref().unwrap()).is_some());

    let json = round_trip(&CaUninstallParams {
        scope: TrustScopeDto::LocalMachine,
    });
    assert_eq!(json["scope"], "local_machine");
    round_trip(&KillPortHolderParams {
        port: 80,
        pid: 4412,
        confirm: true,
    });
    let kill: KillPortHolderParams = from(json!({"port": 80, "pid": 4412}));
    assert!(!kill.confirm, "confirm is never implied");

    round_trip(&SiteEnvParams {
        site: SiteRef::from("loja.test"),
        reveal: true,
    });
    let env: SiteEnvParams = from(json!({"site": "loja.test"}));
    assert!(!env.reveal, "masked unless asked");
    let dotenv = SiteEnvDto {
        site_id: SiteId::new(),
        host: "loja.test".into(),
        files: vec![DotenvFileDto {
            name: ".env".into(),
            path: "C:/Forgebox/www/loja/.env".into(),
            vars: vec![
                DotenvVarDto {
                    key: "APP_ENV".into(),
                    value: Some("local".into()),
                    secret: false,
                    line: 1,
                },
                DotenvVarDto {
                    key: "DB_PASSWORD".into(),
                    value: None,
                    secret: true,
                    line: 9,
                },
            ],
        }],
        revealed: false,
    };
    let json = round_trip(&dotenv);
    assert_eq!(json["files"][0]["vars"][1]["value"], Value::Null);
}

#[test]
fn extended_system_dtos_read_api_1_0_json() {
    let value: ConfigValueDto = from(json!({"key": "tld", "value": ".test", "is_default": true}));
    assert!(value.requires.is_empty());
    let check: CheckDto = from(json!({
        "id": "path.user", "status": "ok", "message": "ok", "fix": null
    }));
    assert_eq!((check.fix_method, check.fix_params), (None, None));
}

// --- events ---------------------------------------------------------------------------

#[test]
fn e3_events_have_payload_types_that_round_trip() {
    for (kind, payload) in [
        ("mail.changed", "MailChangedPayload"),
        ("terminal.exited", "TerminalExitedPayload"),
        ("config.changed", "ConfigChangedPayload"),
        ("tls.ca_untrusted", "TlsCaUntrustedPayload"),
        ("port.holder_killed", "PortHolderKilledPayload"),
    ] {
        assert!(
            EVENT_PAYLOADS.contains(&(kind, payload)),
            "{kind} → {payload} missing from EVENT_PAYLOADS"
        );
        let parsed: EventKind = kind.parse().unwrap();
        assert_eq!(parsed.as_str(), kind);
    }
    let instance_id = InstanceId::new();
    let json = round_trip(&MailChangedPayload {
        v: EVENT_PAYLOAD_VERSION,
        instance_id,
        messages: 5,
        unread: 2,
        new_messages: 2,
    });
    // The event filter finds it by instance.
    let filter = EventFilter {
        instance_id: Some(instance_id),
        ..EventFilter::kinds(["mail.*"])
    };
    assert!(filter.matches("mail.changed", &json));
    let site_id = SiteId::new();
    let json = round_trip(&TerminalExitedPayload {
        v: EVENT_PAYLOAD_VERSION,
        terminal_id: TerminalId::new(),
        exit_code: Some(0),
        site_id: Some(site_id),
    });
    assert!(
        EventFilter {
            site_id: Some(site_id),
            ..EventFilter::default()
        }
        .matches("terminal.exited", &json)
    );
    let changed = round_trip(&ConfigChangedPayload {
        v: EVENT_PAYLOAD_VERSION,
        keys: vec!["tld".into()],
        requires: vec!["tls.ca_rotate".into()],
    });
    let minimal: ConfigChangedPayload = from(json!({"v": 1, "keys": ["dns.enabled"]}));
    assert!(minimal.requires.is_empty());
    assert_eq!(changed["keys"], json!(["tld"]));
    round_trip(&TlsCaUntrustedPayload {
        v: EVENT_PAYLOAD_VERSION,
        scope: TrustScopeDto::CurrentUser,
        thumbprint_sha1: "AB".repeat(20),
    });
    round_trip(&PortHolderKilledPayload {
        v: EVENT_PAYLOAD_VERSION,
        port: 80,
        pid: 4412,
        exe: Some("httpd.exe (C:/xampp/apache/bin)".into()),
        client: Some("Forgebox.exe".into()),
    });
}
