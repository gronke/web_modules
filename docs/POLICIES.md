# Behavior policies

How `build`, `dev` and the static router treat pages, output paths, the output directory and symlinks.
These are the toolchain's policies, not the CLI's: the same rules bind the library's `Build` / `Dev` builders, `build()` and `Frontend`.
The CLI flag reference: [CLI.md](CLI.md).

## HTML policy

The build never reads or rewrites your HTML.
Pages are generated only where you opt in: a `*.tera` template (rendered with the generated import map as `{{ importmap }}`), or the `--html` / `--template` fallback when no source provides an `index.html`.
The generated import map is the contract (`importmap.json`, the `{{ importmap }}` Tera variable, the `{importmap}` placeholder), and it is the only map the unresolved-import check validates against; a hand-authored page owns its inline map.
Template-rendered JavaScript joins the module graph and is validated like any emitted module, with one ordering rule: runtime-helper vendoring is decided before templates render, so an `@oxc-project/runtime` import that appears only in template-rendered JavaScript fails the check rather than vendoring the runtime.
Put such code in a `.ts` / `.js` source.

## Duplicate output paths

Two sources claiming one output path (`index.html` next to `index.html.tera`, `app.js` next to `app.ts`, `style.css` next to `style.scss`, or the same path in two roots) fail `build` before any write, listing every conflict; `dev` warns instead.
`--skip-duplicates` opts into precedence: the earlier root wins, and within a root a Tera template beats a literal file beats a transformed sibling, the same rule in `build` and `dev`.
Generated outputs are reserved regardless: a source claiming `importmap.json`, a path under `web_modules/`, a `.map` sidecar (with `--sourcemap`), or a `.gz` sidecar (with `--gzip`) fails the build even under `--skip-duplicates`.

## Output directory

Each build is staged in a temporary sibling and then atomically replaces `--out`, so the output always describes exactly the current sources: nothing from a previous build survives, and a failed build leaves the previous output untouched.
`--out` must therefore be dedicated: absent, empty, or a previous build's output, recognized by the `.web-modules-out` marker the build writes.
Anything else (the project directory under `--out .`, a directory with your own files) is refused rather than deleted; delete a pre-existing output directory once when upgrading.
The `web_modules/` vendor cache carries over from the previous output and is re-validated, so packages are not re-downloaded on every build; packages you no longer request are pruned.

## Symlinks

`--symlinks` (also `Processors::symlinks`, the builders' `.symlinks(…)`, and `Frontend::symlinks`) sets what a symlink in a source tree means, consistently across `build`, `dev`, and the static router:

| Mode | build | serving |
|---|---|---|
| `follow` (default) | a link resolving outside its own root fails the build | 404 |
| `follow-unsafe` | every link publishes; a dangling one warns and skips | a dangling one 404s |
| `redirect` | links are skipped with a warning | `307 Temporary Redirect`, the link content is the `Location` |
| `move` | links are skipped with a warning | `308 Permanent Redirect`, same rule |

The redirect modes are compiled behind the default-on `symlink-move` feature; `--no-default-features` drops them, leaving `follow` and `follow-unsafe`.
They answer without opening the target, taking the link content literally as the `Location`, which is also why a static build skips a link.
A symlink mode never relaxes a security sandbox: path traversal, the reject list, source-hiding, the SCSS import sandbox, and vendor-extraction hardening are unaffected.
The live-reload watcher's behavior through links is backend-defined; under `follow-unsafe` an edit behind an out-of-tree link may not trigger a reload.
