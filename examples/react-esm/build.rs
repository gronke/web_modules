//! Build pipeline for the `react-esm` example — entirely pure Rust, no Node, no CDN:
//!
//!   1. `npm_utils::install::node_modules` installs react + react-dom + zustand (all
//!      CommonJS) into `web/node_modules/` — the "npm install", in Rust.
//!   2. `web_modules::bundle::bundle` bundles `web/app.tsx` plus that `node_modules/` tree
//!      into ONE browser ES module (`$OUT_DIR/dist/app.js`) with rolldown: CommonJS→ESM,
//!      JSX/TS transformed, `process.env.NODE_ENV` folded to `"production"`, React inlined
//!      exactly once, output minified.
//!   3. writes a tiny `index.html` next to it — the bundle is self-contained, so there is
//!      no import map and no bare specifier for the browser to resolve.
//!
//! `main.rs` embeds `$OUT_DIR/dist` with `include_dir!`, so the shipped binary serves the
//! bundle statically with no rolldown linked in.

use std::path::PathBuf;

use web_modules::bundle::{bundle, BundleOptions};

const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>web-modules · React (bundled CJS→ESM)</title>
</head>
<body>
<div id="root"></div>
<script type="module" src="/app.js"></script>
</body>
</html>
"#;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web = manifest.join("web");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dist");

    // 1. Install the (transitive) dependency tree into web/node_modules/ — pure Rust, no npm.
    web_modules::npm_utils::install::node_modules(&web.join("package.json"), &web)
        .expect("install react/react-dom/zustand into web/node_modules");

    // 2. Bundle the entry + node_modules into one browser ES module (rolldown — pure Rust).
    std::fs::create_dir_all(&out).expect("create dist dir");
    bundle(&BundleOptions {
        entry: &web.join("app.tsx"),
        cwd: &web,
        out_dir: &out,
        production: true,
    })
    .expect("bundle the react app");

    // 3. The bundle is self-contained, so index.html only needs the module <script>.
    std::fs::write(out.join("index.html"), HTML).expect("write index.html");

    // Re-bundle when the app or its declared dependencies change.
    println!("cargo:rerun-if-changed=web/app.tsx");
    println!("cargo:rerun-if-changed=web/package.json");
}
