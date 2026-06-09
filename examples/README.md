# Examples

Runnable demos of the [`web-modules`](..) toolchain — each is a small app you can
`cargo run` and open in the browser. They double as integration tests (driven by
[`tests/examples.rs`](../tests/examples.rs)).

| Example | Demonstrates |
|---------|--------------|
| [`lit-element`](lit-element) | a Lit 3 component styled with Bootstrap — TypeScript + static reactive properties, with a Web Components polyfill fallback |
| [`d3`](d3) | a D3 chart — a non-Lit, UMD-global dependency (loaded as a `<script>`, no import-map entry) |
| [`bootstrap`](bootstrap) | theme Bootstrap from its SCSS sources |
| [`compose`](compose) | compose sibling component dirs (declared as `file:` path-deps in `package.json`) into one app — a single `Mount` set drives both the import map *and* the tsconfig |
| [`embedded`](embedded) | bake the whole site (minified JS + compressed CSS + `.gz` sidecars) into the binary and serve it statically, no filesystem access |
| [`tauri`](tauri) | drive the live dev server from `cargo tauri dev` — a Tauri v2 desktop window whose frontend web-modules compiles + live-reloads (and builds for release) |
| [`react-umd`](react-umd) | classic React, buildless — `Extract::File` vendors React 18's prebuilt **UMD** build, loaded as a `<script>` global; the app is `React.createElement` (no JSX), transformed on the fly |
| [`react-esm`](react-esm) | React 19 from npm (**CommonJS**) folded into one browser ES module via the opt-in `bundle` feature (rolldown); a zustand store proves a single shared React instance |

## Running

Workspace examples — `cargo run -p <crate>`:

```sh
cargo run -p lit-element     # also: d3 · bootstrap-scss · compose · embedded · react-umd · react-esm
```

Then open the printed URL (default `http://127.0.0.1:8080/`). The live examples
recompile a `.ts`/`.scss` on reload; `embedded` instead serves assets baked into the
binary. (Note the `bootstrap` directory's crate is `bootstrap-scss`.)

`tauri` is **standalone** — excluded from the workspace (its `src-tauri` crate is its own
workspace root and pulls in webkit2gtk), and driven from its own manifest via `cargo tauri
dev`. See its README for the exact command.
