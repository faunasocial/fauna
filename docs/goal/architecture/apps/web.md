# Web App — target state

Owns: web
Status: ratified — as-built architecture; remaining gaps declared in § Implementation status today
Authority: web-app architecture — the Svelte 5 SPA shell (routes, Svelte stores), the WASM chunk inventory + `ensure*Wasm()` loader discipline, the `WsRpcClient` singleton + web-side persistence (localStorage/IndexedDB), PWA/service-worker push glue, and the Deno + Vite build; cross-app behavior (auth ceremony, MLS semantics, content APIs) → [`common.md`](common.md); chunk build gating → [`../build-system.md`](../build-system.md) § WASM chunking convention; C2PA badge semantics → [`../../ui/media.md`](../../ui/media.md); element IDs / per-page scope → `tests/e2e-unified/ui.yaml`.

Last verified: 2026-08-26 (docs-consistency sweep — re-check since the 2026-08-20 sweep, 113-commit gap, 2 fixes: § API Layer's nostr caller claim was wrong, not stale — of `publish-signed`/`badges`/`zap-total` only `zap-total` was dead, `nostr.ts`'s two other exports have zero live UI callers either since the `/nostr` page's NIP-07 publish UI was removed when bridge management unified onto `/bridges`; and § State Management's localStorage table was incomplete — added the missing `fauna-push-subscribed`/`fauna-push-subscribed-actor`/`fauna-push-device-id` rows. Re-verified against current code: the Route Structure table (30 `+page.svelte` files), the WASM crate/chunk inventory (ten cdylib crates / nine built chunks, `fauna-wasm-panic-hook` correctly excluded), all ten `ensure*Wasm()` loaders, the `WsRpcClient` singleton + token cache, the conversations dual-rail receive path, MLS/moderation retirement, push kinds, C2PA glue, and the build commands — all held. No code bugs, no NEEDS-RULING. Re-confirmed CLEAN, zero drift, sweep (2026-09-08, 134-commit gap) — no new route/chunk/localStorage key added since; the two previously-flagged code-debt items (`nostr.ts`'s dead exports, a stale routing comment in `NostrSettingsSection.svelte`) are unchanged. Re-confirmed CLEAN, zero drift, sweep (2026-09-16, ~81-commit gap touching `apps/fauna-web/`/`libs/fauna-wasm*/` since sweep's anchor) — Route Structure re-counted directly off the filesystem (30 `+page.svelte` files, table breakdown still sums to 29 + root), the `WsRpcClient` singleton/`retireClient`/`getClient` shape, the per-(node,identity) token cache, the localStorage keys table (incl. `fauna-push-subscribed`/`-actor`/`-device-id` exact spellings), the `fauna_tab_account` per-tab pin correctly living in `sessionStorage` not this table, the conversations dual-rail receive path (`drainInbox`+`pollConversations`+HPKE), the retired `ws.ts`/`moderation.ts`/standalone-MLS-engine claims, the nostr WS-RPC migration + zero-live-caller claims, the settings sub-page list, and the five-builder singleton-memo census — all re-verified against current code, all held. No code bugs, no drift) | Source: `apps/fauna-web/`, `libs/fauna-wasm*/`

## Goal

A static Svelte 5 SPA, served under `/app/`, that drives every fauna feature through lazily-loaded sibling WASM chunks and a single shared WS-RPC client plane (`WsRpcClient` + typed wasm machines). MLS runs in WASM inside the conversations manager's one engine, kept multi-tab-safe through the nest-side state replica. The build runs on Deno (no Node.js) via Vite + `@sveltejs/adapter-static`.

---

## Tech Stack

- **Framework:** SvelteKit + Vite, Svelte 5 with TypeScript
- **Runtime:** Deno (no Node.js) — `deno task dev / build / check`
- **Deployment:** `@sveltejs/adapter-static` — fully static SPA served under `/app/`
- **PWA:** `static/manifest.json` — `display: standalone`, `scope: /app/`

---

## WASM Integration

There are **eleven** `libs/fauna-wasm*` cdylib crates. Ten build as chunks
today (the justfile `wasm:` umbrella); each built chunk has its own JS module
and its own idempotent `ensure*Wasm()` entry point in `apps/fauna-web/src/lib/`
— save `fauna-wasm-share`, which no app page loads: the private share link
viewer, a separate page outside the app shell, is its only importer.

| Crate | Wrapper | Role | Status |
|-------|---------|------|--------|
| `fauna-wasm` (core) | `lib/wasm.ts` | identity, auth signing, posts, the typed machine classes (`WsRpcClient`, conversations/feed/mail/media/devices managers), classification, file chunking | chunk, loaded on first authenticated action |
| `fauna-wasm-onboarding` | `lib/wasm-onboarding.ts` | OnboardingMachine, provisioning, registrar, provider verification | chunk, loaded on `/onboarding` |
| `fauna-wasm-folders` | `lib/wasm-folders.ts` | folder control plane | chunk |
| `fauna-wasm-media` | `lib/wasm-media.ts` | media explorer machine + thumbnail fetch | chunk |
| `fauna-wasm-labeler-catalog` | `lib/wasm-labeler-catalog.ts` | labeler catalog machine | chunk |
| `fauna-wasm-share` | `lib/share-viewer/main.ts` | the private share link viewer's open + decrypt + verify (`fauna_client_share::viewer`, [`../../behavior/share-links.md`](../../behavior/share-links.md) § The private-file extension) | chunk, loaded only by the viewer page `share-viewer.html` — the SPA build's second entry, outside the app shell's root layout |
| `fauna-wasm-connected-apps` | `lib/wasm-connected-apps.ts` | connected-apps machine | chunk |
| `fauna-wasm-launch` | `lib/wasm-launch.ts` | `LaunchMachine` bindings driving every app-launch row | chunk, loaded on `/onboarding` (app-launch routing) |
| `fauna-wasm-atproto-settings` | `lib/wasm-atproto-settings.ts` | `AtprotoSettingsMachine` — the ATProto page-level settings machine (app credentials, connected-app sessions, the external-apps kill-switch) | chunk, loaded on the `atproto` settings sub-page |
| `fauna-wasm-backups` | `lib/wasm-backups.ts` | `BackupsMachine` — the Backups page's snapshot-half machine, built over the core chunk's socket through the shared rpc port (§ Transport) | chunk, loaded on `/backups` |
| `fauna-wasm-content-index` | `lib/wasm-content-index.ts` | client-side content index | **on hold** — wrapper exists, chunk excluded from the umbrella |

**Discipline (load-bearing):** no wasm-bindgen object crosses chunk boundaries;
cross-chunk APIs are pure data in / out (`Uint8Array`, hex strings, JSON), and
each chunk's constructor lives in the module that holds its singleton. Wrappers
do NOT share wasm-bindgen instances or memory — that would break under Vite's
chunk splitting, since each chunk gets its own independently-instantiated wasm
module. When a subsystem earns its own chunk, follow
[`../build-system.md`](../build-system.md) § WASM chunking convention for the
recipe/gating/Dockerfile obligations — do not re-derive them here.

**Two things the core chunk owns are lent to the other chunks through typed
ports, as pure data.** Its socket, through the shared rpc port (§ Transport).
And its account runtime, through the account port (`SharedAccountPort`, built
2026-09-30 with the Devices page's fleet door): a chunk whose shared Rust needs
the account store takes that port beside its `SharedRpcPort` and never hosts a
runtime of its own. The shape, what may cross, and the build status are owned by
[`../account-client-lifecycle.md`](../account-client-lifecycle.md) § The
client-side lifecycle → *The account port*.

**Every `ensure*Wasm()` memoizes the in-flight init PROMISE, not just the
result.** `wasm.ts::
ensureWasm`'s own doc comment states why: two concurrent callers must `await`
the SAME in-flight init, so the chunk's `mod.default()` runs exactly once — a
second concurrent call re-running it resets that chunk's wasm linear memory,
silently wiping any module statics a peer caller had already seeded (worst
case: `wasm-atproto-settings.ts`'s critical-alerts registry, feeder #1 of
`../../behavior/critical-alerts.md` § Mechanism). A bare `if (wasmModule)
return` guard is NOT sufficient — two callers entering before either assigns
`wasmModule` both pass it. The shape (mirror verbatim, do not re-derive):
`if (wasmModule) return Promise.resolve(); if (!wasmInit) { wasmInit =
(async () => { … })().catch((e) => { wasmInit = null; throw e; }); } return
wasmInit;` — the `.catch` reset arm is required too, so a transient failure
(chunk-fetch error) doesn't permanently cache a rejection. All ten wrapper
files in the table above follow this; `wasm-loader-guard.test.ts` mechanically
asserts every one still does.

**The reset-on-failure discipline applies to every memo layered on top of a
loader's own `wasmInit`, not just `wasmInit` itself.** `wasm-onnx.ts`'s
`ensureOnnxReady()` memoizes a *second*, onnx-specific `Promise<boolean>`
(wasm init + model fetch combined) on top of `ensureOnnxWasm`'s own guard; its
`catch` arm resolved `false` without clearing that second memo, so one
transient model-fetch failure permanently zeroed spam scoring for the rest of
the page load. Fixed to clear `onnxReady` on the same `catch`, pinned by
`wasm-onnx-ready.test.ts` (loader and test both retired with the ONNX scorer
2026-10-02 — the lesson stands). Any future downstream memo built on an
`ensure*Wasm()` loader needs its own reset arm — the base loader's guard does
not protect a caller's own layer.

Per-chunk export inventories are deliberately not listed (they re-rot on every
export change); each wrapper file is its own inventory.

---

## Transport: the WS-RPC plane (`src/lib/rpc.ts`)

`rpc.ts` owns the singleton browser **`WsRpcClient`** (a typed wasm class from
the core chunk): one WebSocket per actor with the bearer in
`Sec-WebSocket-Protocol: fauna.v1, bearer.<token>`, request/reply plus typed
push events, reconnect/refresh handled inside the shared Rust client. The page
layer talks to typed wasm machines (`WasmMailSettingsMachine`,
`WasmConversationsManager`, `WasmFeedManager`, …) imported through `rpc.ts` —
new features go through this plane, never a hand-rolled socket.

The legacy `src/lib/ws.ts` `WsConnection` — a push-only migration remnant using
the retired `?token=` query-param form, whose sole consumer was
`routes/events/+page.svelte` — is **fully deleted** (2026-07-12):
`events/+page.svelte` now consumes the shared push stream via `onPushEvent()`
(`rpc.ts`) like every other page. There is no second socket anywhere in the
SPA; do not reintroduce one.

**The page-machine chunks ride that same socket through the shared rpc port
(built 2026-09-25).** A chunk cannot hold the core
chunk's `WsRpcClient` (no wasm-bindgen object crosses a chunk boundary, § WASM
Integration), so until then `fauna-wasm-folders` (the Devices/Folders machine
and the folder wizard), `-media`, `-backups`, `-labeler-catalog` and
`-atproto-settings` each took the `(nestUrl, actorIdHex, tokenProvider)`
triple and dialled a `WsRpcClient` of their own — six sockets beside the
singleton's, each with its own reconnect loop, so after a nest restart the
`connection-status` indicator could read online while a page's machine was
still asleep in its own backoff. Now `rpc.ts::sharedRpcPort(secretHex)` is
the ONE implementation of the typed `SharedRpcPort` those chunks declare
(`fauna_rpc_wasm::shared_port`, emitted into every chunk's `.d.ts` — so each
constructor is typed `port: SharedRpcPort`, never `any`), every chunk
constructor takes exactly it, and only pure data crosses: kind strings,
canonical-CBOR `Uint8Array`s, the bearer string. The port resolves the
CURRENT singleton on every call (`getClient(secretHex)` inside `request`),
so a machine that outlives an identity/nest swap — the devices session memo,
the atproto singleton — follows the socket instead of holding a retired one;
`nestUrl()` reads the wasm client's live (SRV-swapped) value, and the core's
`WsRpcClient.requestRaw` runs the request with the same reconnect-wait and
per-kind deadline as any of its own. The rule from here: **a chunk that talks
to the nest takes a `SharedRpcPort`; nothing outside `rpc.ts` constructs a
`WsRpcClient`** (the pairing seam's peer-nest client, built in Rust, is the one
exception — a second nest). Pinned by `shared-rpc-port-contract.test.ts`
(`just web-unit-test`): no `lib/wasm-*.ts` loader takes a `tokenProvider`,
every nest-facing loader types its transport as `SharedRpcPort`, and the port
resolves the live singleton per request. The transport-level statement is
`../transport.md` § Design decisions; the loop-level consequence
`../transport-connection.md` § Connection lifecycle.

---

## Conversations (`src/lib/conversations.ts`)

The browser twin of linux's `src/conversations/`: a process-wide shared-wasm
**`ConversationsManager`** singleton carrying BOTH rails — SMTP send/receive
and FaunaMls E2E DMs — with app-wide receive polls (`drainInbox` +
`pollConversations` + `fauna.email.inbox.fetch` HPKE decrypt) and the e2e
command hook. Page behavior → [`../../ui/conversations.md`](../../ui/conversations.md);
protocol → [`../../behavior/direct-messages.md`](../../behavior/direct-messages.md).

## MLS

The conversations manager's engine is the **one** web MLS engine — multi-tab-safe
via the nest-side state replica + CAS, and the only minter of key packages. The
earlier standalone engine plane (`mlsInitEngine`, IndexedDB `fauna_mls`, the
`fauna_mls_active_tab` single-tab lock, the `beforeunload` localStorage backup)
was **retired** with slice 6 of the cross-device MLS state-sync work — do not
rebuild it; a second engine's init keys are invisible to the Welcome-processing
engine. Mechanism owner: [`../../behavior/devices.md`](../../behavior/devices.md)
§ Cross-device MLS group-state sync.

---

## State Management

### Svelte Stores (`src/lib/store.ts`)

| Store | Contents |
|-------|----------|
| `identity` | Secret key, actorId, handle, domain, tier (`types.ts::Identity` — no `registered` field; derived from `handle != null` where needed) |
| `inbox` / `sent` | Decoded email/messages |
| `groups` | Joined groups |
| `knocks` / `contacts` | Contact edges |
| `reconnectTick` / `connectionStatus` | Connection-plane signals |

Feed state lives in the `WasmFeedManager` plane, not in stores.

### Async manager readiness — never silently drop a user action

Web is the one app whose page managers arrive **async after mount** (the wasm
module and the manager build in the browser), so a managed page has a window in
which its `manager` is null and a `manager?.` call no-ops. A user action landing
in that window must be **refused visibly** — a disabled control (the
conversations "+" and the feed composer/create gates), or the page's
error-message surface set to `t.common.still_loading` — never silently dropped:
the human half of the e2e test-agent contract ("never silently drop a command",
convention 11), applied to real users. The conversations page enforces this at
its one `run()` chokepoint; the feed page (no chokepoint) surfaces the refusal
at each handler's null-manager guard, with genuine non-user-action ordering
guards (an `$effect`, a `$derived.by` read, a subscription callback) annotated
`// manager-gate-ok: <reason>`. Enforced over the source by
`src/lib/manager-gate-contract.test.ts` (`just web-unit-test`), the sibling of
the feed refresh contract below. Native apps are outside this rule by
construction — they build managers synchronously at session attach, so the null
window does not exist. (Ratified 2026-08-22, closing the open class; the measured cost of the silent shape was the
conversations "+" discarding clicks for the ~1 s manager build — 5 red e2e
tests across 3 files misread as "environmental".)

**The guard is judged by its REMEDY, not by its spelling** (2026-08-22, second
pass). The rule binds any `!manager` guard whose whole body is a `return` —
**the condition's shape is irrelevant**, so a compound guard
(`if (!manager || !id || !composeBody.trim()) return;`) carries exactly the same
duty as a bare one. The first enforcement pass matched only the bare spelling
and so declared the class closed while five compound guards stood in the feed
page, three of them genuinely reachable: `submitReply` and
`handleSubscribeBridgeFeed` sit behind buttons that never gate on `feedReady`,
and `handleCompose` gates on `feedReady` — which tracks the manager only — while
also returning on `!$identity`, so an absent identity reached an **enabled**
submit and discarded the post with no `compose-error` at all (that surface is
derived from the manager's snapshot, and the manager was never asked to act).
Corollary for authors: **a disabled control only discharges the duty for the
conditions it actually gates on** — split the guard, surface the readiness
faults, and leave only the genuinely-nothing-to-do conditions as silent no-ops.

**The same duty binds a handler's `catch`** (2026-08-22, same pass). A managed
page's handler may not assume the manager stamped the failure: the shared
managers stamp every path **they own**, but a throw on the way *in* —
a wasm binding rejecting before the manager ran, a stale interface after a torn
rebuild, a plain `TypeError` — leaves that surface empty. A catch whose only
action is `logMessage(...)` is then the whole of the user's feedback, and
`logMessage` writes to the **WASM log ring**, which no page renders and no e2e
can read (`$lib/wasm.ts::logMessage`) — so the click lands, the action never
happens, and nothing anywhere says so. Two requirements, both met by the feed
composer's catch: log to the **browser console** as well (the ring
`drivers/web.py::console_log` captures, which is what a failing e2e dumps), and
surface the reason on the page's own writable error **when the manager did not
already stamp its own** — read off the manager's snapshot directly rather than
a `$derived`, whose recompute the catch would be racing.

**A FAILED build must never be memoized** (2026-09-09, closing the same class
from the other side — that rule covers an action landing *while* the manager is
null, this one covers the manager that never arrives at all). Every actor-scoped
singleton in the SPA is built behind a promise memo (`getFeedManager`,
`getConversationsManager`, `getSearchManager`, `loadEventDrafts`, and — the one
they are all built *over* — `rpc.ts`'s `getClient`), and **a
promise memo caches a rejection exactly as durably as a value**. So a builder
that installs its promise and never clears it turns one transient failure —
`ensureWasm()` losing a race, a WS-RPC connect failing, a 500 from a loaded
nest — into a singleton that is *permanently unbuildable for the rest of the
page's life*, not merely slow. **The actor-scoped drop is not the remedy**: it
runs on an identity CHANGE, while the commonest re-entry is the SAME actor
mounting the route again, where `store.ts`'s subscription early-returns on the
unchanged `secretHex` and no handler re-fires at all. So each builder clears its
own memo on the failure path, guarded by identity (`if (memo === thisBuild)`) so
a newer build the drop has since installed is not thrown away — the hazard the
actor-changed throw names, and why *that* throw deliberately does not clear.
`ensureWasm` has held this since `wasm-loader-guard.test.ts`;
`getConversationsManager`'s engine-role refusal wrote the reasoning down ("so a
later attempt can win … instead of being served this same rejection forever")
for one failure path, and it generalizes to all of them. Enforced over the
source by `src/lib/singleton-build-memo-contract.test.ts` (`just web-unit-test`).
The rule is argued from the shape, not from a measured incident: no failure
attributable to it has been observed in the field.

**The census is the rule's weak point, and it stopped one layer short**
(2026-09-09, same day). The four page managers were enumerated; `getClient` —
the WS-RPC transport singleton every one of them reaches through `call()` — was
not, and it had **no failure clear at all**. That is the worst place in the SPA
for this bug rather than a fifth equal instance: a rejected page manager costs
one rail, a rejected client costs *every* nest call on *every* page. Its
`(actor, nest URL)` key is not a substitute for the clear — the key forces a
rebuild only when one of them CHANGES, which is exactly the case the rule
already says the actor-scoped drop cannot cover. So the standing duty for an
author is the **census**, not the four names: any module that installs a promise
in a module-level slot and returns it to later callers is in this class, and
belongs in `singleton-build-memo-contract.test.ts`'s table on the commit that
introduces it.

**And a build that never SETTLES is memoized exactly as durably — so every
memoized build carries an external settle deadline** (2026-09-09). The memo rule
above covers the build that *rejects*; `void building.catch(...)` never fires for
a promise with no terminal state at all, so a pending-forever build is served
out of the slot to every later caller, indistinguishably and for the same
duration, having raised nothing. The two are therefore one property — **a
singleton build must reach a terminal state** — and are stated together, because
a reader who finds only the memo half re-derives the pending case as "already
covered". The pending arm is not theoretical on this codebase: a
`wasm_bindgen_futures` task whose poll throws a JS exception **dies mid-poll**,
surfacing a browser `pageerror` and never settling its JS promise. Its
consequence is the one that matters here — ***"the request is bounded" proofs
hold only while the task survives its polls***: `ensureConnected`'s 15 s throw
and a kind's RPC deadline both live *inside* the task, so a dead task takes its
own bounds down with it and no internal budget can fire. Only a timer **outside**
the task survives, which is why the deadline is external by necessity rather
than preference. `$lib/singleton-build.ts`'s `guardSingletonBuild` is the one
home (45 s, the budget `onboarding/+page.svelte`'s `armLaunchWatchdog` has
carried since this class was first measured on the launch path in 2026-07-16 —
far enough above the ~20 s of bounds it backstops that it can only fire on a
task that is genuinely dead). On expiry it **rejects**, which routes into
machinery that already exists: the memo clear above, and the awaiting page's own
`catch` (the catch duty three paragraphs up) writing `loadError` plus a browser
console line. It is **not** a retry and **not** a longer wait — it does not
repair the killed task and must never be read as a fix for whatever threw; the
budget bounds how long a surface may *lie* about being ready, and cannot make a
slow build pass, so e2e convention 14 is untouched. A private watchdog per
builder would have been the fifth copy of a rule five surfaces need identically
(priorities #1/#2), which is the whole reason it is shared and why the census
duty above governs it too: same table, same test file, one entry per builder.

**The deadline abandons a build it cannot stop — so an abandoned build leaves
nothing behind** (2026-09-10). The deadline rejects the *awaiter*; it cannot
stop the build, because a promise is not cancellable. So a build that is merely
slow rather than dead is **abandoned while still alive**, a state no failure
path before the deadline could produce (a rejection yields nothing; a resolution
was always the current one). Left alone it resolves after a later mount has
installed its replacement and writes its own result over it: the memo vends one
build while the module slot holds another, and every reader of the slot — the
snapshot refreshers, the draft saves, the e2e counters — reads the build the
page was told had failed. Same actor, so not a leak: a split-brain. Two rules
close it. **No write after abandonment:** `guardSingletonBuild` takes the build
as a *function* and hands it `stillWanted()`, false from the moment the deadline
gives up on it and never true again (and true for good once a build settled in
time, so a fire-and-forget tail may keep consulting it); every builder checks it
beside `stillThisActor()` after each await and before each module-state write —
per write, never once, the identity seam's own shape one predicate over. A
build's tail works on the value it built and never re-reads its slot, which by
then may hold the replacement or the incoming actor's build. **Retract what was
written before:** a build can be abandoned *after* installing, dying in the tail
that follows (the feed manager's draft restore, the conversations manager's MLS
passes), so the memo clear nulls the product slot along with the memo, inside
its `=== guarded` branch — while the memo held this build nothing else could
write that slot. Without it the conversations receive poll, which rebuilds only
on an empty slot, would pump the abandoned engine for the rest of the page's
life. The conversations build, whose tail writes MLS state pass after pass,
re-checks both seams before each pass: an abandonment puts a second engine for
the account in this same tab, and the one call already in flight cannot be
stopped, but the next one can. `rpc.ts` consults the same predicate with a
different remedy: an abandoned client arrives holding a socket and a push
fan-out, so it is **retired** on arrival rather than merely not stored — the
trigger is shared, the remedy is the builder's, and the client generation
answers only what it was made for, supersession by a key change. Enforced by
the third section of `singleton-build-memo-contract.test.ts` over the same
census table (per slot, against the most recent await before each write) and by
`singleton-build.test.ts`, which drives the interleavings at single-digit
millisecond budgets — never a sleep past 45 s.

**Retiring the WS-RPC client is a teardown site, so it calls the canonical drop** (2026-09-09). `getClient` keys its singleton on `(actor, nest URL)` and, when either changes, closes the superseded client — and `WsRpcClient::close` latches a **one-way** flag, so that client never reconnects. The four page managers do not merely use it, they **wrap** it (`c.feedManager(...)`, `c.searchManager()` construct shared-Rust managers generic over `WsRpcClient`), and `getFeedManager()` keys on **nothing** — it memoizes one promise and hands it to every later caller. So a retire without a drop leaves every manager holding a permanently dead transport, vended for the rest of the page's life. The nest half is the half with no other trigger: `store.ts` fires the actor-scoped drop from a subscription to the identity, keyed on `secretHex`, and a nest change is the *same* actor — so nothing else fires at all. The reachable journey is client-side end to end with module state surviving: `admin-nest`'s factory reset → `accountsClearNestBinding()` → onboarding → its `LoggedIn` exit writes the new `fauna_node_url` → back to the feed. **The remedy is the drop, not a second key**: giving each manager `getClient`'s own `(actor, nest URL)` key would be four copies of one rule (priorities #1/#2, the same argument that put the settle deadline here rather than in four private watchdogs), and it re-derives through a URL string the fact that actually matters — the client died. [`account-scoping.md`](account-scoping.md) § The scoping taxonomy already states the rule in these words: exactly one canonical drop per app, and every teardown site calls it with no list of its own; this was the one teardown site that called nothing. The drop runs **after** the retire and deliberately **not** inside `retireClient`, which also runs on the supersede arms where the retired instance is a freshly-built client no manager was ever built over. Enforced over the source by `src/lib/client-retire-drop-contract.test.ts`, the fourth sibling of the contracts above.

**A registered actor-scoped drop is pure state, and may not reach for wasm.**
`resetActorScopedState()` runs every drop under its own `try`/`catch` on purpose
(one broken drop must not strand the others), which means a drop that throws is
**invisible**: the switch reports success and the state that drop owned stays
live for the incoming actor — a class 1/4 breach of `account-scoping.md` § The
scoping taxonomy's in-memory corollary, with no error anywhere. A drop therefore
touches module/component state only: no wasm face, no **lazy constructor**, no
round trip. (`screenTime.svelte.ts`'s drop read `usage()`, which *builds* its
handle through wasm, so it threw `WASM not initialized` on every login that
lands before the module is up — which the e2e agent's `set_state` patch always
is, since it bypasses the `identity.login()` that would have run `ensureWasm()`.)
The witness is the browser console, so the guard is an e2e boot invariant:
`tests/test_web_boot_effect_loop.py::test_boot_drops_every_actor_scoped_reset_without_throwing`.

### localStorage Keys

| Key | Contents |
|-----|----------|
| `fauna/index` + `fauna/{actor}/secret` / `nest_url` | **The identity store (2026-09-24)** — the shared `AccountRegistry`'s index blob and per-actor slots, keyed verbatim (`libs/fauna-client-accounts/src/web_store.rs`); the SPA resolves its identity through `accountsSessionMaterial` (the active account, or a pinned tab's own), and the handle / domain / tier cache lives in the index entry (`updateCache`). `registered` is **not** stored — it's derived from `handle != null`. The pre-registry `fauna_secret` / `fauna_handle` / `fauna_domain` / `fauna_tier` / `fauna_node_url` keys are neither written nor read any more ([`../long-term-store.md`](../long-term-store.md) § Downgrade mirror + abandoned-append recovery) |
| `fauna-push-subscribed` / `fauna-push-subscribed-actor` | Whether this browser holds a push subscription, and which actor's `push_subscriptions` row it currently points at (`push-actor.ts`) — install-scoped, not account-scoped, so no sign-out/account-removal erase touches them (`account-scoping.md` § The scoping taxonomy, class 2). The actor key alone clears when a leave-gesture's row drop lands (`common.md` § Registration — "the subscription follows the signed-in identity"); the subscribed bit survives every leave-shape and clears only on the user's Disable |
| `fauna/{actor}/device_id` + `install/device_secret` | This browser's device id, **one per account** (`$lib/device-id`'s `getDeviceId(actorId)`, over the shared `AccountRegistry::device_id_for_actor`): the account's persisted slot, else `derive_device_id(install_secret, actor_id)` over the install-scoped secret — lowercase-64-hex, the `[u8; 32]` shape the native apps use, for `fauna.sync.changes.record`/`fauna.folders.*` attribution and as the push-subscription key. The per-account slot goes with the account's erase; the secret, named by no erase, survives every sign-out. The rule: [`sync-agent-credentials.md`](sync-agent-credentials.md) § Credential model, the 2026-09-20 ruling |
| `fauna-push-device-id` | **Gone (2026-09-24)** — the pre-derivation id, one value for every account on the browser, retired 2026-09-22 and its adoption-at-upgrade + retired-id drain removed by the compat-remnant sweep, since no pre-derivation browser exists ([`sync-agent-credentials.md`](sync-agent-credentials.md) § Credential model) |

(The per-account slot above IS the long-term-store contract's per-actor
`device_id` slot — [`../long-term-store.md`](../long-term-store.md) § Web —
filled lazily by the get-or-create rather than at onboarding, and mirrored
into the legacy `fauna_device_id` while its account is active. This
table covers the standing identity/session-plane keys; the onboarding
wizard's own resume slots — pending-invite, awaiting-DNS,
pending-factory-reset (`fauna_pending_*`), each deleted once its wizard step
completes — are a separate inventory owned by
[`../../behavior/onboarding.md`](../../behavior/onboarding.md).)

### Identity store at rest (design ruled 2026-10-01; refutable until built; NOT built)

The rows of the identity store above are plain values today. What the web app owes at rest, why, and the approval the design still awaits are owned by [`common.md`](common.md) § Credential storage → *The web app's posture*; this section owns the mechanics of that design and nothing here is built (§ Implementation status today).

**An envelope, sealed row by row.** One header row (`fauna/seal`) carries a version byte and one or more *wraps* of a random 32-byte **store key**; a passphrase wrap is the store key sealed with ChaCha20-Poly1305 under an Argon2id-derived key — the shared `fauna_core::kdf` interactive parameters the web app already runs for wrapped blobs, with a 16-byte random salt and the parameters recorded beside it, so a later cost tightening reads old headers unchanged. Opening the wrap **is** the passphrase check: no separate verifier rests in the browser, and a wrong passphrase and a damaged header are one failure the copy names honestly — the terminal app's rule ([`tui.md`](tui.md) § Credential storage), kept. Every other row the shared `SecretStore` writes — `fauna/index`, each `fauna/{actor}/…` slot, `install/device_secret` — holds its value sealed under the store key, a fresh nonce per write, with the versioned context string and the row's own logical key as associated data, so a sealed value cannot be moved to another slot. Row *names* stay readable: they hold actor ids, which are public keys, and reveal which accounts this browser holds and nothing more.

**Why not the terminal app's one blob over the whole map.** That shape makes every write a read-modify-write of all accounts' secrets, and tabs are web's concurrent writers: a lost update could destroy a second account's only copy of its key — the hazard `tui.md`'s *a write never overwrites a map it could not read* guards with a file lock. Web's counterpart, the Web Lock ([`../long-term-store.md`](../long-term-store.md) § Multi-account evolution, web's leg), is asynchronous around synchronous mutators and absent outside a secure context: it narrows a race and is not something the survival of client-only key material may rest on. Sealing row by row leaves concurrency exactly as built — a write touches one row — and the envelope makes changing the passphrase **one row write**: only the header's wrap is re-sealed (fresh salt, current parameters), so at every crash instant exactly one passphrase opens the store, other tabs keep working on the unchanged store key, and no row is rewritten. What is shared with the terminal app is everything but the container: the KDF and its parameters, the AEAD, the context-string discipline (`../key-material-hierarchy.md` § Architectural rules, rule 3), the unlock / create / change surfaces and their element ids, and the recovery-seed nudge before a change.

**Where the logic lives: shared Rust.** A sealing `SecretStore` wrapper over `LocalStorageSecretStore`, with the header's create / open / re-wrap beside it, unit-tested natively over an in-memory store; the crate home is the build's call (beside the terminal app's `sealed` core if `fauna-credential-store` compiles for wasm, else `fauna-client-accounts` beside `web_store.rs`). It runs in the wasm core, never through `crypto.subtle`, which a page loaded over plain HTTP — a fresh nest on a LAN — does not have. The TypeScript that reads identity rows directly today (`wasm-launch.ts`'s reads of `fauna/index` and the active secret) goes through the store.

**Unlock comes before launch routing.** Launch routing reads the store, so nothing runs until it serves reads — the terminal app's order. No header and no rows: **create** mode (passphrase and confirmation), deliberately before onboarding, because a wizard left half-way persists resume rows. A header: **unlock** mode. The surface is the terminal app's unlock page, shared — `tui-unlock` in `ui.yaml`, `platforms: [tui]` today; web joining it, under whatever name, is an id-level change the user approves — plus one thing a browser needs that a terminal does not: a **start-over** action on the unlock page that, after an explicit confirmation, erases this browser's identity store and returns to create mode. A person who has forgotten the passphrase has no file to delete, and without it the only way out is the browser's own site-data dialog. The seal has no escrow, so a forgotten passphrase costs this browser's copy of the key; the identity comes back from the recovery kit, an exported key or another device, as on every app. In Settings the shared credential-store section ([`../../ui/settings.md`](../../ui/settings.md) § Credential store) renders on web with its existing ids — the status line and the change-passphrase modal.

**The unlocked store key lives in page memory and nowhere else.** The page hands it to each wasm chunk (they are separate runtimes); it is never written to `localStorage`, IndexedDB or `sessionStorage` — a browser may write session storage to disk to restore a session. A reload, like a launch, therefore asks again unless another tab answers: **a newly opened tab asks the tabs already unlocked for the store key over a same-origin `BroadcastChannel`**, so one unlock serves the browser session for as long as one tab stays open, and the per-tab account pin (`sessionStorage`'s `fauna_tab_account`) costs no second prompt. This exposes nothing new: the only listener is script in the app's origin, which the seal never claimed to stop.

**The push worker is untouched.** `service-worker.ts` shows a notification from the payload it is handed and holds no identity material today; the design gives it none. A notification tapped while the app is locked opens the unlock page.

**Rows a browser already holds.** At its first launch with the seal, a browser holding plain rows gets create mode, and each row is then rewritten sealed in place. A sealed value is self-describing, so a launch that finds both kinds resumes the rewrite, and no plain value is replaced before its sealed form has been read back (`../../principles.md` § No user-data loss — client-only key material). A build older than the seal cannot read sealed rows and would route to onboarding over an intact store; the build settles that against [`../long-term-store.md`](../long-term-store.md) § Downgrade mirror + abandoned-append recovery and [`../version-compatibility.md`](../version-compatibility.md) before it lands, not after. The header lives as long as any sealed row does — `install/device_secret` outlives every sign-out — and only start-over or a factory reset removes it.

**Test builds.** The harness-driven app keeps plain rows unless a journey opts into the sealed arm — the same class of carve-out as the native `FAUNA_E2E_CREDENTIAL_DIR` (`common.md` § Credential storage), available to test-capable builds only; a shipped build has no plain arm. How a web build is known to be test-capable is the build's to establish.

**A passkey wrap (follow-on).** A second kind of wrap in the same header: a WebAuthn credential created in the page with the PRF extension, its PRF output run through `BLAKE3::derive_key` under a versioned context to give the wrapping key, the credential id and PRF salt recorded in the wrap. Because the header holds wraps of one store key, a passkey is added or removed without touching a row, and a passphrase wrap can stay beside it as the fallback. A passkey that is lost with no other wrap is a forgotten passphrase.

### IndexedDB

The on-device ONNX spam classifier's model + vocab bytes (`fauna-spam` DB,
`models` store, `wasm-onnx.ts`) — install-scoped, deployment-wide, not keyed
per actor (class 4); **retired 2026-10-02** with the scorer (an existing
install's leftover DB is inert — it never held user data). The per-account Bayesian replica that used to live here
(`spam-model.ts`) had zero production callers and was deleted 2026-09-01; the
real on-device scoring position is `inbox_scorer.rs`
([`account-scoping-dispositions.md`](account-scoping-dispositions.md) § Isolation-contract gap ledger,
web row). The `fauna_mls` database is retired (§ MLS).

### In-Memory

- **Post deduplication set:** `seenPostIds` — capped at 10,000 entries (oldest evicted first) via `isNewPost()`
- **Token cache:** per-(node, identity) `Map<string, { token, expiresAt }>` in `api.ts`, keyed by `tokenKey(nodeUrl, secretHex)` — the identity is part of the key so two actors on the same nest can never share a cached bearer (a cross-identity bearer-reuse bug this fixed)

---

## API Layer (`src/lib/api.ts`)

- `nodeUrl()` returns `localStorage.fauna_node_url` if set, otherwise
  `window.location.origin` — the SPA can talk to a different nest than the one
  serving the page.
- `getAuthToken(secretHex, targetNode?)` mints bearers over the **pre-identity
  anonymous WS-RPC silent challenge** (`challengeVerify`, the same ceremony as
  `silentSignIn`) and caches them per (node, identity) (§ In-Memory, Token
  cache) with the deadline anchored on the device clock at receipt, reusing a
  token that expires more than 60 seconds in the future. Ceremony owner:
  [`../../behavior/login.md`](../../behavior/login.md); anonymous-connection
  mechanics → [`../transport-connection.md`](../transport-connection.md) § Pre-identity.
- Protocol modules: `bridges.ts` (unified bridge link/settings over
  `fauna.bridges.*`) and `nostr.ts` (nostr key handling; its native-content
  HTTP fetches — `publish-signed`/`badges`/`zaps` — were migrated
  `2026-07-22` to the `nostr.{events.publish_signed,badges.list,zaps.total}`
  WS-RPC kinds, deleting the corresponding `/api/v1/nostr/*` routes and
  closing the client-to-nest HTTP surface entirely. **None of the three have
  a live web UI caller today**: `nostr.ts`'s `publishSignedEvent()` (the
  NIP-07 publish leg) and `getNostrBadges()` wrap the kinds but are called
  from nowhere in `apps/fauna-web/src/` — the `/nostr` page's NIP-07 UI was
  removed when bridge management unified onto `/bridges` and a
  badges display was never built — while the zap-total wasm face
  (`nostrZapTotal`) is kept for a future NIP-57 zap-adapter surface, its
  TS-side wrapper having been deleted `2026-07-23` as dead code for the same
  reason (zero callers). `hasNip07()`/`nip07GetPublicKey()` (also in
  `nostr.ts`) stay live — `BridgeCard.svelte`'s NIP-07 link-mode probe — and
  are a separate concern from event publishing —
  [`../../ui/nostr.md`](../../ui/nostr.md) § WS-RPC migration contract).

---

## Push Notifications

### Service Worker (`src/service-worker.ts`)

| Event | Behavior |
|-------|----------|
| `push` | Parses the JSON payload, calls `self.registration.showNotification()` with title and body |
| `notificationclick` | Focuses an existing fauna window or opens a new one at `/app/` |

### Push Module (`src/lib/push.ts`)

The three nest hops ride WS-RPC — `fauna.push.{vapid_key,subscribe,unsubscribe}`
via `rpc.ts` (`pushVapidKey` / `pushSubscribe` / `pushUnsubscribe`); the browser
side uses `PushManager.subscribe()` with the VAPID key. Transport/dispatch/
encryption contract → [`common.md`](common.md) § Push Notifications. The
settings page renders the status display + Enable/Disable action button (not
a checkbox toggle) — [`../../ui/settings.md`](../../ui/settings.md) § Push notifications.

---

## Content Moderation

Client-side scanning runs after MLS decryption **inside Rust**: bodies are
decrypted in `WasmConversationsManager` and never cross into JS, so the
post-decrypt classify hook and the `LocalDetectionStore` it writes live in the
shared crates (read back via `moderationLocalDetections()` in
`$lib/conversations`). The former TS-side `src/lib/moderation.ts`
(`LabelBuffer` + `classifyAndReport()`) was **deleted 2026-07-19** with the
`scan_report` producer retirement — the kind itself left the wire 2026-09-24
([`../../behavior/moderation.md`](../../behavior/moderation.md) § State & data
shape owns the verdict). `api.ts` keeps `trainSpam` (`fauna.moderation.train`); the uncalled
`syncSpamModel` left with `fauna.moderation.model_sync` (2026-10-02).

---

## Onboarding and app-launch (`src/routes/onboarding/`, `src/lib/onboarding/`)

The provisioning/claim wizard lives under **`/onboarding`**, driven by the
shared `OnboardingMachine` (the onboarding chunk) through
`lib/onboarding/machine.svelte.ts` and its pending-state stores. There are no
`/setup/*` routes. Flow behavior, page set, and exit-outcome routing →
[`../../behavior/onboarding.md`](../../behavior/onboarding.md) (owner). Web's
app-launch routing is machine-carried: every launch row is driven through the
shared `LaunchMachine` (`fauna-wasm-launch` chunk, `lib/wasm-launch.ts`,
`createLaunchMachine()`); the former web-local `runSilentChallenge` classifier
is deleted.

---

## C2PA

Web glue only: `src/lib/c2pa.ts` lazy-loads `@contentauth/c2pa-web` +
`c2pa.wasm`, short-circuits on the `x-c2pa: false` hint, and caches per URL.
Badge semantics, the hint's trust model, and the badge-correction rule →
[`../../ui/media.md`](../../ui/media.md) § C2PA provenance (owner).

---

## Build and Deployment

| Command | Effect |
|---------|--------|
| `deno task dev` | Vite dev server |
| `deno task build` | Static output to `build/` |
| `deno task check` | SvelteKit sync + TypeScript validation |
| `deno task preview` | Serve the static build locally |

`VITE_GIT_SHA` is injected at build time from the environment (falls back to
`"dev"`). This is the only build-time variable. The static output is served
under `/app/`; all routing is client-side.

---

## Route Structure

30 `+page.svelte` routes:

| Area | Routes |
|------|--------|
| **Messaging** | `/conversations`, `/events`, `/contacts`, `/notifications` |
| **Social** | `/feed`, `/search`, `/nostr`, `/profile/[[actorId]]` |
| **Media & files** | `/media`, `/backups`, `/devices` |
| **Bridges** | `/bridges` |
| **Family** | `/family` — guardian/supervised parental-controls surface, gated behind a `familyStatus()` read (`../../behavior/family-safety.md`, owner) |
| **Admin hub** | `/admin` + `/admin/{settings,users}` + `/admin/{aliases,bridges-pending,calendar,contacts,custody-hosting,dns,files,logs,mail,nest,web}` — each the ui.yaml page id minus its `admin-` prefix (the e2e web bridge maps a nav id such as `admin-dns` to `/app/admin/dns`) |
| **Onboarding** | `/onboarding` |
| **Settings shell** | `/settings/[[subpage]]` (rail sub-pages incl. status (empty subpage), devices, folders, mail-settings, nests, logs, muted-words, subscription-settings, web) |

The root `+page.svelte` and `+layout.svelte` handle authentication gating and
initial store hydration.

---

## Implementation status today

- **Declared absences (owner [`../../behavior/family-safety.md`](../../behavior/family-safety.md) § The account age band):** no store age signal and therefore no `invite-request-age-notice` (D3 — at most the two store-distributed mobile apps ever receive one; the id is `platform_elements: {android, ios}` in ui.yaml), and no kids flavor (the kids-app bullet: the kids door is a store construct, android and ios only). The e2e gate for both is `declared_absence`, never `skip_unbuilt`.
- **Declared absence (owner [`common.md`](common.md) § Home-screen widget):** no home-screen widget — a browser tab has no home screen to hold one, so the behaviour itself is absent, not a mechanism; the catalog's `home-screen-widget` page declares web absent citing that section (user-approved 2026-09-26).
- **Declared absence (owner [`../installers/README.md`](../installers/README.md) § Knowing a newer version is out):** no "a newer version is out" notice — the SPA is served by the nest and is always the nest's own version (§ Build and Deployment), so there is nothing for it to be newer than; the catalog's `app-version-and-updates` outcome 2 is absent here by design (user-approved 2026-09-26; applied the same day as the per-outcome absence `web (outcome 2)`).
- **Declared absence (owner [`../../behavior/authorization-server.md`](../../behavior/authorization-server.md) § Consent → *How the same-device handoff is built*; ruled 2026-10-02, refutable until the user ratifies the catalog entry):** no `fauna://consent` route intake — a browser tab is launched by no OS link. A page, or an installed PWA, may register a handler only for a `web+` or safelisted scheme (`registerProtocolHandler`, the manifest's `protocol_handlers`), and even that handler is reached only by a navigation inside the browser that holds it, never by the `open` a device app issues — so no spelling of the route can bring the web app forward, and the absence is the behaviour's, not a mechanism's. The handoff's own ruling already covers a device whose Fauna app no link can launch: the client finds no handler and takes the browser door with the same handle, served by the nest that serves this SPA, and the row that door opens is an ordinary browser-start row, listed to the account's apps — the signed-in web tab's requests tray included — as that start rules. Two mechanisms were weighed and refused: a `web+fauna` handler (a second scheme every device app would have to learn, reachable only from a link inside one browser, while a website client already has the browser door), and an SPA route that consumes the handle through the signed-in session (a third door, which *one PAR, two doors* excludes, and a third URI spelling for clients). The e2e gate is `declared_absence` in `ConnectedAppsActions.open_handoff_route`, so the two handoff journeys declare web; the catalog's `connected-apps` outcomes 17 and 18 take the per-outcome absence `web (outcomes 17, 18)` once the user approves it ([`../feature-catalog.md`](../feature-catalog.md) § The coverage contract).
- **§ Identity store at rest is NOT built** (design ruled 2026-10-01): the identity store's rows are plain `localStorage` values, there is no header row, no unlock page and no credential-store section on web. Its required launch prompt awaits user approval (owner [`common.md`](common.md) § Credential storage → *The web app's posture*).
- **Architecture as described above is built** (WS-RPC plane, conversations
  manager, chunk inventory, onboarding wizard, push over kinds).
- **The three previously-tracked "remaining web-side legs" are all done**
  (re-verified 2026-07-19, sweep — this section had gone stale for weeks
  while each landed against a *different* doc): the `routes/events` legacy
  `WsConnection` consumer is **deleted outright**, not just fixed (
  2026-07-12 — see § Transport); the moderation web slice-5 queue unification
  landed 2026-07-12 (`../../behavior/moderation.md` § Implementation status
  today); the knock-send button wiring landed 2026-07-09
  (`contacts-add-button` drives the real `sendKnock` → `fauna.inbox.send` path,
  `../core-client-kind-catalog.md` § Contacts & Knocks). LaunchMachine adoption is **done**
  (see § Onboarding and app-launch above). `fauna-wasm-content-index` stays on
  hold pending the content-index serving decision
  (`../../behavior/content-index.md`).
- **Resolved 2026-07-19 (owner call made):** the former "`scan_report` has no
  live caller" gap closed by **retirement**, not implementation —
  `../../behavior/moderation.md` § State & data shape dead-registers the kind
  and records why (no read path; unconsented scan telemetry contradicts the
  user-controls-their-data posture). `moderation.ts` and the `rpc.ts`
  `moderationScanReport` wrapper were deleted with it.
