# react-umd example

**Classic React that just works in a browser** — React's prebuilt UMD build, loaded as a
plain `<script>` global. No bundler, no import map, no transform of React itself.

```sh
cargo run -p react-umd
# open http://127.0.0.1:8081/
```

This is the buildless counterpart to [`react-esm`](../react-esm), which has to bundle React
19's CommonJS into ESM with rolldown. React 18 still ships a UMD build that runs in a browser
as-is, so here there's nothing to bundle.

## Two things it showcases

- **Single-asset extraction.** React's npm package is large (CommonJS sources, several
  builds), but a browser needs exactly one file. `main.rs` vendors with
  `Extract::File { from: "umd/react.production.min.js", to: "react.js" }` — pulling **just
  that one asset** out of the package (and the same for react-dom) instead of the whole tree.
  After vendoring, `web/web_modules/` holds only `react/react.js` (~11 KB) and
  `react-dom/react-dom.js` (~130 KB) — nothing else.
- **Classic UMD, loaded as a global.** `web/index.html` loads those two files with classic
  `<script>` tags (react before react-dom), exposing `window.React` / `window.ReactDOM`.
  `web/app.ts` uses those globals via `React.createElement` (no JSX) and is transformed to JS
  on the fly by web-modules (oxc) — so the source is type-checked TS, but nothing is bundled.

## How it works

- **`main.rs`** vendors the two single files into `web/web_modules/` with `Extract::File`,
  then serves the `web/` tree with `web_modules::dev::serve` (compiling `app.ts` per request).
- **`web/app.ts`** types the UMD globals with `declare const React: typeof import("react")`
  (a type-only query, erased at compile time — nothing is imported at runtime) and renders a
  `useState` counter with `React.createElement`.

## Testing (Node, test-tooling only — not needed to build or run)

```sh
cd examples/react-umd
npm ci
npm run test:types   # tsc --noEmit (types the UMD globals via @types/react)
npm run test:e2e     # Playwright: the UMD globals load, the counter increments, no errors
```
