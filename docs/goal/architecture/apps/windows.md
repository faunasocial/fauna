# Windows App — target state

Owns: windows, windows-shell-extension
Status: ratified — as-built architecture; remaining gaps declared in § Implementation status today
Authority: windows-app architecture — the WinUI 3 app and its two nest transports (the WS-RPC `NestRpcClient` over the UniFFI `FfiNestClient`, plus `DirectNestClient` HTTP for unmigrated clusters), in-process MLS via `fauna_ffi.dll`, the per-user sync agent + cfapi on-demand hydration host + per-session named-pipe IPC, and shell-extension behavior (overlay/context-menu COM surfaces, pipe protocol); MSI packaging, COM registration, Path-1 upgrade + the services process model → [`../installers/windows.md`](../installers/windows.md); cross-app behavior → [`common.md`](common.md); the app-lifecycle model mirrors [`linux.md`](linux.md) § System Tray.

Last verified: 2026-09-16 (docs-consistency sweep, one of ~23 concepts in a wide parallel fan-out for a 4080-commit watermark gap — re-flow-traced the two-plane transport: `grep -rl INestRpcClient`/`DirectNestClient` under `FaunaApp/` now land at 107/10 consuming files, up from 100/11 three weeks prior, so the "~100" figure was bumped to "~107" (the "~10" figure held); confirmed the store-safe/payments `Views/Payments/` `UserControl` count still holds at six (`AskingPriceInput`/`NostrZapSignersSection` landed 2026-09-06 are already the doc's cited additions, no further growth); the "known code bug" (`fauna-bridge-service/src/pipe_server.rs:67` querying the deleted `GET /api/v1/bridges`) was then present and has since been resolved by deleting the pipe server. **Drift found+fixed:** (1) the Primary-nav bullet and the Nav-order line still listed "P2P" as a top-level nav destination — the WireGuard-stack deletion (2026-08-23, predating even the prior sweep's watermark) removed the `NavigationViewItem` from `MainPage.xaml`, and this doc's own § P2P section already said so; the two lists contradicted the doc's own § P2P section, now fixed to match. (2) `FaunaApp/Views/` re-enumerated at 62 top-level `.xaml.cs` pages (up from 58) — four pages landed since last sweep with no matching doc update: `AdminCustodyHostingPage` (2026-09-09, Admin hub), `SettingsMailImportPage` (2026-08-31) and `SettingsMemberReviewPage` (2026-09-09, Settings shell), `LaunchAccountIndexUnreadablePage` (2026-09-11, Launch surfaces) — all four added to their respective Views-section bullets. Grep-terms otherwise clean across the sweep scope (remaining hits across `docs/goal/` are legitimate per-app/per-feature references to the shared transport seam, not restatements of this doc's own windows-specific counts); no-modes tombstone lexicon absent from this doc | Source: `apps/fauna-windows/`

## Goal

A WinUI 3 (C# / XAML / .NET 10) frontend on **two nest transports**: the
**WS-RPC plane** — `NestRpcClient`/`INestRpcClient`, a thin C# seam over the
UniFFI `FfiNestClient` (the native WS-RPC requester shared with Apple/Android)
— carries every migrated cluster; `DirectNestClient` (HTTP) carries the
unmigrated remainder until it migrates. MLS runs in-process by P/Invoke to
`fauna_ffi.dll` — never delegated to a service. The per-user sync agent exists
for file sync + shell-extension integration only; the app does not require it
for basic connectivity.

## Tech Stack

| Component | Detail |
|-----------|--------|
| Language | C# 13 |
| UI framework | WinUI 3 (XAML), WindowsAppSDK 1.7 |
| Runtime | .NET 10 |
| MVVM | CommunityToolkit.Mvvm (`ObservableObject`, `[RelayCommand]`) |
| Crypto | `CryptoService` — delegates all cryptographic operations to the uniffi-generated fauna-ffi bindings (byte layouts guaranteed to match Apple/Android/Linux/Web) |
| Key storage | Windows Credential Manager generic credentials that do not roam, written by the shared Rust arm over the FFI, under the shared multi-account registry (`FfiAccountRegistry` over `LogicalSecretStore`) — the only store (§ Key storage) |
| Serialization | Canonical IPLD dag-cbor for nest wire (per `docs/goal/architecture/serialization.md`); `System.Text.Json` for the externally-forced HTTP/JSON residue (federation JSON-LD, OAuth, NodeInfo). The local shell-extension named-pipe IPC carries length-prefixed **canonical dag-cbor** frames — the same `fauna_cbor` encoder as the nest wire (`encode_frame`/`decode_payload` in `fauna-ipc`), spoken by the shared Rust codec. The C# app no longer mirrors that codec: it reaches the agent through the shared FFI provisioner (§ Named-pipe IPC below). |
| Backend processes | Per-user sync agent (`fauna-sync-agent.exe`) + optional `fauna-nest-service` / `fauna-bridge-service` |
| Shell integration | Cloud Files API (cfapi) via Rust FFI |
| Installer | WiX v6 MSI |
| Auto-updates | `UpdateService` — polls GitHub Releases API, shared `fauna_core::version::is_newer` compare |

## Architecture: the two-plane nest connection

| Plane | Seam | Carries |
|-------|------|---------|
| **WS-RPC (primary for migrated clusters)** | `NestRpcClient` / `INestRpcClient` wrapping the UniFFI `FfiNestClient` | the migrated `fauna.{account,feed,posts}.*` clusters, bridges (`FfiBridgesClient`), mail machines, nests panel, muted words, spam, personalization, labeler catalog, the Events surface over the encrypted CalDAV store (`FfiCaldavClient`), WS-RPC push (OS notifications) |
| **HTTP (`DirectNestClient`)** | direct HTTP to the nest | the unmigrated remainder, until each cluster migrates |

**Page pattern:** a `FaunaApp.Core` ViewModel over the `INestRpcClient` seam
(or a uniffi-generated `I<Machine>` interface for machine-backed pages), faked
for deterministic unit tests — never `FfiNestClient` directly in a page.

The UniFFI client builds its own `wss` URL (`ws_adapter::build_ws_url`) and
handles reconnect + token refresh internally; there is no C# WebSocket layer
(the dead push-only `WebSocketService` was removed when notifications moved
onto WS-RPC push).

**Auth flow:** the Ed25519 key lives in Credential Manager (§ Key storage) as the account's per-actor registry slot, read through the served account's `SessionMaterial` (`RegistrySessionAccount`);
`CryptoService` signs through the fauna-ffi bindings. Launch routing goes
through the shared `LaunchMachine` (§ App Entry); `DirectNestClient` sources
its bearer from the machine.

### Key storage

**The store (built 2026-10-03).** The app's one credential store is Windows
Credential Manager, reached through the shared Rust arm rather than a second
implementation of it: `CredManSecretBackend` (`FaunaApp.Core`) calls
`fauna-ffi`'s windows-only `native_keyring_get` / `_set` / `_delete`, which are
`fauna_credential_store`'s `keyring_*` wrappers over its `win_credman` module.
One registry row is one generic credential: `TargetName` =
`fauna-windows/<logical key>`, the value as a UTF-8 blob,
`CRED_PERSIST_LOCAL_MACHINE` — per-user despite the name, and never roamed to
the person's other PCs. The terminal app and the sync agent write the same arm
under their own namespaces, so the grammar has one writer. A live test reads a
row back through Win32 `CredReadW`, an observer sharing no code with the
writer, and pins the name, the type, the persistence and the bytes
(`CredManSecretBackendTests`). What the cross-app rule asks of a store, and
this one's cell: [`common.md`](common.md) § Credential storage.

**The bound the store brings.** One row holds at most 2560 bytes, and a write
past that reports nothing: the row keeps the value it had. Every registry row
but one is far under it. The account index, which grows with each account, is
bounded against the cap by the shared registry
([`../long-term-store.md`](../long-term-store.md) § Multi-account evolution →
*The index is bounded*).

**The carry-across (removed 2026-10-04).** Until 2026-10-03 the store was
`PasswordVault`, the platform's Credential Locker, which roams, and for one day
the app carried that store's rows into Credential Manager at launch; the carry
was removed whole under the user's blank-slate ruling
([`../compat-remnant-sweep.md`](../compat-remnant-sweep.md) § Still queued, and
what stays), so the app reads and writes Credential Manager alone and no code
names the Locker.

**Client lifetime — resolve the nest client per call; never capture it.** Both
plane clients are **replaced and disposed** whenever the session or nest URL
changes (`App.DisposeNestClients`, fired on re-login, the onboarding hand-off and
a factory-reset re-onboard). They are handed to a page as a `ServiceClients`
navigation parameter, so anything that **outlives one hand-off** — a `static`
singleton, a cached helper — must hold a *resolver* (`() => App.CurrentNest`),
not the instance it was constructed with. A captured reference keeps calling a
disposed `HttpClient`: `ObjectDisposedException`, which a swallow-and-skip
consumer renders as a silently missing element rather than an error. This is not
hypothetical — the feed/conversations `BlobImageLoader` captured its client and
so every image silently stopped loading after any re-login until the app was
restarted (fixed 2026-07-11; regression-guarded by the settings-then-image
ordering in the windows e2e).

The rule has a second half that binds the *producer* of the hand-off: **whenever
the clients are replaced, the hand-off must actually be re-performed.** A page
that took its `ServiceClients` at one navigation keeps them until it is navigated
again, so replacing the clients without re-navigating the current page strands it
on the disposed pair just as surely as a captured field does. This is the shape
the e2e `set_state` seam broke: one command may carry several blocks that each
defer UI-thread work (`session` + `nav` + `messages` + `compose`), and the `nav`
block *overwrote* the `session` block's queued `MainPage` hand-off rather than
sequencing after it — so a second login with no intervening `reset()` (which
would have navigated back to `OnboardingPage` and forced a rebuild anyway) left
`MainPage` calling a disposed `NestRpcClient`. The blocks therefore **compose**,
never overwrite (`PostActionChain.Then`, awaited in order — an `Action`-typed
chain makes each async step `async void` and silently interleaves them). Fixed
2026-07-19; pinned by `tests/e2e-unified/tests/test_second_login_live_clients.py`
plus `PostActionChainTests`.

The rule has a **third half, about the pages themselves: a page is per-login, so
every page-scoped background resource must be released in `OnNavigatedFrom`.**
`MainPage` is not "the app-lifetime shell" — sign-out, account switch and the e2e
`reset()` each navigate the root frame to `OnboardingPage`, and the next login
constructs a brand-new instance. A running `DispatcherTimer` is rooted by the
dispatcher, so a page that never stops one can never be collected: it stays alive
and keeps doing its periodic work forever, once per login ever performed. On
Windows this degrades the whole **process**, not just memory —
`NamedPipeClientStream.ConnectAsync` wraps a *blocking* connect in `Task.Run`, so
each leaked sync-agent poller parks a thread-pool thread on every tick. Past a
handful of logins the pool starves, WS-RPC continuations stop being serviced, and
the app stops answering UIA and test-agent post-actions — surfacing as whichever
probe lands first (a `GetMainWindow` COM timeout, a dead FlaUI-bridge endpoint, or
`App did not acknowledge command`), never as the leak itself. Measured 2026-07-22
before the fix: 8 logins → 7 live pollers, with test-agent state pushes drifting
from ~1s to ~29s. (That measurement came out of the "second family journey" flake
hunt; fixing the leak did **not** resolve that flake, and at the time it looked
like a separate, independent defect. It has since been root-caused and closed —
not a product bug at all: an undrained e2e harness pipe filled its OS buffer and
blocked the app's UI thread inside a native log write, so `set_state` timed out
against an app that was alive throughout. Fixed uniformly across windows/web/android
(`e2e-conventions.md` § Cross-app e2e conventions, convention 13) and confirmed green
end-to-end, tracked internally.) `EventsPage`/`FeedPage` are the
pattern to copy; pinned by
`tests/e2e-unified/tests/test_windows_login_cycle_poller_leak.py`, which asserts
the live-poller count (`diagnostics.live_sync_agent_pollers`) directly so a
regression fails as itself rather than as a downstream timing flake.

---

## Workspace Layout

```
fauna-windows/
├── FaunaApp/                 # WinUI 3 C# solution
│   ├── FaunaApp/             # Presentation layer (XAML views, code-behind)
│   ├── FaunaApp.Core/        # ViewModels, Services, Models (CommunityToolkit.Mvvm)
│   └── FaunaApp.Tests/       # Unit tests
├── fauna-bridge-service/     # Rust: IMAP/CalDAV bridge supervision — spawns the Go fauna-mail-bridge MDA (top-level workspace member)
├── fauna-nest-service/       # Rust: the FaunaNest machine service (network-reachable HTTPS :443, optional)
├── shell-ext/                # Windows shell integration (Cloud Files API)
└── shellext-fixture/         # Test-only harness binary driving shell-ext headlessly (§ Shell Extension → Observation harness)
```
`fauna-ipc` and the sync agent's own logic no longer live under `apps/fauna-windows/`: `fauna-ipc`
moved to the root-workspace `libs/fauna-ipc/`, and the agent moved to the root-workspace
`bins/fauna-sync-agent/` (§ IPC above; A1/A1b, `sync-agent.md`), which builds
`fauna-sync-agent.exe` itself — the wrapper crate `fauna-sync-service/` that produced the exe
from it is retired (2026-10-02; `sync-agent.md` § Implementation status today, A5).

**Test fakes for generated UniFFI interfaces inherit the generated `<X>FakeBase`, never
implement the interface directly** (ratified 2026-07-23).
`scripts/generate-windows-fake-bases.py` emits one abstract `<X>FakeBase : I<X>` per generated
public interface into `FaunaApp.Tests/Generated/FakeBases.g.cs`, every member `virtual` and
throwing; a hand-written fake (`FakeAccountRegistry`, `FakeDraftsSync`, `FakeWebClient`, …)
inherits it and `override`s only the members it implements. This makes a Rust-side interface
growth compile clean by construction instead of relying on every fake author to notice —
`docs/goal/architecture/merge-gate-check.md` § Merge-gate check (win) owns the incident history and
the gate mechanics.

---

## Views

ViewModels live in `FaunaApp.Core` and use `CommunityToolkit.Mvvm`; views bind
to ViewModels, no business logic in code-behind. The page set (one module per
feature — see `FaunaApp/Views/`):

- **Primary nav:** Feed, Conversations, Contacts (+ CardDetail), Profile,
  Bridges, Backups, Media, Search, Calendar (Events + EventDetail),
  Moderation, Notifications, plus the gated **Admin** entry. (P2P is no
  longer a nav destination — removed with the WireGuard stack 2026-08-23,
  § P2P below.)
- **Admin hub:** `AdminShellPage` hosting `AdminDashboardPage`,
  `AdminUsersPage`, `AdminSettingsPage`, `AdminAliasesPage`,
  `AdminBridgesPendingPage`, `AdminCalendarPage`, `AdminContactsPage`,
  `AdminCustodyHostingPage`, `AdminDnsPage`, `AdminFilesPage`,
  `AdminLogsPage`, `AdminMailPage`, `AdminNestPage`, `AdminWebPage`
  (`AdminNavigation.cs`; shell model → `behavior/admin.md`).
- **Settings shell:** `SettingsShellPage` (Status is the default sub-page) +
  sub-pages Account, Encryption, General, LinkedNests (Nests), Logs, Mail,
  MailAliases, MailExport, MailImport, MailLists, MailListMembers, MailSpam,
  MemberReview, MutedWords, Privacy, Subscriptions, TaskDelegation, Web —
  plus `DevicesPage`, `FoldersPage`, `PersonalizationPage`,
  `LabelerCatalogPage`, `AtprotoPage`, `NostrPage`, `StatusPage` reached
  through the shell. Shared controls include `NestsPanel` and
  `MailSettingsPanel`.
- **Launch surfaces:** `OnboardingPage` (+ `Views/Onboarding/`),
  `LaunchRetryPage`, `LaunchNeedsUpdatePage`, `LaunchIdentityChangedPage`,
  `LaunchInstanceChooserPage`, `LaunchAccountIndexUnreadablePage`,
  `FamilyPage` (gated footer).

**Retired (2026-06-28 sync/folder UI unification):** `GroupsPage` /
`GroupChatPage` (groups live in Conversations), `SyncPage`, `SyncFoldersPage`
(both replaced by `FoldersPage`), and the standalone `ConflictsPage`
(conflicts render per-set on `FoldersPage`).

**Nav order (MainPage.xaml):** Feed (default) > Conversations > Contacts >
Profile > Bridges > Backups > Media > Search > Calendar > Moderation >
Notifications > Admin (gated) | Footer: Settings > Family (gated).

**Keyboard shortcuts:** `Ctrl+N` (compose/feed), `Ctrl+K` (quick switcher),
`Ctrl+,` (settings) via `KeyboardAccelerator` on `MainPage`.

**Dialogs:** every `ContentDialog` is shown through the one shell gate
`Controls/Dialogs.cs`, never a bare `ShowAsync()`. WinUI allows one
ContentDialog per XamlRoot and throws on a second, so the gate refuses a
second open on `error-message` (`common.dialog_already_open`) and leaves the
open dialog untouched; its registry is what the e2e `reset` force-closes.
Pinned by `test_windows_dialog_gate_ratchet.py`.

Per-page behavior is owned by the ui/ page docs — this doc does not restate it.

---

## IPC: Named Pipes (per-user — shell extension + app provisioning)

> **The windows sync agent was the base the cross-platform per-user agent generalized from (ratified 2026-07-18); the generalization has since landed** (crate move + persisted-capability + agent-side bearer renewal, milestones A1/A1b/A2 closed 2026-07-19; **A5, the windows swap, closed 2026-07-22** — current status tracked in `sync-agent.md`, not restated here). `fauna-sync-agent.exe` is built from `bins/fauna-sync-agent` itself (the artifact was renamed from `fauna-sync.exe` at A5, 2026-07-22, and the windows-only wrapper crate that produced it until 2026-10-02 is retired; this pipe + `fauna-ipc` codec unchanged as the windows transport — the pipe name is a separate axis from the artifact name; `fauna-ipc` now lives in `libs/`). Owner: [`sync-agent.md`](sync-agent.md); this doc keeps the windows-specific halves (pipe DACL, shell extension, cfapi).

The sync agent's named pipe is **per-user / per-session**: `\\.\pipe\fauna-sync.<user-SID>`, DACL'd to that
user's SID only — replacing the former single global `\\.\pipe\fauna-sync`, which was DACL'd wide to
`BUILTIN\Users` + `INTERACTIVE` and so could not isolate simultaneous users. Two local peers in the user's
session talk to it: the **shell extension** (`shell-ext/`, in that user's `explorer.exe`) for file-status
overlays + context-menu actions, and the **desktop app**, which provisions the agent at logon
(`ProvisionCapability` / `RefreshBearer` — see § cfapi). Both derive the same name from their own process
token (`ProcessIdToSessionId` / the token's user SID).

| Pipe | Name | Peers (same session) |
|------|------|----------------------|
| fauna-sync agent (per-user) | `\\.\pipe\fauna-sync.<user-SID>` | shell extension + desktop app |

**One pipe server, one owner-only DACL (2026-08-24; the second server removed 2026-10-02).**
The per-user sync agent is the only windows pipe server. It serves through the shared
`fauna_ipc::pipe_transport`: one DACL builder, one accept loop, one client handler,
generic over the protocol's message types. It is the windows sibling of
`fauna_ipc::unix_transport` and shares the frame loop with it, so `MAX_FRAME_SIZE`
has exactly one enforcer on both sides of every transport.

`PipeSecurity::for_owner` grants the serving process's own token user and nobody else,
pinned by `pipe_transport`'s own DACL test. Under the per-user model that token user
**is** the interactive user; `BUILTIN\Users` + `INTERACTIVE` were the multi-user leak
removed in Phase 2 of the per-user migration. The builder takes no trustee parameter on
purpose: a server that would need a wider grant adds it there, with its reason, under
review. The FaunaBridge service (`fauna-bridge-service`, packaged per
[`installers/windows.md`](../installers/windows.md) § Services and the per-user sync
agent) served a second pipe, `\\.\pipe\fauna-bridge`, DACL'd to `BUILTIN\Users` so any
local account could reach it, with an unauthenticated `Shutdown` verb. Its only client
was a diagnostic CLI no installer shipped, so pipe, protocol and CLI were deleted
together; the service now serves no IPC and only supervises the Go MDA.

The DACL is the **server's** half of the question and cannot answer the client's: `\\.\pipe\` is machine-wide, so a client that merely opened the right name has no idea whose pipe it opened. The client-side rule — verify the serving process's token user and refuse before writing a frame, plus `SECURITY_IDENTIFICATION` on the connect — is owned by [`sync-agent.md`](sync-agent.md) § Transports.

**Protocol:** Length-prefixed **canonical dag-cbor** frames — the shared `fauna_cbor`
encoder (`encode_frame`/`decode_payload` in `fauna-ipc`), the same wire format as the
nest transport. The shell extension speaks it via the shared Rust codec; the **C# WinUI app no
longer speaks this pipe at all** — it reaches the agent through the shared FFI provisioner, and its
hand-written mirror codec (`DagCbor.cs`, `SyncServicePipeClient`, the `IpcMessages.cs` codec half)
plus the `DagCborIpcTests.cs` conformance pins were deleted with that retirement (2026-08-05).

```
[u32 LE length][canonical dag-cbor payload]
```

- Max frame size: 16 MiB (`MAX_FRAME_SIZE`)
- Requests carry a `id: u64` correlation field; responses echo it back
- Async events (e.g., file status changes) are pushed from the service to the client without a prior request

---

## Local Nest Service (Optional)

`fauna-nest-service` is the installed `FaunaNest` machine service — a real **network-reachable nest server**, not a loopback proxy: it binds `0.0.0.0:443` HTTPS off the self-signed floor cert via the shared `fauna_nest::desktop_serve` sequence (`apps/fauna-windows/fauna-nest-service/src/service.rs`; legacy persisted `7450`-era ports are actively rewritten by its `config.rs`). Packaging, virtual accounts, and lifecycle: `installers/windows.md` § Services and the per-user sync agent. (The old `http://127.0.0.1:7450` caching-proxy/MLS-delegate model is retired — MLS runs via P/Invoke/UniFFI to `fauna_ffi.dll`, never a local HTTP proxy.)

---

## fauna-mls Integration

The C# frontend calls MLS via P/Invoke to `fauna_ffi.dll` (the `fauna-ffi`
crate; conversations ride the default-on `conversations-session` feature's
`ConversationsSession` seam — `ConversationsViewModel` consumes it). MLS state
is persisted in SQLite, account-scoped at `%LOCALAPPDATA%\Fauna\<actor-hex>\mls.db`
(`AccountStateDir.MlsDbPath` — owner: [`account-scoping-dispositions.md`](account-scoping-dispositions.md)
§ Implementation status today, closed 2026-07-23). Pending MLS Welcomes
are fetched and processed on app launch. Platform-binding table + boundaries:
[`common.md`](common.md) § MLS; protocol → `behavior/direct-messages.md`.
(`fauna-nest-service` links `fauna-mls` transitively via `fauna-nest` for its own
internal use, separate from the app's MLS path. `bins/fauna-sync-agent`
does **not** — the sync agent is deliberately bearer-only, building its engines with
`mls: None` (no identity keypair, no MLS state); corrected 2026-07-20, this doc previously
mis-stated it as an `fauna-mls` consumer.)

---

## Shell Extension

`shell-ext/` (→ `fauna_shell.dll`) is the Explorer **COM UI-shell** layer — overlay
icons and a right-click context menu on Fauna-synced files. It is *distinct from* the
Cloud Files API (cfapi) hydration engine: cfapi lives in `libs/fauna-cfapi` and is
consumed by `fauna-sync-agent` (see *cfapi crate* below), **not** by this DLL.

- **Sync status overlays:** four `IShellIconOverlayIdentifier` handlers (synced /
  syncing / cloud-only / error) badge Fauna-tracked items in Explorer — **files *and*
  folders** (USER-ratified 2026-07-14).
  - **Folders carry a badge too**, so a synced folder is identifiable at a glance without
    opening it — the behaviour every comparable app ships (OneDrive, Dropbox). This
    covers the bound sync-root folder and every folder beneath it.
  - **A folder's status is the aggregate of its tracked descendants**, resolved by
    severity so the badge always reports the worst thing inside: any `Error` → **Error**;
    else any `Syncing` → **Syncing**; else any `Synced` → **Synced** (a folder holding a
    mix of hydrated and cloud-only children reads as Synced — its content *is* all
    present-or-available); else all descendants cloud-only → **CloudOnly**; a folder with
    no tracked descendants is **not badged** (untracked, exactly like an untracked file).
  - **Badge geometry:** overlays are composited across the base icon's whole rect, so each
    `.ico` must place its glyph in the **lower-left quadrant** with the rest transparent.
    A centred full-canvas glyph renders as a disc swallowing the file icon (which is what
    shipped until 2026-07-14 — see *Implementation status today*).
- **Context menu:** an `IExplorerCommand` "Fauna" submenu — a **Share** action, a read-only
  **device-info** item (renders `ECS_DISABLED` — grayed, non-clickable; an informational row,
  not a command, USER-decided 2026-07-18 — see *Status matrix* row *Device info leaf*), and a
  **Version history** item that opens a *nested* submenu of the
  file's real versions (newest first; the head marked `(current)` and disabled), each older
  entry restoring that version when invoked. Behavior and restore semantics are owned by
  `behavior/file-versions.md` § File Versions / § Restore — the shell surface never invents its
  own. The nested children are **returned objects, never CLSID-activated**, so the 8-CLSID
  contract (`installers/windows.md` § Shell Extension) is unchanged. **Tracked folders get
  the same submenu with a reduced leaf set** (USER-decided 2026-07-16): Share stays (a bound
  folder *is* the folder); the per-file device-info and version-history leaves hide
  (`context_menu::leaf_hidden_for_folder`). **What Share does** — on a file, on a folder, and
  why the leaf hands off to the app instead of the agent minting a link — is owned by
  [`../../behavior/share-links.md`](../../behavior/share-links.md) § Windows Explorer's Share
  leaf; the hand-off mechanism is this doc's (next bullet).
- **The Share hand-off (specified 2026-09-30).** Four links, each owned once:
  1. **The agent resolves, never mints.** The `ShareFile { path }` pipe verb answers
     `ShareTarget { folder_id, path }` — the Explorer path resolved through
     `path_map::resolve_to_folder_rel` to its bound set's durable id and the folder-relative
     path (`None` for the set's own root) — or an error, which hides the leaf. Which items are
     targets is `share-links.md`'s; the agent's one mechanical fact is the file rule's
     "eligible": its set's running engine holds the public-audience write arm
     (`SyncEngine::is_public_audience`, reported through the progress drain as
     `ProgressEvent::PublicAudience` — the owner-attested verdict the engine already seals by,
     so the leaf and the nest's serve gate agree on one fact; no report yet reads ineligible).
     A cross-nest set is never a target: the routes name a set by this nest's `folders.id`. The
     leaf's `GetState` asks this on Explorer's background pass (`E_PENDING` on the UI-thread
     pass, as the root does).
  2. **The shell opens a route.** `Invoke` asks again and opens the route URI through
     `ShellExecuteW`: `fauna://share-link?folder=<id>&path=<rel>` or
     `fauna://folder-share?folder=<id>`, spelled and parsed only by
     `fauna_core::app_route` (the shell's builder and the app's parser are one grammar).
     Explorer's own user-initiated invoke carries the foreground right, which is why the shell
     launches and the background agent does not. The installer's `fauna://` registration
     (`installers/windows.md` § `Protocol.wxs`; the Store manifest's `uap:Protocol`) starts
     `FaunaApp.exe "<uri>"`.
  3. **The app forwards or applies.** A launch whose argv carries a route parses it (shared)
     and, when a primary instance runs, hands it over — the pending-route file
     `%LocalAppData%\Fauna\pending-route` plus the `Local\FaunaApp-OpenRoute` event, the
     payload twin of the single-instance activate event (§ App Lifecycle) — and exits;
     otherwise it applies the route once the main page is up. The primary raises its window,
     then applies.
  4. **Applying opens a surface; the user still acts.** `share-link` opens Media, locates the file through the
     shared `MediaMachine::locate_path(folder_id, path)`, opens its `media-item-detail` and the
     machine's create surface (`open_share_create`); `folder-share` opens Settings → Folders
     with that set's `folder-share-button` flow open; `consent/<request_uri>` (the
     same-device handoff's app half, `behavior/authorization-server.md` § Consent → *How the
     same-device handoff is built*) opens Settings → Connected apps and calls the shared
     `ConnectedAppsMachine::open_handoff(request_uri)`, which opens the pending request, re-reads
     the page and puts its card in the Requests tray. **The consent arm is the one arm that makes
     a nest call** — the open consumes the single-use handle, so a second open of the same link
     reads as the one expired-link message — but it grants nothing: approving is the user's tap
     on the card, which the page's machine resolves and the route can never do. The first two arms mint,
     grant and write nothing. A `fauna://` link from any source can therefore at worst open a
     surface the user must still act on; a route naming nothing this account can see degrades to
     the page with nothing open, never an error, and a consent URI the shared parser does not
     accept is dropped, never shown. A consent route arriving signed out waits in
     `App.LaunchRoute` like the other two and is applied once the main page is up. The e2e seam
     is the `open_route` automation command (`{"uri": "fauna://…"}`), which feeds
     `App.ApplyRoute` and acks once the page's open has returned.
- Talks to `fauna-sync-agent` over the per-user sync pipe (length-prefixed dag-cbor
  frames; see § IPC) — `recv_event()` for pushed status changes, `request()` for the
  menu actions (`ShareFile` / `GetFileDevices` / `ListFileVersions` / `RestoreFileVersion`
  and the `PinFile` / `UnpinFile` / `FreeSpace` cfapi actions). Every `request()` is bounded
  (`REQUEST_TIMEOUT`): these run inside `explorer.exe`, so a wedged service must never hang
  the user's right-click. The older `GetFileVersions` verb — the *local* state-DB counter, not
  the nest's history — remains for the desktop app's own use.
- **Registration contract (settled target):** **8 COM CLSIDs** = 4 overlay handlers +
  4 context-menu handlers, per `installers/windows.md` § Shell Extension. Space-prefixed
  overlay registry keys (`  FaunaSynced`, …) sort the handlers first within Windows'
  15-overlay system limit.

**Status matrix:**

| Piece | State |
|-------|-------|
| Overlay handlers (4 × `IShellIconOverlayIdentifier`) | built + unit-tested (72 tests incl. HKCU registration round-trip) |
| **Device info leaf** (`FaunaInfoDevices`) | ✅ **decided + implemented (USER, 2026-07-18).** Renders `ECS_DISABLED` (grayed, non-clickable) rather than `ECS_ENABLED` — the item is informational ("Synced to N devices" / "On this device only"), not a command, and an enabled-but-no-op `Invoke` read as a broken menu item (user report 2026-07-17). Reuses the exact convention the version submenu already established for its own informational rows (the `(current)` head — `context_menu::FaunaVersionCommand::GetState`) rather than inventing a new shape; no other app has an equivalent OS-shell context-menu info row to check for cross-app parity against, so internal prior art in this same file is the closest available precedent. Unit-pinned: `context_menu::tests::info_devices_state_is_disabled_not_enabled`. ⚠ **The count is not real yet:** the agent's `GetFileDevices` answers a constant `device_count: 1` (`pipe_server.rs::handle_get_file_devices`), so the row always reads "On this device only". The verb survived the 2026-10-01 removal of the agent's stub IPC methods (`ListFolders` / `CreateFolder` / `GetFolderFiles` went) because this leaf calls it. |
| Context menu (`FaunaContextMenu` + 3 leaf commands) | built + unit-tested; **Share hands off to the app (built 2026-09-30, § Shell Extension → *The Share hand-off*)** — the agent's `ShareFile` target (`pipe_server::share_target`, unit-pinned), the leaf's route mapping (`context_menu::share_route`, unit-pinned against the shared parser), and the app's forward leg (`RouteHandoffEndpointTests`, real named events + the shared FFI parse); the live Explorer click-through is the same last inch as the other leaves. **Registration corrected 2026-07-14** — it had been registered under the legacy `shellex\ContextMenuHandlers` key, whose contract is `IShellExtInit`+`IContextMenu`, which this class does not implement; Explorer therefore `QueryInterface`d, got `E_NOINTERFACE`, and silently dropped it, so the submenu had **never appeared in any menu**. Now a `*\shell\Fauna` verb + `ExplorerCommandHandler` value (`installers/windows.md` § Shell Extension owns the keys). **Fix is unit-pinned and has since been live-verified in Explorer (2026-07-15)** — see the *Live-Explorer verification — context menu* row below (stale "NOT yet live-verified" wording corrected 2026-07-20; the live pass landed the day after this row was written and this row was never updated to match) |
| Class factory + DLL exports + 8-CLSID self-registration | built; `fauna_shell.dll` is regsvr32-able; in-process activation asserted for all 8 CLSIDs. `registration_contract_matches_implemented_interface` now pins each registered key to the interface the class actually exposes — the check whose absence let the dead context menu ship |
| `GetState` fast-path answer (`E_PENDING`, not `ECS_HIDDEN`, on a status-cache miss when `fOkToBeSlow=FALSE`) | built + unit-tested (2026-07-14). Explorer asks on its UI thread first; `ECS_HIDDEN` there is a *permanent* hide, so with the 30 s cache the menu would vanish on any file the overlay had not touched recently |
| Overlay status producer (both directions: hydrate `CloudOnly→Synced`, dehydrate `Synced→CloudOnly` via `path_map`) | built; `GetFileStatus` query path integration-tested headless (`producer_integration.rs`) |
| Pipe transport (real `run_pipe_server` + real `SyncPipeClient`) | integration-tested; a latent synchronous-duplex-handle deadlock was fixed (`FILE_FLAG_OVERLAPPED` per-direction) |
| Engine-driven end-to-end (real cfapi sync root → populate → hydrate → overlay flip) | built + **integration-tested headless** (`cfapi_live_integration.rs`, ~0.5 s): the service's own `register_and_connect` → callbacks → `run_hydration_loop` path against a live sync root, with the folder browsed and the placeholder opened **from another process** (cfapi fires no callbacks for the provider's own I/O). Supersedes the former "not headless-runnable" claim, which was an assumption, not a measurement — and the missing test it justified is what hid the `FileIdentity` bug |
| Live-Explorer verification — **badges** | ✅ **verified live 2026-07-14.** A cloud-only placeholder badges in Explorer; the overlay handler is demonstrably driven by the shell (677 `GetFileStatus` pipe calls in one session) |
| **Badge geometry** (lower-left corner glyph, transparent elsewhere) | ✅ **implemented (2026-07-16, USER-approved interim art).** The four `.ico` assets now confine the glyph to the lower-left quadrant, everything else transparent (green check / blue broken-ring / white-disc blue cloud / red X — programmer art; real brand assets later replace the four files as a pure swap). Generated by a dedicated dev-fleet overlay-icon generator; geometry + per-state colour-distinctness pinned by `overlay::tests::badge_icons_have_lower_left_quadrant_geometry`, which parses the embedded assets — a bad regeneration fails tests, not a user's eyeball. The old full-canvas discs ("a grey circle which covers the whole icon" — user, 2026-07-14) cannot ship again |
| **Folder badges** (bullet above: folders badged; status = severity-aggregate of tracked descendants) | ✅ **built + integration-tested headless (2026-07-15).** A folder path has no `SyncDb` row, so `GetFileStatus` folds the states of its tracked descendants instead of returning `NotTracked`: `fauna_account_store::db::SyncDb::descendant_states` (an indexed prefix-**range** over the `path` PRIMARY KEY — `["{rel}/", "{rel}0")`, wildcard-safe, subtree-only) → `path_map::folder_status_from_states` (the severity fold `Error` > `Syncing` > `Synced` > `CloudOnly`; empty → unbadged). **Design: computed on demand behind the shell's existing 30 s overlay cache — no denormalized DB aggregate**, so a folder badge can never go stale from a write-path update site (`mark_hydrated`, `upsert`, `remove`, restore re-point, stale-hydrated invalidation) forgetting to bump an ancestor; the read is already cached + an indexed subtree scan. Pinned by `producer_integration::get_file_status_aggregates_a_folder_from_its_tracked_descendants` (real handler + real per-folder `SyncDb`: a cloud-only file badges the folder `CloudOnly`; hydrate → `Synced`; an errored sibling → `Error`; an empty folder → unbadged). The live-Explorer **render** still needs a human (same last inch as file badges) |
| **Folder context menu** (`Directory\shell\Fauna`) | ✅ **decided + implemented (USER, 2026-07-16): folders get the submenu with a reduced leaf set.** The same `{4a7b8c10-…}` CLSID registers under `Directory\shell\Fauna` (shell-ext self-registration + the MSI's WiX twin + `test_installer_structure` pin); the root's tracked-ness gate already answers for folders via the badge fold. Reduced set (`context_menu::leaf_hidden_for_folder`, unit-pinned): **Share stays** — a bound location *is* the folder, the natural share target once sharing is live; **device info** and **version history hide** — per-file reads that would render a permanent "unavailable". Renders in the legacy menu via HKCR. The Win11 **default** menu's sparse-package manifest already carries `Directory` **and** `Directory\Background` item types alongside `*` (`apps/fauna-windows/installer/sparse/AppxManifest.xml.in`, present since the manifest's first commit 2026-07-14; pinned by `test_registers_the_verb_for_files_and_folders`) — **not** a remaining gap, corrected 2026-07-19 (a prior pass here mis-stated the manifest as file-types-only). **The folder-specific live render is now also captured (2026-07-22, the dev-fleet UIA observation harness):** a tracked folder shows `DEFAULT_HAS_Fauna=True` in the Win11 default menu (confirmed a genuinely folder-scoped Explorer window via its folder-only verb set — `Open in new tab`/`Pin to Start`/`Open in Terminal`, absent from the file case) and `LEGACY_HAS_Fauna=True` via the `Directory\shell\Fauna` registration (gate ③). No gap remains for either menu, file or folder |
| Live-Explorer verification — **context menu** | ✅ **OBSERVED live 2026-07-15** (headless, no human eye). The **"Fauna" verb renders in the Win11 default context menu** on a tracked file — read via UI Automation by the dev-fleet observation harness below. First render confirmation since the registration fix. See the observation-harness note below the matrix |
| Windows 11 **default** context menu (vs. "Show more options") | ✅ **OBSERVED live 2026-07-15, and the installer ships it — DONE.** "Fauna" renders in the Win11 **default** (top-level) menu (driven by the sparse package) **and** in the legacy "Show more options" menu (driven by the HKCR `*\shell\Fauna` verb), both on the *same* `{4a7b8c10-…}` CLSID. (Gate ② read live via UIA; gate ③ verified via the HKCR registration the OS renders the classic menu from — the `#32768` classic menu is MSAA-only and exposes nothing to UIA.) **The duplicate is cross-menu, not same-menu — decided: keep both, no OS-version gate** (installer decision, `installers/windows.md` § Menu placement): a Win11 `IExplorerCommand` registered the classic way is demoted to "Show more options" only, so gate ②'s UIA read showed exactly **one** "Fauna" in the modern menu; suppressing the HKCR verb would strand users who force the classic menu (a per-user `HKCU` setting a per-machine installer can't soundly detect). **The MSI ships + registers the sparse package, and both custom actions are now LIVE-VERIFIED** on a real elevated box (2026-07-15): `RegisterSparsePackage` produces the per-user **and** provisioned registrations on install (Return value 1), `DeregisterSparsePackage` removes both on uninstall (no stale entry), sequenced before `CleanShellDlls`; pinned by `TestSparsePackageRegistration` (11 tests). Not gated on code-signing — false blocker, corrected |
| **Version history — service half** (`ListFileVersions` → `fauna.files.versions.list` via the shared `SyncClient`, keyed by `fauna_core::sync::path_hash`, scoped by `folder`) | built + unit-tested against an in-memory dag-cbor fake, **and exercised against a real nest headlessly** (tier_3 `versions_tier3.rs`, opt-in `--features tier3-nest`): `list_file_versions` returns the real `sync_changes` projection, proving the `SyncClient` wire shapes against the running handlers rather than a fake that re-encodes its own request types |
| **Version history — Explorer submenu** (`FaunaInfoVersions` → `ECF_HASSUBCOMMANDS` + a dynamic `IEnumExplorerCommand` of per-version children) | built + unit-tested; the children are returned objects, so the 8-CLSID contract is untouched. The selected path is captured in `GetTitle`/`GetState` because `EnumSubCommands` receives no `IShellItemArray` |
| **Explorer's native cloud verbs ("Free up space" / "Always keep on this device")** | ✅ **the product host shell-registers (2026-07-16).** Every on-demand engine registers via `cfapi_host::register_and_connect_shell` — `StorageProviderSyncRootManager` (per-folder display name `Fauna – {folder}`, `AllowPinning=true`, interim imageres icon) — so the verbs / status column / provider grouping render on a real user's root; a leftover filter-only root is shell-registered over in place, never unregistered (measured 2026-09-25 — the `0x8007018B` refusal once measured there no longer reproduces). The verbs complete through the pin-reaction loop. **Registration is persistent** — it belongs to the location↔folder binding; a service stop only disconnects, and only unbind / folder-removal / re-bind / mode-flip tears it down (`reconcile_engines` marks the root via `mark_root_for_full_teardown`; the drop guard runs the measured-order teardown disconnect → filter → shell), with a ghost janitor at service startup for folders deleted while down. Lifecycle rationale + measured facts owned by `behavior/file-sync.md` § Per-file sync-status display. 4 live tests (`cfapi_live_integration.rs`); Explorer render pass via `hold_a_live_root_for_explorer` (now exercising this exact product path under `FAUNA_HOLD_SHELL=1`) + `read-context-menu.ps1` |
| **Live-root observation harness** (`hold_a_live_root_for_explorer`, `#[ignore]`d) | ✅ built 2026-07-16 — holds a real served root (optionally shell-registered, `FAUNA_HOLD_SHELL=1`) with a hydrated file for `FAUNA_HOLD_SECS`, printing `HOLD:` row/badge/event lines; `read-context-menu.ps1 -Invoke` clicks a named verb. Together they answer Explorer-render questions headlessly — no FaunaApp login chain |
| **Version history — `RestoreFileVersion` verb** | built + unit-tested — records via the shared `SyncClient::restore_version`, then performs the recording-device local re-point (`behavior/file-versions.md` § Restore), because catch-up skips a device's own changes and would leave *this* machine's copy stale. **Exercised end-to-end against a real nest (2026-07-15, tier_3 `versions_tier3.rs`, `--features tier3-nest`):** restore records the historical `manifest_hash`/`size_bytes`/`content_key_version` **verbatim** as a new listable head (read back over `versions.get`), and `repoint_entry` re-points this device's own `SyncDb` row to the restored version (`Placeholder` at the new head). **The cfapi byte-plane half is also exercised headlessly** (2026-07-15, TRACK V slice G, `restore_byteplane_tier3.rs`): a real cfapi `FETCH_DATA` re-hydrates the restored version's actual bytes off a re-pointed row. **Remaining:** the live-Explorer submenu **render** (see `behavior/file-versions.md` § Implementation status today) |
| **i18n** | ✅ **ruled + adopted (2026-08-27)****.** The submenu's user-visible strings (menu titles, device/version-count text, restore/share outcomes, the location-mode toggle, the notification caption) resolve through the shared catalog — `file_context_menu.*` + `common.app_name` in `i18n/strings/en.yaml`, via `fauna_core::localized::LocalizedText` + `fauna_i18n::strings::lookup` — rather than hardcoded English literals; the month table is `fauna_i18n::time::month_name_short` instead of a local copy. **The ruling:** a shell extension arguably could follow the OS UI language instead, but the product is single-language today (only `en.yaml` exists), so the distinction is moot in practice and the standing i18n rule (`ui/events.md` § Where logic lives) wins — adopt the catalog like every other user-facing surface. Output is unchanged (still English) until a second locale exists |

Version-history behavior and restore semantics are owned by `behavior/file-versions.md`
§ Restore. Both shell-surface pieces (TRACK V slices C + D) are built;
the live-Explorer *render* pass for both badges (2026-07-14) and the context menu
(2026-07-15) is now done. The service-side restore **control-plane mechanism** is
additionally exercised headlessly against a real nest (TRACK V slice F, 2026-07-15),
and the cfapi byte-plane re-hydration is exercised too (TRACK V slice G, 2026-07-15);
only the submenu pixels remain.

**Observation harness (a dev-fleet script directory, 2026-07-15).** The badges/menu are now
observable **headlessly** — no FaunaApp, login, nest, or manual folder-bind. The
load-bearing constraint: the shell-ext reports a file *tracked* only under an
**on-demand + bound** folder, and — **when a capability is present** — binding one
makes `fauna-sync` register a **cfapi sync root** that is *unenumerable* without a
real provider ("The cloud operation is invalid"). But a `fauna-sync` with **no
capability** skips cfapi registration (`engine_driver::reconcile_engines` returns at
the `No well-formed capability` gate), so the folder stays a **normal, enumerable**
folder while `GetFileStatus` still reports it tracked from the seeded `sync_entries`
row. The harness runs a no-capability `fauna-sync` on the per-SID pipe
(`fauna-shellext-fixture` seeds one tracked file) and drives Explorer's menu via UIA.
Because the DLL hardcodes the single per-SID pipe, it displaces + restores the
running production `fauna-sync` — dev-box only (no Windows CI runner). Full method +
caveats documented alongside the harness scripts.
The on-demand host **does** apply remote changes without a restart (since 2026-07-11): it folds
`changes.list` at `prepare()` and then re-pulls on the constant reconcile cadence (phase 5's
de-knob — no longer a per-folder nest row read), plus immediately on a control-plane reconnect,
feeding both through the same stale-hydrated invalidation path; `behavior/file-sync.md` § Config
+ § Implementation status today own the remaining gaps.

**cfapi crate:** The cfapi module was extracted from the sync service into a standalone
workspace crate at `libs/fauna-cfapi`. `bins/fauna-sync-agent` (the crate that builds
`fauna-sync-agent.exe`, per § IPC above) depends on it
and has real pin/unpin/dehydrate handlers — these back the `PinFile`/`UnpinFile`/`FreeSpace`
pipe methods the shell menu calls.

**On-demand hydration host (cfapi placeholders).** When a sync folder is set to
**on-demand** mode, `fauna-sync-agent` is the cfapi *provider* — it registers the sync
root, runs the shared `fauna-sync-engine`, and serves Windows' FETCH_DATA /
FETCH_PLACEHOLDERS callbacks by calling `SyncEngine::download_file_bytes` and handing the
decrypted bytes to `CfExecute(TRANSFER_DATA)`.

**The root is two-way** (since 2026-07-14): alongside those read callbacks it runs the *same*
shared `fauna_sync_engine::always_resident` watch→`upload_file` loop the always-resident root
does, so a local edit in an on-demand folder is uploaded like any other. On-demand is a storage
choice, not a direction choice; direction is owned by `behavior/on-demand-files.md` § On-Demand Files
→ *Sync direction*, which also records what the one-way root cost. Two Windows-specific notes on
the *binding*, which is what this doc owns: the watcher is the shared `notify` one
(`ReadDirectoryChangesW`), **not** cfapi `NOTIFY_*` callbacks — the upload path registers none
(the sole registered `NOTIFY_*` is the dehydrate-completion *observation*, owned by
`behavior/file-sync.md` § Per-file sync-status display); and a cfapi root makes a naive watcher unsafe, because the provider's own
`CfExecute(TRANSFER_PLACEHOLDERS)` materializes files the OS reports as ordinary `Created`
events — the placeholder guard (`LocalWrites::is_user_write`) is what tells those apart from a
user's write.

Two architectural constraints, both settled (`docs/goal/behavior/on-demand-files.md` § On-Demand Files;
`docs/goal/architecture/key-material-hierarchy.md` rule #7):

- **It runs in the user's logon session, not as LocalSystem** — per-user,
  OneDrive-style. A per-user cfapi sync root can only be served from the owning user's
  session, and the "hydrate when no user is logged in" case is moot.
- **It never holds the identity seed.** The WinUI app — the sole seed holder — provisions
  the helper with a least-privilege capability: today the owner's `BackupKey` +
  a renewable nest bearer (Phase 1), narrowing to a per-on-demand-folder key once
  `chunk_crypto`-keyed folders land (Phase 2). The seed stays in the app's Credential
  Manager.

> **✅ Provisioning is session-scoped, not a login side effect (ratified 2026-07-17 —
> one decision closing two breaks of the same weld).** The
> once-at-login `ProvisionCapability` push had provisioning welded to
> `StartMainAppAsync`, which broke twice: a **restarted agent** waited forever for a
> capability the app would never re-send (live incident 2026-07-17), and the e2e
> `set_state` login — which never enters `StartMainAppAsync` — could **never reach
> the spawn→provision→hydration chain at all** (found during the installer work:
> `test_installer.py::TestFullJourneyInstalledApp` was structurally unpassable).
> The ratified shape: a session-scoped **provisioning convergence loop**
> (`HydrationSessionService`, started by *both* login seams, stopped with the nest
> clients) that each tick (a) ensures the agent is running (probe → spawn), (b)
> pushes `RefreshBearer` with the session's current bearer, and (c) on the agent's
> `"no capability provisioned"` reply — the pinned cross-process contract in
> `pipe_server.rs::handle_refresh_bearer` — runs the full
> resolve+`ProvisionCapability`. A restarted agent therefore re-provisions within
> one tick (≤30 s) with no app restart. The e2e seam gates on
> `FAUNA_E2E_REAL_SYNC_AGENT` (the `real_sync_agent` marker) so a deterministic e2e
> never spawns or writes into the box's installed agent. Rejected alternatives: a
> set_state-path *mirror* of the login push (a third e2e/production divergence that
> tests the mirror, not the mechanism) and a real non-`set_state` e2e login
> (unscoped; fixes nothing for the restarted-agent defect). Security invariants
> unchanged — the seed never leaves the app, the agent holds no renewable
> credential, and the WHEN of provisioning was never a security bound (only the
> capability's content and its app→agent direction are).

> **Per-user agent (ratified 2026-06-19; implemented + tier_3 e2e-guarded 2026-06-20).**
> The per-user-session model governs the **whole** sync agent: `fauna-sync-agent.exe` is a
> per-user **logon agent** (no machine `NT SERVICE\FaunaSync`), launched at sign-in under
> the user's non-elevated token, serving that user's always-resident *and* on-demand sync
> plus the shell-ext pipe (both halves are live since 2026-07-13 — the always-resident half runs the
> **shared** `fauna_sync_engine::always_resident` watcher/upload loop Linux drives too, multiplexed over
> the same `EngineHost` as the on-demand roots; see `behavior/on-demand-files.md` § On-Demand Files). It reads **no `device.toml`** — the app provisions
> `{nest_url, device_id, capability}` live over the per-session pipe (§ IPC); per-user
> sync state lives under `%LocalAppData%\Fauna\sync`. Auth is **app-coupled**: the agent
> syncs while the app is alive to mint + refresh its bearer, matching Linux's in-process
> model; it never holds a renewable credential or the seed (the app's presence is in turn
> guaranteed by auto-start at sign-in + tray residency — § App Lifecycle). Authority:
> `installers/windows.md` § Device configuration. The shipped MSI carries the renamed
> `fauna-sync-agent.exe` artifact (landed 2026-07-22 — `ServiceSync.wxs`/`Package.wxs` stage
> it directly, no swap pending).

> **✅ Design resolved (ratified 2026-07-16; resolves the
> 2026-07-15 ⚠ Open design tension).** The USER requirement — *sync + folder/file
> badges must work regardless of whether FaunaApp is started after a Windows
> sign-in* — is satisfied by **shape A: keep the security model, make the app
> always-present**. The app-coupled agent statement above is **unchanged and stays
> ratified**: the agent never holds a renewable credential or the identity seed —
> the bearer-minting authority stays with the seed-holding app, preserving the
> blast-radius discipline of `key-material-hierarchy.md` (rule #7's spirit applied
> to the agent). What changes is the app's *presence*: FaunaApp becomes
> login-resident via **auto-start at sign-in (default ON, `--autostart`
> tray-resident hidden launch)** + **close-to-tray default ON** + the **persistent
> identity session** — mechanism and defaults in § App Lifecycle → *Auto-start at
> sign-in*. So "the app is running" stops being a user obligation and becomes
> OS wiring, the way OneDrive's sync host is always present. **Shape B (agent-held
> renewable credential) is rejected**: it would create a standing
> credential-at-rest theft surface in the agent for no capability the resident app
> doesn't already provide, reverse the ratified app-coupled statement, and require
> a security review no other track needs (fleet survey: no sibling
> touches the agent-credential surface). A future session with a concrete reason
> the app cannot be resident (e.g. a headless multi-user server SKU) should refute
> shape A here rather than quietly granting the agent a credential.

**Multi-root (N on-demand folders).** Landed: one cfapi sync root + one bearer-only
`SyncEngine` per folder, each `folder`-scoped with its own `fsid-<ref>.db` and a
shared `device.db`, multiplexed on one driving thread by the **shared
`fauna_sync_engine` multi-engine driver** (the same implementation the Linux in-process
app consumes), with cfapi callbacks routed by `CF_CONNECTION_KEY`. Each on-demand
folder carries a `folder` binding; an unbound location is logged and skipped. The
user-facing surface is `FoldersPage` (the folder control plane —
`../../ui/folders.md`). The full capability-handoff chain (Rust receiver, bearer-only
engine host, placeholder population, cfapi OS shim, and the app-side sender — now the
shared `FfiSyncAgentProvisioner` via `SyncAgentSession`, which retired the C#
`CapabilityProvisioner`/`HydrationSessionService` pair the ratified
session-scoped-provisioning note above still names (2026-08-05; current mechanism:
[`sync-agent.md`](sync-agent.md) § Implementation status today) — is landed; the handoff
is best-effort (on-demand is opt-in; an unreachable helper is a normal no-op).

⚠ **The cfapi population call itself was broken until 2026-07-13** — `CfExecute(TRANSFER_PLACEHOLDERS)`
failed `0x8007017C` on every input (placeholders carried an empty `FileIdentity`), so no
placeholder ever appeared and on-demand sync had never actually worked, despite the chain above
being complete. Fixed, and now pinned by a live cfapi test (`libs/fauna-cfapi/tests/live_population.rs`).
Authoritative status + the two cfapi gotchas worth knowing before touching this shim:
[`../../behavior/on-demand-files.md`](../../behavior/on-demand-files.md) § On-Demand Files (Placeholders).

---

## App Entry

- **Window sizing:** DPI-aware **minimum** of 600 DIP per axis (`MinWindowDip`, `App.xaml.cs`); size/position persist to `LocalSettings` on close.
- **CLI arguments:** `--reset` (wipe stored credentials; e2e), `--secret <hex>` / `--nest-url <url>` (launch-machine persistence overrides; win over the credential store), `--autostart` (tray-resident hidden launch; written into the `Run`-key value by the app itself — OS wiring, not a user-facing knob; § App Lifecycle → Auto-start at sign-in).
- **Startup sequence:** launch routing goes through the shared `fauna-launch-machine` (via `libs/fauna-ffi`), same as Linux/Android — `App.xaml.cs.OnLaunched` parses CLI overrides, constructs a `LaunchMachine` over the registry's `LaunchPersistence()`, `await machine.Start()`, and dispatches on `LaunchSnapshot.Phase` — `Online` opens the main view, `WizardAt{…}` lands the onboarding wizard at that step, `Offline{transient}` shows `LaunchRetryPage`. (The former C#-side pre-check — a saved pending-encryption-mode slot with no `nest_url` resuming the wizard at `encryption_mode_choice` — is retired with the page, not relocated: no-modes, ratified 2026-07-12; the `ISecretStore` slot it read is deleted, and `machine.Start()` alone now decides the phase, with a successful claim routing straight to `nat_mode_choice`, `onboarding.md` § 3b-bis.) `DirectNestClient` gets its bearer from the machine (`current_bearer()` / `refresh_token()`), with the self-acquire fallback minting over the shared FFI `FaunaFfiMethods.MintBearer` (`fauna.auth.handshake`); the legacy `POST /api/v1/auth/token` self-acquire was dropped when that route was deleted.
- **Auto-start:** **Default ON** (tri-state explicit user choice; an opt-out is never overridden), registered at the post-auth hook and toggled from Settings. Writes/removes the `Fauna` value in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. Mechanics and the rationale: § App Lifecycle → *Auto-start at sign-in*.

---

## Build & Deploy

### C# App (FaunaApp)

```
dotnet build apps/fauna-windows/FaunaApp/FaunaApp.sln
dotnet test apps/fauna-windows/FaunaApp/FaunaApp.sln
```

Requires: .NET 10 SDK, Windows App SDK 1.7 workload. (XAML builds need MSBuild,
not `dotnet build`, on win-arm64 — see the internal dev-setup notes.)

### Rust Services

Rust builds on Windows require the MSVC linker. Git Bash's `/usr/bin/link.exe` shadows it. Use the wrapper script:

```
cmd //c "scripts\cargo-win.cmd check -p fauna-sync-agent"
```


A `build` (as opposed to a focused `check`/`test --lib`) is heavier than a
focused build's two jobs and belongs to the machine-wide build-slot pool, so run it through `just windows-service-build <crate>` instead of
hand-spelling `cargo-win.cmd build` — it wraps the same invocation in the slot
and is freshness-gated outside it, so a no-op rebuild costs one stamp check:

```
just windows-service-build fauna-sync-agent
```

**Workspace membership:** there is **one** Cargo workspace. `fauna-ipc`,
`bins/fauna-sync-agent` (the sync agent, § IPC above),
the two services, `shell-ext`, and `shellext-fixture` are all members of the
**top-level** workspace — build every one from the repo root via `-p <crate>`, into the root
`target/`. The `apps/fauna-windows/Cargo.toml` sub-workspace was unified away 2026-07-22
(one committed `Cargo.lock`, so `--locked` works for the shipped service binaries); see
`build-target-layout-windows.md` § *Cargo target dir layout (win)* → *Two workspaces, twice the compiles*.

### Installer

WiX v6 MSI in `installer/`. Contains 9 `.wxs` files covering the app, services, shell extension, protocol handler, directory structure, UI, and package metadata, plus `wix.json` and `Strings.wxl`. There is **no** MSBuild `.wixproj` wrapper — the MSI is produced with the `wix build` CLI directly. Packaging authority: `installers/windows.md`.

### Store-safe flavor (payments excision)

windows is the sixth and last app-shell to gain the App-Store escape hatch
(payments excision, landed 2026-08-24) — the split's rationale, the shared
`store-safe` cargo-feature convention, and the sibling apps' equivalents are
owned by [`../dynamic-features.md`](../dynamic-features.md) § Platform-family
surface excision, not restated here. windows' condition is a C# `#define`
(`PAYMENTS`, positive and default-ON, set by all three csprojs and removed by
`/p:FaunaStoreSafe=true`); because XAML has no preprocessor, the define reaches
the glue only, and the §4/§5 provider + claim sections, the claim-redeem
input, and the per-card/detail tip surface (added 2026-08-30, closing a leak
where `FeedPage.xaml` painted the tip ids directly in the shared per-card
shell, outside the excision mechanism) live as `UserControl`s under
`Views/Payments/`, dropped from the store-safe csproj's `Page`/`Compile`
items via a wildcard glob (`Views\Payments\**\*.xaml`/`**\*.cs`), so the count
grows without a csproj edit — eleven today: the four above (the §§4–5 sections,
the claim-redeem panel, the tip display and its list dialog), the Nostr zap-signer
designation control (`NostrZapSignersSection`, 2026-09-06), and the six renders of
the price-and-route class (2026-10-08; the class is owned by
[`../dynamic-features.md`](../dynamic-features.md) § Platform-family surface
excision → *The price-and-route class*) — the tier editor's money fields, the tier
list's and the offer row's price, the offer's checkout link, the sell composer and
the sold-post teaser, which together retired the single asking-price input
(`AskingPriceInput`, 2026-09-06). Recipes: `just windows-ffi-store-safe`
(the dll), `just windows-store-safe` (the app), `just windows-store-safe-check`
(the two-column witness, scanning both the managed assembly and every compiled
`.xbf`, since WinUI puts markup beside the assembly, not inside it).

### Auto-Updates

`UpdateService` polls the GitHub Releases API (`faunasocial/fauna/releases/latest`), compares versions, and reports whether a newer version is available. **The one rule every desktop follows** (amended 2026-10-03): the user can trigger the check from Settings, and the app also looks ONCE per sign-in and only shows the same notice — through the shared `fauna_client::update_look::look_at_sign_in_over_http`, the entry point linux and tui call, never a copy of its own; a failed look is silent. No timer, no download, no self-replacement, no toggle. **Today windows has the asked check only, over a feed URL of its own;** the sign-in look is owed, and both are to call the shared FFI face `fauna-ffi`'s `version` module exports for them (2026-10-05): `check_for_newer_release(feed_origin, user_agent)` (newer with tag, bare version and release page / up to date / failed), `look_at_sign_in(feed_origin, user_agent)` (the newer release or nothing) and `release_feed_origin()` (production's origin — the e2e seam passes the stub's), so no C# keeps a URL or a round trip (tracked internally). The semver comparison is the shared `fauna_core::version::is_newer` via UniFFI (`FaunaFfiMethods.IsNewer`) — cross-language conformance pinned by `VersionCompareFfiTests`. The cross-platform promise this serves — and its desktop-only scope (owed on linux, windows, macOS and, once it has a channel, tui; absent by design on web, iOS and Android) — is owned by [`../installers/README.md`](../installers/README.md) § Knowing a newer version is out.

---

## P2P

No dedicated P2P page — `P2PPage`/`P2PViewModel` (WireGuard peer
registration) were deleted 2026-08-23 with the WireGuard stack; windows
hosts no peer node (`behavior/p2p.md`'s page-surface table). The offline
co-present share ceremony (a nest-free, iroh-direct two-party ceremony,
landed 2026-08-27) lives on the Folders page instead —
`FoldersPage.xaml.cs`'s `RenderOfflineShare`/`OpenOfflineSharePanelAsync`
over `INestRpcClient`'s five ceremony doors + the pure `FaunaFfiMethods`
reads called directly off the page. Layer authority: `behavior/p2p.md` §
Offline share initiation.

---

## Notifications

Toast notifications via `NotificationService` (`AppNotificationManager`); OS
notifications are fed by **WS-RPC push** (re-homed off the removed
`WebSocketService`). With the app closed, the per-user sync agent posts the
push toast instead — the `ws-device` transport ([`common.md`](common.md)
§ Push Notifications → *Transports*, ruled 2026-09-26; WNS was considered and
rejected there): the agent posts only while no app is attached over the IPC
seam, so the two never both fire.

**The app's half (built 2026-10-08).** The Settings → Account push toggle
(`PushNotificationsViewModel` over the shared registration machine through
`FfiPushRegistration`; `Core/Services/PushSession.cs` names the inputs — the
install's derived device id for the actor and the install-scoped intent file
`push-intent.cbor` in the data dir); every session start (`StartMainAppAsync`,
and the e2e `session` login) announces the device and re-arms; a committed
switch drops the outgoing actor's row inside `TearDownAndRelaunchAsync` and a
sign-out drops it before the credential erase, each bounded at 3 s. From its
first session the app holds the agent's attachment lease for the process's life
(`FaunaFfiMethods.AttachToSyncAgent` — the shared `fauna_client_sync::attachment`,
gated like the provision path), and the attach names **the AUMID its own toasts
post under** (`NotificationService.Identity`): the package app's AUMID with
package identity (the Store package, or the MSI's sparse identity), otherwise
the unpackaged registration `AppNotificationManager.Register()` keeps under
`HKCU\Software\Classes\AppUserModelId` (a key named for the exe's path whose
`NotificationGUID` names the AUMID — the SDK's layout, measured 2026-10-08, not
a documented contract, so a read that finds nothing sends no identity and the
agent honestly reports no sink).

**The agent's half (built 2026-10-08).** `bins/fauna-sync-agent/src/push_arm.rs`'s
`toast` sink posts the frame's title and body as a WinRT toast under the
identity the app named, which it keeps in a file under its flat base so a toast
still finds it after an agent restart with the app closed. The toast is
therefore the app's — its name, its icon, its group in the notification centre —
and its registration's activator is what a tap would launch. The sink is
available when the platform's own `ToastNotifier.Setting` reads enabled for that
identity; an unpackaged identity the platform has not met yet (nothing posted
under it) answers "not found" until its first toast, so there the documented
unpackaged registration key decides (measured 2026-10-08: a packaged identity
reads enabled from its registration on, and an unpackaged process posts under
it). Pinned against the real notification platform by `push_arm.rs`'s
`toast_tests` (a per-run registered identity; popup suppressed, the toast read
back from the identity's history and removed).

**Not yet witnessed:** a tap on an agent toast launching the app (the
`common.md` status line's cross-platform "tapped banner" gap — the activator is
the app's own registration's, untested here), and an end-to-end frame from a
live nest reaching a toast through the agent process. System tray icon via Win32 `Shell_NotifyIconW`;
window-close behavior follows the user's **Close to tray** setting (§ App
Lifecycle). The notifications page reads `fauna.notifications.*`
(`behavior/notifications.md`).

---

## Home-screen widget

**The surface (design pass 2026-09-26): the taskbar badge.** The cross-app promise ([`common.md`](common.md) § Home-screen widget) is a glanceable unread count outside the app, kept current in the background, and windows has no one host for it, so three candidates were weighed against both shipping channels ([`../installers/windows.md`](../installers/windows.md) — the MSI with its sparse identity companion, and the full-MSIX Store package). **(1) The Windows 11 Widgets board** is the platform's literal widget host, but "in the current release, only packaged apps can be registered as widget providers" (Microsoft's provider walkthrough) — a full MSIX, which only the Store channel is; it exists on Windows 11 22H2+ only and is a user-disableable optional feature; and a provider is a COM exe server the board activates *out of process, on demand* — a second, headless lifecycle of FaunaApp that would have to reach the account's conversations fold with no tray-resident session behind it. Rich, Store-only, heavy: not the first surface, and a possible later addition (a richer glance) rather than anything the promise still owes once the badge exists. **(2) The taskbar overlay icon** (`ITaskbarList3::SetOverlayIcon`) needs no identity but paints only while the window has a taskbar button — hidden to the tray it is gone, so it is not "outside the app". **(3) Badge notifications** — `BadgeUpdateManager.CreateBadgeUpdaterForApplication()` with the numeric `<badge value="N"/>` — are what Windows paints as the number on an app's taskbar icon "regardless of whether the app is running" (1–99, then "99+"; 0 clears), on Windows 10 and 11, on both channels: the exact twin of linux's launcher badge (the same concept, a number on the launcher icon, fed by the same computation), which the user ratified as that platform's widget. **Chosen.**

**The number is the shared fold, read on every change.** `ConversationsManager.unread_total()` — `fauna_conversations::sum_unread` over *every* thread of the account, not the search-narrowed snapshot list — is the one getter every outside-the-app surface reads (linux's launcher badge and tray tooltip too, since the 2026-09-26 lift), so the badge can never show a count the app would not; what "unread" means is owned by [`../../ui/conversations.md`](../../ui/conversations.md) § State & data shape → *When a thread is read*. `UnreadBadgeObserver` (a `SnapshotObserver` registered once per manager inside `ConversationsManagerHost`'s factory, beside `RoomsRefreshObserver`, so it lives with no `ConversationsPage` open and an actor change's fresh manager gets a fresh one) reads it on the thread pool after each notify — never inline on the mutator's thread — and hands it to `TaskbarBadgeService`, whose decision half is the pure, unit-tested `TaskbarBadgePolicy` / `TaskbarBadgeTracker` pair in `FaunaApp.Core`: zero clears rather than painting a "0" (linux's `count-visible` goes false at zero for the same reason), values above 99 pass through unclamped (the OS owns the "99+" glyph), only a *changed* total is pushed, and the first observation always pushes so a badge the OS kept from a previous run is reconciled to this session's truth.

**Identity — the per-channel condition, stated as a limit, not an absence.** Badge notifications are keyed on the app's AUMID, so a process without package identity gets `0x80070490` from the updater and paints nothing; `TaskbarBadgeService` logs that once (`[badge]`) and stops pushing, and nothing else changes. The **Store package** is packaged, so its FaunaApp always has identity. The **MSI channel's** FaunaApp.exe has it only through the sparse identity package the MSI registers with the Explorer Integration feature (default ON) *and* the `msix` element `FaunaApp/app.manifest.in` carries (its publisher generated at build time from the one home, `installer/PackageIdentity.props`), which binds the exe to that package — measured 2026-09-26: without the element a directly launched exe under a registered external-location package has no identity at all ([`../installers/windows.md`](../installers/windows.md) § Package identity for FaunaApp.exe owns the packaging half: the publisher DN's one build-time home, and the Start-shortcut question still open there). An MSI install with Explorer Integration deselected therefore paints no badge: a real limit of that one install shape, recorded here rather than as a catalog absence, exactly as linux records its stock-GNOME desktop ([`../feature-catalog.md`](../feature-catalog.md) § Closing a gap — an `absences` entry is a per-app deviation, and the windows app *has* the behaviour).

**Background currency — how "without the app being opened" holds on windows.** The OS keeps the last badge on the app's taskbar button across a window close and even a process exit, until the next update; the resident app supplies the updates. Residency is what windows already owes for sync and toasts (§ App Lifecycle): auto-start at sign-in and close-to-tray both default ON, so Fauna is resident from every sign-in with its WS-RPC subscription live, and the observer fires whether the window is mapped or hidden. A deliberate tray **Quit** leaves the badge at its last count until the next launch — stale but never invented, the same conditional linux states for a desktop with no tray host.

**Witness.** `tests/e2e-unified/tests/test_home_screen_widget_taskbar_badge.py::test_taskbar_badge_shows_the_unread_count_and_moves_while_hidden` (tier_3, marked `windows`): lends the bare build output the sparse identity package for the test's duration (`helpers/taskbar_badge.py` — `Add-AppxPackage -Register` on the retargeted sparse manifest, non-elevated under Developer Mode, the Store-registration test's path; it steps aside as an environment skip when a real registration of that identity is on the box), then reads the badge the OS notification platform holds on record for the app's AUMID — the per-user `wpndatabase.db` store the taskbar paints from, a `badge` row keyed on `<PackageFamilyName>!FaunaApp` — checks it against the app's own thread list, closes the window (hidden, process alive) and reads the badge move on a second planted message. Cited on the catalog page's outcomes 1 and 2 for the windows column. What no test reads is the last inch — the pixel on the taskbar button — a human's glance.

**Implementation status today (2026-09-26): BUILT and witnessed — the observer, the policy pair and its unit tests, the WinRT service, the `msix` manifest element with the bootstrapper's identity no-op ([`../installers/windows.md`](../installers/windows.md) § Package identity for FaunaApp.exe), the witness, green on Windows (the catalog's windows column reads full).** The Store package's badge is witnessed too (2026-09-28, `tests/real_session/test_store_package_taskbar_badge.py`, green on Windows: the app started in a registered Store-shape package's context badges `<PFN>!FaunaApp`). Unverified by construction on this box: a real MSI install's identity path (the sparse package's registration is the elevated install step § Shell Extension already awaits), and whether a pin made from the MSI's classic Start shortcut shows the packaged badge (the shortcut-AUMID question, owned by the installer doc).

---

## App Lifecycle

The Windows app follows the same lifecycle model as the Linux app (`linux.md` § System Tray), adapted to Win32/WinUI. Three invariants — single-instance, no-close-loses-state, and cooperative shutdown — make a windowless tray-resident app safe and stop the installer from ever needing to force-kill a healthy app.

**Auto-start at sign-in (default ON — ratified 2026-07-16, the shape-A resolution of § Per-user agent's design tension).** FaunaApp registers itself in the per-user `HKCU\…\CurrentVersion\Run` key as `"<exe>" --autostart` at the universal post-auth hook (`StartMainAppAsync` — every login and returning-user relaunch funnels through it), so after the *first* successful login the app is present at every subsequent Windows sign-in with no manual step — which is what keeps the app-coupled sync agent provisioned (capability handoff + bearer TTL refresh) and the shell-ext badges live. The registration is a **tri-state user choice** (`AppSettingsStore.AutoStartChoice`: unset ⇒ register by default; explicitly off ⇒ never re-register; explicitly on ⇒ register), surfaced as the existing `settings-autostart-toggle` on Settings → General — an explicit opt-out is never overridden (app UI is the only config surface, per `principles.md`). Re-registering on each login with the current exe path self-heals a moved/upgraded install. Gated OFF under the E2E bridge (`FAUNA_E2E_BRIDGE`; policy class `AutoStartGate`, unit-tested) so harness runs never write the machine's real Run key. **`--autostart` launches tray-resident (hidden):** the launch machine, agent spawn/provision, and toast wiring all run exactly as a visible launch (they are code-driven in `App.xaml.cs`, not render-driven), but the window is not activated when routing lands `Online`; the tray icon (double-click / Open) or any second launch (the single-instance redirect) surfaces it. A `--autostart` launch that routes to *onboarding* (no persisted session) **shows the window** — a signed-out auto-start must be loud, not a silently dead agent. The linux leg (default-on autostart `.desktop` + hidden launch equivalent) shipped the same day (`linux.md` § Auto-start at sign-in); macOS's leg was decided 2026-09-26 — default-on, the same tri-state choice and the same hidden launch — and is owned, with its build state, by [`macos.md`](macos.md) § App Lifecycle → *Auto-start at sign-in*.

**Session persistence is load-bearing for this shape.** Auto-start only delivers the requirement if the identity session survives the restart (`SecretStoreLaunchPersistence` routing off the credential store). The `SaveCachedHandle` empty-value store bug is fixed (2026-07-14); the full "drops back to onboarding" symptom still owes one live re-verify on a real claim (tracked internally).

**Single instance is (OS login, account)-scoped, not per-login (owner: [`account-scoping.md`](account-scoping.md) § Concurrent instances; windows re-keyed 2026-07-23, **retired its same-account refusal 2026-08-24**).** Any number of accounts may run concurrently, each its own process — and since the retirement leg, any number of instances of the *same* account too: `SessionInstance.BecomeUnder` passes `FfiServingMode.Concurrent`, so serving takes a **shared** per-account lock and exclusivity survives only in the three genuinely exclusive critical sections. The pre-existing mutex (`SingleInstanceManager`; policy in the unit-tested `SingleInstanceGate`) is **kept**, not replaced — it is a fixed app-wide name (`Local\FaunaApp-SingleInstance`), was never re-keyed per account, and stays in force purely as the plain-launch *raise* layer (a second unbound launch still signals the running instance to surface its window, then exits itself, never terminating the running instance), while the shared `AccountInstanceLock` is the only guard that ever decided whether a second *same-account* session may run; a bound launch skips the mutex outright and is guarded only by that lock. The lock's refusal is not dead code: it still fires against an **exclusive** holder — a pre-retirement windows binary serving the account, the mixed pair degenerating to the stricter law — and the bound-mismatch refusal is mode-independent. Both are **disabled under the E2E bridge** (`FAUNA_E2E_BRIDGE`), where each launch must be its own process. **The launch-collision chooser is wired on windows** (landed the same day as the account-scoped lock — `App.OnLaunched` detects a collision via `LaunchCollisionGate.CollidesWithALiveInstance` *before* `SingleInstanceManager.TryClaim()`, since a collision decided after the mutex signals-and-exits is never decided at all; windows is the second app, after linux, to render the chooser and offer the spawn button).

**Window close = the "Close to tray" setting (default ON — flipped 2026-07-16 with the shape-A ratification; was opt-in).** A persisted `AppSettingsStore.CloseToTray` (`%LocalAppData%\Fauna\app-settings.json`) gates the `TrayIconService` close handler — on (the default) ⇒ hide to tray and stay resident, keeping sync/badges/toasts alive; off ⇒ persist drafts then quit. Rationale: with the app as the agent's bearer-minting service (§ Per-user agent), closing a *window* must not silently stop file sync — a deliberate stop is the tray **Quit** (the cross-app never-a-silent-stop promise is owned by [`common.md`](common.md) § Desktop Residency since 2026-08-30; this paragraph keeps windows' mechanism). An explicitly persisted `false` from before the flip is an explicit choice and is honoured. Toggle on Settings → General (`close-to-tray-toggle`, the shared linux+windows `platform_element`). The linux default flip shipped alongside the autostart leg (linux additionally gates close-to-tray on a live tray host — `linux.md` § System Tray — which the flip preserves).

**No close path leaves unsaved state — windows' leg of the cross-app leave-flush promise, and its worked example ([`../../behavior/reserved-folders.md`](../../behavior/reserved-folders.md) § The leave-flush promise owns the promise since 2026-08-30; this paragraph keeps only windows' mechanism).** Every termination path — tray Quit, a Restart-Manager/OS shutdown request, or the single-instance hand-off — first persists in-progress compose input (`TrayIconService.QuitApplication()`: `ConvDrafts.SaveNowAsync()` + `FeedDrafts.FlushIfPendingAsync()`; the `RestartManagerService` end-session twin). **Both rails are nest-backed, cross-device now (draft-persistence v2, 2026-08-26).** DM-thread drafts ride the shared `fauna_client_drafts::DraftsSync` gate via `ConversationDraftsService`; feed-post drafts ride the same gate via `FeedDraftsService` (rail `"posts"`) — each manager's draft set is restored into it at login (feed: at every Feed-page load, since `FfiFeedManager` rebuilds per visit, unlike the session-scoped conversations manager) and saved (debounced, plus a flush on navigating away from the page and at quit), matching web/linux/android/apple. Neither rail touches the local `drafts.json` file any more — the `FaunaApp.Core` `DraftStore` + `DraftPersistenceService` classes that captured/restored it are deleted; `AccountStateDir.DraftsPath`, kept only to adopt and erase a pre-existing flat file, went with the flat-layout adoption in the compat-remnant sweep (2026-09-24, [`../version-compatibility.md`](../version-compatibility.md) § Dimension 2, the fourth ratified exception). No local-file compose-draft mechanism remains. Drafts Sync mechanics: [`../../behavior/reserved-folders.md`](../../behavior/reserved-folders.md) § Drafts Sync.

**Cooperative shutdown for installers.** `RestartManagerService` registers `RegisterApplicationRestart` and runs a persistent hidden top-level window that receives `WM_QUERYENDSESSION`/`WM_ENDSESSION`, persists drafts, and quits gracefully — so an MSI install/upgrade closes (and relaunches) the app without force-killing; the installer's `taskkill` is a last-resort net. Policy is the unit-tested `RestartManagerGate`; the Win32 mechanism is gated off under `FAUNA_E2E_BRIDGE` and manual-verified.

---

## Content Moderation

Built, over the shared seams: `ModerationViewModel` runs the server∪local
moderation queue — the server rows plus the conversations session's retained
post-decrypt **local detections** (`IModerationLocalDetections`, from
`uniffi.fauna_client_moderation`) — rendered by `ModerationPage`, with
`MutedKeywordsCache`/`MutedKeywordsPreloader` for the muted-words surface and
`ISpamModelClientWrite` for spam-model training. Behavior owner:
`behavior/moderation.md`; windows keeps glue only.

---

## Implementation status today

- **Declared absences (owner [`../../behavior/family-safety.md`](../../behavior/family-safety.md) § The account age band):** no store age signal and therefore no `invite-request-age-notice` (D3 — at most the two store-distributed mobile apps ever receive one; the id is `platform_elements: {android, ios}` in ui.yaml), and no kids flavor (the kids-app bullet: the kids door is a store construct, android and ios only). The e2e gate for both is `declared_absence`, never `skip_unbuilt`.
- **Two-plane transport is the shipped shape** (§ Architecture): `NestRpcClient`
  over `FfiNestClient` for migrated clusters (~107 consuming files in FaunaApp,
  up from ~100 as more clusters migrated), `DirectNestClient` for the remainder
  (~10 consuming files). The bridges HTTP→WS-RPC swap is **done**
  (`BridgesViewModel` drives `INestRpcClient.BridgesListAsync`).
- **Admin hub, moderation pipeline, C2PA render** (`BlobImageLoader`,
  `DmMessageBubble`), **email-alias UI** (`SettingsMailAliasesPage` +
  `MailAliasesPanel`), and the settings shell are built (§ Views).
- **Per-user sync agent** implemented + tier_3 e2e-guarded; the shipped MSI
  carries the renamed `fauna-sync-agent.exe` artifact directly (landed
  2026-07-22, § IPC above) — no `dist`-rebuild gap remains.
- **Auto-start at sign-in + tray residency (shape A)**: implemented 2026-07-16
  (`AutoStartService`/`AutoStartGate`, `--autostart` hidden launch, close-to-tray
  default ON). The **conditional**-residency half of § App Lifecycle → *Auto-start
  at sign-in* — hidden when routing lands `Online`, window **shown** when it lands
  in onboarding — shipped untested and is **headlessly covered since 2026-08-10**
  by `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py` case J: one
  case, both arms, each read through two independent witnesses (the app's own
  published activation decision in `state["launch"]`, and the OS's top-level-window
  list via the FlaUI bridge's `/session/windows`). The second witness is not
  belt-and-braces — a red-verify with an activation injected *after* the gate found
  the self-report still honest and only the OS view red. Open: the live "session
  survives a real relaunch on example.com" re-verify, and the linux advisory legs
  (autostart default + close-to-tray default) — both tracked in the respective
  NEXT files.
- **Shell extension:** COM surfaces + status producer + engine-driven end-to-end
  (headless) done; file-level live-Explorer verification is also done for both
  badges (2026-07-14) and the context menu (2026-07-15) — corrected 2026-07-20,
  this bullet previously read both as still open. The folder-specific context-menu
  render is also now done (2026-07-22, § Shell Extension matrix → *Folder context
  menu*). What remains: the folder-specific live-Explorer render pass for
  **badges** (folder badges are built + integration-tested headless, but not
  separately eyeballed in Explorer) and the version-history submenu's
  live-Explorer render (§ Shell Extension matrix).
- **Remaining genuine gaps:** launch-flow
  follow-ups (the `DirectNestClient` 401-retry chokepoint; the former
  `setup-status.mode == None` resume path is moot — the `mode` field left the
  wire 2026-09-24) tracked internally; draft-surface
  coverage + the cross-app `DraftStore` lift (§ App Lifecycle). (The standalone
  Nostr page — link, toggles, relays, follows — shipped 2026-07-29, and its
  *Connected apps* / NIP-46 bunker section shipped 2026-07-30, closing the
  last "windows absent" row on all 7 apps; `ui/nostr.md` § Implementation
  status today. The *Connected apps* roster half has since moved to the
  Connected apps settings page, built on windows 2026-10-03 with the
  `fauna://consent` route's intake — § Shell Extension → *The Share hand-off*,
  step 4; `ui/connected-apps.md` § Implementation status today.)

Cross-app comparisons live in [`common.md`](common.md)'s platform tables —
this doc deliberately carries no per-platform difference matrix (the previous
one had drifted against common.md).
