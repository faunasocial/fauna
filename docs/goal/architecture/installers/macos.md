# Installer: macOS — target state

Owns: macos-packaging
Status: ratified
Authority: macOS distribution — the all-in-one signed/notarized .pkg (five-component feature tree, machine-service LaunchDaemons under _fauna/_fauna-bridge with socket-activated :443, the per-user sync agent, uninstall/upgrade semantics) and the .dmg app channel; defers the desktop-app architecture to architecture/apps/macos.md, the shared serve-loop + serving-port substrate to architecture/nest/common.md, the MDA enable/gating contract to behavior/mail-bridge-lifecycle.md (only the macOS supervisor wiring lives here), worker-auth scoping to architecture/nest/worker.md, and the mirrored feature-tree/network-posture decisions to architecture/installers/windows.md.

## Goal

Signed and notarized macOS distribution: a single **all-in-one** `.pkg` metapackage installs the desktop **app** (`social.fauna.app` → `/Applications`, default-on, machine-wide like the Windows `DesktopApp` feature) **and** the background **server** services — the nest and the MDA bridge — as **machine-level `LaunchDaemon`s** under dedicated, hidden service users (`_fauna` / `_fauna-bridge`), and the **sync** agent as a per-user `LaunchAgent` — all as feature-selectable components mirroring the Windows MSI feature tree (app + sync + terminal app default-on, nest + bridge default-off) — the fifth component being the **terminal app** (`social.fauna.tui` → `/usr/local/bin/fauna-tui`, the same binary the release archive carries, Developer ID-signed with the rest; its channel is [`tui.md`](tui.md)). A `.dmg` carries the same desktop app for the app-only download path. The app never updates itself: it tells you when a newer version is out and where to get it ([`README.md`](README.md) § Knowing a newer version is out), and you upgrade by installing the newer `.dmg` or `.pkg`. Apple Silicon only — Intel is not supported and not planned. The all-in-one `.pkg` and the `.dmg` can be used together or separately; the common macOS case ("just the app") is the default app + sync, services unticked — exactly the Windows default.

**Why machine-service, not per-user (decision 2026-06-24).** A nest is an always-on, network-reachable server; a per-user `LaunchAgent` ran only while its installing user held a GUI login session (so it stopped on logout, refused to run headless on a Mac-mini server — `resolve_console_user` aborts with no console user — and two users collided on `0.0.0.0:3000`). The macOS-native shape for always-on background server software is a boot-started `LaunchDaemon` under a dedicated service user — which also lets the nest bind the standard `:443` (via launchd socket activation) and makes a **Mac mini a first-class native Fauna server** (no Docker). This converges macOS *up* to the Windows machine-service shape (priority #1/#3). Only the **server** halves move; **app data is untouched** — the user's identity key (Keychain), message/MLS/pin caches, settings, and synced files stay per-user in `~/Library` + Keychain (the app is per-user on every platform). Full rationale + rejected alternatives (drop-the-nest-for-Docker; status-quo): the machine-service decision record (2026-06-24, tracked internally).

## Implementation status today

Read this first when scoping. The machine-service `.pkg` shape below is **built and
unsigned-install-proven on a real macOS (2026-06-28)**; the remainder is credentialed/manual
(the items needing a maintainer-run environment, listed below) plus the declared gaps below.
Decision history lives in git and the machine-service decision record (2026-06-24, tracked
internally).

| Pillar | Shape (landed) | Proven by | Pending |
|---|---|---|---|
| All-in-one `.pkg` app component (`social.fauna.app`) | app default-selected, non-relocatable → `/Applications`; release build via the host-only FFI (`just mac-app release` → `apple-ffi-host release`) | `TestDryRun` 11/11 + `TestFullInstall` 15/15 green on a real Mac machine 2026-06-28, unsigned `.pkg`; brand `AppIcon.icns` landed and macOS-verified 2026-07-20, regenerated from the current mark on macOS 2026-08-21 | signed re-run; Swift-runtime back-deploy check; `TestFullInstall`'s 16th method (`test_sync_agent_resolves_reachable_production_paths`, added 2026-07-20) still unverified — needs interactive sudo; a 17th method (`test_installed_app_registers_its_file_provider_appex`, added 2026-08-28) encodes the FP-registration finding below (all needing a maintainer-run environment) |
| Machine-service daemons (nest + bridge) | postinstall-generated `/Library/LaunchDaemons` plists under `dscl`-created `_fauna`/`_fauna-bridge`; **enabled-when-selected** (no `Disabled` key; components default-unselected); `FAUNA_DATA_DIR` env; nest `Sockets` dict `FaunaNest` → `SockServiceName 443` (the pre-2026-06-25 per-user→system migration was removed by the compat-remnant sweep, 2026-09-25 — § Identifier domain) | unsigned full-install lifecycle 2026-06-28 — install / `dscl` users / daemon load / dylib relocation / migration / upgrade-preserve / uninstall-preserve | signed install + the **live `:443` socket-activation serving proof** (the suite loads the daemon but does not `curl :443`; load ≠ serve) — needs a maintainer-run environment |
| Nest daemon serve loop | `fauna-nest-daemon` runs the nest in-process via the shared cross-OS `fauna_nest::desktop_serve::run_serve_loop` (the same loop the Windows service drives); `launch_activate_socket("FaunaNest")` feeds the pre-bound-listener seam; off launchd it direct-binds the `0.0.0.0:3000` seed | `classify_activation` unit tests; `bins/fauna-nest/tests/desktop_serve_loop.rs` + `desktop_serve_loopback.rs` | live launchd fd inheritance rides the signed install |
| Bridge supervisor + MDA | `build.sh` builds/stages `fauna-bridge-supervisor` + the Go `fauna-mail-bridge` (MDA role — CalDAV + IMAP, no MTA) + `libfauna_ffi.dylib`, relocated via `install_name_tool` to `@rpath`/`@loader_path`; the supervisor self-gates the MDA child on the nest's enable flags (shared `libs/fauna-mda-supervisor`, decision D-E1) | headless build + `otool` relocation proof on macOS (2026-06-23); flag-gating unit-tested in the shared crate | live Apple Calendar/Mail LAN-MUA CalDAV/IMAP round-trip (needs a maintainer-run environment) |
| Network-reachable bind | canonical `0.0.0.0` listen (the loopback rewrite was removed 2026-06-23); target `:443` via socket activation, internal loopback fixed at `127.0.0.1:3000` | cross-device LAN reach verified manually 2026-06-26 (dev binary on `:3000`); macOS Application Firewall blocks an *unsigned* binary's LAN ingress (needed `socketfilterfw --unblockapp`) — a signed+notarized daemon is auto-allowed | installed-`.pkg` cross-device `:443` reach rides the signed install |
| App side | the app manages no daemons today (`SyncDaemonManager` — which last managed only the per-user sync agent — was **deleted 2026-07-13** with the B2 in-process cutover, `../apps/macos.md` § Sync; the app-tends-its-own-helper role returns with the per-user sync agent, `../apps/sync-agent.md` § Packaging); **no in-app "run a nest" toggle and no runtime elevation** — the install-time component selection is the opt-in (decided 2026-06-26, matching Windows) | `TestDryRun` asserts the enabled-when-selected shape | bridge-requires-nest auto-deselect `selected=` expression (GUI-only verifiable; rides the signed-install proof — needs a maintainer-run environment) |
| `.dmg` channel | the app-only download; the app's update path is notice-only, built 2026-10-08 (§ Upgrade → *Desktop App*): Settings → General's About block and the app-menu item ask through the shared `fauna-ffi` check, and the app looks once per sign-in. **The update framework the app used to embed left the tree the same day**: the bundle embeds no third-party framework, the Apple package pins no remote SwiftPM dependency, and no update feed, feed-signing key or in-place update payload is built, signed or published | `release-macos.yml` (public repository, 2026-08-25): credential-free build job + Environment-gated sign/notarize/staple/attest/publish job, driving `just mac-app release` + `just mac-dmg-package` — the shape is pinned by the publish tooling's own workflow-shape test and its recipe/script references by the publish transform's workflow gates | **never executed**: its first tagged run on the public repository is the first proof of the whole pipeline (the `macos` CI job's first run on a GitHub-hosted runner is the buildability proof before it) |
| Worker auth (S2) | resolved 2026-06-25 as a *confirm*, no new wiring: the co-located MDA authenticates by **service-user enrollment** (keypair file + loopback endpoint from the supervisor), and `bins/fauna-nest-daemon` authorizing no worker is the correct standalone state — worker authorization is the unrelated public-nest storage worker's concern (owner: `../nest/worker.md`) | `bins/fauna-nest/tests/desktop_serve_loopback.rs` (reachable co-located loopback) | — |

Declared gaps (target-state prose ahead of code):

- **Minimum-macOS floors are per-component by design, and the `.pkg` now ENFORCES the split (reconciled 2026-07-19; enforced 2026-08-21).** Read the floors as **per-component**: services 13.0; app 15. Three artifacts state the app's floor and all three agree — `Info.plist`'s `LSMinimumSystemVersion` 15.0 (raised from an understated 14.0), the app's `.macOS(.v15)` compile target (`apps/fauna-apple/Package.swift`), and `MACOSX_DEPLOYMENT_TARGET = 15.0` in the `.xcodeproj`. The `.pkg` volume-check stays at **13.0** (`installer/macos/distribution.xml`), deliberately lower, so a headless services-only install (nest/bridge, app unticked) still reaches an older Mac mini — and the **app choice carries its own floor** instead: `start_enabled`/`start_selected` are gated on an `appComponentSupported()` predicate, so below 15 the Desktop App row is unticked and unselectable rather than installing a bundle Finder then refuses to open. Gating the choice rather than raising the volume-check is what keeps the two floors independent. Pinned twice: `test_installer.py::TestDryRun::test_distribution_xml_structure` asserts the shipped `.pkg` carries the attributes, and `test_distribution_choice_gate.py` (tier_1) has **macOS itself evaluate the predicate** via `installer -showChoicesXML` — as shipped, with the floor raised above this machine, and with the floor set to this machine's exact version — because a Distribution predicate is JavaScript no build type-checks, and the boundary case (`>= 0` vs `> 0`, wrong only for users sitting exactly on the floor) is invisible to any assertion on the XML text.
- **App extensions — the `.appex` path is decided (2026-07-18) and M0 (packaging spike) LANDED (2026-07-18).** `apps/fauna-apple/Fauna.xcodeproj` is a thin, hand-maintained project (`objectVersion 56`; XcodeGen/Tuist rejected — no new third-party build tool in the supply chain) whose app/appex targets are shells. It builds `Fauna.app` with the shared **`Fauna-FileProvider.appex`** — a hello-world `NSFileProviderReplicatedExtension` — embedded in `Contents/PlugIns/`. The transient `Fauna-FileProvider-M0Host` scaffolding + `build-and-prove.sh` are **DELETED (M2 slice 3b, 2026-07-19)**: the `Fauna` app target now compiles the **real macOS app** — the shared `@main` shell `Fauna-macOS-Main/FaunaMacOSMain.swift` linking the SwiftPM library product `FaunaMacOSLib` (= `Fauna-macOS/**`; the SPM `FaunaMacOS` executable wraps the same shell + library, so mac-app and the `.xcodeproj` can never drift), with `INFOPLIST_FILE`/`CODE_SIGN_ENTITLEMENTS` pointing at the real `Fauna-macOS/Resources/Info.plist` / `Fauna-macOS.entitlements` (which now carries `group.social.fauna.shared`). **Bundle identity is unified on `social.fauna.fauna`** (the shipping app id; the plist's literal value — moved off `social.fauna.desktop` 2026-08-23, § Identifier domain) with the appex at `social.fauna.fauna.FileProvider` — xcodebuild's embedding validator *requires* the parent-id prefix, which retired M0's `com.fauna.app` scaffolding ids. ⚠ **Same-bundle-id twin gotcha (bit 2026-07-19):** `NSFileProviderManager` resolves the registering app **by bundle id via LaunchServices**, so any stale appex-less `Fauna.app` twin (an installer-test leftover in `/Applications`, an old e2e `FaunaMacOS.app`) that wins that resolution fails every `register` with FP `-2001`/`-2014` (applicationExtensionNotFound); fix = `lsregister -u` the twins (reversible, bundle untouched) — the tier_3 FP fixture now does this itself. The hidden FP test-arg path (`register|remove|list|provision|revoke|signal`) survives in the app binary (`FaunaKit` `FileProviderTestCLI`) **in DEBUG builds only (2026-09-21, convention 15 — `e2e-automation-surface-gating.md` § The convention): a Release/shipped build, macOS or iOS, carries none of it**, and the slice-3b headless proof (ad-hoc sign → register → CloudStorage dir → remove) replaced `build-and-prove.sh`. The legacy `Fauna-FinderSync` is **deleted** (superseded by File Provider — `../../behavior/on-demand-files.md` § On-Demand Files → Apple File Provider binding). `Fauna-NSE` rides the same project when taken. **Three load-bearing M0 findings (2026-07-18, verified on macOS):** (a) **ad-hoc signing suffices** for build + domain register/remove + Finder/CloudStorage appearance + enumerate/hydrate — **no Apple Developer certificate is needed** for headless dev/test (this machine has 0 signing identities); (b) a replicated FP extension **requires an app group** (`NSExtensionFileProviderDocumentGroup` = `group.social.fauna.shared`) for its on-disk replica, and the app-group entitlement is a *restricted* capability xcodebuild refuses to sign without a provisioning profile — so the recipe is **build unsigned (`CODE_SIGNING_ALLOWED=NO`) + manual ad-hoc `codesign --entitlements`**, exactly the `mac-app` pattern; (c) macOS ships a newly-installed FP extension **DISABLED** — a one-time human step **enables it** in *System Settings → General → Login Items & Extensions → Extensions (i) → "Fauna Extensions" → File Provider* (per-extension, survives app reinstall); until enabled, every enumeration fails `NSFileProviderErrorDomainDisabled` (FP -2011, "Sync is not enabled"). This one enable is also the sole gate for `file-sync.md`'s headless tier_3 FP tests. **FIRST REAL-INSTALL OBSERVATION (2026-08-28, supervised, a Developer-ID-signed `--sign-only` `.pkg`) — and it CORRECTS this finding's ordering: the extension does NOT appear in System Settings on install. A DOMAIN MUST BE REGISTERED FIRST.** Measured in order on a fresh install of the `.pkg`: the appex is in the installed bundle and macOS registers it at install time (`pluginkit -m -v -p com.apple.fileprovider-nonui` lists `social.fauna.fauna.FileProvider(1)` at `/Applications/Fauna.app/Contents/PlugIns/Fauna-FileProvider.appex`, with **no** explicit enable/disable state), `mdfind` names exactly one macOS claimant — yet *System Settings → Extensions showed nothing for Fauna*. After one `NSFileProviderManager.add` (the app's `register` test verb, which needs no keychain and no sign-in), **"Fauna Extensions" → File Provider appeared and the human toggle enabled it, and the enable stuck.** So the human step is real but it is the SECOND step, not the first; an install-then-look runbook measures nothing. ⚠ **That probe is no longer reproducible from a shipped build (2026-09-21):** the `register` verb it used is DEBUG-only, so a Release install registers a domain only through the app's own authenticated-launch reconcile (`FileProviderCoordinator.reconcile` — a signed-in account with at least one own folder that the on-demand toggle covers, default ON, and that is not bound to a local folder), and until then System Settings lists nothing for Fauna. The measured ordering finding stands — the extension surfaces only once a domain exists — and its runbook consequence is that an install-then-look check on a shipped build must sign in and leave a folder on the on-demand default first. The enable is also observable in behavior, not just in the pane: enumerating the location `ls`-timed-out (`fts_read: Operation timed out`) before it and **completed** after it. **What the enable does NOT fix is the capability**, and the appex now says so in its own voice — the first measurement of the `-34018` gate from INSIDE the shipped extension rather than from the app: `Fauna-FileProvider[…] [com.apple.securityd:OSStatus] error:[-34018] "Client explicitly specifies access group 7457N3M72H.group.social.fauna.shared but is only entitled for (com.apple.token)"`, whence `FP -1000 "You need to authenticate before accessing this item"` on `fetch-children-metadata(.root)`, an empty location, and Finder's *not signed in*. Note precisely what that refutes: the installed appex **does** carry `com.apple.security.application-groups = 7457N3M72H.group.social.fauna.shared` (verified with `codesign -d --entitlements`) — an app-group entitlement is **not** a keychain-access-group entitlement, and only the latter opens the data-protection plane. Owner of the fix: § Identifier domain → Path A (provisioning profiles). (`log show` returns nothing on a macOS VM — use `log stream` to read fileproviderd.) **The shipping `.pkg`/`.dmg` CARRY the extensions (2026-08-28 — the M2–M5 packaging gap, closed).** `just mac-app` — the one step both channels share, so this reaches the dev `.pkg` and the public `.dmg` from one implementation — now builds the bundle with **`xcodebuild -scheme Fauna`** instead of assembling it from the SwiftPM `FaunaMacOS` executable, and the `Fauna` target's *Embed App Extensions* phase puts `Fauna-FileProvider.appex` + `Fauna-FileProviderUI.appex` (and, since 2026-09-26, the home-screen widget's `Fauna-Widget.appex`) in `Contents/PlugIns/` with the parent-id prefix validated. **Route of record, and why:** the alternative — keep the SwiftPM app and embed a separately-`xcodebuild`-ed appex — was rejected because it compiles `FaunaKit` twice (once per build system) and leaves version / identifier / deployment-target agreement as a standing invariant to police, whereas one build system has no agreement to police; it also converges macOS onto the shape iOS already ships through. Nothing about *what is compiled* changed: the app target links the same `FaunaMacOSLib` + shared `@main` shell the executable did (the can-never-drift argument above). The recipe still stages what the project does not — the rendered `AppIcon.icns` (the Resources phase is empty by design: the `.icns` is a build artifact, not a source) and the bundled `fauna-sync-agent` — then ad-hoc-signs **inside-out**, each `.appex` with **its own** entitlements file before the app (§ Build Pipeline step 4). Derived data lives in the gitignored `apps/fauna-apple/.xcode-build`, the single xcodebuild cache per checkout (the tier_3 FP test builds there too) and a citizen of the mac sweep. ⚠ **The route re-arms the twin gotcha above, and the recipe disarms it:** xcodebuild's last step is `lsregister -f -R` on its own product, so *building* now leaves a second bundle claiming `social.fauna.fauna` — precisely the appex-less twin that fails every FP register with `-2001`/`-2014`. `mac-app` therefore `lsregister -u -R`s the derived-data copy after taking its own copy out (`-R` to match what xcodebuild registered, so the nested `.appex` bundles leave too); a build registers nothing, exactly as before the move (its **exit code is ignored**: measured 2026-08-28, the benign `-10814 from spotlight` scan makes `lsregister` exit **1**, which under `set -e` with stderr dropped killed a `pkg-sign-only` run 857 s in with no diagnostic at all — the unregister is best-effort housekeeping, and the invariant it serves is the claimant check below, not its status), and the bundle users get is registered by the installer. Measured 2026-08-28: after the unregister, `social.fauna.fauna` has exactly one claimant, `/Applications/Fauna.app`. Pinned on the built artifact by `test_installer.py::TestDryRun::test_bundled_agent_carries_its_own_entitlements` (PlugIns present; the FP appex carries only the shared group + sandbox + network, the UI appex neither group nor network, the home-screen widget appex only the shared group + sandbox and no network — added 2026-09-26, [`../apps/ios.md`](../apps/ios.md) § Home-screen widget — the app both groups) and by the tier_4 bundle suite's required-contents list. **iOS targets LANDED (M4 task B, 2026-07-19):** the same `.xcodeproj` now also builds **`Fauna-iOS.app`** (id family **`social.fauna.fauna`**, appex **`social.fauna.fauna.FileProvider`** — same parent-prefix rule; **the SAME family as macOS since 2026-08-23**, because universal purchase puts both platforms on one App Store record and therefore one bundle id — § Identifier domain) embedding an iOS `Fauna-iOS-FileProvider.appex` from the SAME appex sources, over the mirrored SPM split `FaunaiOSLib` + `Fauna-iOS-Main/FaunaiOSMain.swift` (the shell also carries the FP test CLI in DEBUG builds only, driven on iOS via `simctl launch` args). Two iOS-only signing/packaging findings (bit 2026-07-19, simulator): **(i)** the iOS *install validator* requires `CFBundleVersion`/`CFBundleShortVersionString`/`MinimumOSVersion` in the appex plist — the iOS appex configs set `GENERATE_INFOPLIST_FILE = YES` to merge them over the shared source plist ("Invalid placeholder attributes" at `simctl install` otherwise); **(ii)** unlike the macOS build-unsigned-then-manually-codesign recipe, the simulator app must be **signed at build time** (plain `xcodebuild -sdk iphonesimulator` without `CODE_SIGNING_ALLOWED=NO`; no certificate involved) — CoreSimulator provisions app-group containers by parsing the **install-time** signature, and a post-build manual re-sign leaves `GroupContainers = {}`, after which fileproviderd fails every domain op with FP `-2001`/`-2014` ("Error fetching group container"). With build-time signing, the domain CLI proves register/list/remove headlessly in the simulator (M4 task D).

- **First-run experience on the `.pkg` channel: measured unsigned 2026-07-20 (four defects, two P0), re-measured SIGNED 2026-08-23 (the § Identifier domain matrix — supervised double-click installs of Developer-ID-signed `.pkg`s).** The 2026-06-28 `TestFullInstall` proof predates the sync-agent cutover and asserts *installed state*, not *first-run behavior* — the supervised installs surfaced what the suite cannot see. The agent crash-looped under launchd (relative base dir from an empty `HOME` → log-init panic; **fixed** — the resolution invariant is owned by `../apps/sync-agent.md` § Packaging + lifecycle), and the `kTCCServiceSystemPolicyAppData` question is now **MEASURED (2026-08-23, § Identifier domain's matrix record): the Team-ID-prefixed group makes the APP fully silent; the agent still prompts once** — bare or bundled, the prefix does not cover a background executable — and **RESOLVED 2026-08-25 by moving the agent's state out of the container** (§ Identifier domain record item 6; the design record and the deny-crash bug's headless witness are at `../apps/sync-agent.md` § Implementation status today → A4 item 2): the agent now has no TCC prompt on either channel. Two `.pkg`-channel-specific consequences here: (a) **the two channels' TCC divergence dissolved with the move** — before it, the `.dmg`'s bundled agent prompted as "Fauna" on the app's shared TCC subject while this `.pkg`'s `/usr/local/bin` exec prompted as a bare path (`attributed bundle: (null)`) with macOS's scariest wording (measured, not expected: signature-based group authorization did NOT make attribution irrelevant); the `.pkg`'s LaunchAgent still prefers the bundled copy when the app component is installed, so anything the agent is ever attributed to is the app's subject. Also measured on this box: **a `.pkg` whose `/Applications/Fauna.app` destination is occupied by a bundle with a DIFFERENT bundle id** (the Jul-20 ad-hoc `social.fauna.desktop` fossil) makes Installer side-step into `/Applications/Fauna.localized/` — display-named "Fauna", so the user sees two identical-looking apps; real user machines can't hit this pre-1.0 (no foreign-id artifact ever shipped) but a preinstall guard is cheap insurance once upgrades are real; (b) **fixed 2026-07-20** — `Fauna.app` shipped with no `CFBundleIconFile`, rendering a question-mark icon in the launcher; `Resources/AppIcon.icns` now renders Fauna's one brand mark (`sites/fauna-social/public/favicon.svg`, via a dedicated icon-rendering script; the same source the Windows MSIX-logo renderer already used), staged into `Contents/Resources/` by the `mac-app` justfile recipe. **The committed `.icns` renders the CURRENT mark as of 2026-08-21**, closing the gap this bullet carried from 2026-08-13: a 2026-07-29 mark change had moved the SVG outside the subset the dev-fleet rasterizer accepted, so every derived icon silently kept the *previous* mark (the committed `.icns` was still the retired green leaf; the mark is now the beaver head). The rasterizer was extended and the Windows artifacts regenerated 2026-08-13, but `.icns` could not be — `render-app-icons.py` shells out to `iconutil`/`sips` and skips that output on any non-Apple platform — so the leg was entrusted to macOS and run there 2026-08-21. That run also re-rendered `AppIcon.ico` **byte-identically** to the artifact committed on 2026-08-13, which is the evidence that both platforms agree on the same deterministic render. **A future mark change still owes a re-run of the icon-rendering script on macOS** — the `.icns` leg is macOS-only by construction, and no gate can produce it. Recurrence of the *silent* variety is gated by the cheap merge gate `just brand-mark-check`, which blocks any mark the rasterizer cannot read. A stale same-bundle-id twin also re-bit here in a new costume — the *keychain* ACL rather than FP registration (see the M0 twin gotcha above): three bundles declaring `social.fauna.desktop`, ad-hoc signed with differing cdhashes, made every "Always Allow" fail to stick. Full findings, evidence, and owed tests tracked internally.

**Needs a maintainer-run environment** (credentialed/manual; tracked internally): the signed/notarized `build.sh --sign` run (re-running the 11 + 16 suite against the signed `.pkg`, including the still-unverified `test_sync_agent_resolves_reachable_production_paths`); the Swift-runtime back-deploy check on a vanilla box; the live `:443` socket-activation serving proof; the bridge-requires-nest auto-deselect expression (rides the signed GUI install); a live Apple Calendar/Mail LAN-MUA round-trip.

History (one-liners): 2026-06-23 `build.sh` repaired post-I6 + MDA shipped + loopback listen-rewrite removed · 2026-06-24 machine-service re-shape decided · 2026-06-25 foundations (`FAUNA_DATA_DIR`, bind-fallback, shared serve-loop lift, S1 socket activation) + LaunchDaemon packaging + app-side watcher removal · 2026-06-26 enabled-when-selected + no-in-app-toggle decision + manual cross-device reach · 2026-06-28 unsigned full-install lifecycle + app component proven (`TestDryRun` 11, `TestFullInstall` 15).

## Distribution Channels

A single **all-in-one `.pkg`** is the primary installer (the desktop app + all
services, feature-selectable); a **`.dmg`** is retained for the app-only download.
The two can be used together or separately.

### 1. All-in-one Package (.pkg metapackage)

A signed, notarized `.pkg` metapackage that installs the desktop **app**
(`Fauna.app` → `/Applications`) **and** the background **server** services (nest,
bridge) as machine-level `LaunchDaemon`s under dedicated hidden service users, plus
the **sync** agent as a per-user `LaunchAgent` (§ launchd jobs).

- **Source:** `installer/macos/`
- **Signing:** Developer ID Application certificate (`FAUNA_SIGN_IDENTITY`)
- **Notarization:** `xcrun notarytool` using `FAUNA_APPLE_ID`, `FAUNA_TEAM_ID`, `FAUNA_NOTARY_PASSWORD`
- **Minimum macOS:** 13.0 (Ventura) — the `.pkg` volume-check floor, valid for the *service* components; the app component requires 15 and its choice is install-gated at that floor (§ Implementation status today — the floors note)
- **Architecture:** Apple Silicon only (aarch64-apple-darwin)

The installer presents **four** selectable components mirroring the Windows MSI
feature tree — the desktop **app** and **sync** default **selected** (the common
macOS case is "just the app": app + sync, connecting to a remote nest), **nest
and bridge default unselected** (selecting a server component is the install-time
opt-in to self-host, matching Windows "Nest Service default OFF" — § launchd jobs →
Default state). Untick the **app** for a headless services-only / Mac-mini-server
install. The three *service* components' plists are **generated by their postinstall
scripts** (not shipped as payload files), so the script can wire the service user,
system paths, and socket-activation dict; a selected nest/bridge component installs
its daemon **enabled + auto-started** (not `Disabled`), so it runs right after
install. The **app** component is a straight bundle install (no postinstall) to
`/Applications`, pinned **non-relocatable** (`BundleIsRelocatable=false`) so it
always lands in `/Applications`:

| Package | Bundle ID | Contents |
|---|---|---|
| `fauna-app.pkg` | `social.fauna.app` | the `Fauna.app` bundle → `/Applications` (machine-wide, non-relocatable; the app's *data* stays per-user in `~/Library` + Keychain). The native macOS app, built from `apps/fauna-apple/` via `just mac-app`. Default **selected** |
| `fauna-nest.pkg` | `social.fauna.nest` | `fauna-nest-daemon` binary (the macOS nest service shell the LaunchDaemon runs — socket activation + the shared serve loop; **not** the standalone `fauna-nest`) + a `/Library/LaunchDaemons` plist (postinstall-generated, runs as `_fauna`) |
| `fauna-sync.pkg` | `social.fauna.sync` | `fauna-sync-agent` (the per-user agent the `social.fauna.sync-agent` LaunchAgent runs — A4 cutover, `../apps/sync-agent.md` § Packaging) (its only binary since the legacy `fauna-sync` daemon left the payload, 2026-10-02 — `../apps/sync-agent.md` § Headless deployment; the component keeps its name and identifier) + a per-user `~/Library/LaunchAgents` plist (postinstall-generated, runs as the console user, **enabled**) |
| `fauna-tui.pkg` | `social.fauna.tui` | `fauna-tui` (the terminal app — the same binary the per-OS release archive carries, [`tui.md`](tui.md) § The ratified channel; here Developer ID-signed and notarized with the rest, which is what keeps the login-Keychain identity it shares with the desktop app stable across updates) → `/usr/local/bin`, beside the sync component's agent, which it resolves as a sibling. No scripts. Default **selected** (Windows `TerminalApp`, default ON) |
| `fauna-bridge.pkg` | `social.fauna.bridge` | `fauna-bridge-supervisor` (Rust MDA supervisor — the LaunchDaemon runs this as `_fauna-bridge`) + `fauna-mail-bridge` (Go, **MDA role** — CalDAV + IMAP; the MTA stays on the public/Docker nest, per the residential-desktop scope) + `libfauna_ffi.dylib` (the MDA's FFI dylib, resolved via `@loader_path` from beside it) + a `/Library/LaunchDaemons` plist (postinstall-generated) |

The three **service** components include the shared `fauna-uninstall` script (which
also removes `/Applications/Fauna.app`); an app-only install uses the macOS-native
drag-to-Trash.

### 2. Desktop App (.dmg)

A signed, notarized `.dmg` containing the **same** native macOS app (built from
`apps/fauna-apple/`). The all-in-one `.pkg` now also installs the app, so the `.dmg`
is no longer the *only* way to get it; it keeps one job: the **app-only download**
— the lightweight "I have a remote nest, just give me the app" path, preserving the
macOS-native drag-to-Applications experience. It is also how an app-only user
upgrades: the app's newer-version notice names the release page that carries the
new `.dmg` (§ Upgrade → *Desktop App*). Nothing downloads or replaces the bundle on
the app's own initiative — the update-notice rule ([`README.md`](README.md) § Knowing
a newer version is out, ruled 2026-10-03) retired the in-place update payload this
channel used to carry.

**⚠ What the headless tests can and cannot witness about this channel — measured on
macOS 2026-08-27 (Darwin 25.6.0), correcting a premise that had stood in the suite
since it was written.** Gatekeeper is **not** only LaunchServices' business: exec'ing
the inner executable of a *quarantined* bundle invokes it too
(`com.apple.syspolicy.exec` logs `GK performScan` → `GK evaluateScanResult`, with a
TLS round trip for a notarization ticket), and on an un-notarized build
`CoreServicesUIAgent` then presents the blocking "Apple could not verify …" dialog.
The process stays alive and silent behind that modal, so an unattended run does not
fail — it **hangs** until its budget expires, and no budget can rescue it: a modal
nobody dismisses never resolves. Therefore, for as long as no notarized build exists,
**an unattended launch of a quarantined download is not a thing this channel can do**,
and no test may claim it. What `tests/artifact/test_macos_dmg_install.py` proves
headlessly is the packaging round trip, the quarantine stamp an install carries, the
surviving signature, Gatekeeper's *refusal* of the un-notarized build, and the launch
a user reaches **after** giving the approval that refusal demands — the approval
modelled explicitly and gated on the build still being un-notarized, so the stand-in
retires by failing the day notarization lands. The unattended launch is the
credentialed inch, and it arrives with the first notarized `.dmg`, not before.

- **Build command:** `just mac-dmg` (= `just mac-app release` + `just mac-dmg-package`)
- **Where it is built (decided 2026-08-25): in the public repository, by its own `Release (macOS)` workflow, on a GitHub-hosted runner — and attested to it.** The download is the output of the public tree at the tagged commit, so what ships is what anyone can build from the published source, and `gh attestation verify Fauna-<version>.dmg --repo faunasocial/fauna` proves a downloaded file is that exact output (§ Build Pipeline → *The `.dmg` release pipeline*). A self-hosted "trusted" runner was considered and rejected for this artifact: its attestation would be the machine vouching for itself, which is less than a GitHub-hosted runner's, and the signing credentials it would hold belong on a Tier-0 box the project does not have (`../release-integrity.md` § Trust tiers).
- **Signing:** Developer ID Application
- **Notarization:** `notarytool` with an App Store Connect API key (the release workflow) or the Apple-ID credentials the all-in-one package uses (a maintainer's local run) — both shapes in § Build Pipeline → *Required Environment Variables*
- **Minimum macOS:** 15.0 — the bundle's declared `LSMinimumSystemVersion`, reconciled to match the app's `.macOS(.v15)` compile target (`apps/fauna-apple/Package.swift`) (§ Implementation status today — the floors note)
- **Updates:** notice-only — the app tells you a newer version is out and where to get it, never downloads or replaces itself (§ Upgrade → *Desktop App*)
- **URL scheme:** Registers `fauna://` for deep linking

## File Layout

The all-in-one package installs the desktop **app** to `/Applications`, the service binaries
machine-wide, the **server** state under system paths owned by the service users, and the
**app/sync** state per-user (the client/server data split — only the server halves are
machine-scoped):

```
# Desktop app (machine-wide, the social.fauna.app component):
/Applications/Fauna.app                  (non-relocatable; the app's data is per-user — see below)

# Binaries (machine-wide):
/usr/local/bin/fauna-nest-daemon
/usr/local/bin/fauna-tui                 (the terminal app — the social.fauna.tui component; the same
                                          binary the release archive carries, tui.md)
/usr/local/bin/fauna-sync-agent          (CLI/services-only fallback; the social.fauna.sync-agent
                                          LaunchAgent PREFERS the copy at
                                          /Applications/Fauna.app/Contents/MacOS/fauna-sync-agent —
                                          bundle attribution puts its one TCC prompt on the app's
                                          own subject, § Identifier domain record item 5/6)
/usr/local/bin/fauna-bridge-supervisor   (MDA supervisor — the social.fauna.bridge LaunchDaemon runs this)
/usr/local/bin/fauna-mail-bridge         (Go MDA, spawned by the supervisor)
/usr/local/bin/libfauna_ffi.dylib        (MDA's FFI dylib; resolved via @loader_path from beside fauna-mail-bridge)
/usr/local/bin/fauna-uninstall

# Server — machine LaunchDaemons + system data, owned by the dedicated service users:
/Library/LaunchDaemons/social.fauna.nest.plist     (runs fauna-nest-daemon as _fauna)
/Library/LaunchDaemons/social.fauna.bridge.plist   (runs fauna-bridge-supervisor as _fauna-bridge)
/Library/Application Support/Fauna/             (_fauna: nest.db, config, nest/floor/ACME keys, enable + serving-port flags, services.json)
/Library/Application Support/Fauna/logs/        (_fauna: the nest daemon's size-capped log — ../apps/observability.md § Persistence & privacy; neither daemon plist redirects stdout/stderr)
/Library/Application Support/Fauna/bridge/      (_fauna-bridge: MDA keypair + operator-hatch.toml)
/Library/Application Support/Fauna/bridge/logs/ (_fauna-bridge: the supervisor's size-capped log, the MDA child's captured output included)

# App + sync — per-user (UNCHANGED; the user's data stays theirs):
~/Library/LaunchAgents/social.fauna.sync-agent.plist  (sync agent — runs as the user, enabled)
~/Library/Application Support/Fauna/            (the USER-DOMAIN home: app data — messages/MLS/settings, the app's deployment-seed config-replica; the identity key is in the Keychain — on a signed build the data-protection plane's app-only access group, § Identifier domain)
~/Library/Application Support/Fauna/sync/       (the sync agent's OWN config/state-db/chunk-cache/logs + the account store, per-actor scoped — the platform state base every non-sandboxed process shares; ../apps/sync-agent.md § Packaging owns the mechanics)
~/Library/Application Support/Fauna/sync-agent.sock   (the agent's per-user IPC socket)
~/Library/Application Support/Fauna/trust/      (the install-scoped nest-identity pin store — the app writes, the agent + tui read)
(no ~/Library/Logs/Fauna/ — it held the removed `fauna-sync install` job's log; the agent logs under sync/logs/ and no LaunchAgent redirects anywhere — ../apps/observability.md § Persistence & privacy)
~/Library/Group Containers/7457N3M72H.group.social.fauna.shared/  (the CONTAINER domain — the sandboxed File Provider extension's root ONLY since 2026-08-25: its per-set state under sync/, FileProvider/roots, domain-owners.json, and the app-maintained pin REPLICA under trust/. The agent never opens it — § Identifier domain record item 5; the per-domain law is ../../behavior/on-demand-files.md § Apple File Provider binding, *state unification*)
```

> **No legacy-install migration.** A `.pkg` built before the 2026-06-25 machine-service packaging
> installed `nest`/`sync`/`bridge` as per-user LaunchAgents with all state under
> `~/Library/Application Support/Fauna`; the nest postinstall's per-user→system migration of that
> server state was removed by the compat-remnant sweep (`../version-compatibility.md` § Dimension 2,
> the fourth ratified exception — no such install exists).

**Data-dir wiring — `FAUNA_DATA_DIR` (bucket-2 IPC, never a human knob).** The server daemons find
their system data-root via the `FAUNA_DATA_DIR` environment variable the `LaunchDaemon` plist sets —
artifact-wiring the supervisor injects, the IPC sibling of `FAUNA_FRONTED_BY_ROUTER` /
`FAUNA_INTERNAL_LOOPBACK_PORT`, **not** a config a human edits (`principles.md` § One configuration surface — there is
no operator). Both server binaries honour it: `fauna-nest` relocates its whole on-disk layout —
`nest.db`, blobs, sidecar tokens, factory-reset, deployment key (the nest's single identity — the
legacy separate `nest_identity.key` is retired, `nest/box-recovery.md` § Single-identity
unification) — by deriving
`db_path = <FAUNA_DATA_DIR>/nest.db` (`bins/fauna-nest/src/main.rs::data_dir_db_path_override`; an
explicit `--db` still wins, then this env, then the config-file `db_path`); `fauna-bridge-supervisor`
uses it as the nest data-dir it reads flags from + the parent of its `bridge/` child
(`bins/fauna-bridge-supervisor/src/main.rs::data_dir_support_override`). `acme.dir` is pinned separately
by the daemon's config (also bucket-2 — the same split the Windows `fauna-nest-service` uses, pinning
`acme.dir = data_dir/acme` in its `NestConfig`). Unset (a legacy per-user install, or a dev run) ⇒ the
home-derived `~/Library/Application Support/Fauna` — full back-compat.

## Versioning

`CFBundleShortVersionString` (both `Resources/Info.plist` files) and every pbxproj `MARKETING_VERSION` carry the fleet-wide **product version**, sourced from the root `Cargo.toml` — owner [`../product-version.md`](../product-version.md); the `version-lockstep-check` merge gate holds all the copies equal. `CFBundleVersion` / `CURRENT_PROJECT_VERSION` are **pinned at `1`**: under the owner's reship rule an App Store Connect re-upload mints a new product patch version rather than a new build number, so each version train has exactly one build. The `.pkg` reads the version straight from `Cargo.toml` at build time (`installer/macos/build.sh`).

## Identifier domain

**Every apple identifier the OS or a store registers is reverse-DNS on `social.fauna.` — the
organization's own domain.** Swept 2026-08-12, deliberately inside the pre-signing window: no
notarized artifact had ever shipped, so no pkg receipt, provisioning profile, app-group
container, or keychain item existed anywhere under the old spelling and the rename cost
nothing. After the first signed artifact each of these becomes an upgrade-identity migration.

The rule for a *new* identifier, inherited verbatim from
[`android.md`](android.md) § Store identity: **it moves to the org domain iff the OS or a store
registers it and it therefore persists on a user's machine.** Internal source namespacing does
not — which is why android's `com.fauna.app` Java package and the UniFFI Kotlin `package_name`
`com.fauna.ffi` are ratified to stay, and are not drift to fix.

**How the LEAF is spelled — ratified 2026-08-22** (user-directed, after an independent
review; supersedes the platform-suffix habit that produced `social.fauna.ios` and
`social.fauna.desktop`):

> **One identifier per registry unit, rooted at `social.fauna.`, with the leaf being the
> PRODUCT NAME — `social.fauna.fauna`, lowercase — and NEVER a platform word or a generic
> word, because the registry already encodes the platform and a generic leaf encodes nothing.**

The platform-word half is not style; platform leaves are the ones history falsifies. Both of
ours were falsified inside a month: Apple's **universal purchase** requires iOS and macOS to
share ONE bundle id ([add platforms](https://developer.apple.com/help/app-store-connect/create-an-app-record/add-platforms/)),
which killed `social.fauna.ios` *and* `social.fauna.desktop` at a stroke. The registry-conventions
half is why a single uniform leaf across stores is impossible rather than merely inelegant:

**The same leaf satisfies every registry we get to choose on** — this is *convergent*
uniformity, not manufactured uniformity: no store's rules are being fought.

| Registry | Identifier | Governing convention |
|---|---|---|
| Apple (iOS + macOS + later tvOS/visionOS — ONE record) | `social.fauna.fauna` | Apple polices no leaf (charset is alphanumerics, hyphens and periods) and documents a product-name leaf — its actual current example is `com.example.mycompany.HelloWorld` ([preparing your app for distribution](https://developer.apple.com/documentation/xcode/preparing-your-app-for-distribution)). ⚠ Apple's examples **capitalize** the leaf; the lowercase decision below does not rest on Apple. Universal purchase forces one string family-wide; watchOS rides as `social.fauna.fauna.watchkitapp`, never its own record |
| Flathub / AppStream / D-Bus / GtkApplication (linux) | `social.fauna.fauna` | Flathub **bans** `.desktop`, `.app` and `.linux` leaves as *generic* and blesses the product-name repeat — *"It's fine to repeat the application name"* ([requirements](https://docs.flathub.org/docs/for-app-authors/requirements)); the manifest id must equal the metainfo `<id>`; [Flatpak conventions](https://docs.flatpak.org/en/latest/conventions.html) **recommend lowercase** for the leaf too |
| Google Play (android) | `social.fauna.fauna` | No leaf constraint; lowercase is the package-name convention |
| Microsoft Store (windows) | `FaunaSocial.FaunaSocial` | **Minted by Partner Center** — the one genuine exception, not a choice ([`windows.md`](windows.md) § Identity & coexistence) |
| Apple Kids Category + Google Play Families (the **Fauna Kids** flavor, ios + android) | `social.fauna.faunakids` | The same rule applied to a second product: leaf = the product name *Fauna Kids*, lowercase (`social.fauna.kids` would be the generic leaf the rule forbids — rejected 2026-09-25). A second registry unit because a kids listing cannot share an identifier with the 13+ main listing; decision owned by [`../../behavior/family-safety.md`](../../behavior/family-safety.md) § The account age band |

**Lowercase is deliberate — and it is the LEAF that is contested, not legality.** Both
cases are legal everywhere: Android imposes no case rule and `com.Slack` ships at scale, so
uppercase is legal-but-unconventional there, not forbidden. Lowercase wins on the written
guidance of the layer that has any: [Flatpak conventions](https://docs.flatpak.org/en/latest/conventions.html)
— *"the application portion is recommended to be in lowercase as well"* — and the AppStream
spec, *"even though uppercase letters are permitted in a component-ID, it is strongly
encouraged to only use lowercase letters."* Against that stand only unwritten habits
(GNOME's CamelCase convention, Apple's capitalized doc examples). Lowercase is also the case
humans and third-party catalogs normalize toward when re-typing an id, which is the single
best defence against the two-cases hazard above. Nowhere is lowercase risky; a capitalized
leaf buys conformance with habit and costs it on the Linux specs, android convention, and
re-typing resilience.

**The stutter is correct, not awkward** — it is the standard shape for a single-product
organization on the one registry that actually reviews names: `com.slack.Slack`,
`org.signal.Signal`, `com.discordapp.Discord` are all **Flathub** ids. ⚠ Cite them as
*Flathub* precedent only — those products' Apple and Android ids are unrelated
company-history fossils (`com.tinyspeck.slackmacgap`, `org.whispersystems.signal`,
`com.hammerandchisel.discord`), and presenting the trio unlabeled invites a real conflation
that happened once here. The true precedent for **one lowercase string on every registry**
is **Mozilla: `org.mozilla.firefox` is both the macOS bundle id and the Flathub id** —
chosen on Flathub specifically to match Mozilla's other platforms, which is exactly this
plan. Fauna Social makes Fauna; org domain + product name is the honest encoding.

**⚠ Apple's own documentation CONTRADICTS ITSELF on case, and that is why the pin test must
compare BYTE-EXACTLY.** The [CFBundleIdentifier reference](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleidentifier)
says bundle ids are *case-insensitive*; the [AppleCare deployment guide](https://support.apple.com/guide/deployment/bundle-ids-for-iphone-and-ipad-apple-apps-depece748c41/web)
says *case sensitive*. Operationally: the **registry** is case-insensitive (you cannot hold
both variants as distinct App IDs), but **every tool, plist and MDM allowlist compares
case-sensitively** — documented casualties include a fastlane lookup failing across a case
difference ([discussion 22219](https://github.com/fastlane/fastlane/discussions/22219)) and an
entitlement/prefix mismatch blocking TestFlight ([forum 819996](https://developer.apple.com/forums/thread/819996)).
**The hazard is never a particular case — it is the EXISTENCE of two cases of one id.** So:
`tests/e2e-unified/tests/test_apple_identifier_pins.py` must compare byte-exactly, every new
literal site joins it (entitlements, AASA, `BGTaskSchedulerPermittedIdentifiers`, the
`.watchkitapp` suffix), and **no artifact may EVER ship under the capitalized twin, test
builds included** — on Play and Flathub the two cases are genuinely distinct namespaces
(`play.google.com/…?id=com.Slack` resolves while `…?id=com.slack` 404s; Flatpak's
`org.mozilla.firefox` vs Fedora's `org.mozilla.Firefox` are two separate apps that
`flatpak update` cannot cross, which stranded users on an outdated build).

⚠ **Two schemes were considered and REJECTED 2026-08-22 — do not re-litigate without new
evidence.**

1. **Platform-family leaves** (`social.fauna.apple` / `.linux` / `.android`): Flathub's ban
   makes `social.fauna.linux` unregisterable, so the scheme cannot be uniform — which was its
   whole point. `social.fauna.apple` fell with it: an independent review found **no** written
   Apple rule reaching bundle identifiers (the trademark guidance covers product names and
   second-level domains; the only namespace Apple protects is its own `com.apple.` prefix) and
   live App Store apps carrying `apple` as an exact dot-delimited segment — so it is *probably*
   safe at ~80% confidence, with no certainty available. Not a trade worth making on a string
   that **freezes permanently at first build upload**.
2. **A generic leaf** (`social.fauna.app`, briefly recorded earlier the same day): rejected
   because it encodes nothing — every one of them is an app — and because Flathub classifies
   `.app` as *generic* and bans it. ⚠ **Logged because the reasoning failed once:** that ban was
   momentarily read as grounds to keep `.app` on Apple. It is the opposite — evidence that
   `.app` is a poor leaf wherever it is merely *tolerated*. Killing the argument for one
   candidate never establishes the incumbent.

| Tier | Identifiers | Retired spelling |
|---|---|---|
| `.pkg` component ids | `social.fauna.{app,nest,sync,tui,bridge}` (§ Components) | `com.fauna.*` |
| launchd labels | `social.fauna.{nest,bridge}` (daemons), `social.fauna.sync-agent` (agent) | `com.fauna.*`, plus the pre-A4 `com.fauna.sync` and `social.fauna.sync` (the removed `fauna-sync install`'s own agent, gone 2026-10-02) — none swept (below) |
| App groups | macOS: `7457N3M72H.group.social.fauna.shared`; iOS: `group.social.fauna.shared` (app + appex — per-OS fork decided 2026-08-23, below; **the sync agent names NO app group since 2026-08-25**, record item 6); `7457N3M72H.group.social.fauna.account` (macOS app ONLY — the keychain access group `KeychainStore`'s data-protection-plane rows live in, deliberately not the shared group so the sandboxed appex can never read the identity seed; keychain-only, no container dir ever resolved; decided 2026-08-25 with the keychain-plane migration, `../apps/ios.md` § Credential Storage); `group.social.fauna.watchkit` (watch widgets) | `group.com.fauna.*`; unprefixed `group.social.fauna.shared` on macOS (never shipped — dev boxes only); the agent's `7457N3M72H.group.social.fauna.shared` claim (never shipped — `test_apple_identifier_pins.py` pins it absent) |
| Keychain services | `social.fauna.push`, `social.fauna.fileprovider`, `social.fauna.account` (the `KeychainStore` account-credential store — decided 2026-08-24, below) | `com.fauna.*`; `social.fauna.desktop`/`social.fauna.ios` (pre-2026-08-23 per-platform leaves), `social.fauna.fauna` (the 2026-08-23 bundle-id echo), `social.fauna.watch` (watchOS's platform-word leaf) — **never read** since the compat-remnant sweep (below) |
| iOS `BGTaskScheduler` ids | `social.fauna.sync.{upload,pull,bg-upload}` | `com.fauna.*` |
| Bundle ids | `social.fauna.fauna` — ONE string for macOS **and** iOS (+ its `.FileProvider`/`.FileProviderUI` appexes) | `com.fauna.*` moved 2026-07-19; the per-platform `social.fauna.desktop` / `social.fauna.ios` leaves retired 2026-08-23 (*How the LEAF is spelled* above). **No upgrade sweep is owed for this tier** (none is owed on any tier since the compat-remnant sweep, below); `test_apple_identifier_pins.py` pins them absent |

**The keychain-service tier follows the purpose-leaf convention, decided 2026-08-24: the
leaf names *what the item is for* (`push`, `fileprovider`, `account`), never a platform word
and never a repeat of the bundle-id product name.** `KeychainStore` — the app's general
account-credential store (actor secret, node URL, device id, and the other account-scoped
rows) — used to echo the bundle id (`social.fauna.fauna`) on macOS/iOS and carry its own
platform-word leaf (`social.fauna.watch`) on watchOS; both violated *How the LEAF is spelled*
for the same reason bundle ids do, just inside a different tier. It is now
`social.fauna.account`, one value for macOS, iOS, **and** watchOS — different devices, so
the shared name never collides, exactly like `push`/`fileProvider` already are for macOS+iOS.

**No retired spelling is read, copied or swept — on any tier (the compat-remnant sweep,
2026-09-25; [`../version-compatibility.md`](../version-compatibility.md) § Dimension 2, the
fourth ratified exception).** The ruling's premise is that no Fauna installation and no Fauna
data exist anywhere, so nothing predating the sweep holds a row under a retired keychain service,
a plist under a retired launchd label, or server state in a per-user nest install. Removed with
it, each pin flipped to a refusal:

- the keychain-service read-forward (`AppleIdentifiers.KeychainService.retiredAccount`, its
  copy-forward and its delete sweep) and the parked-row merge it fed (the `fauna-parked/<key>`
  namespace and shared Rust's `AccountRegistry::adopt_parked_index`) — refused by
  `KeychainRetiredServiceTests` and `test_apple_identifier_pins.py`;
- the postinstalls' and `fauna-uninstall`'s `com.fauna.*` label sweeps (`retire_legacy_labels`),
  the since-removed `fauna-sync` daemon's `uninstall`/`status` old-label lookup and `install`'s old agent-label detection,
  and the nest postinstall's per-user→system server-state migration — refused by
  `test_apple_identifier_pins.py::test_no_installer_script_sweeps_a_retired_label`.

 **The lesson stands for every rename from
the 2026-10 baseline on:** a tier whose value is a primary-key component of persisted state
(keychain service, app-group container id, database name) owes a read-forward path from the
moment any build wrote under the old spelling — from then on that is a major bump with a
transition window, never a sweep (`../version-compatibility.md`). `test_apple_identifier_pins.py`
keeps a drift guard against any new `com.fauna.*` use on an apple surface.

**One owner per tier, and a pin test for the copies that cannot share one.** The Rust home is
`fauna_core::platform_ids`, the Swift home `FaunaKit`'s `AppleIdentifiers`. Five entitlements
plists (macOS app, iOS app, both File Provider appex flavors, the sync agent), the appex's
`NSExtensionFileProviderDocumentGroup`, the iOS
`BGTaskSchedulerPermittedIdentifiers` array, and `Fauna-NSE` (a deliberately dependency-free
target) must repeat the literals because a plist cannot reference a constant —
`tests/e2e-unified/tests/test_apple_identifier_pins.py` is what keeps every copy equal, and a
new site is added there.

**The macOS app-group Team-ID prefix — DECIDED 2026-08-23 by the first-signed-build
measurement matrix.** Since macOS 15, `~/Library/Group Containers` is TCC-protected
(`kTCCServiceSystemPolicyAppData`): an app reaches an app-group container **without a user
prompt** only when it is deployed through the Mac App Store, **the group identifier is
prefixed with the app's Team ID**, or an **embedded provisioning profile** authorizes the
group (an app *extension* failing the conditions is silently **denied**, never prompted).
The matrix ran supervised on macOS (runbook `installer/macos/measure-first-run-tcc.sh`;
`--sign-only` build, § Build Pipeline; TCC reset between arms). **Ruling: the macOS app
group is `7457N3M72H.group.social.fauna.shared`; iOS keeps the base
`group.social.fauna.shared` verbatim** (iOS never takes the prefix). The fork is per-OS at
the two identifier homes (`fauna_core::platform_ids::APPLE_MACOS_APP_GROUP` /
`APPLE_APP_GROUP`; `AppleIdentifiers.appGroup` under `#if os(macOS)`), the entitlements
plists per flavor, and the File Provider appex's per-flavor `Info.plist` /
`Info-iOS.plist` twins — all pinned by `test_apple_identifier_pins.py`. ⚠ **The prefix is
the TEAM ID `7457N3M72H`, never the enrollment ID** — take it from
`security find-identity -v` (`Fauna Social (7457N3M72H)`), never from enrollment
correspondence (the two look alike and a draft of this passage confused them).

**Measured record (2026-08-23, all supervised; load-bearing for anyone re-measuring):**

1. **Arm (a) — signed, unprefixed group: PROMPTED**, both subjects (the bare agent as a
   path subject with `attributed bundle: (null)`; the app with full bundle attribution,
   subject `social.fauna.fauna`). A Developer ID signature alone authorizes nothing; the
   model is confirmed.
2. **Arm (b) — signed, prefixed group: the APP is fully silent** — access *and* first-ever
   container **creation** (which goes through `containermanagerd` and writes its metadata
   plist; the container is `drwx------` with a `Library/` skeleton).
3. **The AGENT is NOT covered by the prefix — bare or bundled.** A background executable
   prompts for access to the **registered** container even when Developer-ID-signed, group
   entitled, and (when exec'd from `Fauna.app/Contents/MacOS/`) bundle-attributed with the
   app's own subject `social.fauna.fauna`. The authorization is evaluated per-process for
   the app itself and **leaves no TCC grant behind**: an agent started immediately after a
   silent app run, same subject, still prompts. Bisections ruled out the legacy
   `~/Library/Application Support/Fauna` dir and app-first ordering as confounds.
4. ⚠ **False-silent trap for re-measurers:** directories hand-`mkdir`'d under
   `~/Library/Group Containers/<id>/` (as a granted agent run leaves behind) are **not**
   containermanagerd-registered, and access to them is NOT TCC-enforced — an agent can run
   "silent" against such bare dirs and fake a pass. Only a registered container (created by
   the app) is the real condition.
5. **⚠ RESOLVED 2026-08-25 — no TCC decision for this subject EVER binds the next agent
   instance, in either polarity.** The pre-registered discriminator this item used to pose
   (a LOGIN-spawned agent) ran supervised on macOS: **it prompts.** Sequence, from one
   `log stream` capture — launchd spawns the agent at login (`RunAtLoad`, ppid 1, 17:47:56;
   no launchctl and no tool anywhere in the path, so the tool-spawn and caller-identity
   confounds are both excluded); `AUTHREQ_PROMPTING` fires at 17:48:01.888 for
   `kTCCServiceSystemPolicyAppData`, subject `social.fauna.fauna`, responsible
   `fauna-sync-agent` at the bundled path, preceded one line earlier by
   `Session scoped auth is invalid for client`. The user then clicked **Don't Allow**, which
   registered as a real user decision (`AUTHREQ_RESULT authValue=0 authReason=2
   desired_auth=2`) and wrote a durable row (`TCCDEvent: type=Create,
   identifier_type=Bundle ID, identifier=social.fauna.fauna`) — and **54 ms later the agent
   was dead, respawned by launchd, and 84 ms after that row was written the NEW instance was
   prompted for the same service again.** So the 2026-08-24 reading — "a user Allow does not
   stick" — was too narrow: **deny does not stick either.** The row is created and then
   simply does not match the next process, which means **no number of user clicks can ever
   terminate the prompting.** Consequence, pre-registered here and now triggered:
   per-instance prompting is real and the agent's data root must leave the container — so
   option 1 (accept ONE well-worded prompt) is not merely unattractive but **unreachable on
   this shape**. The design fork that follows is owned by
   [`../apps/sync-agent.md`](../apps/sync-agent.md) § Implementation status today → A4
   item 2. The bundled exec's other property stands regardless: its prompt is attributed to
   "Fauna" on the shared subject, not a scary path.
6. **RESOLVED 2026-08-25 — the agent's state left the container, so the agent has no
   TCC prompt on either channel.** The residual agent first-start UX fork that item 5
   narrowed to *move the agent's state out of the container* against *status quo* was
   decided for the move (design pass; the per-domain form of the state-unification
   law is owned by [`../../behavior/on-demand-files.md`](../../behavior/on-demand-files.md)
   § Apple File Provider binding, *state unification*; the agent's own record is
   [`../apps/sync-agent.md`](../apps/sync-agent.md) § Implementation status today → A4
   item 2, which also holds the **deny-crash bug** the matrix found and its headless
   witness). The agent's data root, socket, logs and pin store are all under
   `~/Library/Application Support/Fauna` (§ File Layout), which `kTCCServiceSystemPolicyAppData`
   does not cover (item 3 bisected it out as a confound). The `.pkg`/`.dmg` TCC
   divergence therefore dissolves outright rather than "only by exec'ing the bundled
   copy"; the bundled-exec preference stays as landed, since bundle attribution is still
   the right subject for anything the agent is ever attributed to. **The agent's
   `com.apple.security.application-groups` entitlement is DROPPED (2026-08-25, the same
   design pass's second half):** `installer/macos/fauna-sync-agent.entitlements` names no
   group, `test_apple_identifier_pins.py` pins the absence, and the headless witness is
   `bins/fauna-sync-agent/tests/agent_sigterm.rs::the_agent_binary_boots_in_the_user_domain_and_never_creates_the_container`
   — the real binary on its production resolution path (no `--data-dir`, launchd-like
   `HOME`) serves from the user domain and creates nothing under `Library/Group
   Containers`. That witness, not a human watching for a prompt, is what licensed the
   drop: a dead claim was the exact shape that sent three sessions hunting a prompt the
   claim itself invited. ⚠ The first signed build after the drop still shipped the
   **bundled** agent copy with the app's entitlements — `codesign --deep --entitlements`
   re-stamps nested executables with the outer bundle's claims (§ Build Pipeline step 4;
   the `/usr/local/bin` copy was clean, the copy the LaunchAgent prefers was not) — so the
   signing pipeline now signs inside-out (`sign-app-bundle.sh`) and the artifact pin
   `TestDryRun::test_bundled_agent_carries_its_own_entitlements` witnesses both sets on the
   built bundle. The app-side half landed with it (`SyncStateDir.Domain` — the
   two roots in Swift; `SyncStatesStore`'s two-root Media-badge fold; the app's own
   `config-replica` in the user domain). **What remains is the
   supervised zero-prompt verify** of a fresh `--sign-only` `.pkg`: install → reboot →
   no Fauna prompt of any kind, the agent serving from the user domain, sign in → the
   Finder location appears (the extension authenticating off its container pin replica)
   — exemption 1's last inch; every path in it is asserted headlessly
   (`test_sync_agent_resolves_reachable_production_paths`, the witness above).
7. **The usage-description key for `kTCCServiceSystemPolicyAppData` is
   `NSAppDataUsageDescription`** — recovered 2026-08-25 by scanning `tccd`'s own string
   table, where it sits alongside `NSDesktopFolderUsageDescription`,
   `NSNetworkVolumesUsageDescription` and the rest of that family; the key therefore does
   **not** need to be found by guess-and-rebuild rounds, and one supervised round is enough
   to confirm it reaches the prompt. Measured the same evening: the login prompt logged
   `found general usage key: (null)` / `usage description: (null)`, and neither candidate
   placement carries it — `Fauna.app/Contents/Info.plist` has only
   `NSPhotoLibraryUsageDescription`, and the agent binary has **no `__TEXT,__info_plist`
   section at all** (it is a bare Mach-O under `Contents/MacOS/`, not a bundle). So there are
   two homes to try in order: the subject's bundle plist (a one-line edit), and failing that
   a linker-embedded plist on the responsible executable (`-sectcreate __TEXT __info_plist`).
   ⚠ Deliberately NOT landed yet — item 5's resolution may delete this prompt outright, and
   a usage string for a prompt that should not exist is wasted wording.

**⚠ The macOS data-protection KEYCHAIN plane is blocked on PROVISIONING-PROFILE
infrastructure, not a one-line entitlement — MEASURED 2026-08-28, and it REFUTES the
premise that "an app group is the one entitlement class Developer ID honours without a
provisioning profile."** That premise (`fauna_core::platform_ids`
`APPLE_MACOS_ACCOUNT_KEYCHAIN_GROUP` doc; [`../apps/ios.md`](../apps/ios.md)
§ Credential Storage's *keychain plane* bullet, which pre-registered exactly this as *"a
`legacy` reading there means the group is not an access group under Developer ID after
all — the fallback then IS the behaviour, silently"*) is the one this section's app-group
matrix (record item 6) leaned on to put `KeychainStore`'s account rows on the
data-protection plane under an app-only access group. It does not hold. Three converging
measurements on macOS (a **Developer-ID-signed** artifact and a controlled probe under the
same `Developer ID Application: Fauna Social (7457N3M72H)` cert — the `.dmg`/`--sign-only`
signature class, no MAS, no embedded provisioning profile):

1. **The installed signed `Fauna.app` runs on the LEGACY plane** — its own persisted log
   (`~/Library/Application Support/logs/fauna.log.2026-08-27`) reads
   `[keychain] write plane: legacy (probe status -34018)`, and `secd`, on the *real
   running app*, logs every ~minute: *"Entitlement
   com.apple.security.application-groups=("7457N3M72H.group.social.fauna.shared",
   "7457N3M72H.group.social.fauna.account") is ignored because of invalid application
   signature or incorrect provisioning profile."* The app carries both groups, is
   Developer-ID-signed, is unsandboxed, and embeds **no** provisioning profile
   (`Fauna.app/Contents/` has none) — so its group entitlements are silently ignored as
   data-protection-keychain access groups, and `probeWritePlane()` correctly returns
   `.legacy`.
2. **`com.apple.security.application-groups` is IGNORED as a DP-keychain access group
   under Developer ID without a profile** (the probe: Developer-ID-signed bare Mach-O with
   a team-prefixed app group → `-34018` on every DP-keychain query, `secd` naming the same
   "ignored … incorrect provisioning profile" cause). It survives launch, but reaches
   nothing.
3. **`keychain-access-groups` is a RESTRICTED entitlement — it gets the process KILLED at
   launch without an embedded provisioning profile, ad-hoc AND Developer ID alike**
   (`taskgated-helper`: *"Disallowing … because no eligible provisioning profiles found"*;
   the kill is AMFI's restricted-entitlement check, present with and without the hardened
   runtime). `com.apple.application-identifier` + `com.apple.developer.team-identifier`
   together are killed the same way. So the naive "just add `keychain-access-groups` to the
   app" fix is **worse than a no-op**: it would break every ad-hoc dev/e2e build (`mac-app`
   is ad-hoc) *and* the current Developer-ID pipeline (no profiles), and still would not
   reach the plane without the profile.

**Consequences.** (a) The DP-keychain plane on macOS (for the app's `KeychainStore`
account rows **and** the File Provider appex's shared-group credential rendezvous) requires
an **embedded Developer-ID provisioning profile** authorizing the access group —
registering the App ID + group in the Apple Developer portal, generating a profile, and
wiring the `.pkg`/`.dmg` pipeline to embed it in the app *and* the appex. That is the crux
this leg cannot answer on-box — portal work, and whether a Developer-ID app-group can be a
DP-keychain access group *at all* even with a profile is unconfirmed. (b) The appex's "signing-gated, pending
the org's Apple Developer cert" framing
([`../../behavior/on-demand-files.md`](../../behavior/on-demand-files.md) § Apple File
Provider binding → *Headless-first testing*) is **incomplete**: the cert (Developer ID)
alone is insufficient; the same profile is required. (c) The legacy plane is therefore the
**shipping** behaviour today, and — measured severity — that is one sticky Always-Allow at
first agent read on a fresh box, not a recurring tax
([`../apps/sync-agent.md`](../apps/sync-agent.md) § Packaging + lifecycle owns the
fresh-install prompt measurement). The emergency the row was opened for is gone; closing
the residual prompt is the provisioning-profile investment above, to be weighed, not an
emergency.

**Path A RATIFIED 2026-08-28 (user directive): build the provisioning-profile
infrastructure — and it covers the two BUNDLE stores only; the shared Rust credential slot
stays on the legacy plane by necessity.** The decision was A over B (accept legacy forever).
Feasibility is confirmed from Apple's own docs — TN3137 *On Mac keychains* (*"macOS builds
the list of data protection keychain access groups available to your program from its code
signing entitlements … These entitlements must be authorized by a provisioning profile.
Your program needs an app-like bundle structure in which to embed that profile … the data
protection keychain is only available to programs running in a user context, like an app or
an app extension"*) and *Sharing access to keychain items* (*"You can use app group names as
keychain access group names"*; the portal *"guards against the reuse of app group names
across teams when you try to add an app group to a provisioning profile"* — i.e. an app
group can be added to a profile and used as a DP-keychain access group). ⚠ **There is no
entitlement change to make** — the app already declares
`com.apple.security.application-groups` = [`…shared`, `…account`]; what is missing is the
**embedded provisioning profile that authorizes them**. The scope, split by the TWO
distinct keychain stores on macOS:

- **Store #1 — the app's identity store, Swift `KeychainStore` (service `social.fauna.account`).**
  Reaches the data-protection plane once the app bundle embeds a Developer-ID provisioning
  profile authorizing the app-only access group `7457N3M72H.group.social.fauna.account`.
  Touched by the app alone (Swift). This is the store behind the upgrade re-prompts
  ([`../apps/ios.md`](../apps/ios.md) § Credential Storage owns its plane mechanism). **A
  fixes it.**
- **Store #1b — the File Provider appex rendezvous (shared group `7457N3M72H.group.social.fauna.shared`).**
  The app provisions the app-dead capability into the shared group's data-protection
  keychain and the sandboxed appex reads it back; this is the app→appex credential
  rendezvous the whole on-demand surface depends on, currently *signing-gated* precisely
  because the group is not honoured as a DP access group without a profile
  ([`../../behavior/on-demand-files.md`](../../behavior/on-demand-files.md) § Apple File
  Provider binding). Both the app AND the appex embed a profile authorizing the **shared**
  group. **A unblocks it** — the highest-value payoff, since a fresh app install of Store #1
  does not prompt for its own rows anyway (legacy plane authorizes the creating app), so A's
  Store #1 benefit is really the upgrade path, while the FP feature is genuinely blocked.
- **Store #2 — the shared Rust credential slot, `fauna-credential-store::mac_keychain`
  (service `fauna-account-store`).** Holds the T10 store writer key, device-authorization,
  backup key. Read by the **app (via FFI), the sync agent, AND fauna-tui** — one slot, three
  processes. **A does NOT and CANNOT move this to the DP plane**, for two independent
  reasons: fauna-tui ships **ad-hoc / unsigned** (no profile → `-34018` on the DP keychain),
  and the agent is a **bare `/usr/local/bin` launchd-agent binary** (not a bundle — the DP
  keychain needs an app/appex-like bundle carrying a profile; TN3137's daemon-wrapping trick
  is the only path, disproportionate here). So Store #2 stays legacy, and **the agent's
  single sticky Always-Allow at first read on a fresh box is ACCEPTED** (this is "B" for
  Store #2 specifically) — one prompt per machine, stable under the Developer-ID DR. The row
  was opened as "the agent's keychain story"; the honest answer is that the agent's prompt is
  the one keychain prompt A leaves standing, and it is the acceptable one.

**Mechanism (no code behaviour change; a build-pipeline + portal change only).** Register
the App ID `7457N3M72H.social.fauna.fauna` and the appex App ID
`7457N3M72H.social.fauna.fauna.FileProvider` in the Apple Developer portal with the App
Groups capability enabling `…shared` (+ `…account` on the app), generate a **Developer ID**
provisioning profile for each, and embed them as
`Fauna.app/Contents/embedded.provisionprofile` and
`…/PlugIns/Fauna-FileProvider.appex/Contents/embedded.provisionprofile` in the signing
pipeline (`installer/macos/build.sh` + `sign-app-bundle.sh`, and the public repo's
`release-macos.yml` sign job — the real macOS app builds from the extracted public `fauna`
repo, so the source change lands here and extracts). The ad-hoc `mac-app`/e2e path embeds no
profile and stays exactly as today (Store #1 falls back to legacy, unchanged). The exact
portal runbook (the user's off-machine half) and the pipeline implementation slice are
captured at; the DP-plane "no user-visible dialog"
pin the row's success criterion names cannot be exercised until a real profile exists, so
it rides the first signed build after the portal work — this is a sequential portal → profile
→ pipeline → verify chain, not a headless test.

**Rejected alternative — a DP-plane keychain-broker daemon (considered 2026-08-28, do not
re-propose without new weight on the other side).** The natural "make it one store" idea is a
single always-running per-user process, itself a signed bundle with an embedded profile, that
holds the DP-keychain access group and does all keychain I/O; the app, sync agent, and tui
call it over IPC and never touch the keychain themselves. It **works** and would remove every
prompt — and its appeal is real: it routes *around* the two blockers above (tui's ad-hoc
signature and the agent's bare-binary form stop mattering if only the broker needs the
entitlement). It is **set aside** because the cost is disproportionate to the one sticky
prompt it buys, and it is a security *regression*, not merely more code:

- **It is a central seed-dispensing confused deputy.** Its socket is filesystem-permissioned
  to the user, so any process running as that user can connect; to be safe it must verify each
  caller's code signature (audit token → `SecCode` → designated-requirement check) **and**
  enforce per-caller scoping — the agent gets only `BackupKey` + bearer, the app gets the
  seed, all else refused. That is **reimplementing, in our own security-critical code, the
  exact audience tiering the OS already enforces for free via access groups**, and it inverts
  today's least-privilege boundary, where the identity seed lives in an app-only access group
  no other process can name ([`../apps/ios.md`](../apps/ios.md) § Credential Storage;
  `key-material-hierarchy.md`). A broker would hold the seed and hand it out.
- **macOS access groups already ARE this broker, done by the OS** — a shared,
  membership-gated store, no prompts, per-group scoping enforced by the kernel. The File
  Provider appex already uses it exactly so (it gets `BackupKey` + bearer through the shared
  group, never the seed). A custom broker re-creates that minus the OS's help; the only reason
  it can't already serve tui + the agent is their ad-hoc/bare form — i.e. the broker exists to
  paper over that, which is "build our own `securityd`."
- **It is path-A-*plus*, and converges with the agent.** The broker still needs a bundle +
  embedded profile to reach the DP keychain (this whole section's portal work), then adds a
  new always-running process, an IPC protocol, and the peer-verification machinery on top. And
  "always-running per-user process" already *is* the sync agent — so you would really be
  merging the two, at which point the merged daemon holds the seed and dispenses it, the same
  custody inversion.

**Lighter middle ground, noted for whoever revisits:** the app already brokers to the agent
over `fauna_ipc` (it pushes `BackupKey` + bearer, never the seed); the agent's remaining prompt
is its *preference* to read the shared writer-key slot from the keychain directly (so one
machine renews as one principal — [`../apps/sync-agent.md`](../apps/sync-agent.md) § Credential
model, W5 (account-data-plane.md § Workstreams).5b). Leaning on the already-pushed key instead of that keychain read is a far smaller,
keychain-free lever than a broker — but it re-enters the "agent must renew app-dead" constraint
that shaped the current design (the neighbourhood of the set-aside option (b)). The broker
becomes worth revisiting only if the goal grows past the one prompt — e.g. a genuinely uniform
credential authority wanted for other reasons.

## launchd jobs — machine daemons (server) + per-user agent (sync)

**Target (2026-06-24 re-shape).** The **server** services run as **machine-level `LaunchDaemon`s**
(`/Library/LaunchDaemons/`, boot-started, headless, no login required), each under a dedicated hidden
service user the `.pkg` postinstall creates via `dscl`:

- `social.fauna.nest` → runs `fauna-nest-daemon` as **`_fauna`**
- `social.fauna.bridge` → runs `fauna-bridge-supervisor` (→ Go MDA child) as **`_fauna-bridge`**

The **sync** agent stays a **per-user `LaunchAgent`** (`~/Library/LaunchAgents/social.fauna.sync-agent.plist`,
`gui/$UID`) — it syncs the logged-in user's files into the user's folders, so it is inherently
per-user (the same split as Windows: machine services for nest/bridge, per-user agent for sync).

> **Re-point LANDED 2026-07-19 (ratified 2026-07-18):** the sync component's LaunchAgent is
> **`social.fauna.sync-agent.plist`** running the cross-platform **`fauna-sync-agent`** (`RunAtLoad` +
> `KeepAlive`, **enabled** — an unprovisioned agent idles harmlessly until the app's first post-auth
> provisioning; the legacy `fauna-sync install` refused while the agent plist existed, so nothing could
> double-sync, until that daemon was removed 2026-10-02). A `.dmg`-only install
> self-installs the LaunchAgent at first post-auth (user-writable, no elevation) from the agent binary
> `just mac-app` bundles at `Fauna.app/Contents/MacOS/fauna-sync-agent`.
> Owner: `../apps/sync-agent.md` § Packaging (milestone A4).

Common properties:
- **Default state — installer-time opt-in, no in-app toggle, no runtime elevation (DECIDED 2026-06-26,
  match Windows).** The desktop **app**, **sync** and **terminal app** `.pkg` components default **selected** (the common
  macOS case is "just the app": app + sync, connecting to a remote nest — mirroring the Windows
  `DesktopApp`/`Sync` features, default ON); the nest/bridge `.pkg` components default **unselected**
  (matching Windows "Nest Service default OFF, the desktop is primarily a client" — `windows.md` §
  Feature Tree). The app is a straight bundle install to `/Applications` (no daemon); the rest of this
  section governs the server **daemons**. **Selecting a server component is the opt-in** — made while the
  `.pkg` installer already holds admin rights — and its
  postinstall installs the daemon **enabled and auto-started** (`RunAtLoad`/`KeepAlive`, *not*
  `Disabled`), exactly as the Windows MSI auto-starts a service whose feature is ticked; an unselected
  component installs nothing. There is therefore **no** post-install "run a nest on this Mac" in-app
  toggle and **no** runtime privilege escalation (no privileged helper / `SMAppService` / admin prompt):
  a per-user app can't enable a system daemon, and it doesn't need to — the already-elevated installer
  did it. The app's only role toward a self-hosted nest is the normal client flow (claim it, configure
  it via admin RPC over the loopback), identical to the Windows desktop app. (Toward the per-user *sync
  agent* — user-scope, no elevation — the app does tend lifecycle: `../apps/sync-agent.md`
  § Packaging. The `SyncDaemonManager` that once did so was deleted 2026-07-13.) Rationale: a per-user
  runtime toggle would force a net-new privileged-helper security surface for no gain and diverge from
  Windows, which has no such toggle (priority #1/#3).
- **Auto-restart:** `KeepAlive: true` — launchd restarts a crashed job automatically.
- **Privileged port:** the nest daemon binds `:443` via launchd **socket activation** (root pre-binds
  the port, `_fauna` inherits the fd). **Wired daemon-side (S1, 2026-06-25):** `fauna-nest-daemon`
  inherits the fd via `launch_activate_socket("FaunaNest")` and serves it through
  `fauna_nest::start_server`'s pre-bound-listener seam; the matching plist `Sockets` dict (`FaunaNest`
  → `SockServiceName 443`) ships with the `.pkg` packaging (S3).
- **Log visibility:** Console.app under the respective `social.fauna.*` identifier.

**This shape ships in the current `.pkg`** (packaging landed 2026-06-25/26; unsigned full-install
proven 2026-06-28 — § Implementation status today). A pre-2026-06-25 `.pkg` installed all three as
per-user `LaunchAgent`s (nest on `:3000`); no such install exists, so none is migrated (§ File Layout).

### `social.fauna.bridge` — supervisor + two-level MDA gating

The `social.fauna.bridge` job runs **`fauna-bridge-supervisor`** (the macOS MDA supervisor), **not** the Go MDA directly. This mirrors the Windows `fauna-bridge-service` (SCM) shell; the cross-OS reconcile logic is the shared `libs/fauna-mda-supervisor` crate (decision D-E1). Enablement is **two-level**, identical in concept to Windows and Linux (priority #1/#3):

> **Target note (2026-06-24 re-shape, app-side landed 2026-06-25):** as a machine `LaunchDaemon`, the
> bridge is **boot-loaded and headless** and self-gates entirely on the nest's flag files (level 2 below)
> — it no longer depends on the per-user app's `SyncDaemonManager` to `launchctl bootstrap` it (a per-user
> app cannot manage a system daemon, and a headless Mac-mini server runs no app). **The macOS app's
> per-user `services.json` watcher + `social.fauna.bridge` bootstrap was removed 2026-06-25**
> (it was dead code, never wired into app launch) — on macOS level-1 is now the always-loaded system
> daemon's own boot-load (the supervisor self-gates on the nest flags), **not** an app action. The level-1
> description below is the **historical per-user behavior** (still live on Linux, whose app keeps its
> `service_watcher`); the macOS daemon folds level-1 into the always-loaded daemon's own flag-watch.
> Migration: (tracked internally).

1. **Agent load** — gated on `services.json {bridge}`. The **nest writes `services.json`** (it is nest-managed IPC state derived from an app's bridge-enable choice, never a hand-edited file — `bins/fauna-nest/src/services.rs`). On **macOS** (re-shaped 2026-06-24/25) there is no app-side level-1 watch: the bridge `LaunchDaemon` is boot-loaded and the supervisor self-gates on the nest flags (the per-user `SyncDaemonManager` watch was removed). On **Linux** the app's `service_watcher` still bootstraps/boots out the `social.fauna.bridge` agent on `services.json {bridge}`; on **Windows** the always-on `fauna-nest-service` does this watch. (macOS thus converges *up* to the Windows always-loaded-supervisor shape; Linux remains per-user app-driven.)
2. **MDA child** — once the supervisor is loaded, it **self-gates** the Go `fauna-mail-bridge` MDA child on the nest's `caldav-enabled`/`imap-enabled` flag files: spawns it when either is on, stops it when both are off, restarts it on unexpected exit, and re-pins its CalDAV listener when the admin changes `caldav-port`. This loop runs in the launchd-`KeepAlive`'d supervisor process — **fully headless; it never requires the Fauna app to be running**. The admin's CalDAV/IMAP on/off choice is therefore expressed **exclusively through an app** (which flips the nest flags), upholding the iron-clad single-configuration-surface invariant. On macOS the app has **no** bridge-lifecycle role at all (the boot-loaded system daemon self-gates); on Linux the app's sole role is the one-time level-1 agent bootstrap.

## Network-reachable nest

The installed nest is a **real network-reachable server**, not a loopback-only personal nest — like the Windows installer's network-reachable nest (`installers/windows.md` § Network-reachable nest, RATIFIED 2026-06-20) and the standalone `fauna-nest` default. Under the machine-service `LaunchDaemon` shape it **binds the standard `:443` on all interfaces** (`0.0.0.0:443`) via launchd **socket activation** (root launchd binds the privileged port and hands the listening fd to the `_fauna` daemon), exactly matching Windows; the co-located MDA + same-box app reach it over the **fixed internal loopback `127.0.0.1:3000`** (`FAUNA_INTERNAL_LOOPBACK_PORT` — `nest/common.md` § Same-box reach), and the admin-choosable `serving_port` rides on top. It serves the nest's always-live self-signed *floor* cert (`fauna_nest::self_signed_cert::ensure_floor_present`); Fauna apps key-bind whatever cert is served, and a third-party MUA TOFU-accepts it.

**Port — `:443`.** The machine-daemon shape **removed** the constraint that pinned macOS to `:3000`: a per-user `LaunchAgent` ran as a non-root user and so could not bind the privileged `:443` (and the desktop box has no SNI router to front it, unlike Docker/VPS); `3000` remains the canonical `config/default.toml` `listen` seed for a non-launchd run. A `LaunchDaemon` under `_fauna` binds `:443` via socket activation, so the installed nest **serves the standard `:443` like Windows** — clients reach the box by handle (`test@<lan-ip>`) with no port suffix. The current `.pkg` ships the `Sockets` dict; the **live** `:443` serving proof (fd inheritance on a real install) is the pending signed-install item (§ Implementation status today). A legacy pre-2026-06-25 install serves `:3000` until migrated. (The internal-loopback IPC port stays `3000` either way, so it never collides with the MDA's CalDAV `:8443`.)

This upholds priority #1 (same network posture on every desktop installer) and the user-challenged invariant **"a nest listens on all interfaces, no distinction by client origin"** — a loopback-only macOS nest would be unreachable from another LAN device by `test@<lan-ip>` while the Windows nest is a real server, an unjustified per-app divergence. (The pre-2026-06-23 `common.sh` rewrote `listen` to `127.0.0.1:3000` with a `# Bind to localhost for desktop` comment reflecting an older single-machine assumption the desktop-native-nest direction supersedes; that rewrite was removed.)

### Upgrade Behavior

Postinstall scripts detect whether a plist already exists before writing:

- **Existing plist found:** the script skips the write + re-bootstrap and leaves the installed plist in place — an idempotence guard, *not* a customization affordance (plists are postinstall-generated bucket-2 IPC; no human edits them — `principles.md` § One configuration surface). Consequence worth knowing: a plist-**shape** change in a newer installer (e.g. a future `Sockets` edit) does **not** propagate to an upgraded install — there is no versioned-plist mechanism today (`test_upgrade_preserves_plists` pins the preserve behavior).
- **Service running at upgrade:** the script runs `launchctl kickstart` to restart the service with the new binary

## Platform Support

| Architecture | Supported |
|---|---|
| Apple Silicon (ARM64) | Yes |
| Intel (x86_64) | No |

Intel support is not planned. All macOS targets are aarch64-apple-darwin.

## Build Pipeline

The all-in-one `.pkg` is assembled in stages:

1. **Compile binaries + build the app** — cross-compile `fauna-nest-daemon`, `fauna-sync-agent`, `fauna-bridge-supervisor`, `fauna-tui` (Rust) for `aarch64-apple-darwin`, build the Go `fauna-mail-bridge` (cgo, linking `libfauna_ffi.dylib`, via `just mail-bridge-build`), and build the desktop app bundle `build/Release/Fauna.app` via `just mac-app release` (`xcodebuild -scheme Fauna` over the thin `.xcodeproj` — the only route that produces the app WITH its `Contents/PlugIns/*.appex`; the recipe stages the rendered `AppIcon.icns` and the bundled `fauna-sync-agent` on top — § Implementation status today → App extensions)
2. **Stage payloads** — copy binaries, the `libfauna_ffi.dylib`, plists, and scripts into per-component staging directories; stage the whole `Fauna.app` into the app component's payload root
3. **Relocate the MDA dylib linkage** — `just mail-bridge-build` links `fauna-mail-bridge` against the dylib by its absolute build path; `install_name_tool` rewrites the dylib's install-name to `@rpath/libfauna_ffi.dylib` and the binary's rpath to `@loader_path` so the installed MDA finds the dylib staged beside it in `/usr/local/bin/` (the macOS twin of Windows resolving `fauna_ffi.dll` from the same dir)
4. **Code-sign binaries + the app** — sign each binary + the dylib with the Developer ID Application certificate; sign `Fauna.app` **inside-out** with the hardened runtime + entitlements (`installer/macos/sign-app-bundle.sh`, shared by `mac-app`'s ad-hoc signature, this `--sign` path and `mac-dmg`): the bundled `fauna-sync-agent` first with ITS OWN entitlements file, then **each `Contents/PlugIns/*.appex` with ITS OWN** (resolved by bundle name from `apps/fauna-apple/<Name>/<Name>.entitlements`, so a new extension is signed correctly by existing, and an extension without one is a hard error rather than a silent unsandboxed ship), then the app with `Fauna-macOS.entitlements` and **never `--deep`**. The extensions are where this matters most: the File Provider appex is sandboxed and names ONE app group (its replica container) while the app names two, and the second exists precisely so a sandboxed extension can never reach the identity seed (§ Identifier domain). Measured 2026-08-25: `codesign --deep --entitlements` re-stamps every nested Mach-O with the outer bundle's entitlements, so the bundled agent — the copy the sync postinstall's LaunchAgent prefers — shipped carrying the app's app-group claims while the `/usr/local/bin` copy carried none; `test_installer.py::TestDryRun::test_bundled_agent_carries_its_own_entitlements` pins the built artifact's two entitlement sets. (A notarizable `.pkg` requires its inner app to be Developer-ID-signed with hardened runtime, replacing `mac-app`'s ad-hoc signature.) The bridge artifacts are signed **after** the `install_name_tool` relocation, which invalidates any prior signature
5. **`pkgbuild`** — produce one `.pkg` per component (`fauna-app.pkg`, `fauna-nest.pkg`, `fauna-sync.pkg`, `fauna-bridge.pkg`). The app component uses `--install-location /Applications` with a component plist pinning `BundleIsRelocatable=false`
6. **`productbuild`** — combine component packages into the final metapackage with a Distribution XML (four selectable choices)
7. **`xcrun notarytool`** — submit the metapackage to Apple's notarization service and wait for approval
8. **Staple** — run `xcrun stapler staple` to attach the notarization ticket to the `.pkg`

Three modes (`installer/macos/build.sh`): unsigned (default, stages 1–6 with no
codesign — dev/testing, `just pkg-unsigned`), **`--sign-only`** (stages 1–6
Developer-ID-signed, no notarization — `just pkg-sign-only`; enough for a local
install and the § Identifier domain measurement matrix, since a locally-built
`.pkg` carries no quarantine xattr and never meets Gatekeeper), and `--sign`
(all stages, the release shape, `just pkg`).

### Size & build profile

The `dist` cargo profile (`installers/windows.md` § Size & build profile owns
its declaration and the android leg's numbers — `strip = true`, `lto = "thin"`,
`opt-level = "s"`, `codegen-units = 1`) is threaded through the apple FFI
recipes: `apple-ffi`/`apple-ffi-watch`/`apple-ffi-test`/`apple-ffi-store-safe`
(the multi-slice, device-linkable xcframework) and
`apple-ffi-host`/`apple-ffi-host-test`/`apple-ffi-host-store-safe` (the 1-slice
macOS-only twin `mac-app`/`mac-release`/`mac-dmg` take) all accept an optional
`profile` parameter, default `release` (dev-loop/CI speed, unchanged — a bare
`just mac-app release` is byte-for-byte what it always built). `mac-app`/
`mac-release`/`mac-dmg` forward it to `apple-ffi-host`; `dist` is refused under
`config=debug` (it is a shipping variant of a release build, not a third Xcode
configuration — Xcode has no "Dist" scheme, so `_apple-ffi-host-flavor`
decouples the cargo PROFILE axis from Xcode's CONFIG axis rather than adding
one). The cross-checkout prebuilt-FFI `CACHE_KEY` and the shared `.ffi-flavor`
marker both fold in the profile, so a `release`↔`dist` switch is never served
the other flavor's stale xcframework.

**⚠ Apple does NOT need android's `strip=false` bindgen workaround — measured
empirically 2026-09-11, macOS, NOT assumed.** Android's `[profile.dist]` build
made uniffi-bindgen's Kotlin generation silently emit ZERO files against its
stripped `.so` (`installers/android.md` § Implementation status today). A
controlled A/B on this machine (`cargo build -p fauna-ffi --profile dist`,
default `strip=true`, vs the same build with `--config
profile.dist.strip=false`) found **no such trap on apple's targets**:
uniffi-bindgen's Swift generation against the `dist`-profile `libfauna_ffi.a`
produced the identical 78-file output byte-for-byte-equivalent in file list to
the unstripped control, and — testing the mac shipping path's FFI-HOST build
(the app's default-feature flavor, `apple-ffi-host`'s own cdylib byproduct,
*not* the mail bridge's labeler-flavored one — that narrower test is below) —
`uniffi-bindgen-go` against the same host build's stripped `.dylib` (7,916
symbols vs 215,731 unstripped by `nm -a`; 64 MB vs 96 MB, confirming `strip`
DID run) also produced the same 52-file output. The likely reason: apple's
bindgen reads a staticlib (`.a`, an object-file archive with no final link
step) rather than a linked binary, and even for the cdylib case macOS's strip
apparently does not remove whatever metadata uniffi-bindgen reads — the
mechanism was not root-caused further (would mean reading uniffi's own
vendored dependency source, outside this project's policy for untrusted
third-party code). **So no `--config profile.$PROFILE.strip=false` override
exists anywhere in the apple recipes** — cargo's default `dist` behavior is
safe as-is.

**The mail bridge's ACTUAL labeler-flavored cdylib re-verified against the
same claim, standalone, 2026-09-11:** the above
FFI-host test was a stand-in — same crate, same profile mechanics, a
*different* feature set (`--no-default-features --features labeler` is what
`mail-bridge-ffi` actually builds). A dedicated A/B — `cargo build -p
fauna-ffi --profile dist --no-default-features --features labeler` (default
`strip=true`) vs the identical build with `--config
profile.dist.strip=false`, both with
`CARGO_RESOLVER_FEATURE_UNIFICATION=selected` (per-invocation feature
resolution — the first attempt at this A/B, without that env var, pulled in
an extra `QrMatrix` type via workspace-wide unification and looked like a
strip-dependent interface difference; it was not) — confirms the same
no-trap finding for the exact artifact that ships: 3,778 symbols (stripped)
vs 120,655 (unstripped) by `nm -a`, 37.8 MB vs 55.7 MB (−32%), and
`uniffi-bindgen-go`'s 20-file Go binding output byte-for-byte identical
either way (`diff -rq`). No `strip=false` override needed here either.

**Measured impact** (`aarch64-apple-darwin`, macOS, 2026-09-11 — `just mac-app
release` vs `just mac-app release dist`, a real `xcodebuild`-assembled
`Fauna.app`, not just the FFI slice in isolation):

| artifact | release | dist | Δ |
|---|---|---|---|
| `libfauna_ffi.a` (host, unstripped archive either way) | ~924 MB | ~924 MB | ~0% (staticlib, not link-stripped) |
| `libfauna_ffi.dylib` (host build's cdylib output, unused by the app — see below) | 96.0 MB | 64.2 MB | −33% |
| `Fauna` (the app's main Mach-O, `Contents/MacOS/Fauna`) | 257.0 MB | 240.3 MB | −6.5% |
| **`Fauna.app` (whole bundle, `du -sh`)** | **538 MB** | **506 MB** | **−5.9%** |

**Why the whole-app win is far smaller than android's (−37% AAB) or windows'
(−21.5% MSI):** most of `Fauna.app`'s weight is Swift/SwiftUI (FaunaKit, the
app target, the update framework it then embedded, the two `.appex` bundles) plus the Swift
runtime — none of it touched by a CARGO profile. Only the Rust code the linker
pulls into `Contents/MacOS/Fauna` (statically, from `libfauna_ffi.a`) is
subject to `dist`'s LTO/opt-level/strip, and Xcode's own link-time dead-code
stripping + `STRIP_INSTALLED_PRODUCT` already remove a fair amount of what
`dist`'s `strip=true` would otherwise additionally cut — so the two overlap
rather than stack. Still a real, unconditional win with no observed
correctness cost: both builds completed, ad-hoc-signed, with the identical
`swift build`/xcodebuild steps — `dist` changes only the FFI slice inputs, not
the Swift/Xcode side of the pipeline.

**`installer/macos/build.sh`'s four service binaries + the mail bridge's Go/cdylib build — WIRED, 2026-09-11, same-shape follow-on to the FFI-recipe axis above.** `build.sh` takes a real `--profile <release|dist>` flag (default `release`, combinable with `--sign`/`--sign-only`), threaded into its own `cargo build --profile "$PROFILE"` line for `fauna-nest-daemon`/`fauna-sync`/`fauna-sync-agent`/`fauna-bridge-supervisor`; `just pkg`/`pkg-unsigned`/`pkg-sign-only` each gained a matching `profile="release"` parameter. `mail-bridge-ffi`/`mail-bridge-build` (justfile) gained the same `profile` parameter — cargo builds `--profile {{profile}}` instead of a hard-coded `--release`, and under `dist` the Go binary additionally links `-trimpath -ldflags '-s -w'` (windows parity, `_windows-go-cgo-env`'s identical switch). The flavor-private slot (formerly the bare `mail_ffi_slot` `:=` variable, which could not carry a per-invocation profile — a `:=` variable is evaluated once per `just` PROCESS, before any recipe parameter exists) is now the `mail-ffi-slot profile="release":` recipe, profile-keyed the same way `_apple-ffi-host-flavor` keys `$ARTIFACTS` by `$PROFILE_DIR`; every consumer (eleven-plus recipes, the e2e conftest, the installer) derives it by calling the recipe rather than re-deriving the mapping. `mail-bridge-ffi-check` (the merge-gate freshness/compile gate) deliberately stays hard-wired to `release` — a merge gate has no reason to pay the `dist` build cost.

**Measured impact — the two newly-wired classes** (`aarch64-apple-darwin`, macOS, 2026-09-11, `just pkg-sign-only` vs `just pkg-sign-only dist`, real signed builds via `installer/macos/build.sh`):

| artifact | release | dist | Δ |
|---|---|---|---|
| `fauna-bridge-supervisor` | 2.64 MB | 1.75 MB | −33.7% |
| `fauna-nest-daemon` | 77.56 MB | 48.18 MB | −37.9% |
| `fauna-sync` (legacy daemon, removed 2026-10-02; historical measurement) | 26.64 MB | 14.46 MB | −45.7% |
| `fauna-sync-agent` | 51.90 MB | 29.88 MB | −42.4% |
| `libfauna_ffi.dylib` (mail-bridge-ffi's own labeler-flavor slot — see the strip re-verification above) | 64.64 MB | 36.02 MB | −44.3% |
| `fauna-mail-bridge` (Go binary, `-trimpath -ldflags '-s -w'` under `dist`) | *not independently re-measured this session — windows' `-s -w` measured −52% on the equivalent binary (`installers/windows.md` § Size & build profile) is the cross-platform prior art, not a mac-specific number* | 14.78 MB | — |
| **`Fauna-0.1.2.pkg`** (the full signed all-in-one installer `just pkg-sign-only` produces) | *not independently re-measured this session* | **189.8 MB** | — |

Signature verified on the `dist` build: `pkgutil --check-signature` reports "signed by a developer certificate issued by Apple for distribution" with a trusted timestamp, chained through Developer ID Installer → Developer ID Certification Authority → Apple Root CA — the row's success criterion is a signed-and-verified artifact (`pkg-sign-only` + `pkgutil`), not a notarized one; notarization stays maintainer-run against a real Apple ID (`macos.md` § Required Environment Variables).

**Not wired, and deliberately so:** **the `.dmg` release pipeline and the CI `macos` job deliberately run the SAME recipe** (`just mac-app release` — `release-macos.yml`'s own comment: "The same recipe the CI `macos` job runs"), so the shipped `.dmg`'s `Fauna.app` is exactly what CI already proved buildable. Wiring `profile=dist` into that shared call would pay the (materially higher — LTO thin + 1 CGU + `opt-level=s`) `dist` build cost on **every PR**, not only real releases, trading CI wall-clock for install-footprint on a path android/windows never share (their CI/dev recipes stay on `release`; only their dedicated shipping recipes — `android-bundle`, the windows installer recipes — opt into `dist`). `dist` is available (`just mac-app release dist`, `just mac-dmg dist`, `just pkg-sign-only dist`) but not defaulted into either the CI job, `release-macos.yml`'s build step, or the all-in-one `.pkg` pipeline — a genuine cost/scope call, left for the user rather than decided silently here.

### Required Environment Variables

| Variable | Purpose |
|---|---|
| `FAUNA_SIGN_IDENTITY` | Developer ID Application certificate name for binary and package signing (`--sign-only` + `--sign`) |
| `FAUNA_INSTALLER_IDENTITY` | Developer ID Installer certificate name (used by `productbuild`) (`--sign-only` + `--sign`) |
| `FAUNA_APPLE_ID` | Apple ID email for notarytool authentication (`--sign` only) |
| `FAUNA_TEAM_ID` | Apple Developer team ID (`--sign` only) |
| `FAUNA_NOTARY_PASSWORD` | App-specific password for notarytool (not the Apple ID login password) (`--sign` only) |
| `FAUNA_NOTARY_KEY_FILE` + `FAUNA_NOTARY_KEY_ID` + `FAUNA_NOTARY_ISSUER` | An **App Store Connect API key** (`notarytool --key/--key-id/--issuer`), accepted by `mac-dmg-package` **instead of** `FAUNA_APPLE_ID` + `FAUNA_NOTARY_PASSWORD`. Preferred wherever the credential is stored rather than typed: it is scoped to the key's ASC role, revocable on its own, and involves no account password or 2FA. What the release workflow uses. |

### The `.dmg` release pipeline (decided 2026-08-25)

`release-macos.yml` in the public repository, on every `v*` tag (and on demand for a dry run), two jobs on GitHub-hosted `macos-15` runners:

1. **`build` — no credentials in scope.** `just mac-app release` (the same recipe the CI `macos` job runs on every PR), `codesign --verify`, upload the ad-hoc-signed bundle as a workflow artifact.
2. **`sign` — inside the `release-macos` Environment.** Downloads that bundle, imports the Developer ID Application certificate into a throwaway keychain (plain `security` calls, deleted at the end whether or not signing succeeded), writes the ASC API key, runs `just mac-dmg-package` (hardened-runtime sign → `.dmg` → notarize → staple the `.dmg` **and** the bundle), then `actions/attest-build-provenance` on the `.dmg`, and publishes it (+ the `.sha256`) as the GitHub Release with the verification recipe in the release notes.

The Environment is what gates the credentials: **required reviewer = a maintainer, `v*` tags only**, so no run — not even one launched by a workflow edit merged to `main` — reaches the certificate without a human approving that specific run, and a pull request (from a fork or not) never sees the secrets at all. Secrets it holds: `FAUNA_SIGN_CERT_P12` (base64 `.p12`), `FAUNA_SIGN_CERT_PASSWORD`, `FAUNA_SIGN_IDENTITY`, `FAUNA_TEAM_ID`, `FAUNA_NOTARY_KEY_P8`, `FAUNA_NOTARY_KEY_ID`, `FAUNA_NOTARY_ISSUER`. The credential-custody decision itself (why GitHub secrets and not a Tier-0 machine, for now) is owned by `../release-integrity.md` § Implementation status today. <!-- gitleaks:allow -->

**What the attestation says, and what it does not.** `gh attestation verify` proves the file's digest was produced by *this* workflow at *this* commit of *this* repository on GitHub's infrastructure (Sigstore-signed, transparency-logged). It does not prove the source is trustworthy (`../release-integrity.md` § Defense priority), and it is not bit-for-bit reproducibility — a third party cannot re-sign, and the inner Mach-Os are only comparable after `codesign --remove-signature` once the build is made deterministic, a lower-tier follow-on.

## Uninstall

The `fauna-uninstall` script is included in every component package and installed to `/usr/local/bin/fauna-uninstall`.

```
sudo fauna-uninstall [username]
```

What it does:

1. Boots out the `social.fauna.nest` / `social.fauna.bridge` machine daemons (`launchctl bootout system/…`) and the per-user `social.fauna.sync-agent` agent (`gui/$UID/…`) — current labels only (§ Identifier domain)
2. Removes the daemon plists from `/Library/LaunchDaemons/` and the sync plist from `~/Library/LaunchAgents/`
3. Removes the binaries from `/usr/local/bin/` and the desktop app `/Applications/Fauna.app` (the app's data lives per-user in `~/Library` + Keychain, never inside the bundle)
4. **Preserves** the system server data (the daemon logs live inside it), **both** per-user data homes — the app's `~/Library/Application Support/Fauna/` and the sync agent's app-group container (§ File Layout owns the paths) — **and** the `_fauna`/`_fauna-bridge` service users (they own the preserved server data; a reinstall recreates them idempotently). The script's closing message names each preserved location, since it is the only place the user is ever told where their data went; that message is pinned by `tests/e2e-unified/tests/test_macos_uninstall_message_paths.py` (tier_1, static) because it silently mislabelled the app home as the sync one from the 2026-07-19 state unification until 2026-08-21

To also remove the server data, run manually after uninstalling:

```
sudo rm -rf "/Library/Application Support/Fauna/"
```

## Upgrade

### All-in-one Package

Re-running the `.pkg` installer upgrades the binaries in place (and replaces `/Applications/Fauna.app` if the app component is selected). Postinstall scripts leave existing plists in place (idempotence — § Upgrade Behavior) and restart running services automatically. No manual steps are required. Re-running a newer `.pkg` is also how a `.pkg`-installed app is upgraded: the app itself never updates in place (below).

### Desktop App

The app tells you; you upgrade. The rule is [`README.md`](README.md) § Knowing a newer version is out (ruled 2026-10-03), and on macOS it means:

- **Settings → General → About** shows the version you are running and a **Check for Updates** button; the app menu's *Check for Updates…* is a second door onto the same check. Both ask through the shared `fauna-ffi` face of `fauna_client::update_look`, never a feed of the app's own.
- **Once per sign-in** the app looks by itself and, if a newer release is out, shows the same notice: the version and the release page that carries the new `.dmg` and `.pkg`.
- **Nothing more.** No timer, no background download, no in-place replacement and no toggle. You upgrade by installing the newer `.dmg` (drag over the old app) or re-running the newer `.pkg`.

## Testing

E2E tests: `tests/e2e-unified/tests/platform/macos/test_installer.py`

- **`TestDryRun`** — validates the built `.pkg` archive (no sudo, CI-safe): archive structure (four component packages incl. `fauna-app.pkg`), distribution XML (four choices; app default-selected), the **app component** install (`PackageInfo` `install-location="/Applications"`, `relocatable="false"`, ships `Fauna.app`), component binaries (`fauna-nest-daemon` in the node component), MDA dylib relocation, and — the machine-service proof — that the **postinstall scripts encode the daemon shape**: the nest/bridge postinstall + `common.sh` create `_fauna`/`_fauna-bridge` via `dscl`, generate `/Library/LaunchDaemons` plists running `fauna-nest-daemon`/`fauna-bridge-supervisor` under `UserName` with `FAUNA_DATA_DIR` + the nest's `FaunaNest`→`443` socket dict, bootstrap into the **system** domain, and keep sync per-user. `TestDryRun` is **11** collected cases, headless and CI-safe. The 2026-09-25 compat-remnant sweep (§ above) deleted the parametrized `test_daemon_postinstall_retires_the_pre_rename_label` (nest + bridge, 2 cases) and `test_nest_postinstall_migrates_per_user_state`, since no installer script retires a legacy label or migrates per-user server state any more — `test_apple_identifier_pins.py::test_no_installer_script_sweeps_a_retired_label` pins the removal instead. Re-verified on macOS 2026-09-25: 11/11 green against the rebuilt `.pkg` (`apple-ffi-host release` + `mac-app`, post-sweep tree).
- **`TestFullInstall`** — full install/uninstall lifecycle (requires sudo): the desktop app installed to `/Applications`, binary installation, `dscl` service-user creation, `/Library/LaunchDaemons` daemon plists + the per-user sync agent, system-data-dir ownership (`_fauna` / `_fauna-bridge`), daemon load/unload, upgrade plist preservation, uninstall with data preservation (and `/Applications/Fauna.app` removed). The install **force-selects all four components** via `-applyChoiceChangesXML` (nest/bridge default-unselected) and asserts the **enabled-when-selected** daemon shape (no `Disabled` key). **All four components were VERIFIED on a real Mac machine 2026-06-28 — `TestDryRun` 11/11 + `TestFullInstall` 15/15** (per-class method counts; 26 = the whole file at the time), against the unsigned all-in-one `.pkg` built end-to-end through the **release host-only FFI** (`apple-ffi-host release`; the prior watchos-sim `apple-ffi` flake that gated the release build is fixed — § Implementation status today). `TestFullInstall` gained a 16th method 2026-07-20, `test_sync_agent_resolves_reachable_production_paths` (polls the installed agent's real launchd-spawned socket/data-root, not just well-formedness) — added while chasing the first-run findings below, collection/syntax-checked but **not yet run against a real install**. `TestFullInstall` gained a 17th method 2026-08-28, `test_installed_app_registers_its_file_provider_appex` — encodes the FIRST REAL-INSTALL OBSERVATION above (`pluginkit -m -v -p com.apple.fileprovider-nonui` lists `social.fauna.fauna.FileProvider` from the installed bundle, not a build-tree twin) as a regression assertion; deliberately does not assert the enable state or enumeration (the last inch, still gated on the `-34018` keychain finding). **The 2026-09-25 compat-remnant sweep (§ above) then deleted `test_migration_preserves_per_user_server_state`** (drove `common.sh`'s now-removed `migrate_per_user_nest_to_system` directly), so `TestFullInstall` is **16** methods today, not 17. `TestDryRun`'s 2 retire-label cases (above) ran green headless on the main branch as part of the 2026-08-12 domain sweep (13/13); both they and `test_nest_postinstall_migrates_per_user_state` were removed by the 2026-09-25 sweep in turn — none of the three touched the sudo-gated path. Still **needs a maintainer-run environment** (credentialed/manual; tracked internally): the signed-install + live `:443` socket-activation **serving** proof (the suite loads the daemon but does not `curl :443`), and running the unverified 16th `TestFullInstall` method above (both need interactive sudo).

Run: `sudo pytest tests/e2e-unified/tests/platform/macos/test_installer.py` (or `::TestDryRun` for CI).

**The sudo run must not build.** Running the whole pytest process as root means anything it
builds is written as root, and a root-owned file inside a developer checkout is a one-way door:
`cargo clean` stops at the first `EACCES` and the machine's disk-reclaim sweeps skip what they
cannot delete. That is not hypothetical — the 2026-06-28 verification run lazily triggered
release builds as root and left ~3.8G of root-owned artifacts under `target/`, which went
unnoticed for seven weeks on a disk-bound machine until a human removed them by hand. **So the
supported flow is two steps: build the `.pkg` as your normal user (`installer/macos/build.sh`,
or one non-sudo `TestDryRun` run), then `sudo pytest` the install legs against that artifact.**
Two mechanisms hold it, both in `tests/e2e-unified/helpers/root_build_guard.py` so that neither
depends on a real `sudo` run to be proved (`tests/test_root_build_guard.py`, tier_1,
mutation-graded): the `.pkg` fixture *refuses* to build when root and the cache is stale, and a
session-scoped guard *walks* `target/` and `apps/fauna-apple/.build` before and after the run,
failing with the offending paths and the removal command. The walk is the regression proof —
the refusal only covers the build path we already know about, while the walk catches a root
build arriving by any route. It runs only under sudo (nothing else can create the condition)
and costs a few seconds on a full target dir.
