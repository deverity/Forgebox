//! Parsing of command line values that clap cannot express: durations, instants,
//! `key=value` pairs, paths on the daemon's machine.

use std::path::Path;
use std::time::Duration;

use forge_ipc::base::Timestamp;
use forge_ipc::base::ids::ToolId;
use serde_json::Value;

use crate::error::{CliError, CliResult};

/// `30s`, `10m`, `2h`, `1d`, `90` (seconds).
pub fn duration(text: &str) -> CliResult<Duration> {
    let text = text.trim();
    let (digits, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((at, _)) => text.split_at(at),
        None => (text, "s"),
    };
    let n: u64 = digits.parse().map_err(|_| {
        CliError::usage(format!("duração inválida: {text:?} (use 30s, 10m, 2h, 1d)"))
    })?;
    let factor = match unit {
        "s" | "" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => {
            return Err(CliError::usage(format!(
                "unidade de duração desconhecida em {text:?} (use s, m, h ou d)"
            )));
        }
    };
    Ok(Duration::from_secs(n.saturating_mul(factor)))
}

/// An instant: RFC 3339 (`2026-10-27T12:00:00Z`), `now`, or a duration ago (`24h`).
pub fn instant(text: &str, now: Timestamp) -> CliResult<Timestamp> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("now") {
        return Ok(now);
    }
    if let Ok(ts) = text.parse::<Timestamp>() {
        return Ok(ts);
    }
    let ago = duration(text).map_err(|_| {
        CliError::usage(format!(
            "instante inválido: {text:?} (RFC 3339, `now` ou uma duração atrás: 24h, 7d)"
        ))
    })?;
    now.checked_sub(ago)
        .map_err(|e| CliError::usage(format!("instante fora do intervalo: {e}")))
}

/// `key=value`.
pub fn key_value(text: &str) -> CliResult<(String, String)> {
    match text.split_once('=') {
        Some((key, value)) if !key.trim().is_empty() => {
            Ok((key.trim().to_owned(), value.to_owned()))
        }
        _ => Err(CliError::usage(format!(
            "esperado CHAVE=VALOR, recebi {text:?}"
        ))),
    }
}

/// A value of `forge config set`: JSON when it parses (`true`, `3`, `["a"]`), text otherwise.
pub fn config_value(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

/// `php@8.3` → (`php`, Some(`8.3`)).
pub fn tool_spec(text: &str) -> CliResult<(ToolId, Option<String>)> {
    let (tool, version) = match text.split_once('@') {
        Some((tool, version)) => (tool, Some(version.to_owned())),
        None => (text, None),
    };
    let id = tool
        .parse::<ToolId>()
        .map_err(|e| CliError::usage(format!("ferramenta inválida em {text:?}: {e}")))?;
    Ok((id, version))
}

/// The path as the daemon wants it: absolute, with the `\\?\` prefix removed. The
/// daemon runs on this machine (remote contexts are refused before reaching this).
pub fn absolute(path: &Path) -> CliResult<String> {
    let absolute = std::path::absolute(path)
        .map_err(|e| CliError::usage(format!("{}: {e}", path.display())))?;
    Ok(forge_ipc::base::paths::plain_path(&absolute)
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(duration("1d").unwrap(), Duration::from_secs(86_400));
        assert_eq!(duration("90").unwrap(), Duration::from_secs(90));
        for bad in ["", "m", "10x", "-5s", "1.5h"] {
            assert_eq!(duration(bad).unwrap_err().exit, 2, "{bad:?}");
        }
    }

    #[test]
    fn instants() {
        let now: Timestamp = "2026-10-27T12:00:00Z".parse().unwrap();
        assert_eq!(instant("now", now).unwrap(), now);
        assert_eq!(
            instant("24h", now).unwrap(),
            "2026-10-26T12:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(
            instant("2026-01-01T00:00:00Z", now).unwrap(),
            "2026-01-01T00:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert!(instant("ontem", now).is_err());
    }

    #[test]
    fn key_values_and_config_values() {
        assert_eq!(
            key_value("memory_limit=1G").unwrap(),
            ("memory_limit".into(), "1G".into())
        );
        assert_eq!(key_value("a=").unwrap(), ("a".into(), String::new()));
        assert!(key_value("sem-igual").is_err());
        assert!(key_value("=x").is_err());
        assert_eq!(config_value("true"), Value::Bool(true));
        assert_eq!(config_value("3"), serde_json::json!(3));
        assert_eq!(config_value(".test"), Value::String(".test".into()));
    }

    #[test]
    fn tool_specs() {
        let (id, version) = tool_spec("php@8.3").unwrap();
        assert_eq!((id.as_str(), version.as_deref()), ("php", Some("8.3")));
        assert_eq!(tool_spec("nginx").unwrap().1, None);
        assert!(tool_spec("PHP@8").is_err());
    }

    #[test]
    fn relative_paths_become_absolute() {
        let abs = absolute(Path::new("some/dir")).unwrap();
        assert!(Path::new(&abs).is_absolute(), "{abs}");
        assert!(!abs.starts_with(r"\\?\"));
    }
}
