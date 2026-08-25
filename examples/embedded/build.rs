//! Bakes the frontend at build time into `$OUT_DIR/dist`, **optimized for shipping
//! inside the binary**: TypeScript → minified JS with a linked `.map` (sources embedded),
//! the legal banner collected into `app.js.LEGAL.txt`, SCSS → compressed CSS, plus a
//! `.gz` sidecar for every servable asset. `main.rs` embeds the result with `include_dir!`.
//!
//! Unlike the other examples this one vendors **nothing** (no npm dependencies), so the
//! bake runs entirely offline — the point here is the *output* pipeline (minify + gzip +
//! embed), not vendoring.

use std::path::PathBuf;

use web_modules::build::{build, BuildOptions, Output, Processors};
use web_modules::Comments;

const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>web-modules · embedded</title>
<link rel="stylesheet" href="/styles.css">
{importmap}
<script type="module" src="/app.js"></script>
</head>
<body>
<h1>Baked into the binary</h1>
<click-counter></click-counter>
</body>
</html>
"#;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dist");
    let web = manifest.join("web");

    // Ship the source map beside the minified app.js: an embedded dist stays debuggable.
    let mut processors = Processors::default();
    processors.sourcemap = true;

    build(&BuildOptions {
        specs: &[], // no npm dependencies → the build never touches the network
        roots: std::slice::from_ref(&web),
        out: &out,
        mount: "/web_modules",
        html: HTML,
        template: None,
        processors,
        // The whole point of this example, the output-policy set: minify the emitted
        // JS, write `.gz` sidecars, and collect the legal banner into `app.js.LEGAL.txt`,
        // so what ends up embedded is production-sized yet license-compliant.
        output: Output::optimized().comments(Comments::Collect),
    })
    .expect("build embedded frontend");
}
