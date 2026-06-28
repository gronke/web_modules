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
