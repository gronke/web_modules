//! Build-script helper: **build** a complete static frontend into an output dir for
//! embedding (`include_dir!`): vendored `web_modules/`, transformed TypeScript,
//! compiled SCSS, copied static files, and a rendered `index.html`.
//!
//! Call from a consumer `build.rs` (with web_modules as a `build-dependency`). The fluent `Build`
//! builder (feature `builder`, on by default) is the recommended wrapper; [`build`] over a
//! [`BuildOptions`] is the borrowed low-level form it wraps:
//!
//! ```no_run
//! use std::path::PathBuf;
//! use web_modules::build::{build, BuildOptions};
//! use web_modules::vendor::PackageSpec;
//!
//! # fn main() -> web_modules::Result<()> {
//! let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dist");
//! build(&BuildOptions {
//!     specs: &[PackageSpec::npm("lit", "^3")],
//!     roots: &[PathBuf::from("web")],
//!     out: &out,
//!     mount: "/web_modules",
//!     html: "<!doctype html>{importmap}<script type=module src=/app.js></script>",
//!     template: None,
//!     processors: Default::default(),
//!     output: Default::default(),
//! })?;
//! # Ok(()) }
//! ```

use std::path::{Path, PathBuf};

use super::steps;
use crate::vendor::{self, PackageSpec};
use crate::{Error, Result};

/// Inputs for [`build`].
pub struct BuildOptions<'a> {
    /// Packages to vendor into `<out>/web_modules/`. **Empty ⇒ no vendoring** — the
    /// source tree is just compiled statically, exactly as the dev server serves a
    /// non-vendored tree.
    pub specs: &'a [PackageSpec],
    /// Source root(s). Two sources claiming one output path fail the build unless
    /// [`Processors::skip_duplicates`] is set, which keeps the first root's file —
    /// the order the dev server overlays roots in. Usually one (the source
    /// directory); pass `std::slice::from_ref(&dir)` for the single-root case.
    pub roots: &'a [PathBuf],
    /// Output directory (e.g. `$OUT_DIR/dist`), **replaced atomically** by each
    /// build. Must be absent, empty, or a previous build's output (marked with
    /// `.web-modules-out`); anything else is refused rather than deleted, so a
    /// mistyped `--out` cannot destroy a directory this tool did not produce.
    pub out: &'a Path,
    /// URL prefix the vendored modules are served at (e.g. `"/web_modules"`).
    pub mount: &'a str,
    /// `index.html` template; the literal `{importmap}` is replaced with the generated
    /// `<script type="importmap">…</script>`. Used **only as a fallback** for `index.html`
    /// when the source tree didn't already produce one (e.g. via an `index.html.tera`).
    pub html: &'a str,
    /// Optional Tera template file for `index.html`, rendered with an `importmap`
    /// variable instead of `html` when `Some` (same fallback rule as `html`). Requires
    /// the `tera` feature.
    pub template: Option<&'a Path>,
    /// Which source processors run (TypeScript, SCSS, Tera) and their tuning — the
    /// static-build counterpart of the dev server's processor set.
    pub processors: Processors,
    /// Output optimization: minify the emitted JS and/or write `.gz` sidecars.
    pub output: Output,
}

/// Which source processors run, and their tuning — the static-build counterpart of the
/// dev server's processor set, so `build` and `dev` stay in lock-step.
///
/// `#[non_exhaustive]`, so new processors don't break callers: build from
/// [`Processors::default`] and adjust fields. The defaults mirror the `default` Cargo
/// features — TypeScript, SCSS and Tera all on. A field has effect only when its Cargo
/// feature is compiled in (e.g. `scss` needs the `scss` feature).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Processors {
    /// Transform TypeScript / modern JS → browser JS. Default on.
    pub typescript: bool,
    /// Compile SCSS → CSS. Default on.
    pub scss: bool,
    /// Render `*.tera` to their stripped targets (`index.html.tera` → `index.html`).
    /// Default on.
    pub tera: bool,
    /// Decorator lowering for the TypeScript transform. Defaults to [`Decorators::Lit`];
    /// inert unless the `typescript` processor runs.
    ///
    /// [`Decorators::Lit`]: crate::Decorators::Lit
    pub ts_decorators: crate::Decorators,
    /// Extra SCSS `@use`/`@import` load paths, on top of the source roots and `out`.
    pub extra_scss_load_paths: Vec<PathBuf>,
    /// Paths to keep out of the output and out of serving — config / secrets / source-code.
    /// Defaults to all presets. See [`Reject`](crate::reject::Reject).
    pub reject: crate::reject::Reject,
    /// Allow duplicate output paths (default off). `build` then keeps the
    /// highest-precedence source for each contested path — earlier root first, then a
    /// Tera template over a literal file over a transformed sibling — instead of
    /// failing; the dev server stops warning about the conflicts it would otherwise
    /// report. A source-tree policy like `reject`, shared by `build` and `dev`.
    pub skip_duplicates: bool,
}

impl Default for Processors {
    fn default() -> Self {
        Self {
            typescript: true,
            scss: true,
            tera: true,
            ts_decorators: crate::Decorators::Lit,
            extra_scss_load_paths: Vec::new(),
            reject: crate::reject::Reject::all(),
            skip_duplicates: false,
        }
    }
}

/// Output-optimization toggles. Each processor applies what it can: TS minifies,
/// SCSS already emits compressed, static assets gzip-only. Both default off, so an
/// unset `BuildOptions { .. }` leaves the output unoptimized.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct Output {
    /// Minify the emitted JS (via the TS compile's output; see [`crate::typescript`]).
    pub minify: bool,
    /// Write `<file>.gz` sidecars for servable assets. Requires the `compress` feature.
    pub gzip: bool,
}

impl Output {
    /// Construct from explicit toggles. Use this (or [`Output::default`] for both off and
    /// [`Output::optimized`] for both on) since `Output` is `#[non_exhaustive]` and so can't be
    /// built field-by-field from other crates — including this crate's own `web-modules` binary,
    /// which maps the CLI's `--minify`/`--gzip` flags through here.
    pub fn new(minify: bool, gzip: bool) -> Self {
        Self { minify, gzip }
    }

    /// The production preset: minify the emitted JS **and** write `.gz` sidecars (both
    /// toggles on). Equivalent to `Output::new(true, true)`. Takes full effect with the
    /// `minify` and `compress` features.
    pub fn optimized() -> Self {
        Self::new(true, true)
    }
}

/// Servable extensions the gzip pass writes `.gz` sidecars for — also the set the
/// sidecar reservation guards, so the two can never disagree.
const GZIP_EXTS: [&str; 5] = ["js", "css", "html", "json", "svg"];

/// Emit a `cargo:rerun-if-changed` line — but only when running as a build script, which cargo
/// signals by setting `OUT_DIR`. Outside a build script (e.g. the `web-modules build` subcommand
/// reusing this pipeline) the directive is meaningless and would just spew one line per source
/// file to the CLI's stdout, so it's suppressed.
fn rerun_if_changed(path: &Path) {
    if std::env::var_os("OUT_DIR").is_some() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// Vendor + transform + compile + render into `out`, ready to embed and serve.
/// In a build script (cargo sets `OUT_DIR`) also emits `cargo:rerun-if-changed` for the source tree.
///
/// The build is staged: everything lands in a fresh sibling directory, which then
/// atomically replaces `out`. A reused output directory therefore cannot retain
/// anything from a previous build — no stale module escapes validation, no dropped
/// vendored package keeps shipping — and a failed build leaves the previous output
/// untouched. `out` must be absent, empty, or a previous build's output (recognized
/// by its `.web-modules-out` marker); the build refuses to replace a directory it
/// cannot prove it produced, which is what makes `--out .` safe to mistype.
pub fn build(opts: &BuildOptions<'_>) -> Result<()> {
    let out = std::path::absolute(opts.out)?;
    ensure_replaceable(&out)?;
    let stage = sibling(&out, "stage")?;
    let old = sibling(&out, "old")?;
    // Leftovers of a crashed earlier build; clear before staging anew.
    remove_dir_all_if_present(&stage)?;
    remove_dir_all_if_present(&old)?;
    std::fs::create_dir_all(&stage)?;
    if let Err(e) = build_into(&stage, &out, opts) {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(e);
    }
    // The swap: retire the previous output, promote the stage, drop the old tree.
    if out.exists() {
        std::fs::rename(&out, &old)?;
    }
    std::fs::rename(&stage, &out)?;
    remove_dir_all_if_present(&old)?;
    Ok(())
}

/// The marker each build writes into its output root — how [`build`] recognizes a
/// directory it may replace. Content: the crate version that produced it.
const OUT_MARKER: &str = ".web-modules-out";

/// The one vendored package that comes from the pipeline itself rather than
/// `BuildOptions::specs`: the oxc transform-helper runtime.
const OXC_RUNTIME_PACKAGE: &str = "@oxc-project/runtime";

/// `out` may be replaced only when this build can own it: absent, empty, or marked as
/// a previous build's output. Anything else is someone else's directory — the current
/// dir under `--out .`, a shared `OUT_DIR` — and replacing it would delete files this
/// tool never wrote.
fn ensure_replaceable(out: &Path) -> Result<()> {
    match std::fs::metadata(out) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(Error::Build(format!(
                "web-modules: --out {} exists and is not a directory",
                out.display()
            )))
        }
    }
    if out.join(OUT_MARKER).exists() || std::fs::read_dir(out)?.next().is_none() {
        return Ok(());
    }
    Err(Error::Build(format!(
        "web-modules: {} contains files web-modules did not produce - refusing to \
         replace it; pass an absent or empty --out, or delete the directory once \
         (each build marks its output with {OUT_MARKER})",
        out.display()
    )))
}

/// `.<name>.web-modules-<suffix>` next to `dir` — same filesystem, so the promoting
/// rename is atomic.
fn sibling(dir: &Path, suffix: &str) -> Result<PathBuf> {
    let name = dir.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        Error::Build(format!(
            "web-modules: --out {} has no usable directory name",
            dir.display()
        ))
    })?;
    Ok(dir.with_file_name(format!(".{name}.web-modules-{suffix}")))
}

fn remove_dir_all_if_present(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Seed the stage's vendor dir from the previous output, so npm-utils' cache markers
/// keep matching and an unchanged package costs no re-download. Regular files are
/// hardlinked (cheap; vendored content is only ever replaced, never rewritten in
/// place); the dot-named cache markers are byte-copied, because the vendorer rewrites
/// them in place on a version change and must not reach back into the retired tree
/// through a shared inode. Packages the current build does not request are pruned
/// after vendoring ([`vendor::prune`]).
fn seed_vendor_cache(previous: &Path, stage: &Path) -> Result<()> {
    if !previous.is_dir() {
        return Ok(());
    }
    // Links are not followed: a marked output contains none this tool produced.
    for entry in walkdir::WalkDir::new(previous) {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(previous) else {
            continue;
        };
        let dest = stage.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else if entry.file_type().is_file() {
            let dotted = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if dotted || std::fs::hard_link(path, &dest).is_err() {
                std::fs::copy(path, &dest)?;
            }
        }
    }
    Ok(())
}

/// One staged build: the whole pipeline, writing into `stage`; `previous` (the
/// current `out`, if any) only seeds the vendor cache. The caller promotes the stage
/// on success and removes it on failure.
fn build_into(stage: &Path, previous: &Path, opts: &BuildOptions<'_>) -> Result<()> {
    // Marks the output as replaceable by the next build; written first so even an
    // interrupted stage is recognizable.
    std::fs::write(
        stage.join(OUT_MARKER),
        concat!(env!("CARGO_PKG_VERSION"), "\n"),
    )?;

    // TypeScript transform options — only when the `typescript` processor is compiled in. Without
    // it, `build` doesn't touch `.ts` files at all: they're skipped like any source file (never
    // transformed, never copied raw), exactly as `dev` serves a tree with TS off.
    #[cfg(feature = "typescript")]
    let transpile = crate::typescript::TranspileOptions {
        minify: opts.output.minify,
        decorators: opts.processors.ts_decorators,
        ..Default::default()
    };

    // SCSS `@use`/`@import` load paths span every source root (matching the dev server),
    // plus the stage (so vendored stylesheets under `<out>/web_modules` resolve) and
    // any explicit `--scss-load-path`.
    #[cfg(feature = "scss")]
    let scss_load_paths: Vec<PathBuf> = {
        let mut paths: Vec<PathBuf> = opts.roots.to_vec();
        paths.push(stage.to_path_buf());
        paths.extend(opts.processors.extra_scss_load_paths.iter().cloned());
        paths
    };

    // One walk of the source roots: every enabled step states what it would emit, so
    // duplicate output paths are caught before anything is written and each output
    // path is then written exactly once, by its winner.
    let steps = steps::enabled_steps(
        &opts.processors,
        steps::StepConfig {
            #[cfg(feature = "typescript")]
            transpile,
            #[cfg(feature = "scss")]
            scss_load_paths,
        },
    );
    let preflights: Vec<&dyn steps::Preflight> = steps
        .iter()
        .map(|step| step.as_ref() as &dyn steps::Preflight)
        .collect();
    let report = steps::preflight(opts.roots, &preflights, &opts.processors.reject);

    // A walk problem means the preflight may be incomplete — surface it instead of
    // silently building from a partial picture (a dangling link, an unreadable dir).
    for error in report.walk_errors() {
        crate::static_files::build_warning(&format!("web-modules: preflight: {error}"));
    }

    // Sources may only come from inside their root: a file that canonically resolves
    // elsewhere (a symlink out of the tree) is refused outright — the dev server's
    // canonical containment already refuses to serve such a path, and publishing what
    // dev refuses would make `build` the wider gate.
    let escaping_sources = report.escaping_sources();
    if !escaping_sources.is_empty() {
        let lines = escaping_sources
            .iter()
            .map(|source| {
                format!(
                    "  {} -> {}",
                    opts.roots[source.root].join(&source.rel).display(),
                    source.target.display(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::Build(format!(
            "web-modules: {} source path(s) resolve outside the source root - remove \
             the symlink or move the files into the tree:\n{lines}",
            escaping_sources.len()
        )));
    }

    // Writes may only land inside the output directory: a claim whose target is not a
    // purely normal relative path is refused outright, before anything is written.
    let escaping = report.escaping();
    if !escaping.is_empty() {
        let lines = escaping
            .iter()
            .map(|claim| {
                format!(
                    "  {} (from {}, {})",
                    claim.out_rel.display(),
                    opts.roots[claim.root].join(&claim.rel).display(),
                    steps[claim.step].name(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::Build(format!(
            "web-modules: {} output path(s) would escape the output directory:\n{lines}",
            escaping.len()
        )));
    }

    // Duplicate output paths fail the build before the first write, naming every
    // claimant; `--skip-duplicates` opts into precedence instead — earlier root first,
    // then a Tera template over a literal file over a transformed sibling, the order
    // the dev server resolves a request in.
    let conflicts = report.conflicts();
    if !conflicts.is_empty() && !opts.processors.skip_duplicates {
        let mut lines = Vec::new();
        for conflict in &conflicts {
            lines.push(format!("  {}:", conflict.out_rel.display()));
            for (i, claim) in conflict.claimants.iter().enumerate() {
                let wins = if i == 0 {
                    " - wins with --skip-duplicates"
                } else {
                    ""
                };
                lines.push(format!(
                    "    {} ({}){wins}",
                    opts.roots[claim.root].join(&claim.rel).display(),
                    steps[claim.step].name(),
                ));
            }
        }
        let noun = if conflicts.len() == 1 {
            "path is"
        } else {
            "paths are"
        };
        return Err(Error::Build(format!(
            "web-modules: {} output {noun} claimed by more than one source - remove \
             the duplicates or pass --skip-duplicates:\n{}",
            conflicts.len(),
            lines.join("\n")
        )));
    }

    // The pipeline's own writes are reserved: the standalone import map, the vendored
    // `web_modules/` subtree, and (with gzip on) the `.gz` sidecar of every
    // gzip-eligible output. A source claiming one of those paths would corrupt a
    // generated artifact in whichever order the two writes happen, so it is a hard
    // error — `--skip-duplicates` arbitrates source-against-source precedence and
    // never lets a source replace generated metadata. The fallback `index.html` is
    // the deliberate exception, modelled the other way around: it is synthesised only
    // when no source claims that target, so a source page always wins.
    let winners = report.winners();
    let violations: Vec<String> = winners
        .iter()
        .filter_map(|winner| {
            let out_rel = winner.out_rel.as_path();
            let reserved_for = if out_rel == Path::new("importmap.json") {
                Some("the generated import map".to_string())
            } else if out_rel == Path::new(OUT_MARKER) {
                Some("the output marker".to_string())
            } else if out_rel.starts_with("web_modules") {
                Some("the vendored modules directory (web_modules/)".to_string())
            } else if cfg!(feature = "compress")
                && opts.output.gzip
                && out_rel.extension().and_then(|e| e.to_str()) == Some("gz")
            {
                let inner = out_rel.with_extension("");
                let eligible = inner
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| GZIP_EXTS.contains(&e));
                let written = report.claims_target(&inner)
                    || inner == Path::new("importmap.json")
                    || inner == Path::new("index.html");
                (eligible && written).then(|| format!("the gzip sidecar of {}", inner.display()))
            } else {
                None
            };
            reserved_for.map(|what| {
                format!(
                    "  {} ({}) claims {} - reserved for {what}",
                    opts.roots[winner.root].join(&winner.rel).display(),
                    steps[winner.step].name(),
                    out_rel.display(),
                )
            })
        })
        .collect();
    if !violations.is_empty() {
        return Err(Error::Build(format!(
            "web-modules: {} output path(s) are reserved for generated files:\n{}",
            violations.len(),
            violations.join("\n")
        )));
    }

    // Warm the vendor cache from the previous output before vendoring resolves
    // against it; packages this build no longer requests are pruned again below.
    seed_vendor_cache(&previous.join("web_modules"), &stage.join("web_modules"))?;

    // Vendor only when there are packages to vendor. A non-vendored source tree (no
    // specs and no `--manifest`) just compiles statically — the same thing the dev
    // server does serving such a tree, only emitted instead of served.
    let mut importmap = if opts.specs.is_empty() {
        crate::importmap::Importmap::new()
    } else {
        vendor::vendor(&stage.join("web_modules"), opts.mount, opts.specs)?
    };

    // Emit every non-Tera winner exactly once, feeding the module graph as each file
    // is written. Tera waits: its templates receive the import map, which is final
    // only after helper vendoring below.
    let mut graph = crate::module_graph::ModuleGraph::new();
    let mut tera_winners = Vec::new();
    for winner in winners {
        if steps[winner.step].rank() == steps::Rank::Tera {
            tera_winners.push(winner);
            continue;
        }
        emit_winner(&steps, winner, opts, stage, &importmap, &mut graph)?;
    }

    // Vendor the transform-runtime helpers the graph shows were injected (the decorator
    // helper, etc.) — even a non-vendored build may need these. Tera renders later, so
    // a runtime import appearing only in rendered JS is not auto-vendored — it surfaces
    // in the unresolved check below instead.
    let runtime_vendored = graph.uses_runtime_helpers();
    if runtime_vendored {
        importmap.extend(vendor_transform_runtime(stage, opts.mount)?);
    }

    // Drop vendored packages this build did not request — the seed may carry
    // packages whose spec was removed since the previous build.
    let extra_vendored: &[&str] = if runtime_vendored {
        &[OXC_RUNTIME_PACKAGE]
    } else {
        &[]
    };
    vendor::prune(&stage.join("web_modules"), opts.specs, extra_vendored)?;

    // Emit the import map as a standalone artifact too, so test harnesses (and
    // es-module-shims / an external `<script type="importmap" src>`) can consume it.
    importmap.write_to(&stage.join("importmap.json"))?;

    // Render the Tera winners, with the now-final import map exposed as the
    // `importmap` template variable — the static counterpart of the dev server's
    // on-the-fly `.tera` rendering.
    for winner in tera_winners {
        emit_winner(&steps, winner, opts, stage, &importmap, &mut graph)?;
    }

    // Entry-page fallback: synthesise `index.html` from `--template` / inline `--html`
    // only when no source claims that target (a literal root `index.html`, or an
    // `index.html.tera` when tera runs) — in effect a synthetic claim at the lowest
    // precedence, keyed off the preflight rather than probing the stage.
    if !report.claims_target("index.html") {
        let html = match opts.template {
            Some(template) => {
                rerun_if_changed(template);
                render_template(template, &importmap)?
            }
            None => opts.html.replace("{importmap}", &importmap.to_script_tag()),
        };
        std::fs::write(stage.join("index.html"), html)?;
    }

    // Fail the build if any emitted module imports a bare specifier the generated
    // import map can't resolve (a transform runtime helper, a forgotten dependency,
    // …) — so a browser-load failure becomes a clear build error instead. This still
    // applies to a non-vendored build: importing a bare specifier you didn't vendor
    // is a real error. The generated map is the only validation target: the build
    // never reads a page back, so a hand-authored `index.html` owns its inline map.
    let unresolved = graph.unresolved(&importmap);
    if !unresolved.is_empty() {
        let details = unresolved
            .iter()
            .map(|(file, spec)| format!("  {file}: import \"{spec}\""))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::Build(format!(
            "web-modules: {} unresolved bare import(s) - add them to the vendored \
             specs / import map:\n{details}",
            unresolved.len()
        )));
    }

    #[cfg(feature = "compress")]
    if opts.output.gzip {
        crate::compress::gzip_dir(stage, &GZIP_EXTS)?;
    }

    // Re-run the build script when any source file changes. A bare `rerun-if-changed`
    // on the directory only catches add/remove (the directory's own mtime), not edits
    // to existing files — which would leave an *embedded* build serving stale assets —
    // so the preflight's walk feeds every visited entry through (directories included,
    // to catch add/remove).
    for path in report.walked_paths() {
        rerun_if_changed(path);
    }
    Ok(())
}

/// Emit one preflight winner through its claiming step into `out` (the stage) —
/// parent directories created, the emitted imports recorded in the module graph
/// under the output-relative path.
fn emit_winner(
    steps: &[Box<dyn steps::Step>],
    winner: &steps::ClaimRecord,
    opts: &BuildOptions<'_>,
    out: &Path,
    importmap: &crate::importmap::Importmap,
    graph: &mut crate::module_graph::ModuleGraph,
) -> Result<()> {
    let src = opts.roots[winner.root].join(&winner.rel);
    let dest = out.join(&winner.out_rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let emitted =
        steps[winner.step].emit(&steps::EmitCx { importmap }, &src, &winner.rel, &dest)?;
    if let Some(imports) = emitted.imports {
        graph.insert(winner.out_rel.clone(), imports);
    }
    Ok(())
}

/// npm `@oxc-project/runtime` range; tracks the `oxc_*` crate version.
const OXC_RUNTIME_RANGE: &str = "^0.137";

/// Vendor the oxc transform runtime (`@oxc-project/runtime`) so the helper imports the
/// transform injected — e.g. the legacy-decorator `@oxc-project/runtime/helpers/decorate`
/// — resolve. `build` calls this only when the module graph shows a helper was injected
/// (`ModuleGraph::uses_runtime_helpers`), so it vendors unconditionally here.
pub fn vendor_transform_runtime(out: &Path, mount: &str) -> Result<crate::importmap::Importmap> {
    vendor::vendor(
        &out.join("web_modules"),
        mount,
        &[PackageSpec::npm(OXC_RUNTIME_PACKAGE, OXC_RUNTIME_RANGE)],
    )
}

/// Render `index.html` from a Tera `template`, exposing the import-map script tag
/// as an `importmap` variable.
#[cfg(feature = "tera")]
fn render_template(template: &Path, importmap: &crate::importmap::Importmap) -> Result<String> {
    let mut ctx = crate::templates::Context::new();
    ctx.insert("importmap", &importmap.to_script_tag());
    crate::templates::render_file(template, &ctx)
}

/// Without the `tera` feature a `template` can't be rendered; surface a clear error.
#[cfg(not(feature = "tera"))]
fn render_template(_template: &Path, _importmap: &crate::importmap::Importmap) -> Result<String> {
    Err(Error::Build(
        "rendering a `template` requires the `tera` feature".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::importmap::Importmap;

    #[test]
    fn output_optimized_enables_both() {
        let o = Output::optimized();
        assert!(o.minify && o.gzip, "the production preset turns both on");
    }

    #[test]
    #[ignore = "network: downloads @oxc-project/runtime from the npm registry"]
    fn vendor_transform_runtime_resolves_the_decorator_helper() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("el.js"),
            "import _d from \"@oxc-project/runtime/helpers/decorate\";",
        )
        .unwrap();
        let map = vendor_transform_runtime(dir.path(), "/web_modules").unwrap();
        assert!(map.resolves("@oxc-project/runtime/helpers/decorate"));
        assert!(dir
            .path()
            .join("web_modules/@oxc-project/runtime/src/helpers/esm/decorate.js")
            .is_file());
    }

    #[cfg(feature = "tera")]
    #[test]
    fn template_renders_importmap_variable() {
        let dir = tempfile::tempdir().unwrap();
        let tpl = dir.path().join("index.html.tera");
        std::fs::write(&tpl, "<head>{{ importmap | safe }}</head>").unwrap();
        let mut map = Importmap::new();
        map.insert("lit", "/web_modules/lit/index.js");
        let html = render_template(&tpl, &map).unwrap();
        assert!(html.contains("<script type=\"importmap\">"));
        assert!(html.contains("/web_modules/lit/index.js"));
    }

    /// A `BuildOptions` over a single root with no vendoring and the all-on defaults.
    fn opts<'a>(roots: &'a [PathBuf], out: &'a Path) -> BuildOptions<'a> {
        BuildOptions {
            specs: &[],
            roots,
            out,
            mount: "/web_modules",
            html: "<!doctype html>FALLBACK{importmap}",
            template: None,
            processors: Processors::default(),
            output: Output::default(),
        }
    }

    /// [`opts`] with duplicate output paths allowed — the precedence semantics.
    fn opts_skip<'a>(roots: &'a [PathBuf], out: &'a Path) -> BuildOptions<'a> {
        let mut o = opts(roots, out);
        o.processors.skip_duplicates = true;
        o
    }

    #[test]
    fn build_replaces_stale_outputs_across_rebuilds() {
        // The stale-file failure sequence from the review: a source removed between
        // builds must not survive in a reused output directory — its emitted file
        // (and, with gzip, its sidecar) is gone after the rebuild.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.ts"), "export const x: number = 1;").unwrap();
        let out = dir.path().join("out");
        let mut o = opts(std::slice::from_ref(&src), &out);
        o.output = Output::new(false, true);
        build(&o).unwrap();
        assert!(out.join("app.js").exists());
        assert!(out.join("app.js.gz").exists());
        assert!(out.join(OUT_MARKER).exists(), "the output is marked");

        std::fs::remove_file(src.join("app.ts")).unwrap();
        std::fs::write(src.join("b.txt"), "fresh").unwrap();
        build(&o).unwrap();
        assert!(!out.join("app.js").exists(), "the stale module is gone");
        assert!(!out.join("app.js.gz").exists(), "its sidecar too");
        assert!(out.join("b.txt").exists(), "the fresh file shipped");
    }

    #[test]
    fn build_refuses_a_foreign_output_directory() {
        // `--out .` in a project dir, a shared OUT_DIR: a non-empty directory without
        // the marker was not produced by web-modules and must not be replaced.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("page.html"), "<p>hi</p>").unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("precious.txt"), "mine").unwrap();

        let err = build(&opts(std::slice::from_ref(&src), &out))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("refusing to replace") && err.contains(OUT_MARKER),
            "got {err}"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("precious.txt")).unwrap(),
            "mine",
            "the foreign directory is untouched"
        );
    }

    #[test]
    fn build_accepts_empty_and_previously_built_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("page.html"), "<p>hi</p>").unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();

        let o = opts(std::slice::from_ref(&src), &out);
        build(&o).unwrap();
        build(&o).unwrap();
        assert!(out.join("page.html").exists());
    }

    #[test]
    fn build_failure_leaves_the_previous_output_intact() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("page.html"), "<p>v1</p>").unwrap();
        let out = dir.path().join("out");
        let o = opts(std::slice::from_ref(&src), &out);
        build(&o).unwrap();

        // Make the next build fail its conflict check, after the previous succeeded.
        std::fs::write(src.join("page.html.tera"), "<p>v2</p>").unwrap();
        build(&o).unwrap_err();
        assert_eq!(
            std::fs::read_to_string(out.join("page.html")).unwrap(),
            "<p>v1</p>",
            "the previous output survives a failed rebuild"
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("web-modules-"))
            .collect();
        assert!(leftovers.is_empty(), "no stage/old residue: {leftovers:?}");
    }

    #[test]
    fn build_preserves_the_vendor_cache_and_prunes_dropped_packages() {
        // Runtime-helper vendoring is offline (embedded bytes), so it stands in for
        // any vendored package: its files and cache marker must survive a rebuild
        // (seeded, not re-fetched), while a package the build no longer requests —
        // planted here — is pruned from the seeded stage.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("el.ts"),
            "function dec(_v: unknown, _c: unknown) {}\nclass El { @dec accessor x = 1; }\nexport { El };",
        )
        .unwrap();
        let out = dir.path().join("out");
        let o = opts(std::slice::from_ref(&src), &out);
        build(&o).unwrap();
        let helper_dir = out.join("web_modules/@oxc-project/runtime");
        let marker = out.join("web_modules/.@oxc-project_runtime.version");
        assert!(helper_dir.is_dir(), "runtime helpers vendored");
        assert!(marker.is_file(), "cache marker written");
        let marker_content = std::fs::read_to_string(&marker).unwrap();

        // A vendored package from a previous configuration, no longer requested.
        std::fs::create_dir_all(out.join("web_modules/dropped")).unwrap();
        std::fs::write(out.join("web_modules/dropped/x.js"), "export {};").unwrap();
        std::fs::write(out.join("web_modules/.dropped.version"), "1.0.0").unwrap();

        build(&o).unwrap();
        assert!(helper_dir.is_dir(), "helpers survive the rebuild");
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            marker_content,
            "the cache marker still matches - no re-vendor"
        );
        assert!(
            !out.join("web_modules/dropped").exists(),
            "the dropped package is pruned"
        );
        assert!(
            !out.join("web_modules/.dropped.version").exists(),
            "its marker too"
        );
    }

    #[test]
    fn build_reserves_the_generated_import_map() {
        // `importmap.json.tera` would overwrite the generated map after it is written;
        // a literal `importmap.json` would be overwritten by it. Both are hard errors,
        // and `--skip-duplicates` does not bypass them — it arbitrates
        // source-against-source precedence only.
        for name in ["importmap.json.tera", "importmap.json"] {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("web");
            std::fs::create_dir_all(&src).unwrap();
            let content = if name.ends_with(".tera") {
                "{{ 1 }}"
            } else {
                "{}"
            };
            std::fs::write(src.join(name), content).unwrap();

            let out = dir.path().join("out");
            let err = build(&opts_skip(std::slice::from_ref(&src), &out))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("reserved") && err.contains("importmap.json"),
                "{name}: got {err}"
            );
            let untouched = std::fs::read_dir(&out)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(true);
            assert!(untouched, "{name}: nothing may be written");
        }
    }

    #[test]
    fn build_reserves_the_vendor_directory() {
        // `web_modules/` belongs to vendoring (npm packages, runtime helpers) even in
        // a build that vendors nothing this run — helper vendoring is decided later.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(src.join("web_modules")).unwrap();
        std::fs::write(src.join("web_modules/shim.js"), "export {};").unwrap();

        let out = dir.path().join("out");
        let err = build(&opts(std::slice::from_ref(&src), &out))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("reserved") && err.contains("web_modules"),
            "got {err}"
        );
    }

    #[test]
    fn build_reserves_gzip_sidecars() {
        // A shipped `app.js.gz` collides with the sidecar the gzip pass writes for the
        // emitted `app.js`; without `--gzip` the precompressed file ships untouched.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.js"), "export {};").unwrap();
        std::fs::write(src.join("app.js.gz"), b"\x1f\x8bstale").unwrap();

        let out = dir.path().join("out");
        let mut gzipped = opts(std::slice::from_ref(&src), &out);
        gzipped.output = Output::new(false, true);
        let err = build(&gzipped).unwrap_err().to_string();
        assert!(
            err.contains("reserved") && err.contains("gzip sidecar") && err.contains("app.js"),
            "got {err}"
        );

        let plain_out = dir.path().join("out-plain");
        build(&opts(std::slice::from_ref(&src), &plain_out)).unwrap();
        assert_eq!(
            std::fs::read(plain_out.join("app.js.gz")).unwrap(),
            b"\x1f\x8bstale",
            "without --gzip the shipped .gz is just a static file"
        );
    }

    #[test]
    fn build_rejects_secret_targets_from_every_step() {
        // The reject list guards what may be *emitted*, no matter which step produces
        // it: a template or a compiled source cannot materialize `.env` or
        // `private.key` — the same targets the dev server refuses to serve.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join(".env.tera"), "SECRET={{ 1 }}").unwrap();
        std::fs::write(src.join(".env.ts"), "export const x = 1;").unwrap();
        std::fs::write(src.join(".env.scss"), "b { color: red; }").unwrap();
        std::fs::write(src.join("private.key.tera"), "{{ 2 }}").unwrap();
        std::fs::write(src.join("page.html"), "<p>hi</p>").unwrap();

        let out = dir.path().join("out");
        build(&opts(std::slice::from_ref(&src), &out)).unwrap();

        for target in [".env", ".env.js", ".env.css", "private.key"] {
            assert!(!out.join(target).exists(), "{target} must not be emitted");
        }
        assert!(out.join("page.html").exists(), "the page still ships");
    }

    #[test]
    fn reject_none_opts_rejected_targets_back_in() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("private.key.tera"), "key: {{ 40 + 2 }}").unwrap();

        let out = dir.path().join("out");
        let mut o = opts(std::slice::from_ref(&src), &out);
        o.processors.reject = crate::reject::Reject::none();
        build(&o).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("private.key")).unwrap(),
            "key: 42",
            "an explicit `none` reject list opts the target back in"
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_refuses_a_symlink_escaping_the_source_root() {
        // The review's repro: `web/exposed -> ../private` must not publish
        // `private/credentials.txt`, and the error names the path and its target.
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("credentials.txt"), "secret").unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("page.html"), "<p>hi</p>").unwrap();
        std::os::unix::fs::symlink(&private, src.join("exposed")).unwrap();

        let out = dir.path().join("out");
        let err = build(&opts(std::slice::from_ref(&src), &out))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("resolve outside the source root")
                && err.contains("exposed/credentials.txt")
                && err.contains("private"),
            "got: {err}"
        );
        let untouched = std::fs::read_dir(&out)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true);
        assert!(untouched, "nothing may be written");
    }

    #[cfg(unix)]
    #[test]
    fn build_ships_symlinks_resolving_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("web");
        std::fs::create_dir_all(src.join("real")).unwrap();
        std::fs::write(src.join("real/data.txt"), "data").unwrap();
        std::os::unix::fs::symlink(src.join("real/data.txt"), src.join("alias.txt")).unwrap();

        let out = dir.path().join("out");
        build(&opts(std::slice::from_ref(&src), &out)).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("alias.txt")).unwrap(),
            "data",
            "an in-root link publishes its content"
        );
    }

    #[test]
    fn build_skips_vendor_when_specs_empty() {
        // The issue's repro: `build` a non-vendored tree (no specs / no package.json).
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("page.html"), "<p>hi</p>").unwrap();

        build(&opts(std::slice::from_ref(&src), &out)).unwrap();

        assert!(
            !out.join("web_modules").exists(),
            "no specs ⇒ no vendoring, so no web_modules/ dir"
        );
        assert!(out.join("page.html").exists(), "static file copied through");
        assert!(
            out.join("importmap.json").exists(),
            "empty import map emitted"
        );
        assert!(
            out.join("index.html").exists(),
            "fallback index.html written"
        );
    }

    #[cfg(feature = "scss")]
    #[test]
    fn build_without_typescript_skips_ts_and_compiles_scss() {
        // The pipeline is processor-agnostic: with the TypeScript processor off, `.ts` files are
        // ignored (never transformed, never copied raw) while SCSS still compiles. (Mirrors the
        // compile-time `typescript`-off path the feature gate produces.)
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.ts"), "export const x: number = 1;").unwrap();
        std::fs::write(src.join("styles.scss"), "a { b { color: red } }").unwrap();

        let mut o = opts(std::slice::from_ref(&src), &out);
        o.processors = Processors {
            typescript: false,
            ..Processors::default()
        };
        build(&o).unwrap();

        assert!(out.join("styles.css").exists(), "SCSS still compiled");
        assert!(!out.join("app.js").exists(), "no JS emitted with TS off");
        assert!(!out.join("app.ts").exists(), "`.ts` source not copied raw");
    }

    #[cfg(feature = "tera")]
    #[test]
    fn build_renders_tera_tree() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("index.html.tera"),
            "<head>{{ importmap | safe }}</head>",
        )
        .unwrap();
        std::fs::write(src.join("_partial.html.tera"), "PARTIAL").unwrap();

        build(&opts(std::slice::from_ref(&src), &out)).unwrap();

        let index = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(
            index.contains("<script type=\"importmap\">"),
            "the `.tera` rendered with the importmap var; got:\n{index}"
        );
        assert!(
            !index.contains("FALLBACK"),
            "an in-tree index.html.tera wins over the --html fallback"
        );
        assert!(
            !out.join("_partial.html").exists(),
            "`_`-prefixed partials are not emitted"
        );
    }

    #[cfg(feature = "tera")]
    #[test]
    fn build_first_root_wins_under_skip_duplicates() {
        // A cross-root conflict is an error by default; with `--skip-duplicates` the
        // first root wins, the order the dev server overlays roots in.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("index.html.tera"), "FROM_A").unwrap();
        std::fs::write(b.join("index.html.tera"), "FROM_B").unwrap();

        let roots = vec![a, b];
        let err = build(&opts(&roots, &out)).unwrap_err();
        assert!(
            err.to_string().contains("--skip-duplicates"),
            "strict by default; got: {err}"
        );

        build(&opts_skip(&roots, &out)).unwrap();
        let index = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(
            index.contains("FROM_A"),
            "first root wins a conflict; got: {index}"
        );
    }

    #[cfg(feature = "tera")]
    #[test]
    fn build_conflict_error_lists_every_conflict() {
        // Every contested output path is reported at once — each claimant named with
        // its step — and nothing has been written when the build fails.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("index.html"), "LITERAL").unwrap();
        std::fs::write(src.join("index.html.tera"), "TERA").unwrap();
        std::fs::write(src.join("app.ts"), "export const x = 1;").unwrap();
        std::fs::write(src.join("app.js"), "export const ok = true;").unwrap();

        let err = build(&opts(std::slice::from_ref(&src), &out)).unwrap_err();
        let message = err.to_string();
        for expected in [
            "index.html",
            "app.js",
            "Tera template",
            "static copy",
            "TypeScript transform",
            "--skip-duplicates",
        ] {
            assert!(
                message.contains(expected),
                "missing {expected:?} in: {message}"
            );
        }
        assert!(
            !out.exists(),
            "the conflict check runs before anything is written - no output appears"
        );
    }

    #[test]
    fn build_cross_root_same_relative_path_errors() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("logo.svg"), "<svg>A</svg>").unwrap();
        std::fs::write(b.join("logo.svg"), "<svg>B</svg>").unwrap();

        let roots = vec![a, b];
        let err = build(&opts(&roots, &out)).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("logo.svg") && message.contains("--skip-duplicates"),
            "got: {message}"
        );
    }

    #[cfg(feature = "tera")]
    #[test]
    fn build_analyzes_tera_rendered_js() {
        // JavaScript rendered from a template joins the graph: its unresolvable bare
        // import fails the build like any other emitted module's would.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.js.tera"), "import \"missing-package\";").unwrap();

        let err = build(&opts(std::slice::from_ref(&src), &out)).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("missing-package"),
            "the rendered module's import is validated; got: {message}"
        );
    }

    #[cfg(all(feature = "tera", feature = "typescript"))]
    #[test]
    fn build_tera_rendered_mjs_must_parse() {
        // A rendered `.mjs` is unambiguously a module; when the rendered text does not
        // parse, the build fails naming the template.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("worker.mjs.tera"), "var await = 1;").unwrap();

        let err = build(&opts(std::slice::from_ref(&src), &out)).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("worker.mjs.tera")
                && message.contains("does not parse as an ES module"),
            "got: {message}"
        );
    }

    #[cfg(feature = "tera")]
    #[test]
    fn build_tera_does_not_override_earlier_roots_under_skip() {
        // Root order dominates the within-root ranks: a later root's `.tera` no longer
        // overlays an earlier root's literal same-target file, matching how the dev
        // server resolves the request.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("index.html"), "LITERAL_A").unwrap();
        std::fs::write(b.join("index.html.tera"), "TERA_B").unwrap();

        let roots = vec![a, b];
        build(&opts_skip(&roots, &out)).unwrap();
        let index = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(
            index.contains("LITERAL_A"),
            "the earlier root's literal wins over a later root's template; got: {index}"
        );
    }

    #[cfg(feature = "tera")]
    #[test]
    fn build_graph_follows_root_precedence() {
        // Under `--skip-duplicates` the first root wins a path conflict, and the graph
        // must describe the winner: a shadowed fallback file with an unresolvable
        // import must not fail the build, and the winning file with one must.
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("primary");
        let fallback = dir.path().join("fallback");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(primary.join("app.js"), "export const ok = true;").unwrap();
        std::fs::write(fallback.join("app.js"), "import \"missing-package\";").unwrap();

        let roots = vec![primary.clone(), fallback.clone()];
        build(&opts_skip(&roots, &out)).unwrap();
        let shipped = std::fs::read_to_string(out.join("app.js")).unwrap();
        assert!(
            shipped.contains("ok"),
            "the first root's file ships; got:\n{shipped}"
        );

        // Inverse: the winner itself carries the unresolvable import — that is an error.
        std::fs::write(primary.join("app.js"), "import \"missing-package\";").unwrap();
        std::fs::write(fallback.join("app.js"), "export const ok = true;").unwrap();
        let out2 = dir.path().join("out2");
        let err = build(&opts_skip(&roots, &out2)).unwrap_err();
        assert!(matches!(err, Error::Build(_)), "got {err:?}");
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn build_graph_follows_literal_over_transform_precedence() {
        // Within one root a literal `app.js` outranks the `app.js` a sibling `app.ts`
        // would emit — and the graph must describe the winner, in both directions.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        // The transform output is shadowed: its unresolvable import must not count …
        std::fs::write(
            src.join("app.ts"),
            "import { x } from \"missing-package\";\nexport const y = x;",
        )
        .unwrap();
        std::fs::write(src.join("app.js"), "export const ok = true;").unwrap();
        let out = dir.path().join("out");
        build(&opts_skip(std::slice::from_ref(&src), &out)).unwrap();
        let shipped = std::fs::read_to_string(out.join("app.js")).unwrap();
        assert!(
            shipped.contains("ok"),
            "the literal file outranks the transformed sibling; got:\n{shipped}"
        );

        // … and the winning literal's own unresolvable import must count.
        std::fs::write(src.join("app.ts"), "export const y = 1;").unwrap();
        std::fs::write(src.join("app.js"), "import \"missing-package\";").unwrap();
        let out2 = dir.path().join("out2");
        let err = build(&opts_skip(std::slice::from_ref(&src), &out2)).unwrap_err();
        assert!(matches!(err, Error::Build(_)), "got {err:?}");
    }

    #[test]
    fn build_non_utf8_winner_replaces_shadowed_record() {
        // A non-UTF-8 winning file still records its write, so the shadowed fallback
        // file's unresolvable import must not fail the build — the content it described
        // no longer ships.
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("primary");
        let fallback = dir.path().join("fallback");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(primary.join("app.js"), [0xFF, 0xFE, 0x80, b';']).unwrap();
        std::fs::write(fallback.join("app.js"), "import \"missing-package\";").unwrap();

        let roots = vec![primary, fallback];
        build(&opts_skip(&roots, &out)).unwrap();
        assert_eq!(
            std::fs::read(out.join("app.js")).unwrap(),
            [0xFF, 0xFE, 0x80, b';'],
            "the winning bytes ship unchanged"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn build_non_utf8_literal_replaces_transformed_record() {
        // The non-UTF-8 literal `app.js` outranks the `app.js` a sibling `app.ts`
        // would emit, so the transform's unresolvable import must not fail the build.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("app.ts"),
            "import { x } from \"missing-package\";\nexport const y = x;",
        )
        .unwrap();
        std::fs::write(src.join("app.js"), [0xFF, 0xFE, 0x80, b';']).unwrap();

        build(&opts_skip(std::slice::from_ref(&src), &out)).unwrap();
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn build_checks_classic_script_dynamic_imports() {
        // A copied classic script (`await` as an identifier, so only the script goal
        // parses) can still dynamically import through the document's import map — an
        // unresolvable literal specifier in it is a build error.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("legacy.js"),
            "var await = 1;\nimport(\"missing-package\");",
        )
        .unwrap();

        let err = build(&opts(std::slice::from_ref(&src), &out)).unwrap_err();
        assert!(matches!(err, Error::Build(_)), "got {err:?}");
    }

    #[test]
    fn unresolved_bare_import_errors_without_vendoring() {
        // A non-vendored build that imports a bare specifier it never vendored is still a
        // real error (the import map can't resolve it).
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        // `LitElement` is *used*, so the transform keeps the bare `"lit"` import.
        std::fs::write(
            src.join("app.ts"),
            "import { LitElement } from \"lit\";\nexport class X extends LitElement {}",
        )
        .unwrap();

        let err = build(&opts(std::slice::from_ref(&src), &out)).unwrap_err();
        assert!(
            matches!(err, Error::Build(_)),
            "an unvendored bare import is a build error; got {err:?}"
        );
    }

    #[test]
    fn build_fallback_index_refreshes_on_rebuild() {
        // Regression guard: the `--html` fallback must rewrite index.html on every build, even
        // into a reused output dir (a build script's OUT_DIR). Keying the skip off `out/index.html`
        // existing (rather than the source tree) would leave a stale page on rebuild.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("data.json"), "{}").unwrap(); // the tree provides no index

        let mut o = opts(std::slice::from_ref(&src), &out);
        o.html = "<!doctype html>ONE";
        build(&o).unwrap();
        assert!(std::fs::read_to_string(out.join("index.html"))
            .unwrap()
            .contains("ONE"));

        // Rebuild into the SAME out with a changed `--html`: index.html must refresh, not stick.
        o.html = "<!doctype html>TWO";
        build(&o).unwrap();
        let index = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(
            index.contains("TWO") && !index.contains("ONE"),
            "fallback index.html must refresh on rebuild; got:\n{index}"
        );
    }

    #[cfg(feature = "tera")]
    #[test]
    fn build_tera_wins_over_literal_same_target_under_skip() {
        // A same-root double assignment is an error by default; under
        // `--skip-duplicates` the `*.tera` outranks the literal — the precedence the
        // dev server also applies (it checks `.tera` first), so `dev` and `build`
        // stay in lock-step.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("index.html"), "LITERAL").unwrap();
        std::fs::write(src.join("index.html.tera"), "TERA").unwrap();

        let err = build(&opts(std::slice::from_ref(&src), &out)).unwrap_err();
        assert!(
            err.to_string().contains("index.html"),
            "the double assignment is named; got: {err}"
        );

        build(&opts_skip(std::slice::from_ref(&src), &out)).unwrap();
        let index = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(
            index.contains("TERA") && !index.contains("LITERAL"),
            "the .tera outranks the literal same-target; got:\n{index}"
        );
    }

    #[test]
    fn build_validates_only_the_generated_map() {
        // The page's inline map is the page's own business: it does not stand in for
        // the generated map, so a bare import the generated (here empty) map can't
        // resolve fails the build even though the page's map covers it.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("index.html"),
            r#"<script type="importmap">{"imports": {"lit": "./web_modules/lit/index.js"}}</script>"#,
        )
        .unwrap();
        std::fs::write(
            src.join("app.ts"),
            "import { LitElement } from \"lit\";\nexport class X extends LitElement {}",
        )
        .unwrap();

        let err = build(&opts(std::slice::from_ref(&src), &out)).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("unresolved bare import"),
            "the generated map is the only validation target; got: {message}"
        );
    }

    #[test]
    fn build_never_parses_page_html() {
        // A literal page is copied byte-for-byte and never read back: markup no
        // import-map parser would accept must not affect the build.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        let page = r#"<!doctype html><script type="importmap">{not json}</script>"#;
        std::fs::write(src.join("index.html"), page).unwrap();

        build(&opts(std::slice::from_ref(&src), &out)).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("index.html")).unwrap(),
            page,
            "the literal page ships unchanged"
        );
    }
}
