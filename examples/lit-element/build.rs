//! Bakes the frontend at build time: vendors the npm dependencies, transforms
//! `web/*.ts` and compiles `web/*.scss`, and renders `index.html` with the import
//! map — all into `$OUT_DIR/dist`, which `main.rs` embeds with `include_dir!`.
//!
//! The browser dependencies are sourced from `web/package.json` (`dependencies`),
//! so they're maintained like any npm project; `devDependencies` there (tooling)
//! are not vended. Two packages need per-package vendoring tweaks a flat range
//! can't express, so they're added programmatically — showing both styles.

use std::path::PathBuf;

use web_modules::build::{build, BuildOptions};
use web_modules::vendor::{specs_from_package_json, PackageSpec};

const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>web-modules · Lit + Bootstrap</title>
<script src="/web_modules/@webcomponents/webcomponentsjs/webcomponents-loader.js"></script>
<link rel="stylesheet" href="/web_modules/bootstrap/dist/css/bootstrap.min.css">
<link rel="stylesheet" href="/styles.css">
{importmap}
<script type="module" src="/app.js"></script>
</head>
<body class="py-5 bg-body-tertiary">
<counter-card count="3"></counter-card>
</body>
</html>
"#;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dist");

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
        src: &manifest.join("web"),
        out: &out,
        mount: "/web_modules",
        html: HTML,
        template: None,
        output: Default::default(),
    })
    .expect("build lit-element frontend");
}
