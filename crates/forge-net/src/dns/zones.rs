//! Zones answered by the Forgebox resolver and the answer to one question (pure, no I/O).
//!
//! * Local zones (one per TLD, `.test` by default): **every** name in the zone, the apex
//!   included, answers `A 127.0.0.1` and `AAAA ::1` (docs/06 §5.1).
//! * Remote zones (docs/13 §5): `*.hml.test → 10.0.0.5`; the most specific zone wins, so
//!   `loja.hml.test` goes to the remote machine while `loja.test` stays local. A remote
//!   zone with only an IPv4 address answers `AAAA` with no data (NOERROR), never
//!   NXDOMAIN: a negative answer for one type must not hide the other.
//! * Other types (`MX`, `TXT`, `HTTPS`...) answer no data with the zone's `SOA` in the
//!   authority section (negative caching, RFC 2308); `SOA` at the apex answers it.
//! * Names outside every zone answer NXDOMAIN, not authoritative. There is no upstream
//!   passthrough: the NRPT only sends the zones here.
//! * Short TTLs ([`DEFAULT_TTL`], [`NEGATIVE_TTL`]) so a reload or a new remote is seen
//!   quickly; the resolver cache is flushed on policy changes anyway.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use forge_core::config::ForgeConfig;
use forge_core::dns_name;
use forge_core::error::{ForgeError, Result};
use hickory_server::proto::op::{Query, ResponseCode};
use hickory_server::proto::rr::rdata::{A, AAAA, SOA};
use hickory_server::proto::rr::{DNSClass, Name, RData, Record, RecordType};
use serde::Serialize;

/// TTL of positive answers, in seconds.
pub const DEFAULT_TTL: u32 = 30;
/// TTL of negative answers (SOA `minimum` and SOA record TTL), in seconds.
pub const NEGATIVE_TTL: u32 = 10;

/// What a zone is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneKind {
    /// A TLD of this machine: loopback answers.
    Local,
    /// The sites of a remote daemon (docs/13 §5).
    Remote,
}

/// One zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Zone {
    /// Lowercase name without dots around it (`test`, `hml.test`).
    pub name: String,
    pub ipv4: Option<Ipv4Addr>,
    pub ipv6: Option<Ipv6Addr>,
    pub kind: ZoneKind,
}

impl Zone {
    /// Addresses a name of the zone resolves to, IPv4 first.
    pub fn addresses(&self) -> Vec<IpAddr> {
        self.ipv4
            .map(IpAddr::V4)
            .into_iter()
            .chain(self.ipv6.map(IpAddr::V6))
            .collect()
    }
}

/// The zones of a resolver. Immutable once built; a reload swaps the whole set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZoneSet {
    /// Most specific first (more labels), then by name.
    zones: Vec<Zone>,
    ttl: u32,
}

impl Default for ZoneSet {
    fn default() -> Self {
        Self::new()
    }
}

/// The answer to one question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub code: ResponseCode,
    /// The AA bit: true inside our zones.
    pub authoritative: bool,
    pub answers: Vec<Record>,
    /// The zone's SOA on negative answers.
    pub soa: Vec<Record>,
}

impl Answer {
    fn refused() -> Self {
        Self {
            code: ResponseCode::Refused,
            authoritative: false,
            answers: Vec::new(),
            soa: Vec::new(),
        }
    }
}

/// Lowercase name without the trailing dot (`Loja.Test.` → `loja.test`).
pub fn plain_name(name: &Name) -> String {
    name.to_ascii().trim_end_matches('.').to_ascii_lowercase()
}

impl ZoneSet {
    /// No zones (every question is NXDOMAIN), TTL [`DEFAULT_TTL`].
    pub fn new() -> Self {
        Self {
            zones: Vec::new(),
            ttl: DEFAULT_TTL,
        }
    }

    /// Local zones for `tlds` (`[".test"]`).
    pub fn local(tlds: &[String]) -> Result<Self> {
        let mut set = Self::new();
        for tld in tlds {
            set.add_local(tld)?;
        }
        Ok(set)
    }

    /// The zones of `forgebox.toml`: every TLD local, every `[[dns.remote_zones]]` remote.
    pub fn from_config(config: &ForgeConfig) -> Result<Self> {
        let mut set = Self::local(&config.tlds())?;
        for remote in &config.dns.remote_zones {
            set.add_remote(&remote.zone, remote.address)?;
        }
        Ok(set)
    }

    /// TTL of positive answers.
    pub fn with_ttl(mut self, ttl: u32) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn ttl(&self) -> u32 {
        self.ttl
    }

    /// Adds the local zone of `tld` (`.test`).
    pub fn add_local(&mut self, tld: &str) -> Result<()> {
        let name = dns_name::parse_suffix(tld).map_err(ForgeError::InvalidInput)?;
        self.upsert(Zone {
            name: name.to_owned(),
            ipv4: Some(Ipv4Addr::LOCALHOST),
            ipv6: Some(Ipv6Addr::LOCALHOST),
            kind: ZoneKind::Local,
        });
        Ok(())
    }

    /// Adds `address` to the remote zone `zone` (`hml.test`); call twice for IPv4 and
    /// IPv6. The zone must sit below a local zone already added (it is reached through
    /// that TLD's NRPT rule).
    pub fn add_remote(&mut self, zone: &str, address: IpAddr) -> Result<()> {
        dns_name::check_name(zone).map_err(ForgeError::InvalidInput)?;
        let parent = self.zones.iter().any(|z| {
            z.kind == ZoneKind::Local && z.name != zone && dns_name::is_within(zone, &z.name)
        });
        if !parent {
            return Err(ForgeError::InvalidInput(format!(
                "remote zone {zone:?} must be below one of the local zones {:?}",
                self.names()
            )));
        }
        if address.is_unspecified() || address.is_multicast() {
            return Err(ForgeError::InvalidInput(format!(
                "remote zone {zone:?}: {address} cannot be answered"
            )));
        }
        let mut entry = self
            .zones
            .iter()
            .find(|z| z.name == zone && z.kind == ZoneKind::Remote)
            .cloned()
            .unwrap_or(Zone {
                name: zone.to_owned(),
                ipv4: None,
                ipv6: None,
                kind: ZoneKind::Remote,
            });
        match address {
            IpAddr::V4(v4) => entry.ipv4 = Some(v4),
            IpAddr::V6(v6) => entry.ipv6 = Some(v6),
        }
        self.upsert(entry);
        Ok(())
    }

    fn upsert(&mut self, zone: Zone) {
        self.zones.retain(|z| z.name != zone.name);
        self.zones.push(zone);
        self.zones.sort_by(|a, b| {
            let labels = |z: &Zone| z.name.split('.').count();
            labels(b).cmp(&labels(a)).then_with(|| a.name.cmp(&b.name))
        });
    }

    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    /// Zone names, most specific first.
    pub fn names(&self) -> Vec<String> {
        self.zones.iter().map(|z| z.name.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }

    /// The most specific zone holding `name` (`loja.test`, any case, trailing dot
    /// allowed).
    pub fn find(&self, name: &str) -> Option<&Zone> {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        self.zones
            .iter()
            .find(|z| dns_name::is_within(&name, &z.name))
    }

    /// What `host` should resolve to through this resolver (empty outside the zones).
    pub fn expected(&self, host: &str) -> Vec<IpAddr> {
        self.find(host).map(Zone::addresses).unwrap_or_default()
    }

    /// The answer to `query`.
    pub fn answer(&self, query: &Query) -> Answer {
        if query.query_class() != DNSClass::IN {
            return Answer::refused();
        }
        let qname = query.name();
        let Some(zone) = self.find(&plain_name(qname)) else {
            return Answer {
                code: ResponseCode::NXDomain,
                authoritative: false,
                answers: Vec::new(),
                soa: Vec::new(),
            };
        };
        let mut answers = Vec::new();
        let record = |data: RData| Record::from_rdata(qname.clone(), self.ttl, data);
        let qtype = query.query_type();
        if matches!(qtype, RecordType::A | RecordType::ANY) {
            answers.extend(zone.ipv4.map(|ip| record(RData::A(A(ip)))));
        }
        if matches!(qtype, RecordType::AAAA | RecordType::ANY) {
            answers.extend(zone.ipv6.map(|ip| record(RData::AAAA(AAAA(ip)))));
        }
        let soa = soa_record(zone);
        let is_apex = plain_name(qname) == zone.name;
        if is_apex && matches!(qtype, RecordType::SOA | RecordType::ANY) {
            answers.extend(soa.clone());
        }
        let negative = answers.is_empty();
        Answer {
            code: ResponseCode::NoError,
            authoritative: true,
            answers,
            soa: if negative {
                soa.into_iter().collect()
            } else {
                Vec::new()
            },
        }
    }
}

/// `SOA` of a zone: `ns.forgebox.<zone>. hostmaster.<zone>.`, negative TTL
/// [`NEGATIVE_TTL`]. `None` only for names hickory refuses (not produced by our checks).
fn soa_record(zone: &Zone) -> Option<Record> {
    let apex = Name::from_ascii(format!("{}.", zone.name)).ok()?;
    let mname = Name::from_ascii(format!("ns.forgebox.{}.", zone.name)).ok()?;
    let rname = Name::from_ascii(format!("hostmaster.{}.", zone.name)).ok()?;
    let soa = SOA::new(mname, rname, 1, 3600, 600, 86_400, NEGATIVE_TTL);
    Some(Record::from_rdata(apex, NEGATIVE_TTL, RData::SOA(soa)))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn set() -> ZoneSet {
        let mut set = ZoneSet::local(&[".test".into(), ".localhost".into()]).unwrap();
        set.add_remote("hml.test", "10.0.0.5".parse().unwrap())
            .unwrap();
        set
    }

    fn q(name: &str, qtype: RecordType) -> Query {
        Query::query(Name::from_ascii(name).unwrap(), qtype)
    }

    fn ips(answer: &Answer) -> Vec<IpAddr> {
        answer
            .answers
            .iter()
            .filter_map(|r| r.data.ip_addr())
            .collect()
    }

    #[test]
    fn local_names_resolve_to_loopback() {
        let set = set();
        for name in [
            "loja.test.",
            "a.b.c.test.",
            "test.",
            "LOJA.Test.",
            "app.localhost.",
        ] {
            let a = set.answer(&q(name, RecordType::A));
            assert_eq!(a.code, ResponseCode::NoError, "{name}");
            assert!(a.authoritative);
            assert_eq!(ips(&a), vec![IpAddr::from([127, 0, 0, 1])], "{name}");
            assert_eq!(a.answers[0].ttl, DEFAULT_TTL);
            assert_eq!(a.answers[0].name.to_ascii(), name, "owner keeps the case");
            let aaaa = set.answer(&q(name, RecordType::AAAA));
            assert_eq!(ips(&aaaa), vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]);
            let any = set.answer(&q(name, RecordType::ANY));
            assert_eq!(
                any.answers.len() - usize::from(name.eq_ignore_ascii_case("test.")),
                2
            );
        }
    }

    #[test]
    fn outside_the_zones_is_nxdomain() {
        let set = set();
        for name in ["x.com.", "latest.", "test.com.", "localhost.com."] {
            let a = set.answer(&q(name, RecordType::A));
            assert_eq!(a.code, ResponseCode::NXDomain, "{name}");
            assert!(!a.authoritative);
            assert!(a.answers.is_empty() && a.soa.is_empty());
        }
        assert_eq!(
            ZoneSet::new().answer(&q("loja.test.", RecordType::A)).code,
            ResponseCode::NXDomain
        );
    }

    #[test]
    fn remote_zones_win_and_answer_nodata_for_missing_families() {
        let set = set();
        assert_eq!(set.names()[0], "hml.test", "most specific first");
        let a = set.answer(&q("loja.hml.test.", RecordType::A));
        assert_eq!(ips(&a), vec![IpAddr::from([10, 0, 0, 5])]);
        let aaaa = set.answer(&q("loja.hml.test.", RecordType::AAAA));
        assert_eq!(aaaa.code, ResponseCode::NoError, "NODATA, not NXDOMAIN");
        assert!(aaaa.answers.is_empty());
        assert_eq!(aaaa.soa.len(), 1);
        assert_eq!(aaaa.soa[0].name, Name::from_ascii("hml.test.").unwrap());
        assert_eq!(
            set.expected("x.hml.test"),
            vec![IpAddr::from([10, 0, 0, 5])]
        );
        assert_eq!(
            set.expected("x.test"),
            vec![
                IpAddr::from([127, 0, 0, 1]),
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            ]
        );
        assert!(set.expected("x.com").is_empty());

        let mut both = set.clone();
        both.add_remote("hml.test", "fd00::5".parse().unwrap())
            .unwrap();
        let aaaa = both.answer(&q("loja.hml.test.", RecordType::AAAA));
        assert_eq!(ips(&aaaa), vec!["fd00::5".parse::<IpAddr>().unwrap()]);
        assert_eq!(both.zones().len(), 3, "same zone merged");
    }

    #[test]
    fn other_types_get_nodata_with_soa() {
        let set = set();
        for qtype in [
            RecordType::MX,
            RecordType::TXT,
            RecordType::HTTPS,
            RecordType::NS,
        ] {
            let a = set.answer(&q("loja.test.", qtype));
            assert_eq!(a.code, ResponseCode::NoError, "{qtype}");
            assert!(a.answers.is_empty());
            assert_eq!(a.soa.len(), 1);
            assert_eq!(a.soa[0].ttl, NEGATIVE_TTL);
        }
        let soa = set.answer(&q("test.", RecordType::SOA));
        assert_eq!(soa.answers.len(), 1);
        assert_eq!(soa.answers[0].record_type(), RecordType::SOA);
        let not_apex = set.answer(&q("loja.test.", RecordType::SOA));
        assert!(not_apex.answers.is_empty());
    }

    #[test]
    fn non_internet_classes_are_refused() {
        let mut query = q("version.bind.", RecordType::TXT);
        query.set_query_class(DNSClass::CH);
        assert_eq!(set().answer(&query).code, ResponseCode::Refused);
    }

    #[test]
    fn construction_is_validated() {
        assert!(ZoneSet::local(&["test".into()]).is_err());
        let mut set = ZoneSet::local(&[".test".into()]).unwrap();
        for (zone, ip) in [
            ("hml.example", "10.0.0.5"),
            ("test", "10.0.0.5"),
            ("HML.test", "10.0.0.5"),
            ("hml.test", "0.0.0.0"),
            ("hml.test", "ff02::1"),
        ] {
            assert!(
                set.add_remote(zone, ip.parse().unwrap()).is_err(),
                "{zone} {ip}"
            );
        }
        let mut config = ForgeConfig::default();
        config
            .dns
            .remote_zones
            .push(forge_core::config::RemoteZoneConfig {
                zone: "hml.test".into(),
                address: "10.0.0.5".parse().unwrap(),
            });
        let from_config = ZoneSet::from_config(&config).unwrap();
        assert_eq!(
            from_config.names(),
            vec!["hml.test".to_owned(), "test".to_owned()]
        );
        assert_eq!(from_config.with_ttl(5).ttl(), 5);
        assert!(!set.is_empty());
    }
}
