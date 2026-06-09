//! Output optimization, bound as specs: **minify** as a compile output, and **gzip**
//! `.gz` sidecars. Offline; needs the `minify` + `compress` features (on under
//! `--all-features`). The decode round-trip of a sidecar is covered by `compress`'s own
//! unit tests; here we pin the public behavior — smaller, single-line JS, and sidecars
//! written for the right extensions.
#![cfg(all(feature = "minify", feature = "compress"))]

use std::path::Path;

use web_modules::compress::{gzip_dir, gzip_file};
use web_modules::minify::minify_str;
use web_modules::typescript::{compile_str, compile_str_with, TranspileOptions};

const TS: &str = r#"
export function greet(name: string): string {
  const greeting = "hello, " + name;
  return greeting.toUpperCase();
}
export const answer: number = 6 * 7;
"#;

#[test]
// `TranspileOptions` is `#[non_exhaustive]`, so the struct-literal form clippy suggests
// is unavailable to external crates — default-then-assign is the only option.
#[allow(clippy::field_reassign_with_default)]
fn minify_compile_output_is_smaller_and_single_line() {
    let path = Path::new("greet.ts");
    let normal = compile_str(TS, path).unwrap();
    // `TranspileOptions` is `#[non_exhaustive]` — default-construct, then set the field.
    let mut opts = TranspileOptions::default();
    opts.minify = true;
    let minified = compile_str_with(TS, path, &opts).unwrap();

    assert!(
        minified.len() < normal.len(),
        "minified ({}) should be smaller than normal ({})",
        minified.len(),
        normal.len()
    );
    // Whitespace collapsed to (at most) a trailing newline.
    assert!(
        minified.matches('\n').count() <= 1,
        "minified: {minified:?}"
    );
    // Still a real ES module — exported names can't be mangled away.
    assert!(minified.contains("export"));
    assert!(minified.contains("greet"));
}

#[test]
fn minify_str_shrinks_external_js() {
    // For JS the compiler didn't produce (vendored), `minify` is the byte-level pass.
    let src = "function add(a, b) {\n  const sum = a + b;\n  return sum;\n}\nexport { add };\n";
    let out = minify_str(src, Path::new("vendor.js")).unwrap();
    assert!(out.len() < src.len());
    assert!(out.matches('\n').count() <= 1);
}

#[test]
fn gzip_dir_writes_sidecars_only_for_listed_extensions() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // Compressible content so a sidecar is actually produced.
    std::fs::write(dir.join("app.js"), "console.log(1);\n".repeat(200)).unwrap();
    std::fs::write(dir.join("style.css"), "body { color: red }\n".repeat(200)).unwrap();
    std::fs::write(dir.join("data.bin"), vec![0u8; 4096]).unwrap();

    let written = gzip_dir(dir, &["js", "css"]).unwrap();
    assert_eq!(written, 2, "only .js and .css are in the filter");
    assert!(dir.join("app.js.gz").exists());
    assert!(dir.join("style.css.gz").exists());
    assert!(!dir.join("data.bin.gz").exists());

    // `gzip_file` handles a single asset of any kind.
    gzip_file(&dir.join("data.bin")).unwrap();
    assert!(dir.join("data.bin.gz").exists());
}
