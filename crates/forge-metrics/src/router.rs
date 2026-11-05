//! Host -> site router (docs/06 section 1.3: `hosts = [host, "*.host", ...aliases]`).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use forge_core::ids::SiteId;

/// The hosts one site answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteHosts {
    /// The site.
    pub id: SiteId,
    /// Primary host. It matches exactly, and `*.host` (any depth) matches as a wildcard.
    pub host: String,
    /// Aliases. Each matches exactly; an alias written `*.name` is a wildcard for `name`.
    pub aliases: Vec<String>,
}

/// Where a host goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routed {
    /// An exact host/alias or the longest matching wildcard.
    Site(SiteId),
    /// Nobody answers to this host (counted by [`HostRouter::unrouted`]).
    Unrouted,
}

impl Routed {
    /// The site, if any.
    pub fn site(self) -> Option<SiteId> {
        match self {
            Self::Site(id) => Some(id),
            Self::Unrouted => None,
        }
    }
}

#[derive(Debug, Default)]
struct Table {
    exact: HashMap<Box<str>, SiteId>,
    /// Key: the base name of `*.base`.
    wildcard: HashMap<Box<str>, SiteId>,
}

/// Immutable snapshot of the host table, cheap to clone and to share between tasks.
///
/// A change in the sites builds a new snapshot with [`HostRouter::rebuild`]; the counter of
/// unrouted hosts is shared by all snapshots derived from the same router.
#[derive(Debug, Clone)]
pub struct HostRouter {
    table: Arc<Table>,
    unrouted: Arc<AtomicU64>,
}

impl HostRouter {
    /// Builds a snapshot. Hosts are normalized ([`normalize_host`]); when two sites claim the
    /// same name the first one wins (sites enforce global uniqueness, this only makes the
    /// outcome deterministic).
    pub fn new(sites: &[SiteHosts]) -> Self {
        Self {
            table: Arc::new(Table::build(sites)),
            unrouted: Arc::new(AtomicU64::new(0)),
        }
    }

    /// A new snapshot for `sites` that keeps counting in the same unrouted counter.
    pub fn rebuild(&self, sites: &[SiteHosts]) -> Self {
        Self {
            table: Arc::new(Table::build(sites)),
            unrouted: Arc::clone(&self.unrouted),
        }
    }

    /// Routes a raw host (`Loja.Test:8080`, `shop.loja.test.`): normalizes it first.
    pub fn route(&self, host: &str) -> Routed {
        self.route_normalized(&normalize_host(host))
    }

    /// Routes a host already normalized by [`normalize_host`] (what `AccessRecord::host` is).
    ///
    /// Exact host or alias first, then the longest `*.base` suffix; otherwise counts the host
    /// as unrouted.
    pub fn route_normalized(&self, host: &str) -> Routed {
        if let Some(id) = self.table.exact.get(host) {
            return Routed::Site(*id);
        }
        // `a.b.loja.test` tries `b.loja.test`, `loja.test`, `test`: longest first.
        let mut rest = host;
        while let Some(dot) = rest.find('.') {
            rest = &rest[dot + 1..];
            if let Some(id) = self.table.wildcard.get(rest) {
                return Routed::Site(*id);
            }
        }
        self.unrouted.fetch_add(1, Ordering::Relaxed);
        Routed::Unrouted
    }

    /// How many lookups found no site since the router was created (all snapshots together).
    pub fn unrouted(&self) -> u64 {
        self.unrouted.load(Ordering::Relaxed)
    }

    /// Number of names (exact + wildcard) in this snapshot.
    pub fn len(&self) -> usize {
        self.table.exact.len() + self.table.wildcard.len()
    }

    /// `true` when no site is routable.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Table {
    fn build(sites: &[SiteHosts]) -> Self {
        let mut table = Table::default();
        for site in sites {
            let host = normalize_host(&site.host);
            if !host.is_empty() {
                table.add_exact(&host, site.id);
                table.add_wildcard(&host, site.id);
            }
            for alias in &site.aliases {
                let alias = normalize_host(alias);
                match alias.strip_prefix("*.") {
                    Some(base) if !base.is_empty() => table.add_wildcard(base, site.id),
                    _ if alias.is_empty() || alias.starts_with('*') => {}
                    _ => table.add_exact(&alias, site.id),
                }
            }
        }
        table
    }

    fn add_exact(&mut self, name: &str, id: SiteId) {
        self.exact.entry(name.into()).or_insert(id);
    }

    fn add_wildcard(&mut self, base: &str, id: SiteId) {
        self.wildcard.entry(base.into()).or_insert(id);
    }
}

/// Normalizes a host for matching: trims, drops the port (`:8080`, also after `[::1]`), drops
/// trailing dots and lowercases (ASCII; docs/15 section 7 keeps hosts lowercase).
pub fn normalize_host(raw: &str) -> Cow<'_, str> {
    let mut host = raw.trim();
    if host.starts_with('[') {
        // [v6]:port keeps the brackets and drops the port.
        if let Some(end) = host.find(']') {
            host = &host[..=end];
        }
    } else if let Some(colon) = host.rfind(':') {
        let (name, port) = (&host[..colon], &host[colon + 1..]);
        // Only a single colon followed by digits is a port; a bare IPv6 stays whole.
        if !name.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) {
            host = name;
        }
    }
    let host = host.trim_end_matches('.');
    if host.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(host.to_ascii_lowercase())
    } else {
        Cow::Borrowed(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(host: &str, aliases: &[&str]) -> SiteHosts {
        SiteHosts {
            id: SiteId::new(),
            host: host.to_owned(),
            aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn normalizes_hosts() {
        let cases = [
            ("loja.test", "loja.test"),
            ("Loja.TEST", "loja.test"),
            ("loja.test:8080", "loja.test"),
            ("loja.test.", "loja.test"),
            ("Loja.Test.:443", "loja.test"),
            ("  loja.test ", "loja.test"),
            ("[::1]:8080", "[::1]"),
            ("[::1]", "[::1]"),
            ("::1", "::1"),
            ("127.0.0.1:18086", "127.0.0.1"),
            ("host:notaport", "host:notaport"),
            ("-", "-"),
            ("", ""),
        ];
        for (raw, want) in cases {
            assert_eq!(normalize_host(raw), want, "raw={raw:?}");
        }
    }

    #[test]
    fn exact_alias_wildcard_and_unknown() {
        let loja = site(
            "loja.test",
            &["loja.local", "*.cdn.loja.local", "Blog.Test"],
        );
        let shop = site("shop.loja.test", &[]);
        let other = site("other.test", &[]);
        let router = HostRouter::new(&[loja.clone(), shop.clone(), other.clone()]);

        // Exact host and alias, whatever the case/port/dot.
        assert_eq!(router.route("loja.test"), Routed::Site(loja.id));
        assert_eq!(router.route("LOJA.test:8080"), Routed::Site(loja.id));
        assert_eq!(router.route("loja.local"), Routed::Site(loja.id));
        assert_eq!(router.route("blog.test."), Routed::Site(loja.id));
        assert_eq!(router.route("other.test"), Routed::Site(other.id));
        // `*.host` of the primary host.
        assert_eq!(router.route("api.loja.test"), Routed::Site(loja.id));
        assert_eq!(router.route("a.b.loja.test"), Routed::Site(loja.id));
        // Wildcard alias.
        assert_eq!(router.route("img.cdn.loja.local"), Routed::Site(loja.id));
        // The longest suffix wins over the shorter one, and an exact host over any wildcard.
        assert_eq!(router.route("shop.loja.test"), Routed::Site(shop.id));
        assert_eq!(router.route("x.shop.loja.test"), Routed::Site(shop.id));
        assert_eq!(router.route("x.y.shop.loja.test"), Routed::Site(shop.id));
        // An alias is not a wildcard by itself; the bare base of `*.x` is not matched.
        assert_eq!(router.route("sub.loja.local"), Routed::Unrouted);
        assert_eq!(router.route("cdn.loja.local"), Routed::Unrouted);
        // Unknown hosts.
        assert_eq!(router.route("unknown.test"), Routed::Unrouted);
        assert_eq!(router.route("test"), Routed::Unrouted);
        assert_eq!(router.route("-"), Routed::Unrouted);
        assert_eq!(router.route(""), Routed::Unrouted);
        assert_eq!(router.route("127.0.0.1"), Routed::Unrouted);
        assert_eq!(router.route("notloja.test"), Routed::Unrouted);
        assert_eq!(router.route("xloja.test").site(), None);
    }

    #[test]
    fn counts_unrouted_across_rebuilds() {
        let a = site("a.test", &[]);
        let router = HostRouter::new(std::slice::from_ref(&a));
        assert_eq!(router.unrouted(), 0);
        router.route("nope.test");
        router.route("a.test");
        router.route("nope2.test");
        assert_eq!(router.unrouted(), 2);

        let b = site("b.test", &[]);
        let next = router.rebuild(std::slice::from_ref(&b));
        assert_eq!(next.route("a.test"), Routed::Unrouted);
        assert_eq!(next.route("b.test"), Routed::Site(b.id));
        // The old snapshot is untouched and both feed the same counter.
        assert_eq!(router.route("a.test"), Routed::Site(a.id));
        assert_eq!(router.unrouted(), 3);
        assert_eq!(next.unrouted(), 3);
    }

    #[test]
    fn first_site_wins_on_duplicates_and_junk_is_ignored() {
        let first = site("dup.test", &[]);
        let second = site("DUP.test", &["*", "*.", "", "dup.test"]);
        let router = HostRouter::new(&[first.clone(), second]);
        assert_eq!(router.route("dup.test"), Routed::Site(first.id));
        assert_eq!(router.route("anything.else"), Routed::Unrouted);
        assert!(!router.is_empty());
        assert_eq!(router.len(), 2);
        assert!(HostRouter::new(&[]).is_empty());
    }
}
