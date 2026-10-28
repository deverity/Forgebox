//! Catalog entries and search (docs/04 §5, docs/09 `forge tool search`).
//!
//! Ranking, best first: exact `id` (1000) > exact name (900) > `id` prefix (700) > name
//! prefix (600) > `id` contains (500) > name contains (400) > tag equal (300) > tag contains
//! (200). A query with several words needs every word to match; its score is the lowest word
//! score. An empty query lists everything (score 0) in id order. Matching ignores case
//! (user input), unlike generated configs and paths (docs/15). Ties keep the order the
//! caller gave (registry priority) after sorting by id.

use std::cmp::Reverse;
use std::sync::Arc;

use forge_core::ids::ToolId;
use forge_manifest::schema::{Category, Kind, Manifest};
use serde::Serialize;

use crate::index::{Section, Verification};

/// A manifest of a loaded registry, as the catalog shows it.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    /// Name of the registry it comes from.
    pub registry: String,
    /// The registry is the official one.
    pub official: bool,
    pub verification: Verification,
    pub section: Section,
    pub id: ToolId,
    /// Path inside the registry (`tools/php.yaml`).
    pub path: String,
    pub sha256: String,
    /// Newest stable version according to the index.
    pub latest: Option<String>,
    pub manifest: Arc<Manifest>,
}

impl CatalogEntry {
    /// `Tool`, `Extension` or `Stack`.
    pub fn kind(&self) -> Kind {
        self.manifest.kind()
    }

    pub fn name(&self) -> &str {
        self.manifest.name()
    }

    pub fn summary(&self) -> Option<&str> {
        match &*self.manifest {
            Manifest::Tool(m) => m.metadata.summary.as_deref(),
            Manifest::Extension(m) => m.metadata.summary.as_deref(),
            Manifest::Stack(m) => m.metadata.summary.as_deref(),
            _ => None,
        }
    }

    pub fn tags(&self) -> &[String] {
        match &*self.manifest {
            Manifest::Tool(m) => &m.metadata.tags,
            Manifest::Extension(m) => &m.metadata.tags,
            Manifest::Stack(m) => &m.metadata.tags,
            _ => &[],
        }
    }

    /// Stacks have no category.
    pub fn category(&self) -> Option<Category> {
        match &*self.manifest {
            Manifest::Tool(m) => Some(m.metadata.category),
            Manifest::Extension(m) => Some(m.metadata.category),
            _ => None,
        }
    }
}

/// Why a hit matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// Empty query (listing).
    All,
    ExactId,
    ExactName,
    PrefixId,
    PrefixName,
    ContainsId,
    ContainsName,
    ExactTag,
    ContainsTag,
}

impl MatchKind {
    fn score(self) -> u32 {
        match self {
            Self::All => 0,
            Self::ExactId => 1000,
            Self::ExactName => 900,
            Self::PrefixId => 700,
            Self::PrefixName => 600,
            Self::ContainsId => 500,
            Self::ContainsName => 400,
            Self::ExactTag => 300,
            Self::ContainsTag => 200,
        }
    }
}

/// One search result.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub entry: CatalogEntry,
    pub score: u32,
    /// How the weakest word of the query matched.
    pub matched: MatchKind,
}

fn match_term(entry: &CatalogEntry, term: &str) -> Option<MatchKind> {
    let id = entry.id.as_str();
    let name = entry.name().to_lowercase();
    let mut best: Option<MatchKind> = None;
    let mut consider = |kind: MatchKind| {
        if best.is_none_or(|b| kind.score() > b.score()) {
            best = Some(kind);
        }
    };
    if id == term {
        consider(MatchKind::ExactId);
    } else if id.starts_with(term) {
        consider(MatchKind::PrefixId);
    } else if id.contains(term) {
        consider(MatchKind::ContainsId);
    }
    if name == term {
        consider(MatchKind::ExactName);
    } else if name.starts_with(term) {
        consider(MatchKind::PrefixName);
    } else if name.contains(term) {
        consider(MatchKind::ContainsName);
    }
    for tag in entry.tags() {
        let tag = tag.to_lowercase();
        if tag == term {
            consider(MatchKind::ExactTag);
        } else if tag.contains(term) {
            consider(MatchKind::ContainsTag);
        }
    }
    best
}

/// Searches `entries` (already in registry-priority order) by id, name and tag, optionally
/// restricted to one category. See the module docs for the ranking.
pub fn search(entries: &[CatalogEntry], query: &str, category: Option<Category>) -> Vec<SearchHit> {
    let query = query.trim().to_lowercase();
    let terms: Vec<&str> = query.split_whitespace().collect();
    let mut hits: Vec<SearchHit> = entries
        .iter()
        .filter(|e| category.is_none_or(|c| e.category() == Some(c)))
        .filter_map(|entry| {
            let matched = if terms.is_empty() {
                MatchKind::All
            } else {
                terms
                    .iter()
                    .map(|t| match_term(entry, t))
                    .collect::<Option<Vec<_>>>()?
                    .into_iter()
                    .min_by_key(|k| k.score())?
            };
            Some(SearchHit {
                entry: entry.clone(),
                score: matched.score(),
                matched,
            })
        })
        .collect();
    // Stable sort: equal (score, id) keep the registry priority order of the input.
    hits.sort_by(|a, b| {
        (Reverse(a.score), a.entry.id.as_str()).cmp(&(Reverse(b.score), b.entry.id.as_str()))
    });
    hits
}

/// Parses a category as written in manifests and the CLI (`webserver`, `database`...).
pub fn parse_category(text: &str) -> Option<Category> {
    serde_json::from_value(serde_json::Value::String(text.trim().to_lowercase())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str, category: Category, tags: &[&str]) -> CatalogEntry {
        let tags_yaml: String = tags.iter().map(|t| format!("\"{t}\", ")).collect();
        let text = format!(
            "apiVersion: forgebox.dev/v1\nkind: Tool\nmetadata: {{ id: {id}, name: \"{name}\", category: {}, tags: [{tags_yaml}] }}\nspec:\n  platforms: [windows-x64]\n  source:\n    providers:\n      - type: nodejs-dist\n",
            serde_json::to_value(category).unwrap().as_str().unwrap()
        );
        let manifest = forge_manifest::load_str(&text).unwrap();
        CatalogEntry {
            registry: "r".to_owned(),
            official: true,
            verification: Verification::Unverified,
            section: Section::Tools,
            id: ToolId::new(id).unwrap(),
            path: format!("tools/{id}.yaml"),
            sha256: String::new(),
            latest: None,
            manifest: Arc::new(manifest),
        }
    }

    fn catalog() -> Vec<CatalogEntry> {
        vec![
            entry("php", "PHP", Category::Runtime, &["language"]),
            entry("phpmyadmin", "phpMyAdmin", Category::Devtool, &["mysql"]),
            entry("mysql", "MySQL", Category::Database, &["sql", "php"]),
            entry("mariadb", "MariaDB", Category::Database, &["mysql", "sql"]),
            entry("xdebug", "Xdebug", Category::Devtool, &["debug", "php"]),
        ]
    }

    fn ids(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.entry.id.as_str()).collect()
    }

    #[test]
    fn exact_id_beats_prefix_beats_contains_beats_tag() {
        let hits = search(&catalog(), "php", None);
        assert_eq!(ids(&hits), ["php", "phpmyadmin", "mysql", "xdebug"]);
        assert_eq!(hits[0].matched, MatchKind::ExactId);
        assert_eq!(hits[1].matched, MatchKind::PrefixId);
        // `mysql` and `xdebug` only carry the tag `php`.
        assert_eq!(hits[2].matched, MatchKind::ExactTag);
        let hits = search(&catalog(), "sql", None);
        // contains in the id (`mysql`) outranks the tag-only hits.
        assert_eq!(ids(&hits)[0], "mysql");
        assert_eq!(hits[0].matched, MatchKind::ContainsId);
        assert_eq!(ids(&hits), ["mysql", "mariadb", "phpmyadmin"]);
    }

    #[test]
    fn matching_ignores_case_and_filters_by_category() {
        let hits = search(&catalog(), "  MySQL ", Some(Category::Database));
        assert_eq!(ids(&hits), ["mysql", "mariadb"]);
        assert!(search(&catalog(), "php", Some(Category::Cache)).is_empty());
    }

    #[test]
    fn empty_query_lists_by_id_and_every_word_must_match() {
        let all = search(&catalog(), "", None);
        assert_eq!(
            ids(&all),
            ["mariadb", "mysql", "php", "phpmyadmin", "xdebug"]
        );
        assert!(all.iter().all(|h| h.matched == MatchKind::All));
        let both = search(&catalog(), "my admin", None);
        assert_eq!(ids(&both), ["phpmyadmin"]);
        assert!(search(&catalog(), "php nonsense", None).is_empty());
    }

    #[test]
    fn name_matches_count() {
        let hits = search(&catalog(), "phpmyadmin", None);
        assert_eq!(hits[0].matched, MatchKind::ExactId);
        let hits = search(&catalog(), "xdeb", None);
        assert_eq!(ids(&hits), ["xdebug"]);
    }

    #[test]
    fn category_names_parse() {
        assert_eq!(parse_category("Database"), Some(Category::Database));
        assert_eq!(parse_category("nope"), None);
    }
}
