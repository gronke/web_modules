# embedded example

Bakes the whole frontend **into the binary** at build time and serves it statically —
no filesystem access, no network, no Node.

```sh
cargo run -p embedded
# open http://127.0.0.1:8080/
```

`build.rs` runs `web_modules::build` with the whole output-policy set.
TypeScript compiles to **minified** JS with a linked **source map** (`Processors::sourcemap`); the sources ship inside the map, so even the embedded dist stays debuggable.
The legal banner is **collected** into an `app.js.LEGAL.txt` sidecar (`Output::optimized().comments(Comments::Collect)`), a pointer comment left in its place.
SCSS becomes **compressed** CSS, and every servable asset gets a `.gz` sidecar.
`main.rs` embeds the result (`$OUT_DIR/dist`) with `include_dir!` and serves it from memory.
Unlike the other examples it vendors **nothing**; the point here is the *output* pipeline, so the build runs entirely offline (and `minify.webModules` has nothing to act on).
