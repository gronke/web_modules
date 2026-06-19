//! Build-script helper: **build** a complete static frontend into an output dir for
//! embedding (`include_dir!`) — vendored `web_modules/`, transformed TypeScript,
//! compiled SCSS, copied static files, and a rendered `index.html`.
//!
//! Call from a consumer `build.rs` (with web-modules as a `build-dependency`):
//!
//! ```no_run
//! use std::path::{Path, PathBuf};
//! use web_modules::build::{build, BuildOptions};
//! use web_modules::vendor::PackageSpec;
//!
//! # fn main() -> web_modules::Result<()> {
//! let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dist");
//! build(&BuildOptions {
//!     specs: &[PackageSpec::npm("lit", "^3")],
//!     src: Path::new("web"),
//!     out: &out,
//!     mount: "/web_modules",
//!     html: "<!doctype html>{importmap}<script type=module src=/app.js></script>",
//!     template: None,
//!     output: Default::default(),
//! })?;
//! # Ok(()) }
//! ```

use std::path::Path;

use crate::vendor::{self, PackageSpec};
use crate::{Error, Result};

/// Inputs for [`build`].
pub struct BuildOptions<'a> {
    /// Packages to vendor into `<out>/web_modules/`.
    pub specs: &'a [PackageSpec],
    /// Source directory (TypeScript, SCSS, and other static files).
    pub src: &'a Path,
    /// Output directory (e.g. `$OUT_DIR/dist`).
    pub out: &'a Path,
    /// URL prefix the vendored modules are served at (e.g. `"/web_modules"`).
    pub mount: &'a str,
    /// `index.html` template; the literal `{importmap}` is replaced with the
    /// generated `<script type="importmap">…</script>`.
    pub html: &'a str,
    /// Optional Tera template file, rendered with an `importmap` variable (the
    /// `<script type="importmap">…</script>` tag) instead of `html` when `Some`.
    /// Requires the `tera` feature.
    pub template: Option<&'a Path>,
    /// Output optimization: minify the emitted JS and/or write `.gz` sidecars.
    pub output: Output,
}

/// Output-optimization toggles — "make the output smaller". Each processor applies
/// what it can (TS minifies; SCSS already emits compressed; static assets gzip-only).
/// Both default off, so an unset `BuildOptions { .. }` behaves as before.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct Output {
    /// Minify the emitted JS (via the TS compile's output — see [`crate::typescript`]).
    pub minify: bool,
    /// Write `<file>.gz` sidecars for servable assets. Requires the `compress` feature.
    pub gzip: bool,
}

impl Output {
    /// The production preset: minify the emitted JS **and** write `.gz` sidecars (both
    /// toggles on). Reach for this — or [`Output::default`] (both off) — since `Output`
    /// is `#[non_exhaustive]` and so can't be built field-by-field from other crates.
    /// Takes full effect with the `minify` and `compress` features.
    pub fn optimized() -> Self {
        Self {
            minify: true,
            gzip: true,
        }
    }
}

/// Vendor + transform + compile + render into `out`, ready to embed and serve.
/// Emits `cargo:rerun-if-changed` for the source dir.
pub fn build(opts: &BuildOptions<'_>) -> Result<()> {
    std::fs::create_dir_all(opts.out)?;
    let mut importmap = vendor::vendor(&opts.out.join("web_modules"), opts.mount, opts.specs)?;
    let transpile = crate::typescript::TranspileOptions {
        minify: opts.output.minify,
        ..Default::default()
    };
    crate::typescript::compile_directory_with(opts.src, opts.out, &transpile)?;
    #[cfg(feature = "scss")]
    crate::scss::compile_directory(opts.src, opts.out, &[opts.out])?;
    crate::static_files::copy_static(opts.src, opts.out)?;

    // Resolve the runtime helpers the transform emitted (decorator helper, etc.).
    importmap.extend(vendor_transform_runtime(opts.out, opts.mount)?);

    // Fail the build if any emitted module imports a bare specifier the import map
    // can't resolve (a transform runtime helper, a forgotten dependency, …) — so a
    // browser-load failure becomes a clear build error instead.
    let unresolved = unresolved_imports(opts.out, &importmap)?;
    if !unresolved.is_empty() {
        let details = unresolved
            .iter()
            .map(|(file, spec)| format!("  {file}: import \"{spec}\""))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::Build(format!(
            "web-modules: {} unresolved bare import(s) — add them to the vendored \
             specs / import map:\n{details}",
            unresolved.len()
        )));
    }

    // Emit the import map as a standalone artifact too, so test harnesses (and
    // es-module-shims / an external `<script type="importmap" src>`) can consume it.
    importmap.write_to(&opts.out.join("importmap.json"))?;

    let html = match opts.template {
        Some(template) => {
            println!("cargo:rerun-if-changed={}", template.display());
            render_template(template, &importmap)?
        }
        None => opts.html.replace("{importmap}", &importmap.to_script_tag()),
    };
    std::fs::write(opts.out.join("index.html"), html)?;

    #[cfg(feature = "compress")]
    if opts.output.gzip {
        crate::compress::gzip_dir(opts.out, &["js", "css", "html", "json", "svg"])?;
    }

    // Re-run the build script when any source file changes. A bare `rerun-if-changed`
    // on the directory only catches add/remove (the directory's own mtime), not edits to
    // existing files — which would leave an *embedded* build serving stale assets — so
    // walk the tree and emit every entry (the root dir included, to catch add/remove).
    for entry in walkdir::WalkDir::new(opts.src)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        println!("cargo:rerun-if-changed={}", entry.path().display());
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
const OXC_RUNTIME_RANGE: &str = "^0.135";

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

/// Render `index.html` from a Tera `template`, exposing the import-map script tag
/// as an `importmap` variable.
#[cfg(feature = "tera")]
fn render_template(template: &Path, importmap: &crate::importmap::Importmap) -> Result<String> {
    let mut ctx = crate::templates::Context::new();
    ctx.insert("importmap", &importmap.to_script_tag());
    crate::templates::render_file(template, &ctx)
}

/// Without the `tera` feature a `template` can't be rendered — surface a clear error.
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
}
