# d3 example

Renders a bar chart with [D3](https://d3js.org) — a **non-Lit** dependency —
vendored from npm and served by `web-modules`.

```sh
cargo run -p d3
# open http://127.0.0.1:8080/
```

It shows the toolchain isn't Lit-specific: D3 ships a UMD bundle, loaded as a
classic global `<script>` (no import-map entry), while `web/chart.ts` is
transformed to JS on the fly. `web/web_modules/` is generated.
