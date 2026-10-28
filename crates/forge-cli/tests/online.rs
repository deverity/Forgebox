//! The CLI against the official mailpit release (network: GitHub API + a 10 MB download):
//! `forge tool install` through the whole pipeline with its job, then the service it
//! creates: `start`, `status` (human table with a running process), `logs`,
//! `service ports|config`, `stop`, `tool remove` (refused for the current version).
//!
//! `cargo test -p forge-cli --test online -- --ignored --nocapture`

mod common;

use std::time::{Duration, Instant};

use common::*;

#[test]
#[ignore = "downloads mailpit 1.32.0 from GitHub"]
fn installs_and_runs_mailpit_from_the_cli_online() {
    let root = TestRoot::new(true);
    let _daemon = start_daemon(&root);

    // install: a job, followed to its end; JSON result is the DTO of the job.
    let started = Instant::now();
    let install = root.run(&["--json", "tool", "install", "mailpit@1.32.0", "--yes"]);
    install.assert_success();
    println!("installed in {:?}", started.elapsed());
    let result = install.json();
    assert_eq!(result["installed"][0]["version"], "1.32.0", "{result}");
    assert!(root.path.join("bin/mailpit/1.32.0/mailpit.exe").is_file());

    let human = root.run(&["tool", "list", "--installed"]);
    human.assert_success();
    assert!(
        human.stdout.contains("mailpit") && human.stdout.contains("1.32.0"),
        "{human:?}"
    );
    let which = root.json(&["which", "mailpit"]);
    assert_eq!(which["tool"], "mailpit");

    // start: the default instance comes up.
    let start = root.run(&["start", "mailpit"]);
    start.assert_success();
    println!("{}", start.stdout);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        let status = root.json(&["status"]);
        let running = status["instances"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["tool"] == "mailpit" && i["state"] == "running");
        if running {
            break status;
        }
        assert!(Instant::now() < deadline, "mailpit did not run: {status}");
        std::thread::sleep(Duration::from_millis(200));
    };
    let instance = &status["instances"][0];
    assert_eq!(instance["name"], "default");
    assert!(instance["pid"].is_u64());

    // Human table: a row for mailpit/default, running, with its ports.
    let table = root.run(&["status"]);
    table.assert_success();
    println!("{}", table.stdout);
    let row = table
        .stdout
        .lines()
        .find(|l| l.starts_with("mailpit/default"))
        .unwrap_or_else(|| panic!("no mailpit row in {table:?}"));
    assert!(row.contains("rodando"), "{row}");
    assert!(row.contains(&instance["pid"].to_string()), "{row}");

    let ports = root.run(&["service", "ports"]);
    ports.assert_success();
    assert!(
        ports.stdout.contains("mailpit") || ports.stdout.contains("default"),
        "{ports:?}"
    );
    let config = root.run(&["--json", "service", "config", "mailpit"]);
    config.assert_success();
    let logs = root.run(&["logs", "mailpit", "-n", "20"]);
    println!("logs: exit {} -> {} bytes", logs.code, logs.stdout.len());
    assert!(logs.code == 0 || logs.code == 3, "{logs:?}");

    // stop: nothing is left running.
    let stop = root.run(&["stop", "mailpit"]);
    stop.assert_success();
    let status = root.json(&["status"]);
    let still = status["instances"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["state"] == "running");
    assert!(!still, "{status}");

    // remove: asks for --yes, and the version `current` points at is a conflict (exit 4).
    let refused = root.run(&["tool", "remove", "mailpit@1.32.0"]);
    assert_eq!(refused.code, 2);
    let remove = root.run(&["tool", "remove", "mailpit@1.32.0", "--yes"]);
    assert_eq!(remove.code, 4, "{remove:?}");
    assert!(remove.stderr.contains("current"), "{remove:?}");
    assert!(root.path.join("bin/mailpit/1.32.0").exists());
}
