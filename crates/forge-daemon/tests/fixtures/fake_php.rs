//! Fake `php.exe` / `php-cgi.exe` of the daemon integration tests, compiled with `rustc`
//! by the tests (no dependencies). Installed as `bin/php/8.4.27/`, it is found by the
//! manifest's `discover` at boot: `php.exe -r "echo PHP_VERSION;"` prints `8.4.27` and
//! `php.exe -i` prints the `Thread Safety` line of its variant (`nts`). Anything else
//! exits 0 without output.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-r") => print!("8.4.27"),
        Some("-i") => println!("PHP Version => 8.4.27\nThread Safety => disabled"),
        Some("-v") => println!("PHP 8.4.27 (cli) (NTS Visual C++ 2022 x64)"),
        _ => {}
    }
}
