//! Vendor packages into a `web_modules/` tree and build the import map.
//!
//! Orchestration over [`npm_utils`]: each [`PackageSpec`] names a [source](PackageSpec::npm)
//! (an npm package + semver range, or a [GitHub archive](PackageSpec::git) at a ref),
//! how to [extract](Extract) it, and where its files land. Work is cache-guarded
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

use npm_utils::package_json::{spec::Range, Entry, PackageJson};
use npm_utils::{cache, download, extract, registry::Registry};

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
/// them with Rust. Registry ranges are preserved verbatim (`^3`, `~1.2`, …); a
/// `github:owner/repo#ref` or git URL becomes a [git](PackageSpec::git) spec; and
/// local protocols (`file:`/`link:`/`workspace:`/`portal:`) are skipped. Each entry
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
/// [`PackageSpec`]: a `github:` / git URL → a [git](PackageSpec::git) spec; a local
/// protocol (`file:`/`link:`/`workspace:`/`portal:`) → `None` (nothing to vend);
/// anything else → a registry [npm](PackageSpec::npm) spec, range verbatim.
fn dep_to_spec(name: &str, value: &str) -> Option<PackageSpec> {
    if is_local_protocol(value) {
        return None;
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
    let Some(deps) = json.get("dependencies").and_then(|v| v.as_object()) else {
        return Ok((specs, mounts));
    };
    for (name, value) in deps {
        let Some(value) = value.as_str() else {
            continue;
        };
        if let Some(rel) = local_path_dep(value) {
            mounts.push(
                Mount::from_dir(base.join(rel))
                    .specifier(format!("{name}/"))
                    .url(format!("/{name}/")),
            );
        } else if let Some((repo, reference)) = parse_github_dep(value) {
            specs.push(PackageSpec::git(repo, reference));
        } else if !is_unsupported_protocol(value) {
            specs.push(PackageSpec::npm(name.as_str(), value));
        }
    }
    Ok((specs, mounts))
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
    } else if let Some(idx) = locator.find("github.com") {
        locator[idx + "github.com".len()..].trim_start_matches([':', '/'])
    } else {
        return None;
    };
    let path = path.trim_end_matches(".git").trim_matches('/');
    let (owner, rest) = path.split_once('/')?;
    let repo = rest.split('/').next().unwrap_or(rest);
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((format!("{owner}/{repo}"), reference))
}

/// Default selection: keep browser assets (`.js`/`.mjs`/`.css`) **plus `.scss`
/// sources** (so packages like Bootstrap can be themed from their SCSS) while
/// dropping TypeScript sources and the node-only / development build trees some
/// packages ship.
pub fn keep_browser_assets(rel: &str) -> Option<String> {
    if rel
        .split('/')
        .any(|seg| matches!(seg, "src" | "node" | "development"))
    {
        return None;
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

fn vendor_inner(
    vendor_dir: &Path,
    mount: &str,
    specs: &[PackageSpec],
) -> std::result::Result<Importmap, Box<dyn std::error::Error>> {
    let mount = mount.trim_end_matches('/');
    let mut map = Importmap::new();
    std::fs::create_dir_all(vendor_dir)?;

    for spec in specs {
        let dest_dir = match &spec.dest {
            Some(d) if d.is_absolute() => d.clone(),
            Some(d) => vendor_dir.join(d),
            None => vendor_dir.join(&spec.dir),
        };

        // Build-script integration: declare the vendored destination as a
        // `rerun-if-changed` input so Cargo re-runs the build script — and thus
        // re-vendors — when this directory is deleted or its contents change.
        // Without it, wiping a vendored asset (e.g. `node_modules/bootstrap`)
        // leaves a "successful" build whose dev server then fails at runtime on
        // the now-missing file. Gated on a build-script context (`OUT_DIR` is
        // set) so plain library / CLI callers don't emit stray cargo directives.
        if std::env::var_os("OUT_DIR").is_some() {
            println!("cargo:rerun-if-changed={}", dest_dir.display());
        }

        let flat = spec.dir.replace('/', "_");
        let marker = vendor_dir.join(format!(".{flat}.version"));

        // Resolve the archive URL + the cache key (npm version, or the git ref).
        let (archive_url, cache_key, is_git) = match &spec.source {
            Source::Npm { package, range } => {
                let resolved = Registry::npm().resolve(package, &Range::parse(range)?)?;
                (resolved.tarball_url, resolved.version.to_string(), false)
            }
            Source::Git {
                owner,
                repo,
                reference,
            } => (
                download::github_archive_url(owner, repo, reference),
                reference.clone(),
                true,
            ),
        };

        if !is_up_to_date(&marker, &cache_key, &dest_dir, &spec.extract) {
            let lock = vendor_dir.join(format!(".{flat}.lock"));
            cache::with_lock(&lock)(|| -> std::result::Result<(), Box<dyn std::error::Error>> {
                // Re-check inside the lock: a concurrent build may have just done it.
                if is_up_to_date(&marker, &cache_key, &dest_dir, &spec.extract) {
                    return Ok(());
                }
                let bytes = download::fetch(&archive_url)?;
                extract_archive(&bytes, is_git, &spec.extract, &dest_dir)?;
                cache::write_marker(&marker, &cache_key)?;
                Ok(())
            })?;
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

/// Extract `bytes` (an npm `.tar.gz` or a GitHub `.zip`) into `dest` per `extract`.
/// GitHub archives carry a single top-level `repo-<ref>/` directory, stripped
/// generically (its exact name depends on the ref).
fn extract_archive(
    bytes: &[u8],
    is_git: bool,
    extract: &Extract,
    dest: &Path,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
            auto_entries(
                pkg.as_ref(),
                source_name(&spec.source),
                &spec.dir,
                mount,
                dest_dir,
            )
        }
    }
}

/// The package/repo name a spec resolves under (used for auto import-map keys).
fn source_name(source: &Source) -> &str {
    match source {
        Source::Npm { package, .. } => package,
        Source::Git { repo, .. } => repo,
    }
}

/// Derive import-map entries from a package's resolved `exports`. Bare + a `name/`
/// convenience prefix (so subpaths resolve to the vended files); non-identity
/// subpath remaps and `"./*"` pattern prefixes are mapped explicitly. Entries
/// whose target file isn't present are skipped.
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
}
