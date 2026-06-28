//! [`Build`]: a fluent builder for the [`build`](super::build) pipeline.
//!
//! The owned, chainable counterpart of [`BuildOptions`](super::BuildOptions): accumulate
//! roots, vendor specs, the output dir and processor/output toggles, then [`run`](Build::run).
//! It's the recommended entry; `BuildOptions` + [`build`](super::build) remain as the
//! borrowed low-level form. Mirrors the `self`-consuming chain style of
//! [`Frontend`](crate::Frontend).

use std::path::PathBuf;

use super::{build, BuildOptions, Output, Processors};
use crate::builder_shared::source_builder_methods;
use crate::vendor::PackageSpec;
use crate::{Error, Result};

/// `Build`'s default fallback `index.html` (used only when the source tree has no
/// `index.html` / `index.html.tera`). The entry script is RELATIVE (`./app.js`) so the
/// page also loads under a subpath; `{importmap}` is replaced with the import-map `<script>`.
const DEFAULT_HTML: &str = "<!doctype html>{importmap}<script type=module src=./app.js></script>";

/// Fluent builder for a static [`build`](super::build).
///
/// ```no_run
/// use web_modules::Build;
///
/// # fn main() -> web_modules::Result<()> {
/// Build::new()
///     .root("web")
///     .vendor("lit@^3")   // or .manifest("package.json") to read its dependencies
///     .out("dist")
///     .minify(true)
///     .run()?;
/// # Ok(()) }
/// ```
///
/// Shared source inputs (`root`/`roots`, `typescript`/`scss`/`tera`, `decorators`,
/// `scss_load_path(s)`) come from [`source_builder_methods!`](crate::builder_shared); the
/// methods below are build-specific. Defaults mirror the `default` Cargo features
/// (TypeScript, SCSS, Tera on; minify/gzip off), `mount` is `/web_modules`, and `out`
/// is required.
#[derive(Default)]
pub struct Build {
    roots: Vec<PathBuf>,
    out: Option<PathBuf>,
    mount: Option<String>,
    html: Option<String>,
    template: Option<PathBuf>,
    specs: Vec<PackageSpec>,
    manifests: Vec<PathBuf>,
    processors: Processors,
    output: Output,
}

source_builder_methods!(Build);

impl Build {
    /// A new builder with no roots and the default processor/output set. Set at least
    /// [`out`](Self::out) before [`run`](Self::run).
    pub fn new() -> Self {
        Self::default()
    }

    /// The output directory (e.g. `dist`). **Required** — [`run`](Self::run) errors if unset.
    pub fn out(mut self, out: impl Into<PathBuf>) -> Self {
        self.out = Some(out.into());
        self
    }

    /// URL prefix the vendored modules are served at (default `/web_modules`).
    pub fn mount(mut self, mount: impl Into<String>) -> Self {
        self.mount = Some(mount.into());
        self
    }

    /// Fallback inline `index.html` (used only when the tree has no `index.html`);
    /// `{importmap}` is replaced with the import-map `<script>`. Ignored when
    /// [`template`](Self::template) is set.
    pub fn html(mut self, html: impl Into<String>) -> Self {
        self.html = Some(html.into());
        self
    }

    /// Fallback Tera template file for `index.html` (rendered with an `importmap`
    /// variable), used instead of [`html`](Self::html). Requires the `tera` feature.
    pub fn template(mut self, template: impl Into<PathBuf>) -> Self {
        self.template = Some(template.into());
        self
    }

    /// Vendor an npm package, as `name` or `name@range` (e.g. `lit@^3`), parsed via
    /// [`PackageSpec::parse`]. Repeatable.
    pub fn vendor(mut self, spec: impl AsRef<str>) -> Self {
        self.specs.push(PackageSpec::parse(spec.as_ref()));
        self
    }

    /// Vendor pre-built [`PackageSpec`]s (for specs that need more than `name@range`,
    /// or an already-resolved set). Repeatable.
    pub fn vendor_specs(mut self, specs: impl IntoIterator<Item = PackageSpec>) -> Self {
        self.specs.extend(specs);
        self
    }

    /// Also vendor the `dependencies` of this `package.json` (honoring its
    /// `web_modules.webDependencies` whitelist), read at [`run`](Self::run). Repeatable.
    pub fn manifest(mut self, manifest: impl Into<PathBuf>) -> Self {
        self.manifests.push(manifest.into());
        self
    }

    /// Minify the emitted JS (default off). Requires the `minify` feature.
    pub fn minify(mut self, on: bool) -> Self {
        self.output.minify = on;
        self
    }

    /// Write `<file>.gz` sidecars for servable assets (default off). Requires the
    /// `compress` feature.
    pub fn gzip(mut self, on: bool) -> Self {
        self.output.gzip = on;
        self
    }

    /// The production preset: minify **and** gzip (equivalent to `.minify(true).gzip(true)`).
    pub fn optimized(mut self) -> Self {
        self.output = Output::optimized();
        self
    }

    /// Resolve vendor specs (explicit + each `manifest`'s dependencies, deduped by name,
    /// first wins) and run the [`build`](super::build) pipeline. Errors if [`out`](Self::out)
    /// was never set.
    pub fn run(self) -> Result<()> {
        let out = self.out.ok_or_else(|| {
            Error::Build("Build::out is required — set the output directory with .out(…)".into())
        })?;

        // Explicit specs first, then each manifest's `dependencies`; a same-named manifest
        // entry is deduped out (first wins), matching the CLI's vendor resolution.
        let mut specs = self.specs;
        for manifest in &self.manifests {
            specs.extend(crate::vendor::specs_from_package_json(manifest)?);
        }
        let mut seen = std::collections::HashSet::new();
        specs.retain(|s| seen.insert(s.name().to_string()));

        let mount = self.mount.unwrap_or_else(|| "/web_modules".to_string());
        let html = self.html.unwrap_or_else(|| DEFAULT_HTML.to_string());

        build(&BuildOptions {
            specs: &specs,
            roots: &self.roots,
            out: &out,
            mount: &mount,
            html: &html,
            template: self.template.as_deref(),
            processors: self.processors,
            output: self.output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_and_output_map_to_processors() {
        let b = Build::new()
            .root("web")
            .scss(false)
            .tera(false)
            .minify(true)
            .gzip(true)
            .scss_load_path("styles");
        assert_eq!(b.roots, [PathBuf::from("web")]);
        assert!(b.processors.typescript, "typescript stays default-on");
        assert!(!b.processors.scss && !b.processors.tera, "explicit off");
        assert!(b.output.minify && b.output.gzip);
        assert_eq!(
            b.processors.extra_scss_load_paths,
            [PathBuf::from("styles")]
        );
    }

    #[test]
    fn optimized_sets_both_outputs() {
        let b = Build::new().optimized();
        assert!(b.output.minify && b.output.gzip);
    }

    #[test]
    fn reject_preset_and_pattern_compose_on_processors() {
        use crate::reject::Presets;
        let b = Build::new()
            .root("web")
            .reject_preset(Presets::ALL & !Presets::CONFIG)
            .reject(".htpasswd");
        let r = &b.processors.reject;
        assert!(r.rejects("app.ts"), "source preset still on");
        assert!(r.rejects(".env"), "hidden preset still on");
        assert!(!r.rejects("package.json"), "config preset dropped");
        assert!(r.rejects(".htpasswd"), "extra pattern added on top");
    }

    #[test]
    fn vendor_parses_specs() {
        let b = Build::new().vendor("lit@^3").vendor("@lit/context@^1");
        let names: Vec<&str> = b.specs.iter().map(PackageSpec::name).collect();
        assert_eq!(names, ["lit", "@lit/context"]);
    }

    #[test]
    fn run_requires_out() {
        let err = Build::new().root("web").run().unwrap_err();
        assert!(
            matches!(err, Error::Build(_)),
            "missing out is a build error"
        );
    }

    #[test]
    fn run_builds_a_non_vendored_tree() {
        // Parity with the BuildOptions path (cf. pipeline's build_skips_vendor_when_specs_empty):
        // the builder compiles a static tree and writes the fallback index, no web_modules/.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("page.html"), "<p>hi</p>").unwrap();

        Build::new().root(&src).out(&out).run().unwrap();

        assert!(out.join("page.html").exists(), "static file copied through");
        assert!(out.join("index.html").exists(), "fallback index written");
        assert!(!out.join("web_modules").exists(), "no specs ⇒ no vendoring");
    }
}
