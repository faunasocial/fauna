# Installer: Linux Desktop — target state

Owns: linux-desktop-packaging
Status: ratified
Authority: Linux desktop distribution — the five-channel packaging surface (install.sh root/user-prefix model, Snap, Flatpak, Debian package, AppImage), installed-file layout, AppStream/desktop-entry metadata, per-channel uninstall + user-data preservation; defers the desktop app architecture to `../apps/linux.md` and the nest-server installer to `linux-nest.md`.

## Implementation status today

- **Flatpak always-on seam — BUILT 2026-07-20, live-session VERIFIED 2026-07-20** (§ Flatpak): host user unit over the `xdg-config/systemd/user:create` + `org.freedesktop.systemd1` grants, socket relocated to the instance-shared `$XDG_RUNTIME_DIR/app/<app-id>` subdir (`fauna-ipc` `default_socket_path` keyed on `FLATPAK_ID`; `InstanceLock` rides it), `ConditionFileIsExecutable` on the stable `current/active` deploy link, app-child fallback on any grant/derivation failure. Unit-pinned throughout (`fauna-ipc` path + lock derivation; `fauna-linux` channel detection, `.flatpak-info` parse, condition derivation, unit contents); flatpak bundle build-verified on a Linux dev machine. **Live-session verification (mutates the real desktop session's systemd user manager, which the e2e harness bans — e2e-launch-isolation.md § point 10 — so this was a manual pass, now codified as automated real-session tests, e2e-conventions.md § point 12) — all core always-on criteria PASS and re-verify live on a fresh, from-current-code Flatpak build:** (1) `~/.config/systemd/user/fauna-sync-agent.service` written with the correct `ExecStart` + `ConditionFileIsExecutable` pointing at the deployed `current/active` binary; (2) `systemctl --user status` shows `enabled` + `active (running)`, agent process alive under `bwrap`; (3) the socket exists at `$XDG_RUNTIME_DIR/app/social.fauna.fauna/fauna/sync-agent.sock`; (4) quitting the app (`flatpak kill`) leaves the agent unit running — always-on confirmed; (5) after `flatpak uninstall`, `systemctl --user start fauna-sync-agent` correctly condition-skips (`Active: inactive (dead)`, journal shows `skipped, unmet condition check ConditionFileIsExecutable=…`) with no Restart-flap. **Automating criterion (5) surfaced a real bug the human-paced manual pass had missed (2026-07-20): uninstalling the *running* agent Restart-flapped the unit forever**, because `Restart=always` auto-restarts do NOT re-check `Condition*=` and at `RestartSec=5` the start limit never trips (5×5 s > 10 s window). Fixed by adding `ExecCondition=` (re-run on every auto-restart → condition-skips it) to the shared unit template — § Installation Files; proven live in `tests/e2e-unified/tests/real_session/` (the flatpak-free `test_sync_agent_unit_flap.py` pins the systemd anti-flap mechanism headlessly; `test_sync_agent_flatpak_seam.py` step 5 asserts the same end-to-end). **The folder-portal-persistence-across-restart caveat below remains unverified** — the live pass surfaced a real, now-fixed bug in the location-binding UI (`folder-save-paths` silently dropping a pending folder path) that consumed the pass's interactive budget before this narrower caveat could be exercised; re-attempt in a future pass if it matters before declaring file-sync fully supported on this channel. **Snap always-on: still externally gated** — snapd `user-daemons` re-verified experimental as of snapd 2.75 (2026-04); § Snap.
- **`fauna-sync-agent` ships on all five channels — landed + verified 2026-07-19** (milestone A3 remainder; per-channel design in § Installation Files / § Uninstall). Verified on a Linux dev machine: **install.sh** — tier_3 `TestShellScript` green (agent install/uninstall + the user-mode unit removal, driven through `uninstall.sh`, now a thin alias after its duplicate uninstall logic drifted); **deb** — `just linux-deb` → `dpkg-deb --contents` shows `usr/bin/fauna-sync-agent` beside the app; **AppImage** — AppDir staged + `appimagetool`-packed, `Fauna.AppImage --sync-agent --help` dispatches to the bundled agent and plain `--version` still reaches the app (tier_3 `TestAppRunDispatch` pins the AppRun mechanism headlessly); **Flatpak** — full `flatpak-builder` run exports `/app/bin/fauna-sync-agent` (75/27 MB pair). **Snap remains build-unverified** (no snapcraft on any dev machine; `override-build` addition is the documented best-effort — verify when the channel gains a consumer). Two latent traps fixed in the same change: the client-written unit previously Restart-flapped **forever** after any uninstall (no start-limit trip at `RestartSec=5` — now `ConditionFileIsExecutable`, § Installation Files), and `dh_clean`'s detritus sweep deleted a **tracked** `Cargo.toml.orig` under a vendored crate tree-wide during every deb build (`rules` now runs `dh_clean -Xvendor/`).
- **Working channels:** `install.sh` (root/user prefix + uninstall), AppImage, Debian package (`just linux-deb`, fixed + verified 2026-07-12 — see below), and **Flatpak (fixed + verified 2026-07-14 — see the Toolchain floor bullet below)**.
- **Snap:** `snapcraft.yaml` is present and as documented, but there is no `just` recipe — build with `snapcraft` directly. **Decision (2026-07-12, ratified by this doc): stays that way** — a Snap-specific `just` recipe isn't worth adding while the channel has no CI/dev-loop consumer; `snapcraft` direct-build is the documented path.
- **Debian package: FIXED + VERIFIED 2026-07-12 — `just linux-deb` now produces an installable `.deb` on a Linux dev machine.** All four originally-declared breaks are fixed: `debian/rules` now installs `$(CARGO_TARGET_DIR)/release/fauna-desktop` (falls back to `target/release/...`) into `debian/fauna-desktop/usr/bin/fauna-desktop` (both binary path and package-dir now agree with `control`'s `Package: fauna-desktop`); `debian/changelog` + `debian/source/format` (`3.0 (native)`) now exist; `debian/control` gained the missing `Source:` stanza (`Build-Depends: debhelper-compat (= 13), pkg-config, libgtk-4-dev, libadwaita-1-dev` — deliberately no `cargo`/`rustc`, the pinned nightly toolchain comes from rustup, not apt) and `Architecture: any` (not the stale `amd64` — the primary dev machine building this package is aarch64; `dpkg-gencontrol` correctly resolved the built package to `arm64`, confirmed in the produced `.deb`'s own control data). Also fixed: the shared `social.fauna.fauna.desktop` (used by both this channel and Flatpak) had `Exec=fauna-linux`/`Icon=social.fauna.fauna`, neither of which exists — corrected to `Exec=fauna-desktop`/`Icon=fauna`, matching `install.sh`'s `fauna.desktop` and the canonical `$PREFIX/bin/fauna-desktop` path below; `rules` now also installs `fauna.svg` as the icon (previously the deb channel shipped no icon at all). New `just linux-deb` recipe: copies `apps/fauna-linux/packaging/debian/` to a disposable `/debian` at the repo root (`dpkg-buildpackage`'s expected location) — a real copy, not a symlink, because a symlink let `dh`'s build-stamp files leak into the tracked source dir and once got silently replaced by a plain directory mid-build (broke `dh_installdocs`'s `debian/control` lookup). **Proof:** `debhelper` installed (user ran `sudo apt install -y debhelper`; only unmet Build-Dep per `dpkg-checkbuilddeps`), `just linux-deb` run twice (clean + reproducible) → `fauna-desktop_0.1.0_arm64.deb`, `dpkg-deb --info`/`--contents` confirm `Architecture: arm64`, correctly auto-populated `Depends:` (`${shlibs:Depends}` resolved real libgtk-4/libadwaita/etc. sonames), and `/usr/bin/fauna-desktop` + the desktop entry + `fauna.svg` icon all present at the goal doc's canonical paths.
- **Manifest duplication — RESOLVED 2026-07-13.** `apps/fauna-linux/social.fauna.desktop.json` (the broken duplicate — `command: "fauna"` didn't match the `fauna-desktop` binary it staged) is deleted; `apps/fauna-linux/packaging/flatpak/social.fauna.fauna.yml` is now the sole manifest and the one all four `just linux-flatpak*` recipes build. Its missing icon-install line is also fixed (`fauna.svg` → `/app/share/icons/hicolor/scalable/apps/fauna.svg`, matching `Icon=fauna` in the shared desktop entry).
- **Toolchain floor — RESOLVED 2026-07-14 (option (c) of the 2026-07-13 diagnosis): the manifest installs the repo's pinned toolchain via rustup in a network-enabled build step; full build verified on a Linux dev machine.** Background (kept because it explains why no SDK-extension shape can work): `libs/fauna-core` needs rustc ≥1.85 (edition2024) and a transitive dep (`netwatch`) needs ≥1.91, but a bare `sdk-extensions` entry resolves against the GNOME SDK's own numeric branch ("46"/"47"), where no `org.freedesktop.Sdk.Extension.rust-*` ref exists — and no GNOME SDK to date (46, 47, 50 — three releases over a year) pins an extension-point version for `rust-stable`, so the extension route is structurally dead, not version-laggy. The decision: **(c) rustup honoring `rust-toolchain.toml`** beats (b) bare-freedesktop-SDK-with-GTK-modules because the manifest's `dir` source already requires sandbox network for cargo's crates.io fetch (so (b) saves no network dependency while adding a permanent GTK4/libadwaita module-maintenance tax), and beats (a) waiting because three GNOME releases show no trend toward the pin materializing. Bonus over every alternative: the Flatpak now builds with the **exact pinned nightly** every dev machine and CI use, retiring the stable-vs-nightly-pin deviation the old manifest carried. **Verified 2026-07-14 on a Linux dev machine:** full `flatpak-builder` run — rustup installs `nightly-2026-05-20` (1.97.0-nightly) from the repo's `rust-toolchain.toml`, 821 crates compile in ~4.5 min, appstream composes, and the export carries `/app/bin/fauna-desktop` (75 MB stripped aarch64 ELF). The first-ever successful build also surfaced a latent packaging bug: flatpak refuses to export icons not named after the app-id, so `fauna.svg` was silently dropped — the manifest now installs it as `social.fauna.fauna.svg` and rewrites the shared desktop entry's `Icon=` at build time (see § 3). **The 2026-07-13 "intermittent rofiles-fuse error" is root-caused, not intermittent:** sandboxed tool environments cannot FUSE-mount; `flatpak-builder --disable-rofiles-fuse` disables the optimization and builds deterministically — use it whenever `fusermount3: Permission denied` appears (a normal terminal needs nothing). Also pass `--state-dir` on the same filesystem as the build dir when building outside the repo root.
- **Runtime bumped 46 → 50 (2026-07-14):** the first successful install printed flatpak's EOL warning — GNOME 46 has been unsupported since 2025-04 (no GTK/adwaita security fixes). The old manifest was pinned to 46 by its SDK rust extension version-coupling; the rustup toolchain (above) removes that coupling, so the manifest now tracks the current supported GNOME branch. Policy: bump `runtime-version` when GNOME EOLs the pinned branch — it is a one-line change plus a rebuild.
- **Desktop-entry unification lean — partially closed 2026-07-13:** the AppImage's `fauna-linux.AppDir/fauna-linux.desktop` referenced a binary (`fauna-linux`), icon (`social.fauna.fauna`), and WM class (`fauna-linux`) that don't exist — none of the three matched the real `fauna-desktop` binary/`fauna` icon. Renamed to `fauna-desktop.AppDir/fauna-desktop.desktop` with `Exec=fauna-desktop`/`Icon=fauna`/`StartupWMClass=fauna-desktop`, matching the already-correct shared Flatpak/Debian entry; `install.sh`'s `fauna.desktop` also gained the missing `Chat;` category. **Still open:** three *filenames* still ship (`fauna.desktop`, `social.fauna.fauna.desktop`, `fauna-desktop.desktop`) as independently-maintained copies, not one generated/shared source — Flatpak's naming is constrained by its app-id so full single-file convergence may not be worth it; revisit only if a fourth copy drifts.

## Goal

Linux desktop distribution spans five complementary channels: a shell-script installer that auto-selects between root and user-local prefixes, a strict-confinement Snap on `core24`, a Flatpak under the current supported `org.gnome.Platform` branch (50 today; the manifest tracks GNOME's support window — 46 went EOL 2025-04), a Debian package, and a self-contained AppImage. All channels install the same files to the same relative paths under their prefix, support x86_64 and aarch64, target GTK 4.12+ / libadwaita 1.5+, and preserve user data on uninstall. The Linux nest server has its own separate installer, owned by `linux-nest.md`.

## Distribution Methods

### 1. Shell Script (`install.sh`)

Source: `apps/fauna-linux/install.sh`

The script detects whether it is running as root and selects the install prefix automatically. A custom prefix can be forced with an environment variable.

| Invocation | Prefix |
|---|---|
| `./install.sh` | `~/.local/` |
| `sudo ./install.sh` | `/usr/local/` |
| `PREFIX=/opt/fauna ./install.sh` | `/opt/fauna/` |

Installs the binary, `.desktop` file, and SVG icon, then updates the icon and desktop caches.

Uninstall: `./install.sh --uninstall` or `./uninstall.sh`

### 2. Snap

Source: `apps/fauna-linux/packaging/snap/snapcraft.yaml`

| Property | Value |
|---|---|
| Base | core24 |
| Confinement | strict |
| Extension | GNOME |

Permissions: `network`, `network-bind`, `home`, `desktop`, `desktop-legacy`, `wayland`, `x11`, `unity7` (the launcher badge — `apps/linux.md` § Home-screen widget: snapd's `unity7` interface is what admits the `com.canonical.Unity.LauncherEntry.Update` broadcast from `/com/canonical/unity/launcherentry/<digits>`; auto-connected on classic desktops, unverified here like the rest of this channel)

Build dependencies: `libgtk-4-dev`, `libadwaita-1-dev`, `libsqlite3-dev`

Build: `snapcraft` (no `just` recipe exists for Snap)

Sync agent on this channel: `bin/fauna-sync-agent` ships in the snap
(`override-build` addition), and the app (detecting `SNAP`) runs it as a
**direct child** — app-lifetime residency, the sandbox interim (Flatpak
graduated to its always-on host-unit seam 2026-07-20; § Installation Files):
strict confinement hides `~/.config` dotfiles from the `home` interface and
offers no user-unit control surface. The snap-native always-on shape, not yet
built: a second `apps:` entry with `daemon: simple` + `daemon-scope: user`,
gated behind snapd's experimental user-daemons flag — adopt when snapd
stabilizes it. **Gate re-verified 2026-07-20: still experimental as of snapd
2.75 (2026-04)**, and shipping behind it would also require every user to run
`snap set system experimental.user-daemons=true` — a works-out-of-the-box
violation — so the interim stands until snapd graduates the flag. (No socket
change is needed when it does: snapd already rewrites `$XDG_RUNTIME_DIR` to
the per-snap `/run/user/<uid>/snap.<name>`, which a user-daemon agent and the
app both see.)

### 3. Flatpak

Source: `apps/fauna-linux/packaging/flatpak/social.fauna.fauna.yml`

| Property | Value |
|---|---|
| App ID | `social.fauna.fauna` |
| Runtime | `org.gnome.Platform` — the current supported branch (50 today; bump when GNOME EOLs it) |
| Toolchain | rustup in a network-enabled `build-commands` step, honoring the repo's `rust-toolchain.toml` pin — the same toolchain as every dev machine and CI (decided 2026-07-14; replaces the SDK rust extension, which structurally cannot reach a new-enough rustc — see § Implementation status) |

The build sandbox runs with `--share=network` (rustup download + cargo's
crates.io fetch — the manifest's `dir` source vendors nothing). This is fine
for the self-distributed bundle channel this doc specifies, but **Flathub
would reject it**: publishing there would need vendored cargo sources
(`flatpak-cargo-generator`) and a toolchain the sandbox can reach offline —
a separate deliberate track if Flathub ever becomes a goal. The manifest also
installs the icon under the app-id name and rewrites the shared desktop
entry's `Icon=` line at build time (flatpak exports only app-id-named icons;
the deb/install.sh copies stay `Icon=fauna`).

Permissions: `wayland`, `x11`, `ipc`, `network`, `gpu`, D-Bus (notifications, status notifier, **Secret Service** — credential storage fails `ServiceUnknown` without it, and `com.canonical.Unity` for the launcher badge — `apps/linux.md` § Home-screen widget; a `--talk-name`, not a name grant, so the no-session-bus-name-grants decision below stands), filesystem (`xdg-config/fauna`, `xdg-data/fauna`)

**No session-bus name grants — and that is a decision, not an omission (2026-08-22).** Until
the app-id convergence this manifest carried three: `--own-name=social.fauna.desktop` plus the
wildcard pair `--own-name=social.fauna.desktop.*` / `--talk-name=social.fauna.desktop.*`. All
three existed for one reason — the GtkApplication id differed from this Flatpak's app-id, so
the sandbox D-Bus proxy refused `g_application_register()` and every per-account raise name
without being told. With `APP_ID` now spelled `social.fauna.fauna`, the *same string* as
`app-id`, Flatpak's default session-bus policy already admits `$FLATPAK_ID` **and its
subnames** ([sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)),
which covers both the app-wide name and the per-(OS login, account) raise channel
`social.fauna.fauna.a<token>` — a subname by construction (`account-scoping.md` § Concurrent
instances owns the mechanism, this doc owns the grant). ⚠ Re-adding any of the three is a
signal the ids have drifted apart again; fix the id, not the manifest. Pinned by
`packaging_identity_test.rs::the_manifest_carries_no_redundant_grants_for_its_own_id`.

Confinement caveat (verified by the first sandboxed run, 2026-07-14): a sync
folder configured by a non-Flatpak install at an arbitrary host path is not
visible inside the sandbox — the engine logs a reconcile/watcher error for
that set and skips it. Folders picked *inside* the Flatpak go through the GTK
file-chooser portal; whether those grants persist across restarts for the
always-resident engine is unverified — check before declaring file-sync
supported on this channel. The tray registers via the sandbox-safe unique-name
path (`ksni::spawn_without_dbus_name`, keyed on `FLATPAK_ID`).

Sync agent on this channel: `/app/bin/fauna-sync-agent` ships in the bundle,
and the app keeps it **always-on** via the host-unit seam (built 2026-07-20):
a host systemd user unit exec'ing
`flatpak run --command=fauna-sync-agent social.fauna.fauna`, written from the
sandbox through `--filesystem=xdg-config/systemd/user:create` (the grant
mounts the host's `~/.config/systemd/user` at its host path — the sandbox's
own `XDG_CONFIG_HOME` is app-private, so the writer targets
`$HOME/.config/systemd/user` directly) and enabled/started over the user
manager's D-Bus API via `--talk-name=org.freedesktop.systemd1`
(Reload → EnableUnitFiles → StartUnit — `systemctl` binary calls don't cross
the sandbox). The agent socket relocates to the shared
`$XDG_RUNTIME_DIR/app/<app-id>` subdir — the one runtime dir flatpak shares
between instances of the same app-id — so the app's sandbox instance reaches
the agent's (`fauna_ipc::unix_transport::default_socket_path`, keyed on
`FLATPAK_ID`; the `InstanceLock` rides the socket path, so the
single-instance guard relocates with it). The unit's
`ConditionFileIsExecutable` points at the deployed agent binary under the
installation's stable `current/active` link, derived from `.flatpak-info`'s
`app-path` — uninstall-inert and update-proof (§ Installation Files). If any
step fails (grants absent on an install with stale overrides, no readable
`app-path`), the app falls back to the pre-seam interim: the bundled agent as
a **direct child**, app-lifetime residency. Implementation:
`apps/fauna-linux/src/sync_agent.rs` (`LaunchChannel::FlatpakUnit`).
Caveat, same as the folder-portal one above: whether host-side `flatpak run`
re-grants portal-picked folder access to the agent's instance across restarts
is unverified — check before declaring file-sync fully supported here.

| Task | Command |
|---|---|
| Build | `just linux-flatpak` |
| Install | `just linux-flatpak-install` |
| Run | `just linux-flatpak-run` |
| Export bundle | `just linux-flatpak-bundle` (produces a `.flatpak` file) |

### 4. Debian Package (`.deb`)

Source: `apps/fauna-linux/packaging/debian/` — **fixed + verified 2026-07-12; see § Implementation status today.**

`debian/control`:

| Field | Value |
|---|---|
| Package | `fauna-desktop` |
| Architecture | `any` (resolved from the build host at package time — not a hardcoded `amd64`; the fleet's dev machines are aarch64) |
| Depends (runtime) | `${shlibs:Depends}`, `${misc:Depends}`, `libgtk-4-1`, `libadwaita-1-0`, `fuse3` — the setuid `fusermount3` the agent execs for unprivileged mounts (`../../behavior/on-demand-files.md` § Linux FUSE binding); `install.sh` checks for `fusermount3` and names the package when missing — a note, never a failed install, since on-demand is a choice |
| Build-Depends | `debhelper-compat (= 13)`, `pkg-config`, `libgtk-4-dev`, `libadwaita-1-dev` (no `cargo`/`rustc` — the pinned nightly toolchain comes from rustup, not apt) |

Build: `just linux-deb` (requires `debhelper`; copies `debian/` to a disposable directory at the repo root — a real copy, not a symlink, per § Implementation status today — then `dpkg-buildpackage -us -uc -b`)

### 5. AppImage

Source: `apps/fauna-linux/packaging/appimage/`

Self-contained — runs on most distributions without installation. The bundled `AppRun` script sets `LD_LIBRARY_PATH` and `GDK_PIXBUF_MODULE_FILE` before launching the binary, and dispatches `--sync-agent` to the bundled `usr/bin/fauna-sync-agent` — the systemd user unit written by the app under an AppImage install execs the stable `.AppImage` file itself with that flag, because the per-run FUSE mount path is throwaway (§ Installation Files). Staging = `AppRun` + the `.desktop`/icon + both binaries under `usr/bin/`, packed with `appimagetool` (installed per the Linux dev-machine setup doc); the tier_3 fixture in `tests/e2e-unified/tests/platform/linux/test_installer.py` stages the same layout. No `just` recipe — the 2026-07-12 Snap rationale (no recipe while the channel has no CI/dev-loop consumer) applies here too.

## Installation Files

All methods install to the same relative paths under `$PREFIX`:

```
$PREFIX/bin/fauna-desktop
$PREFIX/share/applications/<desktop-entry>
$PREFIX/share/icons/hicolor/scalable/apps/fauna.svg
```

**Every method also ships `$PREFIX/bin/fauna-sync-agent`** (ratified 2026-07-18; shipped 2026-07-19) — the per-user sync+backup agent. The *app* writes + enables its systemd **user** unit (`fauna-sync-agent.service`) at first post-auth, the same universal-hook shape as the autostart `.desktop`; packages install the binary only, never a system unit. Lifecycle owner: `../apps/sync-agent.md` § Packaging. The five channels reduce to three lifecycle shapes (the app detects its channel from `FLATPAK_ID`/`SNAP`/`APPIMAGE`):

| Channels | Unit `ExecStart` | Residency |
|---|---|---|
| install.sh, deb (+ dev tree) | the `fauna-sync-agent` binary beside the app (quoted absolute path) | always-on user unit |
| AppImage | the stable `.AppImage` file itself with `--sync-agent` (`AppRun` dispatches; the per-run FUSE mount path is throwaway). Moving the file self-heals on next app launch | always-on user unit |
| Flatpak | `/usr/bin/flatpak run --command=fauna-sync-agent "social.fauna.fauna"` — a **host** unit the sandboxed app writes through its `xdg-config/systemd/user:create` grant and drives over the user manager's D-Bus API (§ Flatpak). The app-id is quoted (since 2026-08-02): it comes from `$FLATPAK_ID`, and unquoted a bare space in it appends further `flatpak run` arguments | always-on user unit (app-child fallback when the grants are missing) |
| Snap | none — strict confinement offers no user-unit surface; the app direct-spawns the bundled agent as a child | **app-lifetime (interim)** — § Snap for the snapd-gated always-on shape |

**On-demand (FUSE) availability per channel (built 2026-10-04 with the linux app's switch).** The agent's on-demand root needs an openable `/dev/fuse` and a `fusermount3` on PATH (the `fuse3` package). install.sh, deb and AppImage have both on an ordinary desktop; Flatpak and Snap run the agent inside a sandbox with neither, so on those channels the agent's boot probe reports on-demand unavailable and the apps render the mode toggle disabled with that reason. Owner of the probe and the rule: `../../behavior/on-demand-files.md` § Linux FUSE binding.

Every written unit carries **two guards on the same `<target>`** — `ConditionFileIsExecutable=<target>` (in `[Unit]`) **and** `ExecCondition=/usr/bin/test -x "<target>"` (in `[Service]`) — the **uninstall story for channels whose uninstaller can't reach per-user unit files**: a `dpkg -r`, a root `install.sh --uninstall`, a deleted `.AppImage`, or a `flatpak uninstall` cannot remove the unit, so it instead goes condition-skipped — silent — the moment its target disappears, and self-heals when a reinstalled app's ensure path next runs. **Both lines are load-bearing because they fire on different start paths** (verified against the live systemd user manager — `tests/e2e-unified/tests/real_session/`): `ConditionFileIsExecutable` is checked on an *initial* / explicit / boot start, but systemd does **not** re-evaluate `Condition*=` on a `Restart=always` **auto-restart** — and `RestartSec=5` × the default `StartLimitBurst=5` spans 25 s, past the 10 s `StartLimitIntervalSec` window, so the start limit never trips. So without `ExecCondition` an uninstall that kills the *running* agent (Flatpak's `flatpak uninstall` SIGKILLs the instance; a native binary's deletion catches the agent's next exit) would Restart-flap **forever**, each auto-restart re-running the doomed `ExecStart` every 5 s. `ExecCondition` re-runs on every start attempt *including* auto-restarts, so each one condition-skips (journal: "Skipped due to 'exec-condition'") and the unit settles cleanly `inactive`. The target is the exec binary itself on the native/AppImage channels; on Flatpak — whose exec target `/usr/bin/flatpak` survives an uninstall — it is instead the deployed agent binary under the installation's **stable** deploy link (`…/app/social.fauna.fauna/current/active/files/bin/fauna-sync-agent`, derived from `.flatpak-info`'s per-commit `app-path`; the stable link survives `flatpak update`, where a per-commit path would condition-skip a healthy install until the next app launch healed the unit).

**No unit is composed from a value that could invent a directive (invariant since 2026-08-02).** A unit file is a
*structured* format whose record separator is the newline, and all three `ExecStart` forms above are
built by interpolating environment-derived values — so unit composition treats every such value as
untrusted input. The shared `UnitExec` (`libs/fauna-client-sync`
`agent_spawner.rs`) is **valid by construction**: private fields, fallible constructors, and any
control character in either the exec value or the condition path is **refused** — no unit is written
at all, plus a loud log — rather than sanitized, because a stripped path would still be written and
then compared against the honest one by the ensure path. Fencing at the composition point (not
per-caller) is what makes the guarantee hold for the next input someone routes in. That the quoting
in the table above defends only the *argument* axis, never this one, is the reason it is stated
separately. Pinned by mutation-graded tests in `agent_spawner.rs`; found in a security review,
and its narrower sibling — the agent pin's own presence in release artifacts — is
`e2e-automation-surface-gating.md` convention 15.

**ONE desktop entry ships, and its basename IS the app id** — `apps/fauna-linux/packaging/social.fauna.fauna.desktop`, installed under that name by every channel (Flatpak, deb, `install.sh`). Converged 2026-08-22 with the app-id rename: before it, `install.sh` and the deb shipped a second, separately-maintained `fauna.desktop`, which is why the metainfo's `<launchable type="desktop-id">social.fauna.fauna.desktop</launchable>` had **never resolved on the deb channel** — a software-centre entry with no launch button, reported by nothing. The merged entry carries the union of both predecessors: `Exec=fauna-desktop %u` and `MimeType=x-scheme-handler/fauna;` (the URL-scheme handler, from the `install.sh` copy) plus `StartupWMClass=fauna-desktop` and `Terminal=false` (from the packaging copy). The Flatpak channel still rewrites `Icon=fauna` → `Icon=social.fauna.fauna` at build time, because flatpak exports only app-id-named icons; the deb/`install.sh` copies stay `Icon=fauna`.

⚠ The autostart entry is deliberately NOT renamed — `autostart.rs` writes its own `$XDG_CONFIG_HOME/autostart/fauna.desktop` from a string literal, it is never installed from a packaged file, and the shell does not match windows against autostart entries. Renaming it would strand the enabled entry of every existing install.

Basename↔id agreement is pinned by `packaging_identity_test.rs::the_desktop_entry_basename_and_the_launchable_are_the_app_id`.

## Versioning

The snap's `version:` (and any version a channel stamps on its artifact) is the fleet-wide **product version**, sourced from the root `Cargo.toml` — owner [`../product-version.md`](../product-version.md); the `version-lockstep-check` merge gate holds the copies equal. The metainfo's `<releases>` list is the other class that doc defines: a shipped-release history, appended at release time, whose newest entry may trail the tree version but never lead it.

## AppStream Metadata

File: `social.fauna.fauna.metainfo.xml` — descriptions, screenshots, and release notes for graphical software centers (GNOME Software, KDE Discover). The `Network;InstantMessaging;Chat;` categories live in the `.desktop` files, not the metainfo (which carries no Categories tag).

## Platform Support

### Distributions

| Distribution | Supported methods |
|---|---|
| Ubuntu 22.04+ | deb, snap, flatpak |
| Fedora 38+ | flatpak, AppImage |
| Arch Linux | flatpak, AppImage, manual |
| Debian 12+ | deb, flatpak |
| Other GTK4 distros | flatpak, AppImage, manual |

### Architectures

| Architecture | Supported |
|-------------|-----------|
| x86_64 (amd64) | Yes |
| aarch64 (arm64) | Yes |

Runtime requirements: GTK 4.12+ and libadwaita 1.5+ (`apps/fauna-linux/Cargo.toml`'s `adw` crate builds against the `v1_5` binding feature — `adw::Dialog`, used by the folder creation wizard, was added in libadwaita 1.5).

## Uninstall

| Method | Command | Sync-agent user unit |
|---|---|---|
| Shell script | `./install.sh --uninstall` or `./uninstall.sh` | **user-prefix run: removed** (`disable --now` + unit file + daemon-reload — the uninstaller runs *as* the unit's user, the one channel that can); root run: other users' units go condition-inert |
| Snap | `snap remove fauna` | none written (sandbox interim — app-child residency) |
| Flatpak | `flatpak uninstall social.fauna.fauna` | remains, condition-inert (flatpak has no uninstall hooks that could reach `~/.config/systemd/user`; the unit's condition target is the installation's `current/active` deploy link, which the uninstall removes) |
| Debian package | `apt remove fauna-desktop` | remains per-user, condition-inert (root cannot reach user homes) |
| AppImage | Delete the `.AppImage` file | remains, condition-inert (the unit's exec target *is* the deleted file) |

"Condition-inert" = the unit's condition guards no longer hold, so systemd skips every start silently — including `Restart=always` auto-restarts, which only the `ExecCondition` guard covers (a bare `ConditionFileIsExecutable` is not re-checked on auto-restart, so on its own it would let a running-at-uninstall unit Restart-flap forever — see § Installation Files); a reinstall self-heals it (§ Installation Files). User data in `~/.local/share/fauna/` and `~/.config/fauna/` is preserved by all uninstall methods — a leftover inert unit file is lifecycle wiring, not user data.

## Nest Server Installer

`bins/fauna-nest/install.sh` installs fauna-nest as a systemd system service for self-hosting — owner: `linux-nest.md` (install path, unit shape, layout, claim code).
