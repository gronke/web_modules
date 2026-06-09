//! Composition example: build one app out of several sibling examples, referenced by
//! their `web/` directories. It reuses the **lit-element** counter and the **d3** chart
//! and wires them together — each click logs its timestamp and the chart redraws the
//! press distribution over time — Bootstrap-themed.
//!
//! It shows web-modules' composition path end to end:
//!   - `web/package.json` local **path-deps** (`file:../../<sibling>/web`) become
//!     [`Mount`](web_modules::Mount)s named by the dependency key (`counter`, `chart`);
//!     registry deps (`lit`) become vendoring specs — both from one
//!     [`read_package_json`].
//!   - One **mount set** drives everything: the runtime **import map**
//!     ([`Importmap::from_mounts`]) and the editor **tsconfig**
//!     ([`write_tsconfig_base`]) are co-generated from it, so they can't drift.
//!   - The dev server serves every mount under its prefix, compiling `.ts`/`.scss` on the
//!     fly and hiding the sources (only the compiled `.js`/`.css` are reachable).
//!
//! Third-party runtime deps are vended into *this* app's `web_modules/` (sibling
//! `web_modules/` are vended on demand and gitignored, so a composing app owns its
//! vendored assets). Path-deps mount the sibling *source components*.
//!
//! `cargo run -p compose`, then open the printed URL and click the counter.

use std::net::SocketAddr;
use std::path::Path;

use web_modules::importmap::Importmap;
use web_modules::tsconfig::write_tsconfig_base;
use web_modules::vendor::{read_package_json, vendor, PackageSpec};
use web_modules::Mount;

const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>web-modules · compose (Lit counter × D3 chart)</title>
<link rel="stylesheet" href="/app.css">
<script src="/web_modules/d3/dist/d3.min.js"></script>
{importmap}
<script type="module" src="/app.js"></script>
</head>
<body class="p-5 bg-body-tertiary">
<h1 class="h4 mb-3">Press distribution over time</h1>
<p class="text-secondary mb-4">
  Click the counter — each click logs its timestamp, and the D3 chart shows how many
  presses fell in each one-second bucket. Counter from the <code>lit-element</code>
  example, chart from the <code>d3</code> example, Bootstrap-themed — composed by reference.
</p>
<counter-card count="0" class="d-block mb-4"></counter-card>
<svg id="presses" width="520" height="240" class="border rounded bg-white"></svg>
</body>
</html>
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let web = manifest.join("web");

    // 1. Read package.json: registry deps → vendoring specs; local path-deps → mounts
    //    (named by the dependency key — `counter`, `chart`).
    let (mut specs, sibling_mounts) = read_package_json(&web.join("package.json"))?;
    // d3 ships a UMD bundle (loaded as a global <script>) and Bootstrap is vended for its
    // SCSS only — neither needs an import-map entry, a tweak a flat range can't express,
    // so they're declared programmatically (cf. the lit-element example's popper tweak).
    specs.push(PackageSpec::npm("d3", "^7").no_imports());
    specs.push(PackageSpec::npm("bootstrap", "^5").no_imports());

    // 2. Vendor the third-party deps into our own web_modules/; `lit` auto-derives its
    //    import-map entries from its package.json.
    let vendored = vendor(&web.join("web_modules"), "/web_modules", &specs)?;

    // 3. One mount set: the sibling component sources, plus our own files at the root.
    let mut mounts = sibling_mounts;
    mounts.push(Mount::root(&web));
    dedup_by_prefix(&mut mounts);

    // 4. Co-generate the runtime import map and the editor tsconfig from that one set.
    let mut importmap = vendored;
    importmap.extend(Importmap::from_mounts(&mounts));
    importmap.write_to(&web.join("importmap.json"))?;
    write_tsconfig_base(&mounts, manifest, &manifest.join("tsconfig.json"))?;

    // 5. Render index.html with the import map inlined. There's no build step — the dev
    //    server compiles app.ts/app.scss on request — so this is the one generated file.
    std::fs::write(
        web.join("index.html"),
        HTML.replace("{importmap}", &importmap.to_script_tag()),
    )?;

    // Headless (CI cache-warming / Docker image build): vendor + co-gen, then exit.
    if std::env::var_os("WEB_MODULES_VENDOR_ONLY").is_some() {
        return Ok(());
    }

    let app = web_modules::dev::dev_router_mounted(mounts);
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    web_modules::serve(app, addr).await?;
    Ok(())
}

/// Drop any later mount that repeats an earlier mount's URL prefix (first wins) — so a
/// dependency listed twice, or a sibling re-declaring a prefix, can't double-mount.
fn dedup_by_prefix(mounts: &mut Vec<Mount>) {
    let mut seen = std::collections::BTreeSet::new();
    mounts.retain(|m| seen.insert(m.url_prefix().to_string()));
}
