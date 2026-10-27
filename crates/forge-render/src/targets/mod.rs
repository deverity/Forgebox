//! Render targets: which files each known tool gets and where (docs/03 §3.2).
//!
//! A [`Target`] is a static table of [`FileSpec`]s. It maps the neutral model to
//! templates and output paths and contains no per-tool logic; the per-tool knowledge is
//! the template text. The tables live in one module per known server
//! ([`nginx`], [`apache`], [`caddy`], [`php`], [`mysql`], [`mariadb`], [`postgresql`],
//! [`mailpit`]).
//!
//! Today the tables are Rust constants because M1-E1 (manifest schema) is not merged yet.
//! The intended end state is that a manifest's `config.files` declares them (template,
//! output path, scope, block name) and this module disappears; the [`FileSpec`] fields
//! are shaped for that move.

use std::path::{Path, PathBuf};

use forge_core::error::{ForgeError, IoResultExt, Result};
use serde::Serialize;

use crate::drift::Drift;
use crate::engine::Engine;
use crate::managed::{
    CommentStyle, WriteOutcome, render_block, with_generated_header, write_atomic, write_block_file,
};
use crate::model::{RenderContext, UpstreamModel, VhostModel};

pub mod apache;
pub mod caddy;
pub mod mailpit;
pub mod mariadb;
pub mod mysql;
pub mod nginx;
pub mod php;
pub mod postgresql;

/// How a file is owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// 100 % generated: header + content, rewritten as a whole.
    Generated,
    /// Shared with the user: content lives in a managed block with this name.
    Managed(&'static str),
}

/// How many files a [`FileSpec`] expands to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// One file.
    Global,
    /// One file per site; `{kind}` in `template` becomes `php`, `static` or `proxy`.
    Site,
    /// One file per site that has TLS.
    SiteSsl,
    /// One file per PHP pool.
    Pool,
}

/// One output file of a target.
#[derive(Debug, Clone, Copy)]
pub struct FileSpec {
    /// How many files this spec produces.
    pub scope: Scope,
    /// Embedded template name (`nginx/nginx.conf.j2`).
    pub template: &'static str,
    /// Output path relative to `etc/`; placeholders `{host}`, `{pool}`, `{series}`,
    /// `{instance}`.
    pub out: &'static str,
    /// Generated file or managed block.
    pub ownership: Ownership,
    /// Comment syntax of the format.
    pub style: CommentStyle,
    /// JSON pointer into the serialized [`RenderContext`] that must be present and
    /// truthy for the file to be rendered (`/php/xdebug`).
    pub when: Option<&'static str>,
}

/// A tool Forgebox knows how to configure.
#[derive(Debug)]
pub struct Target {
    /// Tool id, as in the registry (`nginx`).
    pub id: &'static str,
    /// Files rendered, in order.
    pub files: &'static [FileSpec],
    /// Directories under `etc/` that must exist even when no file lands in them yet
    /// (Apache fails on `IncludeOptional dir/*.conf` when `dir` itself is missing).
    pub dirs: &'static [&'static str],
}

/// Per-file outcomes and the drifts found by an apply (callers publish
/// `config.drift_detected` for each drift).
pub type ApplyReport = (Vec<(PathBuf, WriteOutcome)>, Vec<Drift>);

/// A rendered file, not yet written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFile {
    /// Path relative to `etc/`, `/`-separated.
    pub path: String,
    /// Rendered template output (no header, no block markers).
    pub body: String,
    /// Ownership.
    pub ownership: Ownership,
    /// Comment syntax.
    pub style: CommentStyle,
}

impl RenderedFile {
    /// Full content of the file when it does not exist yet: the generated header plus the
    /// body, or just the managed block.
    pub fn new_file_content(&self) -> Result<String> {
        match self.ownership {
            Ownership::Generated => Ok(with_generated_header(self.style, &self.body)),
            Ownership::Managed(name) => render_block(self.style, name, &self.body),
        }
    }

    /// Writes the file under `etc` (blocking I/O). Generated files are rewritten when
    /// different; managed files only touch their block. A user-edited block is kept unless
    /// `force`, and reported as [`WriteOutcome::DriftKept`].
    pub fn apply(&self, etc: &Path, force: bool) -> Result<WriteOutcome> {
        let path = etc.join(&self.path);
        match self.ownership {
            Ownership::Generated => write_atomic(&path, &self.new_file_content()?),
            Ownership::Managed(name) => {
                write_block_file(&path, self.style, name, &self.body, force)
            }
        }
    }
}

#[derive(Serialize)]
struct Scoped<'a> {
    #[serde(flatten)]
    ctx: &'a RenderContext,
    site: Option<&'a VhostModel>,
    upstream: Option<&'a UpstreamModel>,
}

fn truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null | serde_json::Value::Bool(false) => false,
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

struct Names<'a> {
    host: Option<&'a str>,
    pool: Option<&'a str>,
    series: Option<&'a str>,
    instance: &'a str,
}

fn expand(pattern: &str, names: &Names<'_>) -> Result<String> {
    let mut out = pattern.to_owned();
    for (key, value) in [
        ("{host}", names.host),
        ("{pool}", names.pool),
        ("{series}", names.series),
        ("{instance}", Some(names.instance).filter(|s| !s.is_empty())),
    ] {
        if out.contains(key) {
            let value = value.ok_or_else(|| {
                ForgeError::InvalidInput(format!("'{pattern}' needs a value for {key}"))
            })?;
            out = out.replace(key, value);
        }
    }
    Ok(out)
}

impl Target {
    /// Renders, creates [`Target::dirs`] and writes everything under `etc`; returns the
    /// per-file outcomes and the drifts found (blocking I/O).
    pub fn apply(
        &self,
        engine: &Engine,
        ctx: &RenderContext,
        etc: &Path,
        force: bool,
    ) -> Result<ApplyReport> {
        let files = self.render(engine, ctx)?;
        self.ensure_dirs(etc)?;
        apply_all(&files, etc, force)
    }

    /// Creates [`Target::dirs`] under `etc` (blocking I/O).
    pub fn ensure_dirs(&self, etc: &Path) -> Result<()> {
        for dir in self.dirs {
            let path = etc.join(dir);
            std::fs::create_dir_all(&path).at("creating", &path)?;
        }
        Ok(())
    }

    /// Renders every file of this target for `ctx`.
    ///
    /// The context is validated first ([`RenderContext::normalized`]): hosts are
    /// lowercased, paths made POSIX, unknown pools rejected.
    pub fn render(&self, engine: &Engine, ctx: &RenderContext) -> Result<Vec<RenderedFile>> {
        let ctx = ctx.normalized()?;
        let ctx_json = serde_json::to_value(&ctx)
            .map_err(|e| ForgeError::Internal(format!("serializing context: {e}")))?;
        let series = ctx.php.as_ref().map(|p| p.series.as_str());
        let mut out = Vec::new();
        for spec in self.files {
            if let Some(pointer) = spec.when {
                if !ctx_json.pointer(pointer).is_some_and(truthy) {
                    continue;
                }
            }
            let base = Names {
                host: None,
                pool: None,
                series,
                instance: &ctx.instance.name,
            };
            let mut push = |site: Option<&VhostModel>,
                            upstream: Option<&UpstreamModel>,
                            template: String,
                            names: Names<'_>|
             -> Result<()> {
                let scoped = Scoped {
                    ctx: &ctx,
                    site,
                    upstream,
                };
                let body = engine.render(&template, &scoped)?;
                out.push(RenderedFile {
                    path: expand(spec.out, &names)?,
                    body,
                    ownership: spec.ownership,
                    style: spec.style,
                });
                Ok(())
            };
            match spec.scope {
                Scope::Global => push(None, None, spec.template.to_owned(), base)?,
                Scope::Site => {
                    for site in &ctx.sites {
                        let template = spec.template.replace("{kind}", site.kind.template_kind());
                        let names = Names {
                            host: Some(site.primary_host()),
                            ..base
                        };
                        push(Some(site), None, template, names)?;
                    }
                }
                Scope::SiteSsl => {
                    for site in ctx.sites.iter().filter(|s| s.ssl.is_some()) {
                        let names = Names {
                            host: Some(site.primary_host()),
                            ..base
                        };
                        push(Some(site), None, spec.template.to_owned(), names)?;
                    }
                }
                Scope::Pool => {
                    for pool in &ctx.pools {
                        let names = Names {
                            pool: Some(&pool.name),
                            ..base
                        };
                        push(None, Some(pool), spec.template.to_owned(), names)?;
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Every known target.
pub fn all() -> [&'static Target; 8] {
    [
        &nginx::TARGET,
        &apache::TARGET,
        &caddy::TARGET,
        &php::TARGET,
        &mysql::TARGET,
        &mariadb::TARGET,
        &postgresql::TARGET,
        &mailpit::TARGET,
    ]
}

/// Target of a tool id.
pub fn by_id(id: &str) -> Option<&'static Target> {
    all().into_iter().find(|t| t.id == id)
}

/// Writes all `files` under `etc`; returns `(relative path, outcome)` per file and the
/// drifts found (callers publish `config.drift_detected` for each).
pub fn apply_all(files: &[RenderedFile], etc: &Path, force: bool) -> Result<ApplyReport> {
    let mut outcomes = Vec::with_capacity(files.len());
    let mut drifts = Vec::new();
    for f in files {
        let outcome = f.apply(etc, force)?;
        if let WriteOutcome::DriftKept(d) = &outcome {
            drifts.push(d.clone());
        }
        outcomes.push((PathBuf::from(&f.path), outcome));
    }
    Ok((outcomes, drifts))
}
