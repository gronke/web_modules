#![cfg(all(feature = "bundle", feature = "npm"))]
//! Integration tests for the `bundle` feature — they double as documentation of the CommonJS→ESM
//! pipeline: install a `node_modules/` tree with npm-utils, then bundle it with rolldown into one
//! self-contained browser ES module. Entirely pure Rust — no Node at any step.
//!
//! Network-gated (`#[ignore]`): each test hits the npm registry and exercises the rolldown tree, so
//! run them with the feature and `--include-ignored`:
//!
//! ```text
//! cargo test --features "bundle npm" --test bundle -- --include-ignored
//! ```

use web_modules::bundle::{bundle, BundleOptions};

/// A minimal React app — a `useState` hook + `react-dom/client` render + JSX. Exercises React's
/// runtime (so its implementation has to be bundled in) and rolldown's JSX transform.
const REACT_COUNTER: &str = r#"
import { useState } from "react";
import { createRoot } from "react-dom/client";

function App() {
  const [n, setN] = useState(0);
  return <button onClick={() => setN(n + 1)}>count {n}</button>;
}

createRoot(document.getElementById("root")).render(<App />);
"#;

/// The whole pipeline a consumer's `build.rs` runs: install `deps` (a package.json `dependencies`
/// fragment) into a temp `web/node_modules/`, write `app.jsx`, bundle it, and return the bundled
/// `app.js` source. `Path` import keeps the `&web.join(..)` calls readable.
fn bundle_app(deps: &str, app_jsx: &str, production: bool) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let web = tmp.path();
    std::fs::write(
        web.join("package.json"),
        format!("{{ \"dependencies\": {{ {deps} }} }}"),
    )
    .unwrap();
    std::fs::write(web.join("app.jsx"), app_jsx).unwrap();

    // 1. Install the transitive dependency tree into web/node_modules/ (npm-utils — pure Rust).
    web_modules::npm::install::node_modules(&web.join("package.json"), web).unwrap();
    // 2. Bundle the entry + node_modules into one browser ES module (rolldown — pure Rust).
    let out = web.join("dist");
    bundle(&BundleOptions {
        entry: &web.join("app.jsx"),
        cwd: web,
        out_dir: &out,
        production,
    })
    .unwrap();

    // rolldown names the output after the entry: app.jsx → app.js.
    std::fs::read_to_string(out.join("app.js")).expect("bundle wrote dist/app.js")
}

#[test]
#[ignore = "network: installs npm packages and builds the rolldown tree"]
fn react_bundles_to_a_browser_safe_production_module() {
    let js = bundle_app(r#""react": "^19", "react-dom": "^19""#, REACT_COUNTER, true);

    // React's implementation is inlined — the bundle is self-contained (no CDN, no bare specifiers a
    // browser would have to resolve), hence large.
    assert!(
        js.len() > 50_000,
        "React should be inlined; bundle is only {} bytes",
        js.len()
    );

    // Browser-safe: the `production` define replaced `process.env.NODE_ENV`, so no unresolved
    // `process.env` access survives (that would throw `process is not defined` in a browser). The
    // only residual `process` mentions are `typeof process` guards — legal on an undeclared global
    // and short-circuiting — so there is no bare `process` dereference.
    assert!(
        !js.contains("process.env"),
        "the production bundle must carry no unresolved process.env"
    );
}

#[test]
#[ignore = "network: installs npm packages and builds the rolldown tree"]
fn the_production_flag_selects_react_prod_and_shrinks_the_bundle() {
    let dev = bundle_app(
        r#""react": "^19", "react-dom": "^19""#,
        REACT_COUNTER,
        false,
    );
    let prod = bundle_app(r#""react": "^19", "react-dom": "^19""#, REACT_COUNTER, true);

    // rolldown folds `process.env.NODE_ENV` away at bundle time in BOTH modes, so neither bundle
    // leaks a bare `process.env` to the browser; the `production` flag controls *which* value it
    // folds in — and therefore which React build gets selected.
    assert!(
        !dev.contains("process.env"),
        "dev: process.env is resolved away"
    );
    assert!(
        !prod.contains("process.env"),
        "prod: process.env is resolved away"
    );

    // production=false (no define, no minify) bundles React's *development* build — its per-module
    // region header survives in the un-minified output.
    assert!(
        dev.contains("react.development"),
        "the dev bundle uses React's development build"
    );

    // production=true folds in NODE_ENV=production → React's *production* build, dev branches
    // dead-code-eliminated, output minified — strictly smaller than the dev bundle.
    assert!(
        prod.len() < dev.len(),
        "prod ({}) should be smaller than dev ({})",
        prod.len(),
        dev.len()
    );
}

#[test]
#[ignore = "network: installs npm packages and builds the rolldown tree"]
fn dependencies_share_a_single_react_instance() {
    // react-dom depends on react (a peer dependency) and the app imports react too. A correct bundle
    // includes React *once* — duplicating it would yield two React instances and break hooks
    // ("invalid hook call") at runtime. Checked on the un-minified dev bundle, where rolldown's
    // per-module region headers survive and are countable.
    let dev = bundle_app(
        r#""react": "^19", "react-dom": "^19""#,
        REACT_COUNTER,
        false,
    );
    let react_copies = dev
        .matches("node_modules/react/cjs/react.development")
        .count();
    assert_eq!(
        react_copies, 1,
        "React must be bundled exactly once (single instance); found {react_copies}"
    );
}

// ---- Containment (offline: hand-made node_modules, no registry) ----
// The source tree may be untrusted: nothing outside `cwd` may be folded into the bundle.

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn bundle_refuses_a_relative_import_escaping_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let web = tmp.path().join("web");
    write(
        &web.join("app.js"),
        "import '../secret.js';\nexport const x = 1;\n",
    );
    write(
        &tmp.path().join("secret.js"),
        "export const leaked = 'TOPSECRET';\n",
    );
    let out = web.join("dist");
    let err = bundle(&BundleOptions {
        entry: &web.join("app.js"),
        cwd: &web,
        out_dir: &out,
        production: false,
    })
    .unwrap_err();
    assert!(err.to_string().contains("outside the bundle cwd"), "{err}");
    assert!(!out.join("app.js").exists(), "no bundle emitted");
}

#[cfg(unix)]
#[test]
fn bundle_refuses_a_symlinked_package_escaping_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let web = tmp.path().join("web");
    let outside = tmp.path().join("outside-pkg");
    write(
        &outside.join("package.json"),
        r#"{"name":"evil","main":"index.js"}"#,
    );
    write(
        &outside.join("index.js"),
        "module.exports = { leaked: 'TOPSECRET' };\n",
    );
    write(&web.join("app.js"), "import 'evil';\nexport const x = 1;\n");
    std::fs::create_dir_all(web.join("node_modules")).unwrap();
    std::os::unix::fs::symlink(&outside, web.join("node_modules/evil")).unwrap();
    let err = bundle(&BundleOptions {
        entry: &web.join("app.js"),
        cwd: &web,
        out_dir: &web.join("dist"),
        production: false,
    })
    .unwrap_err();
    assert!(err.to_string().contains("outside the bundle cwd"), "{err}");
}

#[test]
fn bundle_still_folds_in_cwd_modules() {
    let tmp = tempfile::tempdir().unwrap();
    let web = tmp.path().join("web");
    write(
        &web.join("app.js"),
        "import { marker } from './lib/marker.js';\nexport const x = marker;\n",
    );
    write(
        &web.join("lib/marker.js"),
        "export const marker = 'MARKER_LOCAL';\n",
    );
    let out = web.join("dist");
    bundle(&BundleOptions {
        entry: &web.join("app.js"),
        cwd: &web,
        out_dir: &out,
        production: false,
    })
    .unwrap();
    let js = std::fs::read_to_string(out.join("app.js")).unwrap();
    assert!(js.contains("MARKER_LOCAL"), "{js}");
}
