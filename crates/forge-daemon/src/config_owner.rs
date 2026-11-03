//! Single owner of `forgebox.toml` writes (ADR-0040, M2-E3-T10).
//!
//! Every write (`system.config_set`, park/unpark, the Laragon import, `net.dns_*`) is a
//! read-modify-write of the whole file. Two of them interleaving would lose one update, so
//! they all go through one serial executor: a task with a mailbox that runs one turn at a
//! time. A turn reads the current preferences, applies the caller's change, saves the
//! file, hands the new value to the daemon, applies what can be applied live
//! ([`apply_effects`]) and publishes `config.changed`. No lock is held across an `.await`:
//! the mailbox is the only thing that serializes.
//!
//! Live effects per key:
//! - `dns.enabled`: starts or stops the resolver;
//! - `tld`, `dns.extra_tlds`, `dns.remote_zones`: the zones are reloaded, parked folders
//!   and the web configs are re-rendered (`config.rendered`); a TLD change also asks for a
//!   new CA (`tls.ca_rotate`, the CA name constraints list the TLDs) and for the DNS
//!   policy of the new suffix (`net.dns_enable`);
//! - `parked_dirs`: parked folders are reconciled;
//! - `default_server`: the web configs are re-rendered;
//! - `case_sensitive`: read when a folder is created, so it only applies to new folders.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use forge_core::config::ForgeConfig;
use forge_core::error::{ForgeError, Result};
use forge_core::event::EventKind;
use forge_ipc::api::ConfigChangedPayload;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::api::{reconcile_parked, reconcile_quietly};
use crate::daemon::Daemon;

/// Waiting for a free slot in the mailbox.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// A whole turn: save + live effects (a web re-render is the slow part).
const TURN_TIMEOUT: Duration = Duration::from_secs(90);
/// Bound of one live effect that talks to another component.
const EFFECT_TIMEOUT: Duration = Duration::from_secs(30);

type Turn = Pin<Box<dyn Future<Output = ()> + Send>>;

/// What one write did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Change {
    /// Dotted keys whose value changed (empty when the write changed nothing).
    pub keys: Vec<String>,
    /// Follow-ups the daemon did not do by itself, as method names.
    pub requires: Vec<String>,
}

/// Handle on the serial executor. Cheap to clone.
#[derive(Clone)]
pub struct ConfigOwner {
    tx: mpsc::Sender<Turn>,
}

impl ConfigOwner {
    /// Starts the executor task; it ends when the last handle is dropped.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn spawn() -> Self {
        let (tx, mut rx) = mpsc::channel::<Turn>(32);
        tokio::spawn(async move {
            while let Some(turn) = rx.recv().await {
                turn.await;
            }
        });
        Self { tx }
    }

    /// Runs `turn` after the turns queued before it and returns its result.
    pub async fn run<T, F>(&self, turn: F) -> Result<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let job: Turn = Box::pin(async move {
            let _ = reply.send(turn.await);
        });
        self.tx
            .send_timeout(job, SEND_TIMEOUT)
            .await
            .map_err(|_| ForgeError::Timeout {
                operation: "queueing a preferences write".into(),
                after: SEND_TIMEOUT,
            })?;
        match tokio::time::timeout(TURN_TIMEOUT, answer).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ForgeError::Internal("the preferences owner stopped".into())),
            Err(_) => Err(ForgeError::Timeout {
                operation: "writing preferences".into(),
                after: TURN_TIMEOUT,
            }),
        }
    }
}

/// Applies `change` to the current preferences (inside the owner's turn), saves them and
/// applies the live effects. A change that leaves the preferences equal writes nothing.
pub async fn update<F>(daemon: &Daemon, change: F) -> Result<Change>
where
    F: FnOnce(&ForgeConfig) -> Result<ForgeConfig> + Send + 'static,
{
    let d = daemon.clone();
    daemon
        .config_owner
        .run(async move {
            let old = d.config();
            let new = change(&old)?;
            new.validate()?;
            if new == *old {
                return Ok(Change::default());
            }
            let path = d.root.forgebox_toml();
            let saved = new.clone();
            forge_core::runtime::spawn_blocking_timeout(
                "saving forgebox.toml",
                Duration::from_secs(5),
                move || saved.save(&path),
            )
            .await?;
            d.set_config(new.clone());
            let keys = changed_keys(&old, &new);
            let requires = apply_effects(&d, &new, &keys).await;
            let payload = ConfigChangedPayload {
                v: 1,
                keys: keys.clone(),
                requires: requires.clone(),
            };
            d.bus.publish(
                EventKind::ConfigChanged,
                serde_json::to_value(&payload).unwrap_or(Value::Null),
            );
            Ok(Change { keys, requires })
        })
        .await
}

/// [`update`] that sets the whole preferences (the Laragon import, which computed them from
/// a snapshot).
pub async fn replace(daemon: &Daemon, config: ForgeConfig) -> Result<Change> {
    update(daemon, move |_| Ok(config)).await
}

/// Dotted keys of the leaves that differ between `old` and `new` (arrays and scalars are
/// leaves; a key missing on one side counts as different).
pub fn changed_keys(old: &ForgeConfig, new: &ForgeConfig) -> Vec<String> {
    let flat = |c: &ForgeConfig| {
        let mut out = BTreeMap::new();
        flatten(
            "",
            &serde_json::to_value(c).unwrap_or(Value::Null),
            &mut out,
        );
        out
    };
    let (a, b) = (flat(old), flat(new));
    let all: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    all.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect()
}

fn flatten(prefix: &str, value: &Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let dotted = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&dotted, child, out);
            }
        }
        leaf => {
            out.insert(prefix.to_owned(), leaf.clone());
        }
    }
}

/// Follow-ups that depend only on which keys changed.
pub fn requires_for(new: &ForgeConfig, keys: &[String]) -> Vec<String> {
    let mut requires = Vec::new();
    if keys.iter().any(|k| k == "tld" || k == "dns.extra_tlds") {
        requires.push("tls.ca_rotate".to_owned());
        if new.dns.enabled {
            requires.push("net.dns_enable".to_owned());
        }
    }
    requires
}

async fn apply_effects(daemon: &Daemon, new: &ForgeConfig, keys: &[String]) -> Vec<String> {
    let has = |k: &str| {
        keys.iter()
            .any(|x| x == k || x.starts_with(&format!("{k}.")))
    };
    let mut rerender = false;

    if has("dns.enabled") {
        let result = if new.dns.enabled {
            bounded("starting the resolver", async {
                daemon.dns.start().await.map(|addr| {
                    tracing::info!(%addr, "dns: resolver started by a preference change");
                })
            })
            .await
        } else {
            bounded("stopping the resolver", daemon.dns.stop()).await
        };
        if let Err(err) = result {
            tracing::warn!(%err, enabled = new.dns.enabled, "dns: preference not applied");
        }
    }

    if has("tld") || has("dns.extra_tlds") {
        daemon
            .sites
            .set_rules(forge_sites::SiteRules::new(new.tlds()));
    }
    if has("tld") || has("dns.extra_tlds") || has("dns.remote_zones") {
        match forge_net::dns::ZoneSet::from_config(new) {
            Ok(zones) => {
                if let Err(err) = bounded("reloading the DNS zones", daemon.dns.reload(zones)).await
                {
                    tracing::warn!(%err, "dns: zones not reloaded");
                }
            }
            Err(err) => tracing::warn!(%err, "dns: zones of the new preferences"),
        }
        rerender = true;
    }
    if has("tld") || has("parked_dirs") {
        let _ = bounded("reconciling parked folders", async {
            reconcile_parked(daemon).await;
            Ok(())
        })
        .await;
        rerender = true;
    }
    if has("default_server") {
        rerender = true;
    }
    if rerender {
        let _ = bounded("re-rendering the web configs", async {
            reconcile_quietly(daemon, "preferences").await;
            Ok(())
        })
        .await;
    }
    requires_for(new, keys)
}

async fn bounded<T>(what: &str, fut: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(EFFECT_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| {
            Err(ForgeError::Timeout {
                operation: what.to_owned(),
                after: EFFECT_TIMEOUT,
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn changed_keys_are_dotted_leaves() {
        let old = ForgeConfig::default();
        let mut new = old.clone();
        new.dns.enabled = false;
        new.tld = ".dev".into();
        new.dns.extra_tlds = vec![".localhost".into()];
        assert_eq!(
            changed_keys(&old, &new),
            ["dns.enabled", "dns.extra_tlds", "tld"]
        );
        assert!(changed_keys(&old, &old).is_empty());
    }

    #[test]
    fn a_tld_change_requires_a_new_ca() {
        let config = ForgeConfig::default();
        let requires = requires_for(&config, &["tld".to_owned()]);
        assert!(requires.contains(&"tls.ca_rotate".to_owned()));
        assert!(requires_for(&config, &["case_sensitive".to_owned()]).is_empty());
    }

    #[tokio::test]
    async fn turns_run_one_at_a_time_in_order() {
        let owner = ConfigOwner::spawn();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut tasks = Vec::new();
        for i in 0..5u32 {
            let (owner, log) = (owner.clone(), log.clone());
            tasks.push(tokio::spawn(async move {
                owner
                    .run(async move {
                        log.lock().unwrap().push(format!("start{i}"));
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        log.lock().unwrap().push(format!("end{i}"));
                        Ok(())
                    })
                    .await
            }));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        let entries = log.lock().unwrap().clone();
        assert_eq!(entries.len(), 10);
        for pair in entries.chunks(2) {
            assert!(pair[0].starts_with("start") && pair[1].starts_with("end"));
            assert_eq!(pair[0][5..], pair[1][3..]);
        }
    }
}
