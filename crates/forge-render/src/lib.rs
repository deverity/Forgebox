//! Forgebox config renderer: minijinja templates, managed blocks and drift detection.
//!
//! Crate of the Forgebox workspace, see docs/02-project-structure.md and
//! docs/03-engine.md §3.
//!
//! - [`model`]: server-neutral input (`VhostModel`, `UpstreamModel`, `RenderContext`...).
//! - [`engine`]: minijinja environment with embedded templates and `etc/templates/` overrides.
//! - [`managed`]: `# forgebox:begin/end` blocks, text normalization (LF, UTF-8, no BOM).
//! - [`drift`]: detection of user edits inside managed blocks.
//! - [`targets`]: which files each known tool gets and where.
//!
//! Everything is blocking CPU/file work: the daemon calls it from `spawn_blocking`.

pub mod drift;
pub mod engine;
pub mod managed;
pub mod model;
pub mod targets;

pub use drift::Drift;
pub use engine::Engine;
pub use managed::{CommentStyle, WriteOutcome};
pub use model::{
    ExtraModel, InstanceModel, PathsModel, PhpModel, PrefsModel, ProxyModel, RenderContext,
    SiteKind, SslModel, StaticLocation, ToolModel, UpstreamModel, UpstreamServer, VhostModel,
    XdebugModel,
};
pub use targets::{Ownership, RenderedFile, Target};

#[cfg(test)]
mod tests {
    #[test]
    fn package_name_matches_directory() {
        assert_eq!(env!("CARGO_PKG_NAME"), "forge-render");
    }
}
