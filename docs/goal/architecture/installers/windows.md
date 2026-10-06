# Installer: Windows — target state

Owns: windows-packaging, windows-services
Status: ratified
Authority: Windows distribution — the WiX v6 MSI (feature tree + defaults, per-arch x64/ARM64 packages, major-upgrade/uninstall semantics incl. REMOVE_USER_DATA, `fauna://` registration, shell-extension packaging/registration + the Path-1 side-by-side upgrade contract, the signing plan (Store-signed MSIX + SignPath-signed MSI), dist-profile size policy) and the Windows process model (FaunaNest/FaunaBridge machine services under virtual accounts + failure recovery, the per-user sync logon agent, `device.toml` as Nest↔Bridge artifact-set IPC, FaunaBridge→MDA supervision); defers the app architecture + shell-ext COM behavior to architecture/apps/windows.md (§ Shell Extension, § App Lifecycle), transport trust to architecture/security.md § Transport trust, per-user credential storage to architecture/long-term-store.md, and build runbook mechanics to `apps/fauna-windows/installer/README.md`.

Last verified: 2026-09-09 (docs-consistency sweep — re-checked signing status (still no cert; Store MSIX + SignPath, unchanged), sparse-package CAs (`Package.wxs` `RegisterSparsePackage`/`DeregisterSparsePackage` intact), `fauna-ctl` absence, the 9 `.wxs` file count, the 79-test structural-test count, the `dist` cargo profile's single root-`Cargo.toml` location, and the product-version lockstep value (`0.1.2`) — all still held. **One drift found and fixed:** `release.yml` moved to the public repository on 2026-08-31 (a GitHub-hosted `windows-latest` runner now builds `build-windows` on a `v*` tag push, replacing the never-run internal `workflow_dispatch`-only copy), but this doc's Implementation-status and Testing sections still described the old, deleted internal workflow as disabled with no runner configured — the doc's own dependents list in `../release-integrity.md` § *When a release workflow publishes* omitted this file when the port landed. Fixed here.) | Source: `apps/fauna-windows/installer/`

## Goal

A single WiX v6 MSI installer covers Windows distribution end-to-end: feature selection for the per-user sync agent + the nest/bridge machine services, the WinUI 3 desktop app, and Explorer shell integration; per-architecture packages for x64 and ARM64; standard MSI major-upgrade semantics that preserve user data; the machine services running under per-service Windows virtual accounts (sync is a per-user logon agent, not a service); `fauna://` URI handler registration.

## Implementation status today

Read this first when scoping installer work. **Current frontier (2026-07-09): every target property below is at least built and in source; most are field-proven on real Windows hardware, but not all — a handful (Windows Firewall inbound allow, headless nest-only install, MSI major-upgrade runtime) remain authoring-guarded only, pending a live run, not blocked (§ Testing → Coverage gaps). Two items are unimplemented outright, not merely unverified:** (1) **Signing — RE-SCOPED (USER 2026-08-05; hardened 2026-08-10: ALL Azure usage dropped).** No public-trust certificate is purchased and no signing path uses Azure — Azure Trusted Signing / Artifact Signing is cancelled (every public-trust cert path now requires gov-ID verification of a human applicant, judged too privacy-invasive; and since 2024 no cert grants instant SmartScreen reputation — it accrues per file regardless). Two channels instead: **(a) Microsoft Store (MSIX) — the primary channel**: the Store signs the package itself, so there is no certificate to manage and Store installs never hit SmartScreen; prerequisites are a one-time Partner Center company enrollment (user-side; document/registry-based org verification, no gov-ID — **DONE 2026-08-22, with the app name reserved and the package identity captured**; § Identity & coexistence) and a **Store-ready MSIX package** — a real packaging track, since today's artifacts are the full-feature MSI and the identity-only sparse MSIX. **The packaging design is RATIFIED 2026-08-10 — § Store distribution (MSIX) below** settles the formerly-open questions (feature subset: Desktop App + per-user sync agent + shell-ext context menu, machine services stay MSI-only; identity coexistence with the sparse package; MSI→Store data continuity); build slices tracked internally — nothing of it is built yet. **(b) SignPath Foundation — signs the direct-download MSI**: free open-source code signing (certificate subject reads "SignPath Foundation"), gated on the public source publication; eligibility is OSI license + public codebase + released artifacts + active maintenance, and signing runs through SignPath's managed pipeline against the public repo's CI, verified `signtool verify /pa`. (2) **`release.yml` lives in the public repository**; it triggers on a `v*` tag push or a `workflow_dispatch` naming one, and `build-windows` runs on a GitHub-hosted `windows-latest` runner for both x64 and ARM64 (owner: [`../release-integrity.md`](../release-integrity.md) § *When a release workflow publishes*).

| Property | Status | Evidence |
|---|---|---|
| **Network-reachable nest** — `FaunaNest` binds `0.0.0.0:443` HTTPS off the always-live self-signed floor cert, channel-bound (`served_cert_spki` → `fauna.auth.handshake`); reached as `test@<ip>` via TOFU + channel-binding (architecture/security.md § Transport trust); clear `WSAEADDRINUSE` messaging on a `:443` bind conflict | Built + verified on Windows 2026-06-20 | the service consumes the shared `fauna_nest::desktop_serve` sequence (floor TLS via `self_signed_cert::listener_tls_from_floor`, floor-renew + cert-watcher tasks — one path shared with the macOS service, priority #2); tier_3 `test_onboarding_self_signed_probe.py` |
| **Windows Firewall inbound allow** (TCP 443, rides the Nest component — installed/removed with the feature) | In source, authoring-guarded | `ServiceNest.wxs` `firewall:FirewallException`; `TestFirewall` |
| **Headless nest-only install** — Sync optional (`AllowAbsent`), `DataCleanup` re-homed under both features; **`fauna-ctl` removed from the MSI** (USER-ratified 2026-06-20: the only config surface is installer + app) | In source, authoring-guarded | `Package.wxs`; `test_installer_structure.py` (sync deselectable, DataCleanup dual-ref, no `FaunaCtlExe`) |
| **Same-box app default URL** `https://127.0.0.1:3000` — the fixed internal-loopback co-located-IPC port (`CANONICAL_INTERNAL_LOOPBACK_PORT`), not the movable external serving port (nest/common.md § Same-box reach) — + C# self-signed-floor trust | Built 2026-06-20; **live-verified 2026-07-11** (loopback branch) + **2026-07-12** (SPKI-pin branch) — no residue | the 4 `App.xaml.cs` default sites (`DefaultNestPort = 3000`); the `DirectNestClient` trust-callback mechanism is owned by architecture/apps/windows.md. Two tier_3 suites drive a real FaunaApp against a nest serving its self-signed floor over real HTTPS and prove the C# residual-HTTP legs land — **health** (`IsAvailableAsync`) and **blob upload + download** (`UploadBlobAsync` / `GetBlobAsync`, i.e. media) — on **each** branch of the trust callback: `test_self_signed_nest_client_legs.py` (`self_signed_nest`, loopback → the same-box install) and `test_spki_pinned_nest_client_legs.py` (`spki_pinned_nest` — the same floor cert dialled on the box's **LAN IP**, so the authority is non-loopback and the cert is not WebPKI-valid for it, leaving `spki == pin` as the only term that can accept it → the remote `test@<ip>` case, proven by elimination). Trust-decision detail: architecture/security.md § Transport trust. |
| **Claim-code bootstrap** — minted at first service start into `%ProgramData%\Fauna\nest\claim-code` (a service has no console: the file is the conveyance artifact, redacted from logs) | Built + verified 2026-06-20 | `claim::ensure_claim_code_at`, consumed via the shared `desktop_serve` sequence; the fully-headless zero-box-access conveyance edge is a follow-on, not a blocker |
| **Per-user sync agent** — no `FaunaSync` machine service; per-user HKLM `…\Run` launch of `fauna-sync-agent.exe` (standard token, GUI-subsystem binary); per-session pipe provisioning from the app's vault; per-user state in `%LocalAppData%\Fauna\sync\`; `device.toml` scoped to Nest↔Bridge IPC | In source + tier_3 green 2026-06-20 | `ServiceSync.wxs`; `test_no_faunasync_service`, `test_sync_logon_run_key`, `TestSyncAgentSubsystem`, `test_per_user_sync_agent.py` (2/2) |
| **Bridge feature ships + supervises the Go MDA** (CalDAV + IMAP; stages `fauna-mail-bridge.exe` + gnullvm `fauna_ffi.dll` + `libunwind.dll`; `release.yml` cross-compiles the cgo MDA) | Built; binaries-level AND installed-level e2e GREEN | `ServiceBridge.wxs`; `mda_supervisor.rs`; `test_caldav_imap_serving.py`; `TestCalDAVImapServingAfterInstall` (1 passed, 27 s — Windows 2026-06-19; surfaced + fixed a WiX Bridge-feature bug and 3 `_build_msi` bugs) |
| **`KillFaunaApp` files-in-use CA** — `[INSTALLFOLDER]`-scoped, **force-only**, immediate CA **after** `InstallValidate` (dev-box-safe by path-scoping): the built-in Restart Manager closes cooperating current-version apps at `InstallValidate` (graceful session-end persists drafts + relaunches via `RegisterApplicationRestart`), so this CA is the pure last-resort net for the windowless / cross-session strays RM can't close | In source + field-proven 2026-06-19; softened to force-only-after-`InstallValidate` 2026-07-15; **RM-softening live-verified on real elevated Windows 2026-07-16** — an over-install (`msiexec /fa`) of a running current-version FaunaApp is closed by the built-in RM *at* `InstallValidate` (`/l*v`: *"RESTART MANAGER: Successfully shut down all applications … that held files in use"*), the unsaved feed-compose draft persists to `drafts.json`, the app relaunches, and `KillFaunaApp` runs right after (seq 1401) as the net with nothing to force-kill; the force-net **body itself** is validated directly (the exact CA PowerShell force-kills `[INSTALLFOLDER]`-scoped `FaunaApp.exe` instances and stays reaped, path-scoped so a build-dir / e2e app is left alone). **Finding:** RM closes *any* FaunaApp with a visible top-level window — cooperative, `FAUNA_E2E_BRIDGE`, and even `MSIRESTARTMANAGERCONTROL=Disable` (not honoured on the command line) alike, verified 3× — so the net is exercised only by genuinely *windowless* (tray-hidden pre-Slice-2c) / cross-session strays; full results in the RM-softening plan | `Package.wxs`; the app-side cooperation that lets RM go first (single-instance, persist-on-close, Restart Manager — Slice 2c) is owned by architecture/apps/windows.md § App Lifecycle |
| **Shell-ext Path-1 upgrade** (side-by-side version-stamped DLL, Explorer never closed) + **`CleanShellDlls` orphan cleanup** | Built + live-verified on real Windows hardware 2026-06-26/27 | § Shell Extension § Upgrade handling; `TestShellExtUpgradeSafety` |
| **`CleanSyncRoots` uninstall CA** — unregisters every persistent `Fauna!*` cfapi sync-root registration (filter + shell/`SyncRootManager`) on uninstall, so Explorer never keeps rendering a ghost cloud location; immediate CA (impersonated, non-elevated — the same context `fauna-sync-agent.exe` itself always runs in), reuses `fauna_cfapi::list_shell_sync_roots` + the product's own ghost-janitor primitives via a new `fauna-sync-agent.exe --cleanup-roots` flag | Built + live-verified on real Windows hardware 2026-07-18 (elevated install → seeded a real shell+filter registration → uninstall → both entries confirmed gone, folder + file intact → reinstall) | `Package.wxs`; `fauna-sync-agent::cfapi_host::unregister_all_shell_sync_roots` (lifted from the windows-only sync service into the cross-platform crate 2026-07-19; `fauna-sync-agent.exe` reaches it through `fauna_sync_agent::run_main()`); `TestCleanSyncRoots` in `test_installer_structure.py`; `docs/goal/behavior/file-sync.md` § Per-file sync-status display (registration persistence) |
| **Install → uninstall lifecycle** — install succeeds, ARP registers; plain `msiexec /x` preserves `%ProgramData%\Fauna\`; `REMOVE_USER_DATA=1` deletes it recursively | Field-verified both paths (user manual test 2026-06-19) — lifecycle COMPLETE | `/l*v` logs + folder state; the x64 install/uninstall smoke is authored in `release.yml` |
| **MSI major upgrade** (runtime vN → vN+1) | Authoring-guarded only; runtime upgrade untested | `Upgrade`-table guards; § Testing coverage gaps |
| **Build recipe** — `dist` cargo profile, committed `crt-static` for `*-pc-windows-msvc`, self-contained WinUI publish, absolute `wix -d` paths, mail-bridge cross-build | Proven locally: 506-file, 86.6 MiB ARM64 MSI (2026-06-20) | `apps/fauna-windows/installer/README.md` § Building from a clean checkout; encoded in `release.yml build-windows` |

**Ruled 2026-10-01 — the MDA CalDAV listener interface is a constant, and the bridge service's `config.toml` is retired. Built.** The interface is not a choice anyone makes: a desktop nest's CalDAV listener serves the calendar and mail apps on the box's network, so it binds every interface — which the macOS supervisor has always hard-coded (`0.0.0.0` at the default port). It is now one constant, `fauna_mda_supervisor::CALDAV_LISTEN` in the shared `libs/fauna-mda-supervisor`, used by both desktop supervisors; `BridgeConfig`, its `%ProgramData%\Fauna\bridge\config.toml` and the pipe `Configure` request (with `fauna-ctl`'s `bridge configure`) are deleted. This replaces the earlier target (the interface as app-set nest state). The **port** is the admin's choice and stays one: the admin sets it from any app (`fauna.bridges.set_caldav_port` → nest state → the `/data/caldav-port` flag-file mirror the supervisor reconciles each tick, restarting the MDA on change — owner behavior/caldav-server.md); the supervisor-*written* `operator-hatch.toml` the Go MDA reads stays artifact-set IPC, never human-edited.

**Known small gaps:** per-user `%LocalAppData%\Fauna\` removal is deliberately out of installer scope (design decision 2026-06-05 — a perMachine MSI runs as SYSTEM and can't reach user profiles; delete affordances live in the user's app, and the in-app affordance is a apps/windows.md follow-on). The Add/Remove-Programs product icon gap is **fixed 2026-07-20**: `<Icon Id="FaunaIcon">` + `ARPPRODUCTICON` now point at `AppIcon.ico`, rendered from Fauna's one brand mark (`sites/fauna-social/public/favicon.svg`, via a dedicated icon-rendering script) — the same file `FaunaApp.csproj`'s `<ApplicationIcon>` uses for the .exe's own taskbar/Alt-Tab/Explorer icon. **The rendered-from claim went stale once and is now gated (2026-08-13).** Between 2026-07-29 and 2026-08-13 it was false: a mark change moved the SVG outside the subset the dev-fleet rasterizer accepted, so both this `.ico` and the MSIX logos below kept shipping the *previous* mark while the doc claimed otherwise. The rasterizer now reads `circle` / `ellipse` / paths of `M L H V C Q Z` (absolute and relative), `AppIcon.ico` + the three sparse-package logos are regenerated from the current mark, and the cheap merge gate `just brand-mark-check` blocks any future mark the rasterizer cannot read — so the claim can no longer rot silently. **Visual confirmation on real Windows is DONE (2026-08-25/26), by two independent checks.** The shipped `AppIcon.ico` was rendered through the actual OS icon pipeline (`System.Drawing.Icon`/GDI+, the same code path Explorer, the taskbar, and Add-Remove-Programs' `ARPPRODUCTICON` all use) at both its 16×16 frame (the taskbar/small-icons size the rasterizer fix targeted) and its 32×32 frame (the Add-Remove-Programs size); both read clearly as the beaver mark — round head, ears, eyes, muzzle — not a blob, teeth detail naturally lost at 16×16 but the silhouette stays legible; the three MSIX sparse-package logos (`Square44x44Logo`/`Square150x150Logo`/`StoreLogo`) were inspected directly and match. The live user then confirmed the same on the built `.exe` in Explorer/taskbar/Alt-Tab ("Looks good"), and separately flagged that the mark's current margin within its 32×32 canvas leaves room for the OS to pad the icon with a background plate (blue on Windows, light grey on macOS) and that the mark reads a little small — a canvas-fill refinement, not a rasterizer defect (the rendered `.ico`/PNG artifacts are confirmed fully transparent outside the mark's own shapes, pixel-decoded not just file-diffed), tracked in the windows work queue.

**Dated history:** first real end-to-end build 2026-06-15 (three missing `workspace.dependencies` + stale service config fixed in source; `crt-static` committed — without it a bare fresh VM hits MSI 1920 on `VCRUNTIME140.dll`; recipe blockers encoded in `release.yml`); per-user sync agent ratified 2026-06-19, e2e-guarded 2026-06-20; network-reachable nest ratified + landed 2026-06-20; `fauna-ctl` removed from the MSI 2026-06-20 (USER) and deleted with the bridge pipe 2026-10-02; install + both uninstall paths field-validated 2026-06-19; Nest feature default-OFF ratified 2026-06-21; Path 1 ratified 2026-06-25, implemented + live-verified 2026-06-26/27; Authenticode blocked on org registration recorded 2026-06-27.

## Installer Technology

The Windows installer is built with **WiX v6** (`wix` .NET global tool) and produces a standard MSI package. The `.wxs` source uses the WiX v4-era schema namespace (`http://wixtoolset.org/schemas/v4/wxs`), which the v6 toolset still consumes. Separate packages are built for x64 and ARM64 from the same source files.

### Source Files

All installer source files live in `apps/fauna-windows/installer/`:

| File | Purpose |
|------|---------|
| `Package.wxs` | Top-level package definition, product metadata, upgrade logic |
| `App.wxs` | Desktop app component (FaunaApp.exe and .NET dependencies) |
| `Directories.wxs` | Directory structure under `%ProgramFiles%` and `%ProgramData%` |
| `ServiceSync.wxs` | Sync service component and Windows service definition |
| `ServiceNest.wxs` | Nest service component and Windows service definition |
| `ServiceBridge.wxs` | Bridge service component and Windows service definition |
| `ShellExt.wxs` | Shell extension DLL, COM registration, and overlay icons |
| `Protocol.wxs` | `fauna://` URI protocol handler registration |
| `UI.wxs` | Optional user-data removal on uninstall (`REMOVE_USER_DATA`); the `DataDirRegistry` component + `RemoveFolderEx`. (Feature-selection UI itself is `WixUI_FeatureTree`, referenced in `Package.wxs`.) |
| `Strings.wxl` | Localizable UI strings consumed by the WiX UI extension |
| `wix.json` | Build manifest — the three WiX extensions (Util, UI, Firewall) the build must load |

### Build Command

The canonical, proven build recipe — the three required extensions (Util, UI, **Firewall**), every `-d` staging var (incl. the three `MailBridge*` vars `ServiceBridge.wxs` requires; **no** `CtlBin` — `fauna-ctl` no longer exists), per-arch staging, and the app-directory harvest — lives in **`apps/fauna-windows/installer/README.md` § Building from a clean checkout** (the runbook has one home; `release.yml build-windows` encodes the same steps). Both architecture packages (x64 + ARM64) build from the same `.wxs` sources; only `-arch` and the staged binaries differ.

### Size & build profile

Installer artifacts are built with a dedicated **`dist` cargo profile**, declared once in the root
workspace (`apps/fauna-windows` was unified into it 2026-07-22 — no more nested workspace to
duplicate a profile declaration into; `build-target-layout-windows.md` § *Two workspaces, twice the
compiles*): `inherits = "release"` plus `strip = true`, `lto = "thin"`, `opt-level = "s"`,
`codegen-units = 1`. The installer
build recipes pass `--profile dist` (`just windows-ffi dist`, `just windows-mail-bridge-build … dist`,
and `cargo build … --profile dist` for the service crates); the Go MDA additionally links with
`-ldflags="-s -w"` under `dist`. **Dev / test / e2e builds stay on `release`** for a fast inner loop —
the recipes' `profile` parameter defaults to `release`, so a bare `just windows-ffi` is unchanged.
The MSI cabinet uses `CompressionLevel="high"` (LZX) on `<MediaTemplate>` in `Package.wxs`.

Do **not** add `panic = "abort"` to `dist`: UniFFI's FFI scaffolding relies on `catch_unwind`.

Measured impact (ARM64, 2026-06-20) — the native binaries are ~half the MSI; the self-contained
.NET 10 + Windows App SDK runtime is the other, largely-fixed half (it stays self-contained so the
MSI runs on a bare VM with no runtime install):

| artifact | release | dist | Δ |
|---|---|---|---|
| `fauna-nest-svc.exe` (MSVC) | 53.1 MB | 38.4 MB | −28% |
| `fauna_ffi.dll` — app (MSVC) | 45.2 MB | 32.2 MB | −29% |
| `fauna_ffi.dll` — MDA (gnullvm) | 46.0 MB | 22.4 MB | −51% |
| `fauna-mail-bridge.exe` (Go) | 29.5 MB | 14.2 MB | −52% |
| **`Fauna-Setup-arm64.msi`** | **110.3 MiB** | **86.6 MiB** | **−21.5%** |

MSVC binaries shrink via LTO + `opt-level="s"` (their symbols live in unshipped `.pdb`s, so `strip` is
a no-op for them); the GNU-toolchain binaries (gnullvm dll, Go exe) shrink dramatically because `strip`
/ `-s -w` removes their embedded symbol tables on top of LTO.

## Platform Support

| Architecture | Supported |
|-------------|-----------|
| x64 | Yes |
| ARM64 | Yes |
| x86 (32-bit) | No |

Minimum supported OS: the target floor is derived from WinUI 3 / Windows App SDK requirements (Windows 10 1809+); the test reports on file cover **Windows 11 ARM64** (the Windows box), and the x64 install/uninstall smoke is authored in `release.yml`. No test report is filed for a Windows 10 install.

## Feature Tree

Users can select which components to install through the installer UI. The tree below shows the default state of each feature.

**Nest Service is default OFF** (USER-ratified 2026-06-21): the Windows desktop is primarily a *client*, so a default install is Sync Agent + Desktop App connecting to a remote nest by handle (`test@<ip-or-domain>`). The user opts into self-hosting a nest on this box by ticking **Nest Service** in the installer (or `ADDLOCAL=Nest`).

**Explorer Integration is default ON** (USER-decision 2026-06-23): overlay icons + context menus ship by default (works-out-of-the-box); a user who doesn't want them unticks **Explorer Integration** (it requires **Sync**, its parent).

**Terminal App is default ON** (2026-09-26 — the MSI carrying `fauna-tui.exe` is the user's 2026-09-26 channel ruling, [`tui.md`](tui.md) § The ratified channel; the feature placement is this doc's): `TerminalApp.wxs` ships `fauna-tui.exe` in `%ProgramFiles%\Fauna\` beside `fauna-sync-agent.exe` (the app resolves the agent as its sibling) and appends that directory to the machine `PATH`, so `fauna-tui` is typeable in any shell — the one PATH entry the package writes, removed with the feature. A **top-level** feature rather than a child of Sync or Desktop App: a headless nest box managed over SSH wants the terminal app and neither of those, and unticking either must not take it away. The Store MSIX does **not** carry it (user ruling 2026-09-26).

> **Shell-ext upgrade model — tombstone (superseded designs).** The in-place *close-Explorer-and-restart-it* designs — the `RestartExplorer` CA (with its `sihost.exe` interactive-user derivation), the **B1** `AutoRestartShell`-suppression CAs, and the **B2** `Schedule="afterInstallExecute"` experiment — were each **disproven under live RDP testing on Windows (2026-06-23 → 25): they strand the desktop** (the Restart Manager closes Explorer during `InstallValidate`, before any deferred suppression can run). They are superseded by **Path 1** (version-stamped side-by-side shell DLL; Explorer never closed; USER-decision 2026-06-25), implemented and **live-verified 2026-06-26/27** together with its `CleanShellDlls` orphan cleanup — both DONE; the sole shell-ext remainder is nothing (the old throwaway-VM gate was inherited from the strand-prone designs and does not apply to Path 1). § Shell Extension § *Upgrade handling* below is the single current story; the forensic history is tracked internally.

**Applying a shell-ext change needs only a sign-out, not a reboot** (Path 1, USER-decision 2026-06-25). Because a changed DLL ships under a *new* filename and the install never touches the in-use old file, Explorer simply loads the new DLL the next time it starts — **sign out/in, or restart Explorer**. A full reboot is **not** required (Windows would only swap a same-named *in-use* file at boot, via `MoveFileEx`/`PendingFileRenameOperations` — the mechanism Path 1 deliberately avoids, since a logout doesn't flush that queue). The previously-described live-restart machinery (`RestartExplorer` + the `AutoRestartShell` suppression CAs) is **removed** as part of Path 1 — it was proven to strand the desktop. We are **not** dropping the shell extension to dodge the installer difficulty.

```
Fauna (root)
├── Sync Agent (optional, default ON — per-user logon process, NOT a service)
│   ├── fauna-sync-agent.exe (launched at each user's logon, standard token)
│   └── Explorer Integration (optional, default ON — overlay icons; side-by-side versioned DLL, never closes Explorer on upgrade)
│       └── fauna_shell_<ver>.dll + overlay icons + context menus
├── Terminal App (optional, default ON — the same fauna-tui.exe the release archive carries; tui.md)
│   └── fauna-tui.exe beside fauna-sync-agent.exe + a machine PATH entry for %ProgramFiles%\Fauna
├── Nest Service (optional, default OFF — machine service; opt-in to self-host)
│   └── fauna-nest-svc.exe (+ Windows Firewall inbound TCP :443 allow rule)
├── Bridge Service (optional, default OFF, requires Nest — machine service)
│   └── fauna-bridge-svc.exe
└── Desktop App (optional, default ON)
    ├── FaunaApp.exe + .NET dependencies
    ├── Start Menu shortcut
    └── fauna:// protocol handler
```

**`fauna-ctl.exe` is NOT in the installer** (USER-ratified 2026-06-20). The only user-configuration surface is
the installer + the app, so the box needs no admin CLI: a headless nest-only install is configured entirely
from an app (on the same box or another machine) reached at `test@<ip>` — claim, users, mail/bridge settings,
sync folders are all app UI. (Implemented in source: the `FaunaCtl` ComponentGroup is gone from the `.wxs`;
pending a ship-rebuild + USER-gated MSI install on Windows.)

**`fauna-ctl` is deleted, and with it the FaunaBridge service's pipe (2026-10-02).** After it left the MSI,
`fauna-ctl` (a CLI that duplicated the app's config) survived as an unshipped diagnostic tool, and its bridge
subcommand was the only client of `\\.\pipe\fauna-bridge`, a named pipe the *shipped* `fauna-bridge-svc.exe`
served. That pipe was DACL'd to `BUILTIN\Users`, so every local account could reach it, and its `Shutdown`
verb was unauthenticated and stopped the service's accept loop. It was attack surface on every Nest-selected
install that served no product, so the CLI, the pipe server and its `fauna_ipc::bridge` protocol were removed
together. The FaunaBridge service serves no IPC: it supervises the Go MDA child (§ Services and the per-user
sync agent) and nothing else. A future diagnostic need is met through the app, never by a hand-run tool
([`principles.md`](../../principles.md) § One configuration surface: the apps).

The Bridge Service feature is conditioned on Nest Service being selected. If the user deselects Nest Service, Bridge Service is automatically deselected as well. The Sync Agent feature is optional (`Package.wxs` `Sync` `AllowAbsent="yes"`), so a Nest-only install needs neither Sync nor the Desktop App.

### Running a later installer over an earlier installation (feature migration)

Running a newer (or same-version) installer over an existing Fauna install is a **major upgrade**: a stable `UpgradeCode` (`4a7b8c00-f1e2-4d3a-b5c6-000000000001`) + `<MajorUpgrade AllowSameVersionUpgrades="yes" Schedule="afterInstallInitialize">`, so `RemoveExistingProducts` removes the prior product early and the new one installs in its place (single Apps-&-Features entry, no side-by-side duplicate).

**Feature selections MIGRATE across the upgrade — this is deliberate and safe (USER-ratified 2026-07-15).** WiX v6's `<MajorUpgrade>` enables feature-state migration by default: the built MSI's `Upgrade` row carries the `msidbUpgradeAttributesMigrateFeatures` bit (`Attributes=513` = `0x200` VersionMaxInclusive + `0x001` MigrateFeatures) and `MigrateFeatureStates` runs in both sequences (UI + Execute) at 1200, before `RemoveExistingProducts` (1501). The consequence a session or user must understand:

- **A feature that was installed comes up PRE-SELECTED (ticked) in the upgrade's FeatureTree**, carried over from the prior install. Clicking straight through the upgrade **keeps** every previously-installed feature — including an opt-in **Nest Service**.
- **To REMOVE a previously-installed optional feature on upgrade, the user must ACTIVELY UNTICK it** in the FeatureTree. Merely "not selecting" it does *not* remove it, because it arrives migrated-as-selected. Unticking it transitions the feature to Absent — for **Nest**, its `ServiceControl Remove="uninstall"` stops + deletes the `FaunaNest` service and removes its firewall rule (both ride the `NestService` component). There is **no separate "uninstall the nest?" confirmation dialog** — removal is part of the normal FeatureTree flow, and the old-version removal itself is silent.
- **Silent (`/qn`) upgrades** migrate features too; to drop a migrated feature non-interactively pass `REMOVE=<Feature>` (e.g. `REMOVE=Nest`).

**Why migration-on is the correct choice:** if feature states did *not* migrate, every routine app upgrade would reset features to their `Level` defaults — and because **Nest Service is default-OFF**, an upgrade would then **silently uninstall a user's self-hosted nest** (service + data-serving firewall rule) with no user action. Migrating preserves the user's explicit choices across upgrades; the only cost is that de-selecting a feature on upgrade is an explicit untick, not an omission.

## Installation Directory Layout

```
%ProgramFiles%\Fauna\        ← binaries (fauna-sync-agent.exe, fauna-tui.exe, fauna-nest-svc.exe,
│                                         fauna-bridge-svc.exe, FaunaApp.exe, fauna_shell_<ver>.dll,
│                                         overlay icon files, .NET runtime dependencies)
%ProgramData%\Fauna\         ← machine-service data (preserved on uninstall by default)
├── nest\                    ← Nest service data + device.toml (local-nest install only)
└── bridge\                  ← Bridge service data
```

User-specific data is stored under `%LocalAppData%\Fauna\` — this now includes **all per-user sync state**
(state DBs + location↔folder bindings under `%LocalAppData%\Fauna\sync\`), since sync is a per-user agent.
It is always preserved by the installer
on uninstall (including with `REMOVE_USER_DATA=1`): a `perMachine` MSI runs as SYSTEM and cannot reach
an arbitrary signed-in user's profile, so per-user data is the desktop app's to manage — consistent
with the product invariant that delete affordances live in the user's app.

## Services and the per-user sync agent

> **LANDED 2026-07-22 (was the ratified 2026-07-18 target):** the shipped agent binary is the
> cross-platform **`fauna-sync-agent.exe`**, built from `bins/fauna-sync-agent` itself (the one
> package that produces the agent on every platform — owner [`../apps/sync-agent.md`](../apps/sync-agent.md)
> § Implementation status today, A5); the launch/subsystem/pipe shape is unchanged. **How the
> MSI carries the rename across an upgrade — the two mechanisms, both load-bearing:**
> `MajorUpgrade` schedules `RemoveExistingProducts` `afterInstallInitialize`, so the old
> product's `fauna-sync.exe` **and** its `Fauna Sync` Run-key value are removed before the new
> files land (the renamed artifact also gets its own Component Id + GUID, since MSI component
> rules forbid changing an existing component's key-path file name). The `KillFaunaSync` CA matched
> the pre-A5 image name too, for an agent still *running* at upgrade time on a box coming from
> 0.1.1-or-earlier; the compat-remnant sweep removed that arm 2026-09-24 (no such install exists —
> [`../version-compatibility.md`](../version-compatibility.md) § Dimension 2, the fourth ratified
> exception), so it matches `fauna-sync-agent.exe` alone. Owner: `../apps/sync-agent.md` (milestones A1/A5).

**Sync is a per-user agent, not a machine service** (ratified 2026-06-19 — see § Device configuration).
`fauna-sync-agent.exe` runs in each logged-in user's session under that user's **non-elevated** token, launched at
logon by the installer-registered per-user launch (an HKLM `…\Run` entry or an "at logon of any user"
scheduled task, run with *standard* privileges — **not** "highest privileges"). It is the cfapi placeholder
provider and the shell-extension pipe peer for that user's session, provisioned live by the user's desktop
app (§ Device configuration). There is no `NT SERVICE\FaunaSync`. The shipped agent is linked as a Windows
**GUI-subsystem** binary (`#![windows_subsystem = "windows"]` on release/dist builds), so the per-user logon
launch shows **no console window** — OneDrive-style; debug builds keep a console for `--foreground` dev/CI. A
tier_3 authoring guard (`test_installer_structure.py::TestSyncAgentSubsystem`) asserts the *staged*
`fauna-sync-agent.exe` is PE subsystem 2, because the file-count / service / firewall checks all pass on a console
build — so a stale pre-flip artifact would otherwise ship green (the 2026-06-23 stale-binary reship).

The two **machine** services remain (each under a dedicated auto-created virtual account, granted only the
permissions it needs):

| Service | Executable | Virtual Account |
|---------|-----------|----------------|
| FaunaNest | `fauna-nest-svc.exe` | `NT SERVICE\FaunaNest` |
| FaunaBridge | `fauna-bridge-svc.exe` | `NT SERVICE\FaunaBridge` |

Both are genuine server roles — the local nest hosts every local user, and the bridge serves that nest and
requires it — so they are legitimately machine-wide. Both are configured:

- **Start type:** Automatic (starts on boot)
- **Failure recovery:** restart with a flat 5 s delay for all three restart attempts; failure count resets after 24 h

The machine services are **configured entirely from a Fauna app** (on this box or another machine, reached at
`test@<ip>` once the nest is network-reachable) — claim, users, mail/bridge settings are all app UI, the only
user-configuration surface. For raw service *lifecycle* (start/stop/restart) the box admin uses the Windows Services
snap-in (`services.msc`); there is **no admin CLI** (§ Feature Tree). The per-user sync agent is a logon process, not an SCM service — it appears in Task Manager → Startup, not
in `services.msc`.

### Device configuration — per-user sync agent + scoped `device.toml` (ratified 2026-06-19)

**Sync gets its nest connection live from the app, never from a file.** The per-user sync **agent**
(§ Services and the per-user sync agent) is provisioned at logon by that user's desktop app over the
per-session pipe with `{nest_url, device_id, capability}` drawn from the app's vault (`ISecretStore` /
Credential Manager) — the same model Linux already uses (`apps/fauna-linux/src/sync.rs` runs sync in-process
off the `ISecretStore` nest URL + bearer, with **no** `device.toml`). **The sync path reads no `device.toml`.**
Per-user sync state — state DBs and location↔folder bindings — lives under that user's
`%LocalAppData%\Fauna\sync\`, never `%ProgramData%`. This is the product invariant realized: the nest a user
syncs against is a client-side choice carried as live IPC, not a hand-editable machine config file.

**`device.toml` survives only as Nest↔Bridge machine-local IPC for the local-nest install.** When the optional
local **Nest** is installed it additionally binds a **fixed internal-loopback listener** on `127.0.0.1:3000`
(`CANONICAL_INTERNAL_LOOPBACK_PORT`) alongside its external `0.0.0.0:443`-default listener (see
*Network-reachable nest* in § Implementation status today) — a co-located-IPC port that never moves when the
admin changes the external serving port (nest/common.md § Same-box reach) — so `FaunaNest` writes
`%ProgramData%\Fauna\device.toml` on first run (`load_or_init_device_config`, seeding `nest_port = 3000`) and
the machine **Bridge** service reads it via `fauna_ipc::device::DeviceConfig::default_path()` to dial that
fixed loopback. That is pure inter-process wiring between two co-located machine services (a hard-coded
`127.0.0.1:3000` discovered at runtime), not user configuration. A **remote / no-Nest install has no
`device.toml` and no machine services** — only
per-user sync agents talking to whatever nest the app chose. (Per-user app state such as `mls.db` stays
under `%LocalAppData%\Fauna\`, owned by the interactive app, never a service.)

This resolves the two structural gaps the 2026-06-19 shell-ext live pass surfaced: a no-Nest install no longer
needs a `device.toml` writer (sync is client-provisioned), and per-user agents + per-session pipes serve
simultaneous Windows users. Design + migration ratified 2026-06-19 (tracked internally). The per-user-agent authoring is shipped (`ServiceSync.wxs` registers the Run-key launch;
no `FaunaSync` machine service exists — `test_no_faunasync_service`).

## Shell Extension

The Explorer integration component (`fauna_shell_<ver>.dll`) registers the following COM objects:

- **4 overlay icon handlers** — one each for the synced, syncing, cloud-only, and error states
- **4 context menu handlers** — root folder menu, share action, device info, and version info

A total of **8 COM CLSIDs** are registered in the Windows registry, each `InprocServer32` pointing at the version-stamped DLL. Overlay icon image files are installed alongside the DLL in `%ProgramFiles%\Fauna\`.

### Upgrade handling — Path 1 (side-by-side version-stamped filename; USER-decision 2026-06-25)

The shell-extension DLL is loaded into `explorer.exe` via COM, so **any** MSI operation that removes or replaces the in-use DLL forces its lock to be freed — which on Windows Installer 4.0+ **auto-engages the Restart Manager and closes Explorer**, stranding the desktop (no taskbar). This is independent of authoring: removing the explicit `util:RestartResource` registration does **not** stop MSI from auto-using RM for a file it touches. The earlier in-place *close-Explorer-then-restart-it* designs (the `RestartExplorer` CA; **B1**, suppressing Winlogon's `AutoRestartShell` across the swap) were each **proven under live RDP test (2026-06-23 → 25) to strand the desktop** — B1 because its `AutoRestartShell=0` deferred CA runs *after* `InstallInitialize`, but the Restart Manager closes Explorer earlier, during `InstallValidate`. Forensics tracked internally.

**Path 1 makes the in-use DLL untouchable by the installer**, so RM is never engaged and Explorer is never closed:

- **Version-stamped filename.** The DLL installs as `fauna_shell_<ver>.dll`, where `<ver>` tracks `Package/@Version` via the build-time `$(var.ShellDllName)` (`ShellExt.wxs`). A genuine change ships under a **new** filename, which installs with no lock conflict because Explorer still holds the **old** file. (Same version → same filename. Bump `$(var.ShellDllName)` and `Package/@Version` **together**; `test_installer_structure.py::TestShellExtUpgradeSafety` asserts the coupling on the built MSI, and the cheap `version-lockstep-check` merge gate asserts it pre-build.) `Package/@Version` itself is the fleet-wide **product version** sourced from the root `Cargo.toml` — owner [`../product-version.md`](../product-version.md); under its reship rule the 2026-07-17 `0.1.1` reship is a burned fleet patch number, and any future windows reship mints the next one. The MSIX derives its 4-part version from the same attribute (the store-package build script).
- **`Guid="*"` component.** The `FaunaShellDll` component GUID auto-derives from the keypath (`ProgramFiles64Folder\Fauna\fauna_shell_<ver>.dll`), so a new filename is a **distinct component** — the old and new shell DLLs are genuinely side-by-side, and the old one is merely *orphaned*, never removed.
- **`Permanent="yes"`.** MSI **never removes** the file — not on uninstall, and not when `RemoveExistingProducts` uninstalls the old product during a major upgrade. This is what guarantees the in-use old DLL is never a file MSI must free a lock for. (Required: without it, a genuine-version-change upgrade's `RemoveExistingProducts` would remove the in-use old DLL and strand the desktop.)
- **`NeverOverwrite="yes"`.** On a same-version reinstall the keypath file already exists, so `InstallFiles` does not rewrite the in-use DLL. *Dev caveat:* a changed-but-same-version shell DLL is therefore **not** applied — bump the version (or uninstall first). The shell ext changes rarely; this is the accepted cost of "skip-when-unchanged".
- **No RM / restart CAs.** `util:RestartResource`, the `RestartExplorer` CA, and the three `AutoRestartShell` CAs are removed. (`KillFaunaApp` — the unrelated FaunaApp.exe files-in-use lock — stays.)

A genuine change therefore **applies on the next sign-out / Explorer restart — no reboot** (a logout does not flush `PendingFileRenameOperations`, so we deliberately do not rely on a reboot-time same-name swap). **Implementation status:** implemented in `ShellExt.wxs`/`Package.wxs`, authoring-guarded by `TestShellExtUpgradeSafety`, and **live-verified on real Windows hardware (2026-06-26/27)** — unchanged-DLL and genuine-change upgrades both install over a running Explorer that holds the in-use DLL with **no "Files in Use" and no Explorer restart**, and the new ext activates on an Explorer restart (no reboot). (Path 1 is safe to test on the shared Windows box precisely because it never touches the loaded DLL — the old throwaway-VM gate was inherited from the strand-prone B1 approach and does not apply.)

**Orphan cleanup is implemented + live-verified.** Because the DLL is `Permanent`, a version-bump upgrade leaves the prior `fauna_shell_*.dll` and an uninstall leaves the current one. The **`CleanShellDlls`** custom action (`Package.wxs`) reclaims them in **two strand-safe tiers**: (1) it first attempts a direct **`Remove-Item`** on each stale `fauna_shell_*.dll` — a **not-currently-loaded** orphan (a leftover from a prior uninstall, or the fresh-install-over-leftovers case) is deleted **immediately, with no reboot**; a failed delete on a locked file is a harmless no-op (the installer never force-closes Explorer to free it — that would be the B1 strand). (2) For any DLL **still present** after that delete (genuinely locked — Explorer still holds the just-superseded one) it falls back to `MoveFileEx(path, NULL, MOVEFILE_DELAY_UNTIL_REBOOT)` semantics — appending the file to `HKLM\SYSTEM\…\Session Manager\PendingFileRenameOperations` (Windows deletes it at the next reboot; this only writes a registry entry, never touches the in-use file, never engages RM). In practice the user's own Explorer-restart / sign-in — done anyway to activate the new ext — releases the lock, so the practical cost of the fallback is a sign-out, never a forced reboot. It is a **deferred, `Impersonate="no"` (LocalSystem)** CA that reads the active overlay CLSID's `InprocServer32` + the filesystem and uses **no MSI properties**: on install/upgrade the CLSID names the current DLL → keep it, reclaim the rest; on uninstall the CLSID is already removed → reclaim all. WiX `RemoveFile`/`RemoveFiles` must **not** be used — it routes through the RM-aware deletion path and would re-strand. The four deferred-CA traps this design had to clear — (1) an **immediate** CA can't write `HKLM\SYSTEM`; (2) a deferred **EXE** CA's `[CustomActionData]` resolves to **empty** at runtime; (3) a **relative** exe path fails with MSI error **1314**; (4) MSI's Formatted-field processor **strips empty `{}`** so `catch{}` becomes a `catch;` parse error — are documented inline in `Package.wxs` (tracked internally). (Best-effort: the read-modify-write on the shared `PendingFileRenameOperations` is not atomic like the real `MoveFileEx`, so a concurrent updater could rarely drop the append — leaving a harmless orphan, never clobbering others' entries; `Return="ignore"`.)

### Context-menu registration — the key must match the interface (corrected 2026-07-14)

The root submenu is an **`IExplorerCommand`** handler, so it registers as a **verb** whose
`ExplorerCommandHandler` value names the CLSID:

```
HKLM\SOFTWARE\Classes\*\shell\Fauna                           ← all file types
HKLM\SOFTWARE\Classes\Directory\shell\Fauna                   ← folders (USER-decided 2026-07-16)
    (Default)              = "Fauna"                          ← fallback display text
    ExplorerCommandHandler = {4A7B8C10-F1E2-4D3A-B5C6-D7E8F9A0B1C2}
```

Both keys name the **same** CLSID — the folder menu's reduced leaf set (no per-file
version history / device info) lives in the handler, not the registration
(`apps/windows.md` § Shell Extension owns the behavior). The sparse package's manifest
already declares folder support too — `FileExplorerContextMenus` carries `*`, `Directory`,
*and* `Directory\Background` item types (`apps/fauna-windows/installer/sparse/AppxManifest.xml.in`;
pinned by `test_registers_the_verb_for_files_and_folders`, present since the manifest's
first commit 2026-07-14, ahead of the folder-menu DLL behavior it now backs). The Win11
default-menu *render* on a folder specifically is now live-observed too (2026-07-22) —
see `apps/windows.md` § Shell Extension → *Folder context menu* for current verification status.

**This is the only registry surface Explorer honours for `IExplorerCommand`.** A shell
registration key is a *contract*: it declares which interface Explorer will
`QueryInterface` for. The legacy `*\shellex\ContextMenuHandlers\<Name>` key demands
`IShellExtInit` + `IContextMenu` — which `FaunaContextMenu` does **not** implement.

Until 2026-07-14 the handler was registered under exactly that legacy key (in both the MSI
and `DllRegisterServer`), and this document ratified the contradiction: it named
`IExplorerCommand` as the interface *and* `ContextMenuHandlers` as the key. The result is
silent: Explorer `CoCreateInstance`s the class, `QueryInterface`s for `IContextMenu`, gets
`E_NOINTERFACE`, and **drops the handler with no error and no log** — so the Fauna submenu
had **never once appeared** in Explorer, in either the Windows 11 menu or the legacy "Show
more options" menu. Every in-process test passed throughout, because none asserted that the
*registered key's contract* matched the *implemented interface*. A live-Explorer pass caught
it; `registration_contract_matches_implemented_interface` (`shell-ext/src/lib.rs`) and
`test_context_menu_handler_registered_as_explorer_command` (`test_installer_structure.py`)
now pin both halves. (`register` and `unregister` also deleted the legacy key, healing a machine
carrying the broken registration; the compat-remnant sweep removed that heal 2026-09-24 — no such
machine exists, [`../version-compatibility.md`](../version-compatibility.md) § Dimension 2, the fourth
ratified exception.)

**Menu placement — the sparse package.** A verb + `ExplorerCommandHandler` surfaces in the **legacy**
("Show more options") menu only. The Windows 11 **default** context menu renders context-menu
handlers only from a component that carries **package identity** — i.e. an **MSIX / sparse-package**
`desktop4:FileExplorerContextMenus` registration, never from `HKCR`. (Normative: *Custom context
menu extensions* is listed under "features that only work in apps that have package identity" —
[Packaging overview](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/packaging/).)

Fauna therefore ships a **sparse package** — a manifest-only MSIX with
`uap10:AllowExternalContent`, registered with `Add-AppxPackage -ExternalLocation <INSTALLFOLDER>` —
whose sole job is to grant identity to the shell extension the MSI already installs:

- **Source:** `apps/fauna-windows/installer/sparse/AppxManifest.xml.in` (a **template**: `Publisher`,
  the 4-part version, and the version-stamped DLL name are substituted at build time by
  the sparse-package build script, which reads the latter two out of `Package.wxs` / `ShellExt.wxs`
  so they cannot drift). Brand logos are rendered from the single shared `favicon.svg` by
  a dedicated appx-logo renderer — MSIX requires PNG and hard-fails an unresolvable `Logo`.
- **One implementation, two registration surfaces.** The manifest's `com:Class` and every
  `desktop5:Verb` name the **existing** root `IExplorerCommand` CLSID
  (`{4a7b8c10-…}`) — the same class `ShellExt.wxs` registers as `*\shell\Fauna`. **No 9th CLSID
  exists or may be minted**; pinned by `test_sparse_package.py`, which reads the expected GUID out
  of `ShellExt.wxs` rather than restating it. The two registrations are complementary, not
  competing: the HKCR verb owns the legacy menu (and remains the only surface on Windows 10, and
  for users who force the classic menu), the sparse package owns the Win11 default menu.
- **The DLL stays outside the package.** `com:Class/@Path` resolves inside the `-ExternalLocation`
  directory, so it names the Path-1 version-stamped `fauna_shell_<ver>.dll` the MSI installs — not
  the bare build-output name. Explorer activates it out-of-process via the COM surrogate
  (`dllhost.exe`), which is what carries identity into the handler.
- **The surrogate holds a handle on the DLL.** This is the packaged twin of the known `explorer.exe`
  lock: Path 1 (`Permanent` + version-stamped `@Name`) already makes the file untouchable by the
  installer, so the lock is survivable — but **uninstall must deregister the package**, or a stale
  registration is left pointing at a deleted external location.

### Package identity for FaunaApp.exe (2026-09-26)

The sparse package's `Application` names `App\FaunaApp.exe` as its executable (Id `FaunaApp` — renamed
from `FaunaShellExt` 2026-09-26 to the Id the Store package declares, so both channels share one
AUMID suffix), but **registering the package does not by itself make the app run with identity.**
Measured on Windows 2026-09-26: a directly launched exe under a registered external-location package
reports "no package identity" (`0x80073D54`) unless the exe's own embedded side-by-side manifest carries
a matching `<msix publisher= packageName= applicationId=/>` element (Microsoft, *Grant package identity
by packaging with external location* → "Add identity metadata to your desktop application manifests").
`apps/fauna-windows/FaunaApp/FaunaApp/app.manifest.in` now carries it, so an MSI-installed FaunaApp.exe runs
with the sparse package's identity whenever the Explorer Integration feature (which ships and registers
the package) is installed. What identity unlocks first is the taskbar badge — windows' home-screen widget
([`../apps/windows.md`](../apps/windows.md) § Home-screen widget owns the surface; badge notifications are
keyed on the AUMID and refused without one). `unvirtualizedResources` in the sparse manifest keeps
`%LocalAppData%\Fauna` and the registry unvirtualized, so nothing else about the app's storage moves.
The Store package needs none of this — a full MSIX grants identity to everything inside it, and the
`msix` element is ignored there.

**Identity changes how the Windows App SDK starts.** `WindowsPackageType=None` generates an
auto-initializer that calls the bootstrapper, and the bootstrapper *fails* in a process that already has
package identity — measured 2026-09-26, FaunaApp.exe exited `0x80070032` (`ERROR_NOT_SUPPORTED`) the
moment it ran under the sparse identity. `FaunaApp.csproj` therefore sets
`WindowsAppSDKBootstrapAutoInitializeOptions_OnPackageIdentity_NoOp`, which makes the bootstrapper a
no-op under identity and leaves the identity-less path unchanged. Under identity the Windows App SDK
runtime must then already be in the process's package graph: the release publish is self-contained
(`WindowsAppSDKSelfContained=true`, installer README § step 4 and the release workflow), so it carries
the runtime beside the exe and the shipped sparse manifest declares **no** framework dependency — a box
without the framework could not register a package that did. Only the framework-dependent Debug build
needs one, and the e2e harness adds it to the identity it lends (`tests/e2e-unified/helpers/taskbar_badge.py`).

**The publisher DN has one build-time home: `apps/fauna-windows/installer/PackageIdentity.props`**
(`FaunaPackagePublisher`). The element's `publisher` must equal the identity package's
`Identity/@Publisher`, which is the signing certificate's Subject DN, and a mismatch is silent — the exe
simply runs without identity — so no consumer keeps a copy. `FaunaApp.csproj` imports the props and
generates the embedded manifest from `app.manifest.in` (its `publisher` is a build token, substituted
and XML-escaped by the `FaunaGenerateAppManifest` target); the sparse-package build script's default
publisher and the e2e harness's lent identity read the same file. The value there is the dev self-sign
DN every local build and e2e run uses; a release signed with the production certificate hands its DN to
both halves of the build — `-p:FaunaPackagePublisher=…` (or the same-named environment variable, which
sidesteps `-p:`'s comma splitting) to MSBuild, and the sparse build's publisher argument. The **Store**
channel is outside this home: its production publisher is the Partner Center identity (§ Identity &
coexistence), passed to the store build explicitly; only the store build's *dev* default reads the
props. Pinned by `test_package_identity_publisher.py`, which also fails on the DN literal appearing
anywhere else under `apps/fauna-windows`, `scripts` or `tests/e2e-unified`.

**A full package's identity reaches only a process started in its context** (measured on Windows
2026-09-28, `GetPackageFullName` on the process): an exe started directly from a registered Store
layout's folder runs with **no** identity, while the same exe started through
`Invoke-CommandInDesktopPackage` — or, for a user, from the package's Start entry — runs with the
package's. So a Store install badges through its own activation, and the e2e harness starts a Store-leg
app the same way (`flaui-bridge/SessionManager.cs::StartInPackageContext`).

Two pieces are **open**, tracked in the windows queue:

- **The Start-menu shortcut's AppUserModelID.** `App.wxs` installs a classic shortcut to the exe. The
  running, identity-carrying process groups on the package AUMID (`<PFN>!FaunaApp`) while a pin made
  from that shortcut is keyed on the exe path, so whether the packaged badge lands on the pinned button
  — and whether a launch from the pin and the running process share one button — is open. The known
  fix is a `System.AppUserModel.ID` `ShortcutProperty` set to the package AUMID. Its family name is
  `<Identity/@Name>_<publisher id>`, the publisher id being the first 8 bytes of SHA-256 over the
  UTF-16LE publisher DN, Crockford-base32 encoded (13 characters; checked 2026-09-28 against the family
  name Windows assigned a registered test package) — so the property, if it lands, is
  computed at build time from the one home above, never pasted. Measure on a real install before
  deciding; the measurement needs the elevated, serialized MSI install the § Testing table's
  real-install row describes, which disrupts other work on a shared machine.
- **The elevated-install verification** § Shell Extension's implementation status already awaits also
  proves this path: `Package.Current` resolving inside the installed FaunaApp, and a badge row for the
  package AUMID in the notification store after a message arrives.

> **Implementation status (sparse package), 2026-07-14 — PARTIAL; do not read the prose above as
> shipped.** What exists and is *verified on real Windows hardware*: the manifest template, the logo
> generator, `build-sparse-package.py` (pack + dev self-sign), and the headless structural suite
> (`test_sparse_package.py`, 6 passed). The package **packs, signs, and registers against the real
> `C:\Program Files\Fauna` install** — `Get-AppxPackageManifest` reads back
> `windows.fileExplorerContextMenus` (verb `Fauna` on `*` / `Directory` / `Directory\Background`) and
> the `windows.comServer` surrogate, both bound to the existing `{4a7b8c10-…}` CLSID. **Three gaps
> remain:** (1) **the MSI does not build, ship, or register the package** — there is no MSI-native
> mechanism, so install needs an `Add-AppxPackage -Stage` + `Add-AppxProvisionedPackage` custom
> action and uninstall a matching removal (per-machine; the MSI is per-machine); (2) **the menu has
> not been observed rendering.** The handler's `GetState` returns `ECS_HIDDEN` unless exactly one
> *tracked* file is selected (status via the `fauna-sync` named pipe), so a render check needs a
> running sync service and a tracked file; (3) whether registering both surfaces yields a *duplicate* entry
> inside "Show more options" on Win11 is **unknown and must be observed** — if it does, the two
> registrations need an OS-version condition (VS Code makes them mutually exclusive at install time,
> though its per-user detection is unsound for a per-machine installer).
>
> Gap (2) is a **rendering** question — a genuine last-inch human check *only* for "does it look
> right". The mechanism behind it is not: driving Explorer's context menu through UIA is the
> intended automation, and the absence of that test must not be laundered into "this needs a human"
> (a "this needs a human" claim is load-bearing — attack it before believing it, or it silently
> licenses shipping the mechanism untested; the human check covers only the pixels, never the
> mechanism behind them).
>
> **Live test, 2026-07-15 — gap (2) was attempted and surfaced a prerequisite gap in front of it: no
> installed/staged/built MSI carries the fixed shell DLL.** On a genuinely *tracked* (badged) file
> with Explorer restarted and `fauna-sync` running, **Fauna appeared in *neither* menu — not the
> Win11 default, not "Show more options".** That the *legacy* verb was also absent is the tell: the
> context-menu handler renders nowhere on this box because the installed
> `fauna_shell_0.1.0.dll` (and every staged/`build/installer` copy) is dated **2026-07-13**, ~21 h
> *before* the context-menu contract fix (2026-07-14 11:42). That fix changed the DLL
> *binary* (`shell-ext/src/context_menu.rs` +130, `lib.rs` +179), not only `ShellExt.wxs` — so the earlier
> `wix`-only rebuild (which re-ran over stale staged binaries) produced an MSI with the *fixed
> registration* but the *pre-fix DLL binary*. The **badge still shows** because the overlay is a
> separate COM class (different CLSID set) unaffected by the context-menu contract bug — badge-yes /
> menu-no is the diagnostic signature of a pre-fix shell DLL. **Consequence:** observing gaps (2) and
> (3) requires a **full RUNBOOK rebuild of the shell DLL** (not a `wix`-only pass) followed by a
> **clean install** — because the version-stamped shell DLL is `Permanent`+`NeverOverwrite`, a
> same-version (0.1.0) reinstall does **not** replace the stale file; the fixed DLL lands only via a
> version bump (new stamped `@Name`) or an uninstall-first install. Until a post-fix DLL is installed,
> the "`GetState`→`ECS_HIDDEN` unless a tracked file + running sync service" precondition in gap (2)
> cannot be tested in isolation (the stale-DLL blocker sits in front of it — measure, don't argue).
>
> **Update, 2026-07-15 — the stale-DLL blocker is cleared at the *build* layer; the clean
> install is the one remaining, UAC-gated step.** A targeted rebuild of `fauna-shell-ext` produced the
> post-fix `fauna_shell.dll` (566784 bytes, vs the stale 566272), it was re-staged, and a fresh
> `wix build` produced `build/installer/Fauna-Setup-arm64.msi` (structure suite: 61 passed). An
> admin-install extraction (`msiexec /a`) **confirms the MSI now embeds the post-fix DLL**
> (`fauna_shell_0.1.0.dll`, 566784 bytes, dated 2026-07-15) — so the "no installed/staged/built MSI
> carries the fixed DLL" statement above is now false at the *built* layer. What remains before gaps
> (2)/(3) can be observed is purely the **clean install of that MSI onto the box** (uninstall-first,
> because the `Permanent`+`NeverOverwrite` same-version DLL is not replaced by a plain reinstall) plus
> the sparse-package re-registration and an Explorer restart — every one of those steps needs an
> **elevated (UAC) session**, which is the true remaining gate, not any code defect.
>
> **Update, 2026-07-15 — gap (1) is now AUTHORED + structurally tested: the MSI ships +
> registers the package.** Two deferred/system custom actions were added to `Package.wxs`,
> `RegisterSparsePackage` and `DeregisterSparsePackage`, running the OFFICIAL per-machine sequence
> (MS Learn *grant-identity-to-nonpackaged-apps* § Per-Machine): install
> `Add-AppxPackage -Stage <msix> -ExternalLocation <installdir>; Add-AppxProvisionedPackage -Online
> -PackagePath <msix> -SkipLicense`; uninstall `Remove-AppxProvisionedPackage -Online` (matched by
> `DisplayName == Identity/@Name`, stable across the dev→prod cert swap) + `Remove-AppxPackage
> -AllUsers`. The `.msix` (`Fauna-Sparse.msix`) now ships into `INSTALLFOLDER` as a `ShellExt`-feature
> component (`ShellExt.wxs`), built by the sparse-package build script before `wix build` and passed
> via `-d SparsePackage`. Both CAs obey the four proven deferred-CA traps (read the install dir from
> the overlay CLSID's `InprocServer32`, not `[INSTALLFOLDER]`; absolute Windows PowerShell 5.1 path;
> the no-`"`/`&`/`[..]`/empty-`{}` escaping contract) and gate on `CurrentBuildNumber >= 19041`
> in-script. `test_installer_structure.py::TestSparsePackageRegistration` (11 tests) pins the whole
> contract, and the rebuilt MSI embeds the package. **What remains is exactly one UAC-gated step,
> unchanged in kind from the runs above:** a clean elevated install of this MSI so the CAs actually run, then
> observing gaps (2)/(3) — the render (still blocked on a live *tracked* file; the non-brittle
> observation harness is entrusted to the Windows shell-extension track). The ③ duplicate answer, if
> a duplicate is seen, feeds back only into whether to add an OS-version *suppression* of the HKCR
> verb on Win11 — the register/deregister mechanism itself does not depend on it.
>
> **Update, 2026-07-15 — gap (1) is DONE + LIVE-VERIFIED; gaps (2)/(3) are ANSWERED. STEP E is
> complete.** The two custom actions were exercised on a **real elevated Windows box** (full
> uninstall-old → clean-slate → install → uninstall → reinstall cycle, `msiexec /l*v` captured):
> - **`RegisterSparsePackage` on install → "Return value 1", "running with sufficient privileges".**
>   It produced **both** a per-user `Add-AppxPackage` registration **and** an
>   `Add-AppxProvisionedPackage -Online` (all-users) entry — the latter is the surface manual dev
>   registration never had — and `Fauna-Sparse.msix` shipped into `INSTALLFOLDER`. The CA's
>   PowerShell, read back from the verbose log, is intact (reads the install dir from the overlay
>   CLSID `InprocServer32`, gates `>= 19041`, then `Add-AppxPackage -Stage -ExternalLocation` +
>   `Add-AppxProvisionedPackage -Online -SkipLicense`) — closing the historically-fatal "MSI mangles
>   the script into a parse error" class on a *real* elevated run, not just a static extraction.
> - **`DeregisterSparsePackage` on uninstall → "Return value 1"**, removing **both** the provisioned
>   and the all-users registration (verified `Get-AppxProvisionedPackage`/`Get-AppxPackage -AllUsers`
>   both empty afterward — **no stale registration left pointing at the deleted external location**),
>   and it ran **before** `CleanShellDlls` (which then ran, Return value 1) so the COM surrogate
>   released the DLL before orphan cleanup. The install ProductCode was looked up **dynamically**
>   (`{F148FE9B-…}` this build) — never hardcoded.
>
> **Gap (2) — the menu render — is observed (② = YES):** the Windows shell-extension track's headless
> UIA harness (`scripts/shellext/`) read "Fauna" rendering in the **Win11 default (top-level) menu** on
> a tracked file (`apps/windows.md` § Shell Extension). **Gap (3) — the duplicate — is RESOLVED:
> keep both registrations, no OS-version suppression.** The observed duplicate is **cross-menu, not
> same-menu**: the sparse package owns the modern top-level menu (gate ②'s UIA read showed exactly
> **one** "Fauna" there) and the HKCR verb owns the legacy "Show more options" menu — a Win11
> `IExplorerCommand` registered the classic way is demoted to "Show more options" only, so the two can
> never collide in one menu. Suppressing the HKCR verb on Win11 would **strand users who force the
> classic menu** (they lose the sparse entry entirely), and force-classic is a per-*user* `HKCU`
> setting a per-*machine* installer cannot soundly detect — so suppression is both unnecessary and
> harmful. This is exactly the "complementary, not competing" design stated above; the CAs already
> implement it (they never touch the HKCR verb), so nothing changes. **STEP E ships as-is.**

**That track is NOT gated on code-signing — the cert gates *distribution*, not the work** (corrected
2026-07-14; the prior wording read "sparse package + identity + **signing**" and was twice read as
meaning the track must wait on the "Fauna Social" org registration, which left the Windows installer
work idle for ~2.5 weeks). A sparse package is authored, packed (`MakeAppx /nv`), signed
with a **self-signed** dev cert trusted into `LocalMachine\TrustedPeople`, installed
(`Add-AppxPackage -ExternalLocation`) and fully exercised **on a dev box with no production
certificate whatsoever** — Microsoft's documented dev/test path
([create-certificate-package-signing](https://learn.microsoft.com/en-us/windows/msix/package/create-certificate-package-signing);
on Windows 11, `Add-AppxPackage -AllowUnsigned` needs no cert at all). What the production cert
gates is registering the package on an **end user's** machine, which otherwise fails
`CERT_E_UNTRUSTEDROOT` (0x800B0109) — **the same gate the MSI already sits behind**, not a new one
the sparse package introduces. Switching from the dev cert to the production cert is a **build-time
parameter swap**: `Identity/@Publisher` must equal the signing cert's Subject DN exactly, so the
manifest ships as a **template**, not a literal (note it also re-derives the PackageFamilyName, so a
dev-installed package is a *different* identity and must be `Remove-AppxPackage`d, never upgraded in
place).

> **Implementation status:** all **8 CLSIDs** are implemented in `shell-ext` — the 4
> overlay COM classes and the 4 context-menu COM classes (root `IExplorerCommand` + an
> `IEnumExplorerCommand` enumerator + 3 leaf commands). `DllRegisterServer` /
> `DllUnregisterServer` self-registration writes every CLSID's `InprocServer32`, the
> `ShellIconOverlayIdentifiers` keys, and the `*\shell\Fauna` verb key above. The
> overlay half is **live-verified** (badges render on a cloud-only placeholder,
> 2026-07-14); the context menu is **fixed but not yet live-verified** — the fix landed
> after the pass that found it. See `docs/goal/architecture/apps/windows.md`
> § Shell Extension. `ShellExt.wxs` must register the same CLSID set the DLL exposes (the
> DLL self-registers via `DllRegisterServer` for dev; the MSI writes HKLM directly in
> production).

## Protocol Handler

The `fauna://` URI scheme is registered during install (via `Protocol.wxs`) so that links from web browsers and other applications open directly in the Fauna desktop app. The registration is removed on uninstall.

## Upgrade

The installer uses a standard **MSI major upgrade** strategy:

- All builds share the same product GUID; the MSI version number is bumped on each release.
- When a newer MSI is run on a machine with an older version installed, the old version is automatically removed before the new version is installed.
- User data in `%ProgramData%\Fauna\` and `%LocalAppData%\Fauna\` is preserved across upgrades.
- Services are stopped before removal of the old version and restarted with the new binaries after installation completes.
- **The shell-extension DLL is the exception to "old removed before new installed":** it ships under a version-stamped filename and is `Permanent` (§ Shell Extension § Upgrade handling), so an upgrade installs the new `fauna_shell_<ver>.dll` side-by-side and **never** removes/replaces the in-use old DLL — Explorer is never closed, and the new shell ext activates on the next sign-out (no reboot). The orphaned old DLL is reclaimed out-of-band by the `CleanShellDlls` CA (§ Shell Extension § Upgrade handling).

## Uninstall

### Default Uninstall

Running the uninstaller (via **Add or Remove Programs** or `msiexec /x`) removes all binaries, services, COM registrations, the `fauna://` protocol handler, and the Start Menu shortcut. User data in `%ProgramData%\Fauna\` and `%LocalAppData%\Fauna\` is **preserved**. It also unregisters every persistent cfapi sync-root registration (`CleanSyncRoots` CA, below) — Explorer never renders a ghost cloud location for an uninstalled provider; the synced locations and their files on disk are untouched, only the sync-root **binding** is removed.

### Full Removal (including user data)

To remove the machine-wide service data:

```
msiexec /x Fauna-Setup-x64.msi REMOVE_USER_DATA=1
```

This additionally deletes `%ProgramData%\Fauna\` (via the `DataDirRegistry` component's
`util:RemoveFolderEx`, gated by the `REMOVE_USER_DATA=1` condition). Per-user
`%LocalAppData%\Fauna\` is **not** touched by the installer — see the Installation Directory Layout
note above; clearing it is the desktop app's responsibility.

### Uninstall sequence

1. The per-user sync-agent logon launch (HKLM `…\Run` / scheduled task) is removed; the machine services are stopped (`FaunaNest`, `FaunaBridge`); the running agent is force-killed (`KillFaunaSync` CA — it has no lifecycle window, so the built-in Restart Manager can never close it; `[INSTALLFOLDER]`-scoped, matching `fauna-sync-agent.exe`)
1.5. **Every persistent cfapi sync-root registration is unregistered** — both the filter half (`CfUnregisterSyncRoot`) and the shell half (`StorageProviderSyncRootManager::Unregister`, the `SyncRootManager` registry state) — for every `Fauna!*` entry on the machine, live folder or not (`CleanSyncRoots` CA, immediate, runs the just-installed `fauna-sync-agent.exe --cleanup-roots`; reuses `fauna_cfapi::list_shell_sync_roots` + the product's own ghost-janitor primitives, `fauna-sync-agent::cfapi_host`). Must run **after** step 1 (the old agent's live filter connection must be torn down first — a still-connected root refuses the shell unregister) and **before** step 6 (the exe must still be on disk to invoke). Skipped during a routine version upgrade's `RemoveExistingProducts` of the old product (`NOT UPGRADINGPRODUCTCODE`) — that teardown is not a real uninstall. Registrations are **persistent** by design (`docs/goal/behavior/file-sync.md` § Per-file sync-status display — they survive a service stop/restart/crash and only come down when the binding ends), so without this step every uninstall would strand them: a dead "Fauna – <set>" cloud location Explorer keeps rendering, removable only via regedit.
2. Machine services unregistered from the Windows Service Control Manager
3. Shell extension COM entries removed from the registry
4. `fauna://` protocol handler removed from the registry
5. Start Menu shortcut removed
6. Binaries removed from `%ProgramFiles%\Fauna\` — **except** the `Permanent` shell-extension DLL (`fauna_shell_<ver>.dll`, § Shell Extension § Upgrade handling): MSI leaves it so it never has to free the in-use file's lock (which would close Explorer). With its COM entries removed (step 3), it is inert; the orphaned DLL is reclaimed out-of-band by the `CleanShellDlls` CA (§ Shell Extension § Upgrade handling)
7. If `REMOVE_USER_DATA=1`: `%ProgramData%\Fauna\` removed (per-user `%LocalAppData%\Fauna\` is
   client-managed and is not removed by the installer)

## Store distribution (MSIX)

> **Status: design ratified 2026-08-10. Manifest template + build script BUILT and packing green
> on Windows 2026-08-10** (`makeappx` schema-validates both arch packages headlessly); local
> registration of a real payload, and the submission itself, are not done — see *Implementation
> status* at the end of this section.
> Channel decision of record: `## Implementation status today` signing bullet (1). This section owns
> the *package design*; the build slices, and the user-side Partner Center enrollment + submission
> mechanics, are tracked internally.

The Microsoft Store channel ships a **full MSIX** — a package carrying its binaries inside.
An external-location (sparse) package cannot be Store-distributed, and a developer-hosted Win32
MSI/EXE Store listing needs its own certificate (ruled out by the channel decision), so full MSIX
is the only Store shape that gets Store signing
([packaging overview](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/packaging/)).

### Feature subset — what the Store package carries

Verified per-component against the packaged-app constraint set (2026-08-10):

| MSI feature | Store MSIX? | Verified constraint |
|---|---|---|
| Desktop App (WinUI 3) | **IN** | standard `runFullTrust` win32App; reuses the MSI's self-contained publish output (verify WASDK self-contained-inside-MSIX at build time) |
| `fauna://` handler | **IN** | `uap:Protocol` manifest extension |
| Per-user sync agent | **IN** | `windows.startupTask` desktop extension replaces the HKLM Run key (`Enabled="true"` is honoured for full-trust desktop apps once the app has launched once; user controls it in Task Manager — [StartupTask](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask)) |
| Explorer Integration — context menu | **IN** | `desktop4:FileExplorerContextMenus` + `com:ComServer` surrogate — the sparse package's own mechanism with `com:Class/@Path` now resolving *inside* the package (bare `fauna_shell.dll` name; Path-1 version-stamping is an MSI-lock concern that does not apply inside MSIX) |
| Explorer Integration — 4 overlay badge handlers | **OUT** | `ShellIconOverlayIdentifiers` is an HKLM-only registration with **no MSIX manifest surface**; a Store app cannot write HKLM. Accepted absence: cfapi renders placeholder state natively in Explorer's Status column; corner badges remain an MSI-install nicety |
| Nest Service | **OUT — MSI-only, permanent** | MSIX services run only as `localSystem`/`localService`/`networkService` ([desktop6:Service](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop6-service)) — **no virtual accounts**, so shipping it Store-side would abandon the ratified `NT SERVICE\FaunaNest` least-privilege shape; the `0.0.0.0:443` bind + firewall rule + `%ProgramData%` layout is the opt-in self-hosting path the direct-download MSI serves |
| Bridge Service | **OUT — MSI-only, permanent** | same, plus it requires Nest |

The Store package is therefore **the app**: Desktop App + sync agent + Win11/legacy context menu,
connecting to a remote nest by handle — exactly the MSI's default feature selection, minus overlay
badges. Self-hosting stays a direct-download MSI decision.

### Identity & coexistence with the sparse package

- **Identity comes from Partner Center**: reserving the app name mints `Identity/@Name` and
  `Publisher` (`CN=<GUID>`), the manifest must match them exactly, and the Store re-signs the
  package ([reserve your app's name](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/reserve-your-apps-name)).
  The manifest therefore ships as a **template** (same pattern as the sparse
  `AppxManifest.xml.in`), with `@@IdentityName@@` joining `@@Publisher@@`/`@@PackageVersion@@`.
- **Reserved 2026-08-22 — these are the real values** (Partner Center → Product identity), and
  they are what `build-store-package.py --identity-name` / `--publisher` take:

  | Manifest field | Value |
  |---|---|
  | `Package/Identity/Name` | `FaunaSocial.FaunaSocial` |
  | `Package/Identity/Publisher` | `CN=E8868D60-047A-46A3-B608-0D4CA88AB791` |
  | `Package/Properties/PublisherDisplayName` | `Fauna Social` (already the template's literal) |
  | *(derived)* Package Family Name | `FaunaSocial.FaunaSocial_hra91aebets7r` |
  | *(derived)* Store ID | `9PLWNB0DJ0V7` — listing at `https://apps.microsoft.com/detail/9PLWNB0DJ0V7` |

  They stay **tokens** in the checked-in template regardless: the dev loop self-signs a plain-DN
  identity the Store would reject, so hard-coding either would weld the file to one channel.
- **The listing is named "Fauna Social"; the app is still named "Fauna" (decided 2026-08-22).**
  `Fauna` was unavailable at reservation (another publisher holds it). Two distinct fields
  follow from that, and only one is forced: `Properties/DisplayName` **must** equal a reserved
  name (Partner Center validates it at upload) so it reads `Fauna Social`, while
  `Application/VisualElements/@DisplayName` — the Start-menu / taskbar / Alt+Tab label — is not
  reservation-validated and deliberately stays **`Fauna`**, matching the other six apps
  (priority #1: a Store name-availability accident must not leak a naming divergence into app
  UI). A user installs "Fauna Social" and finds "Fauna" in their Start menu. Microsoft reclaims
  names reserved-but-unpublished after ~3 months and a product may hold several reservations, so
  `Fauna` can later be added to this same product without a new listing or identity change.
- **The Store PFN is a different identity from the sparse package's by construction**
  (`FaunaSocial.Fauna` + SignPath/dev publisher vs. `FaunaSocial.FaunaSocial` + `CN=<GUID>`) —
  confirmed concretely at reservation, not merely predicted — so both packages can be registered
  on one box with no package-level conflict.
- **The Store package subsumes the sparse package for Store installs** — identity is native to a
  full package. The sparse package remains exactly what it is today: the MSI's companion, granting
  identity to the MSI-installed shell ext. Neither replaces the other; they serve disjoint installs.
- **Supported topologies:** (1) Store app alone — the normal client box; (2) MSI alone — any
  feature selection; (3) **Store app + nest-only MSI** — the recommended both-channels box for a
  self-hosting user who wants Store-updated apps. (4) Store app + MSI *with client features* is
  **discouraged but not blocked**: the Win11 default menu renders two "Fauna" entries (sparse +
  Store packages each declare the verb), both `fauna://` registrations exist, and both launch paths
  fire at logon — the app's single-instance invariant (apps/windows.md § App Lifecycle) collapses
  the app; **the agent's second-instance behavior is VERIFIED GREEN on Windows 2026-08-22** (below).
  An optional hardening slice teaches the MSI UI sequence to pre-deselect client features when the
  Store PFN is registered.
- **Two logon agents, one survivor — verified across the package-identity boundary (2026-08-22).**
  Both hooks launch `fauna-sync-agent.exe`, and `pipe_server::InstanceLock` picks the survivor: the
  loser exits `Ok(())` — rc 0, having touched nothing. Two facts made that non-obvious, and both are
  now pinned by `tests/real_session/test_store_package_registration.py`:
  - **Package identity does not change the user SID.** The mutex name derives from the token's user
    SID (`fauna_ipc::sync::current_user_pipe_name` → `mutex_name_for_pipe` →
    `Local\FaunaSyncAgent.<SID>`), so a packaged and an unpackaged agent name the **same** object.
    Had identity altered the SID, the two channels would have contended on different mutexes.
  - **A full-trust packaged process shares the session's `Local\` object namespace.** This is the
    reason the packaged launch (`Invoke-CommandInDesktopPackage`, not a plain spawn) is what the
    test drives: MSIX identity is exactly what could have isolated the namespace, leaving the
    machine-global `FILE_FLAG_FIRST_PIPE_INSTANCE` create-failure as the only stop — the later,
    louder exit the design deliberately does not rely on. The test asserts the loser named the
    expected mutex, so it discriminates the two paths rather than merely observing a clean exit.
  - ⚠ **A packaged app's writes under `%LocalAppData%` are virtualized away from the caller**, with
    no exception since 2026-08-23 — `%LocalAppData%\Fauna` is virtualized too (§ Data continuity).
    Anything a packaged process must hand back (a log, an exit code) therefore has to live outside
    `%LocalAppData%`; a scratch path under the repo tree is what the test uses.

### Data continuity (Store install state; MSI → Store migration RETIRED 2026-08-23)

- **The vault carries over for free**: per-user credentials live in Windows Credential Manager,
  which is per-user, not per-package — a full-trust packaged app reads the same store.
- **The filesystem half is VIRTUALIZED, and that is the ratified choice (user directive,
  2026-08-23 — this reverses the 2026-08-10 ratification, which preferred the opposite).**
  Packaged apps get AppData/registry write virtualization by default, so the Store package writes
  to its own private `%LocalAppData%\Fauna`. It declares **no** opt-out: neither the Windows 11
  scoped [flexible virtualization](https://learn.microsoft.com/en-us/windows/msix/desktop/flexible-virtualization)
  `ExcludedDirectory` form nor the pre-Win11 blanket `desktop6` switch, and therefore **not** the
  `unvirtualizedResources` restricted capability.
  - **What the opt-out bought, and why it went.** It gave a Store install and an MSI install one
    shared `%LocalAppData%\Fauna` — i.e. coexistence with MSI-installed components, plus
    MSI→Store migration. Both serve a box that already has the MSI on it, and **there are no MSI
    installs in the field**. What it cost was the submission's one *unusual* restricted capability:
    the thing certification stops to read a hand-written justification for. `runFullTrust` remains
    (also `rescap:`, but the standard desktop-bridge capability every full-trust Win32 Store app
    declares).
  - **Nothing breaks on a Store-only box.** App, sync agent and shell extension all run under
    package identity, so they share one virtualized view. The single path that would escape it —
    `extract_icons()` handing Explorer a literal `%LOCALAPPDATA%\Fauna\icons`
    (`apps/fauna-windows/shell-ext/src/lib.rs::extract_icons`) — is inert here: its only consumer
    is the icon overlay handler (`overlay.rs`), and an MSIX package cannot register one (it needs
    an HKLM key), which is the same "overlays = no packaged surface" finding the design pass made.
  - **What a would-be migrator loses**, should this ever matter again: a fresh `mls.db` (the device
    re-establishes MLS sessions; conversation history re-syncs from the nest), re-created sync
    bindings, lost local drafts. **No user-irrecoverable loss** — the identity secret lives in
    Windows Credential Manager, which is per-*user*, not per-package, and content lives on the nest.
  - **Reversible.** Re-adding the capability is an additive manifest change plus a resubmission,
    needing a fresh certification justification. It is a goal-doc decision, not a manifest edit;
    `test_store_package.py::test_declares_no_write_virtualization_opt_out` and
    `::test_capability_set_is_exactly_the_ratified_one` fail if a manifest edit tries.
- **`CleanSyncRoots` hazard (MSI uninstall on a both-installed box):** the MSI's uninstall CA
  unregisters **every** `Fauna!*` sync root machine-wide, including roots the Store agent owns.
  Self-heal exists at the code level: the product path *ensures* shell registration when serving a
  root (`bins/fauna-sync-agent/src/cfapi_host.rs::register_and_connect_shell`), so the Store
  agent's next reconcile re-registers its bindings. **VERIFIED live on Windows 2026-08-22** —
  `cfapi_live_integration::uninstall_cleanup_is_self_healed_by_the_next_product_registration`
  drives the CA's exact entry point and proves the binding is *gone* before proving a reconcile
  brings it back, so it discriminates a real heal from a wipe that never happened. The pre-ratified
  fallback (scope the CA to skip when the Store PFN is registered) is therefore **not needed**.
  - The CA runs `fauna-sync-agent.exe --cleanup-roots` with **`Execute="immediate"`**
    (`Package.wxs`), i.e. unelevated in the installing user's own context — so this hazard needs no
    admin, no MSI install and no throwaway VM to reproduce, which is why it is an ordinary test.
  - **Remaining inch, cross-identity:** whether an *unpackaged* MSI uninstall can reach a root
    registered by a *packaged* Store agent is not yet answered. If it cannot, the hazard is
    narrower than stated above rather than wider — either way the heal already covers it.

### Build & packaging shape

- **The store-package build script** — sibling of the sparse-package build script, same conventions:
  reads version out of `Package.wxs` (4-part), stages a full layout (the MSI's staged app publish
  output + `fauna-sync-agent.exe` + `fauna_shell.dll` + Assets), substitutes
  `apps/fauna-windows/installer/store/AppxManifest.xml.in`, `makeappx pack` (no `/nv` — content is
  internal, path validation SHOULD run), per arch: **x64 + ARM64**, two `.msix` files per release.
  - **Four tokens, not three:** `@@IdentityName@@`/`@@Publisher@@`/`@@PackageVersion@@` plus
    **`@@ProcessorArchitecture@@`** — a full MSIX carries native binaries, so each of the two
    packages must declare the arch it carries; one `neutral` package would claim to run everywhere
    while shipping one arch's code.
  - **`--payload-root`** selects the staged payload (default `build/installer/stage/<arch>/`, the
    MSI's own staging layout). It is what lets the packaging mechanism be tested headlessly in
    seconds against stub payload files, instead of behind a ~30-minute self-contained publish.
  - Brand assets have one generator and one home — the packer reuses `installer/sparse/Assets`
    (the same dedicated appx-logo renderer) rather than forking a second copy of the mark.
- **Store submission uploads are unsigned** — the Store signs. The local dev loop self-signs
  exactly like the sparse package (`--self-sign`; the dev-signed package is a different PFN —
  remove, never upgrade across identities, the standing gotcha).
- **Structural tests first (red-first, priority #5)** — two files, split by what they can prove:
  - `test_store_package.py` (tier_1, **fleet-wide**, mirrors `test_sparse_package.py`): parses the
    checked-in template and calls the build script's real functions — identity fully templated,
    no `AllowExternalContent`, the root CLSID read from `ShellExt.wxs` (no 9th GUID), bare
    `fauna_shell.dll` path, startupTask/protocol/context-menu present, **no service declarations**,
    capability set exactly the ratified two, the virtualization exclusion scoped, a real
    `AppListEntry`, and the arch/version/no-`/nv` build invariants.
  - `test_store_package_pack.py` (tier_3, **win-only**): runs the packer over a stub payload and
    inspects the two resulting `.msix` files. This is the half that catches what XML parsing
    cannot — `makeappx` schema-validates the manifest (a misspelled extension category is rejected
    with `C00CE169 … violates enumeration constraint`, verified by perturbation 2026-08-10), and
    the per-arch substitution is proven by the two packages disagreeing on exactly that attribute.

**Store validation surprises are expected on first submission — plan an iterate loop** (tracked
internally).

### Implementation status (Store channel)

| Piece | Status |
|---|---|
| Manifest template `installer/store/AppxManifest.xml.in` | **Built** 2026-08-10 |
| The store-package build script (both arches, self-sign, `--payload-root`) | **Built** 2026-08-10 |
| Structural + packing tests (29, green on Windows) | **Built** 2026-08-10 |
| Local registration of a real payload (startupTask, packaged context menu, `fauna://`) | **GREEN on Windows 2026-08-11** — `tests/real_session/test_store_package_registration.py`, non-elevated |
| Agent second-instance behavior across the package-identity boundary | **GREEN on Windows 2026-08-22** — same file, 8/8; the packaged duplicate exits rc 0 on the `Local\` mutex (§ Identity & coexistence) |
| Taskbar badge under the Store package's own identity | **GREEN on Windows 2026-09-28** — `tests/real_session/test_store_package_taskbar_badge.py`, non-elevated: the Store layout staged by the packer's own functions around the Debug build, the app started in the package's context (it reports the package via `GetPackageFullName`), and the unread count on record for `<PFN>!FaunaApp` (§ Package identity for FaunaApp.exe) |
| `CleanSyncRoots` self-heal verified live | **GREEN on Windows 2026-08-22** — `cfapi_live_integration::uninstall_cleanup_is_self_healed_by_the_next_product_registration`; the CA's fallback (scope by Store PFN) is not needed. Cross-identity inch open (§ Data continuity) |
| Partner Center enrollment (company account, org verification) | **DONE 2026-08-22** — verified; publisher display name "Fauna Social" |
| App name reserved + identity captured | **DONE 2026-08-22** — `FaunaSocial.FaunaSocial` / `CN=E8868D60-…`; § Identity & coexistence holds the values |
| First Store submission | **Not done** — needs a real staged payload built on Windows; no longer blocked on identity |

**Deployment paths — which one a local verification may use (learned 2026-08-11).** Three exist and
only the third is both non-elevated and able to carry this package's extension set; the two dead
ends each *look* right and cost a build to disprove, so they are recorded:

1. `Add-AppxPackage <msix>` **signed** → needs the cert in `LocalMachine\TrustedPeople`, i.e. admin.
2. `Add-AppxPackage <msix> -AllowUnsigned` → **cannot work for us, ever.** It first demands a
   Publisher in the *unsigned namespace* (an OID suffix; `0x80073D2C` otherwise), then refuses the
   package anyway: `0x80073D2B — an unsigned package cannot include Executable activations`. The
   startupTask and COM-surrogate extensions *are* Executable activations.
3. **`Add-AppxPackage -Register <AppxManifest.xml>`** on the staged loose layout — the developer
   registration VS's F5 uses. Developer Mode, no signing, **no admin**, full extension support.
   Its Publisher requirement is the exact inverse of (2): the **signed** namespace, a plain DN with
   no OID suffix (`0x80073D2D` otherwise).

The automated verification uses (3), so it runs unattended. Residual gap: it registers the loose
layout, not the packed `.msix` (the container is covered by `test_store_package_pack.py`);
registering a *signed* `.msix` remains admin-gated. Scope note on the startup task: registration is
asserted, but the OS only materialises StartupTask **state** (and `StartupTask.GetAsync` only
answers) after the app has run once under package identity.

**Scoping caveat — RETIRED 2026-08-23.** This paragraph used to record that the Win11-only
exclusion left Windows 10 Store clients unable to share state with an MSI install. The package now
declares no exclusion on any Windows version (§ Data continuity), so the caveat has no subject:
every Store client virtualizes its own `%LocalAppData%\Fauna`, uniformly.

## Testing

Three tier_3 surfaces validate the installer. `release.yml` ships from the public
repository (moved 2026-08-31; owner:
[`../release-integrity.md`](../release-integrity.md) § *When a release workflow publishes*),
triggered on a `v*` tag push (or a `workflow_dispatch` naming one) with `build-windows`
running on a GitHub-hosted `windows-latest` runner for both the x64 and ARM64 legs — the
public repository's release workflow builds and attests on each `v*` tag. Each gate below
also runs on a Windows ARM64 host or a throwaway VM.

| Surface | File | Needs | Asserts | Windows-runnable? |
|---|---|---|---|---|
| MSI authoring | `tests/e2e-unified/tests/platform/windows/test_installer_structure.py` | a pre-built MSI; **no admin** | `Feature`/`FeatureComponents`/`File` + `Upgrade`/`ServiceInstall`/`ServiceControl`/`Wix4ServiceConfig`/`Registry` tables — feature tree, components, files, major-upgrade authoring, service virtual-accounts + recovery, 8 shell CLSIDs, `fauna://` | **Yes** — read-only `msi.dll` query, non-disruptive (skips, not fails, if the MSI isn't built) |
| Real install | `tests/e2e-unified/tests/platform/windows/test_installer.py` | **admin**; solo + serialized, or a throwaway VM | per-feature `ADDLOCAL` install, services reach RUNNING, file layout, plain-uninstall preserves `%ProgramData%` | **Disrupts other work on the machine** — perMachine services + HKLM/`%ProgramData%` writes; run elevated + serialized when the box is otherwise idle (practiced 2026-06-19), or on a throwaway VM |
| Binaries serving | `tests/e2e-unified/tests/platform/windows/test_caldav_imap_serving.py` | locally-built `fauna-nest-svc.exe` + `fauna-mail-bridge.exe` + `seal-helper-testonly.exe`; **no admin** | the `--foreground` loopback nest writes its TLS *floor* cert under the **data-dir** acme dir (`fauna_nest::start_server` floor write fires on Windows); the supervised MDA serves a CalDAV + IMAP round-trip via two Python `CalDAVClient`s + `imaplib` (GREEN, Track 3 increment 2) | **Yes** — temp data dirs + ephemeral loopback ports, no SCM / HKLM / `%ProgramData%` writes; covers the *Windows binaries* the Linux dev machine's (Linux-binary) any-locator matrix can't |
| Installed serving | `tests/e2e-unified/tests/platform/windows/test_installer.py::TestCalDAVImapServingAfterInstall` | **admin** + throwaway VM + Go/llvm-mingw build toolchain (the MSI now ships the MDA) + `seal-helper-testonly.exe` | the **MSI-installed** `FaunaNest` + `FaunaBridge` SCM services serve CalDAV `:8443` + IMAP `:993`: enable CalDAV over WS-RPC → the FaunaBridge-supervised MDA auto-enrolls on loopback (no manual approve/x25519-poke) → two Python `CalDAVClient`s round-trip a calendar event + `imaplib` opens INBOX off the floor cert | **✅ GREEN — install-level RUN PROVEN on Windows 2026-06-19 (`1 passed, 27s`).** Ran elevated on the dev box (volatile data, serialized). The RUN surfaced + fixed a WiX Bridge-feature install bug + 3 `_build_msi` build-path bugs |
| Store MSIX authoring | `tests/e2e-unified/tests/platform/windows/test_store_package.py` | nothing — pure parsing + in-process calls; **no admin** | the manifest template's identity/capability/extension/virtualization invariants and the build script's arch, version-source and no-`/nv` invariants (§ Store distribution → Build & packaging shape) | **Yes, and on every machine** (tier_1, no `win32` skip — the couplings it guards are fleet-wide, same as the sparse test) |
| Store MSIX packing | `tests/e2e-unified/tests/platform/windows/test_store_package_pack.py` | Windows SDK `makeappx.exe`; **no admin** | the packer accepts the manifest (real schema validation) and produces an x64 **and** an ARM64 `.msix`, each declaring its own arch, carrying its payload, with no surviving `@@` token | **Yes** — stub payload files, ~2 s, no install; skips if the SDK is absent |
| Store MSIX registration | `tests/e2e-unified/tests/real_session/test_store_package_registration.py` | Developer Mode + a staged real payload; **no admin**; `--real-session` | a real per-user registration: Status=Ok, PFN derives from Publisher, the registered manifest kept all four ratified extensions and **no service**, startupTask names `fauna-sync-agent.exe` + is enabled, the `fauna` protocol class exists, and the shell-ext CLSID **activates** in the packaged COM surrogate | **✅ GREEN on Windows 2026-08-11 (6 passed, 5m19s).** Borrow-and-give-back; cleanup verified (no package, no `fauna` class left). Skips if no payload is staged |
| Publisher single home | `tests/e2e-unified/tests/platform/windows/test_package_identity_publisher.py` | nothing — pure parsing + in-process imports; **no admin** | `installer/PackageIdentity.props` declares the one publisher DN; `app.manifest.in`'s msix publisher is the token and `FaunaApp.csproj` generates the embedded manifest from the props; the sparse/store build scripts' dev default and the harness's lent identity equal it; the DN literal appears nowhere else under `apps/fauna-windows`, `scripts`, `tests/e2e-unified` (§ Package identity for FaunaApp.exe) | **Yes, and on every machine** (tier_1; both the restated-literal and the literal-in-template perturbations verified red 2026-09-28) |
| Store taskbar badge | `tests/e2e-unified/tests/real_session/test_store_package_taskbar_badge.py` | Developer Mode + the Debug build; **no admin**; `--real-session` | the app, started in a registered Store-shape package's context, runs with that package's identity and the OS holds the app's own unread count as the badge for `<PFN>!FaunaApp` | **✅ GREEN on Windows 2026-09-28 (1 passed, 49 s).** Borrow-and-give-back; no test package left registered |
| CI smoke | `release.yml` `build-windows` (x64 leg) | a CI run | install/uninstall succeed; `REMOVE_USER_DATA=1` deletes vs. plain uninstall preserves `%ProgramData%\Fauna\` | **No** — it runs on the public repository's GitHub-hosted `windows-latest` runner on each `v*` tag; x64 only |

### Coverage gaps

**Win-runnable authoring guards — implemented 2026-06-06.** `test_installer_structure.py` was
extended beyond the original `Feature`/`FeatureComponents`/`File` coverage — the structural suite
reached **61 tests** by 2026-06-27 (including the `TestShellExtUpgradeSafety`, `TestFirewall`, and
`TestSyncAgentSubsystem` classes) and stands at **79 tests** as of this sweep (2026-07-20), after the
later `TestSparsePackageRegistration` (11 tests, § Menu placement — the sparse package) and
`TestCleanSyncRoots` (4 tests, § Upgrade handling → `CleanSyncRoots`) classes.
Each was confirmed non-vacuous by perturbing the `.wxs` source and watching the matching assertion
fail before reverting:

- **`Upgrade` table** — stable `UpgradeCode`, the `WIX_DOWNGRADE_DETECTED` downgrade-prevention row,
  and `RemoveExistingProducts` sequenced after `InstallInitialize` (§ Upgrade authoring; the runtime
  upgrade itself still needs a real install — see below).
- **`ServiceInstall` / `ServiceControl`** — each service's `Account="NT SERVICE\Fauna*"`, own-process +
  auto-start + normal error control, and the `Event=163` start-on-install / stop-on-both /
  delete-on-uninstall lifecycle (§ Windows Services).
- **`Wix4ServiceConfig`** (`util:ServiceConfig`) — flat-5 s restart ×3, reset after 1 day (§ Windows Services).
- **`Registry` table** — all 8 CLSID `InprocServer32` rows → the version-stamped
  `[INSTALLFOLDER]fauna_shell_<version>.dll` (the guard derives the expected name from
  `Package/@Version`) + `ThreadingModel=Apartment`;
  the 4 **space-prefixed** `ShellIconOverlayIdentifiers` keys (overlay sort priority within Windows'
  15-overlay limit); the `*\shellex\ContextMenuHandlers` key; the `fauna://` scheme rows
  (§ Shell Extension, § Protocol Handler).

These are pure authoring-regression guards — they catch a dropped/renamed CLSID, a wrong service
account, or a broken upgrade GUID **without installing anything**, so they run on Windows today and in
any future CI unchanged. (Caveat: invoking them through `pytest` pulls the e2e suite's `autouse`
`nest_instance` fixture, which builds a nest; the assertions themselves need only the built MSI +
`msi.dll`, so dev iteration can call the test methods directly.)

**Requires a real install — solo/elevated + serialized on Windows, a throwaway VM, or enabled CI** (perMachine
installs disrupt other work on a shared machine, so they run only when it is otherwise idle; the
exception is Path-1 shell-ext live upgrade testing, which never touches the loaded DLL and is safe on the
shared box at any time — § Shell Extension § Upgrade handling)**:**

- The destructive `REMOVE_USER_DATA=1` *delete* path is covered only by the CI smoke in `release.yml`; the admin
  test (`TestUninstallPreservesData`) covers only the *preserve* half. Selective-install + services-running
  run only in the admin test. No single environment exercises the full matrix.
- **ARM64 runtime install has no automated coverage** — the CI smoke is x64-only (windows-latest
  cannot install an ARM64 MSI); the arm64 admin test is dormant. "x64 covers both" holds **only** for the
  arch-independent data-removal custom action, not for arm64 binaries loading / services starting / the
  arm64 shell DLL registering.
- **MSI major-upgrade runtime** (install vN → vN+1; data preserved; no duplicate ARP entry) has no test —
  only the authoring is table-checkable above.
- Service **virtual-account identity** and *installed*-service function — a `RUNNING` assertion only
  proves the SCM stub started (the service sets `RUNNING` before its heavy loop). Two surfaces now cover
  the *function* half: (1) the **binaries-serving** surface above (`test_caldav_imap_serving.py`) covers
  it non-disruptively (the `--foreground` loopback nest boots + writes its TLS floor under the data dir;
  CalDAV/IMAP round-trip GREEN — two Python `CalDAVClient`s + `imaplib` off the floor cert via the
  `fauna-mail-bridge.exe` MDA; engine `helpers/windows_caldav_nest.py`), but spawns the binaries directly
  and so **bypasses the SCM virtual-account services + the FaunaBridge → MDA supervisor**; (2) the new
  **installed-serving** surface (`TestCalDAVImapServingAfterInstall`) closes exactly that gap — it drives
  the real MSI deploy (the `FaunaNest`/`FaunaBridge` virtual-account SCM services + the supervised MDA's
  loopback auto-enrollment) to the same CalDAV+IMAP round-trip, **PROVEN GREEN on Windows 2026-06-19** (`1 passed, 27s`; perMachine
  install ran elevated on the dev box, volatile data + serialized). So the SCM/supervisor path is now
  **fully exercised green** — the install RUN confirmed the `FaunaNest`/`FaunaBridge` virtual-account
  services start, the supervised MDA loopback-auto-enrolls, and the round-trip completes (it also caught +
  fixed a WiX Bridge-feature install bug that had silently left the FaunaBridge service uninstalled).
- The MSI is **unsigned**, so the SmartScreen/UAC path is untested (signing re-scoped 2026-08-05/10 to
  Store-signed MSIX + SignPath Foundation for the direct-download MSI; Azure dropped — see the
  `## Implementation status today` signing bullet).

**Shell-extension functional verification** (the COM surface actually activating) is win-runnable without
Explorer — tracked in `docs/goal/architecture/apps/windows.md` § Shell Extension.
