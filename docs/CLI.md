# web-modules CLI reference

Every `build` flag and its `package.json` `web_modules` key.
Overview and install: [README.md](../README.md).
The behavior policies (HTML, duplicate outputs, the output directory, symlinks): [POLICIES.md](POLICIES.md).

## build

`build` is the static counterpart of `dev`: the same source roots and processors, emitted to `--out` instead of served.
It vendors npm only when you pass `--package` / `--manifest`; `vendor` just fetches dependencies into `web_modules/`.
Each compiler processor (typescript, scss, tera, minify, sourcemap, gzip, dts) has a `--<name>` / `--no-<name>` toggle, and `--no-default-features` turns the default-on set (typescript, scss, tera) off so you re-enable them individually.
Run `web-modules <command> --help` for any subcommand's flags.

## `--minify`

Covers the whole dist tree: compiled TypeScript, copied `.js` / `.mjs`, Tera-rendered JS, `npm://` assets, and the vendored `web_modules/` (opt the npm content out with `--no-minify-web-modules` or `"minify": {"webModules": false}`).
Every file is rewritten through one oxc parse→codegen pass.
CSS needs no toggle, since grass always emits compressed.

## `--comments <keep|strip|collect|none>`

Sets the comment policy for emitted JS (`"comments": "strip"`).
Unset, `--minify` implies `strip`.

- `strip` drops normal, JSDoc and annotation comments but keeps legal comments (`//!`, `/*!`, `@license`, `@preserve`) inline, so license text always ships.
- `collect` moves them into a `<output>.LEGAL.txt` sidecar beside each file: verbatim, deduplicated, blank-line separated, with a pointer comment left in the code.
  The format is stable, so compliance tooling may rely on it.
- `none` drops everything, for tiny embedded targets.
  The vendored `LICENSE` / `NOTICE` files still ship.

CSS needs no policy: grass's compressed output already keeps only `/*!` loud comments.
A legal comment above a type-only declaration that erases (a leading `interface` or `type`) is preserved, where `tsc` drops it with the declaration, so a module's license header survives even when its first statement is types.

## `--bundle`

Requires the opt-in `bundle` feature; the released binary carries it.
It folds the built tree per entry point: each entry (`app.js` without `--bundle-entry`) keeps its exact URL with its imports inlined, shared and dynamically-imported code lands in content-hashed `chunks/`, and `importmap.json` + `web_modules/` drop out of the output, so your HTML keeps working unchanged.
Minify, comments and sourcemap apply through rolldown's single pass (its maps reference the staged compiled modules, and `collect` degrades to inline legal comments in bundled files).
The graph must be analyzable from the entries: a worker script or a second page's module needs its own `--bundle-entry`, and the build fails naming any survivor whose bare imports lost the import map.
A source `.tera` page still renders with the real map (before bundling); the inline map it embeds goes unused once every import is inlined.
A bundled build re-vendors from the network each time, since the vendored tree is consumed and there is nothing to reuse as a cache.

## `--sourcemap`

Off by default, so an embedded dist stays lean.
It emits a source map for every compiled TypeScript file, with the sources embedded (`sourcesContent`) since `.ts` files never ship: `build` writes a `<file>.map` sidecar linked by file name, `dev` serves the map inline as a `data:` URL.
Vendored packages' own shipped `.map` files follow the same toggle, and flipping it re-vendors instead of reusing the differently-shaped cache.
SCSS is not covered, since grass emits no source maps.

## Dependencies

A dependency may be a registry range, an https `.tgz`, or a git reference (`github:owner/repo#ref`); name it under `web_modules.sourceDependencies` and its TypeScript is compiled into the layout its own `tsconfig.json` declares.
Pin a git dependency to a commit rather than a branch: a commit is cached by name and costs no network once vendored, while a branch is re-downloaded every run so that moving it is noticed.

## Library builds

Three flags turn `build` into a library compiler that produces an npm package rather than a deployable site, so a TypeScript element library can build with no Node toolchain.

- `--external <spec>` (repeatable, or a `web_modules.external` array) marks a bare import as intentionally unresolved: a library with a peer dependency emits `import ... from "lit"` without vendoring it, which keeps that specifier from failing the unresolved-import check while the emitted code stays bare.
  A bare name covers its subpaths, so `--external lit` also allows `lit/decorators.js`.
- `--dts` / `--no-dts` (off by default, or `"dts": true`) emits a `.d.ts` beside each compiled module via oxc's `isolatedDeclarations`.
  Declarations are produced per file with no type-checking, so every module boundary must carry an explicit type; a source that omits one fails the build.
  That is the whole cost of shipping typings without `tsc --emitDeclarationOnly`.
- `--library` (alias `--no-page`, or `"library": true` / `"noPage": true`) skips the page scaffolding (the synthesized fallback `index.html` and the standalone `importmap.json`) that a library build would only delete.
  A source-provided `index.html` is still emitted, and the `.web-modules-out` marker is kept, so a rebuild into the same directory needs no `rm -rf`.
