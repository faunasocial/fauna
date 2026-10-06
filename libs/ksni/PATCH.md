# Local patch on vendored ksni 0.2.2

Upstream: <https://github.com/iovxw/ksni> (Unlicense / public domain), pinned at the
published **0.2.2** source tree. Files are otherwise verbatim and left in upstream
style (`rustfmt.toml` ignores `libs/ksni`) so re-vendoring stays a clean diff.

## Why this fork exists

On Wayland, mutter (GNOME Shell) refuses to raise/focus a window unless the
activation request carries a valid **xdg-activation token**. A click on a system
tray icon/menu lands on GNOME Shell's surface, not ours, so we cannot mint a valid
token ourselves. GNOME solves this for the StatusNotifierItem (SNI) protocol with a
dedicated D-Bus method: the `ubuntu-appindicators` / `appindicatorsupport` extension
mints a token from the real tray-click input event and calls
`org.kde.StatusNotifierItem.ProvideXdgActivationToken(token)` on the item **before**
dispatching `Activate` / `SecondaryActivate` / the dbusmenu `Event`.

Upstream ksni — **including 0.3.x** — does not declare or handle
`ProvideXdgActivationToken`. So GNOME's call gets `UnknownMethod`, the freshly-minted
token is discarded, and our `present()` is denied: mutter posts the passive
"Fauna is ready" notification instead of showing the window. This is the
close-to-tray "Open Fauna" restore bug (tracked internally).

## The patch (3 files)

- `dbus_interfaces/StatusNotifierItem.xml` — add the `ProvideXdgActivationToken`
  method (one `in` string arg `token`). The dbus-codegen pass turns it into a
  `provide_xdg_activation_token(&self, token: &str)` trait method in the committed
  `src/dbus_interfaces.rs` (§ Generated D-Bus code).
- `src/lib.rs` — add a `Tray::provide_xdg_activation_token(&mut self, token: String)`
  default no-op so consumers can override it to capture the token.
- `src/service.rs` — implement `provide_xdg_activation_token` on the generated SNI
  interface impl for `InnerState<T>`: lock the model and forward to the trait hook
  (no tray re-render, unlike `activate`).

The consumer (`apps/fauna-linux/src/tray.rs`) overrides the hook to stash the token,
which `show_window` then feeds to `gtk::Window::set_startup_id` right before
`present()` (GTK ≥ 4.14.6 consumes it as the Wayland activation token automatically).

## Generated D-Bus code (2026-10-05)

Upstream's `build.rs` ran `dbus-codegen` 0.9 at every build to turn the three
`dbus_interfaces/*.xml` files into Rust. That build-dependency pulled `clap` 2.34
with `ansi_term` (RUSTSEC-2021-0139) and `atty` (RUSTSEC-2021-0145,
RUSTSEC-2024-0375) into the lockfile — unmaintained/unsound code executed on every
build machine, for output that never changes. So the fork commits that output and
drops the generator:

- `build.rs` is deleted and `[build-dependencies]` removed from `Cargo.toml`.
- `src/dbus_interfaces.rs` is dbus-codegen 0.9.1's output, byte-for-byte as the
  last build produced it (sha256 `86f9a1df…80b0f0` before the 4-line header was
  prepended), with the same `GenOpts` upstream's `build.rs` passed per file
  (`skipprefix` `org.kde.` / `com.canonical.`; `ServerAccess::AsRefClosure` for the
  two served interfaces).
- `src/dbus_interface.rs` `include!`s it from the source tree instead of `OUT_DIR`.

The XML files stay as the source of truth. Changing an interface means editing the
XML and regenerating with dbus-codegen 0.9 — a new tool on a dev machine, so it
waits for the owner's approval of that install.

This was chosen over moving to ksni 0.3 (which has no codegen build step): it adds
no new third-party code, while 0.3 is a rewrite the xdg-activation patch would have
to be re-read and re-ported onto.

## Upstreaming

This is a clean, spec-aligned addition; worth proposing upstream (iovxw/ksni). If a
future ksni release implements `ProvideXdgActivationToken`, drop this fork and revert
`apps/fauna-linux/Cargo.toml` to the crates.io dependency.
