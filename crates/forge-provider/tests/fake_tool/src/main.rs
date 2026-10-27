//! Fake `php.exe` of the layout tests. The version is the content of `fake-version.txt`
//! next to the executable (so every installed "version" is the same binary):
//! * `-v` prints `PHP <version> (cli) (fake)`, like `php -v`;
//! * `--env NAME` prints `PHP <version> ENV <value of NAME>` (shim environment checks);
//! * anything else prints `PHP <version> ARGS <arg>|<arg>...` (shim pass-through checks).

fn main() {
    let exe = std::env::current_exe().expect("current exe");
    let version = std::fs::read_to_string(exe.with_file_name("fake-version.txt"))
        .map(|v| v.trim().to_owned())
        .unwrap_or_else(|_| "0.0.0".to_owned());
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-v") => println!("PHP {version} (cli) (fake)"),
        Some("--env") => {
            let name = args.get(1).map(String::as_str).unwrap_or_default();
            println!(
                "PHP {version} ENV {}",
                std::env::var(name).unwrap_or_default()
            );
        }
        _ => println!("PHP {version} ARGS {}", args.join("|")),
    }
}
