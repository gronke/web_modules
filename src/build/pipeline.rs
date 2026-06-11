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

use std::collections::BTreeSet;
use std::path::Path;

use crate::importmap::Importmap;
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
    /// Drop vendored packages the app never imports (import-graph prune). Off by
    /// default; enable via [`with_prune_unused`](Self::with_prune_unused).
    pub prune_unused: bool,
}

impl Output {
    /// The production preset: minify the emitted JS **and** write `.gz` sidecars (both
    /// toggles on). Reach for this — or [`Output::default`] (both off) — since `Output`
    /// is `#[non_exhaustive]` and so can't be built field-by-field from other crates.
    /// Takes full effect with the `minify` and `compress` features. Leaves `prune_unused`
    /// off — add it explicitly with [`with_prune_unused`](Self::with_prune_unused).
    pub fn optimized() -> Self {
        Self {
            minify: true,
            gzip: true,
            prune_unused: false,
        }
    }

    /// Enable the import-graph prune ("tree-shaking" for the native-ESM vendor): after
    /// vendoring, delete every vendored package not reachable from the app's imports,
    /// keeping the embedded output lean. Chain onto any preset, e.g.
    /// `Output::optimized().with_prune_unused(true)`.
    ///
    /// **Caveat — dynamic imports:** only a statically written `import("name")` is
    /// followed; a package reached *only* through a computed `import(expr)` can't be
    /// seen and would be pruned. Don't enable this if you load a vendored package by
    /// computed specifier (or keep it reachable with a static import). Packages with no
    /// import-map entry (a SCSS load path, a `<script>` global) are never touched.
    pub fn with_prune_unused(mut self, on: bool) -> Self {
        self.prune_unused = on;
        self
    }
}

/// Vendor + transform + compile + render into `out`, ready to embed and serve.
/// Emits `cargo:rerun-if-changed` for the source dir.
pub fn build(opts: &BuildOptions<'_>) -> Result<()> {
    std::fs::create_dir_all(opts.out)?;
    let importmap = vendor::vendor(&opts.out.join("web_modules"), opts.mount, opts.specs)?;
    let transpile = crate::typescript::TranspileOptions {
        minify: opts.output.minify,
        ..Default::default()
    };
    crate::typescript::compile_directory_with(opts.src, opts.out, &transpile)?;
    #[cfg(feature = "scss")]
    crate::scss::compile_directory(opts.src, opts.out, &[opts.out])?;
    crate::static_files::copy_static(opts.src, opts.out)?;

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

    // Tree-shake the vendored tree (opt-in): drop packages the app never imports.
    let importmap = if opts.output.prune_unused {
        prune_unused(opts.out, opts.mount, importmap, opts.specs)?
    } else {
        importmap
    };

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

/// Module specifiers from an emitted module's `import` / `export … from` and dynamic
/// `import()`, read from oxc's parser module record. The `build` module is
/// `typescript`-gated, so oxc is always available here; parsing (rather than a lexical
/// scan) keeps this robust against specifiers that merely appear inside strings or
/// comments and against minified spacing.
fn module_specifiers(js: &str) -> Vec<String> {
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    let allocator = Allocator::default();
    // oxc yields a (possibly partial) module record even on parse errors → best-effort.
    let record = Parser::new(&allocator, js, SourceType::mjs())
        .parse()
        .module_record;

    // Static `import` / `export … from` requests are keyed by specifier.
    let mut specs: Vec<String> = record
        .requested_modules
        .keys()
        .map(|s| s.to_string())
        .collect();

    // Dynamic `import(...)`: the record holds the span of the specifier *expression* —
    // take it when that's a plain string literal, skip computed expressions.
    for dynamic in &record.dynamic_imports {
        let span = dynamic.module_request;
        if let Some(raw) = js.get(span.start as usize..span.end as usize) {
            if let Some(lit) = string_literal_value(raw) {
                specs.push(lit);
            }
        }
    }
    specs
}

/// The inner text of a single-/double-quoted string literal (`"lit"` → `lit`), or
/// `None` for anything else (e.g. a computed `import(expr)`).
fn string_literal_value(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(a), Some(b)) if raw.len() >= 2 && (*a == b'"' || *a == b'\'') && a == b => {
            Some(raw[1..raw.len() - 1].to_string())
        }
        _ => None,
    }
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

/// A browser ES module on disk (`.js`/`.mjs`).
fn is_js_module(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|x| x.to_str()),
        Some("js" | "mjs")
    )
}

/// The import-map `url`'s owning package: the longest `universe` spec dir that
/// prefixes the URL's path after `mount` (a scoped `@scope/name` wins over a shorter
/// match). `None` for a URL outside the vendored mount.
fn package_of<'a>(url: &str, mount: &str, universe: &[&'a PackageSpec]) -> Option<&'a str> {
    let rest = url.strip_prefix(mount)?.trim_start_matches('/');
    universe
        .iter()
        .map(|s| s.name())
        .filter(|dir| rest == *dir || rest.strip_prefix(*dir).is_some_and(|r| r.starts_with('/')))
        .max_by_key(|dir| dir.len())
}

/// The vendored packages a JS source imports: its bare specifiers, resolved through
/// `map`, mapped to their owning package.
fn packages_in(js: &str, mount: &str, universe: &[&PackageSpec], map: &Importmap) -> Vec<String> {
    module_specifiers(js)
        .into_iter()
        .filter(|s| is_bare(s))
        .filter_map(|s| map.resolve(&s).map(str::to_string))
        .filter_map(|url| package_of(&url, mount, universe).map(str::to_string))
        .collect()
}

/// Import-graph prune ("tree-shaking" for the native-ESM vendor): walk the bare imports
/// from the app's emitted modules **through** the vendored packages, then delete every
/// vendored package — and its import-map entries — that nothing reaches. Package
/// granularity (a reached package is kept whole).
/// [`no_imports`](crate::vendor::PackageSpec::no_imports) specs (no import-map entry —
/// SCSS load paths, `<script>`-loaded globals) are never touched. Returns the rebuilt map.
fn prune_unused(
    out: &Path,
    mount: &str,
    map: Importmap,
    specs: &[PackageSpec],
) -> Result<Importmap> {
    let vendor_dir = out.join("web_modules");
    let mount = mount.trim_end_matches('/');
    // Only entry-having packages may be pruned (others aren't in the JS graph).
    let universe: Vec<&PackageSpec> = specs.iter().filter(|s| s.has_imports()).collect();

    // Roots: every package the app's own (non-vendored) modules import.
    let mut reachable: BTreeSet<String> = BTreeSet::new();
    let mut work: Vec<String> = Vec::new();
    for entry in walkdir::WalkDir::new(out)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !is_js_module(path) || path.components().any(|c| c.as_os_str() == "web_modules") {
            continue;
        }
        let js = std::fs::read_to_string(path)?;
        for pkg in packages_in(&js, mount, &universe, &map) {
            if reachable.insert(pkg.clone()) {
                work.push(pkg);
            }
        }
    }

    // Walk the package graph through the vendored code (whole-package scan).
    while let Some(pkg) = work.pop() {
        let Some(spec) = universe.iter().find(|s| s.name() == pkg) else {
            continue;
        };
        for entry in walkdir::WalkDir::new(spec.resolved_dest(&vendor_dir))
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !is_js_module(entry.path()) {
                continue;
            }
            let js = std::fs::read_to_string(entry.path())?;
            for dep in packages_in(&js, mount, &universe, &map) {
                if reachable.insert(dep.clone()) {
                    work.push(dep);
                }
            }
        }
    }

    // Delete unreachable entry-having packages from disk.
    let mut dropped: Vec<&str> = Vec::new();
    for spec in &universe {
        if !reachable.contains(spec.name()) {
            let dest = spec.resolved_dest(&vendor_dir);
            if dest.exists() {
                std::fs::remove_dir_all(&dest)?;
            }
            dropped.push(spec.name());
        }
    }
    if !dropped.is_empty() {
        dropped.sort_unstable();
        println!(
            "cargo:warning=web-modules: pruned {} unused vendored package(s): {}",
            dropped.len(),
            dropped.join(", ")
        );
    }

    // Rebuild the import map with only the reachable packages' entries.
    let mut pruned = Importmap::new();
    for (specifier, url) in map.iter() {
        if package_of(url, mount, &universe).is_some_and(|p| reachable.contains(p)) {
            pruned.insert(specifier, url);
        }
    }
    Ok(pruned)
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
    fn ast_scanner_ignores_strings_and_comments() {
        // A string literal and a comment that *look* like imports must not be picked
        // up; real imports must — including minified, no-space `from"x"` the old
        // lexical scan would have missed.
        let js = "import{x}from\"real-pkg\";\n\
                  const s = \"import 'fake-in-string'\";\n\
                  // import \"fake-in-comment\";\n\
                  const d = import(\"dyn-pkg\");";
        let specs = module_specifiers(js);
        assert!(
            specs.contains(&"real-pkg".to_string()),
            "minified import found"
        );
        assert!(
            specs.contains(&"dyn-pkg".to_string()),
            "dynamic import found"
        );
        assert!(
            !specs.iter().any(|s| s.contains("fake")),
            "string/comment look-alikes ignored: {specs:?}"
        );
    }

    #[test]
    fn ast_scanner_follows_reexports() {
        // `export … from` / `export * from` are module requests too (oxc keeps them in
        // the parser's `requested_modules`). The prune walks the graph through these, so
        // a package reached *only* via a re-export must still be seen — pin that here.
        let js = "export { html } from \"pkg-a\";\n\
                  export * from \"pkg-b\";\n\
                  import \"pkg-c\";";
        let specs = module_specifiers(js);
        assert!(
            specs.contains(&"pkg-a".to_string()),
            "named `export … from` followed: {specs:?}"
        );
        assert!(
            specs.contains(&"pkg-b".to_string()),
            "star `export * from` followed: {specs:?}"
        );
        assert!(
            specs.contains(&"pkg-c".to_string()),
            "plain import still found: {specs:?}"
        );
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

    #[test]
    fn prune_drops_unreachable_keeps_transitive() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        let vm = out.join("web_modules");
        // app → lit → lit-html; `unused` is an orphan; `globals` is a no-imports
        // package (a SCSS load path / <script> global) that must survive untouched.
        std::fs::write(out.join("app.js"), "import { LitElement } from \"lit\";").unwrap();
        for (pkg, src) in [
            ("lit", "import \"lit-html\";\nexport class X {}"),
            ("lit-html", "export const html = 1;"),
            ("unused", "export const z = 1;"),
            ("globals", "export const g = 1;"),
        ] {
            std::fs::create_dir_all(vm.join(pkg)).unwrap();
            std::fs::write(vm.join(pkg).join("index.js"), src).unwrap();
        }

        let mut map = Importmap::new();
        map.insert("lit", "/web_modules/lit/index.js");
        map.insert("lit-html", "/web_modules/lit-html/index.js");
        map.insert("unused", "/web_modules/unused/index.js");
        // `globals` is vended with no import-map entry.

        let specs = [
            PackageSpec::npm("lit", "^3"),
            PackageSpec::npm("lit-html", "^3"),
            PackageSpec::npm("unused", "^1"),
            PackageSpec::npm("globals", "^1").no_imports(),
        ];
        let pruned = prune_unused(out, "/web_modules", map, &specs).unwrap();

        assert!(vm.join("lit/index.js").exists(), "app dep kept");
        assert!(vm.join("lit-html/index.js").exists(), "transitive dep kept");
        assert!(!vm.join("unused").exists(), "orphan pruned");
        assert!(
            vm.join("globals/index.js").exists(),
            "no-imports package untouched"
        );

        let keys: Vec<&str> = pruned.iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            vec!["lit", "lit-html"],
            "map rebuilt to reachable only"
        );
    }

    #[test]
    fn prune_deletes_custom_dest_at_resolved_path() {
        // A spec with a custom `dest` lives at a different on-disk path than its name, so
        // the prune must delete it at its `resolved_dest`, not `<vendor>/<name>`.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        let vm = out.join("web_modules");
        std::fs::write(out.join("app.js"), "import { u } from \"used\";").unwrap();
        std::fs::create_dir_all(vm.join("used")).unwrap();
        std::fs::write(vm.join("used/index.js"), "export const u = 1;").unwrap();
        // `relocated` is vended under web_modules/vendor/relocated, and nothing imports it.
        std::fs::create_dir_all(vm.join("vendor/relocated")).unwrap();
        std::fs::write(vm.join("vendor/relocated/index.js"), "export const r = 1;").unwrap();

        let mut map = Importmap::new();
        map.insert("used", "/web_modules/used/index.js");
        map.insert("relocated", "/web_modules/relocated/index.js");

        let specs = [
            PackageSpec::npm("used", "^1"),
            PackageSpec::npm("relocated", "^1").dest("vendor/relocated"),
        ];
        let pruned = prune_unused(out, "/web_modules", map, &specs).unwrap();

        assert!(vm.join("used/index.js").exists(), "reachable package kept");
        assert!(
            !vm.join("vendor/relocated").exists(),
            "unreachable custom-dest package deleted at its resolved_dest"
        );
        let keys: Vec<&str> = pruned.iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            vec!["used"],
            "map rebuilt to the reachable entry only"
        );
    }

    #[test]
    fn prune_handles_scoped_and_dynamic_imports() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        let vm = out.join("web_modules");
        // app statically imports a scoped package and dynamically imports another.
        std::fs::write(
            out.join("app.js"),
            "import { x } from \"@scope/used\";\nconst m = import(\"dynpkg\");",
        )
        .unwrap();
        for pkg in ["@scope/used", "@scope/unused", "dynpkg"] {
            std::fs::create_dir_all(vm.join(pkg)).unwrap();
            std::fs::write(vm.join(pkg).join("index.js"), "export const v = 1;").unwrap();
        }

        let mut map = Importmap::new();
        map.insert("@scope/used", "/web_modules/@scope/used/index.js");
        map.insert("@scope/unused", "/web_modules/@scope/unused/index.js");
        map.insert("dynpkg", "/web_modules/dynpkg/index.js");

        let specs = [
            PackageSpec::npm("@scope/used", "^1"),
            PackageSpec::npm("@scope/unused", "^1"),
            PackageSpec::npm("dynpkg", "^1"),
        ];
        let pruned = prune_unused(out, "/web_modules", map, &specs).unwrap();

        assert!(
            vm.join("@scope/used/index.js").exists(),
            "scoped static import kept"
        );
        assert!(vm.join("dynpkg/index.js").exists(), "dynamic import kept");
        assert!(
            !vm.join("@scope/unused").exists(),
            "unreachable scoped package pruned"
        );
        let keys: Vec<&str> = pruned.iter().map(|(k, _)| k).collect();
        assert!(keys.contains(&"@scope/used") && keys.contains(&"dynpkg"));
        assert!(!keys.contains(&"@scope/unused"));
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
