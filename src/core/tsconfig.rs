//! Generate TypeScript `tsconfig` `paths` from a set of [`Mount`]s — the editor /
//! `tsc` side of import resolution, co-generated from the **same** mount set as the
//! runtime [`Importmap`](crate::importmap::Importmap::from_mounts) so the two can't
//! drift.
//!
//! For a mount with specifier `@module/contacts/` and dir `modules/contacts/web/src`,
//! this emits `"@module/contacts/*": ["./modules/contacts/web/src/*"]` (dir relative
//! to `base`, typically the workspace root). Pair with [`tsconfig_node_modules_paths`]
//! for the third-party `node_modules` paths (derived from a `package.json`) and merge
//! both into one `compilerOptions.paths`.

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

/// Build the `compilerOptions.paths` object resolving each third-party npm package
/// declared in `package_json` to its `./node_modules/<pkg>` location (plus a `<pkg>/*`
/// subpath glob). The package set is read via
/// [`specs_from_package_json`](crate::vendor::specs_from_package_json), so it honors the
/// `web-modules.webDependencies` whitelist and skips local (`file:`/`workspace:`) deps —
/// the editor then resolves exactly the packages the build vendors. Compose the result
/// with [`tsconfig_paths`] (first-party mounts) into one `paths` map.
///
/// `node_modules` is assumed to sit beside the `tsconfig.json` (the usual layout), so the
/// emitted values are `./node_modules/<pkg>`. Keys are sorted for byte-stable output.
pub fn tsconfig_node_modules_paths(package_json: &Path) -> Result<Value> {
    let specs = crate::vendor::specs_from_package_json(package_json)?;
    let mut paths = Map::new();
    for spec in &specs {
        let name = spec.name();
        paths.insert(name.to_string(), json!([format!("./node_modules/{name}")]));
        paths.insert(
            format!("{name}/*"),
            json!([format!("./node_modules/{name}/*")]),
        );
    }
    Ok(Value::Object(paths))
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

    #[test]
    fn node_modules_paths_from_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("package.json");
        std::fs::write(
            &pkg,
            r#"{ "dependencies": { "lit": "^3", "@lit/context": "1.1.6", "jose": "6.2.3" } }"#,
        )
        .unwrap();
        let paths = tsconfig_node_modules_paths(&pkg).unwrap();
        let obj = paths.as_object().unwrap();
        assert_eq!(obj["lit"], json!(["./node_modules/lit"]));
        assert_eq!(obj["lit/*"], json!(["./node_modules/lit/*"]));
        // Scoped packages are emitted verbatim.
        assert_eq!(obj["@lit/context"], json!(["./node_modules/@lit/context"]));
        assert_eq!(
            obj["@lit/context/*"],
            json!(["./node_modules/@lit/context/*"])
        );
        assert_eq!(obj["jose"], json!(["./node_modules/jose"]));
        assert_eq!(obj.len(), 6, "3 packages × (bare + /*)");
    }

    #[test]
    fn node_modules_paths_honor_webdependencies_whitelist() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("package.json");
        // `pg` is server-only; the webDependencies whitelist keeps it out of the editor set.
        std::fs::write(
            &pkg,
            r#"{ "dependencies": { "lit": "^3", "pg": "^8" },
                "web-modules": { "webDependencies": ["lit"] } }"#,
        )
        .unwrap();
        let paths = tsconfig_node_modules_paths(&pkg).unwrap();
        let obj = paths.as_object().unwrap();
        assert!(obj.contains_key("lit"));
        assert!(
            !obj.contains_key("pg"),
            "pg is not in webDependencies → no tsconfig path"
        );
        assert_eq!(obj.len(), 2, "only lit (bare + /*)");
    }
}
