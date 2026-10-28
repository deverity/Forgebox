//! Stand-in for php.exe / php-cgi.exe / mysqld.exe, compiled by the import tests with
//! rustc (`tests/import_laragon.rs`) and copied next to a `fake-version` file whose first
//! line is the version to report. Not a cargo target.

fn main() {
    let exe = std::env::current_exe().expect("current exe");
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let version = exe
        .parent()
        .map(|dir| dir.join("fake-version"))
        .and_then(|file| std::fs::read_to_string(file).ok())
        .and_then(|text| text.lines().next().map(str::trim).map(str::to_owned))
        .unwrap_or_else(|| "0.0.0".to_owned());
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = args.first().map(String::as_str).unwrap_or("");
    match (stem.as_str(), flag) {
        ("php" | "php-cgi", "-r") => print!("{version}"),
        ("php" | "php-cgi", "-i") => {
            println!("phpinfo()");
            println!("PHP Version => {version}");
            println!("Thread Safety => disabled");
        }
        ("php" | "php-cgi", "-v") => println!("PHP {version} (cli)"),
        ("mysqld", "--version") => println!(
            "{}  Ver {version} for Win64 on x86_64 (MySQL Community Server - GPL)",
            exe.display()
        ),
        _ => {
            eprintln!("fake tool: unsupported arguments {args:?}");
            std::process::exit(1);
        }
    }
}
