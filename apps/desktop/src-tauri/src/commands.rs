//! App-level Tauri commands (build facts). Daemon commands live in `bridge::commands`.

use forge_ipc::ApiError;
use serde::Serialize;

/// Static facts about this build, shown by the shell and used by smoke tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppInfo {
    /// Product name.
    pub name: &'static str,
    /// Version of the desktop crate (same as the workspace).
    pub version: &'static str,
    /// Daemon API version this GUI was built against (`forge_ipc::API_VERSION`).
    pub api_version: String,
}

/// Facts about this build; does not touch the daemon.
pub fn build_info() -> AppInfo {
    AppInfo {
        name: "Forgebox",
        version: env!("CARGO_PKG_VERSION"),
        api_version: forge_ipc::API_VERSION.to_string(),
    }
}

#[tauri::command]
pub async fn app_info() -> AppInfo {
    build_info()
}

/// Largest file `save_text_file` writes (a support bundle is a few hundred KiB).
const SAVE_TEXT_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Writes `contents` to `path`, chosen by the user in the dialog plugin's "Save as" (the webview
/// has no file system access). Only absolute `.json` paths are accepted, so a compromised page
/// cannot use it to drop executables or scripts. The write is atomic (temporary file + rename)
/// and runs in `spawn_blocking`.
pub fn write_text_file(path: &std::path::Path, contents: &str) -> Result<(), ApiError> {
    let json = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
    if !path.is_absolute() || !json || path.file_name().is_none() {
        return Err(ApiError::new(
            "invalid_input",
            format!("{} is not an absolute .json path", path.display()),
        ));
    }
    if contents.len() > SAVE_TEXT_MAX_BYTES {
        return Err(ApiError::new("invalid_input", "the file is too large"));
    }
    let io = |e: std::io::Error| ApiError::new("io", format!("{}: {e}", path.display()));
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".forgebox-tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, contents).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(e)
    })
}

#[tauri::command]
pub async fn save_text_file(path: String, contents: String) -> Result<(), ApiError> {
    tokio::task::spawn_blocking(move || write_text_file(std::path::Path::new(&path), &contents))
        .await
        .map_err(|e| ApiError::new("internal", format!("save_text_file: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_reports_product_and_api_version() {
        let info = build_info();
        assert_eq!(info.name, "Forgebox");
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.api_version, forge_ipc::API_VERSION.to_string());
    }

    #[test]
    fn app_info_serializes_with_snake_case_fields() {
        let json = serde_json::to_value(build_info()).unwrap();
        assert_eq!(json["name"], "Forgebox");
        assert!(json["api_version"].as_str().unwrap().contains('.'));
    }

    #[test]
    fn write_text_file_writes_json_atomically() {
        let dir = std::env::temp_dir().join(format!("forgebox-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.JSON");
        write_text_file(&path, "{\"v\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"v\":1}");
        assert!(!dir.join("report.JSON.forgebox-tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_text_file_refuses_other_extensions_relative_paths_and_huge_content() {
        let dir = std::env::temp_dir();
        for bad in [
            dir.join("run.bat"),
            dir.join("noext"),
            "relative.json".into(),
        ] {
            let err = write_text_file(&bad, "x").unwrap_err();
            assert_eq!(err.code, "invalid_input", "{}", bad.display());
        }
        let huge = "x".repeat(SAVE_TEXT_MAX_BYTES + 1);
        assert!(write_text_file(&dir.join("big.json"), &huge).is_err());
    }
}
