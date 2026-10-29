//! Fake `php.exe` / `php-cgi.exe` of the daemon integration tests, compiled with `rustc`
//! by the tests (no dependencies). Installed as `bin/php/<version>/` (the folder name is
//! the version it reports), it is found by the manifest's `discover` at boot:
//! `php.exe -r "echo PHP_VERSION;"` prints the version and `php.exe -i` prints the
//! `Thread Safety` line of its variant (`nts`). Anything else exits 0 without output.
//!
//! As `php-cgi.exe -b <ip>:<port>` it is the FastCGI worker of the forge-fcgi tests
//! (`forge-fcgi/tests/fake_php_cgi`, included below) answering the pool's health script
//! with its version; a file named `cgi-fails` next to the executable makes it exit with
//! code 7 at once instead (a version whose pool cannot start).

#[path = "../../../forge-fcgi/tests/fake_php_cgi/src/main.rs"]
mod cgi;

/// `8.3.36` from the folder the executable is in; `8.4.27` outside a version folder.
fn version() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            let name = exe.parent()?.file_name()?.to_str()?.to_owned();
            let version: String = name
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            (!version.is_empty()).then_some(version)
        })
        .unwrap_or_else(|| "8.4.27".to_owned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let version = version();
    if args.iter().any(|a| a == "-b") {
        let marker = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("cgi-fails")));
        if marker.is_some_and(|m| m.exists()) {
            eprintln!("fake php-cgi: cgi-fails marker present");
            std::process::exit(7);
        }
        if std::env::var_os("FAKE_PHP_VERSION").is_none() {
            // Single-threaded here: nothing else reads the environment.
            unsafe { std::env::set_var("FAKE_PHP_VERSION", &version) };
        }
        cgi::main();
        return;
    }
    match args.first().map(String::as_str) {
        Some("-r") => print!("{version}"),
        Some("-i") => println!("PHP Version => {version}\nThread Safety => disabled"),
        Some("-v") => println!("PHP {version} (cli) (NTS Visual C++ 2022 x64)"),
        _ => {}
    }
}
