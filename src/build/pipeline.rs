//! Build-script helper: **build** a complete static frontend into an output dir for
//! embedding (`include_dir!`): vendored `web_modules/`, transformed TypeScript,
//! compiled SCSS, copied static files, and a rendered `index.html`.
//!
//! Call from a consumer `build.rs` (with web_modules as a `build-dependency`). The fluent
//! [`Build`](crate::Build) builder is the recommended entry:
//!
//! ```no_run
//! use std::path::PathBuf;
//! use web_modules::Build;
//!
//! # fn main() -> web_modules::Result<()> {
//! let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dist");
//! Build::new()
//!     .root("web")
//!     .vendor("lit@^3")
//!     .out(out)
//!     .run()?;
//! # Ok(()) }
//! ```
//!
//! [`build`] over a [`BuildOptions`] is the borrowed low-level form the builder wraps:
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

use crate::vendor::{self, PackageSpec};
use crate::{Error, Result};

/// Inputs for [`build`].
pub struct BuildOptions<'a> {
    /// Packages to vendor into `<out>/web_modules/`. **Empty ⇒ no vendoring** — the
    /// source tree is just compiled statically, exactly as the dev server serves a
    /// non-vendored tree.
    pub specs: &'a [PackageSpec],
    /// Source root(s), merged first-match-wins (the first root wins a path conflict),
    /// exactly as the dev server overlays them. Usually one (the source directory);
    /// pass `std::slice::from_ref(&dir)` for the single-root case.
    pub roots: &'a [PathBuf],
    /// Output directory (e.g. `$OUT_DIR/dist`).
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
    /// Decorator lowering for the TypeScript transform. Defaults to [`Decorators::Lit`].
    ///
    /// [`Decorators::Lit`]: crate::typescript::Decorators::Lit
    pub ts_decorators: crate::typescript::Decorators,
    /// Extra SCSS `@use`/`@import` load paths, on top of the source roots and `out`.
    pub extra_scss_load_paths: Vec<PathBuf>,
}

impl Default for Processors {
    fn default() -> Self {
        Self {
            typescript: true,
            scss: true,
            tera: true,
            ts_decorators: crate::typescript::Decorators::Lit,
            extra_scss_load_paths: Vec::new(),
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
pub fn build(opts: &BuildOptions<'_>) -> Result<()> {
    std::fs::create_dir_all(opts.out)?;

    // Vendor only when there are packages to vendor. A non-vendored source tree (no
    // specs and no `--manifest`) just compiles statically — the same thing the dev
    // server does serving such a tree, only emitted instead of served.
    let mut importmap = if opts.specs.is_empty() {
        crate::importmap::Importmap::new()
    } else {
        vendor::vendor(&opts.out.join("web_modules"), opts.mount, opts.specs)?
    };

    // SCSS `@use`/`@import` load paths span every source root (matching the dev server),
    // plus the output dir (so vendored stylesheets under `<out>/web_modules` resolve) and
    // any explicit `--scss-load-path`.
    #[cfg(feature = "scss")]
    let load_paths: Vec<&Path> = {
        let mut paths: Vec<&Path> = opts.roots.iter().map(PathBuf::as_path).collect();
        paths.push(opts.out);
        paths.extend(
            opts.processors
                .extra_scss_load_paths
                .iter()
                .map(PathBuf::as_path),
        );
        paths
    };

    let transpile = crate::typescript::TranspileOptions {
        minify: opts.output.minify,
        decorators: opts.processors.ts_decorators,
        ..Default::default()
    };

    // Compile each root last-to-first, so the FIRST root wins a path conflict (it is
    // written last, overwriting) — the order the dev server overlays roots in.
    for root in opts.roots.iter().rev() {
        if opts.processors.typescript {
            crate::typescript::compile_directory_with(root, opts.out, &transpile)?;
        }
        #[cfg(feature = "scss")]
        if opts.processors.scss {
            crate::scss::compile_directory(root, opts.out, &load_paths)?;
        }
        // Carry across everything the processors don't transform (HTML, images, JSON, …);
        // sources (`.ts`/`.scss`/`.tera`) are skipped by `copy_static`.
        crate::static_files::copy_static(root, opts.out)?;
    }

    // Resolve the runtime helpers the transform emitted (decorator helper, etc.) — even a
    // non-vendored build may need these.
    importmap.extend(vendor_transform_runtime(opts.out, opts.mount)?);

    // Fail the build if any emitted module imports a bare specifier the import map
    // can't resolve (a transform runtime helper, a forgotten dependency, …) — so a
    // browser-load failure becomes a clear build error instead. This still applies to a
    // non-vendored build: importing a bare specifier you didn't vendor is a real error.
    let unresolved = unresolved_imports(opts.out, &importmap)?;
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

    // Emit the import map as a standalone artifact too, so test harnesses (and
    // es-module-shims / an external `<script type="importmap" src>`) can consume it.
    importmap.write_to(&opts.out.join("importmap.json"))?;

    // Render every `*.tera` in the tree to its stripped target (`index.html.tera` →
    // `index.html`), with the import map available as the `importmap` variable. Looped
    // last-to-first so the first root wins, matching the compile order. The static
    // counterpart of the dev server's on-the-fly `.tera` rendering.
    #[cfg(feature = "tera")]
    if opts.processors.tera {
        for root in opts.roots.iter().rev() {
            render_tera_tree(root, opts.out, &importmap)?;
        }
    }

    // Entry-page fallback: synthesise `index.html` from `--template` / inline `--html` only when
    // the *source tree* doesn't provide one (a root-level `index.html`, or an `index.html.tera`
    // when tera is on). Keyed off the source tree, not `out/index.html`: probing the output would
    // wrongly skip the refresh on an incremental rebuild into a reused output dir (a build
    // script's `OUT_DIR`), leaving a stale page when `--html`/`--template` or the import map changed.
    let tera_on = cfg!(feature = "tera") && opts.processors.tera;
    let tree_provides_index = opts.roots.iter().any(|root| {
        root.join("index.html").exists() || (tera_on && root.join("index.html.tera").exists())
    });
    if !tree_provides_index {
        let html = match opts.template {
            Some(template) => {
                rerun_if_changed(template);
                render_template(template, &importmap)?
            }
            None => opts.html.replace("{importmap}", &importmap.to_script_tag()),
        };
        std::fs::write(opts.out.join("index.html"), html)?;
    }

    #[cfg(feature = "compress")]
    if opts.output.gzip {
        crate::compress::gzip_dir(opts.out, &["js", "css", "html", "json", "svg"])?;
    }

    // Re-run the build script when any source file changes. A bare `rerun-if-changed`
    // on the directory only catches add/remove (the directory's own mtime), not edits to
    // existing files — which would leave an *embedded* build serving stale assets — so
    // walk every root and emit every entry (the root dir included, to catch add/remove).
    for root in opts.roots {
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            rerun_if_changed(entry.path());
        }
    }
    Ok(())
}

/// Bare module specifiers from an emitted module's `import`/`export … from` and
/// dynamic `import()` statements (covers oxc codegen's output forms).
fn module_specifiers(js: &str) -> Vec<String> {
    const PATTERNS: &[&str] = &[
        "from \"",
        "from '",
        "import \"",
        "import '",
        "import(\"",
        "import('",
    ];
    let mut specs = Vec::new();
    for pat in PATTERNS {
        // Every pattern ends in its quote char; skip defensively if one were empty.
        let Some(quote) = pat.chars().last() else {
            continue;
        };
        let mut from = 0;
        while let Some(p) = js[from..].find(pat) {
            let start = from + p + pat.len();
            match js[start..].find(quote) {
                Some(end) => {
                    specs.push(js[start..start + end].to_string());
                    from = start + end + 1;
                }
                None => break,
            }
        }
    }
    specs
}

/// A *bare* specifier (resolved via the import map), not a relative/absolute/URL one.
fn is_bare(spec: &str) -> bool {
    !(spec.starts_with('.')
        || spec.starts_with('/')
        || spec.contains("://")
        || spec.starts_with("data:"))
}

/// Emitted (non-vendored) `.js` under `dir` whose bare imports the `importmap`
/// can't resolve, as `(relative path, specifier)`.
fn unresolved_imports(
    dir: &Path,
    importmap: &crate::importmap::Importmap,
) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("js") {
            continue;
        }
        // Vendored modules resolve among themselves; only check our emitted code.
        if path.components().any(|c| c.as_os_str() == "web_modules") {
            continue;
        }
        let js = std::fs::read_to_string(path)?;
        for spec in module_specifiers(&js) {
            if is_bare(&spec) && !importmap.resolves(&spec) {
                let rel = path.strip_prefix(dir).unwrap_or(path).display().to_string();
                out.push((rel, spec));
            }
        }
    }
    Ok(out)
}

/// npm `@oxc-project/runtime` range; tracks the `oxc_*` crate version.
const OXC_RUNTIME_RANGE: &str = "^0.137";

/// Vendor the oxc runtime helpers the transform emitted (e.g. the legacy-decorator
/// `@oxc-project/runtime/helpers/decorate`) so their bare imports resolve. Scans emitted JS under
/// `out`; vendors `@oxc-project/runtime` when used, else returns an empty map. `build` calls this.
pub fn vendor_transform_runtime(out: &Path, mount: &str) -> Result<crate::importmap::Importmap> {
    let mut uses_runtime = false;
    for entry in walkdir::WalkDir::new(out)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("js") {
            continue;
        }
        // Vendored modules carry their own imports; only scan our emitted output.
        if path.components().any(|c| c.as_os_str() == "web_modules") {
            continue;
        }
        let js = std::fs::read_to_string(path)?;
        if module_specifiers(&js)
            .iter()
            .any(|s| is_bare(s) && s.starts_with("@oxc-project/runtime"))
        {
            uses_runtime = true;
            break;
        }
    }
    if !uses_runtime {
        return Ok(crate::importmap::Importmap::new());
    }
    vendor::vendor(
        &out.join("web_modules"),
        mount,
        &[PackageSpec::npm("@oxc-project/runtime", OXC_RUNTIME_RANGE)],
    )
}

/// Render every `*.tera` under `root` to its stripped target under `out`
/// (`index.html.tera` → `index.html`), skipping `_`-prefixed partials, with the
/// import-map `<script>` tag exposed as the `importmap` variable. The static counterpart
/// of the dev server's on-the-fly `.tera` rendering. Returns the number rendered.
///
/// `tera::one_off` (via [`crate::templates`]) has no template registry, so each file
/// renders independently — `{% include %}` / `{% extends %}` across files aren't
/// supported (hence the `_`-partial skip is a convention, not an inheritance system).
///
/// Runs as a final overlay (after vendoring + the import-map/unresolved-import checks), so a
/// `*.tera` takes precedence over a same-named compiled/static target — matching the dev server,
/// which checks `.tera` first. Two consequences of that placement, both for unusual configs: across
/// multiple roots a later root's `.tera` overwrites an *earlier* root's literal same-named file
/// (the dev server resolves per-root, so the earlier root wins there — a minor divergence), and JS
/// emitted *by* a `.tera` (e.g. `app.js.tera`) is not scanned for unresolved bare imports.
#[cfg(feature = "tera")]
fn render_tera_tree(
    root: &Path,
    out: &Path,
    importmap: &crate::importmap::Importmap,
) -> Result<usize> {
    let mut ctx = crate::templates::Context::new();
    ctx.insert("importmap", &importmap.to_script_tag());
    let mut count = 0;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let is_tera = path
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.eq_ignore_ascii_case("tera"));
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !is_tera || name.starts_with('_') {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|e| Error::Template(e.to_string()))?;
        // Drop the final `.tera`: `index.html.tera` → `index.html`, `page.tera` → `page`.
        let dest = out.join(rel).with_extension("");
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let html = crate::templates::render_file(path, &ctx)?;
        std::fs::write(&dest, html)?;
        count += 1;
    }
    Ok(count)
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
    fn scanner_and_bare_classification() {
        let js = "import { a } from \"lit\";\n\
                  import _d from \"@oxc-project/runtime/helpers/decorate\";\n\
                  import \"./local.js\";\n\
                  const m = import(\"bootstrap\");";
        let specs = module_specifiers(js);
        assert!(specs.contains(&"lit".to_string()));
        assert!(specs.contains(&"@oxc-project/runtime/helpers/decorate".to_string()));
        assert!(specs.contains(&"./local.js".to_string()));
        assert!(specs.contains(&"bootstrap".to_string()));
        assert!(is_bare("lit") && is_bare("@oxc-project/runtime/helpers/decorate"));
        assert!(!is_bare("./local.js") && !is_bare("/x.js") && !is_bare("https://h/y.js"));
    }

    #[test]
    fn vendor_transform_runtime_is_noop_without_helpers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.js"),
            "import { LitElement } from \"lit\";",
        )
        .unwrap();
        // No @oxc-project/runtime import -> nothing vendored, no network.
        let map = vendor_transform_runtime(dir.path(), "/web_modules").unwrap();
        assert!(!map.resolves("@oxc-project/runtime/helpers/decorate"));
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

    #[test]
    fn flags_unresolved_app_import_but_skips_vendored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.js"),
            "import { LitElement } from \"lit\";\n\
             import _d from \"@oxc-project/runtime/helpers/decorate\";",
        )
        .unwrap();
        // A vendored module with its own bare import is ignored.
        std::fs::create_dir_all(dir.path().join("web_modules/lit")).unwrap();
        std::fs::write(dir.path().join("web_modules/lit/index.js"), "import \"x\";").unwrap();

        let mut map = Importmap::new();
        map.insert("lit", "/web_modules/lit/index.js");
        let unresolved = unresolved_imports(dir.path(), &map).unwrap();
        assert_eq!(
            unresolved.len(),
            1,
            "only the helper import is unresolved; got {unresolved:?}"
        );
        assert!(unresolved[0].1.starts_with("@oxc-project/runtime"));
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
    fn build_first_root_wins() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("index.html.tera"), "FROM_A").unwrap();
        std::fs::write(b.join("index.html.tera"), "FROM_B").unwrap();

        let roots = vec![a, b];
        build(&opts(&roots, &out)).unwrap();

        let index = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(
            index.contains("FROM_A"),
            "first root wins a conflict; got: {index}"
        );
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
    fn build_tera_wins_over_literal_same_target() {
        // A `*.tera` overlays a same-named literal in the same root — the precedence the dev
        // server also applies (it checks `.tera` first), so `dev` and `build` stay in lock-step.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("index.html"), "LITERAL").unwrap();
        std::fs::write(src.join("index.html.tera"), "TERA").unwrap();

        build(&opts(std::slice::from_ref(&src), &out)).unwrap();
        let index = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(
            index.contains("TERA") && !index.contains("LITERAL"),
            "the .tera overlays the literal same-target; got:\n{index}"
        );
    }
}
