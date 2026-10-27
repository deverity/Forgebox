//! Global preferences: `etc/forgebox.toml`.
//!
//! The file is 100 % owned by Forgebox (written by Settings and `forge config set`) and
//! carries a header saying so (ADR-0007). Missing keys take the defaults below, so an
//! empty or partial file is valid; unknown keys are ignored (a newer Forgebox may have
//! written them). Defaults come from docs/03 §4 (port ranges), docs/06 (TLD, server, DNS
//! zones), docs/13 §5 (zones of remote daemons), docs/14 (metrics) and docs/15 (case
//! sensitivity).
//!
//! Port ranges are named; manifests refer to them by name (`port_range: php-pools`,
//! `ports[].range: databases`, docs/04). Each range is a list of single ports and
//! `"start-end"` spans:
//!
//! ```toml
//! [ports.ranges]
//! web = [80, 443]
//! php-pools = ["19000-19999"]
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::dns_name;
use crate::error::{ForgeError, IoResultExt, Result};
use crate::ids::ToolId;
use crate::paths::write_atomic;

/// First lines of every `forgebox.toml` written by Forgebox.
pub const CONFIG_HEADER: &str = "\
# forgebox.toml: Forgebox global preferences.
# Managed by Forgebox (Settings, `forge config set`). Comments are not preserved.
";

/// Root of `forgebox.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ForgeConfig {
    /// TLD of local sites, with the leading dot (`.test`). Always lowercase.
    pub tld: String,
    /// Tool id of the front web server that listens on the `web` range (docs/06 §3).
    pub default_server: ToolId,
    /// Parked directories: relative to the root (`www`) or absolute, `/` separators.
    pub parked_dirs: Vec<String>,
    /// Linux parity: case-sensitive directories by default (docs/15, ADR-0010).
    pub case_sensitive: bool,
    pub ports: PortsConfig,
    pub dns: DnsConfig,
    pub metrics: MetricsConfig,
    pub updates: UpdatesConfig,
    pub editor: EditorConfig,
    pub terminal: TerminalConfig,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        Self {
            tld: ".test".to_owned(),
            default_server: ToolId::new("nginx").expect("valid tool id"),
            parked_dirs: vec!["www".to_owned()],
            case_sensitive: true,
            ports: PortsConfig::default(),
            dns: DnsConfig::default(),
            metrics: MetricsConfig::default(),
            updates: UpdatesConfig::default(),
            editor: EditorConfig::default(),
            terminal: TerminalConfig::default(),
        }
    }
}

/// `[ports]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PortsConfig {
    /// Ports never handed out automatically to other owners (Xdebug 9003).
    pub reserved: Vec<u16>,
    /// Named ranges used by the allocator and referenced by manifests. Ranges missing
    /// from the file keep their defaults, so overriding one range never drops another.
    #[serde(deserialize_with = "deserialize_ranges")]
    pub ranges: BTreeMap<String, PortSet>,
}

fn default_ranges() -> BTreeMap<String, PortSet> {
    [
        ("web", "80, 443"),
        ("web-alt", "8080, 8443"),
        ("php-pools", "19000-19999"),
        ("xdebug", "9003"),
        (
            "databases",
            "3306-3399, 5432-5499, 6379-6399, 11211-11299, 27017-27099",
        ),
        ("apps", "3000-3999"),
        ("mail", "1025, 8025"),
        ("storage", "9000, 9001"),
    ]
    .into_iter()
    .map(|(name, spec)| (name.to_owned(), spec.parse().expect("valid default")))
    .collect()
}

fn deserialize_ranges<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, PortSet>, D::Error> {
    let mut ranges = default_ranges();
    ranges.extend(BTreeMap::<String, PortSet>::deserialize(deserializer)?);
    Ok(ranges)
}

impl Default for PortsConfig {
    fn default() -> Self {
        Self {
            reserved: vec![9003],
            ranges: default_ranges(),
        }
    }
}

impl PortsConfig {
    /// The range called `name` (`php-pools`, `databases`...).
    pub fn range(&self, name: &str) -> Option<&PortSet> {
        self.ranges.get(name)
    }

    /// Whether `port` must not be allocated automatically.
    pub fn is_reserved(&self, port: u16) -> bool {
        self.reserved.contains(&port)
    }
}

/// Inclusive span of ports; `start == end` for a single port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortSpan {
    pub start: u16,
    pub end: u16,
}

impl PortSpan {
    pub fn contains(&self, port: u16) -> bool {
        (self.start..=self.end).contains(&port)
    }

    /// Number of ports in the span.
    pub fn len(&self) -> u32 {
        u32::from(self.end) - u32::from(self.start) + 1
    }

    /// Always false: a span has at least one port.
    pub fn is_empty(&self) -> bool {
        false
    }
}

impl fmt::Display for PortSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.start == self.end {
            write!(f, "{}", self.start)
        } else {
            write!(f, "{}-{}", self.start, self.end)
        }
    }
}

impl FromStr for PortSpan {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let parse = |p: &str| -> std::result::Result<u16, String> {
            let port: u16 = p
                .trim()
                .parse()
                .map_err(|_| format!("invalid port {p:?}"))?;
            if port == 0 {
                return Err("port 0 is not allowed".to_owned());
            }
            Ok(port)
        };
        let (start, end) = match s.split_once('-') {
            Some((a, b)) => (parse(a)?, parse(b)?),
            None => {
                let p = parse(s)?;
                (p, p)
            }
        };
        if start > end {
            return Err(format!("range {s:?} has start > end"));
        }
        Ok(Self { start, end })
    }
}

/// Ordered list of spans (a named range). TOML: `[80, 443]`, `["19000-19999"]`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortSet(pub Vec<PortSpan>);

impl PortSet {
    pub fn contains(&self, port: u16) -> bool {
        self.0.iter().any(|s| s.contains(port))
    }

    /// Ports in order (first span first), lazily.
    pub fn ports(&self) -> impl Iterator<Item = u16> + '_ {
        self.0.iter().flat_map(|s| s.start..=s.end)
    }
}

impl FromStr for PortSet {
    type Err = String;

    /// Parses `"80, 443"` or `"3306-3399, 5432-5499"`.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        s.split(',')
            .map(str::parse)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(PortSet)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PortSpanRepr {
    Port(u16),
    Span(String),
}

impl Serialize for PortSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let items: Vec<PortSpanRepr> = self
            .0
            .iter()
            .map(|s| {
                if s.start == s.end {
                    PortSpanRepr::Port(s.start)
                } else {
                    PortSpanRepr::Span(s.to_string())
                }
            })
            .collect();
        items.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PortSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let items = Vec::<PortSpanRepr>::deserialize(deserializer)?;
        items
            .into_iter()
            .map(|item| match item {
                PortSpanRepr::Port(0) => Err("port 0 is not allowed".to_owned()),
                PortSpanRepr::Port(p) => Ok(PortSpan { start: p, end: p }),
                PortSpanRepr::Span(s) => s.parse(),
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(PortSet)
            .map_err(serde::de::Error::custom)
    }
}

/// `[dns]` (docs/06 §5, docs/13 §5): what the local resolver answers besides `tld`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DnsConfig {
    /// More suffixes answered with `127.0.0.1`/`::1` and routed to the resolver
    /// (`[".localhost"]`), same syntax as `tld`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_tlds: Vec<String>,
    /// Zones of remote daemons (docs/13 §5): every name under `zone` (`hml.test`, below one
    /// of the TLDs) resolves to `address` (`10.0.0.5`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remote_zones: Vec<RemoteZoneConfig>,
}

/// One entry of `[[dns.remote_zones]]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteZoneConfig {
    /// Zone name without dots around it, lowercase (`hml.test`).
    pub zone: String,
    /// Address of the remote machine (IPv4 answers `A`, IPv6 answers `AAAA`).
    pub address: IpAddr,
}

/// `[metrics]` (docs/14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    pub enabled: bool,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// `[updates]` (docs/05 §4): background checks respect these.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatesConfig {
    /// What to do on a metered connection.
    pub metered: MeteredPolicy,
    /// Window in which background updates do not run. `None`: no quiet hours.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<QuietHours>,
}

/// Local-time window `"HH:MM"`..`"HH:MM"`; may wrap midnight (`22:00`..`07:00`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietHours {
    pub start: String,
    pub end: String,
}

impl QuietHours {
    /// Whether `minute_of_day` (0..1440, local time) falls in the window.
    pub fn contains(&self, minute_of_day: u16) -> Result<bool> {
        let start = parse_hhmm(&self.start)?;
        let end = parse_hhmm(&self.end)?;
        Ok(if start <= end {
            (start..end).contains(&minute_of_day)
        } else {
            minute_of_day >= start || minute_of_day < end
        })
    }
}

fn parse_hhmm(value: &str) -> Result<u16> {
    let invalid = || ForgeError::ConfigInvalid(format!("time {value:?} must be HH:MM"));
    let (h, m) = value.split_once(':').ok_or_else(invalid)?;
    if h.len() != 2 || m.len() != 2 {
        return Err(invalid());
    }
    let h: u16 = h.parse().map_err(|_| invalid())?;
    let m: u16 = m.parse().map_err(|_| invalid())?;
    if h > 23 || m > 59 {
        return Err(invalid());
    }
    Ok(h * 60 + m)
}

/// Behaviour of background downloads on metered connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeteredPolicy {
    /// Do not download in the background while metered (default).
    #[default]
    Pause,
    /// Download anyway.
    Allow,
}

/// `[editor]`: command used by "open in editor". `None`: auto-detect.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// `[terminal]`: external terminal command. `None`: auto-detect.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl ForgeConfig {
    /// Every suffix answered by the local resolver: `tld` then `[dns] extra_tlds`, without
    /// duplicates (`[".test"]` by default).
    pub fn tlds(&self) -> Vec<String> {
        let mut out = vec![self.tld.clone()];
        for extra in &self.dns.extra_tlds {
            if !out.contains(extra) {
                out.push(extra.clone());
            }
        }
        out
    }

    /// Reads `path`; a missing file yields the defaults. The result is validated.
    ///
    /// Blocking file I/O: call from `spawn_blocking` in async code.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ForgeError::io_at("reading", path, e)),
        };
        let config = Self::from_toml(&text)
            .map_err(|e| ForgeError::ConfigInvalid(format!("{}: {e}", path.display())))?;
        Ok(config)
    }

    /// Parses and validates TOML text (a leading UTF-8 BOM is tolerated).
    pub fn from_toml(text: &str) -> Result<Self> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let config: Self =
            toml::from_str(text).map_err(|e| ForgeError::ConfigInvalid(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Serializes to TOML with [`CONFIG_HEADER`]; LF, UTF-8 without BOM.
    pub fn to_toml(&self) -> Result<String> {
        let body = toml::to_string(self).map_err(|e| ForgeError::ConfigInvalid(e.to_string()))?;
        Ok(format!("{CONFIG_HEADER}\n{body}"))
    }

    /// Validates and writes atomically (temporary file + rename).
    ///
    /// Blocking file I/O: call from `spawn_blocking` in async code.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).at("creating", dir)?;
        }
        write_atomic(path, self.to_toml()?.as_bytes())
    }

    /// Checks values that serde cannot express.
    pub fn validate(&self) -> Result<()> {
        let invalid = |msg: String| Err(ForgeError::ConfigInvalid(msg));
        for tld in std::iter::once(&self.tld).chain(&self.dns.extra_tlds) {
            if let Err(reason) = dns_name::parse_suffix(tld) {
                return invalid(format!(
                    "tld {tld:?} must look like \".test\" (lowercase, leading dot): {reason}"
                ));
            }
        }
        let tlds = self.tlds();
        for remote in &self.dns.remote_zones {
            if let Err(reason) = dns_name::check_name(&remote.zone) {
                return invalid(format!("dns remote zone: {reason}"));
            }
            let under_tld = tlds.iter().any(|tld| {
                let tld = &tld[1..];
                remote.zone != tld && dns_name::is_within(&remote.zone, tld)
            });
            if !under_tld {
                return invalid(format!(
                    "dns remote zone {:?} must be below one of the TLDs {tlds:?} (like \"hml.test\")",
                    remote.zone
                ));
            }
            if remote.address.is_unspecified() || remote.address.is_multicast() {
                return invalid(format!(
                    "dns remote zone {:?}: address {} cannot be answered",
                    remote.zone, remote.address
                ));
            }
        }
        if self.parked_dirs.iter().any(|d| d.trim().is_empty()) {
            return invalid("parked_dirs must not contain empty entries".to_owned());
        }
        for (name, set) in &self.ports.ranges {
            if ToolId::new(name.as_str()).is_err() {
                return invalid(format!(
                    "port range name {name:?} must use [a-z0-9-] (like \"php-pools\")"
                ));
            }
            if set.0.is_empty() {
                return invalid(format!("port range {name:?} is empty"));
            }
        }
        if self.ports.reserved.contains(&0) {
            return invalid("port 0 cannot be reserved".to_owned());
        }
        if let Some(q) = &self.updates.quiet_hours {
            parse_hhmm(&q.start)?;
            parse_hhmm(&q.end)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn defaults_match_docs() {
        let c = ForgeConfig::default();
        assert_eq!(c.tld, ".test");
        assert_eq!(c.default_server.as_str(), "nginx");
        assert_eq!(c.parked_dirs, vec!["www".to_owned()]);
        assert!(c.case_sensitive);
        assert!(c.metrics.enabled);
        assert_eq!(c.updates.metered, MeteredPolicy::Pause);
        assert_eq!(c.updates.quiet_hours, None);
        assert_eq!(c.editor.command, None);
        assert_eq!(c.terminal.command, None);
        assert_eq!(c.dns, DnsConfig::default());
        assert_eq!(c.tlds(), vec![".test".to_owned()]);

        let p = &c.ports;
        let web: Vec<u16> = p.range("web").unwrap().ports().collect();
        assert_eq!(web, vec![80, 443]);
        let alt: Vec<u16> = p.range("web-alt").unwrap().ports().collect();
        assert_eq!(alt, vec![8080, 8443]);
        let pools = p.range("php-pools").unwrap();
        assert!(pools.contains(19000) && pools.contains(19999) && !pools.contains(20000));
        assert_eq!(pools.0[0].len(), 1000);
        assert!(p.is_reserved(9003));
        assert!(p.range("xdebug").unwrap().contains(9003));
        let dbs = p.range("databases").unwrap();
        for port in [3306, 3307, 5432, 6379, 11211, 27017] {
            assert!(dbs.contains(port), "{port}");
        }
        assert_eq!(dbs.ports().next(), Some(3306));
        let apps = p.range("apps").unwrap();
        assert!(apps.contains(3000) && apps.contains(3999));
        let mail: Vec<u16> = p.range("mail").unwrap().ports().collect();
        assert_eq!(mail, vec![1025, 8025]);
        let storage: Vec<u16> = p.range("storage").unwrap().ports().collect();
        assert_eq!(storage, vec![9000, 9001]);
        c.validate().unwrap();
    }

    #[test]
    fn round_trip_through_toml() {
        let mut c = ForgeConfig {
            tld: ".localhost".into(),
            case_sensitive: false,
            ..ForgeConfig::default()
        };
        c.parked_dirs.push("D:/clientes".into());
        c.updates.quiet_hours = Some(QuietHours {
            start: "22:00".into(),
            end: "07:00".into(),
        });
        c.updates.metered = MeteredPolicy::Allow;
        c.editor.command = Some("code".into());
        let text = c.to_toml().unwrap();
        assert!(text.starts_with(CONFIG_HEADER));
        assert!(!text.contains('\r'));
        assert!(text.contains("web = [80, 443]"), "{text}");
        assert!(text.contains("php-pools = [\"19000-19999\"]"), "{text}");
        assert_eq!(ForgeConfig::from_toml(&text).unwrap(), c);
    }

    #[test]
    fn partial_file_takes_defaults_and_ignores_unknown_keys() {
        let c = ForgeConfig::from_toml(
            "tld = \".dev\"\nfuture_key = 1\n[ports.ranges]\napps = [\"4000-4099\"]\n",
        )
        .unwrap();
        assert_eq!(c.tld, ".dev");
        assert!(c.case_sensitive);
        assert!(c.ports.range("apps").unwrap().contains(4050));
        assert!(!c.ports.range("apps").unwrap().contains(3000));
        assert_eq!(
            c.ports.range("web"),
            ForgeConfig::default().ports.range("web"),
            "ranges missing from the file keep their defaults"
        );
        assert_eq!(c.metrics, MetricsConfig::default());
    }

    #[test]
    fn invalid_values_are_rejected() {
        for bad in [
            "tld = \"test\"",
            "tld = \".TEST\"",
            "tld = \".\"",
            "tld = \".loja..test\"",
            "default_server = \"Nginx\"",
            "[ports.ranges]\nweb = [0]",
            "[ports.ranges]\nweb = [\"90-80\"]",
            "[ports.ranges]\nweb = [\"abc\"]",
            "[ports.ranges]\nWeb = [80]",
            "[ports.ranges]\nweb = []",
            "[updates]\nquiet_hours = { start = \"25:00\", end = \"07:00\" }",
            "[updates]\nmetered = \"sometimes\"",
            "case_sensitive = \"yes\"",
        ] {
            let err = ForgeConfig::from_toml(bad).unwrap_err();
            assert_eq!(err.code(), "config_invalid", "{bad}: {err}");
        }
    }

    #[test]
    fn dns_section_round_trips_and_validates() {
        let c = ForgeConfig::from_toml(
            "[dns]\nextra_tlds = [\".localhost\", \".test\"]\n\
             [[dns.remote_zones]]\nzone = \"hml.test\"\naddress = \"10.0.0.5\"\n",
        )
        .unwrap();
        assert_eq!(c.tlds(), vec![".test".to_owned(), ".localhost".to_owned()]);
        assert_eq!(c.dns.remote_zones[0].zone, "hml.test");
        assert_eq!(c.dns.remote_zones[0].address, IpAddr::from([10, 0, 0, 5]));
        let text = c.to_toml().unwrap();
        assert!(text.contains("[[dns.remote_zones]]"), "{text}");
        assert_eq!(ForgeConfig::from_toml(&text).unwrap(), c);
        assert!(
            !ForgeConfig::default()
                .to_toml()
                .unwrap()
                .contains("remote_zones"),
            "empty lists are not written"
        );

        for bad in [
            "[dns]\nextra_tlds = [\"localhost\"]",
            "[dns]\nextra_tlds = [\".Local\"]",
            "[[dns.remote_zones]]\nzone = \"test\"\naddress = \"10.0.0.5\"",
            "[[dns.remote_zones]]\nzone = \"hml.example\"\naddress = \"10.0.0.5\"",
            "[[dns.remote_zones]]\nzone = \"HML.test\"\naddress = \"10.0.0.5\"",
            "[[dns.remote_zones]]\nzone = \"hml.test\"\naddress = \"0.0.0.0\"",
            "[[dns.remote_zones]]\nzone = \"hml.test\"\naddress = \"224.0.0.1\"",
            "[[dns.remote_zones]]\nzone = \"hml.test\"\naddress = \"nope\"",
        ] {
            let err = ForgeConfig::from_toml(bad).unwrap_err();
            assert_eq!(err.code(), "config_invalid", "{bad}: {err}");
        }
    }

    #[test]
    fn quiet_hours_wrap_midnight() {
        let q = QuietHours {
            start: "22:00".into(),
            end: "07:00".into(),
        };
        assert!(q.contains(23 * 60).unwrap());
        assert!(q.contains(6 * 60 + 59).unwrap());
        assert!(!q.contains(12 * 60).unwrap());
        let day = QuietHours {
            start: "09:30".into(),
            end: "18:00".into(),
        };
        assert!(day.contains(9 * 60 + 30).unwrap());
        assert!(!day.contains(18 * 60).unwrap());
    }

    #[test]
    fn load_or_default_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("etc").join("forgebox.toml");
        assert_eq!(
            ForgeConfig::load_or_default(&path).unwrap(),
            ForgeConfig::default()
        );

        let c = ForgeConfig {
            default_server: ToolId::new("apache").unwrap(),
            ..ForgeConfig::default()
        };
        c.save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "no BOM");
        assert!(!bytes.contains(&b'\r'), "LF only");
        assert_eq!(ForgeConfig::load_or_default(&path).unwrap(), c);

        std::fs::write(&path, "tld = 3").unwrap();
        let err = ForgeConfig::load_or_default(&path).unwrap_err();
        assert_eq!(err.code(), "config_invalid");
        assert!(err.to_string().contains("forgebox.toml"), "{err}");
    }

    #[test]
    fn bom_is_tolerated_on_read() {
        let c = ForgeConfig::from_toml("\u{feff}tld = \".test\"\n").unwrap();
        assert_eq!(c.tld, ".test");
    }
}
