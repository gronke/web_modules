# Security

`web-modules` processes **untrusted content**: package tarballs downloaded from registries, and whole source trees a developer points the tool at (a cloned repository, a third-party theme).
The design treats that content as hostile and keeps it away from anything it shouldn't reach.

## Defenses in place

- **Dev server** rejects path traversal on two independent layers — a lexical check on the request path and a containment check on the resolved filesystem path, which also defeats a symlink pointing outside a source root.
  A reject list (config manifests, dotfiles, source extensions, keys / certificates / database dumps) is checked on the request string and re-checked on the resolved file name, so case folding or a trailing dot cannot serve a rejected file under an allowed name.
  Compile failures answer with a generic 500 body; the detail, which can embed local paths, goes to the developer's console only.
- **Symlink policy** is explicit (`--symlinks`): the default `follow` confines a link to its own source root (the build fails on an escape, serving 404s), `redirect`/`move` answer with the link's content as a sanitized `Location` without ever opening the target, and `follow-unsafe` follows anywhere — an opt-in escape hatch, never the default.
- **Source files are never served raw**: `.ts`/`.tsx`/`.mts`, `.scss`, and `.tera` are reachable only through their compiled targets, matched case-insensitively on the resolved path.
- **CLI config is contained**: path fields of a `package.json` `web_modules` block (`roots`, `out`, `template`, `scss.loadPaths`) must be purely relative and, when they exist on disk, canonically resolve inside the project — an untrusted repository cannot steer `dev`/`build` into serving, reading, or writing outside itself.
  CLI flags and environment variables are operator-controlled.
- **SCSS `@use`/`@import` is sandboxed** (`SandboxFs`): every probe and read is canonicalized and confined to the source roots and their load paths, fail-closed; the compile error names any existing path a probe was refused on.
- **TypeScript transform** (oxc) performs no module resolution and executes no code — it strips types and lowers decorators on a single in-memory source.
  The decorator runtime (`@oxc-project/runtime`) is vendored at an exact pinned version tracking the oxc toolchain.
- **Templating** (Tera) renders one template in isolation (`one_off`), with no template store and therefore no filesystem include reach.
  Autoescaping is intentionally off so the import-map `<script>` is emitted verbatim; insert any *other* untrusted value with Tera's `| escape` filter.
- **Import-map derivation** links only files actually present in the extracted tree and escapes specifiers/URLs for the HTML `<script>` context, so a hostile `package.json` cannot inject markup into the generated `index.html`.
- **Tarball handling** (via `npm-utils`) fetches https-only — redirects included — rejects path traversal in archives, skips symlink/hardlink entries, verifies each installed package's sha512 (`web-modules ci`), and caps downloads (100 MB) and extraction (4 GiB, 200k entries).
- **Bundling** (the opt-in `bundle` feature) contains module resolution to the bundle root: a `../..` import chain or a symlinked package that escapes the tree fails the build instead of folding local files into the published bundle.
- **Build-script output** keeps walked paths and messages free of control characters, so a crafted filename cannot inject `cargo:` directives.
- **Embedded (release) server** has no filesystem access at all: assets are compiled into the binary and served from an in-memory tree.

## Trust anchors and caveats

- **`web-modules ci` trusts the lockfile twice**: each tarball's URL *and* the sha512 it is verified against come from the same `package-lock.json`, so on an untrusted lockfile the integrity check authenticates the author's intent, not safety.
  `npm-utils` warns once per distinct tarball host that is not the npm registry; the fetch itself is https-only and capped.
- **Vendored packages are not integrity-pinned**: `vendor()` resolves the newest version matching each range and trusts the registry over TLS; it does not yet verify the registry-published `dist.integrity`.
  Pin ranges deliberately and review lock-step version markers.
- **`npm://` symlinks resolve like Node**: the `node_modules` lookup ascends ancestor directories, so a package installed *above* the project can be found and served.
  A bounded resolution (stopping at the project directory) exists in `npm-utils` (`package_dir_within` / `package_file_within`) and adoption here is planned; the lookup is always confined to the resolved package's canonical directory.
- **`Mount::from_dir` / `read_package_json` are a config-trust boundary** (library API): a manifest's `web_modules.root` and `file:`/`link:` dependency targets are honored as given.
  Only feed them manifests you trust; downstream serving stays contained relative to whatever root resolves.
- **Processors run in-process** with the build's ambient authority; the confined parts are listed above.
  The one network egress a source file can trigger is the pinned decorator-runtime fetch described there.
- **The dev server validates no `Host` header**: as with any local dev server, a DNS-rebinding page in the browser can read whatever the served roots expose.
  The default bind is `127.0.0.1`; keep it there on untrusted networks.

## Reporting

Please report suspected vulnerabilities privately to <stefan@gronke.net> rather than opening a public issue.
