# esptool-git example

Consumes a dependency that publishes **only TypeScript**, straight from its git reference, compiled from source by this toolchain.
No npm build, no committed `lib/`, no Node.

The dependency is [esptool-js](https://github.com/gronke/fork-esptool-js) (Apache-2.0), the library that speaks the ESP32 serial protocol.
The page asks a board what it is and prints the answer.

```sh
cargo run -p esptool-git
# open http://127.0.0.1:8080/ in Chrome or Edge and connect a board
```

## What it demonstrates

- **A git dependency built from source.** `web/package.json` names it by git reference and lists it under `web_modules.sourceDependencies`:

  ```json
  { "dependencies": { "esptool-js": "github:gronke/fork-esptool-js#gronke" },
    "web_modules": { "sourceDependencies": ["esptool-js"] } }
  ```

  `source_specs_from_package_json` turns that into a source spec and `vendor_sources` fetches it into `web/deps/`, returning a `Mount`.
  From there it is indistinguishable from the [`compose`](../compose) example's `file:` path-deps: one mount set, sources compiled on request, raw `.ts` never served.

- **Why it has to work this way.** The repository ships `src/*.ts` and nothing else.
  The default browser-asset extraction drops `src/` — reasonably, since a published package's sources are redundant beside its built output — which leaves a source-only package with nothing to vendor.
  `keep_sources` keeps the sources instead, and the pipeline compiles them.

- **Prebuilt and source-built side by side.** `pako` and `atob-lite` are ordinary registry deps, vendored as published output into `web/web_modules/`, and esptool-js imports both.
  One import map covers both kinds.

- **Co-generated map and tsconfig.** The runtime import map (`Importmap::from_mounts`) and the editor `tsconfig.json` (`write_tsconfig_base`) come from the same mount set, so an editor resolves `esptool-js/...` exactly as the browser does.

## The page

`web/app.ts` imports `ESPLoader` and `Transport` from `esptool-js/src/index.js`, a prefix specifier into the mount and the same shape `compose` uses to import a sibling by name.
It reports the chip's description, MAC, crystal frequency, features and flash size.

It **only reads.** `detectChip()` identifies the chip over Web Serial, the flasher stub is never uploaded and nothing is written, so pointing this at a board cannot modify it.

The port picker is filtered to Espressif's USB vendor id (`0x303a`), so it offers boards rather than every serial port on the machine.
Web Serial means Chrome or Edge, and the page says so and disables the button elsewhere.

## Generated files

`web/deps/`, `web/web_modules/`, `web/index.html`, `web/importmap.json` and `tsconfig.json` are all produced at startup from the tracked sources, and gitignored.
