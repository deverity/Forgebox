//! Imitates the version output of real tools, chosen by the executable's file name:
//! `php.exe` / `php-cgi.exe` (`-r`, `-v`, `-i`) and `nginx.exe` (`-v`, on stderr).
//! A file named `fake-ts` next to the executable makes it report a Thread Safe build.

fn main() {
    let exe = std::env::current_exe().expect("current exe");
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let ts = exe.with_file_name("fake-ts").exists();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = args.first().map(String::as_str).unwrap_or("");

    if stem.starts_with("nginx") {
        eprintln!("nginx version: nginx/1.22.0");
        return;
    }
    let safety = if ts { "enabled" } else { "disabled" };
    let build = if ts { "TS" } else { "NTS" };
    match flag {
        "-r" => print!("8.3.21"),
        "-v" => println!(
            "PHP 8.3.21 (cli) (built: May  6 2025 11:00:00) ({build} Visual C++ 2019 x64)"
        ),
        "-i" => {
            println!("phpinfo()");
            println!("PHP Version => 8.3.21");
            println!("Thread Safety => {safety}");
        }
        _ => {
            eprintln!("fake php: unsupported arguments {args:?}");
            std::process::exit(1);
        }
    }
}
