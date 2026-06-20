//! Pre-compress emitted assets to `.gz` sidecars (gzip) so a static server can serve
//! them with `Content-Encoding: gzip`.
//!
//! Byte-level and **universal**: the compression half of the crate's output
//! optimization (the per-type half is [minification](crate::minify)). Pair with
//! [`server`](crate::server)'s static router, which prefers a `<file>.gz` sidecar when
//! the client sends `Accept-Encoding: gzip`.

use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;
use walkdir::WalkDir;

use crate::Result;

/// Write `<path>.gz` next to `path` (gzip, best compression). Returns the compressed
/// byte length.
pub fn gzip_file(path: &Path) -> Result<u64> {
    let bytes = std::fs::read(path)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&bytes)?;
    let compressed = encoder.finish()?;
    std::fs::write(append_gz(path), &compressed)?;
    Ok(compressed.len() as u64)
}

/// Gzip every file under `dir` whose extension is in `exts` (e.g.
/// `&["js", "css", "html", "json", "svg"]`), writing `<file>.gz` sidecars. Already
/// `.gz` files are skipped. Returns the number of sidecars written.
pub fn gzip_dir(dir: &Path, exts: &[&str]) -> Result<usize> {
    let mut count = 0;
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext == "gz" || !exts.contains(&ext) {
            continue;
        }
        gzip_file(path)?;
        count += 1;
    }
    Ok(count)
}

/// `<path>` with `.gz` appended (keeping the original extension, e.g.
/// `app.js` → `app.js.gz`).
fn append_gz(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".gz");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn gzip_file_writes_a_decodable_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("app.js");
        std::fs::write(&f, b"export const x = 1;\n").unwrap();
        gzip_file(&f).unwrap();
        let gz = dir.path().join("app.js.gz");
        assert!(gz.exists());
        let bytes = std::fs::read(&gz).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
        let mut out = String::new();
        decoder.read_to_string(&mut out).unwrap();
        assert_eq!(out, "export const x = 1;\n");
    }

    #[test]
    fn gzip_dir_filters_by_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.js"), b"x").unwrap();
        std::fs::write(dir.path().join("b.png"), b"x").unwrap();
        let n = gzip_dir(dir.path(), &["js"]).unwrap();
        assert_eq!(n, 1);
        assert!(dir.path().join("a.js.gz").exists());
        assert!(!dir.path().join("b.png.gz").exists());
    }
}
