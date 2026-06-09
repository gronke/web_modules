# web-modules

**Pure-Rust tooling for developing Web Components** — vendor npm packages, transform
TypeScript/SCSS, and serve or embed a native-ESM frontend, with **no Node, no npm and no
bundler** at build time. Use it as a **`web-modules` CLI** for everyday development, or as a
**library** from a `build.rs` / at runtime. Built on [`npm-utils`], [oxc] and [`grass`].

## Features

| Capability | What it does |
|------------|--------------|
| **Vendor** | resolve + download npm packages into `web_modules/<name>/` + an [import map] (bare specifiers → vended files; npm and GitHub sources; curate the browser set with a `webDependencies` whitelist) |
| **Transform** | TypeScript → browser JS ([oxc]) and SCSS → CSS ([`grass`]); optional minify, `.d.ts`, XLIFF i18n, and icon generation — *the processors* (full list on [docs.rs]) |
| **Dev server** | serve a frontend straight from source — compile TS/SCSS on the fly, watch the tree, live-reload the browser |
| **Build** | vendor + transform + render `index.html` into an embeddable `dist/` from a `build.rs` (`web_modules::build`), then `include_dir!` it into your binary |
| **Bundle** | fold a CommonJS package + its `node_modules/` into one browser ES module via [rolldown] — for React-class packages that ship only CJS (opt-in) |

## CLI

```bash
cargo install web_modules --features cli
```

| Command | |
|---------|--|
| `web-modules dev [roots…]` | dev server with watch + live-reload |
| `web-modules compile [roots…] --out <dir>` | compile a source tree into an output dir |
| `web-modules vendor [pkg…] [--manifest <pkg.json>]` | vendor npm packages → `web_modules/` + import map |
| `web-modules ci [dir]` | install a `package-lock.json`'s exact tree — a pure-Rust `npm ci` |
| `web-modules npm <args…>` | run an [`npm-utils`] command (`add` / `install` / `upgrade` / …) |

Run `web-modules <command> --help` for flags.

## Library

```toml
[dependencies]
web_modules = "0.1"   # Rust 1.82+
```

`typescript`, `scss` and `tera` are on by default; opt into `minify`, `axum`, `dev`, `cli`,
`bundle`, … as needed (`full` enables everything but `bundle`). For the `build.rs` / runtime
API and the full feature list see the **[API docs][docs.rs]**; for runnable demos see
**[`examples/`](examples/)**.

## License

MIT — see [LICENSE](LICENSE).

[import map]: https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/script/type/importmap
[`npm-utils`]: https://github.com/gronke/rust-npm-utils
[oxc]: https://oxc.rs
[`grass`]: https://github.com/connorskees/grass
[rolldown]: https://rolldown.rs
[docs.rs]: https://docs.rs/web_modules
