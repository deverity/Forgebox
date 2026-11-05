//! Acceptance of M2-E3-T9 (local launchers): `shell.open`, `shell.open_editor` and
//! `sites.open` editor/terminal.
//!
//! * A real `forgeboxd` with a fake editor and a fake terminal in `[editor]`/`[terminal]`
//!   (`tests/fixtures/fake_launcher.rs`, which records its argv, `PATH` and folder):
//!   `open_editor(path, 42, 3)` hands the editor the path with spaces and the position, a path
//!   outside the root, the parked folders and the sites is `forbidden`, `shell.open` starts the
//!   terminal in the folder with the Forgebox `PATH`, and the terminal is still alive after the
//!   daemon stopped.
//! * A daemon in this process behind a remote transport: both methods are `forbidden`.
#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use forge_core::config::ForgeConfig;
use forge_core::paths::{ForgeRoot, InitOptions};
use forge_core::platform::{self, RecordingShellOpener, ShellOpen};
use forge_daemon::args::Args;
use forge_daemon::boot::{self, BootOptions};
use forge_daemon::instance_lock::InstanceLock;
use forge_ipc::api::methods::{jobs_get, shell_open, shell_open_editor, sites_add, sites_open};
use forge_ipc::api::{
    JobIdParams, JobState, Role, ShellOpenEditorParams, ShellOpenParams, SiteAddParams, SiteDto,
    SiteOpenParams, SiteOpenTarget, SiteRef,
};
use forge_ipc::auth::{Authenticator, IpcToken};
use forge_ipc::transport::{ConnectionInfo, InMemoryTransport, Transport};
use forge_ipc::{Client, Credentials, Server};

use common::*;

/// What the fake launcher recorded.
#[derive(Debug)]
struct Record {
    args: Vec<String>,
    path: String,
    cwd: String,
    pid: u32,
}

fn read_record(out: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(out).ok()?;
    text.contains("END\n").then_some(())?;
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .unwrap_or_default()
            .to_owned()
    };
    Some(Record {
        args: text
            .lines()
            .filter_map(|l| l.strip_prefix("ARG="))
            .map(str::to_owned)
            .collect(),
        path: field("PATH="),
        cwd: field("CWD="),
        pid: field("PID=").parse().unwrap(),
    })
}

/// Waits for the fake launcher to record itself in `out` (removed first by the caller).
async fn wait_record(out: &Path) -> Record {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(record) = read_record(out) {
            return record;
        }
        assert!(Instant::now() < deadline, "no record in {}", out.display());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Canonical path without the `\\?\` prefix: how the daemon spells what it hands over.
fn plain(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned()
}

fn alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
}

/// Ends the fake terminals however the test ends.
struct Kill(Vec<u32>);

impl Drop for Kill {
    fn drop(&mut self) {
        for pid in &self.0 {
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
        }
    }
}

async fn wait_job(client: &Client, id: forge_core::ids::JobId) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let job = client
            .call::<jobs_get>(JobIdParams { job_id: id })
            .await
            .unwrap();
        match job.state {
            JobState::Done => return job.result.unwrap_or_default(),
            JobState::Failed | JobState::Cancelled => {
                panic!("job {id} ended {:?}: {:?}", job.state, job.error)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} still {:?}", job.state);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn add_site(root: &TestRoot, name: &str) -> SiteAddParams {
    let project = root.path().join("www").join(name);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("index.php"), "<?php echo 1;").unwrap();
    SiteAddParams {
        dir: project.display().to_string(),
        host: Some(format!("{name}.test")),
        php: None,
        server: None,
        secure: Some(false),
        driver: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_launchers_start_detached_programs_with_the_forgebox_environment() {
    let root = TestRoot::new();
    let fake = compile_fixture("fake_launcher");
    let out_dir = tempfile::Builder::new().prefix("fbl-").tempdir().unwrap();
    let editor_out = out_dir.path().join("editor.txt");
    let terminal_out = out_dir.path().join("terminal.txt");
    let toml = std::fs::read_to_string(root.root.forgebox_toml()).unwrap();
    std::fs::write(
        root.root.forgebox_toml(),
        format!(
            "{toml}\n[editor]\ncommand = '\"{exe}\" \"{out}\" 0 {{path}} {{line}} {{column}}'\n\n[terminal]\ncommand = '\"{exe}\" \"{tout}\" 120 {{dir}}'\n",
            exe = fake.display(),
            out = editor_out.display(),
            tout = terminal_out.display(),
        ),
    )
    .unwrap();
    let project = root.path().join("www").join("my site");
    std::fs::create_dir_all(&project).unwrap();
    let file = project.join("app.php");
    std::fs::write(&file, "<?php echo 1;").unwrap();

    let mut daemon = spawn_daemon(&root, &[]);
    let (client, _) = connect(&root, &mut daemon, Duration::from_secs(60)).await;
    let mut kill = Kill(Vec::new());

    // open_editor(path, 42, 3): the path with spaces is one argument, then line and column.
    let ack = client
        .call::<shell_open_editor>(ShellOpenEditorParams {
            path: file.display().to_string(),
            line: Some(42),
            column: Some(3),
        })
        .await
        .unwrap();
    assert!(ack.ok);
    let record = wait_record(&editor_out).await;
    assert_eq!(record.args, [plain(&file), "42".into(), "3".into()]);
    assert_eq!(
        record.cwd,
        plain(&project),
        "the editor starts in the file's folder"
    );

    // Without a position the template defaults to 1:1.
    let _ = std::fs::remove_file(&editor_out);
    client
        .call::<shell_open_editor>(ShellOpenEditorParams {
            path: file.display().to_string(),
            line: None,
            column: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_record(&editor_out).await.args,
        [plain(&file), "1".into(), "1".into()]
    );

    // Outside the root, the parked folders and the sites: forbidden, nothing started.
    let outside = tempfile::Builder::new().prefix("fbo-").tempdir().unwrap();
    let stranger = outside.path().join("secret.txt");
    std::fs::write(&stranger, "x").unwrap();
    let _ = std::fs::remove_file(&editor_out);
    let err = client
        .call::<shell_open_editor>(ShellOpenEditorParams {
            path: stranger.display().to_string(),
            line: None,
            column: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "forbidden", "{err}");
    // `..` out of the root is outside after canonicalization.
    let sneaky = file.join("..").join("..").join("..").join("..");
    let err = client
        .call::<shell_open_editor>(ShellOpenEditorParams {
            path: sneaky
                .join(outside.path().file_name().unwrap())
                .display()
                .to_string(),
            line: None,
            column: None,
        })
        .await;
    assert!(err.is_err());
    let missing = root.path().join("www").join("missing.php");
    let err = client
        .call::<shell_open_editor>(ShellOpenEditorParams {
            path: missing.display().to_string(),
            line: None,
            column: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
    let err = client
        .call::<shell_open_editor>(ShellOpenEditorParams {
            path: "relative/file.php".into(),
            line: None,
            column: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_input");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !editor_out.exists(),
        "a refused path never reaches the editor"
    );

    // shell.open: the terminal starts in the folder, with the Forgebox PATH.
    client
        .call::<shell_open>(ShellOpenParams {
            dir: Some(plain(&project)),
        })
        .await
        .unwrap();
    let record = wait_record(&terminal_out).await;
    kill.0.push(record.pid);
    assert_eq!(record.args, [plain(&project)]);
    assert_eq!(record.cwd, plain(&project));
    let shims = plain(&root.root.bin_shims());
    assert!(
        record
            .path
            .split(';')
            .any(|entry| entry.trim_end_matches('\\').eq_ignore_ascii_case(&shims)),
        "bin/shims on the terminal's PATH: {}",
        record.path
    );
    let err = client
        .call::<shell_open>(ShellOpenParams {
            dir: Some(root.path().join("nope").display().to_string()),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");

    // sites.open: the editor gets the project folder, the terminal opens in it.
    let job = client
        .call::<sites_add>(add_site(&root, "loja"))
        .await
        .unwrap();
    let site: SiteDto = serde_json::from_value(wait_job(&client, job.id).await).unwrap();
    let site_dir = root.path().join("www").join("loja");
    let _ = std::fs::remove_file(&editor_out);
    let open = |with| {
        let client = &client;
        let site = site.host.clone();
        async move {
            client
                .call::<sites_open>(SiteOpenParams {
                    site: SiteRef::from(site.as_str()),
                    with,
                })
                .await
        }
    };
    assert!(open(SiteOpenTarget::Editor).await.unwrap().ok);
    let record = wait_record(&editor_out).await;
    assert_eq!(record.args[0], plain(&site_dir));
    std::fs::remove_file(&terminal_out).unwrap();
    assert!(open(SiteOpenTarget::Terminal).await.unwrap().ok);
    let record = wait_record(&terminal_out).await;
    kill.0.push(record.pid);
    assert_eq!(record.args, [plain(&site_dir)]);

    // Detached: both terminals outlive the daemon.
    let pids = kill.0.clone();
    assert!(pids.iter().all(|pid| alive(*pid)), "{pids:?}");
    let (status, _) = stop(&client, &mut daemon, Duration::from_secs(30)).await;
    assert!(status.success());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        pids.iter().all(|pid| alive(*pid)),
        "the terminals are alive after the daemon stopped: {pids:?}"
    );
    drop(kill);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(pids.iter().all(|pid| !alive(*pid)), "cleanup");
}

struct FixedRole(Role);

impl Authenticator for FixedRole {
    fn authenticate(
        &self,
        _: &forge_ipc::api::HelloParams,
        _: &ConnectionInfo,
    ) -> forge_core::error::Result<Role> {
        Ok(self.0)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_clients_cannot_launch_and_sites_open_no_longer_says_not_implemented() {
    let test_root = TestRoot::new();
    let root = ForgeRoot::new(platform::long_path(test_root.path())).unwrap();
    let lock = InstanceLock::acquire(&root).expect("instance lock");
    let config = ForgeConfig::load_or_default(&root.forgebox_toml()).unwrap();
    let report = root
        .init(InitOptions {
            case_sensitive: config.case_sensitive,
        })
        .unwrap();
    let args = Args::parse_from([
        "forgeboxd",
        "--root",
        &root.path().display().to_string(),
        "--registry",
        &repo_registry().display().to_string(),
        "--shutdown-timeout",
        "20",
    ]);
    let opener = RecordingShellOpener::new();
    let options_boot = BootOptions {
        platform: Some(platform::current().with_shell_opener(Arc::new(opener.clone()))),
        ..BootOptions::default()
    };
    let booted = boot::boot_with(args, root.clone(), &lock, config, report, options_boot)
        .await
        .expect("boot");

    let file = root.path().join("etc").join("forgebox.toml");
    let mut clients = Vec::new();
    for (role, remote) in [(Role::Admin, false), (Role::Admin, true)] {
        let transport = Arc::new(if remote {
            InMemoryTransport::remote()
        } else {
            InMemoryTransport::new()
        });
        let server = Server::builder(Arc::new(FixedRole(role)))
            .router(forge_daemon::api::router(&booted.daemon))
            .build();
        let (handle, _task) = server.bind_and_spawn(transport.as_ref()).await.unwrap();
        let client = Client::new(
            transport as Arc<dyn Transport>,
            Credentials::new(IpcToken::generate().unwrap(), "launch-test", "0"),
            options(),
        );
        clients.push((client, handle, remote));
    }
    for (client, _, remote) in &clients {
        let editor = client
            .call::<shell_open_editor>(ShellOpenEditorParams {
                path: file.display().to_string(),
                line: Some(1),
                column: None,
            })
            .await;
        let terminal = client
            .call::<shell_open>(ShellOpenParams { dir: None })
            .await;
        if *remote {
            assert_eq!(editor.unwrap_err().code(), "forbidden");
            assert_eq!(terminal.unwrap_err().code(), "forbidden");
        } else {
            // Local: allowed (the recording opener starts nothing; the machine decides
            // whether an editor and a terminal exist).
            for result in [editor.map(|a| a.ok), terminal.map(|a| a.ok)] {
                if let Err(err) = result {
                    assert_eq!(err.code(), "not_found", "no editor/terminal: {err}");
                }
            }
        }
    }
    assert!(
        opener
            .calls()
            .iter()
            .all(|c| matches!(c, ShellOpen::Launch(_))),
        "{:?}",
        opener.calls()
    );
    let launched = opener.calls().len();
    assert!(launched <= 2, "only the local client launched: {launched}");

    // sites.open editor and terminal answer through the launchers.
    let credentials = Credentials::local(&root, "launch-test", "0").unwrap();
    let local = Client::connect(
        forge_ipc::transport::local_transport(&root),
        credentials,
        options(),
    )
    .await
    .expect("client connects");
    let client = &local;
    let job = client
        .call::<sites_add>(add_site(&test_root, "blog"))
        .await
        .unwrap();
    let site: SiteDto = serde_json::from_value(wait_job(client, job.id).await).unwrap();
    for with in [SiteOpenTarget::Editor, SiteOpenTarget::Terminal] {
        let result = client
            .call::<sites_open>(SiteOpenParams {
                site: SiteRef::from(site.host.as_str()),
                with,
            })
            .await;
        match result {
            Ok(ack) => assert!(ack.ok),
            Err(err) => assert_eq!(err.code(), "not_found", "{with:?}: {err}"),
        }
    }
    let folder = PathBuf::from(plain(&test_root.path().join("www").join("blog")));
    let launches: Vec<_> = opener
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            ShellOpen::Launch(spec) => Some(spec),
            _ => None,
        })
        .collect();
    assert!(
        launches
            .iter()
            .skip(launched)
            .all(|spec| spec.cwd == folder || spec.args.iter().any(|a| a == folder.as_os_str())),
        "{launches:?}"
    );
    // The terminal launch carries the Forgebox PATH.
    if let Some(spec) = launches.last().filter(|s| s.cwd == folder) {
        let env = spec.env.as_ref().expect("complete environment");
        assert!(env.iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH")));
    }

    local.close();
    for (client, handle, _) in clients {
        client.close();
        drop(handle);
    }
    booted
        .daemon
        .shutdown
        .request(forge_daemon::daemon::ShutdownRequest {
            reason: "user".into(),
            restart: false,
            keep_services: false,
        });
    forge_daemon::shutdown::run(booted, lock).await;
}
