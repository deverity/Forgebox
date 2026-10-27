//! Site CRUD over the store with `site.*` events (docs/03 §8).
//!
//! Writes go through the store's writer thread, reads through the read pool; events are
//! published **after** the commit. Every call has the store's timeouts (ADR-0011).
//!
//! Each write closure runs in its own savepoint on the writer thread (see
//! `forge_core::store::writer`): an error rolls the whole closure back, so multi-step
//! operations (delete replaced parked sites, then insert) need no explicit transaction.

use forge_core::error::{ForgeError, Result};
use forge_core::event::{EventBus, EventKind};
use forge_core::ids::SiteId;
use forge_core::store::sites::{Site, SiteRepo};
use forge_core::store::{StoreReader, StoreWriter};
use jiff::Timestamp;

use crate::site::{SiteDraft, SiteRules, Warning, normalize_alias, normalize_host, slug_for_host};

/// Payload version of `site.*` events.
const PAYLOAD_VERSION: u32 = 1;

/// Wire form of `forge_ipc::api::events::SitePayload` (checked by tests/payload_contract.rs).
fn payload(site: &Site) -> serde_json::Value {
    serde_json::json!({
        "v": PAYLOAD_VERSION,
        "site_id": site.id,
        "host": site.host,
    })
}

/// Result of [`SiteService::create`].
#[derive(Debug, Clone)]
pub struct Created {
    pub site: Site,
    pub warnings: Vec<Warning>,
    /// Implicit (parked) sites that used one of the new site's names and were replaced:
    /// an explicit site always wins over a parked one.
    pub replaced_parked: Vec<Site>,
}

/// Handle to the sites of the store. Cheap to clone.
#[derive(Clone)]
pub struct SiteService {
    writer: StoreWriter,
    reader: StoreReader,
    bus: EventBus,
    rules: SiteRules,
}

impl std::fmt::Debug for SiteService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteService")
            .field("rules", &self.rules)
            .finish_non_exhaustive()
    }
}

impl SiteService {
    pub fn new(writer: StoreWriter, reader: StoreReader, bus: EventBus, rules: SiteRules) -> Self {
        Self {
            writer,
            reader,
            bus,
            rules,
        }
    }

    /// Rules applied to host names.
    pub fn rules(&self) -> &SiteRules {
        &self.rules
    }

    /// Creates an explicit (or, with `draft.parked`, an implicit) site. Names already
    /// used by a **parked** site replace it; names used by an explicit site are a
    /// `Conflict`. Emits `site.removed` for replaced sites, then `site.created`.
    pub async fn create(&self, draft: SiteDraft) -> Result<Created> {
        let (site, warnings) = draft.into_site(&self.rules)?;
        let to_insert = site.clone();
        let replaced = self
            .writer
            .execute(move |conn| {
                let repo = SiteRepo::new(conn);
                let mut replaced: Vec<Site> = Vec::new();
                if !to_insert.parked {
                    for name in to_insert.names() {
                        if let Some(owner) = repo.owner_of_name(name, None)? {
                            if owner.parked {
                                if !replaced.iter().any(|r| r.id == owner.id) {
                                    replaced.push(owner);
                                }
                            }
                        }
                    }
                    for owner in &replaced {
                        repo.delete(owner.id)?;
                    }
                }
                repo.insert(&to_insert)?;
                Ok(replaced)
            })
            .await?;
        for old in &replaced {
            self.bus.publish(EventKind::SiteRemoved, payload(old));
        }
        self.bus.publish(EventKind::SiteCreated, payload(&site));
        Ok(Created {
            site,
            warnings,
            replaced_parked: replaced,
        })
    }

    /// Creates an implicit site from a parked directory unless its host is taken:
    /// `Ok(None)` when an explicit site (or an existing parked one) already owns it.
    pub async fn create_parked_if_free(&self, mut draft: SiteDraft) -> Result<Option<Site>> {
        draft.parked = true;
        let (site, _) = draft.into_site(&self.rules)?;
        let to_insert = site.clone();
        let inserted = self
            .writer
            .execute(move |conn| {
                let repo = SiteRepo::new(conn);
                if repo.owner_of_name(&to_insert.host, None)?.is_some() {
                    return Ok(false);
                }
                match repo.insert(&to_insert) {
                    Ok(()) => {}
                    // slug collision (`a.b-test` vs `a-b.test`): leave it to the first one
                    Err(ForgeError::Conflict(_)) => return Ok(false),
                    Err(e) => return Err(e),
                }
                Ok(true)
            })
            .await?;
        if inserted {
            self.bus.publish(EventKind::SiteCreated, payload(&site));
        }
        Ok(inserted.then_some(site))
    }

    /// Replaces the stored site with `site` (same id), revalidating host and aliases and
    /// refreshing `updated_at`/`slug`. Emits `site.updated`.
    pub async fn update(&self, mut site: Site) -> Result<Site> {
        site.host = normalize_host(&site.host, &self.rules)?.value;
        site.slug = slug_for_host(&site.host);
        let mut aliases = Vec::new();
        for raw in &site.aliases {
            let alias = normalize_alias(raw)?.value;
            if alias != site.host && !aliases.contains(&alias) {
                aliases.push(alias);
            }
        }
        site.aliases = aliases;
        site.updated_at = Timestamp::now();
        let to_store = site.clone();
        self.writer
            .execute(move |conn| SiteRepo::new(conn).update(&to_store))
            .await?;
        self.bus.publish(EventKind::SiteUpdated, payload(&site));
        Ok(site)
    }

    /// Records whether the site's project folder is case-sensitive (after a conversion
    /// or the boot check, docs/15 §3.1). Emits `site.updated` only when the value
    /// changed. `NotFound` if the site does not exist.
    pub async fn set_case_sensitive(&self, id: SiteId, enabled: bool) -> Result<Site> {
        let (site, changed) = self
            .writer
            .execute(move |conn| {
                let repo = SiteRepo::new(conn);
                let mut site = repo
                    .get(id)?
                    .ok_or_else(|| ForgeError::NotFound(format!("site {id}")))?;
                if site.case_sensitive == enabled {
                    return Ok((site, false));
                }
                site.case_sensitive = enabled;
                site.updated_at = Timestamp::now();
                repo.update(&site)?;
                Ok((site, true))
            })
            .await?;
        if changed {
            self.bus.publish(EventKind::SiteUpdated, payload(&site));
        }
        Ok(site)
    }

    /// Removes the site; `NotFound` if it does not exist. Emits `site.removed`.
    pub async fn remove(&self, id: SiteId) -> Result<Site> {
        let removed = self
            .writer
            .execute(move |conn| {
                let repo = SiteRepo::new(conn);
                let site = repo
                    .get(id)?
                    .ok_or_else(|| ForgeError::NotFound(format!("site {id}")))?;
                repo.delete(id)?;
                Ok(site)
            })
            .await?;
        self.bus.publish(EventKind::SiteRemoved, payload(&removed));
        Ok(removed)
    }

    /// Site by id.
    pub async fn get(&self, id: SiteId) -> Result<Option<Site>> {
        self.reader
            .query(move |conn| SiteRepo::new(conn).get(id))
            .await
    }

    /// Site by primary host (case-insensitive input).
    pub async fn get_by_host(&self, host: &str) -> Result<Option<Site>> {
        let host = host.to_ascii_lowercase();
        self.reader
            .query(move |conn| SiteRepo::new(conn).get_by_host(&host))
            .await
    }

    /// Every site ordered by host.
    pub async fn list(&self) -> Result<Vec<Site>> {
        self.reader.query(|conn| SiteRepo::new(conn).list()).await
    }
}
