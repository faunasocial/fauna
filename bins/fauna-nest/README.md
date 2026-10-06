# fauna-nest — the server

The Fauna server ("nest"): WS-RPC API over one WebSocket per actor, SQLite
storage, TLS with automatic certificates (ACME), federation, and the admin
surface every app's admin pages talk to. One binary; everything a user or
admin can choose is configured from the client apps and persisted server-side
— there are no server config files to hand-edit.

## Run it

**As a deployment**, use the Docker image — it bundles the web app, the mail
bridge, and the supporting services under one supervisor:
[`docs/guides/nest-internet-setup.md`](../../docs/guides/nest-internet-setup.md)
(architecture: [`docs/goal/architecture/installers/docker.md`](../../docs/goal/architecture/installers/docker.md)).
`just docker-run` at the repo root builds and runs that image locally.

**As a bare dev binary**:

```sh
just nest                                  # release build (cargo build -p fauna-nest --release)
cargo run -p fauna-nest -- --bind 127.0.0.1:3000
```

On first boot it prints a one-time admin claim code (also at
`<data-dir>/claim-code`).

## Test

```sh
cargo test -p fauna-nest --lib             # unit tests
cargo test -p fauna-nest --test <name>     # one integration suite
```

Prefer focused test invocations — `--tests` builds ~40 integration binaries.
The cross-app e2e suite (`tests/e2e-unified/`) spins real nest binaries via
its `nest_instance` fixture.

## Layout pointers

- `src/main.rs` / `src/lib.rs` — startup, TLS/listener setup
- `src/*_handlers.rs` — WS-RPC handlers by domain (auth, dns, bridges, admin, …)
- `src/db/` — storage layer
- Business logic shared with clients lives in `libs/fauna-*`, not here

Design docs: `docs/goal/architecture/nest/` (deployment, TLS, storage modes,
recovery) and `docs/goal/architecture/transport.md` (the wire).
