//! Symlink policy for walking and serving source trees.
//!
//! A source tree is possibly-untrusted input, so what a symlink means is a policy
//! decision, not an accident of the walker: [`SymlinkMode`] selects it once and the
//! build preflight, the dev server, and the static router all obey the same choice.
//! The default, [`Follow`](SymlinkMode::Follow), is the safe one — a link resolves
//! only within its own source root.
//!
//! The mode governs **source-file discovery and request resolution only**. It never
//! relaxes a compiler or security sandbox: SCSS `@use`/`@import` resolution and npm
//! archive extraction stay containment-always, request-path traversal and the reject
//! list apply in every mode, and the build's output-escape guard is unconditional.

/// How symlinks in a source tree behave, everywhere: the build preflight, the dev
/// server, and the static router. Selected on
/// [`Processors::symlinks`](crate::build::Processors) (so `build` and `dev` share
/// it), on [`Frontend::symlinks`](crate::Frontend), and via `--symlinks` on the CLI.
///
/// | Mode | build | serving |
/// |---|---|---|
/// | `Follow` (default) | a link resolving outside its root fails the build | 404 |
/// | `FollowUnsafe` | every link publishes; dangling warns and skips | dangling 404s |
/// | `Redirect` | links are skipped with a warning | `307`, the link content is the `Location` |
/// | `Move` | links are skipped with a warning | `308`, same `Location` rule |
///
/// `Redirect` and `Move` respond without ever opening the link target; a static
/// build cannot express an HTTP redirect, so those two modes skip symlinks when
/// building. Embedded (`include_dir!`) trees carry no symlinks — the mode is a
/// filesystem concern.
///
/// The two redirect modes are compiled behind the default-on `symlink-move`
/// feature: `--no-default-features` yields a build in which a symlink can never
/// become a redirect, while `Follow` and `FollowUnsafe` are always available.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SymlinkMode {
    /// Follow a symlink within its own source root; one that canonically resolves
    /// outside the root is refused (the build errors, serving 404s). Links do not
    /// work across source directories.
    #[default]
    Follow,
    /// Follow every symlink, wherever it points. A dangling link 404s when served
    /// and is skipped with a warning when building.
    FollowUnsafe,
    /// Do not follow: serving answers `307 Temporary Redirect` with the symlink's
    /// own content as the `Location`; the build skips the link with a warning.
    /// Requires the `symlink-move` feature (default-on).
    #[cfg(feature = "symlink-move")]
    Redirect,
    /// [`Redirect`](SymlinkMode::Redirect), but permanent: `308 Permanent Redirect`.
    /// Requires the `symlink-move` feature (default-on).
    #[cfg(feature = "symlink-move")]
    Move,
}

/// CLI mirror of [`SymlinkMode`] (`--symlinks follow|follow-unsafe|redirect|move`),
/// kept separate so the domain enum stays `#[non_exhaustive]` without tying the
/// public API to clap.
#[cfg(feature = "cli")]
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SymlinkModeArg {
    #[default]
    Follow,
    FollowUnsafe,
    #[cfg(feature = "symlink-move")]
    Redirect,
    #[cfg(feature = "symlink-move")]
    Move,
}

#[cfg(feature = "cli")]
impl From<SymlinkModeArg> for SymlinkMode {
    fn from(arg: SymlinkModeArg) -> Self {
        match arg {
            SymlinkModeArg::Follow => SymlinkMode::Follow,
            SymlinkModeArg::FollowUnsafe => SymlinkMode::FollowUnsafe,
            #[cfg(feature = "symlink-move")]
            SymlinkModeArg::Redirect => SymlinkMode::Redirect,
            #[cfg(feature = "symlink-move")]
            SymlinkModeArg::Move => SymlinkMode::Move,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_safe_mode() {
        assert_eq!(SymlinkMode::default(), SymlinkMode::Follow);
    }

    #[cfg(feature = "cli")]
    #[test]
    fn cli_values_are_kebab_case() {
        // Pins the user-facing strings, `move` included (a legal value string even
        // though lowercase `move` is a Rust keyword). The redirect values exist only
        // with the default-on `symlink-move` feature.
        use clap::ValueEnum;
        let names: Vec<_> = SymlinkModeArg::value_variants()
            .iter()
            .map(|v| v.to_possible_value().unwrap().get_name().to_string())
            .collect();
        #[cfg(feature = "symlink-move")]
        assert_eq!(names, ["follow", "follow-unsafe", "redirect", "move"]);
        #[cfg(not(feature = "symlink-move"))]
        assert_eq!(names, ["follow", "follow-unsafe"]);
    }
}
