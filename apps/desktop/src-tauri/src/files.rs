//! Saving a downloaded mail attachment (M2-E3-T13).
//!
//! The webview never picks the destination itself: this command shows the native "save as"
//! dialog and writes the bytes the daemon returned (`mail.attachment`, at most 4 MiB). The file
//! name comes from the message sender, so it only seeds the dialog (reduced to a plain file
//! name); the user's choice in the dialog is the only path ever written.

use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

use forge_ipc::ApiError;

/// Largest base64 text accepted (`MAIL_ATTACHMENT_MAX_BYTES` grows by 4/3 on the wire).
const MAX_BASE64_LEN: usize = 6 * 1024 * 1024;
/// Name offered when the sender's one is empty after cleaning.
const FALLBACK_NAME: &str = "attachment";
/// Longest suggested name, in characters.
const MAX_NAME_CHARS: usize = 120;

/// Reduces a sender-controlled name to a plain Windows file name: no folders, no characters
/// Windows rejects, no control characters, no trailing dots or spaces, not a device name.
pub fn sanitize_file_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .take(MAX_NAME_CHARS)
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        return FALLBACK_NAME.to_owned();
    }
    let stem = trimmed.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit()));
    if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// Decodes the daemon's standard-alphabet base64, refusing oversized input.
pub fn decode_content(content_base64: &str) -> Result<Vec<u8>, ApiError> {
    if content_base64.len() > MAX_BASE64_LEN {
        return Err(ApiError::new(
            "invalid_input",
            "attachment is too large to save",
        ));
    }
    STANDARD
        .decode(content_base64)
        .map_err(|e| ApiError::new("invalid_input", format!("attachment is not base64: {e}")))
}

/// Asks where to save `file_name` and writes `content_base64` there. Resolves the chosen path,
/// or `None` when the user cancelled.
#[tauri::command]
pub async fn save_attachment(
    app: AppHandle,
    file_name: String,
    content_base64: String,
) -> Result<Option<String>, ApiError> {
    let bytes = decode_content(&content_base64)?;
    let suggested = sanitize_file_name(&file_name);
    tokio::task::spawn_blocking(move || {
        let picked = app
            .dialog()
            .file()
            .set_file_name(suggested)
            .blocking_save_file();
        let Some(file) = picked else {
            return Ok(None);
        };
        let path: PathBuf = file
            .into_path()
            .map_err(|e| ApiError::new("invalid_input", format!("unusable path: {e}")))?;
        std::fs::write(&path, bytes)
            .map_err(|e| ApiError::new("io_error", format!("cannot write the file: {e}")))?;
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|e| ApiError::new("internal", format!("save task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_only_a_plain_file_name() {
        assert_eq!(sanitize_file_name("report.pdf"), "report.pdf");
        assert_eq!(sanitize_file_name("..\\..\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_file_name("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_file_name("we?ird:na*me.txt"), "we_ird_na_me.txt");
        assert_eq!(sanitize_file_name("trail. . "), "trail");
    }

    #[test]
    fn sanitize_falls_back_and_avoids_device_names() {
        assert_eq!(sanitize_file_name(""), "attachment");
        assert_eq!(sanitize_file_name("..."), "attachment");
        assert_eq!(sanitize_file_name("dir/"), "attachment");
        assert_eq!(sanitize_file_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_file_name("LPT1"), "_LPT1");
        assert_eq!(sanitize_file_name("console.txt"), "console.txt");
        assert_eq!(
            sanitize_file_name(&"x".repeat(500)).chars().count(),
            MAX_NAME_CHARS
        );
    }

    #[test]
    fn decode_accepts_base64_and_refuses_junk_or_oversize() {
        assert_eq!(decode_content("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(
            decode_content("not base64!").unwrap_err().code,
            "invalid_input"
        );
        let huge = "A".repeat(MAX_BASE64_LEN + 4);
        assert_eq!(decode_content(&huge).unwrap_err().code, "invalid_input");
    }
}
