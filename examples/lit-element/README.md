# lit-element example

A Lit 3 component styled with Bootstrap 5 — vendored, transformed and **baked at
build time** by web-modules (see `build.rs`), then served by axum. No Node, no bundler.

```sh
cargo run -p lit-element            # dev: live-reload (edit web/app.ts and reload)
cargo run -p lit-element --release  # serves the frontend embedded in the binary
# open http://127.0.0.1:8080/
```

How it works:

- **`build.rs`** calls `web_modules::build` to vendor the npm deps, transform
  `web/app.ts` and compile `web/styles.scss`, and render `index.html` (with the
  import map) into `$OUT_DIR/dist`.
- **`main.rs`** embeds that tree with `include_dir!` and serves it via
  `Frontend::new(&DIST).source("web").auto()` — **live-reload** in debug (recompiles
  `web/*.ts`/`*.scss` on the fly, baked assets as fallback), **embedded** in `--release`.
- `web/app.ts` — a Lit component (TypeScript, static reactive `properties`,
  decorator-free, so the output needs no runtime helpers). Uses Bootstrap's `Tooltip`
  (Bootstrap JS + Popper, via the import map).
- `@webcomponents/webcomponentsjs` is loaded as a classic-script polyfill fallback.
