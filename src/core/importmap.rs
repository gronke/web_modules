//! A small, deterministic [import map] composer.
//!
//! Keys are module specifiers (bare `"lit"` or prefix `"lit/"`), and values
//! are URLs the browser resolves them to. Backed by a [`BTreeMap`] so repeated
//! builds emit byte-identical output (stable diffs, cache-friendly).
//!
//! [import map]: https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/script/type/importmap
//!
//! ```
//! use web_modules::importmap::Importmap;
//! let mut map = Importmap::new();
//! map.insert("lit", "/web_modules/lit/index.js")
//!    .insert("lit/", "/web_modules/lit/");
//! assert!(map.to_json().contains("\"lit\""));
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use url::Url;

use crate::{Error, Result};

/// Base for the map's URLs, which carry none; a special scheme, so `\` splits like `/`.
static ADDRESS_BASE: LazyLock<Url> =
    LazyLock::new(|| Url::parse("http://importmap.invalid/").expect("a valid base URL"));

/// An ES module import map (`{ "imports": { … } }`).
///
/// This is the imports-only dialect the build emits and validates — not a WHATWG
/// import-map processor. `scopes`, base-URL resolution and multi-map merging are out
/// of scope, because the build only ever interprets the map it generated itself.
/// The struct is the wire shape: serde derives both directions of the
/// `{ "imports": { … } }` document, so parsing and printing cannot drift apart.
/// Unknown top-level keys (`scopes`, `integrity`, …) are ignored on read.
#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Importmap {
    imports: BTreeMap<String, String>,
}

impl Importmap {
    /// An empty import map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build an import map from a set of [`Mount`](crate::mount::Mount)s: each
    /// mount with a non-empty specifier contributes `<specifier>` → `<url>`. The
    /// runtime counterpart to [`tsconfig`](crate::tsconfig)'s editor `paths`,
    /// generated from the same mount set.
    pub fn from_mounts(mounts: &[crate::mount::Mount]) -> Self {
        let mut map = Self::new();
        for mount in mounts {
            let specifier = mount.specifier_prefix();
            if !specifier.is_empty() {
                map.insert(specifier, mount.url_prefix());
            }
        }
        map
    }

    /// Add or replace an entry. Returns `&mut self` for chaining.
    pub fn insert(&mut self, specifier: impl Into<String>, url: impl Into<String>) -> &mut Self {
        self.imports.insert(specifier.into(), url.into());
        self
    }

    /// Merge `other` into `self`; on key conflicts `other` wins (call order is
    /// precedence: merge more specific fragments last).
    pub fn extend(&mut self, other: Importmap) -> &mut Self {
        self.imports.extend(other.imports);
        self
    }

    /// Whether the map has no entries.
    pub fn is_empty(&self) -> bool {
        self.imports.is_empty()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.imports.len()
    }

    /// Iterate `(specifier, url)` pairs in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.imports.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Whether [`resolve`](Self::resolve) maps `specifier`.
    pub fn resolves(&self, specifier: &str) -> bool {
        self.resolve(specifier).is_some()
    }

    /// The address `specifier` maps to: its exact key's URL, or the longest `/`-key's URL
    /// with the rest appended, as written, not normalized.
    /// `None` where no key matches or the browser refuses the match.
    ///
    /// ```
    /// use web_modules::importmap::Importmap;
    /// let mut map = Importmap::new();
    /// map.insert("lit", "/web_modules/lit/index.js")
    ///    .insert("lit/", "/web_modules/lit/")
    ///    .insert("lit/directives/", "/vendor/directives/");
    /// assert_eq!(map.resolve("lit").as_deref(), Some("/web_modules/lit/index.js"));
    /// assert_eq!(map.resolve("lit/html.js").as_deref(), Some("/web_modules/lit/html.js"));
    /// assert_eq!(
    ///     map.resolve("lit/directives/repeat.js").as_deref(),
    ///     Some("/vendor/directives/repeat.js"),
    /// );
    /// assert_eq!(map.resolve("react"), None);
    /// assert_eq!(map.resolve("lit/../secret.js"), None);
    /// ```
    pub fn resolve(&self, specifier: &str) -> Option<String> {
        let (key, url) = self.entry(specifier)?;
        let rest = &specifier[key.len()..];
        // The browser's checks, on the URLs it would parse.
        let address = ADDRESS_BASE.join(url).ok()?;
        if key.ends_with('/') && !address.as_str().ends_with('/') {
            return None;
        }
        if !rest.is_empty() {
            let target = address.join(rest).ok()?;
            if !target.as_str().starts_with(address.as_str()) {
                return None;
            }
        }
        Some(format!("{url}{rest}"))
    }

    /// The exact key, else the longest prefix key.
    fn entry(&self, specifier: &str) -> Option<(&str, &str)> {
        if let Some((key, url)) = self.imports.get_key_value(specifier) {
            return Some((key, url));
        }
        self.imports
            .iter()
            .filter(|(key, _)| key.ends_with('/') && specifier.starts_with(key.as_str()))
            .max_by_key(|(key, _)| key.len())
            .map(|(key, url)| (key.as_str(), url.as_str()))
    }

    /// Read an import-map fragment file: a JSON document whose top-level
    /// `"imports"` object has string values.
    pub fn from_json_file(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        Self::from_json_str(&text, &path.display().to_string())
    }

    /// Parse an import-map JSON document (a top-level `"imports"` object with string
    /// values); `context` names the source in error messages.
    pub fn from_json_str(json: &str, context: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|e| Error::ImportMap(format!("{context}: {e}")))
    }

    /// Serialize to a pretty JSON document.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("string map serializes")
    }

    /// Render a complete `<script type="importmap">…</script>` element (compact JSON).
    ///
    /// Specifiers and URLs are auto-derived from each package's `package.json`, which
    /// is untrusted. `<`, `>` and `&` in the JSON are emitted as `\uXXXX` escapes so a
    /// hostile value (e.g. a specifier containing `</script>`) cannot terminate the
    /// element; the escapes are valid JSON, so the browser parses the same import map.
    pub fn to_script_tag(&self) -> String {
        format!(
            "<script type=\"importmap\">{}</script>",
            escape_for_script(&serde_json::to_string(self).expect("string map serializes"))
        )
    }

    /// Write the import map to `path`, creating parent directories.
    pub fn write_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, self.to_json())?;
        Ok(())
    }
}

/// Escape JSON for embedding in an HTML `<script>` element. Script data can only be
/// terminated by `</`, so neutralising `<` (plus `>`/`&` by convention) as `\uXXXX`
/// stops an untrusted specifier/URL from closing the tag. These are valid JSON string
/// escapes; a browser decodes them back, so the parsed import map is unchanged. Used
/// only for the inline tag; the standalone `importmap.json` is served as JSON.
fn escape_for_script(json: &str) -> String {
    json.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_other_wins() {
        let mut base = Importmap::new();
        base.insert("lit", "/old.js").insert("only-base", "/b.js");
        let mut other = Importmap::new();
        other.insert("lit", "/new.js").insert("only-other", "/o.js");
        base.extend(other);
        let json = base.to_json();
        assert!(json.contains("/new.js"));
        assert!(!json.contains("/old.js"));
        assert!(json.contains("only-base") && json.contains("only-other"));
    }

    #[test]
    fn round_trips_through_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("importmap.json");
        let mut original = Importmap::new();
        original
            .insert("lit", "/web_modules/lit/index.js")
            .insert("lit/", "/web_modules/lit/");
        original.write_to(&path).unwrap();
        let parsed = Importmap::from_json_file(&path).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn script_tag_is_compact_and_wrapped() {
        let mut map = Importmap::new();
        map.insert("lit", "/web_modules/lit/index.js");
        let tag = map.to_script_tag();
        assert!(tag.starts_with("<script type=\"importmap\">{"));
        assert!(tag.ends_with("}</script>"));
        assert!(!tag.contains('\n'));
    }

    #[test]
    fn script_tag_escapes_html_breakout() {
        // A hostile package.json `exports` key could carry markup like this.
        let mut map = Importmap::new();
        map.insert(
            "evil/</script><script>alert(1)</script>",
            "/web_modules/evil/index.js",
        );
        let tag = map.to_script_tag();
        // The injected closing tag is neutralised; the element has exactly one of its own.
        assert!(tag.contains("\\u003c/script\\u003e"));
        assert_eq!(tag.matches("</script>").count(), 1);
        // The standalone JSON artifact (served as application/json) is left verbatim.
        assert!(map.to_json().contains("</script>"));
    }

    #[test]
    fn resolves_exact_and_prefix() {
        let mut map = Importmap::new();
        map.insert("lit", "/web_modules/lit/index.js")
            .insert("lit/", "/web_modules/lit/");
        assert!(map.resolves("lit"));
        assert!(map.resolves("lit/decorators.js")); // via the prefix key
        assert!(!map.resolves("react"));
        assert!(!map.resolves("@oxc-project/runtime/helpers/decorate"));
    }

    #[test]
    fn resolve_maps_exact_and_prefix_keys() {
        let mut map = Importmap::new();
        map.insert("lit", "/web_modules/lit/index.js")
            .insert("lit/", "/web_modules/lit/");
        // Exact beats prefix.
        assert_eq!(
            map.resolve("lit").as_deref(),
            Some("/web_modules/lit/index.js")
        );
        map.insert("pkg/", "/web_modules/pkg/")
            .insert("pkg/index.js", "/elsewhere.js");
        assert_eq!(
            map.resolve("pkg/index.js").as_deref(),
            Some("/elsewhere.js")
        );
        assert_eq!(
            map.resolve("lit/decorators.js").as_deref(),
            Some("/web_modules/lit/decorators.js")
        );
        assert_eq!(map.resolve("lit/").as_deref(), Some("/web_modules/lit/"));
        assert_eq!(map.resolve("react"), None);
        assert!(!map.resolves("react"));
    }

    #[test]
    fn resolve_takes_the_longest_prefix() {
        let mut map = Importmap::new();
        // `@shell/` sorts first.
        map.insert("@shell/components/", "/components/")
            .insert("@shell/", "/shell/");
        assert_eq!(
            map.resolve("@shell/components/button.js").as_deref(),
            Some("/components/button.js")
        );
        assert_eq!(
            map.resolve("@shell/registry.js").as_deref(),
            Some("/shell/registry.js")
        );
    }

    #[test]
    fn resolve_refuses_what_the_browser_blocks() {
        let mut map = Importmap::new();
        map.insert("lit/", "/web_modules/lit/")
            .insert("bad/", "/vendor/bad")
            .insert("pkg/", "/pkg/")
            .insert("pkg/sub/", "/pkg-sub")
            .insert("cdn/", "https://cdn.example/pkg/")
            .insert("broken", "http://[::1");
        // Climbing above the key's URL, however spelled.
        for specifier in [
            "lit/../secret.js",
            "lit/a/../../secret.js",
            "lit/%2e%2e/secret.js",
            "lit/..\\secret.js",
            "lit//etc/passwd",
            "lit///evil.example/x.js",
            "lit/https://evil.example/x.js",
            "cdn/../x.js",
        ] {
            assert_eq!(map.resolve(specifier), None, "{specifier}");
            assert!(!map.resolves(specifier), "{specifier}");
        }
        // No fallback to a shorter key.
        for specifier in ["bad/x.js", "bad/", "pkg/sub/x.js"] {
            assert_eq!(map.resolve(specifier), None, "{specifier}");
        }
        assert_eq!(map.resolve("broken"), None);
        assert_eq!(
            map.resolve("lit/a/../b.js").as_deref(),
            Some("/web_modules/lit/a/../b.js")
        );
        assert_eq!(
            map.resolve("cdn/x.js").as_deref(),
            Some("https://cdn.example/pkg/x.js")
        );
        assert_eq!(map.resolve("pkg/x.js").as_deref(), Some("/pkg/x.js"));
    }

    #[test]
    fn a_key_without_trailing_slash_is_no_prefix() {
        let mut map = Importmap::new();
        map.insert("lit", "/web_modules/lit/index.js")
            .insert("@scope/pkg", "/pkg.js");
        for specifier in ["lit/decorators.js", "lit-html", "@scope/pkg/x.js", "li"] {
            assert_eq!(map.resolve(specifier), None, "{specifier}");
            assert!(!map.resolves(specifier), "{specifier}");
        }
    }

    #[test]
    fn rejects_non_string_and_missing_imports() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.json");
        fs::write(&bad, r#"{"imports":{"lit":42}}"#).unwrap();
        assert!(matches!(
            Importmap::from_json_file(&bad).unwrap_err(),
            Error::ImportMap(_)
        ));
        fs::write(&bad, r#"{"nope":{}}"#).unwrap();
        assert!(matches!(
            Importmap::from_json_file(&bad).unwrap_err(),
            Error::ImportMap(_)
        ));
    }
}
