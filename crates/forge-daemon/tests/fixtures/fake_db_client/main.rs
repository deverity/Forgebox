//! Fake database client of the databases tests (M2-E3-T5), compiled with `rustc` (no
//! dependencies) and copied as `mysql.exe` / `psql.exe` (the name picks the family) into
//! the `bin/` folder of a version. It never talks to a server:
//!
//! * every run appends one line to `argv.log` next to the executable: the arguments as a
//!   Rust `{:?}` of `Vec<String>`;
//! * `databases.txt` (next to the executable, one name per line) is the "server": a
//!   `SELECT` prints the system databases of the family followed by it, a
//!   `CREATE DATABASE` adds a name (`exit 1` and the real client's message when it exists)
//!   and a `DROP DATABASE` removes one (`exit 1` when it does not exist);
//! * `--execute=<sql>` (mysql) or `-c <sql>` (psql) carry the statement; without one the
//!   run is an interactive shell: it prints `fake-shell <args>` and exits with the number
//!   in `shell-exit.txt` (0 when absent).
//! * copied as `mysqldump.exe` / `pg_dump.exe` it is a **dump** (M2-E3-T6): it writes its
//!   pid to `dump.pid` and prints [`dump_bytes`] of deterministic SQL to stdout, sized by
//!   `dump.txt` (`<bytes> [<ms per MiB> [<exit code>]]`, default 1 MiB, no pause, 0); a
//!   non-zero exit prints 30 numbered lines to stderr first;
//! * with `restore.txt` next to it (`[<ms per MiB> [<exit code>]]`), a run without a
//!   statement is a **restore**: it writes its pid to `restore.pid`, copies stdin to
//!   `restored.bin` (pausing after each MiB), then exits with the code; a non-zero exit
//!   prints 30 numbered lines to stderr.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The dump's SQL: `INSERT` lines whose values come from a xorshift generator, cut at
/// exactly `size` bytes. The tests regenerate it to check what reached the file.
fn dump_bytes(size: usize, mut sink: impl FnMut(&[u8])) {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut done = 0usize;
    let mut row = 0u64;
    let mut line = String::new();
    while done < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        line.clear();
        line.push_str(&format!(
            "INSERT INTO `t` VALUES ({row},'{state:016x}{:016x}');\n",
            state.rotate_left(29)
        ));
        let take = line.len().min(size - done);
        sink(&line.as_bytes()[..take]);
        done += take;
        row += 1;
    }
}

fn numbers(dir: &Path, file: &str) -> Option<Vec<u64>> {
    let text = std::fs::read_to_string(dir.join(file)).ok()?;
    Some(
        text.split_whitespace()
            .filter_map(|n| n.parse().ok())
            .collect(),
    )
}

fn fail_lines(what: &str, code: i32) -> ! {
    for i in 1..=30 {
        eprintln!("{what} error line {i}");
    }
    std::process::exit(code);
}

fn dump(dir: &Path) -> ! {
    std::fs::write(dir.join("dump.pid"), std::process::id().to_string()).expect("dump.pid");
    let config = numbers(dir, "dump.txt").unwrap_or_default();
    let size = config.first().copied().unwrap_or(1 << 20) as usize;
    let pause = config.get(1).copied().unwrap_or(0);
    let code = config.get(2).copied().unwrap_or(0) as i32;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, stdout.lock());
    let mut since_pause = 0usize;
    dump_bytes(size, |bytes| {
        if out.write_all(bytes).is_err() {
            std::process::exit(3);
        }
        since_pause += bytes.len();
        if pause > 0 && since_pause >= 1 << 20 {
            since_pause = 0;
            let _ = out.flush();
            std::thread::sleep(std::time::Duration::from_millis(pause));
        }
    });
    let _ = out.flush();
    if code != 0 {
        fail_lines("dump", code);
    }
    std::process::exit(0);
}

fn restore(dir: &Path, config: &[u64]) -> ! {
    std::fs::write(dir.join("restore.pid"), std::process::id().to_string())
        .expect("restore.pid");
    let pause = config.first().copied().unwrap_or(0);
    let code = config.get(1).copied().unwrap_or(0) as i32;
    let mut file = std::fs::File::create(dir.join("restored.bin")).expect("restored.bin");
    let mut stdin = std::io::stdin().lock();
    let mut buf = vec![0u8; 64 * 1024];
    let mut since_pause = 0usize;
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        file.write_all(&buf[..n]).expect("writing restored.bin");
        since_pause += n;
        if pause > 0 && since_pause >= 1 << 20 {
            since_pause = 0;
            std::thread::sleep(std::time::Duration::from_millis(pause));
        }
    }
    drop(file);
    if code != 0 {
        fail_lines("restore", code);
    }
    std::process::exit(0);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = std::env::current_exe().expect("current exe");
    let dir: PathBuf = exe.parent().expect("parent").to_path_buf();
    let family = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let postgres = family == "psql";
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("argv.log"))
        .expect("argv.log");
    writeln!(log, "{args:?}").expect("writing argv.log");
    drop(log);
    if family == "mysqldump" || family == "pg_dump" {
        dump(&dir);
    }

    let sql = args
        .iter()
        .find_map(|a| a.strip_prefix("--execute=").map(str::to_owned))
        .or_else(|| {
            args.iter()
                .position(|a| a == "-c")
                .and_then(|i| args.get(i + 1).cloned())
        });
    let Some(sql) = sql else {
        if let Some(config) = numbers(&dir, "restore.txt") {
            restore(&dir, &config);
        }
        println!("fake-shell {}", args.join(" "));
        let code = std::fs::read_to_string(dir.join("shell-exit.txt"))
            .ok()
            .and_then(|t| t.trim().parse::<i32>().ok())
            .unwrap_or(0);
        std::process::exit(code);
    };

    let store = dir.join("databases.txt");
    let mut names: Vec<String> = std::fs::read_to_string(&store)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .filter(|l| !l.is_empty())
        .collect();
    let eol = if postgres { "\r\n" } else { "\n" };
    if sql.starts_with("SELECT") {
        let system: &[&str] = if postgres {
            &["postgres", "template0", "template1"]
        } else {
            &["information_schema", "mysql", "performance_schema", "sys"]
        };
        let mut out = String::new();
        for name in system.iter().map(|s| (*s).to_owned()).chain(names) {
            // A second column: only the first one is the name.
            out.push_str(&format!("{name}\tx{eol}"));
        }
        print!("{out}");
        return;
    }
    // The quoted identifier of the statement: `name` (MySQL) or "name" (PostgreSQL).
    let quote = if postgres { '"' } else { '`' };
    let name = sql
        .split(quote)
        .nth(1)
        .map(str::to_owned)
        .expect("quoted database name");
    if sql.starts_with("CREATE DATABASE") {
        if names.contains(&name) {
            eprintln!("ERROR 1007 (HY000): Can't create database '{name}'; database exists");
            std::process::exit(1);
        }
        names.push(name);
    } else if sql.starts_with("DROP DATABASE") {
        if !names.contains(&name) {
            eprintln!("ERROR 1008 (HY000): Can't drop database '{name}'; database doesn't exist");
            std::process::exit(1);
        }
        names.retain(|n| *n != name);
    } else {
        eprintln!("unexpected statement: {sql}");
        std::process::exit(2);
    }
    std::fs::write(&store, names.join("\n")).expect("writing databases.txt");
}
