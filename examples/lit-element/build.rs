//! Bakes the frontend at build time: vendors the npm dependencies, transforms
//! `web/*.ts` and compiles `web/*.scss`, and renders `web/index.html.tera` (a Tera
//! template, with the import map injected) — all into `$OUT_DIR/dist`, which `main.rs`
//! embeds with `include_dir!`.
//!
//! It then writes `e2e/resolution.json`, where `tests/importmap.spec.ts` checks the
//! browser against `imports::read_module` and `Importmap::resolve`.
//!
//! The browser dependencies are sourced from `web/package.json` (`dependencies`),
//! so they're maintained like any npm project; `devDependencies` there (tooling)
//! are not vended. Two packages need per-package vendoring tweaks a flat range
//! can't express, so they're added programmatically — showing both styles.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use url::Url;
use web_modules::build::{build, BuildOptions};
use web_modules::importmap::Importmap;
use web_modules::imports::{read_module, ImportKind, ModuleImports};
use web_modules::vendor::{specs_from_package_json, PackageSpec};

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dist");
    let web = manifest.join("web");

    // Browser deps come from web/package.json `dependencies` (import-map entries
    // auto-derived from each package.json); `devDependencies` there are not vended.
    let mut specs = specs_from_package_json(&manifest.join("web/package.json"))
        .expect("read browser dependencies from web/package.json");

    // Two packages need tweaks a flat package.json range can't express, declared
    // programmatically instead:
    specs.push(
        // @popperjs/core's `module` points at lib/index.js; we want the browser ESM.
        PackageSpec::npm("@popperjs/core", "^2").imports([
            ("@popperjs/core", "dist/esm/index.js"),
            ("@popperjs/core/", "dist/esm/"),
        ]),
    );
    // Loaded via a classic <script>, so vend it without an import-map entry.
    specs.push(PackageSpec::npm("@webcomponents/webcomponentsjs", "^2").no_imports());

    build(&BuildOptions {
        specs: &specs,
        roots: std::slice::from_ref(&web),
        out: &out,
        mount: "/web_modules",
        // `web/index.html.tera` is rendered by the tree (with `{{ importmap | safe }}`
        // becoming the generated <script type="importmap">); `html`/`template` here are
        // only fallbacks for when the tree has no `index.html`.
        html: "",
        template: None,
        processors: Default::default(),
        output: Default::default(),
    })
    .expect("build lit-element frontend");

    write_resolution(&out);
}

/// Every bare import the shipped modules carry, with the address `resolve` gives it, and
/// the modules the page loads up front and through first-party `import()`.
fn write_resolution(out: &Path) {
    let map = Importmap::from_json_file(&out.join("importmap.json")).expect("read importmap.json");
    let site = Url::parse("http://site.invalid/").unwrap();
    // By URL path; the UMD and CommonJS files do not parse as modules.
    let modules: BTreeMap<String, ModuleImports> = web_modules::walk::files_within(out)
        .expect("walk the build output")
        .into_iter()
        .filter(|rel| rel.extension().is_some_and(|ext| ext == "js"))
        .filter_map(|rel| {
            let read = read_module(&std::fs::read_to_string(out.join(&rel)).ok()?).ok()?;
            let url = site
                .join(&rel.to_string_lossy().replace('\\', "/"))
                .unwrap();
            Some((url.path().to_string(), read))
        })
        .collect();

    let mut specifiers = BTreeMap::new();
    for read in modules.values() {
        for import in read.imports.iter().filter(|i| is_bare(&i.specifier)) {
            specifiers.insert(import.specifier.clone(), map.resolve(&import.specifier));
        }
    }

    // The module a specifier in `from` loads, by URL path; `None` for another origin.
    let target = |from: &str, specifier: &str| -> Option<String> {
        let url = if is_bare(specifier) {
            let address = map
                .resolve(specifier)
                .unwrap_or_else(|| panic!("{from} imports {specifier}, which the map refuses"));
            site.join(&address)
        } else {
            site.join(from).and_then(|from| from.join(specifier))
        }
        .ok()?;
        (url.origin() == site.origin()).then(|| url.path().to_string())
    };
    // Static imports reach every module in `roots`' closure; `import()` is followed out of
    // first-party modules only, since a package's own may never run.
    let closure = |roots: Vec<String>, dynamic: bool| {
        let (mut seen, mut queue) = (BTreeSet::new(), roots);
        while let Some(path) = queue.pop() {
            if !seen.insert(path.clone()) {
                continue;
            }
            let read = modules
                .get(&path)
                .unwrap_or_else(|| panic!("{path} is no module of the build output"));
            let first_party = !path.starts_with("/web_modules/");
            queue.extend(
                read.imports
                    .iter()
                    .filter(|i| i.kind == ImportKind::Static || (dynamic && first_party))
                    .filter_map(|i| target(&path, &i.specifier)),
            );
        }
        seen
    };
    let up_front = closure(vec!["/app.js".to_string()], false);
    let on_demand: Vec<String> = closure(vec!["/app.js".to_string()], true)
        .difference(&up_front)
        .cloned()
        .collect();

    let json = serde_json::json!({
        "specifiers": specifiers,
        "static": up_front,
        "lazy": on_demand,
    });
    std::fs::create_dir_all(out.join("e2e")).expect("create e2e/");
    std::fs::write(
        out.join("e2e/resolution.json"),
        serde_json::to_string_pretty(&json).unwrap(),
    )
    .expect("write e2e/resolution.json");
}

/// Neither a URL nor `/`, `./` or `../`: what only the import map resolves.
fn is_bare(specifier: &str) -> bool {
    !(specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../"))
        && Url::parse(specifier).is_err()
}
