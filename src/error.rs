//! The crate's unified error type.
//!
//! Every fallible public function returns [`Result<T>`] (i.e. `Result<T, Error>`),
//! so a caller handles **one** error type across vendoring, import-map I/O, and
//! every processor.
//!
//! Variants carry a human-readable message (oxc/grass diagnostics are already
//! formatted strings); [`Error::Io`] additionally preserves the underlying
//! [`std::io::Error`] as its [`source`](std::error::Error::source) so the chain is
//! intact. The enum is `#[non_exhaustive]`, so new variants can be added without a
//! breaking release.

use std::fmt;

/// Anything that can go wrong inside `web_modules`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Filesystem / I/O failure (preserves the underlying error as `source`).
    Io(std::io::Error),
    /// TypeScript / modern-JS transform failure (parse, semantic, or transform).
    TypeScript(String),
    /// SCSS compilation failure.
    Scss(String),
    /// JavaScript minification failure.
    Minify(String),
    /// `.d.ts` declaration-emission failure.
    Dts(String),
    /// Vendoring failure: semver parsing, registry resolution, download, or extract.
    Vendor(String),
    /// Import-map parsing or composition failure.
    ImportMap(String),
    /// HTML template rendering failure.
    Template(String),
    /// i18n (XLIFF) parsing or merging failure.
    I18n(String),
    /// Icon (favicon / app-icon) generation failure.
    Icons(String),
    /// `build` pipeline failure (e.g. an emitted module imports an unresolved bare specifier).
    Build(String),
    /// Composition (`Mount`) / tsconfig generation failure.
    Compose(String),
    /// CommonJS→ESM bundling failure (the `bundle` feature, via rolldown).
    Bundle(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::TypeScript(m) => write!(f, "typescript: {m}"),
            Error::Scss(m) => write!(f, "scss: {m}"),
            Error::Minify(m) => write!(f, "minify: {m}"),
            Error::Dts(m) => write!(f, "dts: {m}"),
            Error::Vendor(m) => write!(f, "vendor: {m}"),
            Error::ImportMap(m) => write!(f, "importmap: {m}"),
            Error::Template(m) => write!(f, "template: {m}"),
            Error::I18n(m) => write!(f, "i18n: {m}"),
            Error::Icons(m) => write!(f, "icons: {m}"),
            Error::Build(m) => write!(f, "{m}"),
            Error::Compose(m) => write!(f, "compose: {m}"),
            Error::Bundle(m) => write!(f, "bundle: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Convenience alias: `Result<T, web_modules::Error>`.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn display_prefixes_message_by_kind() {
        assert_eq!(
            Error::TypeScript("bad token".into()).to_string(),
            "typescript: bad token"
        );
        assert_eq!(Error::Scss("oops".into()).to_string(), "scss: oops");
        assert_eq!(Error::Compose("drift".into()).to_string(), "compose: drift");
        // `Build` carries its own already-formatted message, no prefix.
        assert_eq!(
            Error::Build("unresolved import".into()).to_string(),
            "unresolved import"
        );
    }

    #[test]
    fn io_preserves_underlying_error_as_source() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err: Error = io.into(); // exercises From<io::Error>
        assert!(matches!(err, Error::Io(_)));
        assert_eq!(err.to_string(), "no such file");
        // The chain is intact: the underlying io::Error is reachable as the source.
        assert!(err.source().is_some());
    }
}
