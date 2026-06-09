# compose example

Builds **one app out of several sibling examples**, referenced by their `web/`
directories. It reuses the [`lit-element`](../lit-element) counter and the
[`d3`](../d3) chart and wires them together: each click logs its timestamp and the
chart redraws the **press distribution over time** — Bootstrap-themed.

```sh
cargo run -p compose
# open http://127.0.0.1:8080/ and click the counter
```

## What it demonstrates

This is the composition path end to end — the part schuhkarton leans on, exercised
generically:

- **Path-deps → mounts.** `web/package.json` lists the siblings as local path-deps
  (`"counter": "file:../../lit-element/web"`, `"chart": "file:../../d3/web"`).
  `web_modules::vendor::read_package_json` routes those to
  [`Mount`](https://docs.rs/web-modules)s named by the dependency **key** (npm's `file:`
  rule), and registry deps (`lit`) to vendoring specs — from one read.
- **Import a component by name, across mounts.** `web/app.ts` does
  `import 'counter/counter.js'` and `import { renderChart } from 'chart/chart.js'`;
  the bare specifiers resolve through the co-generated import map. Neither sibling
  knows this app exists.
- **One mount set, co-generated maps.** The runtime **import map**
  (`Importmap::from_mounts`) and the editor **`tsconfig.json`** (`write_tsconfig_base`)
  are generated from the *same* mounts, so they can't drift.
- **Multi-prefix live serving + source-hiding.** The dev server serves `/counter/…`,
  `/chart/…` and the app's own `/` together, compiling `.ts`/`.scss` on the fly; the
  `.ts`/`.scss` sources are never served raw — only the compiled `.js`/`.css`.
- **SCSS resolved across the vendored tree.** `web/app.scss` themes and builds Bootstrap
  from its vendored `.scss` with grass — no Node.

## Vendored vs. mounted

Path-deps mount the sibling **source components** (`counter.ts`, `chart.ts`). Third-party
**runtime** deps — `lit` (from `package.json`), plus `d3` (UMD global) and `bootstrap`
(SCSS) added programmatically in [`src/main.rs`](src/main.rs) — are vended into *this*
app's `web/web_modules/`. A composing app owns its vendored assets: sibling `web_modules/`
are vended on demand and gitignored, so they can't be relied on across a fresh build.

`web/web_modules/`, `web/importmap.json`, `web/index.html` and `tsconfig.json` are all
generated at startup and gitignored.
