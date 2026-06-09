//! Generate TypeScript `tsconfig` `paths` from a set of [`Mount`]s — the editor /
//! `tsc` side of import resolution, co-generated from the **same** mount set as the
//! runtime [`Importmap`](crate::importmap::Importmap::from_mounts) so the two can't
//! drift.
//!
//! For a mount with specifier `@module/contacts/` and dir `modules/contacts/web/src`,
//! this emits `"@module/contacts/*": ["./modules/contacts/web/src/*"]` (dir relative
//! to `base`, typically the workspace root). Callers compose these with any
//! hand-authored entries (single-file aliases, third-party `node_modules` paths).

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::mount::Mount;
use crate::{Error, Result};

/// Build the `compilerOptions.paths` object resolving each mount's specifier to its
/// source dir (relative to `base`). Mounts without a specifier (root mounts) are
/// skipped. Keys/values are sorted for byte-stable output.
pub fn tsconfig_paths(mounts: &[Mount], base: &Path) -> Value {
    let mut paths = Map::new();
    for m in mounts {
        let spec = m.specifier_prefix();
        if spec.is_empty() {
            continue;
        }
        // `@module/x/` → `@module/x/*` ; `lib/` → `lib/*`.
        let key = format!("{}*", spec);
        let target = format!("{}/*", relative_dir(base, m.dir()));
        paths.insert(key, json!([target]));
    }
    Value::Object(paths)
}

/// Write a base `tsconfig.json` whose `compilerOptions.paths` is
/// [`tsconfig_paths`], creating parent directories. A starting point for a host
/// that has no other compiler options to merge.
pub fn write_tsconfig_base(mounts: &[Mount], base: &Path, path: &Path) -> Result<()> {
    let doc = json!({
        "compilerOptions": {
            "moduleResolution": "bundler",
            "paths": tsconfig_paths(mounts, base),
        }
    });
    let json = serde_json::to_string_pretty(&doc).map_err(|e| Error::Compose(e.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}

/// `dir` relative to `base` as a `./`-prefixed, forward-slash path. Falls back to
/// the dir verbatim when it isn't under `base`.
fn relative_dir(base: &Path, dir: &Path) -> String {
    let rel = dir.strip_prefix(base).unwrap_or(dir);
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        ".".to_string()
    } else if rel.is_absolute() {
        s
    } else {
        format!("./{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_from_mounts_are_specifier_to_relative_dir() {
        let base = Path::new("/work");
        let mounts = [
            Mount::new("contacts", "/work/modules/contacts/web/src")
                .specifier("@module/contacts/")
                .url("/modules/contacts/"),
            Mount::new("lib", "/work/packages/frontend/web/src/lib"),
            // root mount contributes nothing
            Mount::root("/work/packages/frontend/web/src"),
        ];
        let paths = tsconfig_paths(&mounts, base);
        let obj = paths.as_object().unwrap();
        assert_eq!(
            obj["@module/contacts/*"],
            json!(["./modules/contacts/web/src/*"])
        );
        assert_eq!(obj["lib/*"], json!(["./packages/frontend/web/src/lib/*"]));
        assert_eq!(obj.len(), 2, "root mount has no specifier → no path entry");
    }

    #[test]
    fn write_base_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let mounts = [Mount::new("ui", base.join("ui/src"))];
        let out = base.join("tsconfig.base.json");
        write_tsconfig_base(&mounts, base, &out).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(
            written["compilerOptions"]["paths"]["ui/*"],
            json!(["./ui/src/*"])
        );
    }
}
