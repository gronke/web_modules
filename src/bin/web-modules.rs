//! `web-modules` CLI: `dev` (dev server), `compile` (compile root(s) to an output dir),
//! `vendor` (vendor npm into `web_modules/` + import map), `ci` (pure-Rust `npm ci`), and `npm`
//! (delegates to npm-utils' `add`/`install`/`upgrade`/…). Requires the `cli` feature.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use web_modules::vendor::{vendor, PackageSpec};

/// This binary's fallible return, `()` by default.
type Res<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
    /// Dev server: compile TS/SCSS on the fly, watch the tree, live-reload.
    Dev {
        /// Source root(s), merged (first match wins). Defaults to the cwd.
        roots: Vec<PathBuf>,
        /// Address to bind.
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
    },
    /// Compile source root(s) into an output tree (TS→JS, SCSS→CSS, static files copied).
    ///
    /// Multiple roots are merged (first root wins on a path conflict), exactly as `dev`
    /// serves them. Emits a directory tree, not a bundle; for the full
    /// vendor+transform+render embed pipeline use the `web_modules::build` helper.
    Compile {
        /// Source root(s), merged (first match wins). Defaults to the current dir. With `--out`
        /// omitted, the *last* path is taken as the output directory.
        roots: Vec<PathBuf>,
        /// Output directory (clearer than a trailing positional, and the unambiguous form when
        /// passing more than one root).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Also compile SCSS.
        #[arg(long)]
        scss: bool,
        /// Minify emitted `.js`.
        #[arg(long)]
        minify: bool,
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

/// Resolve `compile`'s `[roots…]` + optional `--out` into `(source roots, output dir)`.
/// `--out` wins; otherwise the last positional path is the output. Remaining paths are the
/// source roots, defaulting to the current dir. Errors when no output can be determined.
fn resolve_compile_io(
    mut roots: Vec<PathBuf>,
    out: Option<PathBuf>,
) -> Res<(Vec<PathBuf>, PathBuf)> {
    let out = match out {
        Some(out) => out,
        None => roots
            .pop()
            .ok_or("compile: give an output directory - a trailing path or `--out <dir>`")?,
    };
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }
    Ok((roots, out))
}

/// Parse one positional vendor spec: `name`, `name@range`, or `@scope/name@range`; the range
/// `@` is the last one, so a leading scope `@` is preserved.
fn parse_spec(p: &str) -> PackageSpec {
    match p.rfind('@') {
        Some(i) if i > 0 => PackageSpec::npm(&p[..i], &p[i + 1..]),
        _ => PackageSpec::npm(p, "*"),
    }
}

/// Build vendor's spec set from positional `name@range` specs plus each `--manifest`
/// package.json's `dependencies`. A positional spec wins over a same-named manifest entry.
/// Errors when neither source yields a package.
fn build_vendor_specs(packages: &[String], manifests: &[PathBuf]) -> Res<Vec<PackageSpec>> {
    let mut specs: Vec<PackageSpec> = packages
        .iter()
        .map(String::as_str)
        .map(parse_spec)
        .collect();
    // Each `--manifest` package.json's `dependencies`, via the same helper build scripts use.
    for path in manifests {
        specs.extend(web_modules::vendor::specs_from_package_json(path)?);
    }
    if specs.is_empty() {
        return Err("vendor: give package specs (e.g. lit@^3) or --manifest <package.json>".into());
    }
    // A positional spec wins over a same-named manifest entry (dedupe, keeping the first).
    let mut seen = std::collections::HashSet::new();
    specs.retain(|s| seen.insert(s.name().to_string()));
    Ok(specs)
}

#[tokio::main]
async fn main() -> Res {
    match Cli::parse().command {
        Command::Dev { mut roots, addr } => {
            if roots.is_empty() {
                roots.push(PathBuf::from("."));
            }
            web_modules::dev::serve(roots, addr).await?;
        }
        Command::Compile {
            roots,
            out,
            scss,
            minify,
        } => {
            let (roots, out) = resolve_compile_io(roots, out)?;
            // SCSS `@use`/`@import` load paths span every root, matching the dev server.
            let load_paths: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
            let (mut modules, mut stylesheets, mut copied) = (0, 0, 0);
            // Compile last-to-first so the first root's files win on a path conflict (the order
            // `dev` resolves overlapping roots in).
            for root in roots.iter().rev() {
                modules += web_modules::typescript::compile_directory(root, &out)?;
                if scss {
                    stylesheets += web_modules::scss::compile_directory(root, &out, &load_paths)?;
                }
                // Carry across everything the processors don't transform (HTML, images, JSON, …)
                // so the output is a complete, servable tree — not just the compiled modules.
                copied += web_modules::static_files::copy_static(root, &out)?;
            }
            if minify {
                web_modules::minify::minify_directory(&out)?;
            }
            println!(
                "compiled {modules} module(s){} + copied {copied} static file(s) from {} root(s) → {}{}",
                if scss {
                    format!(", {stylesheets} stylesheet(s)")
                } else {
                    String::new()
                },
                roots.len(),
                out.display(),
                if minify { " (minified)" } else { "" },
            );
        }
        Command::Vendor {
            out,
            mount,
            importmap,
            manifest,
            packages,
        } => {
            let specs = build_vendor_specs(&packages, &manifest)?;
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

    #[test]
    fn compile_io_two_paths_last_is_output() {
        let (roots, out) = resolve_compile_io(vec!["web".into(), "dist".into()], None).unwrap();
        assert_eq!(roots, [PathBuf::from("web")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_many_roots_last_is_output() {
        let (roots, out) =
            resolve_compile_io(vec!["a".into(), "b".into(), "dist".into()], None).unwrap();
        assert_eq!(roots, [PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_single_path_defaults_source_to_cwd() {
        let (roots, out) = resolve_compile_io(vec!["dist".into()], None).unwrap();
        assert_eq!(roots, [PathBuf::from(".")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_out_flag_keeps_all_positionals_as_roots() {
        let (roots, out) =
            resolve_compile_io(vec!["a".into(), "b".into()], Some("dist".into())).unwrap();
        assert_eq!(roots, [PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_out_flag_without_roots_defaults_to_cwd() {
        let (roots, out) = resolve_compile_io(vec![], Some("dist".into())).unwrap();
        assert_eq!(roots, [PathBuf::from(".")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_no_output_is_an_error() {
        assert!(resolve_compile_io(vec![], None).is_err());
    }

    #[test]
    fn parse_spec_handles_bare_scoped_and_ranged() {
        assert_eq!(parse_spec("lit").name(), "lit");
        assert_eq!(parse_spec("lit@^3").name(), "lit");
        assert_eq!(parse_spec("@lit/context").name(), "@lit/context");
        assert_eq!(parse_spec("@lit/context@^1").name(), "@lit/context");
    }

    #[test]
    fn vendor_specs_requires_a_source() {
        assert!(build_vendor_specs(&[], &[]).is_err());
    }

    #[test]
    fn vendor_specs_from_positional_specs() {
        let specs = build_vendor_specs(&["lit@^3".into(), "@lit/context@^1".into()], &[]).unwrap();
        assert_eq!(names(&specs), ["lit", "@lit/context"]);
    }

    #[test]
    fn vendor_specs_reads_a_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(&manifest, r#"{"dependencies":{"ms":"^2.1.3"}}"#).unwrap();
        let specs = build_vendor_specs(&[], std::slice::from_ref(&manifest)).unwrap();
        assert_eq!(names(&specs), ["ms"]);
    }

    #[test]
    fn vendor_specs_positional_wins_over_manifest_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(&manifest, r#"{"dependencies":{"lit":"^2","ms":"^2"}}"#).unwrap();
        let specs =
            build_vendor_specs(&["lit@^3".into()], std::slice::from_ref(&manifest)).unwrap();
        // lit (positional, first) then ms (manifest); the manifest's lit is deduped out.
        assert_eq!(names(&specs), ["lit", "ms"]);
    }
}
