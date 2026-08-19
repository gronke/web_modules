//! Git-source example: consume a dependency that publishes **only TypeScript**, compiled
//! from its own sources by this toolchain.
//!
//! [esptool-js](https://github.com/gronke/fork-esptool-js) is declared in
//! `web/package.json` by git reference and named under
//! `web_modules.sourceDependencies`. There is no built output to vendor — the repository
//! ships `src/*.ts` and nothing else — so vendoring compiles it, into the layout its own
//! `tsconfig.json` declares. What lands in `web_modules/` is browser-ready JavaScript,
//! indistinguishable from any other vendored package: `import … from 'esptool-js'` and
//! nothing here knows it arrived as source.
//!
//! Two steps, both from one `package.json`:
//!   - `read_package_json` → vendoring specs for the prebuilt registry deps (`pako`,
//!     `atob-lite`), minus anything named as a source dependency.
//!   - `source_specs_from_package_json` → the git package, compiled by [`vendor`] into
//!     `web_modules/esptool-js/lib/`, where its manifest already points.
//!
//! The page itself asks an ESP32 what it is, over Web Serial: chip description, MAC,
//! features, crystal frequency and flash size. It only ever reads — no flasher stub is
//! uploaded and nothing is written — so pointing it at a board cannot modify it.
//!
//! `cargo run -p esptool-git`, then open the printed URL in Chrome or Edge (Web Serial)
//! and connect a board.

use std::net::SocketAddr;
use std::path::Path;

use web_modules::importmap::Importmap;
use web_modules::tsconfig::write_tsconfig_base;
use web_modules::vendor::{read_package_json, source_specs_from_package_json, vendor};
use web_modules::Mount;

const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="icon" href="data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 100 100%22><text y=%22.9em%22 font-size=%2290%22>🔌</text></svg>">
<title>web-modules · esptool-js compiled from its git sources</title>
<link rel="stylesheet" href="/app.css">
{importmap}
<script type="module" src="/app.js"></script>
</head>
<body>
<main>
  <h1>What is this chip?</h1>
  <p class="sub">
    esptool-js is compiled from its TypeScript, fetched by git reference — no npm build,
    no committed <code>lib/</code>. Chrome or Edge, for Web Serial.
  </p>
  <button id="connect">Connect a board</button>
  <p id="status" class="status">Not connected.</p>
  <dl id="report" hidden></dl>
  <pre id="log" aria-label="esptool-js output"></pre>
</main>
</body>
</html>
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let web = manifest.join("web");
    let package_json = web.join("package.json");

    // 1. Registry deps → vendoring specs, minus anything named a source dependency.
    let (mut specs, mut mounts) = read_package_json(&package_json)?;

    // 2. The source-built dependency, whose TypeScript vendoring compiles. It sits beside
    //    the prebuilt ones because the result is the same kind of thing: browser-ready
    //    JavaScript under `web_modules/`, entries derived from its own manifest.
    specs.extend(source_specs_from_package_json(&package_json)?);

    // 3. Vendor them all. esptool-js imports `pako` for the deflate stream and
    //    `atob-lite` to decode the flasher stubs.
    let vendored = vendor(&web.join("web_modules"), "/web_modules", &specs)?;

    // 4. Our own files at the root; the vendored tree needs no mount.
    mounts.push(Mount::root(&web));

    // 5. Co-generate the runtime import map and the editor tsconfig from that one set.
    let mut importmap = vendored;
    importmap.extend(Importmap::from_mounts(&mounts));
    importmap.write_to(&web.join("importmap.json"))?;
    write_tsconfig_base(&mounts, manifest, &manifest.join("tsconfig.json"))?;

    // 6. Render index.html with the import map inlined. There is no build step — the dev
    //    server compiles app.ts/app.scss on request — so this is the one generated file.
    std::fs::write(
        web.join("index.html"),
        HTML.replace("{importmap}", &importmap.to_script_tag()),
    )?;

    // Headless (CI cache-warming / Docker image build): fetch + co-gen, then exit.
    if std::env::var_os("WEB_MODULES_VENDOR_ONLY").is_some() {
        return Ok(());
    }

    let app = web_modules::dev::dev_router_mounted(mounts);
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    println!("esptool-git: http://{addr}/");
    web_modules::serve(app, addr).await?;
    Ok(())
}
