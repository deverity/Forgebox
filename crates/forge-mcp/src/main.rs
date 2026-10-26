//! Forgebox MCP server (forge-mcp): exposes the daemon API to AI agents and IDEs.
//!
//! Placeholder: prints name and version and exits 0 until its roadmap task.

const BIN_NAME: &str = "forge-mcp";

fn banner() -> String {
    format!("{BIN_NAME} {}", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::banner;

    #[test]
    fn banner_has_name_and_version() {
        assert_eq!(banner(), format!("forge-mcp {}", env!("CARGO_PKG_VERSION")));
    }
}
