# web-modules GitHub Action

The composite action and how to drive it.
Overview and the Pages recipe: [README.md](../README.md#github-actions).

## What it does

A composite action builds a deployable `dist/` (vendor, transform and render, with the import map injected), with no Node on the runner.
It downloads a prebuilt `web-modules` binary for the runner's OS/arch (Linux x86_64/arm64, macOS arm64/x86_64, Windows x86_64/arm64), or compiles from the action's own source with `from-source: true`.
Publishing stays composed with the official actions.

## Version pinning

Pin `@v0` to track the latest 0.x, or an exact `@v0.3.1`, which fetches the matching binary for a reproducible run.
The `version` input overrides the ref.

## Build a dist artifact

```yaml
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: gronke/web_modules@v0
        with:
          packages: "lit@^3 bootstrap@^5"   # and/or: manifest: web (a dir) or web/package.json
          template: web/index.html.tera     # or inline `html:`; omit for a minimal default
          minify: true
      - uses: actions/upload-artifact@v7
        with: { name: site, path: dist }
```

## Install the binary only

With `build: "false"` the action installs the verified binary onto `PATH` and stops, for jobs whose own scripts drive `web-modules` (`build`, `vendor`, `npm audit`):

```yaml
jobs:
  frontend:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: gronke/web_modules@v0
        with: { build: "false" }        # verified binary on PATH, no build
      - run: scripts/frontend-build.sh  # your script calls `web-modules build ...`
      - run: web-modules npm audit web
```

## Pages and project paths

Enable Pages once under *Settings, Pages, Source: GitHub Actions*.
A project page is served under `/<repo>/`, so pass `mount: /<repo>/web_modules` and keep entry scripts relative (`./app.js`); a user or org `*.github.io` page serves at the root (default `mount: /web_modules`).
This repo dogfoods the action: [`examples/gh-pages/`](../examples/gh-pages) is built and deployed to Pages by [`.github/workflows/pages.yml`](../.github/workflows/pages.yml).

## Inputs

| Input | Default | What it does |
|---|---|---|
| `src` | `web` | Source directory (TypeScript, SCSS, HTML and other static files). |
| `out` | `dist` | Output directory; the `dist` output echoes it. |
| `mount` | `/web_modules` | URL prefix the vendored modules are served at; `/<repo>/web_modules` for a project page. |
| `packages` |  | Space-separated package specs to vendor, e.g. `"lit@^3 bootstrap@^5"`; optional when `manifest` is set. |
| `manifest` |  | A package.json file, or a directory containing one, whose `dependencies` are also vendored. |
| `html` |  | Inline index.html; the literal `{importmap}` becomes the import-map `<script>`. Keep entry scripts relative (`./app.js`). |
| `template` |  | A Tera template file rendered with an `importmap` variable, instead of `html`. |
| `minify` | `false` | Minify the whole dist tree. |
| `minify-web-modules` | `true` | With `minify`, also minify the vendored `web_modules/` tree. |
| `sourcemap` | `false` | Emit source maps for compiled JS. |
| `comments` |  | Comment policy for emitted JS: `keep`, `strip`, `collect` or `none`; empty lets `minify` imply `strip`. |
| `bundle` | `false` | Fold the built tree per entry point; `importmap.json` and `web_modules/` drop out of the output. |
| `bundle-entries` | `app.js` | Space-separated bundle entry points, output-relative. |
| `gzip` | `false` | Write `.gz` sidecars next to assets. |
| `reject-preset` | `all` | Which reject presets keep config / secret / source paths out of the output, e.g. `all,!config` or `none`. |
| `reject-list` |  | Space-separated reject patterns that fully replace the presets, e.g. `.env .git/ *.php`. |
| `build` | `true` | Run `web-modules build` after installing; `false` makes the action a pure installer and ignores every build input above. |
| `version` |  | Which released binary to download (`0.3`, `v0.3.1`, `edge`). Empty: the release matching an exact pinned action tag, else latest. Ignored with `from-source: true`; a binary older than the CLI contract (currently >= 0.4.0) is refused. |
| `from-source` | `false` | Build from the action's own source with cargo instead of downloading: platforms without a prebuilt binary, or an exact ref. Bundle a cache yourself via actions/cache on ~/.cargo. |

Each input's description in [action.yml](../action.yml) is the reference wording.
The build inputs map to the `web-modules build` flags in [CLI.md](CLI.md); run `web-modules build --help` for the same list from the binary.
