//! Rings of the most recent 5xx and 4xx requests of a site ("Erros recentes", docs/14
//! section 4.1). In memory only; entries older than the retention are dropped, so a site that
//! went idle gives its memory back.

use std::collections::VecDeque;

use jiff::Timestamp;

use crate::record::AccessRecord;

/// One request that failed, as shown in "Erros recentes". The route is normalized and never
/// has a query string (docs/14 section 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentError {
    /// When the request finished.
    pub at: Timestamp,
    /// HTTP status (`400..=599`).
    pub status: u16,
    /// Request method (may be empty when the format has none).
    pub method: String,
    /// Normalized route (may be empty when the format has no URI).
    pub route: String,
    /// Host the request was for (one of the site's names).
    pub host: String,
    /// Total time, in microseconds.
    pub duration_us: u64,
    /// Upstream (PHP/proxy) time, in microseconds.
    pub upstream_us: Option<u64>,
    /// Response body bytes.
    pub bytes: u64,
}

/// The rings of a site, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentErrors {
    /// Last 5xx responses.
    pub server: Vec<RecentError>,
    /// Last 4xx responses.
    pub client: Vec<RecentError>,
}

#[derive(Debug)]
struct Entry {
    at: Timestamp,
    status: u16,
    duration_us: u64,
    upstream_us: Option<u64>,
    bytes: u64,
    method: Box<str>,
    route: Box<str>,
    host: Box<str>,
}

impl Entry {
    fn heap_bytes(&self) -> usize {
        self.method.len() + self.route.len() + self.host.len()
    }

    fn to_public(&self) -> RecentError {
        RecentError {
            at: self.at,
            status: self.status,
            method: self.method.to_string(),
            route: self.route.to_string(),
            host: self.host.to_string(),
            duration_us: self.duration_us,
            upstream_us: self.upstream_us,
            bytes: self.bytes,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct ErrorRings {
    server: VecDeque<Entry>,
    client: VecDeque<Entry>,
}

fn push(ring: &mut VecDeque<Entry>, cap: usize, entry: Entry) {
    if cap == 0 {
        return;
    }
    if ring.len() >= cap {
        ring.pop_front();
    } else if ring.len() == ring.capacity() {
        // Never more than `cap` slots (the default doubling would overshoot).
        let extra = (ring.len().max(4)).min(cap - ring.len());
        ring.reserve_exact(extra);
    }
    ring.push_back(entry);
}

fn prune(ring: &mut VecDeque<Entry>, before: Timestamp) {
    // Arrival order is end_ts order up to the tail delay: pruning from the front is enough.
    while ring.front().is_some_and(|e| e.at < before) {
        ring.pop_front();
    }
    if ring.is_empty() {
        *ring = VecDeque::new();
    }
}

impl ErrorRings {
    pub(crate) fn push(&mut self, rec: &AccessRecord, cap: usize) {
        let ring = match rec.status {
            500..=599 => &mut self.server,
            400..=499 => &mut self.client,
            _ => return,
        };
        let entry = Entry {
            at: rec.end_ts,
            status: rec.status,
            duration_us: rec.duration_us,
            upstream_us: rec.upstream_us,
            bytes: rec.bytes,
            method: rec.method.as_str().into(),
            route: rec.route.as_str().into(),
            host: rec.host.as_str().into(),
        };
        push(ring, cap, entry);
    }

    pub(crate) fn prune(&mut self, before: Timestamp) {
        prune(&mut self.server, before);
        prune(&mut self.client, before);
    }

    pub(crate) fn snapshot(&self) -> RecentErrors {
        RecentErrors {
            server: self.server.iter().rev().map(Entry::to_public).collect(),
            client: self.client.iter().rev().map(Entry::to_public).collect(),
        }
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        let slots = (self.server.capacity() + self.client.capacity()) * size_of::<Entry>();
        let strings: usize = self
            .server
            .iter()
            .chain(self.client.iter())
            .map(Entry::heap_bytes)
            .sum();
        slots + strings
    }
}
