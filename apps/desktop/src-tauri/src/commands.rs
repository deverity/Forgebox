//! App-level Tauri commands (build facts). Daemon commands live in `bridge::commands`.

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
}
