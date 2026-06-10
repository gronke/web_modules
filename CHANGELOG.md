# Changelog

Notable changes to `web_modules`. The format follows [Keep a Changelog]; the project is
pre-1.0, so minor releases may include breaking changes.

## [Unreleased]

### Added
- **Import-graph prune** — opt-in "tree-shaking" for the native-ESM vendor. Enable with
  `Output::with_prune_unused(true)`: after vendoring, the build walks the bare imports
  from your modules *through* the vendored packages and deletes every vendored package
  (and its import-map entries) that nothing imports, keeping the embedded output lean.
  Package granularity; `no_imports` packages (SCSS load paths, `<script>` globals) are
  left untouched. Only statically written `import("name")` is followed (a computed
  `import(expr)` can't be — see the method docs).

### Changed
- Module-specifier scanning now reads the **oxc parser's module record** rather than a
  lexical scan — robust against specifiers that only appear in strings/comments and
  against minified spacing (e.g. `from"x"`).

## [0.1.0]

Initial release: a pure-Rust, buildless toolchain for ES modules and Web Components —
vendor npm packages into `web_modules/` + an import map, transform TypeScript/SCSS, a
watch/live-reload dev server, a `build.rs` build pipeline (embeddable via `include_dir!`),
an optional CommonJS→ESM bundler (rolldown), and the `web-modules` CLI. The
`web-modules.webDependencies` whitelist (à la @pika/web / Snowpack) curates the vended set.

[Keep a Changelog]: https://keepachangelog.com/
