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

use std::io::Write;
use std::path::PathBuf;

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

    let sql = args
        .iter()
        .find_map(|a| a.strip_prefix("--execute=").map(str::to_owned))
        .or_else(|| {
            args.iter()
                .position(|a| a == "-c")
                .and_then(|i| args.get(i + 1).cloned())
        });
    let Some(sql) = sql else {
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
