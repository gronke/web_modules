# embedded example

Bakes the whole frontend **into the binary** at build time and serves it statically —
no filesystem access, no network, no Node.

```sh
cargo run -p embedded
# open http://127.0.0.1:8080/
```

`build.rs` runs `web_modules::build` with `Output::optimized()`: TypeScript →
**minified** JS, SCSS → **compressed** CSS, plus a `.gz` sidecar for every servable
asset. `main.rs` embeds the result (`$OUT_DIR/dist`) with `include_dir!` and serves it
from memory. Unlike the other examples it vendors **nothing** — the point here is the
*output* pipeline (minify + gzip + embed), so the build runs entirely offline.
