//! `forge`: the Forgebox command line (docs/09).

fn main() {
    std::process::exit(forge_cli::run(std::env::args_os()));
}
