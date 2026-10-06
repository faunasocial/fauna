# fauna-web — the web app

The Fauna web app: a Svelte 5 single-page app built with **Deno** (no
Node.js), sharing the Rust core with every other app via WebAssembly
(`libs/fauna-wasm*`, built with `wasm-pack`). In production the nest server
serves this app itself at `/app/` — users never install anything.

## Build

Needs `deno` and `wasm-pack` (plus the repo's pinned Rust toolchain, which
installs itself). From the repository root:

```sh
just web        # builds the WASM modules + the SPA into apps/fauna-web/build
```

## Develop

```sh
just web-dev    # builds, then serves the SPA on :8080 against a local nest on :3000
```

The SPA and nest are separate origins in this dev split (hence the CORS flags
the recipe passes); in production the nest serves the SPA same-origin.

## Test

```sh
deno task --config apps/fauna-web/deno.json check   # type-check (svelte-check)
just web-test                                       # test-flavoured build for the e2e suite
pytest tests/e2e-unified/tests/ --client web        # cross-app e2e (see tests/e2e-unified/README.md)
```

## Layout

- `src/routes/` — SvelteKit pages (feed, conversations, settings, admin, onboarding, …)
- `src/lib/` — shared components + the WASM glue (`wasm-*.ts`)
- `static/` — the built WASM chunks land here (gitignored; `just web` populates them)

UI element IDs come from `tests/e2e-unified/ui.yaml` (`data-testid`) — the
same IDs as every other app.
