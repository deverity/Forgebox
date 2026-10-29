//! Configuration rendering on behalf of the other components (docs/03 §3):
//!
//! * [`base_context`]: the [`RenderContext`] of an installed version (paths, tool, prefs
//!   from `forgebox.toml`, the `php` model when the manifest declares
//!   `config.extensions`).
//! * [`apply_target`]: renders the `forge-render` target of a tool (when there is one)
//!   into `etc/`, with the quick settings written into the rendered files (or into their
//!   overlay / `settings` block when no target renders them, ADR-0042), only touching
//!   managed blocks and generated files, and publishes `config.rendered` /
//!   `config.drift_detected` (docs/03 §8). Files the user edited inside a managed block
//!   are kept (drift), never overwritten here.
//! * [`DaemonRenderer`]: the [`TemplateRenderer`] of the layout engine
//!   (`post_install[].render`), over the same engine.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use forge_core::config::ForgeConfig;
use forge_core::error::{IoResultExt, Result};
use forge_core::event::{EventBus, EventKind};
use forge_core::paths::{ForgeRoot, posix_string};
use forge_core::store::ToolVersionRow;
use forge_ipc::api::ConfigFilePayload;
use forge_manifest::schema::tool::ToolManifest;
use forge_manifest::version::Version;
use forge_provider::layout::{RenderRequest, TemplateRenderer};
use forge_registry::Registry;
use forge_render::managed::{CommentStyle, WriteOutcome, with_generated_header, write_atomic};
use forge_render::settings::SettingsFile;
use forge_render::targets;
use forge_render::{Engine, PhpModel, RenderContext};

use crate::catalog;

/// Version of the `config.*` payloads.
pub const CONFIG_EVENT_VERSION: u32 = 1;

/// Context of `row` (an installed version of `manifest`) for `root` and `config`.
pub fn base_context(
    root: &ForgeRoot,
    config: &ForgeConfig,
    manifest: &ToolManifest,
    row: &ToolVersionRow,
) -> RenderContext {
    let mut ctx = RenderContext::new(root, manifest.metadata.id.as_str(), &row.version);
    ctx.tool.dir = posix_string(Path::new(&row.path));
    ctx.prefs.tld = config.tld.trim_start_matches('.').to_owned();
    ctx.php = php_model(manifest, row);
    ctx
}

/// The PHP model of a version whose manifest declares `config.extensions`
/// (`defaults_on`, gated `zend_extensions`); `None` otherwise.
pub fn php_model(manifest: &ToolManifest, row: &ToolVersionRow) -> Option<PhpModel> {
    let extensions = manifest.spec.config.extensions.as_ref()?;
    let scheme = manifest.spec.versioning.scheme;
    let zend = Version::parse(scheme, &row.version)
        .map(|v| extensions.zend_extensions_for(scheme, &v))
        .unwrap_or_default();
    Some(PhpModel {
        series: catalog::series_of(manifest, &row.version),
        extensions: extensions.defaults_on.clone(),
        zend_extensions: zend,
        settings: BTreeMap::new(),
        xdebug: None,
    })
}

/// Leaves out of `ctx.php.extensions` the ones whose file (`config.extensions.dir` +
/// `pattern`) is not in `version_dir`: compiled into that build (PHP 8.4 for Windows
/// has `bcmath` built in, so `extension = bcmath` would only print a startup warning)
/// or not shipped. Blocking (file checks): call from `spawn_blocking`.
pub fn keep_present_extensions(
    manifest: &ToolManifest,
    version_dir: &Path,
    ctx: &mut RenderContext,
) {
    let (Some(config), Some(php)) = (manifest.spec.config.extensions.as_ref(), ctx.php.as_mut())
    else {
        return;
    };
    php.extensions.retain(|name| {
        let present = config.file_of(version_dir, name).is_file();
        if !present {
            tracing::debug!(
                extension = %name,
                dir = %version_dir.display(),
                "extension file not shipped (built in or absent): left out of the overlay"
            );
        }
        present
    });
}

/// Files written (or found identical) by [`apply_target`].
#[derive(Debug, Clone, Default)]
pub struct Applied {
    pub written: Vec<String>,
    pub unchanged: usize,
    pub drifted: Vec<String>,
}

impl Applied {
    pub fn changed(&self) -> bool {
        !self.written.is_empty()
    }
}

fn fs_path(root: &ForgeRoot, path: &Path) -> Option<String> {
    path.strip_prefix(root.path())
        .ok()
        .map(posix_string)
        .filter(|p| p.starts_with("etc/"))
}

/// Renders the target of `ctx.tool.id` into `etc/` (blocking file I/O: call from
/// `spawn_blocking`) with the quick `settings` of the tool written in: into the rendered
/// file they belong to, else into their overlay or a `settings` block of their file
/// ([`forge_render::settings`]). Tools without a target only get their settings files.
/// `owner` names what the files configure in the events (`nginx`, `php@8.3`).
pub fn apply_target(
    root: &ForgeRoot,
    engine: &Engine,
    bus: &EventBus,
    ctx: &RenderContext,
    owner: &str,
    settings: &[SettingsFile],
) -> Result<Applied> {
    let etc = root.etc();
    let mut files = match targets::by_id(&ctx.tool.id) {
        Some(target) => {
            let files = target.render(engine, ctx)?;
            target.ensure_dirs(&etc)?;
            files
        }
        None => Vec::new(),
    };
    let rest = forge_render::settings::apply_to_rendered(&mut files, &etc, settings)?;
    for index in rest {
        files.push(forge_render::settings::standalone(&settings[index], &etc)?);
    }
    if files.is_empty() {
        return Ok(Applied::default());
    }
    let (outcomes, drifts) = targets::apply_all(&files, &etc, false)?;
    let mut applied = Applied::default();
    for (relative, outcome) in outcomes {
        let path = etc.join(&relative);
        let payload = ConfigFilePayload {
            v: CONFIG_EVENT_VERSION,
            owner: owner.to_owned(),
            path: posix_string(&path),
            fs_path: fs_path(root, &path),
        };
        match outcome {
            WriteOutcome::Written => {
                applied.written.push(posix_string(&relative));
                bus.publish(
                    EventKind::ConfigRendered,
                    serde_json::to_value(&payload).unwrap_or_default(),
                );
            }
            WriteOutcome::Unchanged => applied.unchanged += 1,
            WriteOutcome::DriftKept(_) => {}
        }
    }
    for drift in drifts {
        let payload = ConfigFilePayload {
            v: CONFIG_EVENT_VERSION,
            owner: owner.to_owned(),
            path: posix_string(&drift.file),
            fs_path: fs_path(root, &drift.file),
        };
        tracing::warn!(file = %drift.file.display(), block = %drift.block, "managed block edited by the user; kept");
        applied.drifted.push(posix_string(&drift.file));
        bus.publish(
            EventKind::ConfigDriftDetected,
            serde_json::to_value(&payload).unwrap_or_default(),
        );
    }
    Ok(applied)
}

/// [`TemplateRenderer`] of the layout engine: renders `post_install[].render` with the
/// embedded templates (or `etc/templates` overrides) and the version's context.
pub struct DaemonRenderer {
    root: ForgeRoot,
    engine: Engine,
    registry: Registry,
    config: Arc<dyn Fn() -> Arc<ForgeConfig> + Send + Sync>,
}

impl DaemonRenderer {
    pub fn new(
        root: ForgeRoot,
        engine: Engine,
        registry: Registry,
        config: Arc<dyn Fn() -> Arc<ForgeConfig> + Send + Sync>,
    ) -> Self {
        Self {
            root,
            engine,
            registry,
            config,
        }
    }
}

impl TemplateRenderer for DaemonRenderer {
    fn render(&self, request: &RenderRequest) -> Result<()> {
        let config = (self.config)();
        let vars = &request.vars;
        let row = ToolVersionRow {
            id: forge_core::ids::VersionId::new(),
            tool_id: request.tool.clone(),
            version: vars.version.clone(),
            canonical_name: vars.canonical_name.clone(),
            variant: vars.variant.clone(),
            path: vars.paths.version.clone(),
            state: forge_core::store::VersionState::Installed,
            source: forge_core::store::VersionSource::Registry,
        };
        let mut ctx = match catalog::tool(&self.registry, &request.tool) {
            Some(entry) => {
                let mut ctx = base_context(&self.root, &config, &entry.manifest, &row);
                keep_present_extensions(&entry.manifest, &request.files_dir, &mut ctx);
                ctx
            }
            None => {
                let mut ctx = RenderContext::new(&self.root, request.tool.as_str(), &vars.version);
                ctx.tool.dir = vars.paths.version.clone();
                ctx
            }
        };
        if let Some(php) = ctx.php.as_mut() {
            php.series = vars.series.clone();
        }
        let body = self.engine.render(&request.template, &ctx.normalized()?)?;
        // Same comment style as the target that renders this template
        // later (php/php.ini.j2 → generated, `;` comments); otherwise generated with `#`.
        let style = targets::by_id(request.tool.as_str())
            .and_then(|t| t.files.iter().find(|f| f.template == request.template))
            .map_or(CommentStyle::Hash, |f| f.style);
        if let Some(dir) = request.dest.parent() {
            std::fs::create_dir_all(dir).at("creating", dir)?;
        }
        write_atomic(&request.dest, &with_generated_header(style, &body)).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::ids::{ToolId, VersionId};
    use forge_core::store::{VersionSource, VersionState};

    fn php_manifest() -> ToolManifest {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/tools/php.yaml"),
        )
        .unwrap();
        match forge_manifest::load_str(&text).unwrap() {
            forge_manifest::Manifest::Tool(t) => t,
            _ => unreachable!(),
        }
    }

    fn row(version: &str) -> ToolVersionRow {
        ToolVersionRow {
            id: VersionId::new(),
            tool_id: ToolId::new("php").unwrap(),
            version: version.into(),
            canonical_name: format!("{version}-nts-vs17-x64"),
            variant: BTreeMap::new(),
            path: format!("C:/fb/bin/php/{version}-nts-vs17-x64"),
            state: VersionState::Installed,
            source: VersionSource::Registry,
        }
    }

    #[test]
    fn php_model_comes_from_the_manifest_extensions() {
        let manifest = php_manifest();
        let old = php_model(&manifest, &row("8.4.27")).unwrap();
        assert_eq!(old.series, "8.4");
        assert_eq!(old.zend_extensions, ["opcache"]);
        assert!(old.extensions.contains(&"mbstring".to_owned()));
        let new = php_model(&manifest, &row("8.5.1")).unwrap();
        assert!(new.zend_extensions.is_empty(), "8.5 embeds OPcache");
    }

    #[test]
    fn built_in_extensions_are_left_out_of_the_overlay() {
        // PHP 8.4 for Windows ships no php_bcmath.dll (bcmath is built in): the overlay
        // must not ask for it (M1 gate: a startup warning on every CLI run).
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let version_dir = dir.path().join("bin/php/8.4.27-nts-vs17-x64");
        std::fs::create_dir_all(version_dir.join("ext")).unwrap();
        for name in ["curl", "mbstring"] {
            std::fs::write(version_dir.join(format!("ext/php_{name}.dll")), b"").unwrap();
        }
        let manifest = php_manifest();
        let mut row = row("8.4.27");
        row.path = posix_string(&version_dir);
        let mut ctx = base_context(&root, &ForgeConfig::default(), &manifest, &row);
        assert!(
            ctx.php
                .as_ref()
                .unwrap()
                .extensions
                .contains(&"bcmath".to_owned())
        );
        keep_present_extensions(&manifest, &version_dir, &mut ctx);
        assert_eq!(ctx.php.unwrap().extensions, ["curl", "mbstring"]);
    }

    #[test]
    fn php_target_renders_into_etc_with_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let manifest = php_manifest();
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let ctx = base_context(&root, &ForgeConfig::default(), &manifest, &row("8.4.27"));
        let applied =
            apply_target(&root, &Engine::embedded_only(), &bus, &ctx, "php@8.4", &[]).unwrap();
        assert_eq!(applied.written, ["php/8.4/conf.d/10-forgebox.ini"]);
        let text =
            std::fs::read_to_string(root.etc().join("php/8.4/conf.d/10-forgebox.ini")).unwrap();
        assert!(text.contains("zend_extension = opcache"));
        assert!(!text.contains('\r'));
        let event = events.try_recv().unwrap();
        assert_eq!(event.kind, EventKind::ConfigRendered);
        assert_eq!(
            event.payload["fs_path"],
            "etc/php/8.4/conf.d/10-forgebox.ini"
        );
        let again =
            apply_target(&root, &Engine::embedded_only(), &bus, &ctx, "php@8.4", &[]).unwrap();
        assert!(!again.changed());
    }

    #[test]
    fn settings_go_to_the_overlay_and_into_rendered_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let root = ForgeRoot::new(dir.path()).unwrap();
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let engine = Engine::embedded_only();
        let manifest = php_manifest();
        let row = row("8.4.27");
        let ctx = base_context(&root, &ForgeConfig::default(), &manifest, &row);
        let stored = BTreeMap::from([("memory_limit".to_owned(), "1G".to_owned())]);
        let vars = crate::vars::RuntimeVars::new(forge_provider::layout::TemplateVars {
            tool: "php".into(),
            version: row.version.clone(),
            series: "8.4".into(),
            canonical_name: row.canonical_name.clone(),
            variant: BTreeMap::new(),
            paths: forge_provider::layout::PathVars::new(&root, Path::new(&row.path)),
        });
        let plans = crate::settings::plans(&manifest, &stored, &vars).unwrap();
        // The user's php.ini, never touched.
        let user_ini = root.etc().join("php/8.4/php.ini");
        std::fs::create_dir_all(user_ini.parent().unwrap()).unwrap();
        std::fs::write(&user_ini, "; mine\r\nmemory_limit=64M\r\n").unwrap();
        let applied = apply_target(&root, &engine, &bus, &ctx, "php@8.4", &plans).unwrap();
        assert_eq!(
            applied.written,
            [
                "php/8.4/conf.d/10-forgebox.ini",
                "php/8.4/conf.d/90-forgebox-settings.ini"
            ]
        );
        assert_eq!(
            std::fs::read(&user_ini).unwrap(),
            b"; mine\r\nmemory_limit=64M\r\n"
        );
        let overlay =
            std::fs::read_to_string(root.etc().join("php/8.4/conf.d/90-forgebox-settings.ini"))
                .unwrap();
        assert!(overlay.starts_with("; generated by forgebox"), "{overlay}");
        assert!(overlay.contains("\nmemory_limit = 1G\n"), "{overlay}");
        let kinds: Vec<EventKind> = std::iter::from_fn(|| events.try_recv())
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            [EventKind::ConfigRendered, EventKind::ConfigRendered]
        );

        // A tool without a render target: a `settings` block in its own file, the user's
        // lines around it byte for byte.
        let mut other = php_manifest();
        other.metadata.id = ToolId::new("fake-db").unwrap();
        let file = &mut other.spec.config.files[0];
        file.overlay = None;
        file.path = "{{ paths.etc }}/fake/my.ini".into();
        let plans = crate::settings::plans(&other, &stored, &vars).unwrap();
        let own = root.etc().join("fake/my.ini");
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, "[client]\r\nport=1\r\n").unwrap();
        let mut ctx = ctx.clone();
        ctx.tool.id = "fake-db".into();
        apply_target(&root, &engine, &bus, &ctx, "fake-db/default", &plans).unwrap();
        let text = std::fs::read_to_string(&own).unwrap();
        assert!(
            text.starts_with("[client]\r\nport=1\r\n\n; forgebox:begin settings (hash="),
            "{text}"
        );
        assert!(text.contains("\nmemory_limit = 1G\n"), "{text}");
    }
}
