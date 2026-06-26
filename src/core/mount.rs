//! Compose a frontend from many prefix-mounted source directories.
//!
//! A [`Mount`] ties three things together for one source tree: the import
//! **specifier** authors write (`@module/contacts/`), the **url** it is served at
//! (`/modules/contacts/`), and the **dir** it lives in (`modules/contacts/web/src`).
//! They are independent in general, so a host that composes many crate-provided
//! `web/` trees can serve each under its own prefix and resolve cross-tree imports.
//!
//! The crate stays agnostic about *how* the mount set is assembled (a caller may
//! discover it from a directory scan, a Cargo dependency graph, or hard-code it),
//! and uses the one set to drive serving ([`dev`](crate::dev)), the runtime import
//! map ([`Importmap::from_mounts`](crate::importmap::Importmap::from_mounts)), and the
//! editor's TypeScript resolution ([`tsconfig`](crate::tsconfig)): one source of
//! truth, so the three never drift.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// A source directory mounted into the composed app.
///
/// ```
/// use std::path::Path;
/// use web_modules::Mount;
///
/// // Simple: prefix ties specifier + url together.
/// let ui = Mount::new("ui", Path::new("ui/src"));            // ui/  -> /ui/  from ui/src
/// // Decoupled: specifier, url, and dir all differ.
/// let contacts = Mount::new("contacts", Path::new("modules/contacts/web/src"))
///     .specifier("@module/contacts/")
///     .url("/modules/contacts/");
/// # let _ = (ui, contacts);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    specifier: String,
    url: String,
    dir: PathBuf,
    watch: bool,
}

impl Mount {
    /// Mount `dir` under `prefix`: import specifier `"<prefix>/"`, served at
    /// `"/<prefix>/"`. Surrounding slashes in `prefix` are ignored.
    pub fn new(prefix: impl AsRef<str>, dir: impl Into<PathBuf>) -> Self {
        let p = prefix.as_ref().trim_matches('/');
        Self {
            specifier: format!("{p}/"),
            url: format!("/{p}/"),
            dir: dir.into(),
            watch: true,
        }
    }

    /// Mount `dir` at the site root (`/`) with **no** import specifier, for the
    /// shell whose files are referenced by absolute URL, not a bare specifier.
    pub fn root(dir: impl Into<PathBuf>) -> Self {
        Self {
            specifier: String::new(),
            url: "/".to_string(),
            dir: dir.into(),
            watch: true,
        }
    }

    /// Build a mount from a directory that may carry a `package.json`: the name
    /// (specifier/url segment) is the manifest's `name` if present, else the dir's
    /// basename; the served root is `<dir>/<web_modules.root>` when the manifest
    /// declares that field, else `dir` itself. Chain [`specifier`](Self::specifier) /
    /// [`url`](Self::url) to override the name; a caller-given name wins (npm's
    /// `file:`/alias rule: **given ＞ package.json `name` ＞ dir basename**).
    pub fn from_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let (name, root) = match std::fs::read(dir.join("package.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        {
            Some(json) => {
                let name = json.get("name").and_then(Value::as_str).map(str::to_string);
                let root = json
                    .get("web_modules")
                    .and_then(|v| v.get("root"))
                    .and_then(Value::as_str)
                    .map(|r| dir.join(r.trim_start_matches("./")));
                (name, root)
            }
            None => (None, None),
        };
        let name = name.unwrap_or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string()
        });
        let served = root.unwrap_or_else(|| dir.clone());
        Mount::new(&name, served)
    }

    /// Override the import specifier (e.g. `"@module/contacts/"`). Empty = none.
    pub fn specifier(mut self, specifier: impl Into<String>) -> Self {
        self.specifier = specifier.into();
        self
    }

    /// Override the served URL prefix (e.g. `"/modules/contacts/"`).
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Set whether this mount is watched for live-reload (default `true`).
    pub fn watched(mut self, watch: bool) -> Self {
        self.watch = watch;
        self
    }

    /// The source directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The served URL prefix (always starts with `/`; `"/"` for a root mount).
    pub fn url_prefix(&self) -> &str {
        &self.url
    }

    /// The import specifier (empty for a [root](Mount::root) mount).
    pub fn specifier_prefix(&self) -> &str {
        &self.specifier
    }

    /// Whether this mount is watched for live-reload.
    pub fn is_watched(&self) -> bool {
        self.watch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_derives_specifier_and_url_from_prefix() {
        let m = Mount::new("/ui/", "ui/src");
        assert_eq!(m.specifier_prefix(), "ui/");
        assert_eq!(m.url_prefix(), "/ui/");
        assert_eq!(m.dir(), Path::new("ui/src"));
        assert!(m.is_watched());
    }

    #[test]
    fn decoupled_specifier_url_dir() {
        let m = Mount::new("contacts", "modules/contacts/web/src")
            .specifier("@module/contacts/")
            .url("/modules/contacts/")
            .watched(false);
        assert_eq!(m.specifier_prefix(), "@module/contacts/");
        assert_eq!(m.url_prefix(), "/modules/contacts/");
        assert_eq!(m.dir(), Path::new("modules/contacts/web/src"));
        assert!(!m.is_watched());
    }

    #[test]
    fn root_mount_has_no_specifier() {
        let m = Mount::root("packages/frontend/web/src");
        assert_eq!(m.specifier_prefix(), "");
        assert_eq!(m.url_prefix(), "/");
    }

    #[test]
    fn from_dir_uses_package_json_name_and_root() {
        let tmp = tempfile::tempdir().unwrap();
        let comp = tmp.path().join("widgets");
        std::fs::create_dir_all(comp.join("src")).unwrap();
        std::fs::write(
            comp.join("package.json"),
            r#"{"name":"@acme/widgets","web_modules":{"root":"./src"}}"#,
        )
        .unwrap();
        let m = Mount::from_dir(&comp);
        assert_eq!(m.specifier_prefix(), "@acme/widgets/");
        assert_eq!(m.url_prefix(), "/@acme/widgets/");
        assert_eq!(m.dir(), comp.join("src"));
    }

    #[test]
    fn from_dir_falls_back_to_basename_and_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let comp = tmp.path().join("plain");
        std::fs::create_dir_all(&comp).unwrap(); // no package.json
        let m = Mount::from_dir(&comp);
        assert_eq!(m.specifier_prefix(), "plain/");
        assert_eq!(m.dir(), comp);
    }

    #[test]
    fn given_name_overrides_package_json_name() {
        let tmp = tempfile::tempdir().unwrap();
        let comp = tmp.path().join("widgets");
        std::fs::create_dir_all(&comp).unwrap();
        std::fs::write(comp.join("package.json"), r#"{"name":"@acme/widgets"}"#).unwrap();
        // A caller-given name wins (npm's file:/alias rule).
        let m = Mount::from_dir(&comp)
            .specifier("counter/")
            .url("/counter/");
        assert_eq!(m.specifier_prefix(), "counter/");
        assert_eq!(m.url_prefix(), "/counter/");
    }
}
