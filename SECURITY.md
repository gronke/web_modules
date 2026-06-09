# Security

`web-modules` processes **untrusted content from npm registries**: it downloads
package tarballs, extracts files, derives import maps from each package's
`package.json`, and compiles TypeScript/SCSS. The design treats that content as
hostile and keeps it away from anything it shouldn't reach.

## Defenses in place

- **Tarball extraction** (via `npm-utils`) rejects path traversal (no `..` or
  absolute components), skips symlinks rather than following them, and caps the
  download size.
- **Import-map derivation** links only files that are actually present in the
  extracted tree, and escapes specifiers/URLs for the HTML `<script>` context, so a
  hostile `package.json` cannot inject markup into the generated `index.html`.
- **Dev server** rejects path traversal on two independent layers — a lexical check
  on the request path and a containment check on the resolved filesystem path (which
  also defeats a symlink that points outside a source root).
- **Embedded (release) server** has no filesystem access at all: assets are compiled
  into the binary and served from an in-memory tree.
- **Templating** (Tera) renders developer-authored templates with no filesystem
  include loader. Autoescaping is intentionally off so the import-map `<script>` is
  emitted verbatim; insert any *other* untrusted value with Tera's `| escape` filter.
- **TypeScript transform** (oxc) performs no module resolution and executes no code —
  it strips types and lowers decorators on a single in-memory source.

## Known limitations

- **Processors are not sandboxed.** Compilation runs in-process with the build's full
  ambient authority (filesystem, network). In particular, SCSS compiled by `grass`
  resolves `@use`/`@import` by path: a malicious vendored stylesheet that your code
  `@use`s could cause files outside the project to be read at build time and inlined
  into the output. Running processors in a confined sandbox (restricted filesystem and
  network) is under consideration. Until then, treat vendored packages as you would
  any build dependency and review them before vending.
- **No tarball integrity check or decompression-size cap yet.** Verifying the
  registry-published `dist.integrity`/`shasum` and bounding decompressed size are
  tracked for a future `npm-utils` release.

## Reporting

Please report suspected vulnerabilities privately to <stefan@gronke.net> rather than
opening a public issue.
