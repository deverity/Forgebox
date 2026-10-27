//! Command line of `forgeboxd` (docs/09: `forge daemon start` launches it).

use std::path::PathBuf;

use clap::Parser;

/// `forgeboxd`: the Forgebox engine (user-space daemon, one per installation root).
#[derive(Debug, Clone, Parser)]
#[command(name = "forgeboxd", version, about, long_about = None)]
pub struct Args {
    /// Installation root (default: `FORGEBOX_ROOT`, `portable.flag` next to the
    /// executable, `%LOCALAPPDATA%\Forgebox\root.toml`, then `C:\Forgebox`).
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,

    /// Also log to stdout (human format) besides `logs/forgeboxd.log.<date>`.
    #[arg(long)]
    pub foreground: bool,

    /// Log filter (`error`, `warn`, `info`, `debug`, `trace` or a `tracing` directive
    /// such as `info,forge_fcgi=debug`). `RUST_LOG` wins when set.
    #[arg(long, value_name = "FILTER", default_value = "info")]
    pub log_level: String,

    /// Remote listener (`wss://0.0.0.0:7788`), remote mode (M6-E3). Not available yet.
    #[arg(long, value_name = "URL")]
    pub listen: Option<String>,

    /// Replace this binary with a signed update and restart (docs/05 §5, M4-E1). Not
    /// available yet.
    #[arg(long, value_name = "FILE")]
    pub self_update: Option<PathBuf>,

    /// Source of the official registry (folder with `index.yaml`, or its URL), instead of
    /// the built-in one. Development and tests.
    #[arg(
        long,
        value_name = "SOURCE",
        env = "FORGEBOX_OFFICIAL_REGISTRY",
        hide = true
    )]
    pub registry: Option<String>,

    /// Global budget of the graceful shutdown (pools and instances stopped in parallel).
    #[arg(long, value_name = "SECONDS", default_value_t = 30, hide = true)]
    pub shutdown_timeout: u64,
}

impl Args {
    /// Arguments that start the same daemon again (`daemon.restart`).
    pub fn restart_args(&self, root: &std::path::Path) -> Vec<std::ffi::OsString> {
        let mut out: Vec<std::ffi::OsString> = vec!["--root".into(), root.as_os_str().into()];
        out.push("--log-level".into());
        out.push(self.log_level.clone().into());
        if let Some(registry) = &self.registry {
            out.push("--registry".into());
            out.push(registry.into());
        }
        out.push("--shutdown-timeout".into());
        out.push(self.shutdown_timeout.to_string().into());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_flags() {
        let args = Args::try_parse_from([
            "forgeboxd",
            "--root",
            r"D:\fb",
            "--foreground",
            "--log-level",
            "debug",
        ])
        .unwrap();
        assert_eq!(args.root.as_deref(), Some(std::path::Path::new(r"D:\fb")));
        assert!(args.foreground);
        assert_eq!(args.log_level, "debug");
        assert!(args.listen.is_none() && args.self_update.is_none());
        assert_eq!(args.shutdown_timeout, 30);
    }

    #[test]
    fn restart_keeps_root_and_level() {
        let args = Args::try_parse_from(["forgeboxd", "--log-level", "warn"]).unwrap();
        let again = args.restart_args(std::path::Path::new(r"D:\fb"));
        let text: Vec<String> = again
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&text[..4], ["--root", r"D:\fb", "--log-level", "warn"]);
    }
}
