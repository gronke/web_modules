# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Per-release notes are also published on each [GitHub Release](https://github.com/gronke/web_modules/releases) (sourced from the annotated tag) and on [crates.io](https://crates.io/crates/web_modules).

## [Unreleased]

### Fixed

- fix(serve): filesystem reads and on-the-fly compiles run on tokio's blocking pool — concurrent requests no longer queue behind one slow read or compile
- fix(dev): a response that fails to build is a `500`, not a panic

## [0.5.0] - 2026-07-06

### Added

- feat(build): duplicate output detection — `build` fails before writing anything when two sources claim one output path, listing every conflict; `dev` warns about each conflict at startup instead of failing; `--skip-duplicates` (both commands, `Processors`, and the builders) keeps the highest-precedence source silently
- feat: selectable symlink modes — `--symlinks follow|follow-unsafe|redirect|move` (also `Processors::symlinks`, the builders, and `Frontend::symlinks`) choose what a source-tree symlink means, consistently across `build`, `dev`, and the static router: `follow` (default) keeps the within-root containment, `follow-unsafe` follows everywhere, `redirect`/`move` answer `307`/`308` with the link content as the `Location` while a build skips the link with a warning; the two redirect modes are compiled behind the default-on `symlink-move` feature, so `--no-default-features` builds cannot express them at all
- feat(build): generated outputs are reserved — a source claiming `importmap.json`, a path under `web_modules/`, or (with `--gzip`) the `.gz` sidecar of an emitted file fails the build even under `--skip-duplicates`, which arbitrates source-against-source precedence only
- `web_modules::build::DEFAULT_HTML` — the fallback inline `index.html` the `Build` builder and the CLI share, as a public constant

### Changed

- **`build` stages the output and replaces `--out` atomically** — a reused output directory can no longer retain stale files from a previous build (a removed source's emitted module, a dropped package's vendored files), and a failed build leaves the previous output untouched; `--out` must be absent, empty, or a previous build's output (marked `.web-modules-out`), so a mistyped `--out .` is refused instead of deleting anything — delete a pre-existing output directory once when upgrading; the vendor cache carries over between builds and no-longer-requested packages are pruned
- refactor(build): one preflight scan of the source roots decides what every stage emits, and each output path is written exactly once by its winner; runtime-helper vendoring and the unresolved-import check read imports captured as each file is emitted instead of re-scanning the emitted `.js`
- Under `--skip-duplicates`, a conflict resolves by one rule in `build` and `dev` alike: earlier root first, then a Tera template over a literal file over a transformed sibling — a later root's `.tera` no longer overwrites an earlier root's file, and `dev` now serves a literal `.js`/`.css` instead of compiling a shadowed sibling source
- The unresolved-import check runs after Tera rendering, and JavaScript rendered from a template joins the module graph — an unresolvable import in it now fails the build
- `build` warns when a copied `.js` parses under neither the module nor the classic-script goal — its imports cannot be validated
- The import map's `{ "imports": … }` wire shape is a serde derive on `Importmap` itself, so serialization and parsing share one definition; fragment parse errors now carry serde_json's line/column diagnostics
- Without the `typescript` feature, emitted `.js`/`.mjs` is no longer scanned lexically for imports — each such file warns that its imports are not validated, instead of risking phantom bare specifiers from `import` text inside comments or strings
- npm-utils 0.6 (audit, package sources, `--dir`, `--progress`) — the `web-modules npm` passthrough inherits the new CLI; the library APIs the vendorer uses are unchanged

### Fixed

- fix(build): find import specifiers in minified output by reading the AST
- fix(build): specifiers with a URL scheme (`blob:`, `node:`, `about:`, …) are no longer reported as unresolved bare imports — classification asks the WHATWG URL parser (the `url` crate), the browser's own first resolution step
- fix(build): a source file that canonically resolves outside its root (a symlink out of the tree) fails the build instead of being published — the dev server's containment already refused to serve such a path; source-walk problems surface as warnings instead of being silently dropped
- fix(build): the reject list applies to every emitted target, not only static copies — a template or compiled source can no longer materialize a rejected path (`.env.tera` → `.env`, `.env.ts` → `.env.js`), matching what the dev server refuses to serve

### Removed

- `minify::minify_directory`, the in-place, symlink-following tree walk — minification happens inline in the transform, and `minify_str` covers JavaScript the compiler didn't produce

## [0.4.0] - 2026-06-28

### Added

- Fluent `Build` / `Dev` builders (`web_modules::Build` / `Dev`), behind a default-on `builder` feature.
- Zero-config `web_modules` block in `package.json` drives `dev` / `build`; `build` auto-vendors its `dependencies`.
- `PackageSpec::parse`; `web_modules::Decorators` at the crate root.

### Changed

- `build` is the static counterpart of `dev`: positional `[ROOTS]…`, `--out` (default `dist`), vendoring only when given packages/manifests.
- Processor-agnostic pipeline — `build()` / `BuildOptions` / `Processors` need no `typescript`; `DevConfig` aliases `Processors`.
- npm-utils 0.5.3 (native TLS roots, stricter sha512 integrity, hardened extraction); drop grass's clap CLI from the default build.
- The minimum supported Rust version is 1.95 (tracks the oxc transform toolchain).

### Removed

- The `compile` command (folded into `build`).

## [0.3.0] - 2026-06-24

### Added

- The reusable **`web-modules build` GitHub Action** — a composite action that builds a deployable `dist/` (vendor npm, transform TS/SCSS, render `index.html` with the import map injected) with no Node on the runner.
  - Downloads a prebuilt `web-modules` binary for the runner's OS/arch, or builds from source with `from-source: true`.
  - Prebuilt binaries for Linux x86_64/arm64, macOS arm64/x86_64, and Windows x86_64 plus native arm64 (built on `windows-11-arm`); on Windows ARM it prefers the native binary and falls back to the x86_64 build under x64 emulation.
  - With no `version` input the binary matches the pinned action tag (`uses: …@v0.3.0` fetches the v0.3.0 binary — reproducible); moving tags, branches, and commit SHAs use the latest release.
  - A moving `v0` major tag, recreated by CI after each release to point at the highest stable 0.x, so `uses: gronke/web_modules@v0` tracks the latest 0.x.
  - A job summary of each build, and a clear error when the `src` directory is missing.
- A single `SHA256SUMS` per release, which the action verifies the downloaded binary against.
- README badges (CI / crates.io / docs.rs / license), this changelog, and Dependabot for the workflow actions.
- CI: an `actionlint` job (hardened Docker container) linting the workflows; the Pages workflow dogfoods the action end-to-end via the download path.

### Fixed

- Vendor: emit `cargo:rerun-if-changed` for vendored destinations.

## [0.2.0] - 2026-06-20

### Added

- Icons: configurable icon-set builder (`from_image_path` → `generate`).
- `tsconfig_node_modules_paths`: resolve 3rd-party paths from `package.json`.

### Changed

- Gate the `npm-utils` re-export behind a dedicated `npm` feature.
- Require npm-utils 0.5.1; oxc 0.135 → 0.137.
- Docs: cleanup, consistency, brevity.

## [0.1.0] - 2026-06-13

- Initial release: a pure-Rust, buildless toolchain for ES modules and Web Components.

[0.5.0]: https://github.com/gronke/web_modules/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/gronke/web_modules/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/gronke/web_modules/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/gronke/web_modules/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/gronke/web_modules/releases/tag/v0.1.0
