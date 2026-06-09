# bootstrap example (SCSS theming)

Compiles **custom SCSS that sources Bootstrap's own SCSS** — overriding variables
to theme it — vendored from npm and served by `web-modules`. No Node, no dart-sass.

```sh
cargo run -p bootstrap-scss
# open http://127.0.0.1:8080/
```

- `web/app.scss` overrides `$primary`/`$secondary`/… then `@import`s Bootstrap's
  `scss/bootstrap`. web-modules keeps packages' `.scss` sources, so grass can build
  Bootstrap from source and apply the overrides → `/app.css`, on the fly.

`web/web_modules/` is generated.
