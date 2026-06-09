# react-esm example

**React, from npm, as a single browser ES module** — installed and bundled entirely in
Rust. No Node, no CDN, no bundler at runtime.

```sh
cargo run --manifest-path examples/react-esm/Cargo.toml
# open http://127.0.0.1:8080/
```

## Why this example needs a bundler (and the others don't)

web-modules' normal path is *buildless*: it vendors a package's **browser ES modules**
into `web_modules/` and lets the browser's import map resolve them (see the `lit-element`
and `d3` examples). React can't be used that way — `react` and `react-dom` ship **CommonJS
only**: `react`'s package entry is `module.exports = … require("./cjs/react.production.js")`,
which references `module`/`require`/`process`, none of which exist in a browser. (React 19
also dropped the old UMD builds.) So React has to be **bundled** into real ESM first.

That's what the opt-in **`bundle`** feature does, using [rolldown] — the embedded,
oxc-based Rust bundler. Still pure Rust, still no Node. The buildless `react-umd` example
next door shows the *other* answer: load React's UMD build as a global, no bundler at all.

## How it works (all in `build.rs`, pure Rust)

1. **install** — `web_modules::npm_utils::install::node_modules` resolves and installs
   `react`, `react-dom` and `zustand` (the transitive tree, CommonJS and all) into
   `web/node_modules/`. This is a real "npm install", implemented in Rust.
2. **bundle** — `web_modules::bundle::bundle` runs rolldown over `web/app.tsx` + that
   `node_modules/` tree, producing one self-contained browser ES module
   (`$OUT_DIR/dist/app.js`): CommonJS→ESM, JSX/TS transformed, `process.env.NODE_ENV`
   folded to `"production"` (so React takes its production path and dev branches are
   dead-code-eliminated), React inlined exactly once, minified (~190 KB).
3. **embed + serve** — `main.rs` embeds `$OUT_DIR/dist` with `include_dir!` and serves it
   via `Frontend::embedded(&DIST).router()`. The shipped binary has **no rolldown** linked
   in — the app is already bundled.

## What it proves: a single, shared React instance

The counter's state lives in a **zustand store**. zustand is a *separate* dependency that
itself imports React (it calls `useSyncExternalStore`); the component imports React too. For
the hooks in both to work, the bundle must contain **exactly one** React instance, shared by
the app and by zustand — a duplicate would throw *"invalid hook call"* the moment the two
hit different React dispatchers. rolldown deduplicates React to a single copy, so the app
just works. That diamond (`app → react`, `app → zustand → react`, resolved to one react) is
the point of the example, and the Playwright test is the live proof.

## Testing (Node, test-tooling only — not needed to build or run)

```sh
cd examples/react-esm
npm ci
npm run test:types   # tsc --noEmit (oxc/rolldown strip types without checking them)
npm run test:e2e     # Playwright: counter increments via the store, zero console errors
```

The e2e watches for console errors and failed requests while it clicks the counter: no
errors ⇒ a single React instance. See `tests/counter.spec.ts`.

[rolldown]: https://rolldown.rs
