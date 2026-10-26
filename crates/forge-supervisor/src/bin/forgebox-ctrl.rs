//! `forgebox-ctrl`: internal helper of `forgeboxd` that delivers `CTRL_BREAK` to the
//! console of a supervised process (docs/03 §1.3, ADR-0015).
//!
//! Usage: `forgebox-ctrl break <pid>`. Started by the daemon without a console of its
//! own; attaches to the target's console, generates the event for its process group and
//! exits. The exit code carries the result (see `forge_core::platform`).

fn main() {
    let code = forge_core::platform::ctrl_break_helper_main(std::env::args_os().skip(1));
    std::process::exit(code);
}
