//! Reject-path filtering: keep config / secret / server-side files out of built output,
//! bundles, and served responses.
//!
//! A [`Reject`] is a compiled set of rules consulted per path. Matching is **per path
//! component** and **case-insensitive**. Built output (`copy_static`) checks every component of
//! the path under the source root with [`rejects_path`](Reject::rejects_path), so a rejected
//! directory (`.git/`) is dropped whole. Serving (the dev server and the static `Frontend`)
//! checks the request string with [`rejects`](Reject::rejects), then re-checks the canonicalized
//! file name, so OS case-folding, a trailing dot, or a symlink cannot serve a rejected file under
//! an allowed name. On the resolved path only the file name is matched; directory components are
//! matched on the request path.
//!
//! Rules come from **presets** (`--reject-preset`, default [`all`](Reject::all)) or an
//! explicit **list** (`--reject-list`, a full replace):
//! - **source** — server-side / source extensions (`php`, `ts`, …).
//! - **hidden** — any dotfile / dotdir component (`.git/`, `.env`, …), except `.well-known`.
//! - **config** — build manifests (`package.json`, `tsconfig.json`, …).
//!
//! The default (all presets) is safe-by-default; pass `none` (or a narrower set) to opt out.

use std::path::Path;

/// A set of reject-rule preset groups, composed with the bitwise operators and passed to the
/// [`Build`](crate::Build) / [`Dev`](crate::Dev) / [`Frontend`](crate::Frontend) builders'
/// `reject_preset` (the typed counterpart of `--reject-preset`).
///
/// ```
/// use web_modules::reject::Presets;
/// let all_but_config = Presets::ALL & !Presets::CONFIG; // everything except config manifests
/// let config_or_source = Presets::CONFIG | Presets::SOURCE;
/// assert!(all_but_config.contains(Presets::HIDDEN) && !all_but_config.contains(Presets::CONFIG));
/// # let _ = config_or_source;
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Presets(u8);

impl Presets {
    /// Server-side / source-code extensions that must never be published or served raw.
    pub const SOURCE: Presets = Presets(0b001);
    /// Dotfiles and dotdirs (`.git/`, `.env`, `.sops`, …), except the standard `.well-known`.
    pub const HIDDEN: Presets = Presets(0b010);
    /// Build / tooling manifests (`package.json`, `package-lock.json`, `tsconfig.json`, …).
    pub const CONFIG: Presets = Presets(0b100);
    /// Every preset (`SOURCE | HIDDEN | CONFIG`) — the default selection.
    pub const ALL: Presets = Presets(0b111);
    /// No presets.
    pub const NONE: Presets = Presets(0);

    /// Whether every group in `other` is set.
    pub fn contains(self, other: Presets) -> bool {
        self.0 & other.0 == other.0
    }

    fn parse_one(name: &str) -> Option<Presets> {
        match name {
            "source" => Some(Presets::SOURCE),
            "hidden" => Some(Presets::HIDDEN),
            "config" => Some(Presets::CONFIG),
            _ => None,
        }
    }
}

impl std::ops::BitOr for Presets {
    type Output = Presets;
    fn bitor(self, rhs: Presets) -> Presets {
        Presets(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for Presets {
    type Output = Presets;
    fn bitand(self, rhs: Presets) -> Presets {
        Presets(self.0 & rhs.0)
    }
}

impl std::ops::Not for Presets {
    type Output = Presets;
    /// Complement within [`ALL`](Presets::ALL), so `!CONFIG == SOURCE | HIDDEN`.
    fn not(self) -> Presets {
        Presets(!self.0 & Presets::ALL.0)
    }
}

/// `source`-preset extensions (lowercase, compared case-insensitively).
const SOURCE_EXTS: &[&str] = &["php", "php5", "phtml", "ts", "tsx", "mts", "scss", "tera"];
/// `config`-preset filenames (lowercase).
const CONFIG_NAMES: &[&str] = &[
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "tsconfig.json",
];
/// The one dotted name the `hidden` preset still allows (a standard served directory).
const WELL_KNOWN: &str = ".well-known";

/// A compiled set of reject rules. Build it from [`presets`](Reject::parse_presets) (the
/// `--reject-preset` default is [`all`](Reject::all)) or an explicit
/// [`list`](Reject::from_list) (`--reject-list`, a full replace). [`Default`] is `all`,
/// so the option is safe-by-default wherever it's threaded.
#[derive(Clone, Debug)]
pub struct Reject {
    /// Lowercased extensions (a component whose extension matches is rejected).
    exts: Vec<String>,
    /// Lowercased exact component names (a file or directory name).
    names: Vec<String>,
    /// Reject any component starting with `.` (except [`WELL_KNOWN`]).
    hidden: bool,
}

impl Default for Reject {
    /// All presets — the safe default.
    fn default() -> Self {
        Self::all()
    }
}

impl From<Presets> for Reject {
    /// Compile the rule set selected by `presets` (the typed form of `--reject-preset`).
    fn from(presets: Presets) -> Self {
        let mut r = Self::empty();
        if presets.contains(Presets::SOURCE) {
            r.exts = SOURCE_EXTS.iter().map(|s| s.to_string()).collect();
        }
        if presets.contains(Presets::CONFIG) {
            r.names = CONFIG_NAMES.iter().map(|s| s.to_string()).collect();
        }
        r.hidden = presets.contains(Presets::HIDDEN);
        r
    }
}

impl Reject {
    fn empty() -> Self {
        Self {
            exts: Vec::new(),
            names: Vec::new(),
            hidden: false,
        }
    }

    /// All presets (`source` + `hidden` + `config`) — the safe default.
    pub fn all() -> Self {
        Presets::ALL.into()
    }

    /// Reject nothing (the `none` preset) — opt out of all filtering.
    pub fn none() -> Self {
        Self::empty()
    }

    /// Add one pattern to the list (`*.ext` extension, `name/` directory, or `name`), on top of
    /// whatever's already set. Case-insensitive. Drives the builders' `reject(pattern)`.
    pub fn add(&mut self, pattern: impl AsRef<str>) {
        let pat = pattern.as_ref().trim();
        if pat.is_empty() {
            return;
        }
        if let Some(ext) = pat.strip_prefix("*.") {
            if !ext.is_empty() {
                self.exts.push(ext.to_ascii_lowercase());
                return;
            }
        }
        let name = pat.strip_suffix('/').unwrap_or(pat);
        self.names.push(name.to_ascii_lowercase());
    }

    /// Parse a `--reject-preset` expression: a comma list with `all` / `none` and `!name`
    /// removal, evaluated left-to-right. Examples: `all` (default), `all,!config`,
    /// `source,hidden`, `none`. Unknown preset names are an error.
    pub fn parse_presets(expr: &str) -> Result<Self, String> {
        let mut active = Presets::NONE;
        for tok in expr.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            match tok {
                "all" => active = Presets::ALL,
                "none" => active = Presets::NONE,
                _ => {
                    let (remove, name) = tok
                        .strip_prefix('!')
                        .map_or((false, tok), |rest| (true, rest));
                    let preset = Presets::parse_one(name)
                        .ok_or_else(|| format!("unknown reject preset `{name}` (in `{expr}`)"))?;
                    active = if remove {
                        active & !preset
                    } else {
                        active | preset
                    };
                }
            }
        }
        Ok(active.into())
    }

    /// Build from an explicit pattern list (`--reject-list`), a **full replace** ignoring
    /// presets. Each pattern is `*.ext` (an extension), `name/` (a directory), or `name`
    /// (an exact component, including a dotfile like `.env`). Case-insensitive.
    pub fn from_list<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut r = Self::empty();
        for pat in patterns {
            r.add(pat);
        }
        r
    }

    /// Whether any component of a lexical relative / request path (separated by `/` or `\`)
    /// is rejected. Use on the raw request string, before touching the filesystem.
    pub fn rejects(&self, rel: &str) -> bool {
        rel.split(['/', '\\']).any(|c| self.component_rejected(c))
    }

    /// Whether any component of a resolved [`Path`] is rejected. Use on the canonicalized
    /// path the request actually opened, so case-folding / trailing dots can't bypass it.
    pub fn rejects_path(&self, path: &Path) -> bool {
        path.components().any(|c| match c {
            std::path::Component::Normal(os) => {
                os.to_str().is_some_and(|n| self.component_rejected(n))
            }
            _ => false,
        })
    }

    /// Whether this set rejects nothing (e.g. the `none` preset).
    pub fn is_empty(&self) -> bool {
        self.exts.is_empty() && self.names.is_empty() && !self.hidden
    }

    fn component_rejected(&self, comp: &str) -> bool {
        if comp.is_empty() {
            return false;
        }
        let lower = comp.to_ascii_lowercase();
        if self.hidden && lower.starts_with('.') && lower != WELL_KNOWN {
            return true;
        }
        if self.names.contains(&lower) {
            return true;
        }
        if let Some(ext) = Path::new(&lower).extension().and_then(|e| e.to_str()) {
            if self.exts.iter().any(|e| e == ext) {
                return true;
            }
        }
        false
    }
}

/// Emit a `tracing::warn!` for a path dropped by the reject list — so users can see what
/// was excluded from built output / serving / bundling. With the `tracing` feature off,
/// this is a no-op (and pulls no `tracing` dependency).
#[cfg(feature = "tracing")]
pub(crate) fn warn_rejected(path: &str) {
    tracing::warn!(target: "web_modules", path, "dropped by the reject list");
}

/// No-op fallback when the `tracing` feature is off.
#[cfg(not(feature = "tracing"))]
pub(crate) fn warn_rejected(_path: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_reject_config_secrets_and_source() {
        let r = Reject::all();
        assert!(r.rejects("package.json"));
        assert!(r.rejects(".env"));
        assert!(r.rejects("web/.git/config"));
        assert!(r.rejects("api/secret.php"));
        assert!(r.rejects("app.ts"));
        // Legitimate web assets are kept.
        assert!(!r.rejects("app.js"));
        assert!(!r.rejects("styles.css"));
        assert!(!r.rejects("logo.svg"));
        assert!(!r.rejects("index.html"));
    }

    #[test]
    fn well_known_is_allowed_even_under_hidden() {
        let r = Reject::all();
        assert!(!r.rejects(".well-known/security.txt"));
        assert!(!r.rejects(".well-known/acme-challenge/abc"));
        // but other dotdirs are still rejected
        assert!(r.rejects(".ssh/id_rsa"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let r = Reject::all();
        assert!(r.rejects(".ENV"));
        assert!(r.rejects("web/.Git/config"));
        assert!(r.rejects("API/SECRET.PHP"));
        assert!(r.rejects("Package.JSON"));
        assert!(!r.rejects(".WELL-KNOWN/x"));
    }

    #[test]
    fn presets_compose_with_all_and_negation() {
        let no_config = Reject::parse_presets("all,!config").unwrap();
        assert!(!no_config.rejects("package.json")); // config allowed
        assert!(no_config.rejects(".env")); // hidden still on
        assert!(no_config.rejects("x.php")); // source still on

        let only = Reject::parse_presets("source,hidden").unwrap();
        assert!(only.rejects("x.php") && only.rejects(".env"));
        assert!(!only.rejects("package.json"));

        let none = Reject::parse_presets("none").unwrap();
        assert!(none.is_empty());
        assert!(!none.rejects(".env") && !none.rejects("package.json"));

        assert!(Reject::parse_presets("all,!bogus").is_err());
    }

    #[test]
    fn from_list_is_a_full_replace() {
        let r = Reject::from_list(["*.php", ".git/", "package.json", ".env"]);
        assert!(r.rejects("x.php"));
        assert!(r.rejects("a/.git/b"));
        assert!(r.rejects("package.json"));
        assert!(r.rejects(".env"));
        // Not in the explicit list (presets are ignored): a `.ts` source and other dotfiles.
        assert!(!r.rejects("app.ts"));
        assert!(!r.rejects(".sops"));
    }

    #[test]
    fn rejects_path_matches_resolved_components() {
        let r = Reject::all();
        assert!(r.rejects_path(Path::new("/abs/web/.git/config")));
        assert!(r.rejects_path(Path::new("/abs/web/secret.PHP")));
        assert!(!r.rejects_path(Path::new("/abs/web/app.js")));
        assert!(!r.rejects_path(Path::new("/abs/web/.well-known/security.txt")));
    }

    #[test]
    fn presets_bitwise_ops_select_rules() {
        // Complement within ALL: everything except config.
        let all_but_config: Reject = (Presets::ALL & !Presets::CONFIG).into();
        assert!(all_but_config.rejects("x.php")); // source on
        assert!(all_but_config.rejects(".env")); // hidden on
        assert!(!all_but_config.rejects("package.json")); // config off

        // Union of two groups, the third left out.
        let cfg_or_src: Reject = (Presets::CONFIG | Presets::SOURCE).into();
        assert!(cfg_or_src.rejects("package.json") && cfg_or_src.rejects("app.ts"));
        assert!(!cfg_or_src.rejects(".env")); // hidden not selected

        // contains() reflects membership; NONE is ALL's complement of itself.
        let sel = Presets::ALL & !Presets::CONFIG;
        assert!(sel.contains(Presets::SOURCE) && sel.contains(Presets::HIDDEN));
        assert!(!sel.contains(Presets::CONFIG));
        assert!(Presets::ALL.contains(Presets::CONFIG));
        assert_eq!(Presets::NONE, Presets::ALL & !Presets::ALL);
    }

    #[test]
    fn add_layers_extra_patterns_onto_a_selection() {
        // The builders' `reject(pattern)` is additive on top of `reject_preset`.
        let mut r: Reject = Presets::NONE.into();
        r.add(".htpasswd");
        r.add("*.bak");
        assert!(r.rejects(".htpasswd"));
        assert!(r.rejects("dir/notes.BAK")); // case-insensitive extension
        assert!(!r.rejects("index.html"));
    }
}
