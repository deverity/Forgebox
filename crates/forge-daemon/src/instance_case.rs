//! `instance.case_sensitive` of a service instance with a data folder (docs/15 §5).
//!
//! The value decides settings that are fixed once the data folder is initialized (MySQL
//! and MariaDB `lower_case_table_names`: a server whose `my.ini` no longer matches its
//! data dictionary refuses to start). So:
//!
//! * **Not initialized yet** (the data folder does not exist; `services.rs` runs
//!   `data.init` only then): the value follows `forgebox.toml` **and** the real flag of
//!   the folder (or of its closest existing parent, which a new folder inherits). A flag
//!   that cannot be read, or a read that times out, counts as not case-sensitive (an `=0`
//!   server never initializes on an insensitive folder). The value is recorded in
//!   `instances.settings` ([`CASE_SENSITIVE_KEY`]) before `data.init` uses it.
//! * **Initialized**: the recorded value wins over the flag and over `forgebox.toml`. A
//!   failed or timed-out read changes nothing. A folder that now reads as insensitive
//!   while the instance was initialized case-sensitive keeps `=0` and publishes
//!   `fs.case_sensitivity_unavailable` (`fs.case.reason.data_dir_kept_sensitive`); an
//!   instance initialized insensitive while `forgebox.toml` asks for parity keeps `=2`
//!   and says so (`reason_code: initialized` when the folder itself is case-sensitive).
//! * **Initialized before the value was recorded** (daemons older than this module): the
//!   first affirmative read decides and is recorded, as for a new folder. Until then a
//!   failed read keeps what those daemons rendered, `forgebox.toml`'s `case_sensitive`.
//! * **Unknown whether initialized** (the existence check failed or timed out): the
//!   record when there is one; without it the render fails (start again), because
//!   guessing could break an initialized data folder.
//!
//! Every filesystem access runs in `spawn_blocking` with a timeout (docs/03 §7).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_core::error::{ForgeError, Result};
use forge_core::fs::case::{CaseReasonCode, DataDirCase, case_event_payload, data_dir_case};
use forge_core::paths::posix_string;
use forge_core::platform::CaseSensitiveFs;
#[cfg(doc)]
use forge_core::store::CASE_SENSITIVE_KEY;

/// What the daemon found about a data folder before rendering its instance.
#[derive(Debug)]
pub struct DataDirProbe {
    /// Whether the data folder exists, i.e. it was initialized. `Err`: unknown (the
    /// check failed or timed out).
    pub initialized: Result<bool>,
    /// The real flag of the folder (or of its closest existing parent). `Err`: the read
    /// timed out. `Ok(Insensitive { code: Error, .. })`: the read failed.
    pub case: Result<DataDirCase>,
}

impl DataDirProbe {
    /// Looks at `data_dir` (each step in `spawn_blocking`, bounded by `timeout`).
    pub async fn run(cs: Arc<dyn CaseSensitiveFs>, data_dir: &Path, timeout: Duration) -> Self {
        let path = data_dir.to_path_buf();
        let initialized = forge_core::runtime::spawn_blocking_timeout(
            "checking whether the data folder exists",
            timeout,
            move || match std::fs::metadata(&path) {
                Ok(meta) => Ok(meta.is_dir()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(ForgeError::io_at("reading", &path, e)),
            },
        )
        .await;
        let path: PathBuf = data_dir.to_path_buf();
        let case = forge_core::runtime::spawn_blocking_timeout(
            "checking the data folder case sensitivity",
            timeout,
            move || Ok(data_dir_case(cs.as_ref(), &path)),
        )
        .await;
        Self { initialized, case }
    }

    /// The flag when it was really read (sensitive, or insensitive for a known reason);
    /// `None` when the read failed or timed out.
    fn affirmative(&self) -> Option<&DataDirCase> {
        match &self.case {
            Ok(DataDirCase::Insensitive {
                code: CaseReasonCode::Error,
                ..
            })
            | Err(_) => None,
            Ok(case) => Some(case),
        }
    }
}

/// What to render and record for one instance.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseOutcome {
    /// `instance.case_sensitive`.
    pub sensitive: bool,
    /// Value to record as the one the data folder is (or is about to be) initialized
    /// with; `None` when the record is already right or nothing can be known yet.
    pub record: Option<bool>,
    /// Payload of `fs.case_sensitivity_unavailable`, when there is something to say.
    pub event: Option<serde_json::Value>,
    /// Why a value was kept without an event (for the log).
    pub note: Option<String>,
}

/// Decides `instance.case_sensitive` (see the module docs). `wanted` is `case_sensitive`
/// of `forgebox.toml`, `recorded` the value in `instances.settings`. `Err` only when it is
/// unknown whether the folder was initialized and nothing was recorded.
pub fn decide(
    wanted: bool,
    recorded: Option<bool>,
    probe: DataDirProbe,
    data_dir: &Path,
) -> Result<CaseOutcome> {
    let read = probe.affirmative().cloned();
    match (probe.initialized, recorded) {
        // Not initialized: decide now; `data.init` will use this value.
        (Ok(false), _) => {
            let (sensitive, event) = if wanted {
                fallback(probe.case, data_dir)
            } else {
                (false, None)
            };
            Ok(CaseOutcome {
                sensitive,
                record: (recorded != Some(sensitive)).then_some(sensitive),
                event,
                note: None,
            })
        }
        // Initialized (or unknown, where the record is the safe bet): the record wins.
        (initialized, Some(kept)) => {
            let event = match (kept, read) {
                (
                    true,
                    Some(DataDirCase::Insensitive {
                        dir, code, detail, ..
                    }),
                ) => {
                    let reason = forge_core::fs::case::messages::data_dir_kept_sensitive(&detail).1;
                    Some(case_event_payload(
                        &posix_string(&dir),
                        None,
                        Some((code, &reason)),
                    ))
                }
                (
                    false,
                    Some(DataDirCase::Insensitive {
                        dir, code, reason, ..
                    }),
                ) if wanted => Some(case_event_payload(
                    &posix_string(&dir),
                    None,
                    Some((code, &reason)),
                )),
                (false, _) if wanted => {
                    let reason =
                        forge_core::fs::case::messages::data_dir_initialized_insensitive().1;
                    Some(case_event_payload(
                        &posix_string(data_dir),
                        None,
                        Some((CaseReasonCode::Initialized, reason)),
                    ))
                }
                _ => None,
            };
            let note = match (&initialized, probe.case) {
                (Err(err), _) => Some(format!(
                    "data folder state unknown ({err}); keeping the recorded value"
                )),
                (_, Err(err)) => Some(format!(
                    "case flag not read ({err}); keeping the recorded value"
                )),
                (
                    _,
                    Ok(DataDirCase::Insensitive {
                        code: CaseReasonCode::Error,
                        detail,
                        ..
                    }),
                ) => Some(format!(
                    "case flag not read ({detail}); keeping the recorded value"
                )),
                _ => None,
            };
            Ok(CaseOutcome {
                sensitive: kept,
                record: None,
                event,
                note,
            })
        }
        // Initialized before the value was recorded.
        (Ok(true), None) => match read {
            Some(case) => {
                let (sensitive, event) = if wanted {
                    fallback(Ok(case), data_dir)
                } else {
                    (false, None)
                };
                Ok(CaseOutcome {
                    sensitive,
                    record: Some(sensitive),
                    event,
                    note: None,
                })
            }
            None => Ok(CaseOutcome {
                sensitive: wanted,
                record: None,
                event: None,
                note: Some(format!(
                    "case flag of an initialized data folder without a recorded value not \
                     read; keeping forgebox.toml case_sensitive = {wanted} until it can be"
                )),
            }),
        },
        (Err(err), None) => Err(err),
    }
}

/// A folder that is not initialized: case-sensitive only when the flag was read as set;
/// otherwise the `fs.case_sensitivity_unavailable` payload of the fallback to `=2`.
fn fallback(checked: Result<DataDirCase>, data_dir: &Path) -> (bool, Option<serde_json::Value>) {
    match checked {
        Ok(DataDirCase::Sensitive) => (true, None),
        Ok(DataDirCase::Insensitive {
            dir, code, reason, ..
        }) => (
            false,
            Some(case_event_payload(
                &posix_string(&dir),
                None,
                Some((code, &reason)),
            )),
        ),
        Err(err) => {
            let (_, reason) = forge_core::fs::case::messages::data_dir_fallback(&err.to_string());
            (
                false,
                Some(case_event_payload(
                    &posix_string(data_dir),
                    None,
                    Some((CaseReasonCode::Error, &reason)),
                )),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::paths::{ForgeRoot, InitOptions};
    use forge_core::platform::CaseSensitivitySupport;

    /// NTFS where the user lacks `FILE_DELETE_CHILD` (a root under `C:\` on a managed
    /// machine): reading works, every `set` is refused.
    struct RefusingCs;

    impl CaseSensitiveFs for RefusingCs {
        fn support(&self, _dir: &Path) -> CaseSensitivitySupport {
            CaseSensitivitySupport::Supported
        }
        fn get(&self, _dir: &Path) -> Result<bool> {
            Ok(false)
        }
        fn set(&self, _dir: &Path, _enabled: bool) -> Result<()> {
            Err(ForgeError::NeedsElevation("fs.set_case_sensitive".into()))
        }
    }

    /// Every folder is case-sensitive.
    struct SensitiveCs;

    impl CaseSensitiveFs for SensitiveCs {
        fn support(&self, _dir: &Path) -> CaseSensitivitySupport {
            CaseSensitivitySupport::Supported
        }
        fn get(&self, _dir: &Path) -> Result<bool> {
            Ok(true)
        }
        fn set(&self, _dir: &Path, _enabled: bool) -> Result<()> {
            Ok(())
        }
    }

    /// The flag cannot be read (a transient error, e.g. an antivirus holding the folder).
    struct FailingCs;

    impl CaseSensitiveFs for FailingCs {
        fn support(&self, _dir: &Path) -> CaseSensitivitySupport {
            CaseSensitivitySupport::Supported
        }
        fn get(&self, dir: &Path) -> Result<bool> {
            Err(ForgeError::Internal(format!(
                "NtQueryInformationFile {}: access denied",
                dir.display()
            )))
        }
        fn set(&self, _dir: &Path, _enabled: bool) -> Result<()> {
            Ok(())
        }
    }

    /// The read takes longer than the timeout (an antivirus scanning the folder).
    struct SlowCs;

    impl CaseSensitiveFs for SlowCs {
        fn support(&self, _dir: &Path) -> CaseSensitivitySupport {
            CaseSensitivitySupport::Supported
        }
        fn get(&self, _dir: &Path) -> Result<bool> {
            std::thread::sleep(Duration::from_millis(600));
            Ok(true)
        }
        fn set(&self, _dir: &Path, _enabled: bool) -> Result<()> {
            Ok(())
        }
    }

    const QUICK: Duration = Duration::from_secs(5);
    const SHORT: Duration = Duration::from_millis(50);

    /// `data/mysql` exists; returns the base, `data/mysql/default` (not created).
    fn layout() -> (tempfile::TempDir, PathBuf) {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("data").join("mysql");
        std::fs::create_dir_all(&data).unwrap();
        (base, data.join("default"))
    }

    async fn probe(
        cs: impl CaseSensitiveFs + 'static,
        dir: &Path,
        timeout: Duration,
    ) -> DataDirProbe {
        DataDirProbe::run(Arc::new(cs), dir, timeout).await
    }

    #[tokio::test]
    async fn initialized_and_a_failing_check_keep_the_recorded_case_sensitive_tables() {
        // Review of the gate fix: a transient read failure rewrote my.ini to
        // lower_case_table_names=2 and MySQL 8 refused to start (data dictionary at 0).
        let (_base, dir) = layout();
        std::fs::create_dir(&dir).unwrap();
        let checked = probe(FailingCs, &dir, QUICK).await;
        assert!(matches!(checked.initialized, Ok(true)));
        let outcome = decide(true, Some(true), checked, &dir).unwrap();
        assert!(outcome.sensitive, "lower_case_table_names must stay 0");
        assert_eq!(outcome.record, None);
        assert_eq!(outcome.event, None, "nothing became unavailable");
        assert!(outcome.note.unwrap().contains("access denied"));
        // Also when forgebox.toml no longer asks for it.
        let outcome = decide(false, Some(true), probe(FailingCs, &dir, QUICK).await, &dir).unwrap();
        assert!(outcome.sensitive);
    }

    #[tokio::test]
    async fn initialized_and_a_timed_out_check_keep_the_recorded_case_sensitive_tables() {
        let (_base, dir) = layout();
        std::fs::create_dir(&dir).unwrap();
        let checked = probe(SlowCs, &dir, SHORT).await;
        assert!(matches!(checked.initialized, Ok(true)));
        assert!(matches!(checked.case, Err(ForgeError::Timeout { .. })));
        let outcome = decide(true, Some(true), checked, &dir).unwrap();
        assert!(outcome.sensitive, "lower_case_table_names must stay 0");
        assert_eq!(outcome.record, None);
        assert_eq!(outcome.event, None);
        assert!(outcome.note.is_some());
    }

    #[tokio::test]
    async fn a_fresh_data_folder_and_a_failing_check_get_insensitive_tables() {
        let (_base, dir) = layout();
        let checked = probe(FailingCs, &dir, QUICK).await;
        assert!(matches!(checked.initialized, Ok(false)));
        let outcome = decide(true, None, checked, &dir).unwrap();
        assert!(!outcome.sensitive, "lower_case_table_names must be 2");
        assert_eq!(outcome.record, Some(false), "data.init uses this value");
        let event = outcome.event.expect("fs.case_sensitivity_unavailable");
        assert_eq!(event["reason_code"], "error");
        // A stale record of a folder that was deleted does not matter either.
        let outcome = decide(true, Some(true), probe(FailingCs, &dir, QUICK).await, &dir).unwrap();
        assert!(!outcome.sensitive);
        assert_eq!(outcome.record, Some(false));
        // Timed out: the same.
        let outcome = decide(true, None, probe(SlowCs, &dir, SHORT).await, &dir).unwrap();
        assert!(!outcome.sensitive);
        assert_eq!(outcome.record, Some(false));
        assert_eq!(outcome.event.unwrap()["reason_code"], "error");
    }

    #[tokio::test]
    async fn a_fresh_data_folder_follows_the_flag_and_the_configuration() {
        let (_base, dir) = layout();
        let outcome = decide(true, None, probe(SensitiveCs, &dir, QUICK).await, &dir).unwrap();
        assert_eq!(
            outcome,
            CaseOutcome {
                sensitive: true,
                record: Some(true),
                event: None,
                note: None,
            }
        );
        // Already recorded: nothing to write.
        let outcome = decide(
            true,
            Some(true),
            probe(SensitiveCs, &dir, QUICK).await,
            &dir,
        )
        .unwrap();
        assert_eq!(outcome.record, None);
        let outcome = decide(false, None, probe(SensitiveCs, &dir, QUICK).await, &dir).unwrap();
        assert!(!outcome.sensitive);
        assert_eq!(outcome.record, Some(false));
        assert_eq!(outcome.event, None);
    }

    #[tokio::test]
    async fn a_folder_without_the_flag_falls_back_to_insensitive_tables() {
        // Regression (M1 gate review): `case_sensitive = true` in forgebox.toml, init could
        // not set the flag, and the instance was still rendered with
        // lower_case_table_names=0, so `mysqld --initialize` aborted.
        let base = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(base.path().join("Forgebox")).unwrap();
        let report = root.init_with(InitOptions::default(), &RefusingCs).unwrap();
        assert!(
            report
                .case_sensitive_failed
                .iter()
                .any(|(dir, _)| *dir == root.data()),
            "init only records the failure"
        );
        let data_dir = root.data().join("mysql").join("default");
        let outcome = decide(
            true,
            None,
            probe(RefusingCs, &data_dir, QUICK).await,
            &data_dir,
        )
        .unwrap();
        assert!(!outcome.sensitive, "lower_case_table_names must be 2");
        assert_eq!(outcome.record, Some(false));
        let payload = outcome.event.expect("fs.case_sensitivity_unavailable");
        assert_eq!(payload["reason_code"], "insensitive");
        assert!(
            payload["reason"]
                .as_str()
                .unwrap()
                .contains("lower_case_table_names=2"),
            "{payload}"
        );
        let checked = payload["dir"].as_str().unwrap();
        assert!(!checked.contains('\\'), "{checked}");
        assert!(
            checked.starts_with(&posix_string(&root.data())),
            "{checked}"
        );
    }

    #[tokio::test]
    async fn an_initialized_instance_never_flips_on_an_affirmative_read() {
        let (_base, dir) = layout();
        std::fs::create_dir(&dir).unwrap();
        // Initialized with =0, the folder now reads insensitive: keep =0 and warn.
        let outcome = decide(true, Some(true), probe(RefusingCs, &dir, QUICK).await, &dir).unwrap();
        assert!(outcome.sensitive, "lower_case_table_names must stay 0");
        assert_eq!(outcome.record, None);
        let event = outcome.event.expect("warning");
        assert_eq!(event["reason_code"], "insensitive");
        let reason = event["reason"].as_str().unwrap();
        assert!(reason.contains("lower_case_table_names=0"), "{reason}");
        assert!(!reason.contains("=2"), "{reason}");
        // Initialized with =2, the folder now is case-sensitive: keep =2 and say why.
        let outcome = decide(
            true,
            Some(false),
            probe(SensitiveCs, &dir, QUICK).await,
            &dir,
        )
        .unwrap();
        assert!(!outcome.sensitive, "lower_case_table_names must stay 2");
        assert_eq!(outcome.event.unwrap()["reason_code"], "initialized");
        // ... with no event when forgebox.toml does not ask for parity.
        let outcome = decide(
            false,
            Some(false),
            probe(SensitiveCs, &dir, QUICK).await,
            &dir,
        )
        .unwrap();
        assert_eq!((outcome.sensitive, outcome.event), (false, None));
        // Initialized with =2 on a folder that still is insensitive: the usual fallback.
        let outcome = decide(
            true,
            Some(false),
            probe(RefusingCs, &dir, QUICK).await,
            &dir,
        )
        .unwrap();
        assert!(!outcome.sensitive);
        assert_eq!(outcome.event.unwrap()["reason_code"], "insensitive");
    }

    #[tokio::test]
    async fn an_initialized_folder_without_a_record_adopts_the_first_affirmative_read() {
        let (_base, dir) = layout();
        std::fs::create_dir(&dir).unwrap();
        // Failed read: what older daemons rendered (forgebox.toml), nothing recorded.
        let outcome = decide(true, None, probe(FailingCs, &dir, QUICK).await, &dir).unwrap();
        assert!(outcome.sensitive);
        assert_eq!(outcome.record, None);
        assert!(outcome.note.is_some());
        let outcome = decide(true, None, probe(SlowCs, &dir, SHORT).await, &dir).unwrap();
        assert!(outcome.sensitive);
        assert_eq!(outcome.record, None);
        // Affirmative reads are recorded.
        let outcome = decide(true, None, probe(SensitiveCs, &dir, QUICK).await, &dir).unwrap();
        assert_eq!((outcome.sensitive, outcome.record), (true, Some(true)));
        let outcome = decide(true, None, probe(RefusingCs, &dir, QUICK).await, &dir).unwrap();
        assert_eq!((outcome.sensitive, outcome.record), (false, Some(false)));
        assert_eq!(outcome.event.unwrap()["reason_code"], "insensitive");
    }

    #[test]
    fn unknown_state_uses_the_record_or_refuses_to_guess() {
        let dir = Path::new("C:/fb/data/mysql/default");
        let unknown = || DataDirProbe {
            initialized: Err(ForgeError::Timeout {
                operation: "checking whether the data folder exists".into(),
                after: SHORT,
            }),
            case: Err(ForgeError::Timeout {
                operation: "checking the data folder case sensitivity".into(),
                after: SHORT,
            }),
        };
        let outcome = decide(true, Some(true), unknown(), dir).unwrap();
        assert!(outcome.sensitive);
        assert_eq!(outcome.record, None);
        let outcome = decide(true, Some(false), unknown(), dir).unwrap();
        assert!(!outcome.sensitive);
        assert!(matches!(
            decide(true, None, unknown(), dir),
            Err(ForgeError::Timeout { .. })
        ));
    }
}
