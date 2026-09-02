# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Per-release notes are also published on each [GitHub Release](https://github.com/gronke/web_modules/releases) (sourced from the annotated tag) and on [crates.io](https://crates.io/crates/web_modules).

## [Unreleased]

### Changed

- The dev server's live reload hot-swaps stylesheets in place instead of reloading the page, and streams every change over SSE (`/_web_modules/live/events`) to a client of its own (`/_web_modules/live/live.js`); `tower-livereload` is gone.
  Non-stylesheet changes still reload the page by default; `--live-reload css` (`Dev::live_reload(ReloadMode::Css)`) turns that into a console note, `--no-live-reload` (`ReloadMode::Off`) serves without watcher, stream and client.
  The `dev` feature now pulls `futures-core` (the `Stream` trait axum's SSE response takes; already compiled in every axum build) and tokio's `sync`.

- **Breaking:** `--minify` now strips comments from emitted JS: normal, JSDoc and annotation comments go, legal comments (`//!`, `/*!`, `@license`, `@preserve`) stay inline — deliberately not oxc's own minify preset, which drops those too.
  Previously every comment survived minification.
  Pass `--comments keep` for the old behavior.
- **Breaking:** `--minify` now minifies the whole dist tree — byte-copied `.js`/`.mjs`, Tera-rendered JS, `npm://` assets, and the vendored `web_modules/` — instead of only compiled TypeScript output.
  Every file goes through one oxc parse→codegen pass: a first-party module that fails to parse is now a build error (it was copied with a warning), and its recorded imports come from the final AST, so an import removed by dead-code elimination no longer counts against the import map.
  Vendored files that fail to parse are left as shipped, aloud.
  The vendor shaping joins the `vendor-profile:` marker line, so a toggled rebuild re-vendors instead of reusing the differently-shaped cache.
- The `full` feature now includes `bundle` (the rolldown CommonJS→ESM path), so `--features full` builds exactly what the released `web-modules` binary ships, and the action's from-source install matches the download.
  Library consumers on `full` now compile the rolldown / second-oxc tree; the new `lean` feature is the previous `full` set, and docs.rs and the MSRV check use it.

### Added

- `web_modules::live`: the live-reload hub behind the dev server, for hosts with their own compilers or watchers: `LiveReload::watch(mounts)` / `::new`, `record_dependencies(url, paths)`, `notify(path)`, `publish(change)`, `router()` / `events_router()`, `script_tag()` / `meta_tag()`, `inject_script(router, prefix)`.
  The stream says what changed as a kind and a served URL, never as a filesystem path; the browser client swaps a changed `<link rel="stylesheet">` without a flash and dispatches `web-modules:css-reloaded`.
- `scss::compile_file_tracked`: `compile_file` plus the list of files the compile read (the entry and every partial), for caches and dependency maps.
- `dev::dev_router_with_live` / `dev::serve_with_live`: the `dev_router_with` / `serve_with` pair with the live-reload policy.
- `typescript::rewrite_str` and a public `RewriteOptions`: apply an output policy (minify, comments, inline source map) to plain JavaScript through the transformer-free rewrite pass the build already uses internally.
  Consumers no longer route generated or copied JS through `compile_str_with`, whose Lit-preset transform may alter hand-written semantics.
- `PackageSpec::keep_tagged`: a keep-filter with a tag that joins the extraction cache key.
  A `fn` pointer has no stable identity, so a tagless filter reuses a cached tree even after the filter's shape changed; the tag makes a filter change re-extract, and a tree cached without one re-extracts once on adoption.
- `SplitBundleOutput` gained typed emitted lists, `emitted_js` and `emitted_maps`; `emitted` stays their union.
  Callers that read the emitted files by content want the JS list alone — a `.map`'s `sourcesContent` embeds module sources and poisons text scans, which is how a consumer's dangling-import gate once produced false positives.
  The pipeline's own orphan guard now exempts `emitted_js`.
- `env::flag(name, default)`: the boolean policy knob every embedding build script re-implements.
  Unset and empty both mean the default (a Docker `ARG X=` passed through `ENV X=${X}` yields an empty string), `"1"`/`"0"` are explicit, and anything else panics naming the variable.
- `examples/bundle`: a pure-frontend demo of `--bundle`, configured from the `web_modules` block in `package.json`.
- The `embedded` example bakes source maps and collects legal comments; the gh-pages CI dogfood covers `--comments collect` and `--no-minify-web-modules`.
- `--bundle` and `--bundle-entry <path>` (package.json `"bundle": {"entries": [...]}`, builder `Build::bundle` / `Build::bundle_entry`, action inputs `bundle`/`bundle-entries`) fold the built tree per entry point, inside the atomic build.
  Each entry keeps its exact URL with its imports inlined, shared and dynamically-imported code lands in content-hashed `chunks/`, and `importmap.json` + `web_modules/` drop out of the output; minify, comments and sourcemap apply through rolldown's own single pass.
  A surviving module whose bare imports relied on the removed import map fails the build by name — a worker script or second page needs its own entry.
  The released binary now carries the `bundle` feature, and so does `full` (see Changed).
  Bundled builds re-vendor from the network each time, since the vendored tree is consumed rather than cached.
- `SplitBundleOptions` gained `sourcemap` and `comments` fields and `SplitBundleOutput` gained `emitted` and became `#[non_exhaustive]` — **Breaking** for struct-literal construction of either.
  `tests/bundle_split.rs` now also runs in CI; it never did.
- `--comments <keep|strip|collect|none>` (package.json `"comments"`, builder `Build::comments`, library `Output::comments`, action input `comments`) sets the comment policy for every emitted JS file.
- `--minify-web-modules` / `--no-minify-web-modules` (package.json `"minify": {"webModules": false}`, builder `Build::minify_web_modules`, action input `minify-web-modules`) keep the vendored `web_modules/` tree and `npm://` assets out of `--minify`, byte-identical to what the packages shipped. On by default under `--minify`.

- `--sourcemap` emits source maps for compiled TypeScript, in `build` and `dev` alike (package.json key `sourcemap`; builders `Build::sourcemap` / `Dev::sourcemap`).
  `build` writes a `<file>.map` sidecar beside each compiled file and links it by file name; `dev` appends the map inline as a `data:` URL, so no extra route or cache entry exists.
  Sources ship inside the map (`sourcesContent`), so it works although `.ts` files are excluded from the output; with `--gzip` the sidecar is compressed too, and its path is reserved like the other generated files.
  Off by default, so an embedded dist (`include_dir!`) stays lean.
  SCSS is not covered — grass emits no source maps.
- With `--sourcemap`, vendoring also keeps a package's shipped `.js.map`/`.css.map` sidecars beside the assets they describe; the asset filter always dropped them, forcing consumers who wanted them into a custom extract filter.
  Without the flag they are swept — in the gh-pages demo they outweigh the assets they describe — and the choice is recorded as a `vendor-profile:` line in the output marker, so a rebuild with the toggle flipped re-vendors instead of reusing the differently-shaped cache.

### Fixed

- `vendor` follows the `url()` references in the stylesheets it keeps, so a font or an image that only a stylesheet names is vendored alongside it instead of 404ing in the browser.
  References are read through the CSS tokenizer (`cssparser`), so a `url(` inside a comment or a string never counts as one.
- The dev server served a stale stylesheet after editing a partial: its cache was keyed on the entry's mtime alone.
  A compiled stylesheet now revalidates every file it read.

## [0.7.0] - 2026-08-21

The crate gains a tarball dependency source (below); the rest of this release carries the release pipeline and the maintainer documentation.

### Fixed

- `vendor` warns about a package it vendored that nothing in the import map points at and, for one that publishes only TypeScript, names `web_modules.sourceDependencies` as the remedy. An asset-only package legitimately maps nothing, so the remedy is offered as a conditional. Silence here is the worst case: the tree is on disk, the exit status is zero, and the break surfaces later in a browser — or not at all, while a stale inline import map still resolves. A git dependency is the usual cause, since a whole-repo archive derives no entry until it is compiled. (`build` vendors through `build::build`, which does not return the map, so it is not covered yet.)
- `vendor` reports how many packages and entries it wrote, so a run that quietly did less than expected is visible in the one line it prints.
- A source dependency's compiled entry is checked against its own manifest: when the layout its `tsconfig.json` describes is not the one it was published with, vendoring says so instead of leaving `auto_entries` to drop the package from the import map without a word.
- A program source outside an explicit `rootDir` is refused, as `tsc` refuses it (TS6059). It was skipped, so an importer was emitted whose import had been compiled to nowhere and then deleted.
- `rewriteRelativeImportExtensions` is honoured, so a package whose sources name `./util.ts` emits `./util.js` beside the file it names. An emitted specifier that still carries a TypeScript extension is refused, since the source it names does not survive vendoring.
- A relative import resolves to the source of its own module format: `./foo.mjs` is written by `foo.mts`, and a `foo.ts` sibling is no longer compiled in its place.
- `files` and `include` name a program's root files, so a source one of them imports is compiled too and `exclude` no longer removes it. Only the selected files were compiled, and the cleanup then deleted the rest — a root importing a sibling emitted an import that resolved to nothing.
- A missing `rootDir` is inferred from the program's own input files, as `tsc` does, rather than from the text before a glob's first wildcard. A package whose sources all sit under `src/deep` emits from there, where its manifest points.
- `.cts` is CommonJS source and is refused rather than renamed to `.cjs`, as is a config declaring CommonJS output through `module` or an ES 3 / ES 5 target. The transform strips types; it does not rewrite module code.
- `.d.mts` and `.d.cts` are declarations, not sources. They passed as ordinary `.mts`/`.cts` files and could be emitted as `.d.mjs`/`.d.cjs`.
- An absolute `rootDir` or `outDir` is refused rather than read as package-relative: it was safe from escape but silently compiled to a layout the dependency did not ask for.
- A source dependency's `rootDir` and `outDir` are refused unless they stay inside the package, and the source root is deleted only when it is a strict descendant that does not hold the output. The config comes from a downloaded archive, so `rootDir: ".."` named the directory holding the package — which was then walked, written beside, and removed.
- `files`, `include` and `exclude` select which files are compiled, with the directories `tsc` excludes regardless and the output directory subtracted. Everything under the source root was compiled, so an excluded dev or test source was emitted, and one that did not compile failed the vendoring.
- A `tsconfig.json` naming a JSX mode, import source or factory is refused: `.tsx` compiles, but through the transform's own JSX handling rather than the one the config asks for.
- A source dependency is compiled with its own emit semantics rather than this project's: `experimentalDecorators` and `useDefineForClassFields` are read from its `tsconfig.json`, and without the latter the target decides, as `tsc` does. The zero-config compile is the Lit preset, which gave a dependency legacy decorators and assignment-style class fields it never declared.
- `.mts` compiles to `.mjs` and `.cts` to `.cjs`, and `.cts` reaches the compiler at all. Every compiled file was written as `.js`, so a package whose `exports` names `lib/index.mjs` had no such file and lost its import-map entry.
- A `tsconfig.json` `include` may name a file rather than a glob. `include: ["src/index.ts"]` was read as a directory named `src/index.ts`; entries now contribute the directory they name, and the root is the one they share.
- A compiled destination's cache key carries a compile fingerprint, so a pinned commit recompiles when the compiler changes instead of keeping output from an older release.
- A source dependency is no longer also vended as a plain git dependency, which fetched one repository into two directories named differently — after the repository and after the dependency key.
- A source dependency keeps its licence and notice files too, alongside the sources it compiles.
- A vendored package keeps its `LICENSE`, `NOTICE`, `COPYING` and `AUTHORS` files, which the asset filter dropped. Serving a vendor tree is redistribution, and MIT and Apache-2.0 both require the notice to travel with the code.

### Changed

- CI restores a warm `target/` again: the cache action's target entry is restore-only by design and the paired save was never called, so every run — pull request and main alike — compiled the whole workspace cold across each feature set it checks. Saving on main makes pull requests consumers of it.
- CI no longer runs `apt-get` on either happy path: the build's `zstd` install had nothing to install ("already the newest version, 0 newly installed"), and Playwright's `--with-deps` now runs only when the browser cache misses.
- Every CI job carries a `timeout-minutes` backstop. A stalled apt mirror had hung the build for over 40 minutes and an e2e job for 25, both otherwise bounded only by the six-hour default.
- The CLI section states why `--features cli` is required and that the action needs no install.
- crates.io publication is gated on a human signature covering the release commit — the signed `v<version>` tag satisfies it. An unsigned release rehearses the packaging instead of uploading, and a signed `v<version>-sig` companion pushed later completes the publication, retroactively.
- Before the moving `v0` advances onto a release, the pipeline downloads that release's own binary through the action's installer mode — the path every `@v0` consumer takes — and checks it reports the right version. A release that cannot serve its binary leaves `v0` on the last one that could, and the crate unpublished with it.
- One job now establishes the draft release before the binary matrix fans out. Six runners each creating it raced, and GitHub lets drafts share a tag name, so the losers became rival empty drafts — which is how v0.6.0 went live serving no binaries until its assets were copied across by hand.
- Installer mode is no longer exercised on pull requests: it downloaded the *current* release, so a pull request could not break it and an unrelated release fault failed every pull request. The release pipeline proves it instead, before `v0` moves.

### Added

- `tsconfig::TsConfig` reads a package's `tsconfig.json` — the JSONC the format really is, via `jsonc-parser` — into a typed config: the layout to reproduce and the emit-relevant options. Replaces a hand-rolled comment stripper that ended a string at an escaped quote.
- `walk::files_within` walks a tree that came from outside the project without following symbolic links out of it, and `walk::contains` compares what a path reaches rather than what it reads. Used where an extracted archive is read.
- `ClassFields` sets class-field semantics independently of `Decorators`, so standard decorators can pair with assignment semantics — the combination `tsc` uses below ES 2022.
- A git dependency on a branch or tag is keyed on the archive's contents rather than the reference name, so moving the branch re-vendors instead of silently keeping the old tree; a commit id is keyed on itself and still costs no network once vendored.
- Source-built dependencies: a package named under `web_modules.sourceDependencies` is fetched from its git reference and **compiled** into the layout its own `tsconfig.json` declares, so what lands in `web_modules/` is browser-ready JavaScript with entries derived from its own manifest — what a package publishing only TypeScript needs, and reachable from the CLI, not just a Rust driver.
- `examples/esptool-git`: esptool-js consumed from its git reference and compiled by vendoring, with a Web Serial page that reads a connected ESP32's chip info over a bare `esptool-js` import.
- A CommonJS-only package entry gets a generated ESM wrapper, with the bare import-map specifier pointing at that — a dependency shipping no ESM entry at all is otherwise unimportable in a browser.
- `PackageSpec::tarball(name, url)` and a `package.json` `.tgz` dependency form: vendor a pre-packed `npm pack` tarball from an absolute https URL — e.g. a GitHub Release asset — extracted and import-mapped like an npm package, so a component library can be consumed straight from a Release without a registry. A `…/releases/download/….tgz` URL is recognised ahead of the `github:` shorthand.
- MAINTENANCE.md: what a maintainer of this repository, or of a fork, has to have, configure once, and do on each release, with the failures worth recognising and their fixes.
- `scripts/setup-release.sh` performs that setup idempotently — the `crates-io` environment restricted to `v*` tags with an optional reviewer, the tag rulesets, the `ci:tauri` label, and the crates.io trusted publisher through the registry's API. Immutable releases has no REST surface and is reported rather than attempted.

## [0.6.0] - 2026-07-29

### Added

- The action's `build` input (default `"true"`): with `build: "false"` the action installs the SHA256-verified binary onto `PATH` and skips the build, for jobs whose own scripts drive `web-modules` (a repo build script, `vendor`, `npm audit`)
- Releases run through gronke/rust-ci's release flow: `cut.yml` cuts a release/v<version> branch from CHANGELOG.md and Cargo.toml, the pipeline drafts a reviewable pre-release with an unsigned rc marker, and the human-signed `v<version>` tag is sealed against the reviewed tree before the draft flips live with the binaries attached

### Changed

- docs(security): SECURITY.md describes the current posture — the SCSS import sandbox (the stale "processors are not sandboxed" caveat is gone), CLI config containment, bundle containment, the exact-pinned decorator runtime, and the trust anchors that remain (lockfile integrity is self-referential, vendored packages are not integrity-pinned, `npm://` resolution ascends ancestors, `Mount::from_dir` is a config-trust boundary, no `Host` validation on the dev server)
- npm-utils 0.6.2 — the https scheme guard covers every request in a redirect chain, and the cache wipe unlinks symlinks instead of following them

### Fixed

- fix(typescript): an `_`-prefixed `.ts`/`.tsx`/`.mts` source compiles like any other module — the underscore-partial convention belongs to SCSS, where `_x.scss` is an import-only fragment; ES modules have no such concept, and skipping `_Base.ts` stranded every `import './_Base.js'` in the emitted tree (surfacing only at bundle time, as an unresolved import). `.d.ts` declarations remain no-emit

- fix(dev): live `.tera` renders receive the import map baked into the embedded fallback (its `importmap.json`, the contract artifact `build` emits) instead of always an empty one.
  In the `Frontend::embedded(&DIST).source("web")` composition, an edited page previously rendered `{"imports":{}}` while the fallback kept serving the vendored modules, so bare specifiers (`import { LitElement } from 'lit'`) failed to resolve in live mode.
  Without an embedded fallback the map stays empty as before; an unparseable baked map warns and falls back to empty
- fix(scss): a sandbox-refused `@use`/`@import` no longer reads like a missing file — the compile error appends a `note:` naming every existing path a probe was refused on and points at the missing load path (`grass` resolves imports through `is_file` probes, so the refusal in `read` was unreachable and the failure surfaced only as "Can't find stylesheet to import")

### Security

- security(cli): path fields in a `package.json` `web_modules` block (`roots`, `out`, `template`, `scss.loadPaths`) are confined to the project directory — previously an untrusted repository could serve arbitrary directories via `web-modules dev`, read any file into the output via `template`, and plant a new tree at an arbitrary location via `out`. Every entry must now be purely relative (no root, prefix, or `..` component), and an existing path must canonically resolve inside the project, so a symlink in the tree cannot redirect it outside. CLI flags and environment variables are operator-controlled and unaffected
- security(bundle): module resolution is contained to the bundle root (`bundle_split`) / `cwd` (`bundle`) — a `../..` import chain or a symlinked package in `node_modules` that escapes the tree now fails the build instead of folding arbitrary local files into the published bundle. A workspace `node_modules/<pkg>` link pointing outside the project must be brought inside (or the tree bundled from a common root) — the build names the module it refused
- security(icons): source PNGs decode with strict dimension limits (4096×4096) on top of the `image` crate's 512 MiB allocation cap — a crafted icon source declaring enormous dimensions is refused at the header instead of exhausting memory
- security(build): paths and messages emitted into `cargo:` directives are kept free of control characters — a walked filename containing a line break could previously inject arbitrary directives (`cargo:rustc-link-lib=…`) into a build script's output. Such paths are skipped with a plain stderr note; warnings take the stderr path
- security(dev): a compile failure answers 500 with a generic body — the detail, which can embed absolute local paths (the SCSS sandbox's refusal notes name them), goes to the developer's console only, so a client that can reach the dev server (e.g. a DNS-rebinding page) learns nothing about the local layout
- security(build): the oxc transform runtime is vendored at an exact pinned version (`0.138.0`, tracking the oxc toolchain) instead of a floating `^0.137` range that resolved the newest published package at build time — a decorator in a source file no longer picks up whatever the registry newest-serves

## [0.5.1] - 2026-07-06

### Added

- feat(build,serve): `npm://` symlink assets — a source symlink whose target is an `npm://<package>/<subpath>` URL is resolved from `node_modules` (exports-aware, via `npm-utils`) and emitted at the link's own path by `build` / served by `dev`, so a project sources specific files from an installed package (e.g. bootstrap-icons SVGs) without committing copies — a single file, or a whole directory with a trailing slash. Resolution is confined to the package's canonical directory, so an in-package symlink that escapes the module is refused
- CI: a `cargo audit` job scans the locked tree for RustSec advisories — on manifest/lock changes and weekly

### Changed

- The standalone tree helpers (`static_files::copy_static`, `compress::gzip_dir`, `typescript::compile_directory`, `scss::compile_directory`) skip symlinks entirely instead of reading through file links — `SymlinkMode` decisions live in the pipeline, `dev`, and the router
- oxc 0.138 and quick-xml 0.41 — quick-xml 0.40 carried RUSTSEC-2026-0194/-0195 (quadratic attribute-name checks); the dependency lock refreshed alongside

### Fixed

- fix(serve): filesystem reads and on-the-fly compiles run on tokio's blocking pool — concurrent requests no longer queue behind one slow read or compile
- fix(dev): a response that fails to build is a `500`, not a panic
- fix(build,dev): reject-list drops are warned on stderr (`build` per file, `dev` at startup) instead of requiring the `tracing` feature and a subscriber

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
