# Installer: Windows — target state

Owns: windows-packaging, windows-services
Status: ratified
Authority: Windows distribution — the WiX v6 MSI (feature tree + defaults, per-arch x64/ARM64 packages, major-upgrade/uninstall semantics incl. REMOVE_USER_DATA, `fauna://` registration, shell-extension packaging/registration + the Path-1 side-by-side upgrade contract, the signing plan (Store-signed MSIX; the direct-download signer, Azure Artifact Signing), the direct-download split and its package design (an MSIX bundle for the app and the sync agent, an MSI for the nest, the bridge and the terminal app), dist-profile size policy) and the Windows process model (FaunaNest/FaunaBridge machine services under virtual accounts + failure recovery, the per-user sync logon agent, `device.toml` as Nest↔Bridge artifact-set IPC, FaunaBridge→MDA supervision); defers the app architecture + shell-ext COM behavior to architecture/apps/windows.md (§ Shell Extension, § App Lifecycle), transport trust to architecture/transport-trust.md § Transport trust, per-user credential storage to architecture/long-term-store.md, and build runbook mechanics to `apps/fauna-windows/installer/README.md`.

Last verified: 2026-11-16 (§ Direct download, the status line: the direct bundle's first real-payload run graded OK on Windows, both arches; § Store distribution's licence-files row: first packed for real in it). Prior: 2026-11-16 (§ Direct download, 6: the sparse package retired ahead of the client features — its sources, the `msix` element and the harnesses that lent its identity moved onto the full package — § Shell Extension and § Package identity for FaunaApp.exe rewritten). Prior: 2026-11-16 (§ Direct download, 1: the nest MSI built in source as a package of its own, the release's full-feature MSI left as it is until the bundle leg switches both). Prior: 2026-11-15 (§ Direct download: the signer chosen, Azure Artifact Signing, owner-ruled 2026-11-15). Prior: 2026-11-15 (§ Direct download: the package design ratified, nothing built). Prior: 2026-11-14 (§ Testing, the MSI authoring and Real install rows: the MSI is built by the run that selects either suite, the structure suite's unelevated run included, through the one door both read it by, `conftest.py::msi_path` — so the structure suite runs in the verify queue on Windows). Prior: 2026-09-09 (docs-consistency sweep — re-checked signing status (still no cert; Store MSIX + SignPath, unchanged), sparse-package CAs (`Package.wxs` `RegisterSparsePackage`/`DeregisterSparsePackage` intact), `fauna-ctl` absence, the 9 `.wxs` file count, the 79-test structural-test count, the `dist` cargo profile's single root-`Cargo.toml` location, and the product-version lockstep value (`0.1.2`) — all still held. **One drift found and fixed:** `release.yml` moved to the public repository on 2026-08-31 (a GitHub-hosted `windows-latest` runner now builds `build-windows` on a `v*` tag push, replacing the never-run internal `workflow_dispatch`-only copy), but this doc's Implementation-status and Testing sections still described the old, deleted internal workflow as disabled with no runner configured — the doc's own dependents list in `../release-integrity.md` § *When a release workflow publishes* omitted this file when the port landed. Fixed here.) | Source: `apps/fauna-windows/installer/`

## Goal

A single WiX v6 MSI installer covers Windows distribution end-to-end: feature selection for the per-user sync agent + the nest/bridge machine services, the WinUI 3 desktop app, and Explorer shell integration; per-architecture packages for x64 and ARM64; standard MSI major-upgrade semantics that preserve user data; the machine services running under per-service Windows virtual accounts (sync is a per-user logon agent, not a service); `fauna://` URI handler registration.

**That paragraph is what is built. The target (ratified 2026-11-15, § Direct download (MSIX app + nest MSI)) is two files:** an MSIX bundle carrying the app and the sync agent, the same package shape as the Store's, and an MSI carrying only the nest, the bridge and the terminal app. The Microsoft Store package (§ Store distribution (MSIX)) is the third artifact and the primary channel.

## Implementation status today

**The direct download: target and built state differ (2026-11-15).** § Direct download (MSIX app + nest MSI) ratifies the two-file design. Built the same day, dev-signed: the direct package — the template's `@@DisplayName@@` token, the packer's `--channel direct` switch with its write-virtualization opt-out, the x64 + arm64 bundle, the production-publisher home and its refusal, `just windows-direct-package`, the direct channel's grade, and their structural and packing tests; its real bundle graded OK on Windows 2026-11-16 (§ Direct download, the status line). **The nest MSI is built in source (2026-11-16) as a package of its own beside the full-feature one — § Direct download, 1, the implementation note: `installer/nest/Package.wxs`, its door, builder and structure suite — and the public release still ships the full-feature MSI from the top-level sources until its bundle leg ships both files at once.** **The public release workflow builds both direct-download files unsigned and publishes neither (2026-11-16):** `build-windows` links `Fauna-Nest-Setup-<arch>.msi` from `nest/Package.wxs` and the fragments `windows_installer.py::NEST_WXS` names, one by one, with its own notices (`just windows-nest-msi-notices`), and `ci-smoke.ps1 -Package nest` install-smokes it on both arches beside the full-feature MSI's smoke (its own `Fauna Nest` entry, the services running, the terminal app on the machine Path, no app, no sync agent, no shell-extension CLSID); `windows-direct-payload` builds each arch's payload with `just windows-store-package <arch> full "" "" direct` and `build-windows-direct` packs, self-signs with the dev publisher and grades `Fauna.msixbundle` with `just windows-direct-bundle dev`, the second half of `just windows-direct-package`. Both files are workflow artifacts named `direct-windows-*`, which no publish pattern takes; the release's asset list, `SHA256SUMS`, the cosign bundles and `installer/downloads.json` are unchanged, and the shape tests pin all of it. Two choices made for the cold reader: the payload is two per-arch jobs and the bundle one job after them, because a bundle is one `makeappx bundle` over both arch packages and each arch's chain needs the Store build's per-arch budget; and the direct build gates the `release` job like every other build job, so a red one is seen on the version's run, never first on the run that switches the assets. Not yet seen run: the first public dry run of `release.yml` carrying this is owed. Its install witness is built and green on Windows (2026-11-16, § Testing, the direct-package row); one of its six measurements contradicts the design, a packaged agent's sync roots surviving the package's removal (§ Direct download, 5, *Removal*). Unbuilt: the release workflow's switch to the nest MSI with its bundle as release assets and its signing leg (with the grade's check that the manifest's publisher, the recorded string and the signature's subject agree, and the tier_1 check against `installer/official-apps.json`), and the site change. Until the release ships the nest MSI, § Feature Tree, § Services and the per-user sync agent (the Run-key launch), § Shell Extension (the 8 classes, Path 1), § Protocol Handler, § Upgrade and § Uninstall describe the full-feature MSI the release builds, client features included; the design retires the client features and the overlay badges from it, and the row that retires the full-feature package rewrites those sections. The sparse package retired ahead of them (2026-11-16, § Direct download, 6): its sources are gone and every MSI is linked without it, while the full-feature package's two custom actions that registered it stay until that package retires.

Read this first when scoping installer work. **Current frontier (2026-07-09): every target property below is at least built and in source; most are field-proven on real Windows hardware, but not all — a handful (Windows Firewall inbound allow, MSI major-upgrade runtime) remain authoring-guarded only, pending a live run, not blocked — the headless nest-only install left that list 2026-10-31, `TestNestOnly` green on Windows (§ Testing → Coverage gaps). Two items are unimplemented outright, not merely unverified:** (1) **Signing — the direct-download signer is CHOSEN (owner-ruled 2026-11-15): Azure Artifact Signing signs everything the direct download ships, in the organisation's own validated name; SignPath Foundation is not taken; nothing is signed yet (no signing account, no identity validation), and the direct download becomes an MSIX for the app plus an MSI for the nest, with every shipped Windows executable signed — § Direct download (MSIX app + nest MSI), which owns the decision and its state.** What follows is the August re-scope that the 2026-11-13 reopening and this ruling replace for the direct download, kept as the record of channel (a), which stands, and of the SignPath track (b), which is closed as not taken (2026-11-15; no application was ever submitted): **RE-SCOPED (USER 2026-08-05; hardened 2026-08-10: ALL Azure usage dropped — that hardening is what 2026-11-13 reverses).** No public-trust certificate was to be purchased and no signing path was to use Azure — Azure Trusted Signing / Artifact Signing was cancelled (every public-trust cert path requires gov-ID verification of a human applicant, judged too privacy-invasive; and since 2024 no cert grants instant SmartScreen reputation — it accrues regardless). Two channels: **(a) Microsoft Store (MSIX) — the primary channel**: the Store signs the package itself, so there is no certificate to manage and Store installs never hit SmartScreen; prerequisites are a one-time Partner Center company enrollment (user-side; document/registry-based org verification, no gov-ID — **DONE 2026-08-22, with the app name reserved and the package identity captured**; § Identity & coexistence) and a **Store-ready MSIX package** — a real packaging track, since the artifacts then were the full-feature MSI and the identity-only sparse MSIX (retired 2026-11-16). **The packaging design is RATIFIED 2026-08-10 — § Store distribution (MSIX) below** settles the formerly-open questions (feature subset: Desktop App + per-user sync agent + shell-ext context menu, machine services stay MSI-only; identity coexistence with the sparse package; MSI→Store data continuity); build slices tracked internally — nothing of it is built yet. **(b) SignPath Foundation — signs the direct-download MSI**: free open-source code signing (certificate subject reads "SignPath Foundation"), open to us since the public source was published on 2026-10-06 (no application made yet); eligibility is OSI license + public codebase + released artifacts + active maintenance, and signing runs through SignPath's managed pipeline against the public repo's CI, verified `signtool verify /pa`. **Track state 2026-10-14:** the "Code signing policy" page is authored and renders in the site build with every placeholder filled and is deployed with public-texts round 1's site deploy (2026-10-19), saying the application is being prepared — true before submission; once the owner submits, a public-texts correction moves it to "in progress"; the application's answers are drafted; the public repository's `release` and `release-macos` Environments are protected (2026-10-14: one required reviewer, a single `v*` tag rule, no admin bypass); the already-released artifact SignPath asks for is the 0.1.4 pre-release that every version release's public tag now cuts ([`../release-table.md`](../release-table.md) § Tag gate and release table) — nothing in this track cuts a release of its own, and the application is submitted once that pre-release and the policy page exist; the eligibility question is settled on the works-out-of-the-box side: the self-contained MSI carries the Windows App SDK runtime and the WebView2 loader, whose NuGet license metadata is Microsoft's own terms rather than an OSI license, against SignPath's "no proprietary component / system libraries may be included" clause — the application states them and asks SignPath to treat them as system libraries (a framework-dependent MSI would make the installer depend on a Microsoft-installed runtime, which the product invariants rule out); verified by license metadata only, never by reading package source. The public workflow has built the MSI: the branch dry run on the 0.1.5 attempt's sync branch (public run 39718279069, 2026-10-22) had `supply-chain`, every `build-windows` leg (x64 with the install smoke, arm64) and `smoke-windows-arm64` green, and the `v0.1.5` tag run (public run 39829601949) was green too; the signing wiring follows acceptance. (2) **`release.yml` lives in the public repository**; it triggers on a `v*` tag push or a `workflow_dispatch` naming one, and `build-windows` runs on a GitHub-hosted `windows-latest` runner for both x64 and ARM64 (owner: [`../release-integrity.md`](../release-integrity.md) § *When a release workflow publishes*).

| Property | Status | Evidence |
|---|---|---|
| **Network-reachable nest** — `FaunaNest` binds `0.0.0.0:443` HTTPS off the always-live self-signed floor cert, channel-bound (`served_cert_spki` → `fauna.auth.handshake`); reached as `test@<ip>` via TOFU + channel-binding (architecture/transport-trust.md § Transport trust); clear `WSAEADDRINUSE` messaging on a `:443` bind conflict | Built + verified on Windows 2026-06-20 | the service consumes the shared `fauna_nest::desktop_serve` sequence (floor TLS via `self_signed_cert::listener_tls_from_floor`, floor-renew + cert-watcher tasks — one path shared with the macOS service, priority #2); tier_3 `test_onboarding_self_signed_probe.py`; the installed reach test `test_installer.py::TestInstallThenReach` (claim + pinned health at the box's LAN address on `:443`, authored 2026-11-13, no elevated run yet — § Testing) |
| **Windows Firewall inbound allow** (TCP 443, rides the Nest component — installed/removed with the feature) | In source, authoring-guarded | `ServiceNest.wxs` `firewall:FirewallException`; `TestFirewall` |
| **Headless nest-only install** — Sync optional (`AllowAbsent`), `DataCleanup` re-homed under both features; **`fauna-ctl` removed from the MSI** (USER-ratified 2026-06-20: the only config surface is installer + app) | Built; live on Windows 2026-10-31 — `test_installer.py::TestNestOnly::test_install_nest_only` green in the last-green snapshot (the Nest feature alone installs, `FaunaNest` runs, no app, sync agent or `fauna-ctl` present) | `Package.wxs`; `test_installer_structure.py` (sync deselectable, DataCleanup dual-ref, no `FaunaCtlExe`); `TestNestOnly` |
| **Licence and notices files** — `LICENSE-MIT.txt`, `LICENSE-APACHE.txt` and a generated `THIRD-PARTY-NOTICES.txt` beside the binaries in every install subset ([`../shipped-notices.md`](../shipped-notices.md) § Per channel, the Windows MSI row) | In source 2026-11-13; the arm64 MSI's notices generated on Windows over its real inputs (756 components, nothing refused); **no MSI built with them yet** | `Notices.wxs` (its group on every top-level feature); `just windows-msi-notices`, run by `release.yml`'s `build-windows` and the test MSI's `build_msi`; tier_1 `test_installer_notices.py`; the built-MSI leg `test_installer_structure.py` (`test_the_licence_files_sit_in_installfolder_and_are_not_empty`, `test_every_top_level_feature_carries_the_licence_files`) and the installed leg `TestNestOnly` (present and non-empty, gone after uninstall) are owed a run on Windows |
| **Same-box app default URL** `https://127.0.0.1:3000` — the fixed internal-loopback co-located-IPC port (`CANONICAL_INTERNAL_LOOPBACK_PORT`), not the movable external serving port (nest/common.md § Same-box reach) — + C# self-signed-floor trust | Built 2026-06-20; **live-verified 2026-07-11** (loopback branch) + **2026-07-12** (SPKI-pin branch) — no residue | the 4 `App.xaml.cs` default sites (`DefaultNestPort = 3000`); the `DirectNestClient` trust-callback mechanism is owned by architecture/apps/windows.md. Two tier_3 suites drive a real FaunaApp against a nest serving its self-signed floor over real HTTPS and prove the C# residual-HTTP legs land — **health** (`IsAvailableAsync`) and **blob upload + download** (`UploadBlobAsync` / `GetBlobAsync`, i.e. media) — on **each** branch of the trust callback: `test_self_signed_nest_client_legs.py` (`self_signed_nest`, loopback → the same-box install) and `test_spki_pinned_nest_client_legs.py` (`spki_pinned_nest` — the same floor cert dialled on the box's **LAN IP**, so the authority is non-loopback and the cert is not WebPKI-valid for it, leaving `spki == pin` as the only term that can accept it → the remote `test@<ip>` case, proven by elimination). Trust-decision detail: architecture/transport-trust.md § Transport trust. |
| **Claim-code bootstrap** — minted at first service start into `%ProgramData%\Fauna\nest\claim-code` (a service has no console: the file is the conveyance artifact, redacted from logs) | Built + verified 2026-06-20 | `claim::ensure_claim_code_at`, consumed via the shared `desktop_serve` sequence; the fully-headless zero-box-access conveyance edge is a follow-on, not a blocker |
| **Per-user sync agent** — no `FaunaSync` machine service; per-user HKLM `…\Run` launch of `fauna-sync-agent.exe` (standard token, GUI-subsystem binary); per-session pipe provisioning from the app's vault; per-user state in `%LocalAppData%\Fauna\sync\`; `device.toml` scoped to Nest↔Bridge IPC | In source + tier_3 green 2026-06-20 | `ServiceSync.wxs`; `test_no_faunasync_service`, `test_sync_logon_run_key`, `TestSyncAgentSubsystem`, `test_per_user_sync_agent.py` (2/2) |
| **Bridge feature ships + supervises the Go MDA** (CalDAV + IMAP; stages `fauna-mail-bridge.exe` + gnullvm `fauna_ffi.dll` + `libunwind.dll`; `release.yml` cross-compiles the cgo MDA) | Built; binaries-level e2e GREEN; installed-level e2e **re-pointed at the TLS-only service 2026-11-06, awaiting its elevated re-run** | `ServiceBridge.wxs`; `mda_supervisor.rs`; `test_caldav_imap_serving.py`; `TestCalDAVImapServingAfterInstall` — its 2026-06-19 green (1 passed, 27 s on Windows; surfaced + fixed a WiX Bridge-feature bug and 3 `_build_msi` bugs) predates the service going TLS-only the next day and went stale with it: the test dialled plain `http://127.0.0.1:7450`, `--foreground`'s port, which the service never binds, and timed out there on its 2026-10-28 elevated run. It now provisions over TLS to the same-box loopback `127.0.0.1:3000`, pinned to the floor's SPKI (§ Testing → *Coverage gaps*); that nest leg is green non-installed (`test_caldav_imap_serving.py::test_windows_nest_provisions_over_its_pinned_tls_floor`, Windows 2026-11-06), the installed run is owed |
| **`KillFaunaApp` files-in-use CA** — `[INSTALLFOLDER]`-scoped, **force-only**, immediate CA **after** `InstallValidate` (dev-box-safe by path-scoping): the built-in Restart Manager closes cooperating current-version apps at `InstallValidate` (graceful session-end persists drafts + relaunches via `RegisterApplicationRestart`), so this CA is the pure last-resort net for the windowless / cross-session strays RM can't close | In source + field-proven 2026-06-19; softened to force-only-after-`InstallValidate` 2026-07-15; **RM-softening live-verified on real elevated Windows 2026-07-16** — an over-install (`msiexec /fa`) of a running current-version FaunaApp is closed by the built-in RM *at* `InstallValidate` (`/l*v`: *"RESTART MANAGER: Successfully shut down all applications … that held files in use"*), the unsaved feed-compose draft persists to `drafts.json`, the app relaunches, and `KillFaunaApp` runs right after (seq 1401) as the net with nothing to force-kill; the force-net **body itself** is validated directly (the exact CA PowerShell force-kills `[INSTALLFOLDER]`-scoped `FaunaApp.exe` instances and stays reaped, path-scoped so a build-dir / e2e app is left alone). **Finding:** RM closes *any* FaunaApp with a visible top-level window — cooperative, `FAUNA_E2E_BRIDGE`, and even `MSIRESTARTMANAGERCONTROL=Disable` (not honoured on the command line) alike, verified 3× — so the net is exercised only by genuinely *windowless* (tray-hidden pre-Slice-2c) / cross-session strays; full results in the RM-softening plan | `Package.wxs`; the app-side cooperation that lets RM go first (single-instance, persist-on-close, Restart Manager — Slice 2c) is owned by architecture/apps/windows.md § App Lifecycle |
| **Shell-ext Path-1 upgrade** (side-by-side version-stamped DLL, Explorer never closed) + **`CleanShellDlls` orphan cleanup** | Built + live-verified on real Windows hardware 2026-06-26/27 | § Shell Extension § Upgrade handling; `TestShellExtUpgradeSafety` |
| **`CleanSyncRoots` uninstall CA** — unregisters every persistent `Fauna!*` cfapi sync-root registration (filter + shell/`SyncRootManager`) on uninstall, so Explorer never keeps rendering a ghost cloud location; immediate CA (impersonated, non-elevated — the same context `fauna-sync-agent.exe` itself always runs in), reuses `fauna_cfapi::list_shell_sync_roots` + the product's own ghost-janitor primitives via a new `fauna-sync-agent.exe --cleanup-roots` flag | Built + live-verified on real Windows hardware 2026-07-18 (elevated install → seeded a real shell+filter registration → uninstall → both entries confirmed gone, folder + file intact → reinstall) | `Package.wxs`; `fauna-sync-agent::cfapi_host::unregister_all_shell_sync_roots` (lifted from the windows-only sync service into the cross-platform crate 2026-07-19; `fauna-sync-agent.exe` reaches it through `fauna_sync_agent::run_main()`); `TestCleanSyncRoots` in `test_installer_structure.py`; `docs/goal/behavior/file-sync.md` § Per-file sync-status display (registration persistence) |
| **Install → uninstall lifecycle** — install succeeds, ARP registers; plain `msiexec /x` preserves `%ProgramData%\Fauna\`; `REMOVE_USER_DATA=1` deletes it recursively | Field-verified both paths (user manual test 2026-06-19) — lifecycle COMPLETE | `/l*v` logs + folder state; the x64 and arm64 install/uninstall smokes (`installer/ci-smoke.ps1`) are authored in `release.yml` |
| **Terminal App feature** — `fauna-tui.exe` + `fauna-cli.exe` in `INSTALLFOLDER` beside the agent, each its own component, one machine `PATH` entry (on `fauna-tui.exe`'s component) removed with the feature (`TerminalApp.wxs`; § Feature Tree) | Built; authoring-guarded; in both published MSIs since `v0.1.5`; **the installed `PATH` ran elevated in the public release smokes 2026-11-10** | `test_installer_structure.py` (`test_tui_exe_present`, `test_cli_exe_present`, `test_terminal_binaries_beside_the_agent`, `test_one_machine_path_entry`, `test_terminal_app_has_tui_and_cli`) green on Windows 2026-10-28 over an arm64 MSI built there (84 passed, none skipped); the public `release.yml` dry run 39718279069 and `v0.1.5` run 39829601949 (2026-10-22) built both arches' MSIs with `CliBin` and both smokes installed `ADDLOCAL=ALL` requiring `fauna-cli.exe`, and the published MSIs' tables carry the two components and the one `Environment` row; the installed half — `test_installer.py::TestTerminalAppOnly` (admin: install `TerminalApp` alone, both files and the HKLM `Path` entry present, `where fauna-tui` and `fauna-cli --help` in a shell whose PATH is read fresh from the registry, `msiexec /x` then both gone) and `ci-smoke.ps1`'s `Assert-TerminalAppOnPath` — authored 2026-10-28 with its plumbing run unelevated; the smoke's checks ran elevated in the public `release.yml` run 42797762760 (2026-11-10, green), whose `build-windows (x64, win-x64)` and `smoke-windows-arm64` jobs each print one `Terminal app OK:` line (`C:\Program Files\Fauna` on the machine `Path`, `fauna-tui` found, `fauna-cli --help` runs) and pass `Assert-Uninstalled` after it, so both arches' install-and-remove of the `PATH` entry is witnessed; `TestTerminalAppOnly` itself has not run elevated on Windows, the release smoke standing in for it |
| **MSI major upgrade** (runtime vN → vN+1) | Authoring-guarded only; runtime upgrade untested | `Upgrade`-table guards; § Testing coverage gaps |
| **Build recipe** — `dist` cargo profile, committed `crt-static` for `*-pc-windows-msvc`, self-contained WinUI publish, absolute `wix -d` paths, mail-bridge cross-build | Proven locally: 506-file, 86.6 MiB ARM64 MSI (2026-06-20) | `apps/fauna-windows/installer/README.md` § Building from a clean checkout; encoded in `release.yml build-windows` |
| **Every MSI leaves the sparse package out** (`-d NoSparsePackage=1`, `ShellExt.wxs`'s `<?ifndef NoSparsePackage?>`) — the public release since its 0.1.5 dry run (an unsigned sparse package never registers on a user's machine), and the dev builder since the package retired with its sources (§ Direct download, 6), so nothing can build one; the Windows 11 default-menu entry is the full MSIX's, and the full-feature MSI's HKCR verb ("Show more options") ships as before | Release since 2026-10-21; dev builder since 2026-11-16 | `release.yml` Build MSI; `test_public_workflows_shape.py::test_the_release_msi_build_defines_every_installer_variable`; `helpers/windows_installer.py::build_msi`; `test_installer_structure.py::TestSparsePackageRegistration::test_no_sparse_msix_ships` |

**Ruled 2026-10-01 — the MDA CalDAV listener interface is a constant, and the bridge service's `config.toml` is retired. Built.** The interface is not a choice anyone makes: a desktop nest's CalDAV listener serves the calendar and mail apps on the box's network, so it binds every interface — which the macOS supervisor has always hard-coded (`0.0.0.0` at the default port). It is now one constant, `fauna_mda_supervisor::CALDAV_LISTEN` in the shared `libs/fauna-mda-supervisor`, used by both desktop supervisors; `BridgeConfig`, its `%ProgramData%\Fauna\bridge\config.toml` and the pipe `Configure` request (with `fauna-ctl`'s `bridge configure`) are deleted. This replaces the earlier target (the interface as app-set nest state). The **port** is the admin's choice and stays one: the admin sets it from any app (`fauna.bridges.set_caldav_port` → nest state → the `/data/caldav-port` flag-file mirror the supervisor reconciles each tick, restarting the MDA on change — owner behavior/caldav-server.md); the supervisor-*written* `operator-hatch.toml` the Go MDA reads stays artifact-set IPC, never human-edited.

**Known small gaps:** per-user `%LocalAppData%\Fauna\` removal is deliberately out of installer scope (design decision 2026-06-05 — a perMachine MSI runs as SYSTEM and can't reach user profiles; delete affordances live in the user's app, and the in-app affordance is a apps/windows.md follow-on). The Add/Remove-Programs product icon gap is **fixed 2026-07-20**: `<Icon Id="FaunaIcon">` + `ARPPRODUCTICON` now point at `AppIcon.ico`, rendered from Fauna's one brand mark (`sites/fauna-social/public/favicon.svg`, via a dedicated icon-rendering script) — the same file `FaunaApp.csproj`'s `<ApplicationIcon>` uses for the .exe's own taskbar/Alt-Tab/Explorer icon. **The rendered-from claim went stale once and is now gated (2026-08-13).** Between 2026-07-29 and 2026-08-13 it was false: a mark change moved the SVG outside the subset the dev-fleet rasterizer accepted, so both this `.ico` and the MSIX logos below kept shipping the *previous* mark while the doc claimed otherwise. The rasterizer now reads `circle` / `ellipse` / paths of `M L H V C Q Z` (absolute and relative), `AppIcon.ico` + the three MSIX logos (`installer/store/Assets` since 2026-11-16, when the sparse package that first carried them retired) are regenerated from the current mark, and the cheap merge gate `just brand-mark-check` blocks any future mark the rasterizer cannot read — so the claim can no longer rot silently. **Visual confirmation on real Windows is DONE (2026-08-25/26), by two independent checks.** The shipped `AppIcon.ico` was rendered through the actual OS icon pipeline (`System.Drawing.Icon`/GDI+, the same code path Explorer, the taskbar, and Add-Remove-Programs' `ARPPRODUCTICON` all use) at both its 16×16 frame (the taskbar/small-icons size the rasterizer fix targeted) and its 32×32 frame (the Add-Remove-Programs size); both read clearly as the beaver mark — round head, ears, eyes, muzzle — not a blob, teeth detail naturally lost at 16×16 but the silhouette stays legible; the three MSIX logos (`Square44x44Logo`/`Square150x150Logo`/`StoreLogo`) were inspected directly and match. The live user then confirmed the same on the built `.exe` in Explorer/taskbar/Alt-Tab ("Looks good"), and separately flagged that the mark's current margin within its 32×32 canvas leaves room for the OS to pad the icon with a background plate (blue on Windows, light grey on macOS) and that the mark reads a little small — a canvas-fill refinement, not a rasterizer defect (the rendered `.ico`/PNG artifacts are confirmed fully transparent outside the mark's own shapes, pixel-decoded not just file-diffed), tracked in the windows work queue.

**Dated history:** first real end-to-end build 2026-06-15 (three missing `workspace.dependencies` + stale service config fixed in source; `crt-static` committed — without it a bare fresh VM hits MSI 1920 on `VCRUNTIME140.dll`; recipe blockers encoded in `release.yml`); per-user sync agent ratified 2026-06-19, e2e-guarded 2026-06-20; network-reachable nest ratified + landed 2026-06-20; `fauna-ctl` removed from the MSI 2026-06-20 (USER) and deleted with the bridge pipe 2026-10-02; install + both uninstall paths field-validated 2026-06-19; Nest feature default-OFF ratified 2026-06-21; Path 1 ratified 2026-06-25, implemented + live-verified 2026-06-26/27; Authenticode blocked on org registration recorded 2026-06-27.

## Installer Technology

The Windows installer is built with **WiX v6** (`wix` .NET global tool) and produces a standard MSI package. The `.wxs` source uses the WiX v4-era schema namespace (`http://wixtoolset.org/schemas/v4/wxs`), which the v6 toolset still consumes. Separate packages are built for x64 and ARM64 from the same source files.

**The direct download becomes an MSIX bundle for the app beside an MSI for the nest (owner-directed 2026-11-13, reversing the 2026-11-12 ruling "the direct download stays an MSI; no downloadable MSIX").** The 2026-11-12 ruling named what a downloadable MSIX costs: a certificate of its own, a third package identity, and no machine services or overlay badges. The owner reopened it the next day for the install a signed MSIX gives; § Direct download (MSIX app + nest MSI) owns the design and answers each cost. Until it is built, the direct download is the MSI this section describes.

### Source Files

All installer source files live in `apps/fauna-windows/installer/`:

| File | Purpose |
|------|---------|
| `Package.wxs` | Top-level package definition, product metadata, upgrade logic — the full-feature MSI the public release builds (the workflow links every top-level `*.wxs`) |
| `nest/Package.wxs` | The nest MSI's package (§ Direct download, 1): `Fauna Nest`, the same `UpgradeCode`, the Nest (default on, with Bridge under it) and Terminal App features, no custom action; linked with the shared fragments `Directories`, `ServiceNest`, `ServiceBridge`, `TerminalApp`, `Notices` and `UI` and never with the client ones (`App`, `ServiceSync`, `ShellExt`, `Protocol`), the list `tests/e2e-unified/helpers/windows_installer.py::NEST_WXS` names. Its subdirectory keeps it out of the release workflow's top-level glob until the workflow builds it |
| `App.wxs` | Desktop app component (FaunaApp.exe and .NET dependencies) |
| `Directories.wxs` | Directory structure under `%ProgramFiles%` and `%ProgramData%` |
| `ServiceSync.wxs` | Sync service component and Windows service definition |
| `ServiceNest.wxs` | Nest service component and Windows service definition |
| `ServiceBridge.wxs` | Bridge service component and Windows service definition |
| `ShellExt.wxs` | Shell extension DLL, COM registration, and overlay icons |
| `Protocol.wxs` | `fauna://` URI protocol handler registration |
| `UI.wxs` | Optional user-data removal on uninstall (`REMOVE_USER_DATA`); the `DataDirRegistry` component + `RemoveFolderEx`. (Feature-selection UI itself is `WixUI_FeatureTree`, referenced in `Package.wxs`.) |
| `Strings.wxl` | Localizable UI strings consumed by the WiX UI extension (the welcome page's `WelcomeDlgDescription`); every build passes it with `-loc`, or the wizard shows WiX's stock text |
| `License.rtf` | The licence page of the wizard, named by `WixUILicenseRtf` in `Package.wxs` (unset, the stock dialog set shows WiX's placeholder licence); its words, and `Strings.wxl`'s, are a public text ([`../public-texts.md`](../public-texts.md)) |
| `Notices.wxs` | The licence files every shipped binary carries: `LICENSE-MIT.txt`, `LICENSE-APACHE.txt` and the generated `THIRD-PARTY-NOTICES.txt`, under `INSTALLFOLDER`, read from `$(var.NoticesDir)` — the directory `just windows-msi-notices <dir> <arch>` writes (the notices resolved for that arch's triples, as one MSI ships one arch), which every MSI build runs first (`release.yml`'s assembly job and the test MSI's builder, `tests/e2e-unified/helpers/windows_installer.py::build_msi`). Its component group rides every top-level feature, so any install subset carries them. What the files hold and how the notices file is produced → [`../shipped-notices.md`](../shipped-notices.md) |
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
Every MSVC artifact, at every profile, links with the Rust toolchain's own `rust-lld`
([`../build-target-layout-windows.md`](../build-target-layout-windows.md) § UniFFI cdylib rebuild cost
(win) → *Linker verdict* owns the choice and its verification). The MSI cabinet uses `CompressionLevel="high"` (LZX) on `<MediaTemplate>` in `Package.wxs`.

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

Minimum supported OS: the target floor is derived from WinUI 3 / Windows App SDK requirements (Windows 10 1809+); the test reports on file cover **Windows 11 ARM64** (the Windows box), and the x64 and arm64 install/uninstall smokes are authored in `release.yml`. No test report is filed for a Windows 10 install.

## Feature Tree

Users can select which components to install through the installer UI. The tree below shows the default state of each feature.

**Nest Service is default OFF** (USER-ratified 2026-06-21): the Windows desktop is primarily a *client*, so a default install is Sync Agent + Desktop App connecting to a remote nest by handle (`test@<ip-or-domain>`). The user opts into self-hosting a nest on this box by ticking **Nest Service** in the installer (or `ADDLOCAL=Nest`).

**Explorer Integration is default ON** (USER-decision 2026-06-23): overlay icons + context menus ship by default (works-out-of-the-box); a user who doesn't want them unticks **Explorer Integration** (it requires **Sync**, its parent).

**Terminal App is default ON** (2026-09-26 — the MSI carrying `fauna-tui.exe` is the user's 2026-09-26 channel ruling, [`tui.md`](tui.md) § The ratified channel; the feature placement is this doc's): `TerminalApp.wxs` ships `fauna-tui.exe` in `%ProgramFiles%\Fauna\` beside `fauna-sync-agent.exe` (the app resolves the agent as its sibling) and appends that directory to the machine `PATH`, so `fauna-tui` is typeable in any shell — the one PATH entry the package writes, removed with the feature. Since 2026-10-18 the same feature ships `fauna-cli.exe` beside it (the cli app's device shell, the archive's third member — [`tui.md`](tui.md) § Decisions, decision 4) as a second component with no PATH row of its own; the MSI is built with it (§ Implementation status today). A **top-level** feature rather than a child of Sync or Desktop App: a headless nest box managed over SSH wants the terminal app and neither of those, and unticking either must not take it away. The Store MSIX does **not** carry it (user ruling 2026-09-26).

> **Shell-ext upgrade model — tombstone (superseded designs).** The in-place *close-Explorer-and-restart-it* designs — the `RestartExplorer` CA (with its `sihost.exe` interactive-user derivation), the **B1** `AutoRestartShell`-suppression CAs, and the **B2** `Schedule="afterInstallExecute"` experiment — were each **disproven under live RDP testing on Windows (2026-06-23 → 25): they strand the desktop** (the Restart Manager closes Explorer during `InstallValidate`, before any deferred suppression can run). They are superseded by **Path 1** (version-stamped side-by-side shell DLL; Explorer never closed; USER-decision 2026-06-25), implemented and **live-verified 2026-06-26/27** together with its `CleanShellDlls` orphan cleanup — both DONE; the sole shell-ext remainder is nothing (the old throwaway-VM gate was inherited from the strand-prone designs and does not apply to Path 1). § Shell Extension § *Upgrade handling* below is the single current story; the forensic history is tracked internally.

**Applying a shell-ext change needs only a sign-out, not a reboot** (Path 1, USER-decision 2026-06-25). Because a changed DLL ships under a *new* filename and the install never touches the in-use old file, Explorer simply loads the new DLL the next time it starts — **sign out/in, or restart Explorer**. A full reboot is **not** required (Windows would only swap a same-named *in-use* file at boot, via `MoveFileEx`/`PendingFileRenameOperations` — the mechanism Path 1 deliberately avoids, since a logout doesn't flush that queue). The previously-described live-restart machinery (`RestartExplorer` + the `AutoRestartShell` suppression CAs) is **removed** as part of Path 1 — it was proven to strand the desktop. We are **not** dropping the shell extension to dodge the installer difficulty.

```
Fauna (root)
├── Sync Agent (optional, default ON — per-user logon process, NOT a service)
│   ├── fauna-sync-agent.exe (launched at each user's logon, standard token)
│   └── Explorer Integration (optional, default ON — overlay icons; side-by-side versioned DLL, never closes Explorer on upgrade)
│       └── fauna_shell_<ver>.dll + overlay icons + context menus
├── Terminal App (optional, default ON — the same fauna-tui.exe + fauna-cli.exe the release archive carries; tui.md)
│   ├── fauna-tui.exe beside fauna-sync-agent.exe + a machine PATH entry for %ProgramFiles%\Fauna
│   └── fauna-cli.exe (the cli app's device shell; its own component, no PATH row of its own)
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
%ProgramFiles%\Fauna\        ← binaries (fauna-sync-agent.exe, fauna-tui.exe, fauna-cli.exe, fauna-nest-svc.exe,
│                                         fauna-bridge-svc.exe, FaunaApp.exe, fauna_shell_<ver>.dll,
│                                         overlay icon files, .NET runtime dependencies,
│                                         LICENSE-MIT.txt, LICENSE-APACHE.txt, THIRD-PARTY-NOTICES.txt)
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
(`apps/windows.md` § Shell Extension owns the behavior). The full MSIX's manifest
declares folder support too — `FileExplorerContextMenus` carries `*`, `Directory`,
*and* `Directory\Background` item types (`apps/fauna-windows/installer/store/AppxManifest.xml.in`;
pinned by `test_store_package.py::test_registers_the_verb_for_files_and_folders`, as the
retired sparse manifest did from its first commit, 2026-07-14). The Win11
default-menu *render* on a folder specifically is live-observed (2026-07-22, through the
sparse registration then) — see `apps/windows.md` § Shell Extension → *Folder context menu*
for current verification status.

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

**Menu placement — the full package.** A verb + `ExplorerCommandHandler` surfaces in the **legacy**
("Show more options") menu only. The Windows 11 **default** context menu renders context-menu
handlers only from a component that carries **package identity** — an MSIX
`desktop4:FileExplorerContextMenus` registration, never `HKCR`. (Normative: *Custom context
menu extensions* is listed under "features that only work in apps that have package identity" —
[Packaging overview](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/packaging/).)

Fauna's one such registration is the full MSIX's — the Store package and the direct-download
bundle, one template (`apps/fauna-windows/installer/store/AppxManifest.xml.in`; § Store
distribution (MSIX), § Direct download (MSIX app + nest MSI)):

- **One implementation, two registration surfaces.** The manifest's `com:Class` and every
  `desktop5:Verb` name the **existing** root `IExplorerCommand` CLSID (`{4a7b8c10-…}`) — the same
  class `ShellExt.wxs` registers as `*\shell\Fauna`. **No 9th CLSID exists or may be minted**;
  pinned by `test_store_package.py::test_reuses_the_existing_root_clsid_no_ninth_guid`, which reads
  the expected GUID out of `ShellExt.wxs` rather than restating it. While the full-feature MSI's
  sources stand, its HKCR verb owns the legacy menu and the package the default menu; with the
  MSI's client features gone the packaged registration is the one surface (§ Direct download, 6).
- **The DLL is inside the package.** `com:Class/@Path` is the bare `fauna_shell.dll`, resolved
  inside the package; Explorer activates it out-of-process via the COM surrogate (`dllhost.exe`),
  which is what carries identity into the handler. An MSIX is replaced atomically, so the Path-1
  version-stamped name the MSI needs (§ Upgrade handling) does not apply.
- **Observed without an install.** The headless context-menu observation harness (a dev-box
  tool, `apps/windows.md` § Shell Extension) lays out the direct-download package around a build
  output's DLL with the packer's `--stage-only`, registers it loose under Developer Mode under a
  test-only identity, and reads "Fauna" in both menus on a tracked file and on its folder (first
  read so 2026-11-16 on Windows: `apps/windows.md` § Shell Extension, the default-menu row).

**The sparse package — retired 2026-11-16 (§ Direct download, 6).** Until then the MSI shipped
`Fauna-Sparse.msix`, a manifest-only package (`uap10:AllowExternalContent`) registered per machine
with `Add-AppxPackage -Stage -ExternalLocation <INSTALLFOLDER>` + `Add-AppxProvisionedPackage
-Online`, whose one job was to lend the MSI-installed shell extension and FaunaApp.exe a package
identity. The Win11 default-menu entry was observed rendering through it (2026-07-15): one "Fauna"
in the default menu and the HKCR verb's in "Show more options", never two in one menu, since a
Win11 `IExplorerCommand` registered the classic way is demoted to "Show more options". Its
template, build script and structural suite are gone, its logos moved to `installer/store/Assets`,
and every MSI is linked `-d NoSparsePackage=1`, which leaves `ShellExt.wxs`'s package component out.
The full-feature package's `RegisterSparsePackage` / `DeregisterSparsePackage` custom actions stay
in `Package.wxs`, pinned by `test_installer_structure.py::TestSparsePackageRegistration`, until that
package's sources retire: with no package beside the shell DLL the
register action does nothing, and the deregister action still removes a registration an earlier
install left.

### Package identity for FaunaApp.exe (2026-09-26; one identity since 2026-11-16)

**FaunaApp's package identity is the full MSIX's own.** A full package grants identity to every
process started in its context, so the Store package and the direct-download bundle each give the
app its AUMID (`<PFN>!FaunaApp` — both declare `Application/@Id` `FaunaApp`). What identity unlocks
first is the taskbar badge — windows' home-screen widget ([`../apps/windows.md`](../apps/windows.md)
§ Home-screen widget owns the surface; badge notifications are keyed on the AUMID and refused
without one). An unpackaged FaunaApp.exe — a full-feature MSI install, a build output — runs
without identity, and its badge is not painted. The app's embedded manifest carries no `msix`
element: that element bound an exe to an external-location package (measured 2026-09-26: without
it, an exe under a registered external-location package reported "no package identity",
`0x80073D54`) and went with the sparse package (§ Direct download, 6);
`test_package_identity_publisher.py` pins its absence.

**A full package's identity reaches only a process started in its context** (measured on Windows
2026-09-28, `GetPackageFullName` on the process): an exe started directly from a registered
layout's folder runs with **no** identity, while the same exe started through
`Invoke-CommandInDesktopPackage` — or, for a user, from the package's Start entry — runs with the
package's. So an install badges through its own activation, and the e2e harness registers a Debug
build as a full package under a test-only identity
(`tests/e2e-unified/helpers/taskbar_badge.py::registered_package`, the packer's own manifest and
layout) and starts the app the same way (`flaui-bridge/SessionManager.cs::StartInPackageContext`).

**Identity changes how the Windows App SDK starts.** `WindowsPackageType=None` generates an
auto-initializer that calls the bootstrapper, and the bootstrapper *fails* in a process that already
has package identity — measured 2026-09-26, FaunaApp.exe exited `0x80070032` (`ERROR_NOT_SUPPORTED`)
the moment it ran under a package's identity. `FaunaApp.csproj` therefore sets
`WindowsAppSDKBootstrapAutoInitializeOptions_OnPackageIdentity_NoOp`, which makes the bootstrapper a
no-op under identity and leaves the identity-less path unchanged; the full package needs it. Under
identity the Windows App SDK runtime must then already be in the process's package graph: the
release publish is self-contained (`WindowsAppSDKSelfContained=true`, installer README § step 4 and
the release workflow), so it carries the runtime beside the exe and the shipped manifest declares
**no** framework dependency — a box without the framework could not install a package that did.
Only the framework-dependent Debug build needs one, and the e2e harness adds it to the packages it
registers.

**The dev publisher DN has one build-time home: `apps/fauna-windows/installer/PackageIdentity.props`**
(`FaunaPackagePublisher`). A package's `Identity/@Publisher` must equal its signing certificate's
Subject DN and feeds the package family name, so a drifting copy is silently another app, and no
consumer keeps one: the store-package build script's dev default and the e2e harness's registered
packages read the file. The **Store** channel's production publisher is outside this home: it is the
Partner Center identity (§ Identity & coexistence), passed to the store build explicitly; the
direct-download package's production publisher is the second property of the same file (§ Direct
download, 5). Pinned by `test_package_identity_publisher.py`, which also fails on the dev DN literal
appearing anywhere else under `apps/fauna-windows`, `scripts` or `tests/e2e-unified`.

The two pieces the 2026-09-26 design left open — the MSI Start-menu shortcut's AppUserModelID, and
an elevated MSI install proving the sparse path (`Package.Current` inside the installed FaunaApp) —
closed with the sparse package: an MSI-installed FaunaApp has no package identity to group on or to
prove.

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

A GUI uninstall offers the same removal as a checkbox on the wizard's Remove path (`UI.wxs`'s
`RemoveDataDlg`, which only sets `REMOVE_USER_DATA`), and only where there is something for it to
delete: the dialog is reached when the install recorded its data folder (the `DataDir` value
`DataDirRegistry` writes, read back into `FAUNA_PROGRAMDATA_DIR`). That component rides only the
features that make machine data — the nest MSI's Nest, the full-feature package's Sync and Nest — so
a box that took the terminal app alone goes straight from Remove to the confirm page.

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

**Alpha visibility — a private audience, not a public listing (ruled 2026-10-07).** The alpha submission is published with Partner Center's *Private audience* visibility: a
known-user group holding the alpha testers' Microsoft accounts, created under the account's *Audience*
page and chosen on the submission's *Visibility* step. The package still passes certification;
a member opens the listing through the private-audience link Partner Center issues (the App identity
page's sign-in-required URL, not the plain listing address of § Identity & coexistence), while
nobody outside the group can view or install it even with that address; and the Store serves each
member the package for their device's architecture — so the x64 package joins the
same submission beside arm64 before the group is widened. Going public is a later submission that
changes the visibility; the official-apps manifest's `microsoft-store-listing` entry flips with that
public submission, not with the private one ([`../release-integrity.md`](../release-integrity.md)
§ Release signing → *The official-apps list*, the listing rule). A version's private-audience
submission goes up only after the nest of that version is on `:test` and the acceptance nest
fauna.build serves it, and the public submission only after the nest promote to `:latest` (ruled
2026-10-13; the order is the release table's — [`../feature-catalog.md`](../feature-catalog.md) § Tag
gate and release table). Both audiences pass certification; the states a submission passes
through, where its live state is recorded (`installer/store-review.toml`) and what the
organisation does while a review is open are [`store-review.md`](store-review.md)'s. Testers' accounts live in the group
only — never in a file. Which commit the uploaded package is built from → the same section's *A store
upload is built from a recorded public commit*.

**What the Store publishes about the organisation** — the customer-contact set a Company account must
provide and the support email, and the fact that the Company account type is itself the DSA trader
declaration — is owned by [`store-listing-contact.md`](store-listing-contact.md) § Microsoft Store; a
Partner Center answer that changes listing content is read back before it is recorded (that doc's § The
readback rule).

**Flavour — a store flavour: store-safe, then store-no-payments (user-ruled 2026-10-14, superseding
the full build ruled 2026-10-07).** The Store package was first built with the `store-safe` argument to `just
windows-store-package`: the payments plane is compiled away ([`../dynamic-features.md`](../dynamic-features.md)
§ The App-Store escape hatch), and zaps and the cross-user (in-person) share plane go with it, since the flavour excises
all three — `P2P_SHARE` off under `/p:FaunaStoreSafe=true`, the sync agent built `--no-default-features`; the transfer between
the user's own devices stays, its peer leg riding `account-runtime`, not `p2p-share` (`dynamic-features.md`, the `p2p-share` member). That is the same flavour as Apple's store archive ([`ios.md`](ios.md)). The reason is Microsoft
Store Policies 10.8. A non-game PC app may take payment through its own secure third-party purchase
API only if it names the commerce provider at each transaction, authenticates the user, confirms the
transaction and declares the API in Partner Center. Fauna's payments cannot name a provider, so the
Store build carries none, and the submission declares no in-app purchases. The direct-download MSI
keeps the full default-feature build. A return to the full flavour on the Store is a new owner ruling,
shipped as a reship of the Windows package alone ([`../product-version.md`](../product-version.md) § Reships). **`0.1.4` on the Store
(user-ruled 2026-10-14): it ships without payments, or it does not ship there.** No `0.1.4` Store
submission carries the payments plane. If no payments-free package of that version passes its grade, the
Store skips `0.1.4`, and the direct-download MSI ships the version alone. The owner then chose (same day) to ship `0.1.4` on the Store
as `store-safe`, not to wait for a flavour that keeps peer-to-peer sharing. **From `0.1.5`, or an earlier
Windows reship, the Store package is `store-no-payments` (owner-ruled 2026-10-14, recorded 2026-10-17).**
That flavour excises payments and zaps and keeps peer-to-peer sharing
([`../dynamic-features.md`](../dynamic-features.md) § The App-Store escape hatch → *The store flavors*),
so sharing returns to the Store. Its Windows leg is built (2026-10-27): the per-member MSBuild switch,
`windows-store-package`'s `store-no-payments` value, and the public Store workflow's `flavour` input.
A package is built from the commit its version records, so a version ships `store-no-payments` once
its recorded commit carries that leg. `0.1.5`'s does not, and `0.1.5` is tagged but not promoted
([`../product-version.md`](../product-version.md) § Current values), so the first Store package of
the flavour is `0.1.6`'s. The workflow's `flavour` input defaults to `store-no-payments`, and the
workflow refuses that flavour for a recorded commit that predates the leg; such a commit is
dispatched with `flavour=store-safe`.

**The workflow builds with the recorded commit's own recipes (2026-10-28), so it builds only a
commit whose recipes run on its x64 hosted runner.** Each arch runs `just windows-store-package
<arch> <flavour> <identity> <publisher>` from the recorded tree
([`../release-integrity.md`](../release-integrity.md) § What a release ships → *Every release
build runs the `just` recipe*). A tree older than the hosted-runner recipes has a
`scripts/cargo-win.cmd` that assumes an arm64 host and a `windows-store-package` that builds arm64
as the implicit host, which on that runner would pack an x64 build under the arm64 name. The
workflow refuses such a commit before it builds anything, and `0.1.5`'s recorded commit is one of
them. Its packages, in either flavour, can still be built from a public clone at that commit on an
arm64 Windows machine, where those recipes run as written (`just windows-store-package <arch>
store-safe FaunaSocial.FaunaSocial "<publisher>"`), and graded with the packer's `--grade`. The
first package the workflow can build is from a commit carrying these recipes, so `0.1.6`'s.

**The listing's text is a public text, approved in a round (2026-10-19).** The description and the
short description are one text for every store, the public-texts entry `store-listing-main-msstore`, approved in public-texts round 1. Who enters it in
the *Store listings* step, when, and its readback are [`../public-texts.md`](../public-texts.md)
§ Rulings, 15 and 16.

**The listing's screenshots are produced by the harness, never by hand (2026-10-14).** Partner Center
completes a submission's *Store listings* step only with a description and at least one screenshot, a
private-audience submission included. `just e2e-store-listing-screenshots --app windows` (opt-in,
`--store-listing`) runs the real app on injected fixture content, invented people at reserved domains,
and captures the app's own window, never the screen, through the FlaUI bridge's window capture: the
window sized to 1440 × 900 device-independent pixels at its own DPI and rendered alone, so another
window on the box cannot land in a listing image. Each PNG is checked against the desktop limits: at
least 1366 × 768, at most 3840 × 2160, under 50 MB. The render needs an attached desktop: a session
that `query session` shows as `Disc` composes no app content, and the window comes back as its title
bar over a flat client area. So the run skips in a disconnected session, and the bridge refuses any
capture whose client area is one flat colour; a blank shot is never written over a kept one. The harness drives the Debug build, which carries
every plane the store package leaves out, so the fixture sets nothing that paints them (no tips, no
prices, no paid tiers, and not the folders page, whose offline-share section paints whenever that plane
is compiled in), and a shot shows only what the app does for real: no named Bluesky author, which the
feed does not paint yet.

### Feature subset — what the Store package carries

Verified per-component against the packaged-app constraint set (2026-08-10):

| MSI feature | Store MSIX? | Verified constraint |
|---|---|---|
| Desktop App (WinUI 3) | **IN** | standard `runFullTrust` win32App; reuses the MSI's self-contained publish output (verify WASDK self-contained-inside-MSIX at build time) |
| `fauna://` handler | **IN** | `uap:Protocol` manifest extension |
| Per-user sync agent | **IN** | `windows.startupTask` desktop extension replaces the HKLM Run key (`Enabled="true"` is honoured for full-trust desktop apps once the app has launched once; user controls it in Task Manager — [StartupTask](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask)) |
| Explorer Integration — context menu | **IN** | `desktop4:FileExplorerContextMenus` + `com:ComServer` surrogate — the mechanism the retired sparse package used, with `com:Class/@Path` resolving *inside* the package (bare `fauna_shell.dll` name; Path-1 version-stamping is an MSI-lock concern that does not apply inside MSIX) |
| Explorer Integration — 4 overlay badge handlers | **OUT** | `ShellIconOverlayIdentifiers` is an HKLM-only registration with **no MSIX manifest surface**; a Store app cannot write HKLM. Accepted absence: cfapi renders placeholder state natively in Explorer's Status column; corner badges remain an MSI-install nicety |
| Nest Service | **OUT — MSI-only, permanent** | MSIX services run only as `localSystem`/`localService`/`networkService` ([desktop6:Service](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop6-service)) — **no virtual accounts**, so shipping it Store-side would abandon the ratified `NT SERVICE\FaunaNest` least-privilege shape; the `0.0.0.0:443` bind + firewall rule + `%ProgramData%` layout is the opt-in self-hosting path the direct-download MSI serves |
| Bridge Service | **OUT — MSI-only, permanent** | same, plus it requires Nest |

The Store package is therefore **the app**: Desktop App + sync agent + Win11/legacy context menu,
connecting to a remote nest by handle — exactly the MSI's default feature selection, minus overlay
badges. Self-hosting stays a direct-download MSI decision.

### Identity & coexistence

- **Identity comes from Partner Center**: reserving the app name mints `Identity/@Name` and
  `Publisher` (`CN=<GUID>`), the manifest must match them exactly, and the Store re-signs the
  package ([reserve your app's name](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/reserve-your-apps-name)).
  The manifest therefore ships as a **template**, with `@@IdentityName@@` joining
  `@@Publisher@@`/`@@PackageVersion@@`.
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
- **Identity is native to a full package.** The sparse package that once lent an MSI install its
  identity (`FaunaSocial.Fauna`, a different family from the Store's by construction, confirmed at
  reservation) retired 2026-11-16 (§ Direct download, 6); its name is left unused.
- **Supported topologies:** (1) Store app alone — the normal client box; (2) MSI alone — any
  feature selection; (3) **Store app + nest-only MSI** — the recommended both-channels box for a
  self-hosting user who wants Store-updated apps. (4) Store app + MSI *with client features* is
  **discouraged but not blocked**: two "Fauna" context-menu entries (the Store package's in the
  Win11 default menu, the MSI's HKCR verb in "Show more options"; two in the default menu while the
  MSI still registered the sparse package), both `fauna://` registrations exist, and both launch paths
  fire at logon — the app's single-instance invariant (apps/windows.md § App Lifecycle) collapses
  the app; **the agent's second-instance behavior is VERIFIED GREEN on Windows 2026-08-22** (below).
  An optional hardening slice teaches the MSI UI sequence to pre-deselect client features when the
  Store PFN is registered.
- **Two logon agents, one survivor — verified across the package-identity boundary (2026-08-22).**
  Both hooks launch `fauna-sync-agent.exe`, and `fauna_ipc::pipe_transport::InstanceLock` (then `pipe_server::InstanceLock`) picks the survivor: the
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
    narrower than stated above rather than wider — either way the heal already covers it. Half of
    it is measured (2026-11-16, § Direct download, 5, *Removal*): once the package is removed, an
    unpackaged `Unregister` of its root answers S_OK and keeps the key, while `CfUnregisterSyncRoot`
    removes the filter half. With the package still installed, it is not measured.

### Build & packaging shape

- **The store-package build script** — the one packer of both full-MSIX channels:
  reads version out of `Package.wxs` (4-part), stages a full layout (the shipped-flavour app publish
  output + `fauna-sync-agent.exe` + `fauna_shell.dll` + Assets + the three licence files at the
  package root, [`../shipped-notices.md`](../shipped-notices.md) § Per channel), substitutes
  `apps/fauna-windows/installer/store/AppxManifest.xml.in`, `makeappx pack` (no `/nv` — content is
  internal, path validation SHOULD run), per arch: **x64 + ARM64**, two `.msix` files per release.
  - **Five tokens:** `@@IdentityName@@`/`@@Publisher@@`/`@@PackageVersion@@`, plus
    **`@@ProcessorArchitecture@@`** — a full MSIX carries native binaries, so each of the two
    packages must declare the arch it carries; one `neutral` package would claim to run everywhere
    while shipping one arch's code — plus **`@@DisplayName@@`** (2026-11-15), `Properties/DisplayName`,
    the one field the direct-download channel names differently (§ Direct download, *One template,
    one packer, two channels*); the packer sets it per channel. The Store channel's manifest is the
    template's own text with the five values in place, nothing inserted
    (`test_store_package.py::test_the_store_rendering_is_the_template_with_its_tokens_substituted`).
  - **`--payload-root`** selects the staged payload (default `build/installer/store-payload/<arch>/`).
    It is what lets the packaging mechanism be tested headlessly in seconds against stub payload
    files, instead of behind a ~30-minute self-contained publish.
  - **`--stage-only <dir>`** (2026-11-16) lays out one arch's package in `<dir>` and stops — no
    pack, no signature, no bundle — for a loose `Add-AppxPackage -Register` under Developer Mode:
    the context-menu observer's registration (§ Shell Extension, *Menu placement*). The e2e
    harness calls the same `render_manifest` and `_stage` in-process.
  - **The payload an upload carries is built by `just windows-store-package <arch>
    [full|store-safe|store-no-payments] [<identity-name> <publisher>] [store|direct]` (2026-10-07; the third flavour
    2026-10-27; the channel 2026-11-15, `store` by default, `direct` staging the direct package's
    `full` payload without packing it — § Direct download)** — the production FFI (`windows-ffi dist`, never
    `windows-ffi-test`), `fauna-sync-agent` and `fauna-shell-ext` as two `--profile dist --locked`
    builds (§ Size & build profile), a self-contained Release publish of the app with `bin/`+`obj/` wiped first, staged
    into that default root, then packed; both arches. It is a recipe of its own because the only
    other local payload builder, the MSI test helper (`windows_installer.py::build_msi`, staging
    `build/installer/stage/<arch>/`), compiles the e2e automation surface in on purpose.
  - **The packer refuses a payload that would ship broken or test-flavoured**: one without
    `App/FaunaApp.exe` or `App/fauna_ffi.dll` (the csproj includes the native core only if it
    exists, so a publish that ran before the FFI was staged succeeds and ships an app that dies at
    its first P/Invoke), and one whose own binaries show the automation surface — the gated
    `FaunaApp.Testing` namespace in `App/FaunaApp.dll`, an export named `*for_test*` in
    `App/fauna_ffi.dll`, read from the export table (convention 15's strings witness,
    [`../e2e-automation-surface-gating.md`](../e2e-automation-surface-gating.md)). Both markers
    were graded on real builds on Windows 2026-10-07: present in a Debug assembly and a
    `windows-ffi-test` dll (135 seam exports), absent from a Release publish and a production dll
    (0) — and absent from the 2026-08-22 staged Store payload, so no earlier package carried the
    agent. Raw bytes would not do: the production dll holds seam *names* as data, and the
    production assembly an ungated member named `StageImageFromTestAgent`. A dev
    or test package that really wants the surface (the registration and taskbar-badge witnesses
    stage the MSI helper's or the Debug build's payload) passes `--allow-test-surface`; the recipe
    never does.
  - Brand assets have one generator and one home: `installer/store/Assets`, beside the template that
    names them, written by the dedicated appx-logo renderer from the one brand mark (moved there
    2026-11-16 from the retired sparse package's directory).
- **Store submission uploads are unsigned** — the Store signs. The local dev loop self-signs
  (`--self-sign`; the dev-signed package is a different PFN — remove, never upgrade across
  identities, the standing gotcha).
- **Every Store package is certified by the Windows App Certification Kit where it is built (2026-11-06).** The public Store workflow runs the kit the hosted `windows-latest` image carries (`appcert.exe` in the Windows Kits 10 tree; probed 2026-11-06: 24 static tests in under a minute, an arm64 package certified by the x64 kit as well, while the `windows-11-arm` image carries no kit) on each arch's package after the package grade. The kit installs what it tests and an unsigned package cannot install, so it certifies a COPY signed with a certificate minted in the run for exactly the Store publisher and trusted on that runner alone; the upload stays the unsigned package and the signed copy never leaves the runner. `scripts/grade-store-certification.py` grades the report test by test and fails the job on any test that did not pass (a WARNING is printed, not failed) unless `installer/store/certification-allowlist.toml` names that test, verdict and arch with a reason — an entry is for a test that cannot judge the package on a hosted runner or before the Store's own signature, never for a defect; the report is kept with the run as `certification-<arch>` whatever its grade, and its texts are tool output, read as data.
- **Structural tests first (red-first, priority #5)** — two files, split by what they can prove:
  - `test_store_package.py` (tier_1, **fleet-wide**): parses the
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

**The store leg is built (2026-11-12) and has not run** ([`store-review.md`](store-review.md) § The store legs and its status bullet): `release.yml`'s `store-windows` calls this workflow at the tag and its `release` job's last steps run `scripts/msstore-submit.py`, which checks the packages, their certification report, the version's *What's new* and `furthest` before any call, then clones the published submission, replaces the two packages (named `Fauna-Store-<version>-<arch>.msix` in the upload, so a re-run recognises its own pending submission), keeps the visibility, sets publish-on-certification, uploads in blocks, commits and records `in-review` in `store-legs.json`; a re-run reads the store first and uploads nothing twice; a submission in flight that is not this version's is left alone and reds the step. Its first run waits on the credential and on the owner's first submission (0.1.6). As designed: `release.yml` calls `release-store-windows.yml` as a reusable workflow at the tag, so the two packages are built and graded by jobs of the tag's run that hold no credential, and the approved `release` job creates, fills and submits the Partner Center submission through Microsoft's submission API with a Microsoft Entra application's credential that rests only in the `release` Environment. The first submission ever stays the owner's one Partner Center visit (the table's *First Store submission* row): Microsoft's API serves only an app that already has a published submission. The credential is designed to hold no key (2026-11-15, [`store-review.md`](store-review.md) § The store legs, *The Microsoft Store credential holds no secret*), so no expiry date is recorded here; one is recorded only if the fallback key is ever made. The step's federated sign-in is built (2026-11-15; the key is used instead whenever the Environment holds one) and has never run against Microsoft. The credential is still asked of the owner. Once the app's visibility is public, the period hook's public-catalogue reader ([`store-review.md`](store-review.md) § Implementation status today) records `serving` on track `public` with no credential. Until its first green run every submission is a Partner Center visit.

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
| The shipped-flavour payload build (`just windows-store-package`) + the packer's refusal of a test-flavoured or incomplete payload | **Built 2026-10-07** — recipe-shape, packer and pack tests green on Windows (`test_store_package_build.py`); **first end-to-end run green on Windows 2026-10-07** (arm64, private tree, launched before the switch to `dist` so built at `release`; 22 min on a part-warm cargo cache): `Fauna-Store-arm64.msix` 140.8 MiB, 500 entries, identity `FaunaSocial.FaunaSocial` `0.1.3.0` arm64, `App/fauna_ffi.dll` inside, no automation surface (0 of 8488 FFI exports `for_test`), no `.pdb`. The first `dist`-profile run is the upload build itself. The packer and its tests ship in the public repository, so the upload can be built from a public clone. The upload carried the `full` flavour (ruled 2026-10-07), the recipe's default; since 2026-10-14 it carries a store flavour (§ Store distribution, the flavour paragraph) |
| The 0.1.3 upload packages, both arches | **Built and graded on Windows 2026-10-10** from a clean public clone detached (the 0.1.3 public commit), by `just windows-store-package <arch> full FaunaSocial.FaunaSocial "CN=E8868D60-047A-46A3-B608-0D4CA88AB791"` at `dist`. The x64 build was the recipe's first x64 run end to end. Both carry identity `FaunaSocial.FaunaSocial` `0.1.3.0` at their own arch, `runFullTrust` only, `App/FaunaApp.exe`, `App/fauna_ffi.dll`, `fauna-sync-agent.exe` and `fauna_shell.dll`, no `.pdb` and no automation surface, and every one of those four binaries has the package's own machine type. They are unsigned, as a Store upload is. arm64: 112.6 MiB, 500 entries; x64: 118.4 MiB, 504 entries. Their SHA-256 digests are kept with the upload's internal provenance record |
| The upload packages built by a public workflow (owner ruling 2026-10-14, `release-integrity.md` § Release signing → *A store upload is built from a recorded public commit*) | **Written 2026-10-14; its first form green on public run 39718284824 (2026-10-22); the recipe-calling form green on public run 42159950472 (2026-11-06)** — `.github/workflows/release-store-windows.yml` in the public repository, dispatched with the commit a version records and a `flavour` (`store-no-payments`, the default, or `store-safe`; § Store distribution, the flavour paragraph): a `windows-latest` job per arch refuses a recorded commit whose recipes predate the x64 hosted runner (§ Store distribution, the paragraph after the flavour paragraph) and `store-no-payments` for one whose `FaunaApp.csproj` has no `FaunaExcisePayments`, then runs `just windows-store-package <arch> <flavour> <identity> <publisher>` from the recorded tree (since 2026-10-28; until then it spelled the recipe's steps out) — the recipe's flavour table sets the production FFI's features at `dist` (`--no-default-features --features store-safe,p2p-share`, or `store-safe`), the sync agent's (`--no-default-features --features p2p-share`, or `--no-default-features`), and the app's MSBuild property (`-p:FaunaExcisePayments=true`, or `-p:FaunaStoreSafe=true`), then builds the shell extension and the self-contained app publish (`just windows-app-publish`, the MSI app's publish too), and packs with the reservation `FaunaSocial.FaunaSocial` / `CN=E8868D60-…`, then grades the finished package with `scripts/build-store-package.py --grade`: identity (that reservation, the `Package.wxs` version, its arch), `runFullTrust` as the only capability, **every** PE built for its arch (an IL-only AnyCPU assembly passes either arch), no `.pdb`, no automation surface, and the flavour's excision (`--flavour` the resolved flavour; the grade's default is `store-no-payments`): the packaged `fauna_ffi.dll` carries no `fauna.payments.*`, `fauna.tips.*` or zap-signer kind string, and carries the four peer-to-peer sharing markers for `store-no-payments` and none of them for `store-safe` — the names `just windows-store-safe-check` scans in its native column — and a package without that dll fails. It uploads `Fauna-Store-<arch>.msix` and its `.sha256` as workflow artifacts, never release assets. It is not a `release.yml` job because that workflow signs and publishes every artifact of its run. The grade is tested fleet-wide on synthetic packages (`test_store_package.py`) and the workflow's shape by `test_public_workflows_shape.py`; the proof is a dispatch in the public repository, recorded at the end of this row. The workflow was switched to the store-safe flavour (§ Store distribution, the flavour paragraph) on 2026-10-14, before its first dispatch, and took the `flavour` input on 2026-10-27; the input, its default (the grade's `STORE_FLAVOUR`), its two sites (the recipe call and the grade) and the hosted-runner refusal are pinned by `test_public_workflows_shape.py`. **The first form ran green (2026-10-22):** the workflow as it reached the public repository with the 0.1.5 attempt's sync — the `store-safe` flavour, the recipe's steps spelled out, the app published with MSBuild — was dispatched on that attempt's sync branch: four reds (2026-10-20/21) fixed forward, then four green runs, the last public run 39718284824, both `build-store` legs green with the grade in the job; each artifact's own manifest reads `FaunaSocial.FaunaSocial` / `CN=E8868D60-047A-46A3-B608-0D4CA88AB791` / `0.1.5.0` at its arch (x64, arm64), its `.sha256` matches, and its `App/fauna_ffi.dll` carries 0 `fauna.payments.*` and 0 `fauna.tips.*` strings (read back on Linux 2026-11-03). **The recipe-calling form ran green (2026-11-06):** the form that calls the recipe, with the `flavour` input and the hosted-runner refusal, reached the public repository with the 0.1.6 attempt's draft sync and was dispatched there with `flavour=store-no-payments` at that branch's head: three reds fixed forward — the first leg to run a recipe on a hosted runner found that a recipe's bare `bash`, launched by `scripts/build-if-stale.py` through Python's `subprocess`, resolved to Windows' WSL launcher in System32 (CreateProcess searches the system directory before `PATH`), and the gate now resolves a bare `bash` on `PATH` with the Windows directories left out (`resolve_program`, tested in `test_build_if_stale.py`) — then public run 42159950472, both `build-store` legs green with the grade in the job; each artifact's own manifest reads `FaunaSocial.FaunaSocial` / `CN=E8868D60-047A-46A3-B608-0D4CA88AB791` / `0.1.6.0` at its arch (x64, arm64), its `.sha256` matches, and its `App/fauna_ffi.dll` carries 0 `fauna.payments.*` and 0 `fauna.tips.*` strings (read back on Linux 2026-11-06). The Windows VM's public clone is now the build source only for a version the workflow refuses — a recorded commit whose recipes predate the x64 hosted runner, or, for `store-no-payments`, one whose `FaunaApp.csproj` has no `FaunaExcisePayments` |
| The 0.1.6 upload packages, both arches (`release-integrity.md` § Release signing → *A store upload is built from a recorded public commit*) | **Built, graded and certified by public run 43021408936 (2026-11-11)**. The run is `release-store-windows.yml` dispatched with `commit=9a01dbc0273f42b387f670902c8307f14ae4bff2` (the 0.1.6 public commit, the second attempt's) and `flavour=store-no-payments`, and each leg asserted it had checked out that commit. **x64:** `Fauna-Store-x64.msix`, 134,846,246 bytes, 504 entries, sha256 `fc153237842491a1c88b3328bd99497e9d9dd7ec91fae3e6a6c3c9fc55c57b36`. **arm64:** `Fauna-Store-arm64.msix`, 127,561,860 bytes, 500 entries, sha256 `ab6b2909e4322ddf9ea2bb047e5289ca1a879e361fb7536d1b7f6ce8a73bf5d2`. Each matches its `.sha256` and reads identity `FaunaSocial.FaunaSocial` / `CN=E8868D60-047A-46A3-B608-0D4CA88AB791` / `0.1.6.0` at its own arch. The in-job grade and a re-grade on Windows (`scripts/build-store-package.py --grade --flavour store-no-payments`) both print `GRADE OK`. The certify step graded each leg `CERTIFICATION GRADE: PASS`: kit 10.0.26100.9457, 24 tests, 23 passed, and the one FAIL, *Blocked executables*, is allowlisted. Both packages register on Windows from their bytes unpacked as they are (§ Deployment paths, item 3), Status Ok, x64 under emulation. The x64 layout, staged and registered under emulation, then ran the store-install module's development-mode nodes (`just e2e-windows-store-test --store-layout-arch x64`, [`../packaging-witnesses.md`](../packaging-witnesses.md) § Implementation status today, the Windows Store bullet's half (a)): activation by its AUMID, the subject check's refusal of the layout, the sign-in to the feed and the seven `store-install` outcome twins. 10 passed, and the 9 Store-subject nodes skipped as environment. Afterwards no package of the family was registered and no row stood under either release namespace. These are the files the first submission uploads. The run's artifacts expire 2027-02-09 |
| Certification-kit pre-check in the Store workflow (§ Build & packaging shape) | **Written 2026-11-06; run on the 0.1.6 upload packages 2026-11-11** (the row above: each leg `PASS`, its one FAIL allowlisted) — the certify step, the grade (`tests/e2e-unified/tests/platform/windows/test_store_certification_grade.py`, tier_1) and the workflow's shape test are green; the kit itself was proven on the hosted image that day on a throwaway package of each arch. The first dispatch carrying the step judges every failure as defect or not applicable |
| The three licence files at the package root ([`../shipped-notices.md`](../shipped-notices.md) § Per channel, the Microsoft Store row) | **In source 2026-11-13; first packed for real 2026-11-16, in the direct bundle** — `just windows-store-package` stages `LICENSE-MIT.txt`, `LICENSE-APACHE.txt` and the notices of the flavour and arch it builds (one manifest artifact per flavour); the packer refuses a payload without them and the grade refuses a package where any is missing or empty. The grade runs from the workflow's own commit, so a package built from a recorded commit older than this change fails it. Packer, recipe-shape, grade and stub-payload `makeappx` pack tests green on Windows. The first real payloads carrying them were the direct bundle's two, the `full` flavour's notices for each arch, both graded OK on Windows 2026-11-16 (§ Direct download, the status line); no Store-channel package has been packed with them yet |
| First Store submission | **Not done; everything a session prepares is ready (2026-11-11)** — the 0.1.6 upload packages above, x64 beside arm64 (ruled 2026-10-10); the acceptance nest fauna.build serving 0.1.6 on `:test` with the three reviewer accounts and the tester admitted and seeded ([`../org-nests.md`](../org-nests.md) § fauna.build; its box record's read-back of 2026-11-11); the certification and review-access notes approved in public-texts rounds 1 and 7 ([`store-review.md`](store-review.md) § The review-access notes), the certification paragraph's notes-truth rule met on Windows since the admin block's notice was painted there on 2026-11-04; the listing text with round 7's amendment 15 for `store-no-payments`; the trader posture recorded ([`store-listing-contact.md`](store-listing-contact.md) § Microsoft Store). `installer/store-review.toml` holds the Microsoft Store 0.1.6 table at `built`. What is left is the owner's: fauna.build's firewall lift, so that a Microsoft tester can reach it, and the one Partner Center visit — the known-user group, the upload, the listing, the notes and the submit |
| Listing screenshots | **Built 2026-10-14** — the window capture and `just e2e-store-listing-screenshots` (§ Store distribution, the screenshots paragraph); first run green on Windows, 2 shots (feed, conversation) at 2858 × 1789 px on the VM's 200 % display. Those first shots showed the global connection and sync-agent status text laid over the page's top-right; since 2026-10-16 it sits on its own line at the top of the shell, above the sidebar and the page (`transport-connection.md` § Connection-status indicator), and the re-shot set shows no status text over page content. **Kept 2026-10-23:** the set was re-shot on Windows (run green, both shots at 2858 × 1789 px, about 135 KB each, from the 0.1.5 Debug build) and committed, beside the approved listing text (§ Store distribution, the listing-text paragraph) |
| x64 Store-install witness | **Ruling recorded 2026-11-07; half (a) done for 0.1.6 on 2026-11-11** — the fleet has no x64 device, so the Store-served run is arm64's and the x64 leg is (a) the x64 package registered under emulation on Windows and (b) one alpha tester's reported install on an x64 PC; the ruling and who records each half: [`../packaging-witnesses.md`](../packaging-witnesses.md) § Implementation status today, the Windows Store bullet. Half (a) ran for 0.1.6 at its recorded commit on the x64 upload package (the *0.1.6 upload packages* row above); the leg waits for (b)'s report, which lands here as device class and OS version only |
| Alpha visibility (private audience, the known-user group) | **Ruled 2026-10-07**, the group not yet created — § Store distribution's alpha paragraph; the submission that creates it is the first one |

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
   no OID suffix (`0x80073D2D` otherwise). The loose layout may also be a finished, unsigned
   `.msix` unpacked as it is (a plain zip extract, with `AppxBlockMap.xml` and
   `[Content_Types].xml` left in place). Registering that runs the package's own bytes, so an
   upload artifact needs no rebuild to be registered locally. Proven on Windows 2026-11-06 with both
   arches of the public Store workflow's 0.1.6 packages from run 42270367504, x64 under Windows 11
   on Arm's emulation: both reported Status Ok.

The automated verification uses (3), so it runs unattended. Residual gap: it registers the loose
layout, not the packed `.msix` (the container is covered by `test_store_package_pack.py`);
registering a *signed* `.msix` remains admin-gated. Scope note on the startup task: registration is
asserted, but the OS only materialises StartupTask **state** (and `StartupTask.GetAsync` only
answers) after the app has run once under package identity.

**Scoping caveat — RETIRED 2026-08-23.** This paragraph used to record that the Win11-only
exclusion left Windows 10 Store clients unable to share state with an MSI install. The package now
declares no exclusion on any Windows version (§ Data continuity), so the caveat has no subject:
every Store client virtualizes its own `%LocalAppData%\Fauna`, uniformly.

## Direct download (MSIX app + nest MSI)

> **Status: design ratified 2026-11-15 on the owner's direction of 2026-11-13; the direct package is built, dev-signed (2026-11-15), and its real bundle graded (2026-11-16), the rest is not; the signer is chosen (Azure Artifact Signing, owner-ruled 2026-11-15) and nothing is signed with it yet.** The direct download today is still the full-feature MSI, unsigned (§ Implementation status today, the direct-download paragraph). Built: `just windows-direct-package` stages each arch's `full` payload through `windows-store-package <arch> full "" "" direct`, then one call of the Store packer on its direct channel (`scripts/build-store-package.py --channel direct`) packs both arch packages with this section's identity, display name and write-virtualization opt-out, bundles them into `build/installer/Fauna.msixbundle` at the product version and signs the bundle with the dev certificate, and the grade reads the bundle back (`--channel direct --grade`: the bundle's identity and version, one package per arch, each graded for its identity, `Fauna`, the two capabilities, the opt-out in both forms, its arch, no symbols, no automation surface, the licence files and the full flavour's seven native markers). `just windows-direct-package production` packs with the recorded production publisher instead, unsigned, and refuses before anything builds while none is recorded. Its first real-payload run graded OK on Windows on 2026-11-16, built cold in 13 h 9 min: `build/installer/Fauna.msixbundle`, 265,480,767 bytes, `FaunaSocial.FaunaDesktop` 0.1.7.0, publisher `CN=Fauna Social (Dev)`, holding an x64 package (136,442,197 bytes) and an arm64 package (128,997,854 bytes), each graded clean for the direct channel. The public release workflow builds the bundle and the nest MSI unsigned as workflow artifacts and publishes neither (2026-11-16; § Implementation status today, the direct-download paragraph). The install witness is built and green on Windows (2026-11-16; § Testing, the direct-package row): five of its six measurements agree with this section, and the sixth does not — a sync root registered from inside either channel's package survives the package's removal (5, *Removal*). Not built: the release workflow's asset switch and signing leg, the site. Three answers below were settled by the design pass, not by the owner's word, and are marked *refutable by the owner*: the nest MSI's contents with the retirement of the overlay badges (1, 6), and where the package keeps user data (5).

**The direction (owner, 2026-11-13).** A user who downloads Fauna for Windows from the organisation gets the install a signed MSIX gives: one App Installer window naming the app and its publisher, one button, no elevation prompt. So the direct download becomes two files. **(1) An MSIX bundle (x64 + arm64) carrying Fauna Desktop and the per-user sync agent** — the app, for nearly every user. **(2) A separate MSI carrying the nest** — the machine services, for the rare user who hosts a nest on that PC; it is a download of its own that the user fetches and runs, never something the app fetches or launches. **(3) Every Windows executable the organisation ships is Authenticode-signed by the same signer** — the files inside both packages and the loose executables of the terminal-app archive ([`tui.md`](tui.md)), `fauna-tui.exe`, `fauna-cli.exe` and `fauna-sync-agent.exe`.

**Why the nest stays an MSI.** The reasons are § Store distribution (MSIX) → *Feature subset*'s, unchanged by the signer: an MSIX service runs only as `localSystem`, `localService` or `networkService`, never the ratified virtual account, and the firewall rule and the `%ProgramData%` layout are machine-wide writes. An always-running nest under an ordinary user's account was considered and refused the same day: inbound reach still needs one administrator approval (the firewall), and every program of that user could then read the nest's store. Whether installing an MSIX that declares a service needs elevation was not established and is not part of the reason.

**The signer is Azure Artifact Signing, for everything the direct download ships (owner-ruled 2026-11-15).** The MSIX bundle, the nest MSI and every loose executable are signed through Azure Artifact Signing, so every prompt names the organisation's validated legal name and download reputation builds on that name. The owner chose it over two other routes, knowing its costs: a monthly fee from the day the signing account is made, and one representative's government-ID capture and live face check on a phone through the service's outside verification vendor, inside the organisation's identity validation and again at each renewal (the step declined in August). What it buys beyond the name: the release workflow signs with a federated credential bound to the public repository's protected `release` Environment, so no signing secret is stored anywhere and no human act is needed per release. **Nothing is signed yet:** no signing account exists and the identity validation is not submitted. Two things are still unproven and are read before anything is typed into the validation form or shipped: the form's country list (the service's own setup page names Norway among the eligible organisation countries, a second Microsoft page omits it), and the exact publisher string (*Identity* below). The validation is submitted once, with values checked against the public register first, never as a probe. **If the validation fails, the fallback is a certificate bought from a certificate authority, for everything** — also in the organisation's own name; the organisation is under three years old, so the industry rules require a copy of the requester's government ID, and the release workflow would then hold signing secrets. That purchase is put to the owner then; it is not prepared now. **SignPath Foundation is not taken, for any file (owner-ruled 2026-11-15; for the MSIX already ruled out 2026-11-13):** an MSIX's publisher is the package's identity, so a certificate held in another party's name gives that party the power to leave the installed package un-upgradeable (a changed subject at their renewal, a terminated subscription, a revocation); and for the MSI and the loose executables its terms ask a team member's manual approval for every release and a stored token, where the chosen signer asks neither. No application was submitted. History: the 2026-08-10 ruling dropped all Azure use ("never revive"); the owner reopened it on 2026-11-13 and chose on 2026-11-15. The investigation's findings and the plan are tracked internally.

**What the choice fixes in the package.** An MSIX's `Identity/@Publisher` must equal the signing certificate's subject, so the direct-download MSIX has an identity of its own: not the Store package's (`CN=<GUID>`, Store-signed), and not known until the chosen signer's certificate profile shows its subject. **The publisher string is fixed at the first signed release (read from Microsoft's pages 2026-11-13).** The package family is the name plus a hash of the exact publisher string, and an update is accepted only inside one family; a package with the same name and another publisher is refused over the installed one (`0x80073CF3`), so the user must uninstall first, which deletes the package-scoped data. The certificate's key, serial and issuer are not part of the identity: a renewal, or a move to another issuer, keeps updates working when the subject string Windows renders is identical in fields, order and case. Microsoft's publisher-bridging mechanism (persistent identity) carries a package across a changed string only when the OLD certificate signs and timestamps a bridging catalog while still valid, on Windows 11 21H2 or later, never on Windows 10. The two consequences are settled in *Identity* and *Data* below.

### The two files

| File | Carries | Installs | Signed by |
|---|---|---|---|
| `Fauna.msixbundle` | one bundle of the x64 and the arm64 package: Fauna Desktop, the per-user sync agent, the Explorer context menu, the `fauna://` handler | per user, through App Installer, no elevation | a certificate in the organisation's own name |
| `Fauna-Nest-Setup-<arch>.msi` | the nest service, the bridge service, the terminal app | per machine, elevated | the same signer |

**One template, one packer, two channels (uniform shape).** The direct package is built from the Store package's template, packer and payload recipe (§ Build & packaging shape), never a copy of them. It is the same feature subset (§ Feature subset: app, agent started by `windows.startupTask`, packaged context menu, `fauna://`; no service). The channel changes exactly five things, each a token or a switch of the one packer: the identity name, the publisher, `Properties/DisplayName` (a new token: `Fauna` here, where the Store's must equal its reservation, `Fauna Social`), the write-virtualization block (*Data* below), and the flavour. The direct channel then bundles the two arch packages (`makeappx bundle`) and signs the bundle and every executable in it; the Store upload stays two unsigned `.msix` files.

### 1. The nest MSI's features (refutable by the owner)

**The MSI keeps the Nest service, the Bridge service and the Terminal App, and loses every client feature:** the Desktop App, the Sync Agent with its logon Run key, Explorer Integration (all 8 shell classes, the overlay handlers, the legacy-menu verb), the `fauna://` handler and the sparse package.

- **Why not today's full tree.** A full tree is a second way to install the same app from the same download page: two packages to sign, test and explain, with the app's files, logon launch and protocol handler owned by whichever ran last. The owner's reason for the split is that hosting is rare; the MSI is the hosting installer and nothing else. Everything the MSI needed for the client alone goes with it: the sparse package and its two custom actions, the Path-1 version-stamped shell DLL with `CleanShellDlls`, `KillFaunaApp`, `KillFaunaSync` and `CleanSyncRoots`.
- **The headless or offline box is still served.** A nest box with no desktop session wants the nest, the bridge and the terminal app, which is what this MSI is (the terminal app's top-level placement was made for that box, § Feature Tree). A box that wants the app without a network installs the bundle from the file: it carries its runtime and has no framework dependency (§ Package identity for FaunaApp.exe), and where App Installer is absent `Add-AppxPackage` installs it.
- **Defaults.** Nest is on by default: the reason it was off (2026-06-21, "the Windows desktop is primarily a client") belonged to an installer that also carried the client. Bridge stays off and requires Nest. Terminal App stays on.
- **Upgrade from today's MSI.** The nest MSI keeps the `UpgradeCode`, so it replaces a full-feature install as a major upgrade: the nest, the bridge, the terminal app and `%ProgramData%\Fauna` carry over, and the client features are removed. That is acceptable only inside the 0.1.x compat-free window ([`../version-compatibility.md`](../version-compatibility.md) § Dimension 2, the fifth exception), so the trimmed MSI and the first bundle ship in one 0.1.x version. If either slips past 0.2.0, the trim needs a transition release first.
- **Built in source, not yet released (2026-11-16).** The nest MSI is its own package, `installer/nest/Package.wxs`, linked with the fragments it shares with the full-feature package (the client-only directories moved into the client fragments that use them, so it links none); its tables hold exactly the Nest, Bridge, Terminal App and licence-file components, Nest at level 1, only WiX's own custom actions, `ProductName` `Fauna Nest` and the shared `UpgradeCode`. Its test door is `conftest.py::nest_msi_path` (builder `windows_installer.py::build_nest_msi`, no publish), its notices artifact `windows-nest-msi` (`just windows-nest-msi-notices`), and its suite `test_nest_installer_structure.py` (§ Testing). The full-feature package stays untouched beside it because the public release links every top-level `*.wxs` and the site offers that MSI as the Windows desktop download: trimming those sources would ship a nest-only file under the desktop label in the next weekly release, before the bundle can ship. The release workflow already builds and install-smokes this package on both arches, unsigned, as a workflow artifact it does not publish (§ Implementation status today, the direct-download paragraph); it switches its assets to this package and the bundle in one change, which is how the two ship in one 0.1.x version; the full-feature package and its client sources retire after it. Its product name, feature texts and the shared wizard texts are reviewed public texts (round 11): the shared welcome string and uninstall dialog name the package through `[ProductName]`, so both packages build with one `Strings.wxl`, one `UI.wxs` and one licence page. A terminal app installed from this package has no sync agent beside it, so its sync pages need the agent from the bundle or the archive; how it finds a packaged agent is not yet designed.
- **What a box with both files shows.** Nothing twice. The two files share no feature: one context-menu entry, one `fauna://` handler and one logon agent, all the bundle's; the services and the machine `PATH` entry are the MSI's. The same-box app reaches the nest on the fixed loopback port as today (§ Implementation status today, the same-box default URL row). Topology 4 of § Identity & coexistence (a package beside an MSI with client features) cannot arise from a trimmed MSI; it remains only for a box still carrying a full-feature MSI of 0.1.x.

### 2. Updates

**A direct-download install updates the way every self-installed Fauna app does: the app tells the user a newer version is out and names the release page; the user downloads the new bundle and opens it; App Installer installs it over the old one, in the same family, keeping the data.** No `.appinstaller` descriptor ships and the package declares no update URI.

- The cross-app rule is that an app never downloads or installs a newer version by itself ([`README.md`](README.md) § Knowing a newer version is out; macOS retired its update framework under it on 2026-10-08). An `.appinstaller` descriptor makes Windows fetch and apply updates unasked from an address welded into the installed package, which is that behaviour with the operating system as the downloader.
- It is also unproven where it matters: whether an `.appinstaller` update honours publisher bridging is not established.
- The notice needs nothing new: `UpdateCheck` and the sign-in look ([`../apps/windows.md`](../apps/windows.md) § Auto-Updates) already run in this build.
- The bundle's version is the product version in four parts, from `Package.wxs` like the Store package's; a reship mints a new patch number ([`../product-version.md`](../product-version.md) § Reships), so every later release is a higher version and installs over the earlier one.
- **Measured by the install witness (Windows, 2026-11-16)** with version N registered and the app and the agent running in its context, then version N+1 of the same family registered over it. A deployment that may not close the package's processes refuses with `0x80073D02` ("the following apps need to be closed") and leaves version N and its agent running. The app was still running too in three runs. In one run Windows had ended it, filing a `MoAppHang` report for the package. Left alone, the app answers, so it is reported hung when it is asked to close; that is a defect of its own. A deployment allowed to close them (`Add-AppxPackage -ForceApplicationShutdown`) installs N+1 in the same family with Status Ok, closes both, and keeps the data in the real `%LocalAppData%\Fauna` and in the package's own store. Two things are not measured yet. App Installer's own window over a signed bundle is one; it needs the signer. The other is the agent running again after the next app start: the app starts the agent only from a signed-in session (`App.xaml.cs`, the hydration session), which this witness does not have. In the package layout the app finds the agent at `<exe-dir>\..\fauna-sync-agent.exe` (`agent_spawner.rs`).

### 3. The terminal app

**Out of the bundle.** `fauna-tui.exe` and `fauna-cli.exe` reach a Windows user through the archive and through the nest MSI's Terminal App feature ([`tui.md`](tui.md) § The ratified channel), both signed under direction (3).

- One feature subset for both MSIX channels; the Store package leaves the terminal app out by the 2026-09-26 ruling.
- The terminal app's defining use is a headless box over SSH ([`tui.md`](tui.md) § What any channel must satisfy), which a per-user App Installer package does not serve.
- **Beside the package, measured by the install witness (Windows, 2026-11-16):** the packaged agent, started in the direct package's context with no `--pipe-name`, serves the per-user pipe `\\.\pipe\fauna-sync.<SID>`, and an unpackaged client deriving that name from its own SID reaches it, is answered and can stop it. The client in that run is the test process, which speaks the agent's wire, because the terminal app has no headless verb that talks to the agent; the pipe and its name are the same.
- An execution alias would run the terminal app with the package's identity. Nothing has measured its credential store, its agent spawning or its file paths in that state, and the archive install would then behave differently from the packaged one for no gain.

### 4. Flavour

**The full default-feature build** (`full`, the payload recipe's existing value). The store flavours exist for Microsoft Store policy 10.8 (§ Store distribution, the flavour paragraph), which does not bind a download from the organisation, and [`../dynamic-features.md`](../dynamic-features.md) § The App-Store escape hatch already gives every direct download the default flavour.

### 5. Identity, coexistence and data

**Identity.**

| Manifest field | Direct-download value |
|---|---|
| Identity `Name` | `FaunaSocial.FaunaDesktop` |
| Identity `Publisher` | the signing certificate's subject, character for character; a token until the signer's certificate profile shows it |
| Properties `DisplayName` | `Fauna` |
| Properties `PublisherDisplayName` | `Fauna Social` |
| `Application/@Id` | `FaunaApp` (as in the Store package, so the AUMID suffix is the same) |

- **The name is new on purpose.** With the Store's name and another publisher, Windows would refuse whichever package came second with `0x80073CF3`, an error that tells the user nothing. `FaunaSocial.Fauna` was the sparse package's name and is left unused, so a development box that still has a dev-signed sparse package registered cannot collide with a dev-signed direct package. The name, like the publisher, is permanent from the first signed release.
- **The publisher, by outcome.** *Azure Artifact Signing (the chosen signer, 2026-11-15):* the subject is the organisation's validated identity as the service's certificate profile shows it before anything is signed. *A bought certificate (the fallback if the validation fails):* the subject is what the authority issued, read off the certificate. Either way the exact string is read before the first signed release and recorded in three places that must agree: this table, a production-publisher property beside the dev publisher in `apps/fauna-windows/installer/PackageIdentity.props` (`FaunaDirectProductionPublisher`, in place and empty since 2026-11-15; a dev build packs with the dev publisher), and `installer/official-apps.json` (with the family name computed from it, § Package identity for FaunaApp.exe). A production direct build refuses to run while the property is empty; the package grade asserts the manifest's publisher, the recorded string and the signature's subject are equal; and a tier_1 check reds when the recorded string and the published one differ, so the string cannot change by accident.
- **A renewal** is checked against the recorded string before it is used. A changed organisation name or address is the remaining risk on either route; its remedy is the bridging catalog, signed while the old certificate is valid, and it reaches Windows 11 only.

**Coexistence.**

- **With the nest MSI:** disjoint (1 above).
- **With the Store package:** both can be installed, being different families. It is discouraged and not blocked, exactly as topology 4: two "Fauna" Start entries, two context-menu entries, both `fauna://` registrations, and two logon agents of which one survives on the per-user mutex (§ Identity & coexistence, verified 2026-08-22; across the two families by the install witness on Windows 2026-11-16: with both registered, an agent started in the direct package's context serves a pipe, the Store-shape package's agent started on it exits 0 as a duplicate on the per-user mutex, and the first keeps serving). Their data is separate (below). Moving from one channel to the other is a fresh device state on the new one; nothing user-irrecoverable is lost (§ Data continuity).
- **With the sparse package:** none ships (6 below).

**Data (refutable by the owner): the direct package keeps user data in the real `%LocalAppData%\Fauna`, not in the package's own data folder.** It declares the write-virtualization opt-out for that directory and the `unvirtualizedResources` capability the opt-out needs; the Store package keeps declaring neither (§ Data continuity, unchanged).

- **Why the channels differ here.** The Store dropped the opt-out for one cost, a restricted capability that certification stops to read, and no download from the organisation is certified. Against it stand three things the direct channel alone has. A publisher string that can change: a forced uninstall deletes the package's data folder, which holds the local drafts, the sync bindings and `mls.db`, and from 0.2.0 an uninstall the organisation's own certificate forces must lose nothing ([`../../principles.md`](../../principles.md) § No user-data loss). The existing contract: the direct download has always kept `%LocalAppData%\Fauna` across an uninstall (§ Uninstall). And the terminal-app archive: its unpackaged `fauna-tui.exe` and agent use the real directory, and the per-user agent lock already spans package identity, so a packaged agent with a private copy of the state would be a second, different view behind one lock.
- **The form.** Microsoft's flexible-virtualization page (§ Data continuity links it) gives a scoped form for Windows 11 and a blanket switch before it. The requirement is the outcome on every Windows version the package installs on; the package build picks the spelling from that page and the install witness measures it on Windows 11. No Windows 10 run is on file for any Windows installer (§ Platform Support). **The spelling used (2026-11-15): both forms, with `unvirtualizedResources`.** `virtualization:FileSystemWriteVirtualization` excludes `$(KnownFolder:LocalAppData)\Fauna` alone, and `desktop6:FileSystemWriteVirtualization` is `disabled`. Why both: the package installs from 10.0.19041, while the scoped form is Windows 11's; the page says a device that recognises the scoped declarations uses them in place of the old ones, and an earlier one ignores them and honours the old switch. So Windows 11 unvirtualizes this one directory and keeps the rest of AppData the package's private view, and Windows 10 unvirtualizes all of the package's AppData writes, which is the only way it offers to keep this directory real. The capability is supported from Windows 10 1903, below the floor. Registry writes stay virtualized, since nothing here asks otherwise. Both namespaces are declared on `<Package>` and listed in `IgnorableNamespaces`, so a Windows version that does not know one ignores it. The packer inserts the block for the direct channel only; the Store manifest carries none of it (`test_store_package.py`, the direct-manifest and Store-template invariants; `test_store_package_pack.py` packs it with `makeappx`, which schema-validates it). **Measured on Windows 11 by the install witness (Windows, build 26300, 2026-11-16):** a process in the package's context that writes under `%LocalAppData%\Fauna` leaves its file at the real path. Its write elsewhere in `%LocalAppData%` lands in the package's own `Packages\<family>\LocalCache\Local`, so Windows 11 takes the scoped exclusion over the blanket switch. The Windows 10 form is not measured.
- **Removal** leaves `%LocalAppData%\Fauna` in place, as the MSI's uninstall does; deleting it is the app's affordance. An MSIX has no uninstall custom action, so nothing runs `CleanSyncRoots`: whether a sync root registered by a packaged agent is gone after the package is removed is measured by the install witness, for both MSIX channels, and a root left behind is a defect to fix in the agent. **Measured (Windows, 2026-11-16):** after `Remove-AppxPackage`, the file written under the real `%LocalAppData%\Fauna` from the package's context is still there, and the package's own store is gone. **A sync root is NOT gone, on either channel — the defect, open.** The root was registered from inside the package's context with the call `fauna_cfapi::register_sync_root_with_shell` makes; the production agent registers one only for a nest-backed on-demand binding, which the witness cannot mint. After the removal, its `SyncRootManager` key (which records the package's AUMID, `<family>!FaunaApp`) and its cloud-files filter registration both stay, so a cloud-only placeholder in a folder the user bound can no longer be opened. From an unpackaged process afterwards, `CfUnregisterSyncRoot` removes the filter half, but `StorageProviderSyncRootManager.Unregister` answers S_OK and keeps the key; this user, who owns the key, can delete it. Deleting it is not a complete cleanup. A family whose roots went that way went on refusing the next `Register` with `0x80070490`, so the OS keeps per-family state beyond the key, which the fix must clear as well.

### 6. The sparse package and the overlay badges (refutable by the owner)

**Both retire with the MSI's client features.**

- **The sparse package** existed to lend package identity to files an MSI installed. No MSI installs the app or the shell extension any more, and a full package's identity is native, so the default-menu entry, the taskbar badge and the app's AUMID come from the bundle itself. The `msix` element in the app's embedded manifest goes with it; the bootstrapper's identity no-op stays, since the full package needs it. **Built ahead of the client features (2026-11-16):** the template, its build script and its structural suite are gone, the logos moved to `installer/store/Assets`, the `msix` element and the manifest's generation step went (the app embeds a plain `app.manifest`), and every MSI is linked `-d NoSparsePackage=1`. The two harnesses that lent its identity to a build output register a full package instead: the taskbar-badge witness the direct-download package, the Store leg the Store package (`helpers/taskbar_badge.py::registered_package`), and the context-menu observer the direct-download package around the DLL (§ Shell Extension, *Menu placement*). The full-feature package's two custom actions stay until that package retires.
- **The overlay badges** have no MSIX surface (§ Feature subset), and with no client feature in the MSI nothing can register them. Windows then shows sync state the same way on both channels: the Status column Explorer renders from the cloud-files placeholder state. Keeping the badges would mean keeping an MSI-installed shell DLL beside the bundle's, with the Path-1 upgrade contract and a signed companion package, for a corner icon that repeats the Status column.
- **The legacy-menu verb** goes too. The packaged registration is the one context-menu surface; where it renders on Windows 10 and under a forced classic menu is the Store package's behaviour, which the direct channel now shares.

### The site

The download page offers `Fauna.msixbundle` first as Fauna for Windows, and the nest MSI under self-hosting, never as the app. That page, `installer/downloads.json`, the official-apps entries for the direct package and the install guide are public texts and change through a public-texts request, after the files exist.

### Build order

Tracked internally: (a) the direct package from the shared template and packer, dev-signed, with its structural and packing tests — built 2026-11-15, its real bundle graded OK on Windows 2026-11-16; (b) its install witness on Windows — built and green 2026-11-16, its one contradicting measurement a defect of its own; (c) the MSI trimmed to the nest, after (a) — built in source 2026-11-16 as the separate nest package (1 above), the release untouched; (d) the release workflow's bundle and signing leg, after (a), (c) and the signer's choice; (e) the site request, after (d). (a) to (c) need no signer.

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
| MSI authoring | `tests/e2e-unified/tests/platform/windows/test_installer_structure.py` | the MSI, built by the run that selects the suite when its inputs are newer than its `build.stamp` (the one door both suites read it through, `conftest.py::msi_path`; building needs no elevation, only installing does); **no admin** | `Feature`/`FeatureComponents`/`File` + `Upgrade`/`ServiceInstall`/`ServiceControl`/`Wix4ServiceConfig`/`Registry` tables — feature tree, components, files, major-upgrade authoring, service virtual-accounts + recovery, 8 shell CLSIDs, `fauna://` | **Yes** — read-only `msi.dll` query, non-disruptive; runs in the unelevated verify queue on Windows, its MSI built in the run (since 2026-11-14) |
| Nest MSI authoring | `tests/e2e-unified/tests/platform/windows/test_nest_installer_structure.py` | the nest MSI, built by its own door `conftest.py::nest_msi_path` past its own stamp (no publish: the four release binaries, the mail bridge and the WiX link); **no admin** | the three features and no other, Nest and the Terminal App on by default and Bridge off under Nest, exactly the nest's components, no client file, directory, shortcut, COM, shell, protocol or Run-key registration, no custom action of the package's own, `Fauna Nest` with the shared `UpgradeCode` and the product version, the two services, their recovery and the firewall rule, the licence files on both top-level features, the one `PATH` entry (§ Direct download, 1) | **Yes**, unelevated. Authored 2026-11-16; green over a stub-payload link of the real sources on Windows and red in 8 places over the full-feature MSI; the first build over real binaries is a verify request |
| Real install | `tests/e2e-unified/tests/platform/windows/test_installer.py` | **admin**; solo + serialized, or a throwaway VM | per-feature `ADDLOCAL` install, services reach RUNNING, file layout, plain-uninstall preserves `%ProgramData%`; `TestTerminalAppOnly`: the `TerminalApp` feature alone — its two files, the machine `Path` entry read from the registry, `fauna-tui` found and `fauna-cli --help` run by a fresh shell, files and entry gone after `msiexec /x`. `msi_path` publishes the app and links the MSI, the suite's subject, on a 3600 s clock; its inputs are collection-time prebuilds, built for a run in which a selected test that is not skip-marked requests `msi_path` past a stale `build.stamp` — this suite elevated, or the structure suite in any run | **Disrupts other work on the machine** — perMachine services + HKLM/`%ProgramData%` writes; run elevated + serialized when the box is otherwise idle (practiced 2026-06-19), or on a throwaway VM |
| Binaries serving | `tests/e2e-unified/tests/platform/windows/test_caldav_imap_serving.py` | locally-built `fauna-nest-svc.exe` + `fauna-mail-bridge.exe` + `seal-helper-testonly.exe`; **no admin** | the `--foreground` loopback nest writes its TLS *floor* cert under the **data-dir** acme dir (`fauna_nest::start_server` floor write fires on Windows); the supervised MDA serves a CalDAV + IMAP round-trip via two Python `CalDAVClient`s + `imaplib` (GREEN, Track 3 increment 2) | **Yes** — temp data dirs + ephemeral loopback ports, no SCM / HKLM / `%ProgramData%` writes; covers the *Windows binaries* the Linux dev machine's (Linux-binary) any-locator matrix can't |
| Installed serving | `tests/e2e-unified/tests/platform/windows/test_installer.py::TestCalDAVImapServingAfterInstall` | **admin** + throwaway VM + Go/llvm-mingw build toolchain (the MSI now ships the MDA) + `seal-helper-testonly.exe` | the **MSI-installed** `FaunaNest` + `FaunaBridge` SCM services serve CalDAV `:8443` + IMAP `:993`: enable CalDAV over WS-RPC → the FaunaBridge-supervised MDA auto-enrolls on loopback (no manual approve/x25519-poke) → two Python `CalDAVClient`s round-trip a calendar event + `imaplib` opens INBOX off the floor cert | **✅ GREEN — install-level RUN PROVEN on Windows 2026-06-19 (`1 passed, 27s`).** Ran elevated on the dev box (volatile data, serialized). The RUN surfaced + fixed a WiX Bridge-feature install bug + 3 `_build_msi` build-path bugs |
| Installed reach | `tests/e2e-unified/tests/platform/windows/test_installer.py::TestInstallThenReach` (the twin of `platform/macos/test_installer.py::TestInstallThenReach`; shared `helpers/installed_nest_reach.py`) | **admin**; solo + serialized, or a throwaway VM; a LAN IPv4 address on the box | the **MSI-installed** `Nest` feature alone serves at the box's own LAN address on `:443`: the claim code read from `%ProgramData%\Fauna\nest\claim-code`, health read and the claim made at `https://<LAN IP>:443` with the floor's SPKI pinned (no verification-off path), the claim code consumed. A same-box dial proves the all-interfaces bind, not the Firewall inbound allow | **Authored 2026-11-13; no elevated run yet.** Its nest sequence is rehearsed unelevated by `test_caldav_imap_serving.py::test_windows_nest_is_claimed_and_healthy_over_its_pinned_tls_floor` (a TLS `--foreground` nest on loopback) |
| Store and direct MSIX authoring | `tests/e2e-unified/tests/platform/windows/test_store_package.py` | nothing — pure parsing + in-process calls; **no admin** | the manifest template's identity/capability/extension/virtualization invariants and the build script's arch, version-source and no-`/nv` invariants (§ Store distribution → Build & packaging shape); the Store rendering is the template with its five token values and nothing added; the direct rendering (§ Direct download) is `FaunaSocial.FaunaDesktop`, `Fauna`, the two capabilities and the opt-out in both forms with its namespaces declared, and differs from the Store's in nothing else; the bundle's `/bv` version; and the grade on synthetic packages and bundles for both channels, the full flavour's seven markers included | **Yes, and on every machine** (tier_1, no `win32` skip — the couplings it guards are fleet-wide) |
| Store and direct MSIX packing | `tests/e2e-unified/tests/platform/windows/test_store_package_pack.py` | Windows SDK `makeappx.exe`; **no admin** | the packer accepts the manifest (real schema validation) and produces an x64 **and** an ARM64 `.msix`, each declaring its own arch, carrying its payload, with no surviving `@@` token and no opt-out; on the direct channel, one `Fauna.msixbundle` of both arch packages at the product version, each carrying the direct identity, `Fauna`, the two capabilities and the opt-out, and a direct build leaving the Store's staged layout alone | **Yes** — stub payload files, ~6 s, no install; skips if the SDK is absent. Green on Windows 2026-11-15 (21 passed), the direct cases seen red against the pre-change packer first |
| Store MSIX registration | `tests/e2e-unified/tests/real_session/test_store_package_registration.py` | Developer Mode + a staged real payload; **no admin**; `--real-session` | a real per-user registration: Status=Ok, PFN derives from Publisher, the registered manifest kept all four ratified extensions and **no service**, startupTask names `fauna-sync-agent.exe` + is enabled, the `fauna` protocol class exists, and the shell-ext CLSID **activates** in the packaged COM surrogate | **✅ GREEN on Windows 2026-08-11 (6 passed, 5m19s).** Borrow-and-give-back; cleanup verified (no package, no `fauna` class left). Skips if no payload is staged |
| Direct-package install witness | `tests/e2e-unified/tests/real_session/test_direct_package_install_witness.py` | Developer Mode + the direct layout `just windows-direct-package` stages (`build/installer/direct-stage/<arch>`, copied, never registered in place); **no admin**; `--real-session` | (1) the layout registers Status Ok as `FaunaSocial.FaunaDesktop` whose family is the name plus the dev publisher's computed publisher id, four extensions, no service, `Fauna`, the two capabilities and both virtualization forms; (2) a package-context write under `%LocalAppData%\Fauna` is at the real path and outlives removal, one elsewhere in `%LocalAppData%` is the package's own and goes with it; (3) N+1 over a running app and agent: a plain deployment refuses `0x80073D02` with both left running, `-ForceApplicationShutdown` installs, closes both and keeps the data; (4) a sync root registered in the package's context, per channel — each channel's manifest under a fresh test-only family per run, since a family with root history refuses the next `Register` — after removal (an expected failure naming the defect while it stands); (5) a direct and a Store-shape agent on one pipe, the second a duplicate; (6) an unpackaged client on the derived per-user pipe reaches the packaged agent (§ Direct download, 2, 3, 5) | **✅ GREEN on Windows 2026-11-16** (item 4's two nodes expected failures: both channels leave the root). Borrow-and-give-back: no package, no root, no marker left; app and agent launches relocate `LOCALAPPDATA` into the scratch |
| Publisher single home | `tests/e2e-unified/tests/platform/windows/test_package_identity_publisher.py` | nothing — pure parsing + in-process imports; **no admin** | `installer/PackageIdentity.props` declares the one dev publisher DN; the packer's dev default and the harness's registered packages equal it; the DN literal appears nowhere else under `apps/fauna-windows`, `scripts`, `tests/e2e-unified` (§ Package identity for FaunaApp.exe); FaunaApp embeds the checked-in `app.manifest`, which names no package (no `msix` element, no template beside it); the direct package's production publisher is declared once beside the dev one, empty or a plain DN that is not the dev one, and a production direct build refuses while it is empty, beside another publisher, on the Store channel and with self-signing (§ Direct download, 5) | **Yes, and on every machine** (tier_1; the restated-literal perturbation verified red 2026-09-28, the `msix`-element one 2026-11-16) |
| Store taskbar badge | `tests/e2e-unified/tests/real_session/test_store_package_taskbar_badge.py` | Developer Mode + the Debug build; **no admin**; `--real-session` | the app, started in a registered Store package's context, runs with that package's identity and the OS holds the app's own unread count as the badge for `<PFN>!FaunaApp` | **✅ GREEN on Windows 2026-09-28 (1 passed, 49 s).** Borrow-and-give-back; no test package left registered. Stages through `helpers/taskbar_badge.py::registered_package` since 2026-11-16, the direct leg's helper (`apps/windows.md` § Home-screen widget), which also gives it the licence-file placeholders the packer has required since 2026-11-13; **✅ GREEN again on Windows 2026-11-16 on that helper (1 passed, 232 s)**, no test package left registered |
| CI smoke | `release.yml` `build-windows` (x64 leg) and `smoke-windows-arm64`, both running `apps/fauna-windows/installer/ci-smoke.ps1` | a CI run | `ADDLOCAL=ALL` install/uninstall succeed; one ARP entry while installed, none after; every installed native binary is PE-machine the MSI's arch; the terminal app's machine `Path` entry (a shell reading the `Path` fresh finds `fauna-tui` and runs `fauna-cli --help`; both files and the entry gone after uninstall — added 2026-10-28, first run at the next release run); FaunaNest + FaunaBridge reach and stay Running; the 8 shell CLSIDs register and activate through COM; `REMOVE_USER_DATA=1` deletes vs. plain uninstall preserves `%ProgramData%\Fauna\` | **No** — it runs on the public repository's GitHub-hosted runners on each `v*` tag (and a branch dry run): `windows-latest` for x64, `windows-11-arm` for arm64. Authored 2026-10-14; first run green in the public dry run 39718279069 (2026-10-22), both legs |

### Coverage gaps

**Win-runnable authoring guards — implemented 2026-06-06.** `test_installer_structure.py` was
extended beyond the original `Feature`/`FeatureComponents`/`File` coverage — the structural suite
reached **61 tests** by 2026-06-27 (including the `TestShellExtUpgradeSafety`, `TestFirewall`, and
`TestSyncAgentSubsystem` classes) and stands at **79 tests** as of this sweep (2026-07-20), after the
later `TestSparsePackageRegistration` (11 tests, § Shell Extension → *The sparse package — retired 2026-11-16*; 10 since) and
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
  test (`TestUninstallPreservesData`) covers only the *preserve* half. Selective-install runs only in
  the admin test (the CI smoke installs `ADDLOCAL=ALL`). No single environment exercises the full matrix.
- **ARM64 runtime install is covered by the CI smoke, authored 2026-10-14 and green since the public dry run 39718279069 (2026-10-22)** — the x64
  smoke cannot do it (windows-latest cannot install an ARM64 MSI, and "x64 covers both" holds **only** for
  the arch-independent data-removal custom action), so `release.yml`'s `smoke-windows-arm64` job runs the
  same `apps/fauna-windows/installer/ci-smoke.ps1` on GitHub's hosted Windows ARM64 runner
  (`windows-11-arm`), asserting what only arm64 proves — every installed native binary is PE-machine
  arm64, the FaunaNest and FaunaBridge services start and stay Running, and the arm64 shell DLL's 8
  CLSIDs register and activate through COM in a native arm64 process — as well as the x64 smoke's own;
  it gates the `release` job like the x64 smoke (`release-integrity.md` § Release signing). It is never a per-release install on the Windows host
  dev VM (owner ruling, 2026-10-14): a CI-built MSI, machine services included, is a prebuilt binary on
  a machine that builds every release.
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
  loopback auto-enrollment) to the same CalDAV+IMAP round-trip. It ran **green on Windows 2026-06-19**
  (`1 passed, 27s`; perMachine install elevated on the dev box, volatile data + serialized), confirming
  the `FaunaNest`/`FaunaBridge` virtual-account services start, the supervised MDA loopback-auto-enrolls,
  and the round-trip completes (it also caught + fixed a WiX Bridge-feature install bug that had silently
  left the FaunaBridge service uninstalled). **That green is stale:** it came one day
  before the service went TLS-only, and the test kept provisioning over plain `http://127.0.0.1:7450` (the
  `--foreground` `--port` default, never bound under the SCM), so its next elevated run (2026-10-28) timed
  out waiting for TCP there. Since 2026-11-06 the test's nest leg — claim, WS-RPC admin calls, credential
  mint, the port rebind — rides `https`/`wss` to the service's same-box loopback `127.0.0.1:3000`, the
  authority pinned to the floor cert's SPKI read off `%ProgramData%\Fauna\nest\acme\fullchain.pem`
  (nest/common.md § Same-box reach), with no plain-HTTP or verification-off path to the installed nest;
  its MUA legs to the MDA keep `verify=False`, standing for third-party MUAs. The nest leg is proven green
  without an install by `test_caldav_imap_serving.py::test_windows_nest_provisions_over_its_pinned_tls_floor`
  (Windows 2026-11-06, a TLS-serving `--foreground` nest, with a negative control that a disagreeing pin is
  refused before the upgrade); the elevated installed run is owed, and
  until it is green the SCM/supervisor path counts as exercised as of 2026-06-19 only. Since 2026-11-13 it
  also dials the box's own LAN address on CalDAV `:8443` (a round trip with the loopback MUAs), the bare
  address a home nest is reached at; and the **installed-reach** surface (`TestInstallThenReach`) covers
  the nest's own channel promise — claimed and healthy at that address on `:443`, Nest feature alone —
  with no elevated run yet. Neither is a catalog witness until an elevated run has seen it green.
- The MSI is **unsigned**, so the SmartScreen/UAC path is untested (the direct-download signer is Azure
  Artifact Signing, chosen 2026-11-15, its account and identity validation not yet made — § Direct
  download (MSIX app + nest MSI)).

**Shell-extension functional verification** (the COM surface actually activating) is win-runnable without
Explorer — tracked in `docs/goal/architecture/apps/windows.md` § Shell Extension.
