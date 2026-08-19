//! Vendor packages into a `web_modules/` tree and build the import map.
//!
//! Orchestration over [`npm_utils`]: each [`PackageSpec`] names a [source](PackageSpec::npm)
//! (an npm package + semver range, a [GitHub archive](PackageSpec::git) at a ref, or a
//! [pre-packed tarball](PackageSpec::tarball) URL), how to [extract](Extract) it, and
//! where its files land. Work is cache-guarded
//! (a per-package version/ref marker + a cross-process lock), so a second build with
//! unchanged specs does no extraction.
//!
//! For the common case (an npm package whose browser assets are vended and whose
//! import-map entries are **auto-derived from its `package.json`**), a spec is just
//! [`PackageSpec::npm`]:
//!
//! ```no_run
//! use std::path::Path;
//! use web_modules::vendor::{vendor, PackageSpec};
//!
//! # fn main() -> web_modules::Result<()> {
//! let specs = [PackageSpec::npm("lit", "^3")];
//! let importmap = vendor(Path::new("web/web_modules"), "/web_modules", &specs)?;
//! println!("{}", importmap.to_script_tag());
//! # Ok(()) }
//! ```
//!
//! The builder also covers the awkward cases a real app hits: a *full* package
//! staged into a sibling `node_modules/` as a SCSS load path, a *single file*
//! extracted and renamed (a sprite, a font), a *GitHub* (non-npm) source, or a
//! caller-supplied keep-filter. See [`PackageSpec`] and [`Extract`].

use std::path::{Path, PathBuf};

use std::fs;

use npm_utils::package_json::{spec::Range, Entry, PackageJson};
use npm_utils::{cache, download, extract, path_safety, registry::Registry};

use crate::importmap::Importmap;
use crate::mount::Mount;
use crate::{Error, Result};

/// Where a package's bytes come from.
enum Source {
    /// npm registry: resolve `range` to the newest matching published version.
    Npm { package: String, range: String },
    /// A GitHub repository archive (`owner` + `repo`) at a git `reference`
    /// (tag, branch, or commit).
    Git {
        owner: String,
        repo: String,
        reference: String,
    },
    /// A pre-packed tarball at an absolute https URL — an `npm pack` `.tgz`, such
    /// as a GitHub Release asset. Extracted like an npm tarball (nested under
    /// `package/`). `name` gives the import-map base; `url` doubles as the cache
    /// key, so a new release URL re-fetches.
    Tarball { name: String, url: String },
}

/// How a package's archive is extracted into its destination directory.
pub enum Extract {
    /// Keep browser assets (`.js`/`.mjs`/`.css`/`.scss`, dropping `src`/`node`/
    /// `development` trees). The default. Files referenced by the package's
    /// `package.json` exports are kept too, even under `src/`.
    BrowserAssets,
    /// Extract the **entire** archive (no filter), e.g. a full package staged
    /// into a `node_modules/` tree to serve as a SCSS `@use`/`@import` load path.
    Full,
    /// Extract a single file `from` (path inside the package) to `to` (relative
    /// to the destination dir), renaming as needed, e.g. one sprite or font.
    /// Does **not** clear the destination, so several `File` specs can target a
    /// shared directory.
    File { from: String, to: String },
    /// Keep entries for which the predicate returns the (possibly rewritten)
    /// relative path, dropping the rest.
    Filter(fn(&str) -> Option<String>),
}

/// Import-map strategy for a spec.
enum Imports {
    /// Auto-derive from the package's `package.json` exports (npm packages).
    Auto,
    /// No import-map entry (a SCSS/CSS-only package, a `<script>`-loaded global,
    /// or a single vendored file).
    None,
    /// Explicit `(specifier, path)` entries, `path` relative to `<mount>/<dir>/`.
    Explicit(Vec<(String, String)>),
}

/// One package to vendor, built fluently.
///
/// ```
/// use std::path::Path;
/// use web_modules::vendor::{PackageSpec, Extract};
///
/// let specs = [
///     // npm, browser assets, auto-derived import map (the common case):
///     PackageSpec::npm("lit", "^3"),
///     // npm, whole package into a sibling node_modules/ as a SCSS load path:
///     PackageSpec::npm("bootstrap", "^5")
///         .dest(Path::new("node_modules/bootstrap"))
///         .extract(Extract::Full)
///         .no_imports(),
///     // a single committed file, renamed, from a GitHub source archive:
///     PackageSpec::git("feathericons/feather", "v4.29.2")
///         .dest(Path::new("images"))
///         .extract(Extract::File {
///             from: "icons/activity.svg".into(),
///             to: "feather-activity.svg".into(),
///         }),
/// ];
/// # let _ = specs;
/// ```
pub struct PackageSpec {
    source: Source,
    dir: String,
    dest: Option<PathBuf>,
    extract: Extract,
    imports: Imports,
    /// Compile the package's TypeScript after extraction, so what lands in the
    /// vendor tree is what a browser loads. Set by [`PackageSpec::as_source`].
    compile: bool,
}

impl PackageSpec {
    /// An npm package resolved against a semver `range`. Defaults: browser-asset
    /// extraction, auto-derived import map, vended to `<vendor_dir>/<package>/`.
    pub fn npm(package: impl Into<String>, range: impl Into<String>) -> Self {
        let package = package.into();
        Self {
            dir: package.clone(),
            source: Source::Npm {
                package,
                range: range.into(),
            },
            dest: None,
            extract: Extract::BrowserAssets,
            imports: Imports::Auto,
            compile: false,
        }
    }

    /// A GitHub repository archive (`"owner/repo"`) at a git `reference` (tag,
    /// branch, or commit). Defaults: browser-asset extraction, **no** import-map
    /// entry, vended to `<vendor_dir>/<repo>/`.
    pub fn git(repo: impl Into<String>, reference: impl Into<String>) -> Self {
        let full = repo.into();
        let (owner, name) = full.split_once('/').unwrap_or(("", full.as_str()));
        let (owner, name) = (owner.to_string(), name.to_string());
        Self {
            dir: name.clone(),
            source: Source::Git {
                owner,
                repo: name,
                reference: reference.into(),
            },
            dest: None,
            extract: Extract::BrowserAssets,
            imports: Imports::None,
            compile: false,
        }
    }

    /// A pre-packed `.tgz` at an absolute https URL — an `npm pack` archive such as
    /// a GitHub Release asset. Defaults: browser-asset extraction, auto-derived
    /// import map (the tarball carries its own `package.json`), vended to
    /// `<vendor_dir>/<name>/`. Unlike [`git`](Self::git), which fetches a whole-repo
    /// source archive, this is the built, publishable package.
    pub fn tarball(name: impl Into<String>, url: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            dir: name.clone(),
            source: Source::Tarball {
                name,
                url: url.into(),
            },
            dest: None,
            extract: Extract::BrowserAssets,
            imports: Imports::Auto,
            compile: false,
        }
    }

    /// Parse one positional npm spec — `name`, `name@range`, or `@scope/name@range`
    /// (e.g. `lit`, `lit@^3`, `@lit/context@^1`) — into an [`npm`](Self::npm) spec.
    /// The range `@` is the last one, so a leading scope `@` is preserved; a bare
    /// `name` (no range) resolves to `name@*`. Infallible: any string yields a spec.
    pub fn parse(spec: &str) -> Self {
        match spec.rfind('@') {
            Some(i) if i > 0 => Self::npm(&spec[..i], &spec[i + 1..]),
            _ => Self::npm(spec, "*"),
        }
    }

    /// Override the subdirectory under the vendor root (and the import-map URL
    /// segment). Defaults to the package/repo name.
    pub fn dir(mut self, dir: impl Into<String>) -> Self {
        self.dir = dir.into();
        self
    }

    /// Extract somewhere other than `<vendor_dir>/<dir>/`, e.g. a sibling
    /// `node_modules/`. A relative path is resolved against `vendor_dir`.
    pub fn dest(mut self, dest: impl Into<PathBuf>) -> Self {
        self.dest = Some(dest.into());
        self
    }

    /// Choose the extraction mode (default [`Extract::BrowserAssets`]).
    pub fn extract(mut self, extract: Extract) -> Self {
        self.extract = extract;
        self
    }

    /// Shorthand for `.extract(Extract::Filter(keep))`.
    pub fn keep(mut self, keep: fn(&str) -> Option<String>) -> Self {
        self.extract = Extract::Filter(keep);
        self
    }

    /// Take the package's **sources** and compile them, for a package that publishes
    /// no browser-usable build.
    ///
    /// Keeps `src/` (which [`Extract::BrowserAssets`] drops) via [`keep_sources`], then
    /// compiles the TypeScript into the layout the package's own `tsconfig.json`
    /// declares — so `main`/`module`/`exports` resolve against files that are there, and
    /// the vendored tree is browser-ready like any other.
    ///
    /// What that config decides is honoured: `rootDir`/`outDir` and the inputs `files`,
    /// `include` and `exclude` select; the emitted extension per module format; decorator
    /// lowering and class-field semantics. What it cannot decide here is refused with a
    /// message naming the package — aliases, an inherited config, decorator metadata, JSX
    /// modes and factories. `target` is read for the class-field default only: this
    /// transform strips types and lowers what it is told to, and does no target
    /// downlevelling, so a package built for a lower target keeps the syntax its sources
    /// use.
    ///
    /// Needs the `typescript` feature: there is no compiling TypeScript without a
    /// TypeScript compiler.
    pub fn as_source(mut self) -> Self {
        self.extract = Extract::Filter(keep_sources);
        self.compile = true;
        // A git spec derives no import-map entry by default, because a whole-repo archive
        // has no reliable browser entry. Once compiled into the layout its `tsconfig.json`
        // declares, this package's own `main`/`module`/`exports` do resolve, so the
        // ordinary auto-derivation applies.
        self.imports = Imports::Auto;
        self
    }

    /// Provide explicit import-map entries: `(specifier, path)` where `path` is
    /// relative to `<mount>/<dir>/` (use `""` for a prefix specifier like
    /// `("lit/", "")`). Replaces auto-derivation.
    pub fn imports<I, K, V>(mut self, entries: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.imports = Imports::Explicit(
            entries
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        );
        self
    }

    /// Vend the files but add **no** import-map entry.
    pub fn no_imports(mut self) -> Self {
        self.imports = Imports::None;
        self
    }

    /// The identifying name for this spec, the package or repo name unless
    /// overridden via [`dir`](Self::dir). Handy for filtering specs sourced from a
    /// `package.json` before overriding a few programmatically.
    pub fn name(&self) -> &str {
        &self.dir
    }
}

/// Build vendoring specs from the `dependencies` of a `package.json`. Keep your
/// browser dependencies in a real `package.json` next to your sources and vendor
/// them with Rust. Registry ranges are preserved verbatim (`^3`, `~1.2`, …); an https
/// `.tgz` URL becomes a [tarball](PackageSpec::tarball) spec; a `github:owner/repo#ref`
/// or git URL becomes a [git](PackageSpec::git) spec; and local protocols
/// (`file:`/`link:`/`workspace:`/`portal:`) are skipped. Each entry
/// defaults to browser-asset extraction with an auto-derived import map.
///
/// Only `dependencies` is read; `devDependencies` (build/test tooling such as
/// `typescript` or `@playwright/test`) are **not** vended. To include other
/// sections, use [`specs_from_package_json_sections`].
///
/// # `webDependencies` whitelist
///
/// When `dependencies` also carries server-only packages, narrow the browser vend
/// with a `webDependencies` whitelist under the `web_modules` key, the convention
/// [@pika/web] / Snowpack introduced for exactly this (*"useful if your entire
/// dependencies object is too large or contains unrelated, server-only packages"*):
///
/// ```json
/// { "dependencies": { "lit": "^3", "pg": "^8" },
///   "web_modules": { "webDependencies": ["lit"] } }
/// ```
///
/// Only the listed names are vended (in order; versions still come from
/// `dependencies`); a listed name absent from `dependencies` is an error. Without
/// the key, every `dependency` is vended.
///
/// [@pika/web]: https://www.npmjs.com/package/@pika/web
pub fn specs_from_package_json(path: &Path) -> Result<Vec<PackageSpec>> {
    let bytes = std::fs::read(path)?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Vendor(format!("{}: {e}", path.display())))?;
    // `web_modules.webDependencies`: an @pika/web-style whitelist of dependency
    // names to vend (versions taken from `dependencies`). Absent → vend all of
    // `dependencies`.
    let Some(whitelist) = json
        .get("web_modules")
        .and_then(|v| v.get("webDependencies"))
    else {
        return specs_from_package_json_sections(path, &["dependencies"]);
    };
    let whitelist = whitelist.as_array().ok_or_else(|| {
        Error::Vendor(format!(
            "{}: web_modules.webDependencies must be an array of dependency names",
            path.display()
        ))
    })?;
    let deps = json.get("dependencies").and_then(|v| v.as_object());
    // A source-built dependency is vendored from its own spec, which carries the compile
    // step and takes its directory from the dependency key; vending it here as well fetches
    // the same repository twice, into two differently named directories.
    let source_deps = source_dependency_names(&json, path)?;
    let mut specs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in whitelist {
        let Some(name) = entry.as_str() else {
            return Err(Error::Vendor(format!(
                "{}: web_modules.webDependencies entries must be strings",
                path.display()
            )));
        };
        if !seen.insert(name.to_string()) {
            continue;
        }
        if source_deps.iter().any(|n| n == name) {
            continue;
        }
        let value = deps
            .and_then(|d| d.get(name))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Vendor(format!(
                    "{}: web_modules.webDependencies lists `{name}`, not found in dependencies",
                    path.display()
                ))
            })?;
        if let Some(spec) = dep_to_spec(name, value) {
            specs.push(spec);
        }
    }
    Ok(specs)
}

/// Like [`specs_from_package_json`], but read the named dependency `sections`
/// (e.g. `&["dependencies", "devDependencies"]`). The first section to name a
/// package wins; later duplicates are dropped. The `webDependencies` whitelist is
/// **not** applied; that is [`specs_from_package_json`]'s browser-vend rule.
pub fn specs_from_package_json_sections(
    path: &Path,
    sections: &[&str],
) -> Result<Vec<PackageSpec>> {
    let bytes = std::fs::read(path)?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Vendor(format!("{}: {e}", path.display())))?;
    let source_deps = source_dependency_names(&json, path)?;
    let mut specs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for section in sections {
        let Some(deps) = json.get(*section).and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, value) in deps {
            let Some(value) = value.as_str() else {
                continue;
            };
            if source_deps.iter().any(|n| n == name) {
                continue;
            }
            let Some(spec) = dep_to_spec(name, value) else {
                continue;
            };
            if seen.insert(name.clone()) {
                specs.push(spec);
            }
        }
    }
    Ok(specs)
}

/// Turn one `package.json` dependency entry (`name` → `value`) into a vendoring
/// [`PackageSpec`]: an https `.tgz` URL → a [tarball](PackageSpec::tarball) spec; a
/// `github:` / git URL → a [git](PackageSpec::git) spec; a local protocol
/// (`file:`/`link:`/`workspace:`/`portal:`) → `None` (nothing to vend); anything
/// else → a registry [npm](PackageSpec::npm) spec, range verbatim.
fn dep_to_spec(name: &str, value: &str) -> Option<PackageSpec> {
    if is_local_protocol(value) {
        return None;
    }
    // Checked before the `github:` form so a GitHub Release-asset URL
    // (`…/releases/download/<tag>/<file>.tgz`) vends as the packed tarball, not as
    // a repo source archive at the default branch.
    if is_tarball_url(value) {
        return Some(PackageSpec::tarball(name, value));
    }
    Some(match parse_github_dep(value) {
        Some((repo, reference)) => PackageSpec::git(repo, reference),
        None => PackageSpec::npm(name, value),
    })
}

/// A `package.json` value pointing at a local path rather than a registry/git
/// source; nothing to vendor.
fn is_local_protocol(value: &str) -> bool {
    ["file:", "link:", "workspace:", "portal:"]
        .iter()
        .any(|p| value.starts_with(p))
}

/// A `package.json` value pointing at a pre-packed tarball over https (an `npm
/// pack` `.tgz`, e.g. a GitHub Release asset).
fn is_tarball_url(value: &str) -> bool {
    value.starts_with("https://") && (value.ends_with(".tgz") || value.ends_with(".tar.gz"))
}

/// Read a `package.json`'s `dependencies`, splitting them: registry ranges → vendoring
/// [`PackageSpec`]s (kept verbatim; `github:` → git specs), and **local path-deps**
/// (`file:`/`link:`/`./`/`../`) → [`Mount`]s, the target dir, named by the dependency
/// **key** (npm's `file:` rule), honoring the target's `web_modules.root`. Other
/// protocols (`workspace:`/`portal:`/`npm:`) are skipped. Use this to compose sibling
/// dirs straight from a manifest; [`specs_from_package_json`] is the vend-only subset.
pub fn read_package_json(path: &Path) -> Result<(Vec<PackageSpec>, Vec<Mount>)> {
    let bytes = std::fs::read(path)?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Vendor(format!("{}: {e}", path.display())))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut specs = Vec::new();
    let mut mounts = Vec::new();
    // Source-built dependencies are vendored from their own specs, which carry the
    // compile step; vending them here as well would fetch each one twice.
    let source_deps = source_dependency_names(&json, path)?;
    let Some(deps) = json.get("dependencies").and_then(|v| v.as_object()) else {
        return Ok((specs, mounts));
    };
    for (name, value) in deps {
        let Some(value) = value.as_str() else {
            continue;
        };
        if source_deps.iter().any(|n| n == name) {
            continue;
        }
        if let Some(rel) = local_path_dep(value) {
            mounts.push(
                Mount::from_dir(base.join(rel))
                    .specifier(format!("{name}/"))
                    .url(format!("/{name}/")),
            );
        } else if is_tarball_url(value) {
            specs.push(PackageSpec::tarball(name.as_str(), value));
        } else if let Some((repo, reference)) = parse_github_dep(value) {
            specs.push(PackageSpec::git(repo, reference));
        } else if !is_unsupported_protocol(value) {
            specs.push(PackageSpec::npm(name.as_str(), value));
        }
    }
    Ok((specs, mounts))
}

/// The dependency names listed under `web_modules.sourceDependencies`, in manifest
/// order. Absent key → empty.
fn source_dependency_names(json: &serde_json::Value, path: &Path) -> Result<Vec<String>> {
    let Some(listed) = json
        .get("web_modules")
        .and_then(|v| v.get("sourceDependencies"))
    else {
        return Ok(Vec::new());
    };
    let listed = listed.as_array().ok_or_else(|| {
        Error::Vendor(format!(
            "{}: web_modules.sourceDependencies must be an array of dependency names",
            path.display()
        ))
    })?;
    let mut names = Vec::new();
    for entry in listed {
        let name = entry.as_str().ok_or_else(|| {
            Error::Vendor(format!(
                "{}: web_modules.sourceDependencies entries must be strings",
                path.display()
            ))
        })?;
        names.push(name.to_string());
    }
    Ok(names)
}

/// Build **source** specs from a `package.json`: the dependencies named under
/// `web_modules.sourceDependencies`, taken from their git reference and configured to
/// keep their sources ([`PackageSpec::as_source`]).
///
/// A package that publishes only TypeScript has no built output to vendor, so it is
/// consumed the way a `file:` path-dep is — compiled from source by this toolchain.
/// Naming it here is what says so:
///
/// ```json
/// { "dependencies": { "acme-ui": "github:acme/ui#v2", "pako": "^2" },
///   "web_modules": { "sourceDependencies": ["acme-ui"] } }
/// ```
///
/// Pass the result to [`vendor`] alongside the ordinary specs: each is fetched, its
/// TypeScript compiled into the layout its `tsconfig.json` declares, and its entries
/// derived from its own manifest — so the vendored package is browser-ready like any
/// other. The named packages are excluded from [`read_package_json`]'s specs, so
/// nothing is fetched twice.
///
/// A listed name absent from `dependencies`, or whose value is not a git reference, is
/// an error: only a git source has sources to build, and a silent skip would leave the
/// app with an unresolvable import.
pub fn source_specs_from_package_json(path: &Path) -> Result<Vec<PackageSpec>> {
    let bytes = std::fs::read(path)?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Vendor(format!("{}: {e}", path.display())))?;
    let names = source_dependency_names(&json, path)?;
    let deps = json.get("dependencies").and_then(|v| v.as_object());
    let mut specs = Vec::new();
    for name in names {
        let value = deps
            .and_then(|d| d.get(&name))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Vendor(format!(
                    "{}: web_modules.sourceDependencies lists `{name}`, not found in dependencies",
                    path.display()
                ))
            })?;
        let (repo, reference) = parse_github_dep(value).ok_or_else(|| {
            Error::Vendor(format!(
                "{}: web_modules.sourceDependencies lists `{name}`, whose value `{value}` is not a \
                 git reference — only a git source can be built from source",
                path.display()
            ))
        })?;
        specs.push(PackageSpec::git(repo, reference).dir(&name).as_source());
    }
    Ok(specs)
}

/// The path of a local path-dependency value (`file:…`, `link:…`, `./…`, `../…`).
fn local_path_dep(value: &str) -> Option<&str> {
    if let Some(rest) = value.strip_prefix("file:") {
        Some(rest)
    } else if let Some(rest) = value.strip_prefix("link:") {
        Some(rest)
    } else if value.starts_with("./") || value.starts_with("../") {
        Some(value)
    } else {
        None
    }
}

/// Dependency protocols [`read_package_json`] doesn't vendor (handled elsewhere, or
/// not a registry package).
fn is_unsupported_protocol(value: &str) -> bool {
    ["workspace:", "portal:", "npm:"]
        .iter()
        .any(|p| value.starts_with(p))
}

/// Parse a GitHub dependency value into `(owner/repo, ref)`: the npm
/// `github:owner/repo#ref` shorthand or a `git+https://github.com/owner/repo(.git)#ref`
/// URL. The ref defaults to `HEAD` (the default branch) when absent. Returns `None`
/// for a plain registry range.
fn parse_github_dep(value: &str) -> Option<(String, String)> {
    let value = value.trim();
    let value = value.strip_prefix("git+").unwrap_or(value);
    let (locator, reference) = match value.split_once('#') {
        Some((l, r)) => (l, r.to_string()),
        None => (value, "HEAD".to_string()),
    };
    let path = if let Some(rest) = locator.strip_prefix("github:") {
        rest
    } else {
        let idx = locator.find("github.com")?;
        locator[idx + "github.com".len()..].trim_start_matches([':', '/'])
    };
    let path = path.trim_end_matches(".git").trim_matches('/');
    let (owner, rest) = path.split_once('/')?;
    let repo = rest.split('/').next().unwrap_or(rest);
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((format!("{owner}/{repo}"), reference))
}

/// Selection for a source-built package: keep everything a module graph can reach,
/// **including `src/`** — for a package that publishes only sources, that is the
/// package. The counterpart of [`keep_browser_assets`], which drops `src/` because a
/// published package's sources are redundant next to its built output.
///
/// Declaration files are dropped: `.d.ts` has no runtime form, and the compiler emits
/// nothing for it.
pub fn keep_sources(rel: &str) -> Option<String> {
    // A source archive is a whole repository, so it carries directories that are not the
    // package: its own examples, tests and docs would otherwise be vendored and compiled.
    if rel.split('/').any(|seg| {
        // Dot-directories are editor and CI furniture (`.github`, `.vscode`,
        // `.devcontainer`), never something a browser loads.
        seg.starts_with('.')
            || matches!(
                seg,
                "node" | "development" | "node_modules" | "examples" | "test" | "tests" | "docs"
            )
    }) {
        return None;
    }
    if is_legal_notice(rel) {
        return Some(rel.to_string());
    }
    // A lockfile describes how to build the package, not how to load it.
    if matches!(
        rel,
        "package-lock.json" | "npm-shrinkwrap.json" | "yarn.lock"
    ) {
        return None;
    }
    // A declaration has no runtime form. TypeScript writes one per module format, and
    // `.d.mts`/`.d.cts` would otherwise pass as ordinary `.mts`/`.cts` sources.
    if is_declaration(rel) {
        return None;
    }
    const KEPT: [&str; 10] = [
        ".ts", ".tsx", ".mts", ".cts", ".js", ".mjs", ".cjs", ".json", ".css", ".scss",
    ];
    KEPT.iter()
        .any(|ext| rel.ends_with(ext))
        .then(|| rel.to_string())
}

/// Default selection: keep browser assets (`.js`/`.mjs`/`.css`) **plus `.scss`
/// sources** (so packages like Bootstrap can be themed from their SCSS) while
/// dropping TypeScript sources and the node-only / development build trees some
/// packages ship.
/// A licence or notice file, which travels with the code it covers: MIT asks for the notice
/// in all copies, Apache-2.0 for the licence with every distribution, and a vendored tree is
/// served as it stands.
fn is_legal_notice(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "LICENSE" | "LICENCE" | "NOTICE" | "COPYING" | "COPYRIGHT" | "AUTHORS" | "PATENTS"
    ) || stem.starts_with("LICENSE-")
        || stem.starts_with("LICENCE-")
}

pub fn keep_browser_assets(rel: &str) -> Option<String> {
    if rel
        .split('/')
        .any(|seg| matches!(seg, "src" | "node" | "development"))
    {
        return None;
    }
    if is_legal_notice(rel) {
        return Some(rel.to_string());
    }
    (rel.ends_with(".js")
        || rel.ends_with(".mjs")
        || rel.ends_with(".css")
        || rel.ends_with(".scss"))
    .then(|| rel.to_string())
}

/// Resolve + download + extract every spec into `vendor_dir`, returning the
/// composed [`Importmap`] with URLs rooted at `mount` (e.g. `"/web_modules"`).
/// Cache-guarded per package; import-map entries follow each spec's strategy.
///
/// # Build scripts
///
/// When called from a build script (detected via the `OUT_DIR` environment
/// variable), each vendored destination is emitted as a `cargo:rerun-if-changed`
/// input. Cargo then re-runs the build script — re-vendoring the files — if a
/// destination is later deleted or modified, so a wiped vendored asset (e.g.
/// `node_modules/bootstrap`) self-heals on the next build instead of silently
/// surfacing as a runtime failure for the missing file.
pub fn vendor(vendor_dir: &Path, mount: &str, specs: &[PackageSpec]) -> Result<Importmap> {
    vendor_inner(vendor_dir, mount, specs).map_err(|e| Error::Vendor(e.to_string()))
}

/// The layout and the emit semantics to reproduce for a source dependency, read from the
/// `tsconfig.json` it brought with it.
///
/// Reproducing them is what lets a source-built package keep the entry its manifest
/// already declares: the compiled files land where its `package.json` points, under the
/// name its module format implies, with the decorator and class-field semantics its own
/// build uses. Emit-affecting options beyond those are refused rather than approximated,
/// so the gap between this and the package's own `tsc` run is an error and not a surprise.
///
/// Refused rather than guessed: `paths`/`baseUrl` aliases (the emitted specifiers would not
/// resolve in a browser), `extends` (the real options live in a file we do not have) and
/// `emitDecoratorMetadata` (the metadata needs a decorator runtime this compiler does not
/// emit). Each names the package, because the failure is a property of the dependency and
/// not of the project vendoring it.
struct SourcePlan {
    layout: crate::tsconfig::SourceLayout,
    /// The config itself, which also says which files are inputs.
    config: crate::tsconfig::TsConfig,
    /// `useDefineForClassFields`, or what the target implies.
    defines_class_fields: bool,
    /// `experimentalDecorators`.
    legacy_decorators: bool,
    /// `rewriteRelativeImportExtensions`.
    rewrite_import_extensions: bool,
}

fn source_plan(pkg_dir: &Path, package: &str) -> Result<SourcePlan> {
    let Some(config) = crate::tsconfig::TsConfig::load(pkg_dir)
        .map_err(|e| Error::Vendor(format!("{package}: {e}")))?
    else {
        // No tsconfig: compile in place, which is what tsc does with no outDir, and take
        // its defaults for the rest.
        return Ok(SourcePlan {
            layout: crate::tsconfig::SourceLayout::default(),
            config: crate::tsconfig::TsConfig::default(),
            defines_class_fields: false,
            legacy_decorators: false,
            rewrite_import_extensions: false,
        });
    };

    if config.extends.is_some() {
        return Err(Error::Vendor(format!(
            "{package}: tsconfig.json `extends` another config, which is not in the \
             package \u{2014} its real compiler options cannot be known from here"
        )));
    }
    for (set, alias) in [
        (config.compiler_options.paths.is_some(), "paths"),
        (config.compiler_options.base_url.is_some(), "baseUrl"),
    ] {
        if set {
            return Err(Error::Vendor(format!(
                "{package}: tsconfig.json sets `{alias}`, so its imports resolve through \
                 aliases a browser cannot follow"
            )));
        }
    }
    if config.compiler_options.emit_decorator_metadata == Some(true) {
        return Err(Error::Vendor(format!(
            "{package}: tsconfig.json sets `emitDecoratorMetadata`, which needs a \
             decorator runtime this compiler does not emit"
        )));
    }
    // A `.tsx` source compiles, but through the transform's own JSX handling: a config that
    // names a mode or a factory would be emitted against something else.
    for (set, option) in [
        (config.compiler_options.jsx.is_some(), "jsx"),
        (
            config.compiler_options.jsx_import_source.is_some(),
            "jsxImportSource",
        ),
        (config.compiler_options.jsx_factory.is_some(), "jsxFactory"),
        (
            config.compiler_options.jsx_fragment_factory.is_some(),
            "jsxFragmentFactory",
        ),
    ] {
        if set {
            return Err(Error::Vendor(format!(
                "{package}: tsconfig.json sets `{option}`, which this compiler does not \
                 reproduce"
            )));
        }
    }

    // The transform strips types; it does not rewrite module code. A package emitting
    // CommonJS would get its `export`s left as they are under a `.cjs` name, which is
    // neither its own output nor anything a browser loads.
    if config.emits_commonjs() {
        return Err(Error::Vendor(format!(
            "{package}: tsconfig.json emits CommonJS ({}), which this compiler does not \
             produce — a browser-ready package has to be ES modules",
            config.compiler_options.module.as_deref().map_or_else(
                || format!("implied by target {:?}", config.compiler_options.target),
                |m| format!("module {m}")
            )
        )));
    }

    let layout = config
        .layout()
        .map_err(|e| Error::Vendor(format!("{package}: {e}")))?;
    Ok(SourcePlan {
        layout,
        defines_class_fields: config.defines_class_fields(),
        legacy_decorators: config.compiler_options.experimental_decorators == Some(true),
        rewrite_import_extensions: config.compiler_options.rewrite_relative_import_extensions
            == Some(true),
        config,
    })
}

/// Compile a source-built package in place: TypeScript under the layout's root becomes
/// JavaScript under its out directory, non-TS assets are copied alongside, and the
/// TypeScript is removed.
///
/// The result is a browser-ready package in the vendor tree, so nothing downstream needs
/// to know it arrived as source — no mount, no prefix specifier, no compilation at serve
/// or build time.
#[cfg(feature = "typescript")]
fn compile_source_tree(pkg_dir: &Path, package: &str) -> Result<()> {
    use crate::processors::typescript::{ClassFields, Decorators, TranspileOptions};

    let plan = source_plan(pkg_dir, package)?;

    // The dependency's own emit semantics, not this project's: the zero-config compile is
    // the Lit preset, which would hand a package legacy decorators and assignment-style
    // class fields it never asked for.
    let options = TranspileOptions {
        decorators: if plan.legacy_decorators {
            Decorators::Lit
        } else {
            Decorators::Standard
        },
        class_fields: if plan.defines_class_fields {
            ClassFields::Define
        } else {
            ClassFields::Assign
        },
        rewrite_import_extensions: plan.rewrite_import_extensions,
        ..TranspileOptions::default()
    };

    // `files` and `include` name the program's *root* files; a file one of them imports is
    // in the program too, and `exclude` does not remove it. So the roots are collected and
    // then followed, which is also what makes the emitted imports resolvable: a sibling
    // skipped here would be deleted below and its importer left pointing at nothing.
    let carried = crate::walk::files_within(pkg_dir)?;
    let mut queue: std::collections::VecDeque<PathBuf> = carried
        .iter()
        .filter(|rel| plan.config.is_root_file(rel))
        .cloned()
        .collect();

    let mut seen: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    let mut compiled: std::collections::BTreeMap<PathBuf, String> =
        std::collections::BTreeMap::new();
    let mut assets: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();

    while let Some(rel) = queue.pop_front() {
        if !seen.insert(rel.clone()) {
            continue;
        }
        let entry = pkg_dir.join(&rel);
        if is_commonjs_source(&rel) {
            return Err(Error::Vendor(format!(
                "{package}: `{}` is CommonJS source, which this compiler does not convert",
                rel.display()
            )));
        }
        match compiled_extension(&rel) {
            None => {
                assets.insert(rel);
            }
            Some(_) => {
                let source =
                    fs::read_to_string(&entry).map_err(|e| Error::Vendor(e.to_string()))?;
                let output =
                    crate::processors::typescript::compile_str_capturing(&source, &entry, &options)
                        .map_err(|e| {
                            Error::Vendor(format!("{package}: compiling {}: {e}", rel.display()))
                        })?;
                // Read off the emitted AST, so what is followed is what the output imports.
                for import in &output.imports {
                    // The source it names is compiled and then removed, so an emitted
                    // specifier still ending in a TypeScript extension resolves to nothing.
                    // `rewriteRelativeImportExtensions` is how a package that writes
                    // `./util.ts` avoids this; without it, the output is broken.
                    if is_typescript_source(Path::new(import.specifier.as_str())) {
                        return Err(Error::Vendor(format!(
                            "{package}: {} imports `{}`, a TypeScript path that does not \
                             survive compiling — the package needs \
                             `rewriteRelativeImportExtensions`",
                            rel.display(),
                            import.specifier
                        )));
                    }
                    if let Some(next) = resolve_relative_import(pkg_dir, &rel, &import.specifier) {
                        queue.push_back(next);
                    }
                }
                compiled.insert(rel, output.code);
            }
        }
    }

    if compiled.is_empty() {
        return Err(Error::Vendor(format!(
            "{package}: declared as a source dependency but carries no TypeScript"
        )));
    }

    // `tsc` infers a missing `rootDir` from the program's own input files, so it is read off
    // what was compiled rather than off the patterns that found it.
    let root_rel = match &plan.layout.root {
        Some(root) => root.clone(),
        None => crate::tsconfig::common_input_dir(compiled.keys().map(PathBuf::as_path)),
    };
    let root = pkg_dir.join(&root_rel);
    if !root.is_dir() {
        return Err(Error::Vendor(format!(
            "{package}: no `{root_rel}` directory to compile — the archive carried no sources"
        )));
    }
    // The components were refused if they read as leaving the package; this is what they
    // *reach*, which a link inside the archive can make different.
    let out_dir = pkg_dir.join(&plan.layout.out);
    for (dir, named) in [(&root, &root_rel), (&out_dir, &plan.layout.out)] {
        if dir.exists() && !crate::walk::contains(pkg_dir, dir) {
            return Err(Error::Vendor(format!(
                "{package}: `{named}` resolves outside the package"
            )));
        }
    }

    // `tsc` refuses a program whose inputs are not all under the `rootDir` it was given
    // (TS6059), and so does this: the alternative is emitting an importer whose import was
    // compiled to nowhere. Only an explicit root can fail this way — an inferred one is the
    // directory the inputs share, so it contains them by construction.
    if plan.layout.root.is_some() {
        if let Some(stray) = compiled.keys().find(|rel| !rel.starts_with(&root_rel)) {
            return Err(Error::Vendor(format!(
                "{package}: `{}` is in the program but not under `rootDir` `{root_rel}`, so \
                 it has nowhere to be emitted",
                stray.display()
            )));
        }
    }

    let destination = |rel: &Path| -> Option<PathBuf> {
        // An asset outside the root keeps its place: the layout says nothing about it, and
        // an emitted module reaching it still resolves.
        let under = rel.strip_prefix(&root_rel).ok()?;
        Some(out_dir.join(under))
    };

    for (rel, code) in &compiled {
        let Some(dest) = destination(rel) else {
            continue;
        };
        let ext = compiled_extension(rel).unwrap_or("js");
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::Vendor(e.to_string()))?;
        }
        fs::write(dest.with_extension(ext), code).map_err(|e| Error::Vendor(e.to_string()))?;
    }
    // Assets a compiled module still reaches — the JSON a dynamic `import()` pulls.
    for rel in &assets {
        let Some(dest) = destination(rel) else {
            continue;
        };
        if dest == pkg_dir.join(rel) {
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::Vendor(e.to_string()))?;
        }
        fs::copy(pkg_dir.join(rel), &dest).map_err(|e| Error::Vendor(e.to_string()))?;
    }

    // The TypeScript was an intermediate; the vendor tree holds what a browser loads.
    // Removed only when the root is a strict descendant of the package that does not hold
    // the output — deleting the root would otherwise take the files just written with it.
    let removable = !root_rel.is_empty()
        && root_rel != plan.layout.out
        && root != pkg_dir
        && crate::walk::contains(pkg_dir, &root)
        && !out_dir.starts_with(&root);
    if removable {
        fs::remove_dir_all(&root).map_err(|e| Error::Vendor(e.to_string()))?;
    }
    // Whatever the layout, nothing uncompiled may ship — a stray source elsewhere in the
    // archive would be served as TypeScript no browser can import.
    for rel in crate::walk::files_within(pkg_dir)? {
        if is_typescript_source(&rel) {
            fs::remove_file(pkg_dir.join(&rel)).map_err(|e| Error::Vendor(e.to_string()))?;
        }
    }

    // The whole point is that the package's own manifest still points at its output, and
    // the layout was reproduced from a config that may not say everything. If the entry is
    // missing, `auto_entries` would quietly drop the package from the import map, so the
    // mismatch is reported here with both halves of it.
    if let Ok(manifest) = PackageJson::from_path(&pkg_dir.join("package.json")) {
        for entry in manifest.entries() {
            if let Entry::Bare(target) = entry {
                if !pkg_dir.join(&target).is_file() {
                    return Err(Error::Vendor(format!(
                        "{package}: compiled into `{}`, but its manifest points at `{target}`, \
                         which is not there — the layout its tsconfig.json describes is not \
                         the one it was published with, so `rootDir` needs stating",
                        if plan.layout.out.is_empty() {
                            "the package root".to_string()
                        } else {
                            plan.layout.out.clone()
                        }
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The source file a relative import names, if the package carries one.
///
/// TypeScript writes the *emitted* specifier in its sources — `./util.js` for `util.ts` —
/// so the extension is mapped back before looking, and a directory stands for its `index`.
/// A specifier that climbs out of the package resolves to nothing.
#[cfg(feature = "typescript")]
fn resolve_relative_import(pkg_dir: &Path, from: &Path, specifier: &str) -> Option<PathBuf> {
    if !specifier.starts_with("./") && !specifier.starts_with("../") {
        return None;
    }
    let joined = from.parent().unwrap_or(Path::new("")).join(specifier);
    let rel = lexically_normalize(&joined)?;
    let stem = rel.with_extension("");

    // The module format travels with the extension both ways: `./foo.mjs` is written by
    // `foo.mts`, and only by it. Guessing `foo.ts` for it would compile the wrong file when
    // a package carries both.
    let sources: &[&str] = match rel.extension().and_then(|e| e.to_str()) {
        Some("mjs") => &["mts"],
        Some("cjs") => &["cts"],
        Some("jsx") => &["tsx"],
        Some("js") => &["ts", "tsx"],
        // An extensionless specifier, or one naming an asset.
        _ => &[],
    };

    let mut candidates = sources
        .iter()
        .map(|ext| stem.with_extension(ext))
        // `./foo` names `foo.ts`, and `./dir` names the module inside it.
        .chain(["ts", "tsx"].iter().map(|ext| {
            let mut path = rel.clone();
            path.as_mut_os_string().push(format!(".{ext}"));
            path
        }))
        .chain(["index.ts", "index.tsx"].iter().map(|f| rel.join(f)))
        // A real `.js`/`.json` beside the sources is an asset the output still reaches.
        .chain(std::iter::once(rel.clone()));

    candidates.find(|candidate| pkg_dir.join(candidate).is_file())
}

/// A relative path with `.` and `..` resolved textually, or `None` when it climbs out.
#[cfg(feature = "typescript")]
fn lexically_normalize(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop()?;
            }
            Component::Normal(segment) => out.push(segment.to_os_string()),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out.iter().collect())
}

/// The extension TypeScript emits for a source file, or `None` when the file is not one it
/// compiles.
///
/// The module format travels with the extension: `.mts` emits `.mjs`, which is what a
/// `package.json` pointing at `lib/index.mjs` needs to find. `.cts` is CommonJS source and
/// has no entry here — see [`is_commonjs_source`]. A declaration has no runtime form at all.
fn compiled_extension(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    if is_declaration(name) {
        return None;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts" | "tsx") => Some("js"),
        Some("mts") => Some("mjs"),
        _ => None,
    }
}

/// Whether a file name is a TypeScript declaration, in any of the three module forms.
fn is_declaration(name: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

/// Whether a path is CommonJS TypeScript source, which `.cts` is by definition.
fn is_commonjs_source(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !is_declaration(name) && name.ends_with(".cts"))
}

/// Whether a path is TypeScript source in any form, declarations included — what must not
/// remain in a vendored tree.
fn is_typescript_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ts" | "tsx" | "mts" | "cts")
    )
}

/// A source dependency without a compiler is a contradiction, so say so rather than
/// vendoring TypeScript a browser cannot load.
#[cfg(not(feature = "typescript"))]
fn compile_source_tree(_pkg_dir: &Path, package: &str) -> Result<()> {
    Err(Error::Vendor(format!(
        "{package} is a source dependency, which needs the `typescript` feature to compile"
    )))
}

/// Fingerprint of the source-compilation path, mixed into a compiled spec's cache key.
///
/// A compiled tree is derived output, so the source identifier alone does not describe it:
/// the same commit through a different compiler is a different tree, and a marker naming
/// only the commit would keep the old JavaScript across an upgrade. The crate version moves
/// whenever the transform or the extraction rules can have moved, and needs nobody to
/// remember to bump it.
const COMPILE_SCHEMA: &str = concat!("compile", env!("CARGO_PKG_VERSION"));

/// A spec's cache key, carrying the compile fingerprint when the destination is compiled.
///
/// An uncompiled destination is the archive's contents and nothing else, so its key stays
/// as it is; a compiled one is also the compiler's output, and a key that ignored that
/// would call a stale tree fresh.
fn compiled_key(key: String, compile: bool) -> String {
    if compile {
        format!("{key}+{COMPILE_SCHEMA}")
    } else {
        key
    }
}

/// Whether a git reference is a commit id, and so cannot move.
///
/// A branch or tag can be repointed at any time, which is what makes keying a cache on
/// the reference name wrong; a commit id names one tree forever.
fn is_commit_ref(reference: &str) -> bool {
    reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit())
}

/// A cache key derived from an archive's bytes, for a source whose key is not knowable
/// before fetching.
///
/// Same position-weighted fold `npm_utils::cache::file_hash` uses, and for the same
/// reason: this distinguishes one archive from another, and is not an integrity check.
fn content_key(bytes: &[u8]) -> String {
    let mut hash: u64 = 0;
    for (i, byte) in bytes.iter().enumerate() {
        hash = hash.wrapping_add((*byte as u64).wrapping_mul((i as u64).wrapping_add(1)));
    }
    format!("{hash:016x}")
}

fn vendor_inner(
    vendor_dir: &Path,
    mount: &str,
    specs: &[PackageSpec],
) -> std::result::Result<Importmap, Box<dyn std::error::Error + Send + Sync>> {
    let mount = mount.trim_end_matches('/');
    let mut map = Importmap::new();
    std::fs::create_dir_all(vendor_dir)?;

    for spec in specs {
        // Confine the vendored destination to `vendor_dir`. `spec.dir` defaults to an npm package
        // name / `package.json` dependency key (untrusted), and a relative `spec.dest` is likewise
        // caller-relative; a `..` in either would place the destination — which is then wiped with
        // `remove_dir_all` and re-extracted — outside `vendor_dir`. An absolute `spec.dest` is an
        // explicit operator choice (never derived from manifest data), so it stays honored.
        let dest_dir = match &spec.dest {
            Some(d) if d.is_absolute() => d.clone(),
            Some(d) => {
                let rel =
                    d.to_str()
                        .ok_or_else(|| -> Box<dyn std::error::Error + Send + Sync> {
                            format!("vendor destination {d:?} is not valid UTF-8").into()
                        })?;
                path_safety::ensure_within(rel)?;
                vendor_dir.join(d)
            }
            None => {
                path_safety::ensure_within(&spec.dir)?;
                vendor_dir.join(&spec.dir)
            }
        };

        // Build-script integration: declare the vendored destination as a
        // `rerun-if-changed` input so Cargo re-runs the build script — and thus
        // re-vendors — when this directory is deleted or its contents change.
        // Without it, wiping a vendored asset (e.g. `node_modules/bootstrap`)
        // leaves a "successful" build whose dev server then fails at runtime on
        // the now-missing file. The shared helper is a no-op outside a build-script
        // context and refuses paths that could smuggle a line break into the
        // directive stream.
        crate::static_files::cargo_rerun_if_changed(&dest_dir);

        let flat = spec.dir.replace('/', "_");
        let marker = vendor_dir.join(format!(".{flat}.version"));

        // Resolve the archive URL and, when it can be known without fetching, the cache
        // key. `None` means only the archive itself can say whether anything changed: a
        // branch or tag keeps its name across a force-push, so keying on the reference
        // would hold a stale tree forever. npm keys on the resolved version and a
        // tarball on its URL, both of which move with the content.
        let (archive_url, cache_key, is_git) = match &spec.source {
            Source::Npm { package, range } => {
                let resolved = Registry::npm().resolve(package, &Range::parse(range)?)?;
                (
                    resolved.tarball_url,
                    Some(resolved.version.to_string()),
                    false,
                )
            }
            Source::Git {
                owner,
                repo,
                reference,
            } => (
                download::github_archive_url(owner, repo, reference),
                is_commit_ref(reference).then(|| reference.clone()),
                true,
            ),
            // A pre-packed `.tgz`: fetch the URL directly and extract like an npm
            // tarball (`package/` layout, `is_git = false`). The URL is the cache
            // key, so a new release (a new URL) re-fetches.
            Source::Tarball { url, .. } => (url.clone(), Some(url.clone()), false),
        };

        // A known key can settle freshness without the network. A mutable reference
        // cannot, so it always fetches and then compares the archive's contents.
        let keyed = |key: String| compiled_key(key, spec.compile);
        let cache_key = cache_key.map(keyed);
        let fresh = |key: &Option<String>| {
            key.as_deref()
                .is_some_and(|k| is_up_to_date(&marker, k, &dest_dir, &spec.extract))
        };
        if !fresh(&cache_key) {
            let lock = vendor_dir.join(format!(".{flat}.lock"));
            cache::with_lock(&lock)(
                || -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    // Re-check inside the lock: a concurrent build may have just done it.
                    if fresh(&cache_key) {
                        return Ok(());
                    }
                    let bytes = download::fetch(&archive_url)?;
                    // The archive is the key when the reference is not one: a moved branch
                    // yields different bytes, which is what makes it re-extract.
                    let key = match &cache_key {
                        Some(key) => key.clone(),
                        None => keyed(content_key(&bytes)),
                    };
                    // An unchanged archive leaves the tree — and, for a source dependency,
                    // the compiled output — exactly as it is.
                    if is_up_to_date(&marker, &key, &dest_dir, &spec.extract) {
                        return Ok(());
                    }
                    extract_archive(&bytes, is_git, &spec.extract, &dest_dir)?;
                    // Compile inside the lock and before the marker, so the cached state is
                    // always the finished, browser-ready tree.
                    if spec.compile {
                        compile_source_tree(&dest_dir, spec.name())?;
                    }
                    cache::write_marker(&marker, &key)?;
                    Ok(())
                },
            )?;
        }

        for (specifier, url) in import_entries(spec, mount, &dest_dir) {
            map.insert(specifier, url);
        }
    }

    Ok(map)
}

/// Whether a spec's destination is already populated for `cache_key`. For a
/// single-[`File`](Extract::File) extract the specific output must exist; otherwise
/// the destination directory must be non-empty.
fn is_up_to_date(marker: &Path, cache_key: &str, dest_dir: &Path, extract: &Extract) -> bool {
    if !cache::marker_matches(marker, cache_key) {
        return false;
    }
    match extract {
        Extract::File { to, .. } => dest_dir.join(to).exists(),
        _ => cache::dir_has_content(dest_dir),
    }
}

/// Remove vendored entries the current build did not request. The staged build seeds
/// `web_modules/` from the previous output to keep download caches warm, so a package
/// whose spec was dropped since then would otherwise ship forever; this deletes such
/// package dirs and their `.<flat>.version` cache markers (plus leftover lock files
/// and stray files), and removes the vendor dir itself when nothing remains.
/// `extra_dirs` names pipeline-vendored packages beyond `specs` (the transform
/// runtime).
pub(crate) fn prune(vendor_dir: &Path, specs: &[PackageSpec], extra_dirs: &[&str]) -> Result<()> {
    if !vendor_dir.is_dir() {
        return Ok(());
    }
    let keep_dirs: Vec<String> = specs
        .iter()
        .map(|spec| spec.dir.clone())
        .chain(extra_dirs.iter().map(|dir| dir.to_string()))
        .collect();
    let keep_markers: Vec<String> = keep_dirs
        .iter()
        .map(|dir| format!(".{}.version", dir.replace('/', "_")))
        .collect();
    prune_level(vendor_dir, "", &keep_dirs, &keep_markers)
        .map_err(|e| Error::Vendor(e.to_string()))?;
    if std::fs::read_dir(vendor_dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_dir(vendor_dir);
    }
    Ok(())
}

/// One directory level of [`prune`]: keep exact package dirs, descend into scope dirs
/// on the way to a kept package (removing them when emptied), keep current cache
/// markers at the root, and delete everything else.
fn prune_level(
    dir: &Path,
    prefix: &str,
    keep_dirs: &[String],
    keep_markers: &[String],
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue; // never written by the vendorer; leave it alone
        };
        let qualified = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type()?.is_dir() {
            if keep_dirs.contains(&qualified) {
                continue;
            }
            if keep_dirs
                .iter()
                .any(|keep| keep.starts_with(&format!("{qualified}/")))
            {
                prune_level(&entry.path(), &qualified, keep_dirs, keep_markers)?;
                if std::fs::read_dir(entry.path())?.next().is_none() {
                    std::fs::remove_dir(entry.path())?;
                }
                continue;
            }
            std::fs::remove_dir_all(entry.path())?;
        } else if prefix.is_empty() && keep_markers.iter().any(|keep| keep == name) {
            continue;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Extract `bytes` (an npm `.tar.gz` or a GitHub `.zip`) into `dest` per `extract`.
/// GitHub archives carry a single top-level `repo-<ref>/` directory, stripped
/// generically (its exact name depends on the ref).
fn extract_archive(
    bytes: &[u8],
    is_git: bool,
    extract: &Extract,
    dest: &Path,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A whole-directory extract owns its destination; a single-file extract may
    // share one (so don't wipe siblings).
    if !matches!(extract, Extract::File { .. }) {
        cache::clear_directory(dest)?;
    }

    if is_git {
        // Strip the single top-level dir, then apply the mode as a `Matching`
        // closure (`Select::All`/`Files` can't strip a variable prefix).
        fn strip_first(rel: &str) -> Option<&str> {
            rel.split_once('/')
                .map(|(_, rest)| rest)
                .filter(|r| !r.is_empty())
        }
        match extract {
            Extract::BrowserAssets => {
                let keep = move |rel: &str| strip_first(rel).and_then(keep_browser_assets);
                extract::zip(bytes, dest, None, extract::Select::Matching(&keep))?;
            }
            Extract::Full => {
                let keep = move |rel: &str| strip_first(rel).map(str::to_string);
                extract::zip(bytes, dest, None, extract::Select::Matching(&keep))?;
            }
            Extract::File { from, to } => {
                let keep = move |rel: &str| {
                    strip_first(rel).and_then(|r| (r == from.as_str()).then(|| to.clone()))
                };
                extract::zip(bytes, dest, None, extract::Select::Matching(&keep))?;
            }
            Extract::Filter(f) => {
                let f = *f;
                let keep = move |rel: &str| strip_first(rel).and_then(f);
                extract::zip(bytes, dest, None, extract::Select::Matching(&keep))?;
            }
        }
        return Ok(());
    }

    // npm tarballs nest everything under `package/`.
    let strip = Some("package/");
    match extract {
        Extract::BrowserAssets => {
            // Pre-extract package.json to drive the exports-aware keep filter,
            // then extract the kept files.
            extract::tar_gz(
                bytes,
                dest,
                strip,
                extract::Select::Files(&[("package.json", "package.json")]),
            )?;
            let pkg = PackageJson::from_path(&dest.join("package.json")).ok();
            let keep = keep_for(pkg);
            extract::tar_gz(bytes, dest, strip, extract::Select::Matching(&keep))?;
        }
        Extract::Full => {
            extract::tar_gz(bytes, dest, strip, extract::Select::All)?;
        }
        Extract::File { from, to } => {
            let files = [(from.as_str(), to.as_str())];
            extract::tar_gz(bytes, dest, strip, extract::Select::Files(&files))?;
        }
        Extract::Filter(f) => {
            let f = *f;
            let keep = move |rel: &str| f(rel);
            extract::tar_gz(bytes, dest, strip, extract::Select::Matching(&keep))?;
        }
    }
    Ok(())
}

/// Per-package keep-filter for [`Extract::BrowserAssets`]. When `pkg` is `Some`,
/// also keep `package.json` and every file the `exports`/`module`/`main` reference
/// (even under `src/`), then fall back to the browser-asset heuristic.
fn keep_for(pkg: Option<PackageJson>) -> impl Fn(&str) -> Option<String> {
    let referenced = pkg
        .as_ref()
        .map(PackageJson::referenced_paths)
        .unwrap_or_default();
    let keep_manifest = pkg.is_some();
    move |rel: &str| {
        if keep_manifest && rel == "package.json" {
            return Some(rel.to_string());
        }
        if referenced.iter().any(|target| path_covered(rel, target)) {
            return Some(rel.to_string());
        }
        keep_browser_assets(rel)
    }
}

/// Whether `rel` is covered by an `exports` target: an exact file, or any
/// `.js`/`.mjs` under a pattern directory (one ending `/`).
fn path_covered(rel: &str, target: &str) -> bool {
    if target.ends_with('/') {
        rel.starts_with(target) && (rel.ends_with(".js") || rel.ends_with(".mjs"))
    } else {
        rel == target
    }
}

/// Import-map entries for a vended spec, per its [`Imports`] strategy.
fn import_entries(spec: &PackageSpec, mount: &str, dest_dir: &Path) -> Vec<(String, String)> {
    match &spec.imports {
        Imports::None => Vec::new(),
        Imports::Explicit(list) => list
            .iter()
            .map(|(specifier, path)| {
                (
                    specifier.clone(),
                    format!("{mount}/{}/{}", spec.dir, path.trim_start_matches('/')),
                )
            })
            .collect(),
        Imports::Auto => {
            let pkg = PackageJson::from_path(&dest_dir.join("package.json")).ok();
            // A source dependency is imported by the name its manifest gave it, which is
            // `dir` — the repository it was fetched from is not what anyone writes in an
            // `import`. Registry and tarball specs keep naming themselves.
            let specifier = if spec.compile {
                spec.dir.as_str()
            } else {
                source_name(&spec.source)
            };
            auto_entries(pkg.as_ref(), specifier, &spec.dir, mount, dest_dir)
        }
    }
}

/// The package/repo name a spec resolves under (used for auto import-map keys).
fn source_name(source: &Source) -> &str {
    match source {
        Source::Npm { package, .. } => package,
        Source::Git { repo, .. } => repo,
        Source::Tarball { name, .. } => name,
    }
}

/// Derive import-map entries from a package's resolved `exports`. Bare + a `name/`
/// convenience prefix (so subpaths resolve to the vended files); non-identity
/// subpath remaps and `"./*"` pattern prefixes are mapped explicitly. Entries
/// whose target file isn't present are skipped.
/// Warn that a CommonJS entry could not be wrapped, so the reason surfaces at
/// vendor time instead of as a module error in the browser. With the `tracing`
/// feature off this is a no-op, like [`crate::core::reject`]'s counterpart.
#[cfg(feature = "tracing")]
fn warn_unwrappable_commonjs(entry: &str, uses: &str) {
    tracing::warn!(
        target: "web_modules",
        entry,
        uses,
        "CommonJS entry needs the Node environment, so it cannot be wrapped for the \
         browser; the import map still points at it and importing it will fail"
    );
}

/// No-op fallback when the `tracing` feature is off.
#[cfg(not(feature = "tracing"))]
fn warn_unwrappable_commonjs(_entry: &str, _uses: &str) {}

/// Suffix of a generated CommonJS→ESM wrapper.
const ESM_WRAPPER_SUFFIX: &str = ".esm-wrapper.js";

/// Node-only identifiers a browser cannot supply, so a body using them cannot be
/// made loadable by wrapping alone.
const NODE_ONLY: [&str; 6] = [
    "require(",
    "__dirname",
    "__filename",
    "process.",
    "global.",
    "Buffer",
];

/// Whether `body` already speaks ESM, in which case it needs nothing.
fn has_esm_syntax(body: &str) -> bool {
    body.lines().map(str::trim_start).any(|line| {
        line.starts_with("import ")
            || line.starts_with("import{")
            || line.starts_with("import(")
            || line.starts_with("export ")
            || line.starts_with("export{")
            || line.starts_with("export*")
    })
}

/// Whether `body` assigns CommonJS exports.
fn has_commonjs_exports(body: &str) -> bool {
    body.contains("module.exports") || body.contains("exports.")
}

/// Template for a generated wrapper; `{target}` and `{body}` are substituted.
///
/// A raw string rather than a `format!` continuation, so `cargo fmt` cannot fold
/// the lines together and bake its indentation into the emitted file.
const ESM_WRAPPER_TEMPLATE: &str = r#"// Generated by web_modules: `{target}` is CommonJS, which a browser cannot
// import as a module. Its body runs below inside a module/exports scaffold, and
// whatever it assigns becomes this module's default export.
//
// Only the default export is provided: what a CommonJS entry attaches to
// `exports` is not knowable statically, so named imports cannot be offered.
const module = { exports: {} };
const exports = module.exports;

{body}

export default module.exports;
"#;

/// The ESM wrapper for a CommonJS `body`, named after `target` in the comment.
fn esm_wrapper_source(target: &str, body: &str) -> String {
    ESM_WRAPPER_TEMPLATE
        .replace("{target}", target)
        .replace("{body}", body.trim_end())
}

/// Give a CommonJS-only package entry an ESM wrapper, returning the wrapper's
/// path relative to the package directory.
///
/// A bare specifier resolved to a `module.exports` file loads in a bundler and
/// fails in a browser — `does not provide an export named 'default'`, raised at
/// runtime, far from the vendoring that chose the file. Packages that ship no ESM
/// entry at all are otherwise unusable here, so wrap them.
///
/// Returns `None` when the entry is already ESM, or when the body reaches for the
/// Node environment; the latter is warned about, because leaving the import map
/// pointing at it only moves the failure into the browser.
fn write_esm_wrapper(pkg_dir: &Path, target: &str) -> Option<String> {
    if target.ends_with(ESM_WRAPPER_SUFFIX) {
        return None;
    }
    let path = pkg_dir.join(target);
    let body = fs::read_to_string(&path).ok()?;
    if has_esm_syntax(&body) || !has_commonjs_exports(&body) {
        return None;
    }
    if let Some(found) = NODE_ONLY.iter().find(|needle| body.contains(**needle)) {
        warn_unwrappable_commonjs(target, found.trim_end_matches('('));
        return None;
    }

    let wrapper = format!("{target}{ESM_WRAPPER_SUFFIX}");
    let contents = esm_wrapper_source(target, &body);
    // Idempotent: a repeat vendor of an unchanged package rewrites the same bytes.
    if fs::read_to_string(pkg_dir.join(&wrapper)).ok().as_deref() != Some(contents.as_str()) {
        fs::write(pkg_dir.join(&wrapper), &contents).ok()?;
    }
    Some(wrapper)
}

fn auto_entries(
    pkg: Option<&PackageJson>,
    package: &str,
    dir: &str,
    mount: &str,
    pkg_dir: &Path,
) -> Vec<(String, String)> {
    let Some(pkg) = pkg else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let (mut has_bare, mut has_prefix) = (false, false);
    for entry in pkg.entries() {
        match entry {
            Entry::Bare(target) => {
                if pkg_dir.join(&target).is_file() {
                    // A CommonJS entry gets an ESM wrapper, so the bare specifier
                    // resolves to something a browser can actually import.
                    let target = write_esm_wrapper(pkg_dir, &target).unwrap_or(target);
                    out.push((package.to_string(), join(mount, dir, &target)));
                    has_bare = true;
                }
            }
            Entry::Subpath { subpath, target } => {
                // Identity maps are covered by the `name/` prefix below.
                if target != subpath && pkg_dir.join(&target).is_file() {
                    out.push((format!("{package}/{subpath}"), join(mount, dir, &target)));
                }
            }
            Entry::Prefix { subpath, dir: tdir } => {
                out.push((format!("{package}/{subpath}"), join(mount, dir, &tdir)));
                has_prefix = true;
            }
        }
    }
    if has_bare && !has_prefix {
        out.push((format!("{package}/"), format!("{mount}/{dir}/")));
    }
    out
}

fn join(mount: &str, dir: &str, path: &str) -> String {
    format!("{mount}/{dir}/{}", path.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_handles_bare_scoped_and_ranged() {
        assert_eq!(PackageSpec::parse("lit").name(), "lit");
        assert_eq!(PackageSpec::parse("lit@^3").name(), "lit");
        assert_eq!(PackageSpec::parse("@lit/context").name(), "@lit/context");
        assert_eq!(PackageSpec::parse("@lit/context@^1").name(), "@lit/context");
        // The range is taken from after the last `@`; a bare name defaults to `*`.
        assert!(matches!(
            PackageSpec::parse("lit@^3").source,
            Source::Npm { ref range, .. } if range == "^3"
        ));
        assert!(matches!(
            PackageSpec::parse("lit").source,
            Source::Npm { ref range, .. } if range == "*"
        ));
    }

    /// The shape that motivates the wrapper: a package whose only entries are
    /// CommonJS, so a browser import of the bare specifier would fail at runtime.
    #[test]
    fn commonjs_entry_is_detected_and_esm_is_left_alone() {
        let cjs = "module.exports = function _atob(str) {\n  return atob(str)\n}\n";
        assert!(has_commonjs_exports(cjs));
        assert!(!has_esm_syntax(cjs));

        let esm = "export default function _atob(str) {\n  return atob(str);\n}\n";
        assert!(has_esm_syntax(esm));

        // A named-export ESM module must not be mistaken for CommonJS.
        let named = "const x = 1;\nexport { x };\n";
        assert!(has_esm_syntax(named));
    }

    #[test]
    fn wrapper_scaffolds_the_body_and_reexports_it() {
        let src = esm_wrapper_source("atob-browser.js", "module.exports = 42;");
        assert!(src.contains("const module = { exports: {} };"));
        assert!(src.contains("const exports = module.exports;"));
        assert!(src.contains("module.exports = 42;"));
        assert!(src.trim_end().ends_with("export default module.exports;"));
        // The comment names the file it stands in for.
        assert!(src.contains("atob-browser.js"));
    }

    #[test]
    fn wrapping_writes_a_sibling_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cjs.js"), "module.exports = 1;\n").unwrap();

        let first = write_esm_wrapper(dir.path(), "cjs.js").expect("wrapped");
        assert_eq!(first, format!("cjs.js{ESM_WRAPPER_SUFFIX}"));
        let once = std::fs::read_to_string(dir.path().join(&first)).unwrap();

        let again = write_esm_wrapper(dir.path(), "cjs.js").expect("wrapped again");
        assert_eq!(again, first);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(&again)).unwrap(),
            once
        );

        // Wrapping a wrapper would recurse; it must not happen.
        assert_eq!(write_esm_wrapper(dir.path(), &first), None);
    }

    #[test]
    fn an_esm_entry_is_not_wrapped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("esm.js"), "export default 1;\n").unwrap();
        assert_eq!(write_esm_wrapper(dir.path(), "esm.js"), None);
        assert!(!dir
            .path()
            .join(format!("esm.js{ESM_WRAPPER_SUFFIX}"))
            .exists());
    }

    /// Wrapping cannot conjure a CommonJS resolver, so a body reaching for the Node
    /// environment is refused rather than wrapped into something that breaks later.
    #[test]
    fn a_commonjs_body_needing_node_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            (
                "req.js",
                "const p = require('path');\nmodule.exports = p;\n",
            ),
            ("dir.js", "module.exports = __dirname;\n"),
            ("proc.js", "module.exports = process.platform;\n"),
        ] {
            std::fs::write(dir.path().join(name), body).unwrap();
            assert_eq!(write_esm_wrapper(dir.path(), name), None, "{name}");
        }
    }

    #[test]
    fn a_file_that_exports_nothing_is_not_wrapped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain.js"), "const x = 1;\n").unwrap();
        assert_eq!(write_esm_wrapper(dir.path(), "plain.js"), None);
    }

    #[test]
    fn keep_filter_picks_browser_assets() {
        assert_eq!(keep_browser_assets("index.js").as_deref(), Some("index.js"));
        assert_eq!(
            keep_browser_assets("dist/foo.mjs").as_deref(),
            Some("dist/foo.mjs")
        );
        assert_eq!(
            keep_browser_assets("scss/bootstrap.scss").as_deref(),
            Some("scss/bootstrap.scss")
        );
        assert!(keep_browser_assets("src/index.ts").is_none());
        assert!(keep_browser_assets("development/dev.js").is_none());
        assert!(keep_browser_assets("README.md").is_none());
    }

    #[test]
    fn keep_for_keeps_exports_targets_even_under_src() {
        // A CommonJS package whose ESM helper exports live under src/helpers/esm/.
        let pkg = PackageJson::from_json(
            r#"{"type":"commonjs","exports":{
                "./helpers/decorate":{"import":"./src/helpers/esm/decorate.js"},
                "./helpers/extends":{"import":"./src/helpers/esm/extends.js"}
            }}"#,
        )
        .unwrap();
        let keep = keep_for(Some(pkg));
        assert_eq!(
            keep("src/helpers/esm/decorate.js").as_deref(),
            Some("src/helpers/esm/decorate.js")
        );
        assert_eq!(keep("package.json").as_deref(), Some("package.json"));
        // A non-exported source file is still dropped by the heuristic.
        assert!(keep("src/index.ts").is_none());
    }

    #[test]
    fn auto_entries_lit_like_is_bare_plus_prefix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.js"), "export {}").unwrap();
        let pkg = PackageJson::from_json(
            r#"{"exports":{".":{"default":"./index.js"},"./decorators.js":{"default":"./decorators.js"}}}"#,
        )
        .unwrap();
        let entries = auto_entries(Some(&pkg), "lit", "lit", "/web_modules", dir.path());
        assert!(entries.contains(&("lit".into(), "/web_modules/lit/index.js".into())));
        assert!(entries.contains(&("lit/".into(), "/web_modules/lit/".into())));
        // The identity subpath `./decorators.js` is covered by the prefix, not listed.
        assert!(!entries.iter().any(|(s, _)| s == "lit/decorators.js"));
    }

    #[test]
    fn auto_entries_maps_remapped_subpaths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/helpers/esm")).unwrap();
        std::fs::write(dir.path().join("src/helpers/esm/decorate.js"), "export {}").unwrap();
        let pkg = PackageJson::from_json(
            r#"{"type":"commonjs","exports":{"./helpers/decorate":{"import":"./src/helpers/esm/decorate.js"}}}"#,
        )
        .unwrap();
        let entries = auto_entries(
            Some(&pkg),
            "@oxc-project/runtime",
            "@oxc-project/runtime",
            "/web_modules",
            dir.path(),
        );
        assert!(entries.contains(&(
            "@oxc-project/runtime/helpers/decorate".into(),
            "/web_modules/@oxc-project/runtime/src/helpers/esm/decorate.js".into(),
        )));
        // No bare entry and no convenience prefix for a `.`-less package.
        assert!(!entries.iter().any(|(s, _)| s == "@oxc-project/runtime"));
        assert!(!entries.iter().any(|(s, _)| s == "@oxc-project/runtime/"));
    }

    #[test]
    fn auto_entries_none_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(auto_entries(None, "d3", "d3", "/web_modules", dir.path()).is_empty());
    }

    #[test]
    fn explicit_imports_are_rooted_at_mount_dir() {
        let spec = PackageSpec::npm("jose", "^5").imports([("jose", "index.js"), ("jose/", "")]);
        let entries = import_entries(&spec, "/web_modules", Path::new("/nonexistent"));
        assert!(entries.contains(&("jose".into(), "/web_modules/jose/index.js".into())));
        assert!(entries.contains(&("jose/".into(), "/web_modules/jose/".into())));
    }

    #[test]
    fn no_imports_yields_no_entries() {
        let spec = PackageSpec::npm("bootstrap", "^5")
            .extract(Extract::Full)
            .no_imports();
        assert!(import_entries(&spec, "/web_modules", Path::new("/x")).is_empty());
    }

    #[test]
    fn missing_destination_invalidates_cache() {
        // A vendored asset whose marker still records the right version but whose
        // destination was deleted (e.g. someone wiped `node_modules/`) must be
        // treated as stale, so the next `vendor()` re-extracts it. This is the
        // invariant the build-script `rerun-if-changed` emission relies on to
        // self-heal a removed asset instead of leaving a silent runtime failure.
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join(".bootstrap.version");
        cache::write_marker(&marker, "5.3.8").unwrap();
        assert!(cache::marker_matches(&marker, "5.3.8"));

        let dest = tmp.path().join("bootstrap"); // never created
        assert!(
            !is_up_to_date(&marker, "5.3.8", &dest, &Extract::Full),
            "a missing destination must invalidate the cache even when the marker matches",
        );
    }

    #[test]
    fn git_spec_defaults() {
        let spec = PackageSpec::git("feathericons/feather", "v4.29.2");
        assert_eq!(spec.dir, "feather");
        assert!(matches!(spec.imports, Imports::None));
        match spec.source {
            Source::Git {
                owner,
                repo,
                reference,
            } => {
                assert_eq!(
                    (owner.as_str(), repo.as_str(), reference.as_str()),
                    ("feathericons", "feather", "v4.29.2")
                );
            }
            _ => panic!("expected git source"),
        }
    }

    #[test]
    fn tarball_spec_defaults() {
        let spec = PackageSpec::tarball(
            "@gronke/ui-components",
            "https://github.com/gronke/ui-components/releases/download/v0.1.0/gronke-ui-components-0.1.0.tgz",
        );
        assert_eq!(spec.dir, "@gronke/ui-components");
        assert!(matches!(spec.imports, Imports::Auto));
        assert!(matches!(spec.extract, Extract::BrowserAssets));
        assert_eq!(source_name(&spec.source), "@gronke/ui-components");
        match &spec.source {
            Source::Tarball { name, url } => {
                assert_eq!(name, "@gronke/ui-components");
                assert!(url.ends_with("gronke-ui-components-0.1.0.tgz"));
            }
            _ => panic!("expected a tarball source"),
        }
    }

    #[test]
    fn tarball_url_routes_before_github() {
        // A GitHub Release-asset URL must vend as the packed tarball, not as a repo
        // source archive — the `github.com` branch would otherwise capture it.
        let asset = "https://github.com/gronke/ui-components/releases/download/v0.1.0/gronke-ui-components-0.1.0.tgz";
        assert!(matches!(
            dep_to_spec("@gronke/ui-components", asset).unwrap().source,
            Source::Tarball { .. }
        ));
        // A non-GitHub tarball host still routes to a tarball.
        assert!(matches!(
            dep_to_spec("pkg", "https://cdn.example.com/pkg-1.0.0.tgz")
                .unwrap()
                .source,
            Source::Tarball { .. }
        ));
        // The plain `github:` shorthand still routes to a git source.
        assert!(matches!(
            dep_to_spec("feather", "github:feathericons/feather#v4.29.2")
                .unwrap()
                .source,
            Source::Git { .. }
        ));
    }

    #[test]
    fn auto_entries_tarball_dist_shape() {
        // The published dist has subpath exports and no `.` entry, so the specifier
        // is the exact `@scope/pkg/<subpath>` with no bare / no `name/` prefix.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("components")).unwrap();
        std::fs::write(
            dir.path().join("components/input-secret.js"),
            "customElements.define('input-secret', class extends HTMLElement {});",
        )
        .unwrap();
        let pkg = PackageJson::from_json(
            r#"{"exports":{"./input-secret.js":{"default":"./components/input-secret.js"}}}"#,
        )
        .unwrap();
        let entries = auto_entries(
            Some(&pkg),
            "@gronke/ui-components",
            "@gronke/ui-components",
            "/web_modules",
            dir.path(),
        );
        assert!(entries.contains(&(
            "@gronke/ui-components/input-secret.js".into(),
            "/web_modules/@gronke/ui-components/components/input-secret.js".into(),
        )));
        assert!(!entries.iter().any(|(s, _)| s == "@gronke/ui-components"));
        assert!(!entries.iter().any(|(s, _)| s == "@gronke/ui-components/"));
    }

    #[test]
    fn specs_from_package_json_reads_dependencies_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{
                "dependencies": {
                    "lit": "^3",
                    "feather": "github:feathericons/feather#v4.29.2",
                    "local": "file:../x"
                },
                "devDependencies": { "typescript": "^5" }
            }"#,
        )
        .unwrap();
        let specs = specs_from_package_json(&p).unwrap();
        let names: Vec<&str> = specs.iter().map(PackageSpec::name).collect();
        assert!(names.contains(&"lit"));
        assert!(names.contains(&"feather"));
        assert!(!names.contains(&"local"), "file: protocol skipped");
        assert!(!names.contains(&"typescript"), "devDependencies not vended");

        let lit = specs.iter().find(|s| s.name() == "lit").unwrap();
        match &lit.source {
            Source::Npm { range, .. } => assert_eq!(range, "^3", "range preserved verbatim"),
            _ => panic!("lit should be an npm source"),
        }
        let feather = specs.iter().find(|s| s.name() == "feather").unwrap();
        assert!(matches!(feather.source, Source::Git { .. }));
    }

    #[test]
    fn sections_can_opt_into_devdependencies() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"lit":"^3"},"devDependencies":{"typescript":"^5"}}"#,
        )
        .unwrap();
        let specs =
            specs_from_package_json_sections(&p, &["dependencies", "devDependencies"]).unwrap();
        let names: Vec<&str> = specs.iter().map(PackageSpec::name).collect();
        assert!(names.contains(&"lit") && names.contains(&"typescript"));
    }

    #[test]
    fn web_dependencies_whitelist_narrows_to_named_subset() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{
                "dependencies": { "lit": "^3", "lit-html": "^3", "pg": "^8" },
                "web_modules": { "webDependencies": ["lit", "lit-html"] }
            }"#,
        )
        .unwrap();
        let specs = specs_from_package_json(&p).unwrap();
        let names: Vec<&str> = specs.iter().map(PackageSpec::name).collect();
        assert_eq!(names, vec!["lit", "lit-html"], "whitelist order preserved");
        assert!(
            !names.contains(&"pg"),
            "server-only dep left out of the browser vend"
        );
    }

    /// The distinction the feature turns on: a package that publishes only sources is
    /// empty under the browser-asset filter, and whole under this one.
    fn write_tsconfig(dir: &Path, body: &str) {
        std::fs::write(dir.join("tsconfig.json"), body).unwrap();
    }

    /// Reproducing tsc's own mapping is what lets a compiled package keep the entry its
    /// manifest already declares, so no entry has to be guessed. The shape esptool-js
    /// ships: an `outDir` with the root implied by `include`.
    #[test]
    fn the_plan_carries_the_layout_to_reproduce() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        write_tsconfig(
            dir.path(),
            r#"{"compilerOptions":{"outDir":"./lib"},"include":["src/**/*"]}"#,
        );
        let plan = source_plan(dir.path(), "pkg").unwrap();
        assert_eq!(
            plan.layout.root, None,
            "inferred from the inputs, not the globs"
        );
        assert_eq!(plan.layout.out, "lib");
    }

    #[test]
    fn without_a_tsconfig_a_package_compiles_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let plan = source_plan(dir.path(), "pkg").unwrap();
        assert_eq!(plan.layout.root, None);
        assert_eq!(plan.layout.out, "");
        assert!(
            !plan.defines_class_fields,
            "tsc's own default below ES 2022"
        );
        assert!(!plan.legacy_decorators);
    }

    /// A dependency is compiled the way its own build compiles it. The zero-config default
    /// here is the Lit preset, and handing that to a package targeting ES 2022 would give
    /// it assignment-style class fields — observably different code, since a defined field
    /// shadows an inherited getter where an assigned one calls its setter.
    #[test]
    fn the_plan_takes_emit_semantics_from_the_dependency() {
        let dir = tempfile::tempdir().unwrap();
        let plan = |body: &str| {
            write_tsconfig(dir.path(), body);
            source_plan(dir.path(), "pkg").unwrap()
        };

        let es2019 = plan(r#"{"compilerOptions":{"target":"ES2019"}}"#);
        assert!(!es2019.defines_class_fields);
        assert!(!es2019.legacy_decorators);

        let es2022 = plan(r#"{"compilerOptions":{"target":"ES2022"}}"#);
        assert!(es2022.defines_class_fields);

        let lit = plan(r#"{"compilerOptions":{"experimentalDecorators":true,"target":"ES2019"}}"#);
        assert!(lit.legacy_decorators);
        assert!(!lit.defines_class_fields);

        // Standard decorators with define semantics: neither of the two old presets.
        let modern = plan(r#"{"compilerOptions":{"target":"ESNext"}}"#);
        assert!(modern.defines_class_fields);
        assert!(!modern.legacy_decorators);
    }

    /// Aliases, inherited configs and decorator metadata are refused rather than guessed:
    /// the emitted specifiers would not resolve in a browser, the real options are
    /// elsewhere, and the metadata needs a runtime this compiler does not emit.
    #[test]
    fn the_plan_refuses_what_it_cannot_honour() {
        let dir = tempfile::tempdir().unwrap();
        for body in [
            r#"{"compilerOptions":{"paths":{"@/*":["src/*"]}}}"#,
            r#"{"compilerOptions":{"baseUrl":"."}}"#,
            r#"{"compilerOptions":{"emitDecoratorMetadata":true}}"#,
            r#"{"extends":"../base.json"}"#,
            r#"{"extends":["../base.json","../more.json"]}"#,
        ] {
            write_tsconfig(dir.path(), body);
            let Err(err) = source_plan(dir.path(), "acme-ui") else {
                panic!("expected a refusal for {body}");
            };
            assert!(
                err.to_string().contains("acme-ui"),
                "names the package: {err}"
            );
        }
    }

    /// A manifest pointing at `lib/index.mjs` finds nothing if `.mts` emits `.js`.
    #[test]
    fn the_module_format_travels_with_the_extension() {
        for (source, emitted) in [
            ("src/index.ts", Some("js")),
            ("src/app.tsx", Some("js")),
            ("src/index.mts", Some("mjs")),
            // CommonJS source, refused rather than renamed.
            ("src/index.cts", None),
            // A declaration has no runtime form, in any of its three module spellings.
            ("src/index.d.ts", None),
            ("src/index.d.mts", None),
            ("src/index.d.cts", None),
            ("src/data.json", None),
            ("src/style.css", None),
        ] {
            assert_eq!(compiled_extension(Path::new(source)), emitted, "{source}");
        }
        // A declaration is not a source, whichever module form it declares.
        for name in ["index.d.ts", "index.d.mts", "index.d.cts"] {
            assert_eq!(keep_sources(&format!("src/{name}")), None, "{name}");
        }
        // The filter carries every source form through, so an unsupported one is refused
        // with a message rather than vanishing from the tree.
        for ext in ["ts", "tsx", "mts", "cts"] {
            assert!(
                keep_sources(&format!("src/index.{ext}")).is_some(),
                ".{ext} reaches the compiler"
            );
        }
    }

    /// The same commit through a different compiler is a different tree, so a compiled
    /// destination's key has to move when the compiler does — otherwise an upgrade carrying
    /// a transform fix leaves the old JavaScript in place, pinned and apparently fresh.
    #[test]
    fn a_compiled_destination_keys_on_the_compiler_too() {
        let commit = "433170bc68fe2339a2f5b465f8839ae2370f96a0".to_string();
        assert_eq!(
            compiled_key(commit.clone(), false),
            commit,
            "an uncompiled tree is the archive and nothing else"
        );
        let compiled = compiled_key(commit.clone(), true);
        assert_ne!(compiled, commit);
        assert!(compiled.starts_with(&commit), "the source is still named");
        assert!(
            compiled.contains(env!("CARGO_PKG_VERSION")),
            "and the compiler with it: {compiled}"
        );
    }

    /// A package declaring `exports: ./lib/index.mjs` finds nothing if `.mts` emits `.js`,
    /// and `auto_entries` then skips the package rather than resolving it.
    #[test]
    #[cfg(feature = "typescript")]
    fn compiling_a_tree_preserves_the_module_format() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"outDir":"lib","rootDir":"src","target":"ES2019"}}"#,
        );
        std::fs::write(pkg.join("src/index.mts"), "export const a: number = 1;").unwrap();
        std::fs::write(pkg.join("src/plain.ts"), "export const c: boolean = true;").unwrap();
        std::fs::write(pkg.join("src/data.json"), "{}").unwrap();

        compile_source_tree(pkg, "acme").unwrap();

        assert!(pkg.join("lib/index.mjs").is_file(), "an .mts emits .mjs");
        assert!(pkg.join("lib/plain.js").is_file());
        assert!(pkg.join("lib/data.json").is_file(), "assets travel along");
        assert!(
            !pkg.join("lib/index.js").exists(),
            "and not under the wrong name"
        );
        assert!(
            !pkg.join("src").exists(),
            "the TypeScript was an intermediate"
        );
    }

    /// `files` and `include` name root files, not the whole program: a file a root imports
    /// is in it too, and `exclude` does not take it out. Skipping it would emit an importer
    /// whose import was then deleted by the cleanup — a package that cannot load.
    #[test]
    #[cfg(feature = "typescript")]
    fn an_imported_source_joins_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src/deep")).unwrap();
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"rootDir":"src","outDir":"lib","target":"ES2019"},
                "files":["src/index.ts"],"exclude":["src/deep/**"]}"#,
        );
        // The one root, importing a sibling `files` does not name and a file `exclude` does.
        std::fs::write(
            pkg.join("src/index.ts"),
            "export { util } from \"./util.js\";\nexport { deep } from \"./deep/inner.js\";",
        )
        .unwrap();
        std::fs::write(pkg.join("src/util.ts"), "export const util: number = 1;").unwrap();
        std::fs::write(
            pkg.join("src/deep/inner.ts"),
            "export const deep: number = 2;",
        )
        .unwrap();
        // And one nobody imports, which the exclude keeps out.
        std::fs::write(
            pkg.join("src/deep/scratch.ts"),
            "export const s: number = 3;",
        )
        .unwrap();

        compile_source_tree(pkg, "acme").unwrap();

        assert!(pkg.join("lib/index.js").is_file(), "the root");
        assert!(
            pkg.join("lib/util.js").is_file(),
            "a sibling `files` does not name, reached by import"
        );
        assert!(
            pkg.join("lib/deep/inner.js").is_file(),
            "and one `exclude` names, reached the same way"
        );
        assert!(
            !pkg.join("lib/deep/scratch.js").exists(),
            "while an excluded file nobody imports stays out"
        );
        // Every import the emitted root makes resolves to a file beside it.
        let emitted = std::fs::read_to_string(pkg.join("lib/index.js")).unwrap();
        for specifier in ["./util.js", "./deep/inner.js"] {
            assert!(emitted.contains(specifier), "{specifier} in {emitted}");
            assert!(
                pkg.join("lib")
                    .join(specifier.trim_start_matches("./"))
                    .is_file(),
                "{specifier} resolves"
            );
        }
    }

    /// `tsc` infers a missing `rootDir` from the input files, so a package whose sources all
    /// sit one level down emits from there — `lib/index.js`, not `lib/deep/index.js`. Taking
    /// the glob's own prefix would put the output where the manifest does not point.
    #[test]
    #[cfg(feature = "typescript")]
    fn the_inferred_root_follows_the_inputs_not_the_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src/deep")).unwrap();
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"outDir":"lib","target":"ES2019"},"include":["src/**/*.ts"]}"#,
        );
        std::fs::write(pkg.join("src/deep/index.ts"), "export const a: number = 1;").unwrap();
        std::fs::write(pkg.join("src/deep/util.ts"), "export const b: number = 2;").unwrap();

        compile_source_tree(pkg, "acme").unwrap();

        assert!(pkg.join("lib/index.js").is_file(), "rooted at src/deep");
        assert!(pkg.join("lib/util.js").is_file());
        assert!(
            !pkg.join("lib/deep/index.js").exists(),
            "and not at the glob's prefix"
        );
    }

    /// `tsc` refuses a program with an input outside its `rootDir` (TS6059). Emitting the
    /// importer and dropping the import would be worse than refusing: the output would load
    /// and then fail at its first specifier.
    #[test]
    #[cfg(feature = "typescript")]
    fn a_reachable_source_outside_an_explicit_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::create_dir_all(pkg.join("shared")).unwrap();
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"rootDir":"src","outDir":"lib","target":"ES2022"},
                "files":["src/index.ts"]}"#,
        );
        std::fs::write(
            pkg.join("src/index.ts"),
            "export { util } from \"../shared/util.js\";",
        )
        .unwrap();
        std::fs::write(pkg.join("shared/util.ts"), "export const util: number = 1;").unwrap();

        let Err(err) = compile_source_tree(pkg, "acme") else {
            panic!("expected a refusal");
        };
        let message = err.to_string();
        assert!(
            message.contains("shared/util.ts"),
            "names the file: {message}"
        );
        assert!(message.contains("rootDir"), "and why: {message}");
        assert!(
            !pkg.join("lib/index.js").exists(),
            "and nothing was emitted before failing"
        );
    }

    /// A package whose sources name `.ts` paths needs `rewriteRelativeImportExtensions` for
    /// its output to resolve, since the source it names is compiled and then removed.
    #[test]
    #[cfg(feature = "typescript")]
    fn a_typescript_specifier_is_rewritten_when_the_config_says_so() {
        let write = |pkg: &Path, rewrite: bool| {
            std::fs::create_dir_all(pkg.join("src")).unwrap();
            write_tsconfig(
                pkg,
                &format!(
                    r#"{{"compilerOptions":{{"rootDir":"src","outDir":"lib","target":"ES2022",
                        "rewriteRelativeImportExtensions":{rewrite}}}}}"#
                ),
            );
            std::fs::write(
                pkg.join("src/index.ts"),
                "export { util } from \"./util.ts\";",
            )
            .unwrap();
            std::fs::write(pkg.join("src/util.ts"), "export const util: number = 1;").unwrap();
        };

        let on = tempfile::tempdir().unwrap();
        write(on.path(), true);
        compile_source_tree(on.path(), "acme").unwrap();
        let emitted = std::fs::read_to_string(on.path().join("lib/index.js")).unwrap();
        assert!(
            emitted.contains("./util.js") && !emitted.contains("./util.ts"),
            "the emitted specifier names what was emitted: {emitted}"
        );
        assert!(on.path().join("lib/util.js").is_file(), "and it is there");

        // Without it, the specifier would survive into output that cannot load, so the
        // package is refused while the reason is still visible.
        let off = tempfile::tempdir().unwrap();
        write(off.path(), false);
        let Err(err) = compile_source_tree(off.path(), "acme") else {
            panic!("expected a refusal");
        };
        assert!(
            err.to_string().contains("rewriteRelativeImportExtensions"),
            "says what the package needs: {err}"
        );
    }

    /// The module format travels with the extension in both directions: `./foo.mjs` is
    /// written by `foo.mts` and by nothing else.
    #[test]
    #[cfg(feature = "typescript")]
    fn an_import_resolves_to_the_source_of_its_own_format() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"rootDir":"src","outDir":"lib","target":"ES2022"},
                "files":["src/index.mts"]}"#,
        );
        std::fs::write(
            pkg.join("src/index.mts"),
            "export { pick } from \"./foo.mjs\";",
        )
        .unwrap();
        // Both spellings exist; only the `.mts` one is what `./foo.mjs` names.
        std::fs::write(pkg.join("src/foo.mts"), "export const pick = \"mts\";").unwrap();
        std::fs::write(pkg.join("src/foo.ts"), "export const pick = \"ts\";").unwrap();

        compile_source_tree(pkg, "acme").unwrap();

        let emitted = std::fs::read_to_string(pkg.join("lib/foo.mjs")).unwrap();
        assert!(emitted.contains("mts"), "the .mts was compiled: {emitted}");
        assert!(
            !pkg.join("lib/foo.js").exists(),
            "and the .ts sibling was never in the program"
        );
    }

    /// The program is the *runtime* graph: a type-only import is erased before the imports
    /// are read, so the file it named is neither compiled nor shipped. Nothing loads it, and
    /// declarations are excluded from root inference by `tsc` too.
    #[test]
    #[cfg(feature = "typescript")]
    fn a_type_only_import_does_not_join_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"rootDir":"src","outDir":"lib","target":"ES2022"},
                "files":["src/index.ts"]}"#,
        );
        std::fs::write(
            pkg.join("src/index.ts"),
            "import type { Shape } from \"./shapes.js\";\nexport const one: Shape = 1 as Shape;",
        )
        .unwrap();
        std::fs::write(pkg.join("src/shapes.ts"), "export type Shape = number;").unwrap();

        compile_source_tree(pkg, "acme").unwrap();

        assert!(pkg.join("lib/index.js").is_file());
        assert!(
            !pkg.join("lib/shapes.js").exists(),
            "a type-only import has no runtime file to emit"
        );
        let emitted = std::fs::read_to_string(pkg.join("lib/index.js")).unwrap();
        assert!(
            !emitted.contains("shapes"),
            "and the emitted module does not reach for it: {emitted}"
        );
    }

    /// The feature rests on the manifest still pointing at the output. When the layout the
    /// config describes is not the one the package was published with, `auto_entries` would
    /// drop the package from the import map without a word; this says so instead.
    #[test]
    #[cfg(feature = "typescript")]
    fn a_manifest_pointing_at_nothing_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"acme","module":"lib/index.js"}"#,
        )
        .unwrap();
        // `rootDir: "."` is TypeScript 6's default, and puts the output a level deeper.
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"rootDir":".","outDir":"lib","target":"ES2022"}}"#,
        );
        std::fs::write(pkg.join("src/index.ts"), "export const a: number = 1;").unwrap();

        let Err(err) = compile_source_tree(pkg, "acme") else {
            panic!("expected the mismatch to be reported");
        };
        let message = err.to_string();
        assert!(
            message.contains("lib/index.js"),
            "names the target: {message}"
        );
        assert!(message.contains("rootDir"), "and the way out: {message}");
        assert!(
            pkg.join("lib/src/index.js").is_file(),
            "the output it did produce is still there to look at"
        );
    }

    /// `.cts` is CommonJS source by definition. Renaming it `.cjs` while leaving its
    /// `export`s alone produces neither the package's own output nor anything a browser
    /// loads, so it is refused while it is still explicable.
    #[test]
    #[cfg(feature = "typescript")]
    fn commonjs_source_is_refused_rather_than_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"rootDir":"src","outDir":"lib","target":"ES2019"}}"#,
        );
        std::fs::write(pkg.join("src/index.ts"), "export const a: number = 1;").unwrap();
        std::fs::write(pkg.join("src/legacy.cts"), "export const b = 2;").unwrap();

        let Err(err) = compile_source_tree(pkg, "acme") else {
            panic!("expected a refusal");
        };
        let message = err.to_string();
        assert!(message.contains("acme"), "names the package: {message}");
        assert!(message.contains("legacy.cts"), "and the file: {message}");
    }

    /// A config declaring CommonJS output is refused too, before any file is read.
    #[test]
    fn a_commonjs_config_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for body in [
            r#"{"compilerOptions":{"module":"CommonJS"}}"#,
            r#"{"compilerOptions":{"module":"NodeNext"}}"#,
            r#"{"compilerOptions":{"target":"ES5"}}"#,
        ] {
            write_tsconfig(dir.path(), body);
            let Err(err) = source_plan(dir.path(), "acme-ui") else {
                panic!("expected a refusal for {body}");
            };
            assert!(err.to_string().contains("CommonJS"), "says why: {err}");
        }
    }

    /// A dependency gets its own emit semantics. Below ES 2022 a field declared without an
    /// initializer emits nothing, as tsc does; from ES 2022 it is defined on the instance.
    #[test]
    #[cfg(feature = "typescript")]
    fn compiling_a_tree_uses_the_dependency_semantics_not_this_project_s() {
        let source = "export class A { declared: number; ready = 1; }";
        let compile = |tsconfig: &str| {
            let dir = tempfile::tempdir().unwrap();
            let pkg = dir.path();
            std::fs::create_dir_all(pkg.join("src")).unwrap();
            write_tsconfig(pkg, tsconfig);
            std::fs::write(pkg.join("src/index.ts"), source).unwrap();
            compile_source_tree(pkg, "acme").unwrap();
            std::fs::read_to_string(pkg.join("lib/index.js")).unwrap()
        };

        let assigned =
            compile(r#"{"compilerOptions":{"outDir":"lib","rootDir":"src","target":"ES2019"}}"#);
        assert!(
            !assigned.contains("declared"),
            "assignment semantics drop an uninitialized field: {assigned}"
        );

        let defined =
            compile(r#"{"compilerOptions":{"outDir":"lib","rootDir":"src","target":"ES2022"}}"#);
        assert!(
            defined.contains("declared"),
            "define semantics keep it, as ES 2022 specifies: {defined}"
        );
    }

    /// The config is archive content, so a root that leaves the package must never be
    /// walked, written beside, or — as the cleanup does — deleted.
    #[test]
    #[cfg(feature = "typescript")]
    fn a_root_outside_the_package_is_refused_before_anything_is_touched() {
        let outer = tempfile::tempdir().unwrap();
        let sibling = outer.path().join("other-package");
        std::fs::create_dir_all(sibling.join("lib")).unwrap();
        std::fs::write(sibling.join("lib/keep.js"), "export {};").unwrap();

        let pkg = outer.path().join("acme");
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(pkg.join("src/index.ts"), "export const a: number = 1;").unwrap();
        write_tsconfig(
            &pkg,
            r#"{"compilerOptions":{"rootDir":"..","outDir":"lib"}}"#,
        );

        let Err(err) = compile_source_tree(&pkg, "acme") else {
            panic!("expected a refusal");
        };
        assert!(err.to_string().contains("acme"), "names the package: {err}");
        assert!(
            sibling.join("lib/keep.js").exists(),
            "and the neighbour it would have deleted is still there"
        );
    }

    /// An `outDir` inside the `rootDir` puts the output where the cleanup sweeps.
    #[test]
    #[cfg(feature = "typescript")]
    fn output_nested_in_the_source_root_survives_the_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"rootDir":"src","outDir":"src/out"}}"#,
        );
        std::fs::write(pkg.join("src/index.ts"), "export const a: number = 1;").unwrap();

        compile_source_tree(pkg, "acme").unwrap();
        assert!(pkg.join("src/out/index.js").is_file(), "the output is kept");
        assert!(
            !pkg.join("src/index.ts").exists(),
            "and the source it came from is gone"
        );
    }

    /// A package that excludes its own dev sources does not build them, and neither does
    /// this — one of them failing to compile would otherwise fail the whole vendoring.
    #[test]
    #[cfg(feature = "typescript")]
    fn only_the_declared_inputs_are_compiled() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        std::fs::create_dir_all(pkg.join("src/dev")).unwrap();
        write_tsconfig(
            pkg,
            r#"{"compilerOptions":{"rootDir":"src","outDir":"lib"},
                "include":["src/**/*"],"exclude":["src/dev/**"]}"#,
        );
        std::fs::write(pkg.join("src/index.ts"), "export const a: number = 1;").unwrap();
        std::fs::write(
            pkg.join("src/dev/scratch.ts"),
            "export const b: number = 2;",
        )
        .unwrap();

        compile_source_tree(pkg, "acme").unwrap();
        assert!(pkg.join("lib/index.js").is_file());
        assert!(
            !pkg.join("lib/dev/scratch.js").exists(),
            "an excluded source is not an input"
        );
    }

    /// A `.tsx` source compiles, so a config naming a JSX mode or factory would be emitted
    /// against something other than what it asked for.
    #[test]
    fn jsx_settings_are_refused_rather_than_ignored() {
        let dir = tempfile::tempdir().unwrap();
        for body in [
            r#"{"compilerOptions":{"jsx":"react-jsx"}}"#,
            r#"{"compilerOptions":{"jsxImportSource":"preact"}}"#,
            r#"{"compilerOptions":{"jsxFactory":"h"}}"#,
            r#"{"compilerOptions":{"jsxFragmentFactory":"Fragment"}}"#,
        ] {
            write_tsconfig(dir.path(), body);
            let Err(err) = source_plan(dir.path(), "acme-ui") else {
                panic!("expected a refusal for {body}");
            };
            assert!(
                err.to_string().contains("acme-ui"),
                "names the package: {err}"
            );
        }
    }

    /// The repository furniture a whole-repo archive carries is not the package.
    /// The distinction the git cache key turns on: a commit names one tree forever, a
    /// branch or tag can be repointed, and keying on the name kept a stale tree.
    #[test]
    fn only_a_commit_id_counts_as_immutable() {
        assert!(is_commit_ref("433170bc68fe2339a2f5b465f8839ae2370f96a0"));
        assert!(is_commit_ref(&"a".repeat(40)));

        assert!(!is_commit_ref("gronke"), "a branch moves");
        assert!(!is_commit_ref("v0.6.1"), "a tag can be repointed");
        assert!(!is_commit_ref("HEAD"));
        assert!(!is_commit_ref("main"));
        // An abbreviated id is not enough to be sure, so it is treated as mutable.
        assert!(!is_commit_ref("433170b"));
        // Right length, wrong alphabet.
        assert!(!is_commit_ref(&"z".repeat(40)));
    }

    #[test]
    fn content_key_tracks_the_bytes() {
        assert_eq!(content_key(b"abc"), content_key(b"abc"));
        assert_ne!(content_key(b"abc"), content_key(b"abd"));
        // Position-weighted, so a reordering is not a collision.
        assert_ne!(content_key(b"ab"), content_key(b"ba"));
        assert_eq!(content_key(b"").len(), 16, "fixed-width hex");
    }

    #[test]
    fn keep_sources_drops_repo_furniture() {
        for path in [
            "examples/typescript/src/index.ts",
            "test/spec.ts",
            "tests/spec.ts",
            "docs/guide.js",
            ".github/workflows/ci.yml",
            ".vscode/settings.json",
            ".devcontainer/devcontainer.json",
            "package-lock.json",
        ] {
            assert_eq!(
                keep_sources(path),
                None,
                "{path} is not part of the package"
            );
        }
        // The package's own sources still come through.
        assert!(keep_sources("src/index.ts").is_some());
        assert!(keep_sources("package.json").is_some());
    }

    #[test]
    fn keep_sources_keeps_the_src_tree_that_browser_assets_drops() {
        assert_eq!(keep_browser_assets("src/esploader.ts"), None);
        assert_eq!(
            keep_sources("src/esploader.ts").as_deref(),
            Some("src/esploader.ts")
        );
        assert_eq!(
            keep_sources("src/targets/stub_flasher/stub_flasher_32s3.json").as_deref(),
            Some("src/targets/stub_flasher/stub_flasher_32s3.json")
        );
        assert_eq!(
            keep_sources("package.json").as_deref(),
            Some("package.json")
        );
        assert_eq!(
            keep_sources("src/app.scss").as_deref(),
            Some("src/app.scss")
        );

        // A declaration has no runtime form, and nothing under these trees is shipped.
        assert_eq!(keep_sources("src/index.d.ts"), None);
        assert_eq!(keep_sources("node_modules/lit/index.js"), None);
        assert_eq!(keep_sources("development/debug.js"), None);
        // Not part of a module graph.
        assert_eq!(keep_sources("README.md"), None);
        // A licence covers the sources beside it, so it travels with them.
        assert_eq!(keep_sources("LICENSE").as_deref(), Some("LICENSE"));
        assert_eq!(keep_sources("LICENSE-MIT").as_deref(), Some("LICENSE-MIT"));
        assert_eq!(keep_sources("NOTICE").as_deref(), Some("NOTICE"));
    }

    #[test]
    fn source_dependencies_take_the_dependency_key_as_their_dir() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"acme-ui":"github:acme/ui#v2","pako":"^2"},
                "web_modules":{"sourceDependencies":["acme-ui"]}}"#,
        )
        .unwrap();

        let specs = source_specs_from_package_json(&p).unwrap();
        assert_eq!(specs.len(), 1);
        // The dependency key names the directory and the specifier, not the repo.
        assert_eq!(specs[0].dir, "acme-ui");
        assert!(specs[0].compile, "its TypeScript is compiled by vendoring");
        assert!(matches!(specs[0].extract, Extract::Filter(_)));
        // A git spec derives no entries by default; compiling into the layout the manifest
        // declares is what makes the ordinary auto-derivation correct here.
        assert!(matches!(specs[0].imports, Imports::Auto));
    }

    /// The CLI assembles its spec list from `specs_from_package_json` and
    /// `source_specs_from_package_json` and dedupes by spec name. A git spec is named after the
    /// repository and a source spec after the dependency key, so those names differ and a source
    /// dependency left in the vend list survives the dedupe — fetching one repository twice,
    /// into `fork-esptool-js/` beside `esptool-js/`.
    #[test]
    fn the_vend_list_skips_a_source_dependency_in_both_forms() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        let manifest = r#"{"dependencies":{"esptool-js":"github:gronke/fork-esptool-js#v1","pako":"^2"},
                "web_modules":{"sourceDependencies":["esptool-js"]}}"#;
        std::fs::write(&p, manifest).unwrap();
        assert_eq!(
            specs_from_package_json(&p)
                .unwrap()
                .iter()
                .map(PackageSpec::name)
                .collect::<Vec<_>>(),
            vec!["pako"],
            "the git dep is the source dep, vendored from its own spec"
        );

        // The same rule under a webDependencies whitelist that names it too.
        let p = dir.path().join("whitelisted.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"esptool-js":"github:gronke/fork-esptool-js#v1","pako":"^2"},
                "web_modules":{"sourceDependencies":["esptool-js"],
                               "webDependencies":["esptool-js","pako"]}}"#,
        )
        .unwrap();
        assert_eq!(
            specs_from_package_json(&p)
                .unwrap()
                .iter()
                .map(PackageSpec::name)
                .collect::<Vec<_>>(),
            vec!["pako"]
        );
    }

    /// Fetching the same package into two trees would be worse than either, so the
    /// vend list and the source list must not overlap.
    #[test]
    fn a_source_dependency_is_not_also_vended() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"acme-ui":"github:acme/ui#v2","pako":"^2"},
                "web_modules":{"sourceDependencies":["acme-ui"]}}"#,
        )
        .unwrap();

        let (specs, _mounts) = read_package_json(&p).unwrap();
        assert_eq!(
            specs.iter().map(PackageSpec::name).collect::<Vec<_>>(),
            vec!["pako"],
            "a source dependency is vendored from its own spec"
        );
    }

    #[test]
    fn source_dependencies_without_the_key_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(&p, r#"{"dependencies":{"pako":"^2"}}"#).unwrap();
        assert!(source_specs_from_package_json(&p).unwrap().is_empty());
    }

    #[test]
    fn source_dependencies_naming_a_missing_dep_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"pako":"^2"},"web_modules":{"sourceDependencies":["nope"]}}"#,
        )
        .unwrap();
        let Err(err) = source_specs_from_package_json(&p) else {
            panic!("expected an error for the missing dep");
        };
        assert!(err.to_string().contains("nope"), "error names it: {err}");
    }

    /// Only a git source has sources to build; a registry range does not, and a silent
    /// skip would leave the app with an import nothing resolves.
    #[test]
    fn a_source_dependency_that_is_not_a_git_reference_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"pako":"^2"},"web_modules":{"sourceDependencies":["pako"]}}"#,
        )
        .unwrap();
        let Err(err) = source_specs_from_package_json(&p) else {
            panic!("expected an error for the registry range");
        };
        let msg = err.to_string();
        assert!(msg.contains("pako") && msg.contains("git"), "{msg}");
    }

    #[test]
    fn source_dependencies_must_be_an_array() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"a":"github:o/r#v1"},"web_modules":{"sourceDependencies":"a"}}"#,
        )
        .unwrap();
        assert!(source_specs_from_package_json(&p).is_err());
    }

    #[test]
    fn web_dependencies_whitelist_naming_a_missing_dep_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"lit":"^3"},"web_modules":{"webDependencies":["lit","nope"]}}"#,
        )
        .unwrap();
        let Err(err) = specs_from_package_json(&p) else {
            panic!("expected an error for the missing dep");
        };
        assert!(
            err.to_string().contains("nope"),
            "error names the missing dep: {err}"
        );
    }

    #[test]
    fn empty_web_dependencies_vends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"lit":"^3"},"web_modules":{"webDependencies":[]}}"#,
        )
        .unwrap();
        assert!(specs_from_package_json(&p).unwrap().is_empty());
    }

    #[test]
    fn web_dependencies_must_be_an_array() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"lit":"^3"},"web_modules":{"webDependencies":{"lit":"^3"}}}"#,
        )
        .unwrap();
        assert!(specs_from_package_json(&p).is_err(), "object form rejected");
    }

    #[test]
    fn parse_github_dep_handles_shorthand_and_urls() {
        assert_eq!(
            parse_github_dep("github:owner/repo#v1").unwrap(),
            ("owner/repo".to_string(), "v1".to_string())
        );
        assert_eq!(
            parse_github_dep("git+https://github.com/owner/repo.git#abc123").unwrap(),
            ("owner/repo".to_string(), "abc123".to_string())
        );
        assert_eq!(
            parse_github_dep("github:owner/repo").unwrap(),
            ("owner/repo".to_string(), "HEAD".to_string())
        );
        assert!(parse_github_dep("^3").is_none());
    }

    #[test]
    fn read_package_json_splits_registry_and_path_deps() {
        let tmp = tempfile::tempdir().unwrap();
        let sib = tmp.path().join("sib");
        std::fs::create_dir_all(sib.join("pub")).unwrap();
        std::fs::write(
            sib.join("package.json"),
            r#"{"name":"sibling","web_modules":{"root":"./pub"}}"#,
        )
        .unwrap();
        let p = tmp.path().join("package.json");
        std::fs::write(
            &p,
            r#"{"dependencies":{"lit":"^3","sib":"file:./sib","ws":"workspace:*"}}"#,
        )
        .unwrap();
        let (specs, mounts) = read_package_json(&p).unwrap();
        // registry dep vended; `workspace:` skipped; path-dep → key-named mount at the target root.
        assert_eq!(
            specs.iter().map(PackageSpec::name).collect::<Vec<_>>(),
            vec!["lit"]
        );
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].specifier_prefix(), "sib/");
        assert_eq!(mounts[0].url_prefix(), "/sib/");
        assert_eq!(mounts[0].dir(), sib.join("pub"));
    }

    #[test]
    #[ignore = "network: resolves and downloads from the npm registry"]
    fn vendors_lit_end_to_end_auto() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("web_modules");
        let specs = [PackageSpec::npm("lit", "^3")];
        let map = vendor(&root, "/web_modules", &specs).unwrap();
        assert!(root.join("lit/index.js").exists(), "lit entry vendored");
        // Auto-derivation reproduces the known-good entries.
        let json = map.to_json();
        assert!(json.contains("\"lit\": \"/web_modules/lit/index.js\""));
        assert!(json.contains("\"lit/\": \"/web_modules/lit/\""));
        // Second run is a cache hit: idempotent, no panic.
        vendor(&root, "/web_modules", &specs).unwrap();
    }

    #[test]
    fn prune_removes_dropped_packages_and_keeps_current_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("web_modules");
        for d in [
            "lit",
            "dropped",
            "@scope/keep",
            "@scope/drop",
            "@gone/only",
            "@oxc-project/runtime/src",
        ] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
            std::fs::write(dir.join(d).join("index.js"), "export {};").unwrap();
        }
        for m in [
            ".lit.version",
            ".dropped.version",
            ".@scope_keep.version",
            ".@scope_drop.version",
            ".@oxc-project_runtime.version",
            ".lit.lock",
        ] {
            std::fs::write(dir.join(m), "x").unwrap();
        }
        std::fs::write(dir.join("stray.txt"), "x").unwrap();

        let specs = [
            PackageSpec::npm("lit", "^3"),
            PackageSpec::npm("@scope/keep", "^1"),
        ];
        prune(&dir, &specs, &["@oxc-project/runtime"]).unwrap();

        assert!(dir.join("lit/index.js").exists());
        assert!(dir.join("@scope/keep/index.js").exists());
        assert!(dir.join("@oxc-project/runtime/src/index.js").exists());
        assert!(dir.join(".lit.version").exists());
        assert!(dir.join(".@scope_keep.version").exists());
        assert!(!dir.join("dropped").exists(), "dropped package dir removed");
        assert!(
            !dir.join("@scope/drop").exists(),
            "dropped scoped member removed"
        );
        assert!(!dir.join("@gone").exists(), "fully dropped scope removed");
        assert!(
            !dir.join(".dropped.version").exists(),
            "stale marker removed"
        );
        assert!(!dir.join(".lit.lock").exists(), "leftover lock removed");
        assert!(!dir.join("stray.txt").exists(), "stray file removed");
    }

    #[test]
    fn prune_removes_an_emptied_vendor_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("web_modules");
        std::fs::create_dir_all(dir.join("dropped")).unwrap();
        prune(&dir, &[], &[]).unwrap();
        assert!(!dir.exists(), "an emptied vendor dir does not ship");
    }

    /// A vendored tree is redistribution: the licence has to come with the code.
    #[test]
    fn keep_browser_assets_keeps_licences_and_notices() {
        for path in [
            "LICENSE",
            "LICENSE.md",
            "LICENCE.txt",
            "license",
            "LICENSE-MIT",
            "LICENSE-APACHE",
            "NOTICE",
            "COPYING",
            "AUTHORS",
            "dist/LICENSE",
        ] {
            assert_eq!(
                keep_browser_assets(path).as_deref(),
                Some(path),
                "{path} covers the code beside it"
            );
        }
        // Prose and metadata are still not part of the package.
        assert_eq!(keep_browser_assets("README.md"), None);
        assert_eq!(keep_browser_assets("CHANGELOG.md"), None);
        // The excluded trees stay excluded, licence or not.
        assert_eq!(keep_browser_assets("src/LICENSE"), None);
    }
}
