# web_modules

[![CI](https://github.com/gronke/web_modules/actions/workflows/ci.yml/badge.svg)](https://github.com/gronke/web_modules/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/web_modules.svg)](https://crates.io/crates/web_modules)
[![docs.rs](https://img.shields.io/docsrs/web_modules)](https://docs.rs/web_modules)
[![License: MIT](https://img.shields.io/crates/l/web_modules)](LICENSE)

**Pure-Rust tooling for developing Web Components**: vendor npm packages, transform
TypeScript/SCSS, and serve or embed a native-ESM frontend, with **no Node, no npm and no
bundler** at build time. Use it as a **`web-modules` CLI** for everyday development, or as a
**library** from a `build.rs` / at runtime. Built on [`npm-utils`], [oxc], [`grass`] and [rolldown].

## What it does

- **Vendor** - resolve and download npm packages into `web_modules/<name>`, targeted or including dependencies.
- **Transform** - compile and convert source files, minify and process.
- **Dev server** - serve from source, compile on the fly, watch and live-reload.
- **Build** - vendor, transform and render a deployable `dist/` - bake it into your binary, or ship it as a static site (the `web-modules build` CLI or a [GitHub Action](#github-actions)).
- **Bundle** *(opt-in)* - fold CommonJS packages and their `node_modules/` into ES modules.

## Features

Each is a Cargo `--features` flag:

- **typescript / scss** - compile to browser JS and CSS
- **tera** - HTML and [import map] templating
- **minify · dts · i18n · icons** - optional processors
- **compress** - gzip sidecars for static serving
- **bundle** - CommonJS to ESM
- **npm** - expose the `npm-utils` API as `web_modules::npm` (resolve · install · ci)
- **axum · dev** - serve the frontend, with a live-reload dev server

## CLI

```bash
cargo install web_modules --features cli
```

<!-- regenerate: cargo run -p web_modules --bin web-modules --features cli -- --help -->

```console
$ web-modules --help
Buildless web frontend toolchain (no Node)

Usage: web-modules <COMMAND>

Commands:
  dev     Dev server: compile TS/SCSS on the fly, render `*.tera`, watch the tree, live-reload
  build   Build a deployable output tree — the static counterpart of `dev`
  vendor  Vendor npm packages into web_modules/ + an import map
  ci      Install a package-lock.json's exact tree into node_modules/ - a pure-Rust npm ci
  npm     Run an npm-utils command (add · install · ci · upgrade · …)
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

`build` is the **static counterpart of `dev`** — same source roots and processors, emitted to `--out` instead of served — and it vendors npm only when you pass `--package`/`--manifest`; `vendor` just fetches dependencies into `web_modules/`. Each compiler processor (typescript, scss, tera, minify, gzip) has a `--<name>` / `--no-<name>` toggle, and `--no-default-features` turns the default-on set (typescript, scss, tera) off so you re-enable them individually. Run `web-modules <command> --help` for flags.

### HTML policy

The build never reads or rewrites your HTML.
Pages are only generated where you opt in: a `*.tera` template (rendered with the generated import map as the `{{ importmap }}` variable), or the `--html`/`--template` fallback when no source provides an `index.html` at all.
The generated import map is the contract — available as `importmap.json`, the `{{ importmap }}` Tera variable, and the `{importmap}` placeholder — and it is the only map the unresolved-import check validates against; a hand-authored page owns its own inline map.
JavaScript rendered from a template joins the module graph and is validated like any other emitted module, with one ordering rule: runtime-helper vendoring is decided before templates render, so an `@oxc-project/runtime` import appearing only in template-rendered JavaScript fails the unresolved-import check instead of vendoring the runtime — put such code in a `.ts`/`.js` source instead.

### Duplicate output paths

When two sources claim one output path — `index.html` next to `index.html.tera`, `app.js` next to `app.ts`, `style.css` next to `style.scss`, or the same relative path in two roots — `build` fails before writing anything and lists every conflict; `dev` warns on the console instead.
`--skip-duplicates` opts into precedence: the earlier root wins, and within a root a Tera template beats a literal file beats a transformed sibling — the same rule in `build` and `dev`.
Generated outputs are reserved regardless: a source claiming `importmap.json`, a path under `web_modules/`, or (with `--gzip`) the `.gz` sidecar of an emitted file fails the build even under `--skip-duplicates`.

### Output directory

Each build is staged in a temporary sibling directory and then **atomically replaces** `--out`, so the output always describes exactly the current sources — nothing from a previous build survives, and a failed build leaves the previous output untouched.
`--out` must therefore be dedicated: absent, empty, or a previous build's output, which the build recognizes by the `.web-modules-out` marker it writes.
Anything else — the project directory under `--out .`, a directory with your own files — is refused rather than deleted; delete a pre-existing output directory once when upgrading.
Vendored packages are not re-downloaded on every build: the `web_modules/` cache carries over from the previous output and is re-validated, and packages you no longer request are pruned.

### Symlinks

What a symlink in a source tree means is selectable with `--symlinks` (also `Processors::symlinks`, the builders' `.symlinks(…)`, and `Frontend::symlinks`), consistently across `build`, `dev`, and the static router:

| Mode | build | serving |
|---|---|---|
| `follow` (default) | a link resolving outside its own root fails the build | 404 |
| `follow-unsafe` | every link publishes; a dangling one warns and skips | a dangling one 404s |
| `redirect` | links are skipped with a warning | `307 Temporary Redirect`, the link content is the `Location` |
| `move` | links are skipped with a warning | `308 Permanent Redirect`, same rule |

Under `follow` a link works within its own source root and never across roots.
The redirect modes answer without ever opening the target — the link content is the `Location`, taken literally (plus the request's remaining components when a directory link is on the way) — which is also why a static build has nothing to emit for a link and skips it.
In every mode, request-path traversal, the reject list, source-hiding, the SCSS import sandbox, and vendor-extraction hardening are unaffected: a symlink mode never relaxes a security sandbox.
The live-reload watcher's behavior through links is backend-defined; under `follow-unsafe` an edit behind an out-of-tree link may not trigger a reload.

## Library

```toml
[dependencies]
web_modules = "0.4"   # Rust 1.95+
```

`typescript`, `scss` and `tera` are on by default; `full` enables everything except `bundle`.

The fluent `Build` and `Dev` builders (feature `builder`, on by default) are the promoted entry points — `Build` from a `build.rs` (bake a `dist/`), `Dev` for a live-reload server:

```rust
use web_modules::{Build, Dev};

// build.rs — vendor lit, compile web/, write dist/
Build::new().root("web").vendor("lit@^3").out("dist").minify(true).run()?;

// a live-reload dev server (the `dev` feature)
Dev::new().root("web").serve("127.0.0.1:8080".parse()?).await?;
```

Both layer over the lower-level `build(&BuildOptions { … })` / `dev::serve_with`, still public for fine-grained use. For the full `build.rs` / runtime API see the **[API docs][docs.rs]**.

## GitHub Actions

A composite action builds a deployable `dist/` (vendor + transform + render, with the import map injected) — **no Node on the runner**. It downloads a prebuilt `web-modules` binary for the runner's OS/arch (Linux x86_64/arm64, macOS arm64/x86_64, Windows x86_64/arm64), or compiles from this action's source with `from-source: true`. Pin `@v0` to track the latest 0.x, or an exact `@v0.3.1` — which fetches the matching binary (reproducible); the `version` input overrides this. The action is **build-only**; compose it with the official actions to publish.

**Build a dist artifact:**

```yaml
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: gronke/web_modules@v0
        with:
          packages: "lit@^3 bootstrap@^5"   # and/or: manifest: web (a dir) or web/package.json
          template: web/index.html.tera     # or inline `html:`; omit for a minimal default
          minify: true
      - uses: actions/upload-artifact@v7
        with: { name: site, path: dist }
```

**Deploy to GitHub Pages** — grant the Pages permissions + environment on the job, then build and publish with the standard actions:

```yaml
jobs:
  deploy:
    runs-on: ubuntu-latest
    permissions: { pages: write, id-token: write }
    environment: { name: github-pages, url: "${{ steps.deploy.outputs.page_url }}" }
    steps:
      - uses: actions/checkout@v7
      - uses: gronke/web_modules@v0
        with:
          packages: "lit@^3 bootstrap@^5"
          template: web/index.html.tera
          mount: /my-repo/web_modules        # project page is served under /<repo>/
      - uses: actions/configure-pages@v6
      - uses: actions/upload-pages-artifact@v5
        with: { path: dist }
      - id: deploy
        uses: actions/deploy-pages@v5
```

Enable Pages once under *Settings → Pages → Source: GitHub Actions*. A **project** page is served under `/<repo>/`, so pass `mount: /<repo>/web_modules` and keep entry scripts **relative** (`./app.js`); a user/org `*.github.io` page serves at the root (default `mount: /web_modules`). This repo dogfoods the action — [`examples/gh-pages/`](examples/gh-pages) is built and deployed to Pages by [`.github/workflows/pages.yml`](.github/workflows/pages.yml). Run `web-modules build --help` for every flag.

## Examples

The [`examples/`](examples/) tree is full of runnable demos; `cargo run` and open the browser. A few picks:

- [**lit-element**](examples/lit-element) - a Lit 3 component themed with Bootstrap 5, baked at build time, served by axum.
- [**d3**](examples/d3) - a bar chart with D3, a non-Lit npm dependency vendored and served as-is.
- [**react-esm**](examples/react-esm) - React from npm bundled into one browser ES module, entirely in Rust (the `bundle` feature).
- [**embedded**](examples/embedded) - the whole frontend baked into the binary; no filesystem, no network.
- [**tauri**](examples/tauri) - a Tauri v2 desktop app, frontend live-served (and release-baked) by web_modules.

## License

MIT. See [LICENSE](LICENSE).

[import map]: https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/script/type/importmap
[`npm-utils`]: https://github.com/gronke/rust-npm-utils
[oxc]: https://oxc.rs
[`grass`]: https://github.com/connorskees/grass
[rolldown]: https://rolldown.rs
[docs.rs]: https://docs.rs/web_modules
