//! `web-modules` CLI: `dev` (dev server), `build` (compile source root(s) into a deployable output
//! tree — the static counterpart of `dev`, vendoring npm only when packages are given), `vendor`
//! (vendor npm into `web_modules/` + import map), `ci` (pure-Rust `npm ci`), and `npm` (delegates
//! to npm-utils' `add`/`install`/`upgrade`/…). Requires the `cli` feature; the opt-in `env` feature
//! adds `WEB_MODULES_*` environment-variable config to `build`.
//!
//! Each compiler processor (typescript, scss, tera, minify, gzip) contributes its own `--<name>` /
//! `--no-<name>` toggle — and any `--<name>-…` flags — to `build` and `dev` (assembled via
//! [`CompilerConfig`]); a global `--no-default-features` turns the default-on set (typescript,
//! scss, tera) off so they can be re-enabled individually.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use serde_json::Value;
use web_modules::vendor::{vendor, PackageSpec};

/// This binary's fallible return, `()` by default.
type Res<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// `build`'s default inline `index.html`. The entry script is RELATIVE (`./app.js`) so the page
/// also loads under a subpath (e.g. a GitHub *project* page served at `/<repo>/`). The literal
/// `{importmap}` is replaced with the generated import-map `<script>`.
const DEFAULT_HTML: &str = "<!doctype html>{importmap}<script type=module src=./app.js></script>";

#[derive(Parser)]
#[command(
    name = "web-modules",
    version,
    about = "Buildless web frontend toolchain (no Node)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Dev server: compile TS/SCSS on the fly, render `*.tera`, watch the tree, live-reload.
    Dev {
        /// Source root(s), merged (first match wins). Defaults to the cwd.
        roots: Vec<PathBuf>,
        /// Address to bind (default 127.0.0.1:8080).
        #[arg(long)]
        addr: Option<SocketAddr>,
        #[command(flatten)]
        compiler: CompilerConfig,
    },
    /// Build a deployable output tree — the static counterpart of `dev`.
    ///
    /// Compiles the source root(s) — TS→JS, SCSS→CSS, `*.tera`→rendered target, static files
    /// copied — into `--out`, exactly as `dev` serves them, and renders `index.html` with the
    /// import map injected. Vendoring is **optional**: pass `--package name@range` and/or
    /// `--manifest package.json` to vendor npm into `web_modules/`; with neither, a non-vendored
    /// tree just compiles statically. With the opt-in `env` feature each flag also reads a
    /// `WEB_MODULES_*` environment variable; an explicit flag wins.
    Build {
        /// Source root(s), merged (first match wins). Defaults to the cwd.
        roots: Vec<PathBuf>,
        /// Output directory. Defaults to `dist`, or `web_modules.out` in package.json.
        #[arg(long)]
        out: Option<PathBuf>,
        /// URL prefix the vendored modules are served at (default `/web_modules`). Under a GitHub
        /// *project* page (served at `/<repo>/`) pass `/<repo>/web_modules`.
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_MOUNT"))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        mount: Option<String>,
        /// Fallback inline `index.html` (used only when the tree has no `index.html`); `{importmap}`
        /// is replaced with the import-map `<script>`. Keep entry scripts RELATIVE (`./app.js`).
        /// Ignored when `--template` is given. Defaults to a minimal `<script src=./app.js>` shell.
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_HTML"))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        html: Option<String>,
        /// Fallback Tera template file for `index.html`, rendered with an `importmap` variable,
        /// instead of `--html` (same fallback rule: used only when the tree has no `index.html`).
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_TEMPLATE"))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        template: Option<PathBuf>,
        /// Also vendor the `dependencies` of this `package.json` (repeatable).
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_MANIFEST"))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        manifest: Vec<PathBuf>,
        /// Package to vendor, as `name` or `name@range` (e.g. `lit@^3`); repeatable. Optional —
        /// omit (with no `--manifest`) for a non-vendored, static-only build.
        #[cfg_attr(
            feature = "env",
            arg(
                long = "package",
                value_name = "SPEC",
                env = "WEB_MODULES_PACKAGES",
                value_delimiter = ' '
            )
        )]
        #[cfg_attr(not(feature = "env"), arg(long = "package", value_name = "SPEC"))]
        packages: Vec<String>,
        #[command(flatten)]
        compiler: CompilerConfig,
    },
    /// Vendor npm packages into web_modules/ + an import map.
    ///
    /// Packages come from positional `name@range` specs and/or the `dependencies` of
    /// `--manifest` package.json(s).
    Vendor {
        /// Output directory (the `web_modules/` root).
        #[arg(long, default_value = "web_modules")]
        out: PathBuf,
        /// URL prefix the output is served at.
        #[arg(long, default_value = "/web_modules")]
        mount: String,
        /// Write the import map JSON here (default: stdout).
        #[arg(long)]
        importmap: Option<PathBuf>,
        /// Also vendor the `dependencies` of this `package.json` (repeatable).
        #[arg(long)]
        manifest: Vec<PathBuf>,
        /// Packages as `name` or `name@range` (e.g. `lit@^3`). Optional when `--manifest` is given.
        packages: Vec<String>,
    },
    /// Install a package-lock.json's exact tree into node_modules/ - a pure-Rust npm ci.
    ///
    /// devDependencies included, each tarball's sha512 integrity verified, platform-mismatched
    /// optional deps skipped, and `node_modules/.bin/` shims created. Installs a project's Node
    /// test tooling (Playwright, `tsc`) with no npm - only the Node runtime is then needed.
    Ci {
        /// Project directory containing `package-lock.json` (default: current dir).
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Run an npm-utils command (add · install · ci · upgrade · …).
    ///
    /// `web-modules npm add lit@^3` is exactly `cargo npm-utils add lit@^3`.
    #[command(disable_help_flag = true)]
    Npm {
        /// Arguments forwarded verbatim to npm-utils (e.g. `add lit@^3`, `install`, `ci`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

/// The compiler processors' assembled CLI surface, flattened into `build` and `dev` so the two
/// share one input profile. Each processor contributes its own `--<name>` / `--no-<name>` toggle
/// (plus any `--<name>-…` flags); `--no-default-features` turns the default-on set off.
///
/// The binary requires the `cli` feature, which forces `typescript`, `scss`, `minify` and `tera`
/// on, so those are unconditional here; only `gzip` (the `compress` feature) is optional.
#[derive(Args, Debug)]
struct CompilerConfig {
    /// Disable every default-on compiler feature (typescript, scss, tera); re-enable individually
    /// with `--typescript` / `--scss` / `--tera`.
    #[arg(long)]
    no_default_features: bool,
    #[command(flatten)]
    typescript: web_modules::typescript::TypescriptArgs,
    #[command(flatten)]
    scss: web_modules::scss::ScssArgs,
    #[command(flatten)]
    tera: web_modules::templates::TeraArgs,
    #[command(flatten)]
    minify: web_modules::minify::MinifyArgs,
    #[cfg(feature = "compress")]
    #[command(flatten)]
    gzip: web_modules::compress::GzipArgs,
}

/// The resolved toggles + tuning, independent of which features were compiled in — what `build`
/// and `dev` actually consume.
struct ResolvedCompiler {
    typescript: bool,
    scss: bool,
    tera: bool,
    minify: bool,
    gzip: bool,
    ts_decorators: web_modules::typescript::Decorators,
    extra_scss_load_paths: Vec<PathBuf>,
}

impl CompilerConfig {
    /// Resolve each processor toggle + config, layering a `package.json` `web_modules` block under
    /// the CLI flags: `--no-<name>` > `--<name>` > block > (`default_on && !--no-default-features`).
    /// typescript/scss/tera default on; minify/gzip default off.
    fn resolve_with(&self, cfg: &PkgConfig) -> ResolvedCompiler {
        let nd = self.no_default_features;
        ResolvedCompiler {
            typescript: self.typescript.enabled_with(cfg.typescript, true, nd),
            scss: self.scss.enabled_with(cfg.scss, true, nd),
            tera: self.tera.enabled_with(cfg.tera, true, nd),
            minify: self.minify.enabled_with(cfg.minify, false, nd),
            #[cfg(feature = "compress")]
            gzip: self.gzip.enabled_with(cfg.gzip, false, nd),
            #[cfg(not(feature = "compress"))]
            gzip: false,
            ts_decorators: self.typescript.config.decorators.into(),
            // Additive: CLI `--scss-load-path`s first, then the block's `scss.loadPaths`.
            extra_scss_load_paths: {
                let mut paths = self.scss.config.load_paths.clone();
                paths.extend(cfg.scss_load_paths.iter().cloned());
                paths
            },
        }
    }
}

impl ResolvedCompiler {
    /// Map to the build pipeline's [`Processors`](web_modules::build::Processors) — also the dev
    /// server's `DevConfig` (a type alias for the same struct). `#[non_exhaustive]`, so built from
    /// `default()` and assigned (minify/gzip live in `Output`, not here).
    fn into_processors(self) -> web_modules::build::Processors {
        let mut p = web_modules::build::Processors::default();
        p.typescript = self.typescript;
        p.scss = self.scss;
        p.tera = self.tera;
        p.ts_decorators = self.ts_decorators;
        p.extra_scss_load_paths = self.extra_scss_load_paths;
        p
    }
}

/// Default `roots` to the current dir when none were given (matching the dev server and the old
/// `compile` command).
fn roots_or_cwd(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }
    roots
}

/// Build vendor's spec set from positional/`--package` specs plus each `--manifest` package.json's
/// `dependencies`. A positional spec wins over a same-named manifest entry. `vendor` requires a
/// non-empty result (`require_nonempty = true`); `build` allows none (a non-vendored, static build).
fn build_vendor_specs(
    packages: &[String],
    manifests: &[PathBuf],
    require_nonempty: bool,
) -> Res<Vec<PackageSpec>> {
    let mut specs: Vec<PackageSpec> = packages.iter().map(|p| PackageSpec::parse(p)).collect();
    // Each `--manifest` package.json's `dependencies`, via the same helper build scripts use.
    for path in manifests {
        specs.extend(web_modules::vendor::specs_from_package_json(path)?);
    }
    if require_nonempty && specs.is_empty() {
        return Err("vendor: give package specs (e.g. lit@^3) or --manifest <package.json>".into());
    }
    // A positional spec wins over a same-named manifest entry (dedupe, keeping the first).
    let mut seen = std::collections::HashSet::new();
    specs.retain(|s| seen.insert(s.name().to_string()));
    Ok(specs)
}

/// The decoded `web_modules` block from a project's `package.json`. Every field is optional so
/// config resolution can layer it **under** the CLI/env values and **over** the built-in defaults.
/// `Vec` fields are empty when the key is absent (the same "empty == unset" rule the CLI uses).
#[derive(Debug, Default)]
struct PkgConfig {
    roots: Vec<PathBuf>,
    out: Option<PathBuf>,
    mount: Option<String>,
    html: Option<String>,
    template: Option<PathBuf>,
    packages: Vec<String>,
    minify: Option<bool>,
    gzip: Option<bool>,
    typescript: Option<bool>,
    scss: Option<bool>,
    tera: Option<bool>,
    scss_load_paths: Vec<PathBuf>,
}

/// Load the `web_modules` config block from `package.json` in the current directory.
fn load_pkg_config() -> Res<(PkgConfig, Option<PathBuf>)> {
    load_pkg_config_at(Path::new("."))
}

/// Load + parse `<dir>/package.json`'s `web_modules` block. Returns the parsed config plus the
/// package.json path (which `build` uses to auto-vendor the project's `dependencies`):
/// - no file        → `(default, None)` — zero-config, not an error
/// - file, no block → `(default, Some(path))` — still drives auto-vendor
/// - malformed JSON / wrong-typed key → `Err` (naming the offending key)
fn load_pkg_config_at(dir: &Path) -> Res<(PkgConfig, Option<PathBuf>)> {
    let path = dir.join("package.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((PkgConfig::default(), None));
        }
        Err(e) => return Err(format!("{}: {e}", path.display()).into()),
    };
    let json: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    match json.get("web_modules") {
        Some(block) => Ok((parse_block(block)?, Some(path))),
        None => Ok((PkgConfig::default(), Some(path))),
    }
}

/// Hand-parse the `web_modules` object (matching `vendor.rs`'s `serde_json::Value` style — the
/// crate carries no serde-derive). Unknown keys are ignored so a newer config file stays loadable
/// by an older binary; `webDependencies` / `root` are owned by the vendoring / mount code and
/// intentionally skipped here (no double-handling).
fn parse_block(block: &Value) -> Res<PkgConfig> {
    let obj = block
        .as_object()
        .ok_or("package.json: `web_modules` must be an object")?;
    let mut cfg = PkgConfig::default();
    for (key, val) in obj {
        match key.as_str() {
            "roots" => cfg.roots = path_array(val, "web_modules.roots")?,
            "out" => cfg.out = Some(PathBuf::from(as_string(val, "web_modules.out")?)),
            "mount" => cfg.mount = Some(as_string(val, "web_modules.mount")?),
            "html" => cfg.html = Some(as_string(val, "web_modules.html")?),
            "template" => {
                cfg.template = Some(PathBuf::from(as_string(val, "web_modules.template")?));
            }
            "packages" => cfg.packages = string_array(val, "web_modules.packages")?,
            "minify" => cfg.minify = Some(as_bool(val, "web_modules.minify")?),
            "gzip" => cfg.gzip = Some(as_bool(val, "web_modules.gzip")?),
            "typescript" => {
                cfg.typescript = Some(processor_enabled(val, "web_modules.typescript")?)
            }
            "scss" => {
                cfg.scss = Some(processor_enabled(val, "web_modules.scss")?);
                if let Some(lp) = val.as_object().and_then(|o| o.get("loadPaths")) {
                    cfg.scss_load_paths = path_array(lp, "web_modules.scss.loadPaths")?;
                }
            }
            "tera" => cfg.tera = Some(processor_enabled(val, "web_modules.tera")?),
            // Owned elsewhere (vendoring / mount) — read on the package.json content, not here.
            "webDependencies" | "root" => {}
            _ => {}
        }
    }
    Ok(cfg)
}

/// A processor key is `false`/`true` (disable/enable) or an object (presence enables + configures).
fn processor_enabled(val: &Value, ctx: &str) -> Res<bool> {
    match val {
        Value::Bool(b) => Ok(*b),
        Value::Object(_) => Ok(true),
        _ => Err(format!("{ctx} must be a boolean or an object").into()),
    }
}

fn as_string(val: &Value, ctx: &str) -> Res<String> {
    val.as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{ctx} must be a string").into())
}

fn as_bool(val: &Value, ctx: &str) -> Res<bool> {
    val.as_bool()
        .ok_or_else(|| format!("{ctx} must be a boolean").into())
}

fn string_array(val: &Value, ctx: &str) -> Res<Vec<String>> {
    let arr = val
        .as_array()
        .ok_or_else(|| format!("{ctx} must be an array of strings"))?;
    arr.iter()
        .map(|e| {
            e.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{ctx} entries must be strings").into())
        })
        .collect()
}

fn path_array(val: &Value, ctx: &str) -> Res<Vec<PathBuf>> {
    Ok(string_array(val, ctx)?
        .into_iter()
        .map(PathBuf::from)
        .collect())
}

/// CLI value (Some) wins, else the block, else the built-in default. (env folds into the CLI Some.)
fn pick<T>(cli: Option<T>, block: Option<T>, default: T) -> T {
    cli.or(block).unwrap_or(default)
}

/// A non-empty CLI Vec wins, else the block Vec — an empty CLI Vec means "unset" (matching the
/// positional `roots`/`--package` contract that `roots_or_cwd` relies on).
fn pick_vec<T>(cli: Vec<T>, block: Vec<T>) -> Vec<T> {
    if cli.is_empty() {
        block
    } else {
        cli
    }
}

#[tokio::main]
async fn main() -> Res {
    match Cli::parse().command {
        Command::Dev {
            roots,
            addr,
            compiler,
        } => {
            // Config from a `web_modules` block in ./package.json (flags only — dev never vendors).
            let (cfg, _pkg_path) = load_pkg_config()?;
            let config = compiler.resolve_with(&cfg).into_processors();
            let roots = roots_or_cwd(pick_vec(roots, cfg.roots));
            let addr =
                addr.unwrap_or_else(|| "127.0.0.1:8080".parse().expect("valid default addr"));
            web_modules::dev::serve_with(roots, addr, config).await?;
        }
        Command::Build {
            roots,
            out,
            mount,
            html,
            template,
            manifest,
            packages,
            compiler,
        } => {
            // Config from a `web_modules` block in ./package.json, layered under the CLI/env args.
            let (cfg, pkg_path) = load_pkg_config()?;
            let resolved = compiler.resolve_with(&cfg);
            let (minify, gzip) = (resolved.minify, resolved.gzip);
            let output = web_modules::build::Output::new(minify, gzip);
            let processors = resolved.into_processors();

            // Auto-vendor: the discovered package.json acts as an implicit `--manifest`, so its
            // `dependencies` (honoring `web_modules.webDependencies`) are vendored. Explicit
            // `--manifest`/`--package` come first and win on a name clash (dedupe keeps the first);
            // with nothing to vendor, `build` stays static-only.
            let mut manifests = manifest;
            if let Some(path) = &pkg_path {
                manifests.push(path.clone());
            }
            let packages = pick_vec(packages, cfg.packages);
            let specs = build_vendor_specs(&packages, &manifests, false)?;

            let roots = roots_or_cwd(pick_vec(roots, cfg.roots));
            let out = pick(out, cfg.out, PathBuf::from("dist"));
            let mount = pick(mount, cfg.mount, "/web_modules".to_string());
            let html = pick(html, cfg.html, DEFAULT_HTML.to_string());
            let template = template.or(cfg.template);

            // Internal code builds via the explicit `BuildOptions` struct; the `Build` builder is
            // the developer-facing wrapper over this same call.
            web_modules::build::build(&web_modules::build::BuildOptions {
                specs: &specs,
                roots: &roots,
                out: &out,
                mount: &mount,
                html: &html,
                template: template.as_deref(),
                processors,
                output,
            })?;
            println!(
                "built {} root(s) → {} ({} package spec(s), mount {mount}{}{})",
                roots.len(),
                out.display(),
                specs.len(),
                if minify { ", minified" } else { "" },
                if gzip { ", gzipped" } else { "" },
            );
        }
        Command::Vendor {
            out,
            mount,
            importmap,
            manifest,
            packages,
        } => {
            let specs = build_vendor_specs(&packages, &manifest, true)?;
            let map = vendor(&out, &mount, &specs)?;
            match importmap {
                Some(path) => {
                    map.write_to(&path)?;
                    println!("wrote import map → {}", path.display());
                }
                None => println!("{}", map.to_json()),
            }
        }
        Command::Ci { dir } => {
            // `npm ci`, in pure Rust — no npm. (npm-utils is a direct dependency, so the bin
            // calls it without the `bundle`-gated re-export.)
            let installed =
                npm_utils::install::from_lockfile(&dir.join("package-lock.json"), &dir)?;
            println!(
                "installed {} package(s) → {} (npm ci, in Rust - no npm)",
                installed.len(),
                dir.join("node_modules").display()
            );
        }
        Command::Npm { args } => {
            // Delegate to npm-utils' own CLI, so `web-modules npm add lit@^3` is exactly
            // `npm-utils add lit@^3`. The leading token stands in for argv[0] (clap takes the
            // displayed program name from npm-utils' own `#[command(name = …)]`).
            npm_utils::cli::run(std::iter::once(OsString::from("npm-utils")).chain(args))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(specs: &[PackageSpec]) -> Vec<&str> {
        specs.iter().map(PackageSpec::name).collect()
    }

    /// Parse a `build` invocation (with a dummy `--out`) and resolve its compiler config.
    fn resolve_build(extra: &[&str]) -> ResolvedCompiler {
        let argv: Vec<&str> = [&["web-modules", "build", "--out", "out"][..], extra].concat();
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Build { compiler, .. } => compiler.resolve_with(&PkgConfig::default()),
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn vendor_specs_requires_a_source_when_asked() {
        // `vendor` requires a source; `build` (require_nonempty = false) allows none.
        assert!(build_vendor_specs(&[], &[], true).is_err());
        assert!(build_vendor_specs(&[], &[], false).unwrap().is_empty());
    }

    #[test]
    fn vendor_specs_from_positional_specs() {
        let specs =
            build_vendor_specs(&["lit@^3".into(), "@lit/context@^1".into()], &[], true).unwrap();
        assert_eq!(names(&specs), ["lit", "@lit/context"]);
    }

    #[test]
    fn vendor_specs_reads_a_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(&manifest, r#"{"dependencies":{"ms":"^2.1.3"}}"#).unwrap();
        let specs = build_vendor_specs(&[], std::slice::from_ref(&manifest), true).unwrap();
        assert_eq!(names(&specs), ["ms"]);
    }

    #[test]
    fn vendor_specs_positional_wins_over_manifest_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(&manifest, r#"{"dependencies":{"lit":"^2","ms":"^2"}}"#).unwrap();
        let specs =
            build_vendor_specs(&["lit@^3".into()], std::slice::from_ref(&manifest), true).unwrap();
        // lit (positional, first) then ms (manifest); the manifest's lit is deduped out.
        assert_eq!(names(&specs), ["lit", "ms"]);
    }

    #[test]
    fn build_out_defaults_to_dist() {
        // `--out` wins, then `web_modules.out`, then the `dist` convention.
        let dist = PathBuf::from("dist");
        assert_eq!(pick(None::<PathBuf>, None, dist.clone()), dist);
        assert_eq!(
            pick(None, Some(PathBuf::from("b")), dist.clone()),
            PathBuf::from("b")
        );
        assert_eq!(
            pick(Some(PathBuf::from("a")), Some(PathBuf::from("b")), dist),
            PathBuf::from("a")
        );
    }

    #[test]
    fn build_defaults_roots_to_cwd() {
        let cli = Cli::try_parse_from(["web-modules", "build", "--out", "dist"]).unwrap();
        match cli.command {
            Command::Build { roots, .. } => {
                assert!(roots.is_empty(), "no positional roots given");
                assert_eq!(roots_or_cwd(roots), [PathBuf::from(".")]);
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn build_parses_positional_roots_and_packages() {
        let cli = Cli::try_parse_from([
            "web-modules",
            "build",
            "web",
            "shared",
            "--out",
            "dist",
            "--mount",
            "/repo/web_modules",
            "--package",
            "lit@^3",
            "--package",
            "@lit/context@^1",
            "--minify",
        ])
        .unwrap();
        match cli.command {
            Command::Build {
                roots,
                out,
                mount,
                packages,
                compiler,
                ..
            } => {
                assert_eq!(roots, [PathBuf::from("web"), PathBuf::from("shared")]);
                assert_eq!(out, Some(PathBuf::from("dist")));
                assert_eq!(mount.as_deref(), Some("/repo/web_modules"));
                assert_eq!(packages, ["lit@^3", "@lit/context@^1"]);
                assert!(
                    compiler.resolve_with(&PkgConfig::default()).minify,
                    "--minify opts in"
                );
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn toggles_default_on_set_and_opt_in() {
        let d = resolve_build(&[]);
        assert!(d.typescript && d.scss && d.tera, "ts/scss/tera default on");
        assert!(!d.minify && !d.gzip, "minify/gzip default off");
        assert!(resolve_build(&["--minify"]).minify, "--minify opts in");
    }

    #[test]
    fn toggles_no_scss_disables_regardless_of_order() {
        assert!(!resolve_build(&["--no-scss"]).scss);
        // `--no-<name>` wins over `--<name>`, in either order.
        assert!(!resolve_build(&["--no-scss", "--scss"]).scss);
        assert!(!resolve_build(&["--scss", "--no-scss"]).scss);
    }

    #[test]
    fn no_default_features_disables_then_reenables() {
        let nd = resolve_build(&["--no-default-features"]);
        assert!(
            !nd.typescript && !nd.scss && !nd.tera,
            "--no-default-features turns the default-on set off"
        );
        let one = resolve_build(&["--no-default-features", "--scss"]);
        assert!(one.scss, "--scss re-enables after --no-default-features");
        assert!(!one.typescript && !one.tera, "the others stay off");
    }

    #[test]
    fn typescript_decorators_default_lit() {
        use web_modules::typescript::Decorators;
        assert_eq!(resolve_build(&[]).ts_decorators, Decorators::Lit);
        assert_eq!(
            resolve_build(&["--typescript-decorators", "standard"]).ts_decorators,
            Decorators::Standard
        );
    }

    #[test]
    fn build_unset_scalars_are_none() {
        let cli = Cli::try_parse_from(["web-modules", "build"]).unwrap();
        match cli.command {
            Command::Build {
                roots,
                out,
                packages,
                ..
            } => {
                // `out` and the positionals have no env backing, so they're reliably unset here.
                // (mount/html/template are env-backed; their None-ness would race with the
                // env-setting test under `--features env`, so it's covered there instead.)
                assert!(out.is_none(), "no --out and no default ⇒ None");
                assert!(roots.is_empty() && packages.is_empty());
            }
            _ => panic!("expected Build"),
        }
    }

    // ---- package.json `web_modules` block loader ----

    fn write_pkg(json: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), json).unwrap();
        dir
    }

    #[test]
    fn loads_absent_file_as_empty() {
        let dir = tempfile::tempdir().unwrap(); // no package.json
        let (cfg, path) = load_pkg_config_at(dir.path()).unwrap();
        assert!(path.is_none());
        assert!(cfg.out.is_none() && cfg.roots.is_empty() && cfg.scss.is_none());
    }

    #[test]
    fn parses_full_block() {
        let dir = write_pkg(
            r#"{ "web_modules": {
                "roots": ["web", "shared"], "out": "dist", "mount": "/m",
                "html": "<x>", "template": "shell.html.tera", "packages": ["lit@^3"],
                "minify": true, "gzip": false,
                "typescript": true,
                "scss": { "loadPaths": ["styles"] },
                "tera": false
            } }"#,
        );
        let (cfg, path) = load_pkg_config_at(dir.path()).unwrap();
        assert!(path.is_some());
        assert_eq!(cfg.roots, [PathBuf::from("web"), PathBuf::from("shared")]);
        assert_eq!(cfg.out, Some(PathBuf::from("dist")));
        assert_eq!(cfg.mount.as_deref(), Some("/m"));
        assert_eq!(cfg.html.as_deref(), Some("<x>"));
        assert_eq!(cfg.template, Some(PathBuf::from("shell.html.tera")));
        assert_eq!(cfg.packages, ["lit@^3"]);
        assert_eq!(cfg.minify, Some(true));
        assert_eq!(cfg.gzip, Some(false));
        assert_eq!(cfg.typescript, Some(true)); // bool form enables
        assert_eq!(cfg.scss, Some(true));
        assert_eq!(cfg.scss_load_paths, [PathBuf::from("styles")]);
        assert_eq!(cfg.tera, Some(false)); // bool form disables
    }

    #[test]
    fn processor_bool_disables_object_enables() {
        let off = write_pkg(r#"{"web_modules":{"scss":false}}"#);
        let (cfg, _) = load_pkg_config_at(off.path()).unwrap();
        assert_eq!(cfg.scss, Some(false));
        assert!(cfg.scss_load_paths.is_empty());

        let on = write_pkg(r#"{"web_modules":{"scss":{"loadPaths":["a","b"]}}}"#);
        let (cfg, _) = load_pkg_config_at(on.path()).unwrap();
        assert_eq!(cfg.scss, Some(true));
        assert_eq!(
            cfg.scss_load_paths,
            [PathBuf::from("a"), PathBuf::from("b")]
        );
    }

    #[test]
    fn malformed_block_errors() {
        for bad in [
            r#"{"web_modules": []}"#,           // not an object
            r#"{"web_modules": {"mount": 5}}"#, // wrong scalar type
            r#"{"web_modules": {"scss": 3}}"#,  // processor not bool/object
            r#"{ not json "#,                   // malformed JSON
        ] {
            let dir = write_pkg(bad);
            assert!(
                load_pkg_config_at(dir.path()).is_err(),
                "should reject: {bad}"
            );
        }
    }

    #[test]
    fn ignores_webdeps_and_root_returns_path() {
        // The block's vendoring/mount keys are owned elsewhere — the loader skips them, and a
        // package.json with no flag keys still yields its path (so `build` can auto-vendor deps).
        let dir = write_pkg(
            r#"{ "dependencies": {"lit": "^3"},
                 "web_modules": { "webDependencies": ["lit"], "root": "./src" } }"#,
        );
        let (cfg, path) = load_pkg_config_at(dir.path()).unwrap();
        assert!(path.is_some());
        assert!(cfg.roots.is_empty() && cfg.out.is_none() && cfg.scss.is_none());
    }

    // ---- resolution layering ----

    fn build_compiler(extra: &[&str]) -> CompilerConfig {
        let argv: Vec<&str> = [&["web-modules", "build", "--out", "o"][..], extra].concat();
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Build { compiler, .. } => compiler,
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn pick_and_pick_vec_precedence() {
        assert_eq!(pick(Some("a"), Some("b"), "c"), "a");
        assert_eq!(pick(None, Some("b"), "c"), "b");
        assert_eq!(pick(None::<&str>, None, "c"), "c");
        assert_eq!(pick_vec(vec!["a"], vec!["b"]), ["a"]);
        assert_eq!(pick_vec(Vec::<&str>::new(), vec!["b"]), ["b"]);
    }

    #[test]
    fn toggle_folds_block_under_cli() {
        let scss_off = PkgConfig {
            scss: Some(false),
            ..Default::default()
        };
        let scss_on = PkgConfig {
            scss: Some(true),
            ..Default::default()
        };
        // block disables, no flag → off; `--scss` beats block-off.
        assert!(!build_compiler(&[]).resolve_with(&scss_off).scss);
        assert!(build_compiler(&["--scss"]).resolve_with(&scss_off).scss);
        // block enables, `--no-scss` beats block-on.
        assert!(!build_compiler(&["--no-scss"]).resolve_with(&scss_on).scss);
        // `--no-default-features` suppresses the default, but a block `scss:true` re-enables;
        // `--no-default-features` alone leaves scss off.
        assert!(
            build_compiler(&["--no-default-features"])
                .resolve_with(&scss_on)
                .scss
        );
        assert!(
            !build_compiler(&["--no-default-features"])
                .resolve_with(&PkgConfig::default())
                .scss
        );
    }

    #[test]
    fn scss_load_paths_concat_cli_then_block() {
        let block = PkgConfig {
            scss_load_paths: vec![PathBuf::from("b")],
            ..Default::default()
        };
        let r = build_compiler(&["--scss-load-path", "a"]).resolve_with(&block);
        assert_eq!(
            r.extra_scss_load_paths,
            [PathBuf::from("a"), PathBuf::from("b")]
        );
    }

    #[cfg(feature = "env")]
    #[test]
    fn build_env_fills_option() {
        // With the `env` feature, clap fills `WEB_MODULES_MOUNT` straight into the `Option`,
        // so env sits above the package.json block automatically.
        std::env::set_var("WEB_MODULES_MOUNT", "/from-env");
        let parsed = Cli::try_parse_from(["web-modules", "build"]);
        std::env::remove_var("WEB_MODULES_MOUNT");
        match parsed.unwrap().command {
            Command::Build { mount, .. } => assert_eq!(mount.as_deref(), Some("/from-env")),
            _ => panic!("expected Build"),
        }
    }
}
