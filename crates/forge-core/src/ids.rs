//! Identifiers of the domain.
//!
//! Entities are identified by ULIDs (ADR-0009, rule 2: paths are attributes, never keys).
//! Each entity has its own newtype so a `SiteId` can never be passed where an
//! `InstanceId` is expected. All of them serialize as the 26-char Crockford base32
//! string, implement `Display`/`FromStr`, and map to `TEXT` in SQLite.
//!
//! [`ToolId`] is the exception: it is the human slug of a manifest (`php`, `mysql`,
//! `redis-windows`), used in paths (`bin/<tool>/...`) and on the command line.

use std::fmt;
use std::str::FromStr;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Deserializer, Serialize};
use ulid::Ulid;

use crate::error::ForgeError;

/// Error parsing an identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The text is not a valid ULID.
    #[error("invalid {kind} {value:?}: {reason}")]
    InvalidUlid {
        kind: &'static str,
        value: String,
        reason: String,
    },
    /// The text is not a valid tool slug.
    #[error("invalid tool id {value:?}: {reason}")]
    InvalidToolId { value: String, reason: &'static str },
}

impl From<IdError> for ForgeError {
    fn from(err: IdError) -> Self {
        ForgeError::InvalidInput(err.to_string())
    }
}

macro_rules! ulid_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[cfg_attr(feature = "specta", derive(specta::Type))]
        pub struct $name(Ulid);

        impl $name {
            /// Generates a new id (current time + random).
            pub fn new() -> Self {
                Self(Ulid::generate())
            }

            /// Wraps an existing ULID.
            pub const fn from_ulid(ulid: Ulid) -> Self {
                Self(ulid)
            }

            /// The underlying ULID.
            pub const fn ulid(&self) -> Ulid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s).map(Self).map_err(|e| IdError::InvalidUlid {
                    kind: $kind,
                    value: s.to_owned(),
                    reason: e.to_string(),
                })
            }
        }

        impl From<Ulid> for $name {
            fn from(ulid: Ulid) -> Self {
                Self(ulid)
            }
        }

        impl From<$name> for Ulid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.0.to_string()))
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                value
                    .as_str()?
                    .parse()
                    .map_err(|e: IdError| FromSqlError::Other(Box::new(e)))
            }
        }
    };
}

ulid_id!(
    /// An installed version of a tool (`tool_versions` row, `bin/<tool>/<version>`).
    VersionId,
    "version id"
);
ulid_id!(
    /// A service instance of a tool version (`mysql 8.4 "default"`).
    InstanceId,
    "instance id"
);
ulid_id!(
    /// A site (vhost or proxy).
    SiteId,
    "site id"
);
ulid_id!(
    /// A long-running operation reported through `job.*` events (docs/03 §7.4).
    JobId,
    "job id"
);
ulid_id!(
    /// A row of `update_history` (an update or the rollback of one, docs/05 §4).
    UpdateId,
    "update id"
);
ulid_id!(
    /// A persisted or streamed event.
    EventId,
    "event id"
);
ulid_id!(
    /// A row of the `ports` table (logical reservation of a port).
    PortReservationId,
    "port reservation id"
);
ulid_id!(
    /// Idempotency key present in every API operation (ADR-0009, rule 4): a ULID string.
    RequestId,
    "request id"
);
ulid_id!(
    /// A stream subscription of an API connection (`events.subscribe`, `logs.subscribe`,
    /// `metrics.subscribe`); scoped to the connection that opened it.
    SubscriptionId,
    "subscription id"
);
ulid_id!(
    /// An authenticated API connection (returned by `system.hello`).
    SessionId,
    "session id"
);
ulid_id!(
    /// A remote client paired with this daemon (docs/13 §3.2, `forge remote clients`).
    ClientId,
    "client id"
);
ulid_id!(
    /// A terminal session hosted by the daemon (`terminal.*`, ADR-0040): a pseudo-console
    /// that outlives the client that opened it.
    TerminalId,
    "terminal id"
);
ulid_id!(
    /// A writer of a terminal session (`terminal.input`, ADR-0040 §2): generated by the
    /// client once per tab (attachment) and reused across reconnects and retries, unlike the
    /// per-connection `SessionId`. Input is deduplicated per `(terminal, writer)`.
    TerminalWriterId,
    "terminal writer id"
);

/// Maximum length of a [`ToolId`].
pub const TOOL_ID_MAX_LEN: usize = 64;

/// Names that cannot be tool ids because they are directories of the layout (`bin/shims`).
const RESERVED_TOOL_IDS: &[&str] = &["shims", "current"];

/// Slug of a tool manifest: `[a-z0-9-]`, starts and ends with `[a-z0-9]`, at most 64 chars.
///
/// Not a ULID: it is the stable, human id shared by the registry, paths and CLI.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ToolId(String);

impl ToolId {
    /// Validates and wraps `value`.
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        match validate_tool_id(&value) {
            Ok(()) => Ok(Self(value)),
            Err(reason) => Err(IdError::InvalidToolId { value, reason }),
        }
    }

    /// The slug.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_tool_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }
    if value.len() > TOOL_ID_MAX_LEN {
        return Err("must have at most 64 characters");
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("only lowercase letters, digits and '-' are allowed");
    }
    if value.starts_with('-') || value.ends_with('-') {
        return Err("must start and end with a letter or digit");
    }
    if RESERVED_TOOL_IDS.contains(&value) {
        return Err("reserved name");
    }
    Ok(())
}

impl fmt::Display for ToolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ToolId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl AsRef<str> for ToolId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ToolId {
    type Error = IdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ToolId> for String {
    fn from(id: ToolId) -> Self {
        id.0
    }
}

impl<'de> Deserialize<'de> for ToolId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        ToolId::new(value).map_err(serde::de::Error::custom)
    }
}

/// JSON Schema of the slug (feature `schemars`, used by the manifest schemas).
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for ToolId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ToolId".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Tool slug: [a-z0-9-], starts and ends with a letter or digit, at most 64 characters.",
            "type": "string",
            "pattern": "^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$",
            "not": { "enum": RESERVED_TOOL_IDS }
        })
    }
}

impl ToSql for ToolId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.as_str()))
    }
}

impl FromSql for ToolId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        ToolId::new(value.as_str()?).map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_ids_round_trip_display_and_serde() {
        let id = SiteId::new();
        let text = id.to_string();
        assert_eq!(text.len(), 26);
        assert_eq!(text.parse::<SiteId>().unwrap(), id);

        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<SiteId>(&json).unwrap(), id);
    }

    #[test]
    fn ulid_ids_reject_garbage_with_kind_in_message() {
        let err = "not-a-ulid".parse::<InstanceId>().unwrap_err();
        assert!(err.to_string().contains("instance id"), "{err}");
        assert!(serde_json::from_str::<JobId>("\"x\"").is_err());
        let forge: ForgeError = err.into();
        assert_eq!(forge.code(), "invalid_input");
    }

    #[test]
    fn ids_are_distinct_types_and_unique() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a, b);
        let ulid: Ulid = a.into();
        assert_eq!(RequestId::from_ulid(ulid), a);
        assert_eq!(a.ulid(), ulid);
    }

    #[test]
    fn ids_round_trip_through_sqlite() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let id = VersionId::new();
        let tool = ToolId::new("php").unwrap();
        let (got, got_tool): (VersionId, ToolId) = conn
            .query_row("SELECT ?1, ?2", (id, &tool), |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(got, id);
        assert_eq!(got_tool, tool);
        let bad: rusqlite::Result<VersionId> = conn.query_row("SELECT 'zzz'", [], |r| r.get(0));
        assert!(bad.is_err());
    }

    #[test]
    fn tool_id_accepts_slugs() {
        for ok in ["php", "redis-windows", "7zip", "a", "node22"] {
            assert_eq!(ToolId::new(ok).unwrap().as_str(), ok);
        }
    }

    #[test]
    fn tool_id_rejects_invalid() {
        let long = "a".repeat(65);
        for bad in [
            "", "PHP", "php_8", "php.exe", "-php", "php-", "a/b", "a\\b", "..", "shims", "current",
            "café", &long,
        ] {
            assert!(ToolId::new(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn tool_id_json_schema_has_pattern() {
        let schema = schemars::schema_for!(ToolId);
        let value = serde_json::to_value(&schema).unwrap();
        assert_eq!(value["type"], "string");
        assert_eq!(value["pattern"], "^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$");
        assert_eq!(value["not"]["enum"][0], "shims");
    }

    #[test]
    fn tool_id_serde_validates() {
        let id: ToolId = serde_json::from_str("\"mysql\"").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"mysql\"");
        assert!(serde_json::from_str::<ToolId>("\"MySQL\"").is_err());
    }
}
