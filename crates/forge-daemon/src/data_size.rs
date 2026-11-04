//! Size of the data folders of the instances (`InstanceDto.size_bytes`, M2-E2-T10): a
//! cache filled by a periodic scan **off the request path**. [`DataSizes::refresh`] only
//! looks at the cache and, when something is missing or older than the TTL, starts one
//! background task that walks the folders in `spawn_blocking`; `services.status` never
//! waits for it, so the first answers carry `None`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::ids::InstanceId;
use forge_core::sync::{LockLevel, OrderedMutex};

/// How long a measurement is good for.
pub const TTL: Duration = Duration::from_secs(60);
/// Budget of the walk of one folder (a result later than this is dropped).
const WALK_BUDGET: Duration = Duration::from_secs(30);

struct Sample {
    path: PathBuf,
    bytes: u64,
    at: Instant,
}

#[derive(Default)]
struct State {
    samples: HashMap<InstanceId, Sample>,
    scanning: bool,
}

/// The cache. Cheap to clone.
#[derive(Clone)]
pub struct DataSizes {
    state: Arc<OrderedMutex<State>>,
    ttl: Duration,
}

impl Default for DataSizes {
    fn default() -> Self {
        Self::with_ttl(TTL)
    }
}

impl DataSizes {
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            state: Arc::new(OrderedMutex::new(
                LockLevel::DAEMON_TABLES,
                State::default(),
            )),
            ttl,
        }
    }

    /// The last measurement of `id` for `path` (`None` until the first scan, or after the
    /// folder changed).
    pub fn get(&self, id: InstanceId, path: &Path) -> Option<u64> {
        let state = self.state.lock();
        state
            .samples
            .get(&id)
            .filter(|s| s.path == path)
            .map(|s| s.bytes)
    }

    /// Forgets an instance (removed).
    pub fn forget(&self, id: InstanceId) {
        self.state.lock().samples.remove(&id);
    }

    /// Starts a background scan of the `targets` whose measurement is missing or stale.
    /// Returns at once; does nothing while a scan runs. Needs a tokio runtime.
    pub fn refresh(&self, targets: Vec<(InstanceId, PathBuf)>) {
        let due: Vec<(InstanceId, PathBuf)> = {
            let mut state = self.state.lock();
            if state.scanning {
                return;
            }
            let due: Vec<_> = targets
                .into_iter()
                .filter(|(id, path)| {
                    state
                        .samples
                        .get(id)
                        .is_none_or(|s| &s.path != path || s.at.elapsed() >= self.ttl)
                })
                .collect();
            if due.is_empty() {
                return;
            }
            state.scanning = true;
            due
        };
        let this = self.clone();
        tokio::spawn(async move {
            for (id, path) in due {
                let walk = path.clone();
                let measured = forge_core::runtime::spawn_blocking_timeout(
                    "measuring a data folder",
                    WALK_BUDGET,
                    move || Ok(dir_size(&walk)),
                )
                .await;
                // A folder that is not there yet (not initialized) is 0 bytes, not "unknown".
                if let Ok(bytes) = measured {
                    this.state.lock().samples.insert(
                        id,
                        Sample {
                            path,
                            bytes,
                            at: Instant::now(),
                        },
                    );
                }
            }
            this.state.lock().scanning = false;
        });
    }
}

/// Sum of the sizes of the files under `root`; links are not followed and unreadable
/// entries count as 0.
pub fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                total = total.saturating_add(entry.metadata().map_or(0, |m| m.len()));
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_size_sums_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("x"), [0u8; 10]).unwrap();
        std::fs::write(dir.path().join("a/b/y"), [0u8; 32]).unwrap();
        assert_eq!(dir_size(dir.path()), 42);
        assert_eq!(dir_size(&dir.path().join("missing")), 0);
    }

    #[tokio::test]
    async fn refresh_fills_the_cache_without_blocking_and_honours_the_ttl() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x"), [0u8; 7]).unwrap();
        let sizes = DataSizes::with_ttl(Duration::from_millis(50));
        let id = InstanceId::new();
        assert_eq!(sizes.get(id, dir.path()), None, "none until the first scan");
        let started = Instant::now();
        sizes.refresh(vec![(id, dir.path().to_path_buf())]);
        assert!(
            started.elapsed() < Duration::from_millis(20),
            "refresh returns at once"
        );
        for _ in 0..100 {
            if sizes.get(id, dir.path()).is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(sizes.get(id, dir.path()), Some(7));
        // Stale: measured again.
        std::fs::write(dir.path().join("y"), [0u8; 3]).unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        sizes.refresh(vec![(id, dir.path().to_path_buf())]);
        for _ in 0..100 {
            if sizes.get(id, dir.path()) == Some(10) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(sizes.get(id, dir.path()), Some(10));
        assert_eq!(
            sizes.get(id, &dir.path().join("other")),
            None,
            "another folder"
        );
    }
}
