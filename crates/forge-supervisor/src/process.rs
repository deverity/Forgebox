//! What to start and how (docs/03 §1.2): [`SpawnSpec`], its deterministic environment and
//! where stdout/stderr go.
//!
//! The environment of a child is never the daemon's: it is the platform's minimal
//! allowlist ([`ProcessControl::base_env`]: system folders, user profile, temp,
//! processor info), plus the manifest variables ([`SpawnSpec::env`]), plus a `PATH` built
//! only from [`SpawnSpec::path`] (`<root>/bin/<tool>/<version>` and the `depends_on`
//! folders, in that order) followed by the system folders (`%SystemRoot%\System32`). A
//! stray `php.exe` in the user's `PATH` can never be picked up.
//!
//! The job is not part of the spec: the instance actor spawns into its own Job Object
//! ([`crate::job::JobTree::instance_job`]).

use std::ffi::OsString;
use std::path::PathBuf;

use forge_core::platform::{ConsoleMode, ProcessCommand, ProcessControl, StdioTarget};
use forge_core::{ForgeError, Result};
use serde::{Deserialize, Serialize};

/// Where a process' stdout and stderr go.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", content = "path", rename_all = "snake_case")]
pub enum Stdio {
    /// Pipes pumped by the supervisor into `logs/<tool>/<instance>.out.log` (rotated) and
    /// broadcast to log subscribers.
    #[default]
    Capture,
    /// The null device.
    Discard,
    /// Appended by the child itself to this file (no rotation, no live view).
    File(PathBuf),
}

/// Environment variables with Windows semantics: names compare case-insensitively
/// (ASCII), the last `set` wins and the original spelling of the first insertion is
/// kept.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EnvMap(Vec<(String, String)>);

impl EnvMap {
    pub fn new() -> Self {
        Self::default()
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.0
            .iter()
            .position(|(existing, _)| existing.eq_ignore_ascii_case(name))
    }

    /// Sets `name` (replacing any spelling of it).
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) -> &mut Self {
        let name = name.into();
        let value = value.into();
        match self.position(&name) {
            Some(index) => self.0[index].1 = value,
            None => self.0.push((name, value)),
        }
        self
    }

    /// Value of `name`, whatever its case.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.position(name).map(|index| self.0[index].1.as_str())
    }

    /// Removes `name`; returns its value.
    pub fn remove(&mut self, name: &str) -> Option<String> {
        self.position(name).map(|index| self.0.remove(index).1)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for EnvMap {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut map = Self::new();
        for (name, value) in iter {
            map.set(name, value);
        }
        map
    }
}

/// A process to supervise (docs/03 §1.2). Arguments and paths are already rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    /// Absolute path, inside `bin/<tool>/<version>/`.
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Manifest variables, applied over the minimal inherited environment. `PATH` is not
    /// allowed here: use [`SpawnSpec::path`].
    pub env: EnvMap,
    /// `PATH` entries, in order: `<root>/bin/<tool>/<version>` then the `depends_on`
    /// folders. The system folders are appended by the supervisor.
    pub path: Vec<PathBuf>,
    pub stdio: Stdio,
    /// Services never open a console window ([`ConsoleMode::Hidden`]).
    #[serde(skip, default)]
    pub console: ConsoleMode,
    /// `CREATE_NEW_PROCESS_GROUP` (manifest `console_group`, default true): needed for
    /// the `CTRL_BREAK` stop step.
    pub new_process_group: bool,
}

impl SpawnSpec {
    /// Spec with captured stdio, hidden console, own process group, empty env and path.
    pub fn new(exe: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: EnvMap::new(),
            path: Vec::new(),
            stdio: Stdio::Capture,
            console: ConsoleMode::Hidden,
            new_process_group: true,
        }
    }

    /// Appends arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets a manifest variable.
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.set(name, value);
        self
    }

    /// Appends a `PATH` folder.
    pub fn path_entry(mut self, dir: impl Into<PathBuf>) -> Self {
        self.path.push(dir.into());
        self
    }

    /// Sets where stdout/stderr go.
    pub fn stdio(mut self, stdio: Stdio) -> Self {
        self.stdio = stdio;
        self
    }

    /// The complete environment of the child: `base` (the platform allowlist), then the
    /// manifest variables, then the deterministic `PATH`.
    pub fn environment(
        &self,
        base: Vec<(OsString, OsString)>,
        system_path: &[PathBuf],
    ) -> Result<Vec<(OsString, OsString)>> {
        if self.env.get("PATH").is_some() {
            return Err(ForgeError::InvalidInput(
                "PATH cannot be set through env; list the folders in SpawnSpec::path".to_owned(),
            ));
        }
        let mut env: Vec<(OsString, OsString)> = base
            .into_iter()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("PATH"))
            .collect();
        for (name, value) in self.env.iter() {
            env.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
            env.push((name.into(), value.into()));
        }
        let dirs = self.path.iter().chain(system_path);
        let path = std::env::join_paths(dirs).map_err(|e| {
            ForgeError::InvalidInput(format!(
                "invalid PATH entry for {}: {e}",
                self.exe.display()
            ))
        })?;
        env.push(("PATH".into(), path));
        Ok(env)
    }

    /// The platform command for this spec.
    pub fn to_command(&self, processes: &dyn ProcessControl) -> Result<ProcessCommand> {
        let env = self.environment(processes.base_env(), &processes.system_path())?;
        let (stdout, stderr) = match &self.stdio {
            Stdio::Capture => (StdioTarget::Pipe, StdioTarget::Pipe),
            Stdio::Discard => (StdioTarget::Null, StdioTarget::Null),
            Stdio::File(path) => (
                StdioTarget::File(path.clone()),
                StdioTarget::File(path.clone()),
            ),
        };
        Ok(ProcessCommand {
            exe: self.exe.clone(),
            args: self.args.iter().map(OsString::from).collect(),
            cwd: self.cwd.clone(),
            env,
            stdout,
            stderr,
            new_process_group: self.new_process_group,
            console: self.console,
        })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn env_map_is_case_insensitive_and_keeps_first_spelling() {
        let mut env = EnvMap::new();
        env.set("PhpRc", "a").set("PHPRC", "b").set("X", "1");
        assert_eq!(env.get("phprc"), Some("b"));
        assert_eq!(
            env.iter().collect::<Vec<_>>(),
            vec![("PhpRc", "b"), ("X", "1")]
        );
        assert_eq!(env.remove("x"), Some("1".to_owned()));
        assert_eq!(env.len(), 1);
        let collected: EnvMap = [("A", "1"), ("a", "2")].into_iter().collect();
        assert_eq!(collected.iter().collect::<Vec<_>>(), vec![("A", "2")]);
    }

    #[test]
    fn environment_is_base_plus_manifest_plus_deterministic_path() {
        let spec = SpawnSpec::new(r"C:\fb\bin\php\8.4.1\php-cgi.exe", r"C:\fb\bin\php\8.4.1")
            .env("PHPRC", r"C:\fb\etc\php\8.4")
            .env("temp", r"C:\fb\tmp")
            .path_entry(r"C:\fb\bin\php\8.4.1")
            .path_entry(r"C:\fb\bin\vcredist");
        let base = vec![
            (OsString::from("SystemRoot"), OsString::from(r"C:\Windows")),
            (
                OsString::from("TEMP"),
                OsString::from(r"C:\Users\u\AppData\Local\Temp"),
            ),
            (OsString::from("Path"), OsString::from(r"C:\xampp\php")),
        ];
        let env = spec
            .environment(base, &[PathBuf::from(r"C:\Windows\System32")])
            .unwrap();
        let text: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        let sep = if cfg!(windows) { ";" } else { ":" };
        assert_eq!(
            text,
            vec![
                ("SystemRoot".into(), r"C:\Windows".into()),
                ("PHPRC".into(), r"C:\fb\etc\php\8.4".into()),
                ("temp".into(), r"C:\fb\tmp".into()),
                (
                    "PATH".into(),
                    format!(r"C:\fb\bin\php\8.4.1{sep}C:\fb\bin\vcredist{sep}C:\Windows\System32")
                ),
            ]
        );
    }

    #[test]
    fn path_in_env_is_rejected() {
        let spec = SpawnSpec::new("C:/x.exe", "C:/").env("Path", "C:/evil");
        let err = spec.environment(Vec::new(), &[]).unwrap_err();
        assert_eq!(err.code(), "invalid_input");
    }

    #[test]
    fn stdio_serializes_with_mode_tag() {
        let json = serde_json::to_value(Stdio::File("C:/l.log".into())).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"mode": "file", "path": "C:/l.log"})
        );
        assert_eq!(
            serde_json::to_value(Stdio::Capture).unwrap(),
            serde_json::json!({"mode": "capture"})
        );
    }
}
