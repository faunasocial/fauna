# fauna-linux — the Linux desktop app

Native GTK4 + libadwaita desktop app in Rust, sharing the workspace's core
crates (`libs/fauna-*`) directly — no FFI layer, it is the one app that
links the shared Rust natively.

## Build

On Debian/Ubuntu, the system libraries first:

```sh
sudo apt install build-essential pkg-config libssl-dev cmake clang \
                 libgtk-4-dev libadwaita-1-dev libdbus-1-dev
```

Then, from the repository root:

```sh
just linux-run        # build + launch (debug)
just linux-release    # release binary
just linux-install    # install to ~/.local (or $PREFIX)
```

Flatpak packaging exists too: `just linux-flatpak-install` (needs
`flatpak-builder`).

## Test

```sh
cargo test -p fauna-linux                            # unit tests
pytest tests/e2e-unified/tests/ --client linux       # cross-app e2e (see tests/e2e-unified/README.md)
```

## Layout

- `src/views/` — one module per page (feed, conversations, settings, admin, onboarding, …)
- `src/i18n/` — generated string table (edit `i18n/strings/en.yaml` at the repo
  root and run `just i18n-generate`; never edit the generated file)

UI element IDs come from `tests/e2e-unified/ui.yaml` (via `set_widget_name`) —
the same IDs as every other app.
