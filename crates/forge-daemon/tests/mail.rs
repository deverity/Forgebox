//! Mail backend (M2-E3-T7, docs/09 §mail, docs/04 §1.9) through the real `forgeboxd` with
//! the fake Mailpit (`tests/fixtures/fake_mailpit.rs`, a minimal Mailpit API v1 over an
//! in-memory mailbox with `POST /fake/inject` and `POST /fake/hang`).
//!
//! * every method works against the instance the manifest's `spec.mail` selects;
//! * two injected messages produce one `mail.changed` with `new_messages = 2` within 5 s
//!   and an idle mailbox produces none in 10 s;
//! * a hanging Mailpit answers a timeout in at most 2.5 s while `daemon.status` stays
//!   under 50 ms;
//! * without a tool declaring `spec.mail` every method is `not_found` with
//!   `forge tool install mailpit`.
//!
//! `mail.open` starts the user's browser and is not called here; its stopped-instance
//! conflict is.
#![cfg(windows)]

mod common;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use forge_ipc::api::methods::{
    daemon_status, events_subscribe, mail_attachment, mail_clear, mail_delete, mail_env, mail_list,
    mail_mark_read, mail_open, mail_show, mail_status, services_status, services_stop,
};
use forge_ipc::api::{
    EventFilter, EventsSubscribeParams, MailAttachmentParams, MailClearParams, MailDeleteParams,
    MailListParams, MailMarkReadParams, MailShowParams, NoParams, ServiceState, ServiceTarget,
    ServiceTargetsParams,
};
use forge_ipc::client::SubscriptionItem;
use forge_ipc::{ApiError, Client, ClientError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use common::*;

fn fake_mailpit() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| compile_fixture("fake_mailpit")).clone()
}

/// A root with the fake Mailpit installed (`polite`: quick stop).
fn root_with_mailpit() -> TestRoot {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    let toml = format!("{toml}mail = [{}, {}]\n", free_port(), free_port());
    std::fs::write(root.root.forgebox_toml(), toml).unwrap();
    let dir = root.root.bin().join("mailpit").join("1.32.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fake_mailpit(), dir.join("mailpit.exe")).unwrap();
    std::fs::write(dir.join("fake-mode.txt"), "polite").unwrap();
    root
}

fn api_error(err: ClientError) -> ApiError {
    match err {
        ClientError::Api(api) => api,
        other => panic!("not an API error: {other:?}"),
    }
}

async fn wait_state(client: &Client, wanted: ServiceState) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = client
            .call::<services_status>(NoParams {})
            .await
            .unwrap()
            .instances
            .into_iter()
            .find(|i| i.tool.as_str() == "mailpit")
            .map(|i| i.state);
        if state == Some(wanted) {
            return;
        }
        assert!(Instant::now() < deadline, "mailpit is {state:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `POST <path>` to the fake's HTTP port.
async fn post(port: u16, path: &str) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer).await;
    assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 200"));
}

async fn inject(port: u16, query: &str) {
    post(port, &format!("/fake/inject?{query}")).await;
}

struct Started {
    root: TestRoot,
    /// Kept alive: dropping it kills the daemon.
    _daemon: DaemonProcess,
    client: Client,
    http_port: u16,
}

async fn start() -> Started {
    let root = root_with_mailpit();
    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    wait_state(&client, ServiceState::Running).await;
    let status = client.call::<mail_status>(NoParams {}).await.unwrap();
    let ui = status.ui_url.expect("ui_url while running");
    let http_port = ui.rsplit(':').next().unwrap().parse().unwrap();
    Started {
        root,
        _daemon: daemon,
        client,
        http_port,
    }
}

#[tokio::test]
async fn every_method_works_against_the_instance() {
    let t = start().await;
    let c = &t.client;

    let status = c.call::<mail_status>(NoParams {}).await.unwrap();
    assert!(status.running);
    assert_eq!((status.messages, status.unread), (0, 0));
    assert_eq!(status.smtp_port.map(|p| p > 0), Some(true));
    assert_eq!(
        status.ui_url.as_deref(),
        Some(format!("http://127.0.0.1:{}", t.http_port).as_str())
    );

    inject(t.http_port, "count=5&subject=Pedido").await;
    inject(t.http_port, "count=1&subject=Fatura&attachment=1000").await;
    let status = c.call::<mail_status>(NoParams {}).await.unwrap();
    assert_eq!((status.messages, status.unread), (6, 6));

    // list: newest first, paging, query, unread.
    let page = c
        .call::<mail_list>(MailListParams {
            limit: Some(4),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!((page.total, page.unread, page.messages.len()), (6, 6, 4));
    assert_eq!(page.messages[0].subject, "Fatura 6");
    assert_eq!(page.messages[0].attachments, 1);
    assert_eq!(page.messages[0].from, "Loja <loja@example.test>");
    let rest = c
        .call::<mail_list>(MailListParams {
            limit: Some(4),
            offset: Some(4),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rest.messages.len(), 2);
    assert_eq!(rest.messages[1].subject, "Pedido 1");
    let found = c
        .call::<mail_list>(MailListParams {
            query: Some("fatura".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!((found.total, found.messages.len()), (1, 1));
    assert_eq!(found.unread, 6, "the badge is the whole mailbox");

    // show marks read; mark_read=false keeps it unread.
    let id_a = page.messages[1].id.clone();
    let shown = c
        .call::<mail_show>(MailShowParams {
            id: id_a.clone(),
            mark_read: true,
        })
        .await
        .unwrap();
    assert!(shown.summary.read);
    assert_eq!(
        shown.text.as_deref(),
        Some(format!("body of {id_a}").as_str())
    );
    assert_eq!(shown.headers["Subject"].len(), 1);
    assert_eq!(c.call::<mail_status>(NoParams {}).await.unwrap().unread, 5);
    let id_b = page.messages[2].id.clone();
    let kept = c
        .call::<mail_show>(MailShowParams {
            id: id_b.clone(),
            mark_read: false,
        })
        .await
        .unwrap();
    assert!(!kept.summary.read);
    assert_eq!(c.call::<mail_status>(NoParams {}).await.unwrap().unread, 5);

    // mark_read / unread.
    c.call::<mail_mark_read>(MailMarkReadParams {
        ids: vec![id_a.clone(), id_b.clone()],
        read: true,
    })
    .await
    .unwrap();
    assert_eq!(c.call::<mail_status>(NoParams {}).await.unwrap().unread, 4);
    c.call::<mail_mark_read>(MailMarkReadParams {
        ids: vec![id_a.clone()],
        read: false,
    })
    .await
    .unwrap();
    assert_eq!(c.call::<mail_status>(NoParams {}).await.unwrap().unread, 5);

    // attachments: ok under the cap, invalid_input over it (never buffered).
    let fatura = page.messages[0].id.clone();
    let part = c
        .call::<mail_attachment>(MailAttachmentParams {
            id: fatura.clone(),
            part_id: "1".into(),
        })
        .await
        .unwrap();
    assert_eq!((part.size_bytes, part.file_name.as_str()), (1000, "a.bin"));
    inject(t.http_port, "count=1&subject=Grande&attachment=4194305").await;
    let big = c
        .call::<mail_list>(MailListParams {
            limit: Some(1),
            ..Default::default()
        })
        .await
        .unwrap()
        .messages[0]
        .id
        .clone();
    let err = api_error(
        c.call::<mail_attachment>(MailAttachmentParams {
            id: big,
            part_id: "1".into(),
        })
        .await
        .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input");
    assert!(err.message.contains("Mailpit UI"), "{}", err.message);

    // env: the manifest's variables with the instance's ports.
    let env = c.call::<mail_env>(NoParams {}).await.unwrap().vars;
    assert_eq!(env["MAIL_MAILER"], "smtp");
    assert_eq!(
        env["MAIL_PORT"],
        status.smtp_port.unwrap().to_string(),
        "{env:?}"
    );

    // delete / clear need confirmation.
    let err = api_error(
        c.call::<mail_delete>(MailDeleteParams {
            ids: vec![id_a.clone()],
            confirm: false,
        })
        .await
        .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input");
    c.call::<mail_delete>(MailDeleteParams {
        ids: vec![id_a],
        confirm: true,
    })
    .await
    .unwrap();
    assert_eq!(
        c.call::<mail_status>(NoParams {}).await.unwrap().messages,
        6
    );
    let err = api_error(
        c.call::<mail_clear>(MailClearParams { confirm: false })
            .await
            .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input");
    c.call::<mail_clear>(MailClearParams { confirm: true })
        .await
        .unwrap();
    let status = c.call::<mail_status>(NoParams {}).await.unwrap();
    assert_eq!((status.messages, status.unread), (0, 0));

    // A bad id never reaches the backend.
    let err = api_error(
        c.call::<mail_show>(MailShowParams {
            id: "../x".into(),
            mark_read: true,
        })
        .await
        .unwrap_err(),
    );
    assert_eq!(err.code, "invalid_input");

    // Stopped instance: status says so, the other methods are a conflict with the hint.
    c.call::<services_stop>(ServiceTargetsParams {
        targets: vec![ServiceTarget::from("mailpit/default")],
    })
    .await
    .unwrap();
    wait_state(c, ServiceState::Stopped).await;
    let status = c.call::<mail_status>(NoParams {}).await.unwrap();
    assert!(!status.running);
    for err in [
        api_error(c.call::<mail_open>(NoParams {}).await.unwrap_err()),
        api_error(
            c.call::<mail_list>(MailListParams::default())
                .await
                .unwrap_err(),
        ),
    ] {
        assert_eq!(err.code, "conflict");
        assert!(err.message.contains("forge start mailpit/default"));
    }
    show_log(&t.root);
}

#[tokio::test]
async fn two_injected_messages_make_one_event_and_idle_makes_none() {
    let t = start().await;
    let mut events = t
        .client
        .subscribe::<events_subscribe>(EventsSubscribeParams {
            filter: EventFilter::kinds(["mail.changed"]),
            resume_from: None,
        })
        .await
        .unwrap();
    // `start` already called mail.status: the baseline is set.
    inject(t.http_port, "count=2").await;
    let injected = Instant::now();
    let item = tokio::time::timeout(Duration::from_secs(5), events.next_item())
        .await
        .expect("mail.changed within 5 s")
        .expect("open subscription");
    let SubscriptionItem::Item(envelope) = item else {
        panic!("{item:?}");
    };
    assert_eq!(envelope.event.kind, "mail.changed");
    let p = &envelope.event.payload;
    assert_eq!(
        (
            p["messages"].as_u64(),
            p["unread"].as_u64(),
            p["new_messages"].as_u64()
        ),
        (Some(2), Some(2), Some(2)),
        "{p}"
    );
    println!("mail.changed after {:?}", injected.elapsed());
    // Idle: nothing for 10 s.
    let idle = tokio::time::timeout(Duration::from_secs(10), events.next_item()).await;
    assert!(idle.is_err(), "unexpected event while idle: {idle:?}");
}

#[tokio::test]
async fn a_hanging_mailpit_times_out_fast_and_the_daemon_stays_responsive() {
    let t = start().await;
    post(t.http_port, "/fake/hang?on=1").await;
    let c = t.client.clone();
    let slow = tokio::spawn(async move {
        let started = Instant::now();
        let err = c.call::<mail_list>(MailListParams::default()).await;
        (started.elapsed(), err)
    });
    let mut worst = Duration::ZERO;
    while !slow.is_finished() {
        let started = Instant::now();
        t.client.call::<daemon_status>(NoParams {}).await.unwrap();
        worst = worst.max(started.elapsed());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (took, result) = slow.await.unwrap();
    let err = api_error(result.unwrap_err());
    assert_eq!(err.code, "timeout", "{err:?}");
    assert!(took <= Duration::from_millis(2500), "{took:?}");
    assert!(worst < Duration::from_millis(50), "daemon.status {worst:?}");
    println!("timeout after {took:?}, daemon.status worst {worst:?}");
}

#[tokio::test]
async fn without_a_mail_tool_every_method_is_not_found_with_the_install_hint() {
    let root = TestRoot::new();
    let mut daemon = spawn_daemon(&root, &[]);
    let (c, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    let mut errors = vec![
        c.call::<mail_status>(NoParams {}).await.unwrap_err(),
        c.call::<mail_open>(NoParams {}).await.unwrap_err(),
        c.call::<mail_env>(NoParams {}).await.unwrap_err(),
        c.call::<mail_list>(MailListParams::default())
            .await
            .unwrap_err(),
        c.call::<mail_show>(MailShowParams {
            id: "abc".into(),
            mark_read: true,
        })
        .await
        .unwrap_err(),
        c.call::<mail_mark_read>(MailMarkReadParams {
            ids: vec!["abc".into()],
            read: true,
        })
        .await
        .unwrap_err(),
        c.call::<mail_attachment>(MailAttachmentParams {
            id: "abc".into(),
            part_id: "1".into(),
        })
        .await
        .unwrap_err(),
    ];
    errors.push(
        c.call::<mail_delete>(MailDeleteParams {
            ids: vec!["abc".into()],
            confirm: true,
        })
        .await
        .unwrap_err(),
    );
    errors.push(
        c.call::<mail_clear>(MailClearParams { confirm: true })
            .await
            .unwrap_err(),
    );
    for err in errors {
        let err = api_error(err);
        assert_eq!(err.code, "not_found");
        assert!(
            err.message.contains("forge tool install mailpit"),
            "{}",
            err.message
        );
    }
}

/// Sends one message with a text attachment through the SMTP port.
async fn send_smtp(port: u16, subject: &str) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let mut reader = tokio::io::BufReader::new(&mut stream);
    async fn expect(reader: &mut tokio::io::BufReader<&mut tokio::net::TcpStream>, code: &str) {
        use tokio::io::AsyncBufReadExt;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.starts_with(code), "{line:?} (wanted {code})");
            if line.as_bytes().get(3) != Some(&b'-') {
                return;
            }
        }
    }
    expect(&mut reader, "220").await;
    for (line, code) in [
        ("EHLO test\r\n", "250"),
        ("MAIL FROM:<loja@example.test>\r\n", "250"),
        ("RCPT TO:<ana@example.test>\r\n", "250"),
        ("DATA\r\n", "354"),
    ] {
        reader.get_mut().write_all(line.as_bytes()).await.unwrap();
        expect(&mut reader, code).await;
    }
    let body = [
        "From: Loja <loja@example.test>",
        "To: ana@example.test",
        &format!("Subject: {subject}"),
        "MIME-Version: 1.0",
        "Content-Type: multipart/mixed; boundary=XX",
        "",
        "--XX",
        "Content-Type: text/plain",
        "",
        "ola mundo",
        "--XX",
        "Content-Type: text/plain; name=\"nota.txt\"",
        "Content-Disposition: attachment; filename=\"nota.txt\"",
        "Content-Transfer-Encoding: base64",
        "",
        "aGVsbG8=",
        "--XX--",
        ".",
        "",
    ]
    .join("\r\n");
    reader.get_mut().write_all(body.as_bytes()).await.unwrap();
    expect(&mut reader, "250").await;
    reader.get_mut().write_all(b"QUIT\r\n").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs mailpit 1.32.0 (downloaded into the shared cache once)"]
async fn the_real_mailpit_1_32_0_answers_every_method_online() {
    let root = TestRoot::new();
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    let toml = format!("{toml}mail = [{}, {}]\n", free_port(), free_port());
    std::fs::write(root.root.forgebox_toml(), toml).unwrap();
    online::copy_tree(
        &online::extracted(&online::MAILPIT_1_32),
        &root.root.bin().join("mailpit").join("1.32.0"),
    );
    let mut daemon = spawn_daemon(&root, &[]);
    let (c, _) = connect(&root, &mut daemon, Duration::from_secs(30)).await;
    wait_state(&c, ServiceState::Running).await;
    let status = c.call::<mail_status>(NoParams {}).await.unwrap();
    assert_eq!((status.messages, status.unread), (0, 0));
    let mut events = c
        .subscribe::<events_subscribe>(EventsSubscribeParams {
            filter: EventFilter::kinds(["mail.changed"]),
            resume_from: None,
        })
        .await
        .unwrap();

    send_smtp(status.smtp_port.unwrap(), "Pedido 42").await;
    let item = tokio::time::timeout(Duration::from_secs(5), events.next_item())
        .await
        .expect("mail.changed within 5 s")
        .expect("open subscription");
    let SubscriptionItem::Item(envelope) = item else {
        panic!("{item:?}");
    };
    assert_eq!(envelope.event.payload["new_messages"], 1);

    let list = c
        .call::<mail_list>(MailListParams {
            query: Some("pedido".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!((list.total, list.unread), (1, 1));
    let summary = &list.messages[0];
    assert_eq!(summary.subject, "Pedido 42");
    assert_eq!(summary.from, "Loja <loja@example.test>");
    assert_eq!(summary.attachments, 1);

    let kept = c
        .call::<mail_show>(MailShowParams {
            id: summary.id.clone(),
            mark_read: false,
        })
        .await
        .unwrap();
    assert!(!kept.summary.read);
    assert_eq!(kept.text.as_deref().map(str::trim), Some("ola mundo"));
    assert_eq!(c.call::<mail_status>(NoParams {}).await.unwrap().unread, 1);
    let part = &kept.attachments[0];
    let content = c
        .call::<mail_attachment>(MailAttachmentParams {
            id: summary.id.clone(),
            part_id: part.part_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(content.size_bytes, 5);
    assert_eq!(content.content_base64, "aGVsbG8=");
    c.call::<mail_show>(MailShowParams {
        id: summary.id.clone(),
        mark_read: true,
    })
    .await
    .unwrap();
    assert_eq!(c.call::<mail_status>(NoParams {}).await.unwrap().unread, 0);
    assert_eq!(
        c.call::<mail_env>(NoParams {}).await.unwrap().vars["MAIL_PORT"],
        status.smtp_port.unwrap().to_string()
    );
    c.call::<mail_clear>(MailClearParams { confirm: true })
        .await
        .unwrap();
    assert_eq!(
        c.call::<mail_status>(NoParams {}).await.unwrap().messages,
        0
    );
    let (exit, _) = stop(&c, &mut daemon, Duration::from_secs(60)).await;
    assert!(exit.success());
}
