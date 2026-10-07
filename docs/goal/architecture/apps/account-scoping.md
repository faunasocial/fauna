# App account scoping and the multi-account capability stages — target state

Owns: account-scoping
Status: ratified
Authority: owns the client-side data-scoping taxonomy (account-scoped / install-scoped / OS-rendezvous / nest-authoritative replica) with the scoping invariant for new app state, the multi-account capability stages (serialized switching → concurrent instances → concurrent identities) with their ratified per-stage decisions and per-app status, and the per-surface scoping dispositions only by pointer: the remediation that places each store, and each app's closure record of the isolation contract in both its dimensions, are owned by [`account-scoping-dispositions.md`](account-scoping-dispositions.md) (split 2026-09-28). Defers: the account registry / index / legacy mirror / switch-teardown mechanics to [`../long-term-store.md`](../long-term-store.md) § Multi-account evolution; the File-Provider serialized-single-account consequences and the per-actor sync-state mechanism to [`../../behavior/on-demand-files.md`](../../behavior/on-demand-files.md) § On-Demand Files; the sync agent's capability/renewal model to [`sync-agent-credentials.md`](sync-agent-credentials.md) § Credential model; the credential-store contract to [`common.md`](common.md) § Credential storage; e2e isolation rules to [`../e2e-conventions.md`](../e2e-conventions.md) (conventions 10/12). Provenance: the user's three-stage directive (2026-07-21) and fork ratification (2026-07-22): sync surfaces stay active-account-only in the first concurrent-instances delivery; per-actor paths + first-adopter adoption is the uniform remediation; the mixed-MLS-state adoption hand-off is approved.

One app install (one OS-user context) can hold several identities. The
registry that stores them is built and shipped on all seven apps (owner:
`long-term-store.md`). This document owns the question that registry does not
answer: **which of the app's *other* persisted data belongs to one account,
which belongs to the install, and how the answer evolves as the product moves
from switching between accounts to running them concurrently.**

## Vocabulary

- **Account** — the client-side unit: one identity held by this install. The
  shipped term everywhere (AccountRegistry, `account-switcher-item`); all
  seven apps use it. "Multi-user" and "multi-handle" in older discussion
  mean this.
- **Actor id** — the stable 32-byte key (hex) deriving from the identity; the
  namespacing key for every account-scoped datum. Never displayed.
- **Handle** — the account's user-visible name (`user@domain`). Display-only;
  never a storage key (handles can be re-pointed; actor ids cannot).

**Stage-name disambiguation (load-bearing).** `long-term-store.md` and older
material use "Stage 0–3" for the *implementation* stages of the switching
surface (registry → per-platform store/switcher → re-auth-on-activate →
background cross-identity). The **capability stages** below are a different,
product-level axis and are deliberately *named, not numbered*, so the two
vocabularies cannot be confused.

## The capability stages

| Stage | Meaning | Status (2026-07-23) |
|---|---|---|
| **Serialized switching** | One active account per running app instance; switching = registry `set_active` + full session teardown/rebuild. | **Built on all 7 apps** (`long-term-store.md` § Implementation status). The *isolation contract* below is met **on all 7 apps for persisted state**; the **in-memory** dimension is closed on **all 7 apps** — web, linux, apple (2026-08-17 — one canonical `dropActorScopedState()` per target over FaunaKit's shared `ActorScope.resetSharedState()`), windows (2026-08-24 — the same split, `App.DropActorScopedState()` over `Core.Services.ActorScope`, plus the cancellation seam a token-less background loop needed) and **tui (2026-08-27** — listed here as closed since 2026-08-17 with no ledger row behind the claim, and it was not: the retired MLS engine outlived the switch through an `Arc` cycle in shared Rust and three tick-bound background loops; closed on the same cancellation-seam ruling, as a session-closed signal every loop selects on). and **android (2026-08-27** — counted here since 2026-08-17 with no row behind the claim either, and the same audit found it open: its foreground custodian push-debounce loop was retired only by *backgrounding*, which an in-app switch is not, and android had **two** teardown funnels rather than one, with four surfaces on neither; closed the same day on this section's own ruling — one canonical drop, and an identity seam for the loop that holds no cancellation handle). ⚠ **REOPENED and re-closed 2026-09-01 on all four apps that had shipped the events drafts rail** — android, tui, web and linux — each of which shipped it with a correct drop and no seam: the proof that corollary 2's second rule is a standing obligation on every NEW background writer of actor-scoped state, not a one-time audit. The rail was reviewed only on android; the other three were found by sweeping for the same shape, and two of them (tui, linux) had a guard that *looked* like the seam and was not. See [`account-scoping-dispositions.md`](account-scoping-dispositions.md) § Implementation status today → Isolation-contract gap ledger. |
| **Concurrent instances** | Several app instances under one OS login, each bound to a different account, running simultaneously. | **Ratified target, underway.** The shared-Rust groundwork and the (OS login, account) instance lock are wired on apple, linux, tui and windows; linux, windows and tui all render the launch-collision chooser, and linux + windows have the per-(OS login, account) raise channel (tui's raise leg is a declared absence — it prints where the account is served and exits). **Web's tab-pinning landed 2026-09-01** — the wasm session-identity surface plus a per-tab `sessionStorage` pin, so two tabs of one browser profile hold distinct accounts instead of converging on the last-activated one; its Web Locks leg guards the migration section, and **one MLS-writing tab per account is elected as of 2026-09-01** — the losing tab runs no engine and paints the same `served_elsewhere` refusal linux and apple paint for the native role lock. Still open: android's native leg (the last one) — see § Implementation status. |
| **Concurrent identities** | One app instance with several accounts live at once. | **Direction only.** Not designed; this doc owns only the constraints that keep it reachable. |

The stages are strictly ordered by what they demand of storage: serialized
switching needs *isolation* (no cross-account leakage through shared files),
concurrent instances additionally need *concurrency* (two processes touching
the store safely, plus a rule for each OS-global rendezvous point), and
concurrent identities additionally need *in-process multiplexing* (no
process-wide singleton that assumes one identity). Getting the scoping
taxonomy right therefore serves all three at once — which is why the
remediation below is worth doing now, at the switching stage.

## The scoping taxonomy (iron-clad for new app state)

Every datum an app persists belongs to exactly one class, and **a change
that introduces new app persistence declares its class** (in the goal doc
or code comment introducing it). The classes:

1. **Account-scoped** — data derived from or belonging to one identity:
   credentials and resume slots, sync-engine state, MLS/conversation state,
   content caches and replicas, drafts, per-account preferences, backup
   coordinator state, P2P peer state. **This is the default class for
   anything whose meaning depends on who is signed in.** Placement: the
   per-account layout `<platform state base>/<actor-id-hex>/` (the derivation
   is owned by `../../behavior/on-demand-files.md`
   § Multi-account × File Provider, consequence 3 / `fauna_account_store::db`,
   which `fauna_sync_engine::db` re-exports), or actor-namespaced keys
   (`fauna/{actor_id}/…`) in key-value stores (owner: `long-term-store.md`).
2. **Install-scoped** — facts about the install/device, actor-blind **by
   declared design**: the account index (owner: `long-term-store.md`; the
   legacy single-slot mirror beside it retired 2026-09-24, its transitional
   native-app survival deleted 2026-09-28), device-local UI preferences that describe the
   device rather than the person (theme, window geometry, close-to-tray,
   autostart), log files, nest-identity TOFU pin stores (keyed by host — a
   property of the network path, not the identity), the push transport's own
   handles and its **subscribed bit** (web's device id + `fauna-push-subscribed`;
   apple's and tui's shared `push-intent.cbor` record) — one device token, one registration, one
   key pair per install, describing the transport rather than the person, so a
   switch must neither silently re-open a push channel the user closed nor
   close one they left open (registration rule: `common.md` § Registration),
   the photo-library backup switch (`photo-backup-enable-toggle`: apple's
   `fauna.photoBackupEnabled`, android's `auto_photo_backup`) — it chooses whether *this device's* photo
   library is backed up, so a switch keeps it, and the next pass files the
   device's photos into the now-active identity's Photo Library set
   (ratified 2026-09-29; set model: `../../ui/folders.md` § Photo backup),
   one-shot migration flags. Anything claimed as install-scoped needs a stated
   reason; "it was easier" is not one.
3. **OS rendezvous** — namespaces the operating system gives one of per
   machine or per OS login: File Provider domain identifiers, cfapi
   sync-root registrations, the sync agent's socket/pipe + instance lock,
   D-Bus / GApplication names, single-instance mutexes. These cannot be
   duplicated per account; they are **multiplexed** — entries inside them
   carry account-scoped keys, and each stage's rules below say which accounts
   may occupy them when.
4. **Nest-authoritative replica** — local copies of state whose truth lives
   on the nest (feed/content caches, the spam-model replica —
   `mail-spam.md` owns the per-user model lifecycle — nest-backed drafts;
   the sealed `__config` replica was one until the rail retired 2026-10-02,
   [`../config-dissolution.md`](../config-dissolution.md) § The `__config`
   dissolution schedule → *The closure order*, step (6)). Replicas still
   take account-scoped *placement* (class 1), but they are wipe-tolerant:
   deleting one costs a re-sync, never data.

Three corollaries, all binding today:

- **The switch/sign-out isolation contract.** Under serialized switching, no
  account-scoped datum may be read or written by a session authenticated as a
  different account — a switch that lets account B render or modify account
  A's local state is a bug of the same class as serving another account's
  domain (the `foreignDomainOwner` fail-closed rule in `file-sync.md`).
- **In-memory state is account-scoped too, and the contract binds it.** The
  classes above are written for what an app *persists*, but the isolation
  contract says "read **or rendered**" — so a live cache, a manager instance, a
  decrypted-bytes blob URL, or a module-level singleton holding one identity's
  data is account-scoped by class 1/4 exactly as its on-disk twin would be. It
  therefore has to be dropped at the same moment the persisted stores re-scope:
  **at the identity change itself, keyed on the identity** — never on a
  lifecycle event that merely *usually* coincides with it. An app whose
  in-memory drop rides a window/page/shell teardown is correct only while that
  teardown is guaranteed, and must say where the guarantee comes from; where it
  is not guaranteed (a same-route navigation, a reused shell), the state needs
  an explicit identity-keyed reset seam. Two rules follow, both learned the
  expensive way. **First: a drop hand-listed at each teardown site rots** the first
  time someone adds state and forgets one site — every app that had several lists
  was found already drifted, and *the drift, not the missing piece of the day, is
  the finding*. So there is exactly **one canonical drop per app, and every
  teardown site calls it with no list of its own**. Which form that takes is a
  language question, not a design choice to re-litigate: where importing a module
  runs its registration as a side effect the registration sits next to the state
  (web's `lib/actorScope.ts`), and where it does not — Rust, Swift, C# — it is
  **one explicit, statically greppable function**, because a registry there only
  relocates the hand-list (into `mod` declarations, or lazily-run static
  constructors) and adds a *silent* failure mode when a registration is forgotten;
  linux ruled this, apple and windows adopted it. An app whose canonical drop
  spans two assemblies keeps one list per assembly, the outer calling the inner
  (apple's per-target `dropActorScopedState()` over FaunaKit's
  `resetSharedState()`; windows' `App.DropActorScopedState()` over
  `Core.Services.ActorScope`) — never a list per *site*. **Second: dropping state
  is only half of it — the background loops that WRITE that state must be retired
  by the same drop**, and a loop that holds no cancellation handle cannot be
  stopped by any list, so the seam comes before the drop (linux's actor-generation
  counter, apple's cadence objects owning their `Task`, windows' lease-per-loop
  token generation). **Third: the failure is silent by construction** — stale
  actor-scoped state renders as *empty or plausible*, never as an error — so it
  needs a red-first test at the identity change, not an inspection.
- **Erasure follows scope.** Sign-out (the all-accounts erase,
  `long-term-store.md` § Cleanup contract) erases *every* account-scoped
  store, not only the credential namespace; removing one account erases that
  actor's stores. Install-scoped state survives. (This extends the cleanup
  contract's existing rule to content stores; the contract's owner doc keeps
  the credential-namespace mechanics.)
  ⚠ **"Every" includes the stores that do NOT live under the app's own base.**
  After W6 (account-data-plane.md § Workstreams) path unification the account store sits at the per-user root
  `StoreRoot::platform()`, shared across apps
  (`account-data-plane.md` § The account store) — **part of no app's own base, whatever the paths suggest**: a sibling of tui's (`fauna-tui/` beside `fauna/sync/`),
  textually *inside* linux's (`fauna/sync/` under `fauna/`), the same directory either way — so an erase that iterates only the app's own per-actor scopes
  silently misses it. The miss is not merely stale bytes: the credential
  namespace this erase accompanies is where the store's writer key lives, so
  the survivor is a store whose writer the slot no longer holds. Until
  2026-08-27 that stranded the machine — every later sign-in refused the
  store ("belongs to a different writer") and the app ran with no account
  runtime, silently, for good; today the next sign-in of the same account
  self-heals it by adopting the survivor under a fresh writer
  (`account-replica-posture.md` § The store device principal → refinement 10) —
  which is exactly why the erase must still cover it: a signed-OUT user's
  data must not outlive the sign-out in a dir the next sign-in silently
  re-adopts. Measured on tui 2026-08-18 and fixed there; any new host owes
  the same base in its erase.
  **The `fauna-ffi` seat paid it 2026-08-19**, before hosting anything: its
  `account_state_erase_*` pair sweeps the W6 root beside the app base
  (`account-runtime.md` § Implementation status today → *Built — W3 the
  `fauna-ffi` seat*). ⚠ **That is inherited only by a shell that CALLS the
  shared pair, which is not automatic** — this section said "with no call-site
  change" for six days and it was never true of apple: `AccountStateDir.erase` /
  `.eraseAll` were a hand-rolled `FileManager` sweep of the app's own base that
  never reached the seat at all, so both apple targets carried the full
  stranding bug while the doc read as though they did not (found in review,
  2026-08-25; apple moved onto the shared pair in the same pass
  that landed its host). A shell whose erase does not
  call `account_state_erase_*` inherits nothing; that is the thing to check
  first when a new host lands. ⚠ **The residual was the sandboxed case, and it is
  CLOSED 2026-08-22:** the sweep used to resolve the desktop
  `StoreRoot::platform()` unconditionally, so a sandboxed host — which passes
  its own container to the runtime — would have swept a directory its store was
  never in. Both `account_state_erase_*` now take an optional
  `store_container_dir` and resolve it through one shared mapping identical to
  the runtime's (`Some(dir) → StoreRoot::at(dir)`, `None → platform()`), so a
  sandboxed shell hands the same container to both and the two can no longer
  disagree; every desktop caller passes `None` and is unchanged. Android is the
  first host to use it, and iOS took it 2026-08-25 with its own host.
  ⚠ **What the store-root sweep takes is the actor's WHOLE scope under that
  root, not the `account-store/` subdir alone** — and on apple that root is the
  app-group `sync` base, so a sign-out now also drops
  `<app-group>/sync/<actor>/`'s per-set state DBs and `device.db`.
  That is the ratified reading of this section rather than a
  side effect (they are account-scoped stores, and the user asked for their data
  off this device), it is what tui and linux have always done — their
  `SyncPaths::base_dir` delegates to the same `StoreRoot::platform()` — and the
  bindings themselves are nest policy, re-established on the next sign-in
  (`sync-agent.md` § Control plane split). (What the returning sign-in's
  `device.db` holds — and so which `sync_devices` row it comes back to — is
  owned by `sync-agent-credentials.md` § Credential model, the 2026-09-20
  ruling; this section owns only that the file goes.) Apple joining it is convergence onto
  the shared sweep, not a new disposition. **Both
  apple targets read that container from ONE accessor** —
  `AccountStateDir.storeContainerDir`, which `FaunaClient.startAccountRuntime`
  and both erases call — precisely so the runtime's root and the erase's cannot
  drift apart; macOS's is `nil` (there `StoreRoot::platform()` already resolves
  the user-domain base the co-located agent shares —
  `~/Library/Application Support/Fauna/sync`, out of the app-group container
  since 2026-08-25, `account-data-plane.md` § Placement), iOS's is its
  container named explicitly.
  ⚠ **A sandboxed app must not name that container after a directory its own
  flat layout already uses.** Android's is `<filesDir>/account-store`, *not*
  `<filesDir>/sync`, even though `sync` is the desktop root's name and the iOS
  twin's: those two resolve it inside a container of their own, while android
  has one `filesDir` where `sync/` is already a name its own flat layout uses,
  so a W6 root there would mean two things at once.
  **Web's account store is in the erase too — there is no browser carve-out (ruled 2026-10-01; advisory, refutable by the user).** Since web hosts the account runtime ([`../account-client-lifecycle.md`](../account-client-lifecycle.md) § The client-side lifecycle → *The trigger fired*, ruling (4)), each account has a store in the browser origin: one IndexedDB database and one OPFS segment directory, both named `StoreRoot::store_name(actor)`. A sign-out erases that store for every account it reaches, and removing one account erases that account's. Three reasons, none of them special to a browser. The replica rests readable: a browser has no keychain with a free unlock, so web is on the plaintext side of [`../account-replica-posture.md`](../account-replica-posture.md) § Local at-rest posture, and the rows it holds include key custody (the pre-login deployment-seed read needs only the actor id and the store). A browser is the seat most likely to be shared, the case `long-term-store.md` § Cleanup contract property 1 names. And the credential erase beside it already takes the store's writer key, so a kept store is the survivor the ⚠ above describes: a signed-out user's data in a store the next sign-in silently re-adopts. Keeping the replica so that the next sign-in is cheaper was weighed and refused; the saving is the same on every app and was refused there. Four decisions shape web's erase:
  - **The order is the native one, behind one durable decision: refuse, record, stop, wipe, erase.** The other-tab refusal comes first, unchanged (§ Concurrent instances → *Web owes the same refusal*). Then the sign-out is recorded (next decision). Then the runtime's sign-out stop is awaited: it retires the machine's enrollment and closes the store ([`../account-client-lifecycle.md`](../account-client-lifecycle.md) § The client-side lifecycle → ruling (4), *the teardown rider* owns it). Until that stop returns, the runtime can still write a credential slot the wipe is about to delete (the ⚠ *async writer* note below) and holds the database a delete would wait on (the ⚠ *open store* note). Then the credential wipe, then each account's store.
  - **The sign-out is decided once, durably, before its first await, and a load that finds the record finishes it.** The stop is the first thing a web sign-out waits for before its wipe, and a user who confirms Sign Out and closes the tab must not find the account signed in at the next visit. So the gesture first writes an install-scoped record naming the accounts it reaches (read from the registry then). Each account leaves the record when its store is gone, and the record goes when it is empty. A page load that finds the record wipes the credentials and erases the recorded stores before launch routing reads the registry. This is the crash-safe shape `nest/common.md` § Client-state recoverability asks of the nest, applied to the device: one decision point and a boot reconcile. The reconcile retires no enrollment (it has no session to do it over), which is the same best-effort outcome as an offline sign-out. The record carries two facts, because a load has to tell a sign-out that never wiped from one whose user has signed in again: *the credential wipe is owed*, and *the accounts whose store may still be here*. While the wipe is owed, no account reads as signed in on any tab (the identity read fails closed on the record), every account the registry names is reached, and the load runs the wipe before it erases. Once the wipe has run, an account the registry names again is left untouched and dropped from the record (decision 4's second carried-over rule), and the rest are erased at any load, signed out or not. A remove-account records its one account the same way, with no wipe owed and before the registry removal, so a removal the tab did not live to finish leaves either nothing changed or a store the next load erases. The gesture's own tail and the load run one routine, so there is one order to keep right.
  - **The erase's reach is the registry, as the refusal's is.** A web origin has one registry and no sibling app, so the accounts the registry names, plus the signed-in one, are everything a sign-out reaches: each account's credential slots, its actor-keyed `localStorage` state and its account store, through one per-actor erase (the wasm `account_scope` twin, § Implementation status today). The Web Lock the refusal probes is the conversations-engine role's, not the store's engine-singleton role's — web's store takes no serving lock, and that one probe suffices because only the tab holding an account's conversations-engine role hosts its runtime (§ Concurrent instances → *Web owes the same refusal through the lock it already has*, the two-roles ruling).
  - **The store half is a residue class on web.** `removeItem` cannot fail, but a database delete waits behind a connection another tab holds and an OPFS removal can be refused. The delete is bounded: one the browser answers `blocked` has failed at once, and one that says nothing has failed when the sweep's budget lapses. An account whose store is still present stays in the record, the user is told with the shared `EraseResidueView` copy, and a later load sweeps it again — the *residue surface* rules below, with the store name where a native record holds a path. A store the erase could not remove is left whole (its segment directory goes only once its database has), because a later sign-in may adopt it again. Two of those rules carry over in web's terms: the role-lock refusal is asked before a load's re-sweep — one recorded account another tab serves refuses the whole sweep, as natively, while the gesture's own tail does not ask again, having asked before it recorded — and an account the registry names again is someone's store again, left untouched and dropped from the record. The re-sweep runs at any load (decision 2); the `sign-out-residue` view is painted where a sign-out and a signed-out load land, on onboarding's `identity_choice`, and only while something is still left. Its Remove Again control is a third caller of the one routine the gesture's tail and the load run, asking the role-lock question first as a load does; a refused sweep — a load's or a retry's — erases nothing, keeps every planned account recorded and paints the retry's refusal line, as a refused native re-sweep does. **Two cases say nothing, and stay that way on web.** A load that lands signed in sweeps and paints no line: the view is `identity_choice`'s, *"Signed out, but …"* is false to a signed-in user, and the native rule is the same (a signed-in launch is not the user the residue was reported to); the record keeps the account, so the view is there when the browser next lands signed out with the store still present. A remove-account whose store erase fails says nothing at the time: no seat tells the user about a remove-account's residue (tui and linux drop the erase's survivor list), and the surface for it is a signed-in page, which is a cross-app decision and not web's to take alone; web already does more than the native seats, since the account stays recorded, every later load sweeps it again, and a later sign-out counts it in its line. ⚠ A browser cannot withdraw a blocked delete: the request stays queued and runs when the last holder closes, and an open of the same store in that page waits behind it until then. *Web is ruled out of this class* (below) now speaks for the credential half only.
  ⚠ **A store's own async writer can resurrect a slot the erase just deleted,
  and every new account-scoped writer that runs detached from the account's
  own lifecycle owes it the same guard.** An erase quiesces the writers it
  knows to stop first (`long-term-store.md` § Cleanup contract), but a result
  already in flight when the wipe runs — a network fetch, a status poll — can
  still land after it, and an unguarded write re-creates exactly the slot the
  sweep removed. The shared fix is the same no-op-once-gone-from-the-index
  check `save_authenticated` uses (`long-term-store.md` § Cleanup contract):
  consult the account index before writing, and no-op if the actor is no
  longer enrolled. Missed once on the per-actor family-status snapshot
  (`fauna/<actor>/supervision_snapshot`, `libs/fauna-client-accounts`), whose
  write raced the same post-sign-in trigger `save_authenticated` guards and
  carried no guard of its own; fixed at the shared-Rust writer so every
  consumer inherits it.
  ⚠ **The landing site can be another account's *live* session, not only a
  slot the erase deleted.** A result computed as one account and pushed
  through an app-global session slot lands wherever the slot points when it
  arrives, and after a switch that is the NEXT account's session. Windows'
  member-custody re-push (retired 2026-09-27 with every content-key push)
  resolved the content-key blob with the login's own secret (~5 s) but pushed it through the app-global sync-agent slot, and
  its post-resolve re-read asked only whether *some* agent was live — so a
  switch during the resolve replaced the next account's own bindings with
  the previous account's. The guard is an
  **actor-bound target** (the push leg answers only for the account that
  resolved, checked at fire time *and* again after the await), not an
  account-index check: that writer never touches a deleted slot, it touches
  a live one that is not its own. linux, macOS and tui bind resolve and push
  to one object and are exempt by construction; a writer that splits them
  across a login-captured secret and a global slot owes this binding.
  ⚠ **An OPEN store is an unerasable store, and only Windows says so.** Calling
  the shared pair, at the right root, with the writers quiesced, is still not
  enough if a handle on the store's own files is open: Windows refuses to delete
  an open file (`ERROR_SHARING_VIOLATION`, `os error 32`), so the sweep aborts on
  the actor scope it is standing in. POSIX `unlink` removes an open file happily,
  which is why this was invisible on linux/tui/apple for as long as it existed —
  those hosts really do erase the directory, and the still-open engine is left
  writing to an unreachable inode rather than to a store the next sign-in could
  re-adopt. So the *user-facing* invariant held there; on Windows it did not, and
  a signed-out user's conversations stayed readable on disk with the failure
  swallowed as a warning (measured 2026-09-03). Two
  consequences, both shared rather than per-app: **the release is explicit and
  refcount-independent** — `FfiNestClient::release_account_scoped_stores` hands the
  conversations engine's role over, which closes `mls.db` and releases
  `mls.db.lock` however many `Arc`s survive in a shell's object graph (dropping a
  foreign wrapper releases NOTHING: the Rust client holds its own stashes of the
  same session), and `SqliteStorage::retire` closes the connection rather than
  merely refusing later statements; and **the sweep no longer lets one root's
  failure spare the other**, because a `?` on the app base used to mean an
  undeletable `mls.db` also left the W6 account store untouched, in a root nothing
  had tried. A host owes the release call on every path that erases — sign-out,
  account switch, factory reset — the same way it owes the `account_state_erase_*`
  call itself; **windows, apple and android all call it now** — each scoped to the live-engine erase path only (apple:
  `StatusVM.signOut` → `APIClient.releaseAccountScopedStores`, macOS + iOS via
  shared FaunaKit; android: `ApiClient.clearAuth` → `releaseAccountScopedStores`,
  awaited alongside the shared `stopAccountRuntime` before `signOut`'s
  `eraseAllAccounts`, since android's engine retire and store erase are two
  separate synchronous calls with no shared transaction to order them for free).
  Android's factory reset and non-active-row remove erase no live engine
  (`AdminNestVM` drops but does not erase; `AccountSettingsVM.removeAccount`
  targets a non-active actor), so neither needs the release call — the same
  ruling apple's switcher non-active-row remove (`AccountSwitcherVM.remove`)
  already has. POSIX made android's engine-outlives-the-store gap invisible as
  a uniformity debt rather than a data leak, which is why it closed here rather
  than as a data-loss fix.

  ⚠ **A store that is still ASSEMBLING has no handle to release, and that is the
  window a sign-out actually lands in.** The release rule above assumes the host
  can name what to close; an account runtime spawned at the post-auth hook opens
  its database on its own OS thread *before* its start call returns, and the
  handle then has to reach the app — so between those two moments a live
  connection holds `account-store.db` while the app's own handle slot honestly
  reads "no store". A teardown that only *checks* that slot stops nothing and
  erases anyway. Measured `--app tui` on Windows, 2026-09-04: sign-out landed
  475 ms after the assembly minted this machine's writer key, and the erase
  failed with `os error 32` on the W6 root with nothing having been torn down. So the obligation is stronger than "release before
  erase": **a host must be able to WAIT for an in-flight assembly, under a named
  bounded budget, and erase anyway (loudly) if the budget lapses** — the same
  contract windows' `ReleaseAccountScopedStoresBeforeErase` applies to its
  settled stores, extended to the one that does not exist yet.

  **The wait is one shared mechanism, and every host reaches it** (converged
  2026-09-06). `fauna_client_account_runtime::stop_account_runtime` takes the
  settled handle, the assembly still in flight, or both, stops them under **one**
  budget (`ACCOUNT_RUNTIME_STOP_BUDGET`, 5 s — the user asked to be signed out,
  and waiting for an assembly and then stopping what it produced are two halves
  of one stop), and owns the two diagnostic lines the corollary below demands. It
  is shared because the wait existed only on tui, hand-rolled, and a second
  hand-written copy is where two apps start reporting a sign-out differently —
  the same argument the lifecycle host itself was extracted on. What each host
  still owns is **how to drive it**: linux spawns it on the account runtime's tokio handle (its sync agent's un-provision on the same task, ahead of it — built 2026-10-06; the GTK thread never waits for the agent's reply) and runs everything that must follow — the erase above all — as a GTK main-loop continuation of its completion (the erase
  is synchronous, so a spawned stop with nothing sequenced behind it is a race the erase wins; blocking the GTK thread instead held the main loop for the whole budget whenever the stop landed behind a prologue), the `fauna-ffi`
  seat awaits it, tui spawns it (its sync agent's un-provision and the session client's disconnect on the same task) and runs the erase, the next account's launch and the e2e ack as `App`-owned continuations released when the stop's completion crosses its UI queue, refusing input to the outgoing session meanwhile — both continuation hosts order that work through the one shared `StopQueue` (nothing queued runs while any stop is in flight; oldest first).
  **The budget holds only if the assembly's store-open stretch holds no nest
  leg** (2026-09-30). Until `start()`'s readiness barrier answers, the host has
  no handle, so a sign-out cannot raise its cut (`SIGN_OUT_PASS_GRACE`) — it
  can only wait the assembly out, and whatever that stretch spends with
  `account-store.db` open comes straight off the 5 s. The fleet bootstrap's two
  `put`s used to publish there, and while a fresh sign-in's principal still
  waited for its grant each took ~5 s to fail — measured `--app windows` and
  `--app tui` on Windows: the budget lapsed and the erase met the open store in
  about 3 of 10 e2e resets. So the barrier writes
  locally and the prologue's first step publishes, where the cut reaches it;
  a network leg added between the store open and `ready` re-opens this.
  **The in-flight half must be REGISTERED, not merely invalidated.**
  `AccountRuntimeHost` generation-guards the assembly window against *installing*
  under the next account, but invalidating a claim says nothing about the
  database that assembly already opened — so `begin` now registers the assembly
  and `take` hands a teardown both halves, and the host's payload moved from
  install time to claim time because the teardown that most needs it is exactly
  the one that finds an empty slot. Every UniFFI shell inherits the wait through
  `stop_account_runtime` with no per-shell code; what each still owns is the
  **ordering** — that call before its `account_state_erase_*`.
  ⚠ **tui holds the wait WITHOUT the host, and that is not drift.**
  `account-client-lifecycle.md` § The account store → *The client-side lifecycle* rules
  tui a deliberate non-consumer of the lifecycle slot (its supersession guard is
  an actor-id match against the live session, and its handle is `App`-owned by
  that app's no-globals shape). Same contract, one shared wait, two correct
  places to keep the handle — the convergence owed here was the *wait*, never the
  slot.

  ⚠ **Corollary for whoever debugs the next one: the erase must SAY what it
  did.** `erase_actor_state` discarded its `remove_dir_all` result until
  2026-09-04, so the single outcome the function exists to prevent produced no
  line at any level, and "the directory survived" could not be told apart from
  "the directory was re-created afterwards" without a second full e2e cycle. It
  now logs the removal at info and the failure at warn with the path and the
  error, and returns the survivors; a host's own teardown owes the matching line
  (present = something else holds the store, absent = this teardown never ran),
  because those two diagnoses are indistinguishable from the test's assertion
  alone.

  **The same duty reached the sibling sweep on 2026-09-09, and closed a worse
  defect on the way.** `erase_all_account_scopes` — the sweep a *sign-out*
  actually runs, where `erase_actor_state` is the per-account one — did not
  merely discard its outcome: its actor-scope loop returned on the first failing
  `remove_dir_all`, so ONE undeletable file left every later scope under that
  base unattempted. That is the same early-abort already fixed one level up (the
  FFI wrapper deliberately runs both roots), hidden one level down where it cost
  more. It now sweeps every scope regardless, and returns an `EraseSweep`
  {`erased`, `survivors`} — `#[must_use]`, because a dropped sweep is precisely
  the defect. **Empty `survivors` is the only outcome that means the device is
  clean.**

  **One level further down, 2026-09-30: a held file costs the erase only
  itself, and the log names it.** `std::fs::remove_dir_all` returns on its first
  failing entry, so within ONE scope a single file some live handle still held
  (SQLite opens without `FILE_SHARE_DELETE`) stranded every sibling file too, and
  the warn line named only the scope directory — never the holder. Both erase
  legs now go through `remove_tree_naming_held`: after the fast path fails it
  walks the tree removing everything that can go, and the warn line lists the
  entries that could not (capped). The survivor stays the *scope directory*
  (`survivors.len()` is still the count the user is shown); the held-file names
  are for whoever reads the log. It named its first holder the day it landed: a
  sync agent that the e2e `reset`/`logout` arms never un-provisioned, because they
  dropped the agent session before asking it to un-provision — the ordering
  § Concurrent instances' agent bullet already required, now shared by every
  windows erase path (`ErasePrecondition`).

  **The FFI export stopped failing on 2026-09-09, and that is the point.**
  `account_state_erase_all_scopes` returned `Result<u32, _>` with the survivors
  encoded into the error message, which is exactly why all three FFI seats
  handled the outcome in a `catch` and painted a clean "Signed out" on the path
  that fell through. It now returns `FfiEraseSweep` {`erased`, `survivors`,
  `residue`} — the outcome on the success path, where the paint is — and has no
  failure mode of its own, because an `Err` arm that cannot occur is a lie about
  which outcome is exceptional. `residue` is the shared `EraseResidueView` with
  `owes_work` already answered and **no path field at all**: a type that cannot
  express a path cannot leak one onto a user's screen. A seat hands the sweep
  to the residue face's `sign_out_residue_record` (§ *the residue surface*,
  below) and paints what comes back.
  `erase_sweep_fold` folds several sweeps into the one answer a user is owed
  about their device, for a host whose app base is not a single directory
  (android's `filesDir` + Room dir) — because adding up survivor counts is where
  a seat re-derives `owes_work` in a fourth language.

  ⚠ **Survivors fold DEDUPED by path, and every caller that erases overlapping
  trees must fold through `EraseSweep::absorb` rather than extending a `Vec`.**
  `survivors.len()` is the number the user is shown, and a survivor is a
  *location*: the same directory found by two passes is one fact about the
  device, not two. Callers that overlap on purpose exist — linux runs a
  defensive flat-legacy sweep across the same base its per-actor loop just
  walked — and linux told the user *"2 item(s)"* about one undeletable scope
  until it folded through `absorb`. Over-reporting is the safer of the two
  directions, which is exactly why only an end-to-end assertion on the rendered
  line caught it.

  ⚠ **A seat whose sign-out surface re-renders from its own state must PIN the
  line, not paint it once.** linux hands the user a freshly built onboarding
  wizard, and that wizard mirrors its machine's `error_message()` onto the same
  `error-message` banner on every tick — so the residue line, written straight
  to the label after the window was built, was wiped microseconds later. The
  line was in the code, the unit tests covered its wording, and the user still
  saw a clean sign-out; only the journey test below saw it. linux pinned it as a
  sticky notice the banner render fell back to until the residue got a surface
  of its own (below), which it now paints from state the wizard does not own.
  Any seat lifting this leg owes the same question: *what else writes this
  surface after I do?*

  **Telling the USER is the erase's own duty, and it is one decision for every
  seat.** `principles.md` § The user always controls their data puts the delete
  affordance in the app, and a log is not an affordance: a seat that turns the
  failure into a log line and paints a clean "Signed out" has made *proceeding*
  indistinguishable from *succeeding*. So a sign-out that leaves survivors owes
  the user a line, and every word of that line is
  `fauna_client_accounts::EraseResidueView`'s — an app localizes it and declares
  which remedy it can honour, and decides nothing else. Three rules the seats
  share:

  - **The count goes to the user; the paths go to the log.** A user is owed the
    fact and the remedy; `%LOCALAPPDATA%\fauna\<64-hex>\mls.db` is neither, and
    the log is where the corollary above already sends it.
  - **A clean sweep says nothing.** *"0 items were left behind"* is reassurance
    by vacuity, refused here as it is for the group sweep.
  - **The line names only a control the app actually paints** — Remove Again,
    which every seat paints beside it on the residue surface (below). While the
    surface was rolling out, an `EraseRetryAffordance::Absent` arm gave a seat
    without the control the remedy it could reach instead; it was a declared
    parity gap, and it retired with the last seat (macOS / iOS, 2026-10-03)
    together with its three `*_no_retry` strings.

  **The credential half is a residue class too.** A sign-out is two erases —
  the account-scoped stores above and the credential namespace
  (`long-term-store.md` § Cleanup contract owns its mechanics) — and the line
  answers for both. The credential store's delete reports nothing on any arm
  (the keyring warn-logs, apple drops its keychain status, android and windows
  swallow), so the credential erase reads back every key it deleted and returns
  what still reads (`fauna_client_accounts::CredentialSweep`), plus whether a
  wholesale namespace wipe failed — the one residue a read-back cannot see,
  because a store that refuses the wipe for being unreachable refuses the read
  the same way. `EraseResidueView` carries it as a path-free
  `credentials_survived` flag beside the survivor count, `owes_work` covers
  both, and the copy names *"your sign-in credentials"* — never a key name,
  never a count. Two rules follow:

  - **Build the line after BOTH erases.** The filesystem erase must run first
    (it reads the registry the credential wipe destroys), so a line built where
    it runs answers for half a sign-out — which is how linux and tui painted a
    clean "Signed out" over a keyring still holding the identity seed until
    2026-09-13.
  - **Fold after the LAST wipe.** A seat whose platform store resets itself
    after the registry erase (android's `SecureStorage.clear()`) re-asks with
    `reverify_erase` first, or it reports credentials that reset already took.

  **Web is ruled out of this class, not behind on it**: its credentials live in
  `localStorage`, whose `removeItem` has no failure a later read could disagree
  with (an inaccessible storage refuses the read the same way), so web's line
  never names credentials. Its account-store half is a residue class since the
  2026-10-01 ruling (the web paragraph above).

  **The residue surface — its own view, its own retry, and a record that
  outlives the process (IDs user-approved 2026-09-25).** ui.yaml's `sign-out-residue` view (holding `sign-out-residue-message`
  and `sign-out-residue-retry-button`) is an optional element of
  `identity_choice` — where every sign-out and every signed-out launch lands —
  and not of `global`, whose warning/info lines are specified for
  *authenticated* pages only. It is present exactly while the residue owes work,
  and its line is the `Rendered` copy, naming the Remove Again control beside
  it. The residue is on the disk, so what the user is told about it must not
  die with the window: the sign-out writes it into the app's install-scoped
  state as a `fauna_client_accounts::SignOutResidue` record
  (`sign-out-residue.json` under the install base — the survivor paths and
  credential key names the re-sweep needs, which stay on the device and never
  reach the screen), a clean sweep deletes the record, and a **signed-out
  launch re-sweeps it silently first** and paints the view only if something is
  still left. The retry control and the launch run one shared re-sweep,
  `fauna_client_accounts::retry_sign_out_residue`, and four rules make it
  safe — because the one time a launch re-ran the sign-out's own sweep over the
  shared account-store root, it erased a running sibling's live store (see the
  routing bullet above):

  - **Only the recorded paths.** A recorded actor scope is removed whole; a
    recorded base (one the sign-out could not list) gets the sign-out's own
    actor-scope sweep and nothing more. A re-sweep never re-lists a base for
    scopes the sign-out did not leave behind.
  - **The sign-out's own refusal, asked first.** Every account a recorded path
    belongs to is put to the serving-lock probe `sign_out_blocked` asks (§
    Concurrent instances → *An erase refuses while a sibling serves the
    account*); any one served refuses the whole retry, erasing nothing, and the
    line becomes its own refusal
    (`settings.sign_out_residue_retry_blocked_other_window`).
  - **A path written since it was recorded is someone's store again.** Each
    recorded path carries its modification time from just after the erase gave
    up on it; a path that has moved since — a later sign-in on this app, or on a
    sibling sharing the store root that no serving lock can see because it is
    not running — is left untouched and dropped from the record.
  - **Credentials are re-erased only when they survived.** The seat's own
    credential erase (the one its sign-out ran) is re-run with a read-back of
    the recorded keys, and only when the recorded credential half is not clean.

  Every seat paints it (§ Implementation status today); the line rode the
  onboarding `error-message` first, before the surface existed, and no seat
  puts it there any more.

  ⚠ **There are FIVE seats, not four.** The four the corollary above names
  (windows / apple / android / linux) all reach this through
  `account_state_erase_*`; **tui is the fifth and reaches it by another route** —
  its own `account_scope::erase_under` over the shared per-actor
  `erase_actor_state` — so a survey of the FFI export's callers misses it
  entirely. tui dropped that function's survivor list until 2026-09-09. Note the
  two triggers differ in whose defect they are: a store we failed to release is
  ours to close, but an antivirus scanner or search indexer holding a handle
  produces the same `os error 32` with nothing wrong on our side — which is why
  the answer is a
  witness the user can see, not more closing.

## Serialized switching — completing the isolation contract

*Moved 2026-09-28 to [`account-scoping-dispositions.md`](account-scoping-dispositions.md) § Serialized switching — completing the isolation contract:* the remediation for actor-blind surfaces — **per-account paths, uniformly on all seven apps** (ratified 2026-07-22) — with the MLS-state and content-store placements, the class-4 carve-out, the split of preferences by meaning, the retired first-adopter adoption, and the never-delete rule for the identity secret. The contract it completes, and its in-memory corollary, stay here in § The scoping taxonomy.

## Concurrent instances — ratified design

**The instance unit is (OS login, account); whether a second same-account
instance may RUN is per app — the app's `ServingMode` (re-ratified
2026-08-15, the W5.6 build).** A **retired** app serves
`ServingMode::Concurrent`: any number of same-account instances coexist
over the multi-process-safe account store
([`../account-runtime.md`](../account-runtime.md) § Multi-instance
concurrency), exclusivity narrowed to the **three** genuinely exclusive
critical sections — schema migration, the engine-singleton role, and the
conversations-engine role (the role lock beside each `mls_state.db`,
file-derived `mls_state.db.lock`; mechanism owner: that § of the
charter). An **un-retired** app keeps the prior exclusive-or-die law
unchanged: two instances of the same account are refused — **by the shared
lock alone**. No platform guard was ever re-keyed per account, and none
carries the refusal: windows' `Local\FaunaApp-SingleInstance` mutex is a
fixed app-wide name and linux's GApplication singleton a per-(OS login, app)
bus name; both **survive unchanged as the plain-launch raise layer** (§
*Platform single-instance affordances survive as the plain-launch raise
layer*, below), and a bound launch opts out of them entirely. What is keyed
per (OS login, account) is the shared lock and the per-account *raise*
endpoint — never the platform's own single-instance name. (Corrected
2026-08-24 during windows' retirement leg: this sentence had read "the
re-keyed per-(OS login, account) platform guards", contradicting both
[`windows.md`](windows.md) § App Lifecycle — "the pre-existing mutex … is
**kept**, not replaced" — and this doc's own raise-layer paragraph, and it
was the premise a queued track had been carrying.) **Retirement status: tui
RETIRED 2026-08-15**
(the lead app — W5.6's retirement leg, proven by the two-instance
coexistence e2e in `test_account_instance_lock_tui.py`); **linux RETIRED
2026-08-15** (same day, trickle-down leg — proven
by the twin coexistence e2e in `test_account_instance_lock_linux.py`, the
switcher's spawn button now rendering on every row including the served
account's); **macOS RETIRED 2026-08-16** (trickle-down leg — proven by the twin coexistence e2e in
`test_account_switcher_apple.py`, the switcher's spawn button now rendering
on every row including the served account's); **windows RETIRED 2026-08-24**
(trickle-down leg — proven by the coexistence e2e in
`test_account_instance_lock_windows.py`, which replaced the same-account
refusal case it used to assert, with the switcher's spawn button now
rendering on every row including the served account's); ios/android are
structurally one instance per app; web's tabs are already
concurrent-instance hosts (below).

**Windows' leg carried one step the others did not.** It is the only app that
reaches the holder over UniFFI rather than depending on
`fauna-client-accounts` directly, so retiring it turned that wrapper's
pinned `Exclusive` into an FFI-visible parameter
(`fauna_ffi::accounts_registry::FfiServingMode`). **With windows retired,
every app that ever refused a second same-account instance has retired it** —
tui, linux, macOS, windows — **and the `Exclusive` serving mode is removed**
(the compat-remnant sweep's early pass: no installation predates the
retirement, so there is no old binary left for the version-skew interlock to
protect). `ServingMode` and `FfiServingMode` carry the one `Concurrent` arm;
the exclusive *lock* survives only as the probe — the sign-out gate's brief
exclusive acquire and `is_served` — and `AlreadyServed` still fires against
that holder.

**The 2026-08-15 ruling, executed tui-first the same day (ruled by the
store-safety inventory that gated W5.6; built by W5.6):**
exclusive-or-die narrowed to the three critical sections; the per-account
raise channel survives as plain-launch UX — a plain colliding launch
still routes to raise/the chooser, because a second same-account instance
is an *explicit* act (a bound launch, or the switcher's spawn) and never
the accidental outcome of an icon re-click; and the launch-collision
refusal path dies per app, **only after every shipped conversations
engine acquires the role lock** (ordering load-bearing — satisfied by the
W5.6 pre-step, which put the lock in the shared engine-construction path
before any refusal died). Two consequences fixed by the same ruling: the
switcher's "open as new instance" non-active-rows-only restriction lapses
per app with that app's retirement — a spawn for the served account
becomes an ordinary bound launch; and bearer-only processes hosting no
MLS engine (the sync agent) were sanctioned same-account co-residents all
along — they take no app surface and never held this lock.

**apple has no app-level guard of its own to re-key** (verified against code
2026-07-22): neither app target ships an instance lock, and
`Fauna-macOS/Resources/Info.plist` declares no `LSMultipleInstancesProhibited`.
macOS's de-facto single-instance behaviour is LaunchServices refusing to
re-launch a bundle identifier (`social.fauna.fauna`) that is already running
— *outside* our code, keyed per (OS login, bundle id), and bypassed entirely by
launching the executable directly, which is exactly what the e2e driver does
(`drivers/macos.py` launches the bare binary, not the `.app`). So apple's
re-keying leg is not "change the guard" but "**add** one": a
(OS login, account)-keyed lock the app itself takes at launch, over the same
install-scoped base the mutation lock uses. iOS is out of scope by
construction — the OS admits one instance per app and offers no multi-instance
concept, so concurrent instances on apple means macOS.

**The lock mechanism is shared** — every platform's leg consumes
`fauna_client_accounts::AccountInstanceLock` (FFI:
`acquire_account_instance_lock_shared`), so "is this account already served?"
has one implementation, not seven. **Every server takes the lock shared (the
one `ServingMode`, W5.6):** a second same-account instance coexists, never
queued and never refused, and the shared hold is what keeps `is_served`'s
display-only exclusive probe truthful for the chooser and the raise channel.
An exclusive holder (the sign-out probe's brief acquire) refuses every shared
acquire, and any shared holder refuses an exclusive one. A shared acquire
absorbs the probe's two-syscall exclusive window with a bounded millisecond
retry (it would otherwise wrongly refuse a legitimate co-server), and a
persistent block is the honest refusal — an exclusive holder really is
serving. The lock is crash-safe by construction (kernel file
lock, released when the holder dies — no stale-lock reconciliation at boot),
the per-account lock file a never-deleted sibling of the actor state dirs at
the install-scoped base (account-erasure sweeps must not unlink a held lock —
unlinking re-opens the race), and degrading **open** on I/O failure (a
filesystem hiccup must not become an app that refuses to launch; the platform
logs the unguarded proceed). An app acquires at the point its session account
resolves — before opening any of the account's scoped state — reuses its held
lock across same-account session rebuilds (the kernel treats a second open as
a competing owner), swaps locks on a cross-account switch, and treats a
refused acquire under the same terminal contract as a refused binding.

**Platform single-instance affordances survive as the plain-launch raise
layer (ratified 2026-07-22, linux's leg).** Every desktop platform already
has a per-(OS login, app) single-instance affordance with a friendly
raise-the-existing-window behaviour — macOS LaunchServices per bundle id,
linux GApplication's D-Bus bus-name uniqueness, windows'
mutex-plus-activate-event. Re-keying does **not** delete that layer: it stays
in force for every launch that carries **no binding** (the desktop-icon /
tray / notification channel), which is what preserves raise-on-relaunch for
the ordinary user. A **bound** launch always runs as its own process —
opting out of the platform affordance where the platform requires it (linux
adds `NON_UNIQUE` exactly when `FAUNA_BOUND_ACCOUNT` is set) — and the
shared lock, not the platform affordance, is the guard that decides whether
the account is free. The two layers never disagree, because they answer
different questions: the platform layer redirects *unbound* relaunches to
the existing window; the lock answers "is this account served?" — refusing a
second same-account session on an un-retired app (and a refused bound launch
gets the terminal contract, not a raise: there is no coherent window of "the
other instance" to raise for a binding), while on a retired app a bound
same-account launch simply runs as a co-server.

**The per-(OS login, account) raise channel (ratified 2026-07-23).** The
plain-launch raise layer above reaches only the instance that owns the
platform's app-wide single-instance name — and a bound instance deliberately
owns none (it opted out), so without more, "focus the instance serving
account X" has nothing to call when X's server is a bound sibling. The
ratified shape: **every serving desktop instance — plain and bound alike —
additionally claims a per-account activation endpoint on the platform's own
activation channel, its name derived from the account it serves.** Nothing
is registered anywhere and no rendezvous state is written: the endpoint
name is *derivable* by any would-be raiser, and liveness is endpoint
ownership itself, which dies with the process exactly as the instance
lock's flock does — the same crash-safety argument as the lock, and the
reason a rendezvous file beside the lock (the rejected alternative) loses:
a file needs staleness reconciliation and a lock-then-read protocol; an
owned name needs neither. (The other rejected alternative, a per-account
*app id*, is ruled out outright — the app id anchors the desktop-entry /
icon association and the Flatpak `--own-name` grant, and the plain-launch
affordance layer must stay keyed on exactly one of it.) The account token
in the endpoint name is the same normalized actor token the per-account
lock file uses — one shared derivation in `fauna_client_accounts`, so lock
files and endpoints can never key differently. Per platform: **linux**
additionally owns the D-Bus well-known name `social.fauna.fauna.a<token>`
(the `APP_ID` itself never changes; the Flatpak manifest needs no grant for
it at all — the name is a subname of `$FLATPAK_ID`, which the default
session-bus policy already admits since the app-id convergence of
2026-08-22, `../installers/linux-desktop.md` § Flatpak — and the claim is a
manual bus-name own so it works identically under `NON_UNIQUE`); **windows** creates a per-account named
activate event (`Local\FaunaApp-Activate-<token>`) beside its app-wide
mutex-plus-activate-event pair; **macOS** has no colliding process to
raise from (LaunchServices redirects icon re-clicks; the switcher button
is its only entry), so its leg is deferred until an affordance needs one;
**tui** neither claims an endpoint (no window manager can raise a terminal
app) nor raises — its focus-existing exit prints where the account is
served and exits. Raise semantics are best-effort with an honest degrade:
a raiser resolves the would-be account's endpoint and sends the platform's
activate; if the endpoint is unowned (the sibling died between probe and
click, or the serving app is endpoint-less, e.g. tui), the raiser
re-probes the lock — no longer served → continue as a plain launch; still
served → surface the no-channel case on `error-message` rather than exit
into nothing. Focus-existing therefore targets the per-account endpoint
*uniformly* (plain and bound servers alike), with the app-wide channel
remaining what it always was: the platform's own redirect for unbound
relaunches. **The add-account forward stays on the app-wide channel** —
the wizard belongs to the primary, and only a plain instance owns the
app-wide name — with one rule closing its no-owner case: a colliding
*plain* launch that finds the app-wide name unowned runs the wizard
itself, because it is at that moment the install's only plain instance and
hence the primary.

Launching a second instance bound to a different
account is a app-UI affordance, never a hand-edited flag: the spawning
instance passes the chosen account to the new process as launch wiring
(bucket-1 IPC under the one-config-surface invariant). **Two complementary
surfaces are ratified (user-approved 2026-07-22; IDs are ui.yaml's domain,
§ settings + § onboarding `launch_instance_chooser`):**

- **The running instance's surface — the switcher row's "open as new
  instance" button**. On an un-retired app: non-active rows only (a spawn
  for the account this instance already serves would be refused by the
  exclusive instance lock, so the affordance never offers it); the
  restriction **lapses per app with its W5.6 retirement** — a retired app
  offers the button on every row, a spawn for the served account being an
  ordinary bound launch. The spawner sets `FAUNA_BOUND_ACCOUNT` on the
  child; the child owns its own binding outcome, re-auth included. This is
  macOS's only entry point: LaunchServices activates the running app on
  icon re-click rather than starting a process (a Dock-menu "New window
  as…" may join later).
- **The colliding instance's surface — the launch-collision chooser**, for
  platforms whose OS starts a second process on icon re-click (windows,
  linux, tui; each wires it with its re-keying leg). A **plain interactive**
  launch that finds its would-be account already served renders a chooser —
  "already running as X; launch as…" — listing the registry's
  not-currently-served accounts (a display-only probe of the per-account
  lock files; arbitration stays at acquire). Picking one makes **this**
  process the chosen account's bound instance — `bind_account` + the bound
  launch seam, no third process. It also offers **focus-existing** (raise
  the running window and exit — subsuming the pre-re-key raise-on-relaunch
  UX of the platform guards) and, **on windows/linux**, **"log in as a new
  user"**, which *forwards* an add-account intent to the running instance
  and exits: the onboarding scratchpad belongs to the primary, so a
  colliding process never runs the wizard (the same ownership rule as the
  bound-wizard refusal). **tui's chooser omits the add-account forward** —
  it has no app↔app IPC channel to carry the intent, and building one was
  rejected as disproportionate to this single affordance (user decision
  2026-08-01; declared absence owned by `tui.md` § Declared platform
  absences, ui.yaml scoping in `platform_elements`). The chooser is
  strictly a human affordance and is never rendered for a
  `FAUNA_BOUND_ACCOUNT` launch — wired IPC must be deterministic: on an
  un-retired app a bound collision stays terminally refused; on a retired
  app it is an ordinary bound launch that coexists.

**The wiring channel is `FAUNA_BOUND_ACCOUNT=<actor-id-hex>` in the child's
environment** (shared: `fauna_client_accounts::requested_bound_account`, FFI
`requested_bound_account`) — one channel for all seven apps rather than a
per-platform argv/env/pipe choice (priority #1), and deliberately the same
channel every app already uses for launch wiring, so nothing new is
invented. Unset or empty means an ordinary primary launch. Two rules bind the
consumer: the value is **normalized but not validated** where it is read
(`bind_account` stays the single gate, so a malformed value is refused as
`UnknownActor` rather than quietly dropped); and an app that finds a binding
must **launch bound or refuse** — never fall back to a plain launch, which
would put a second window on the *active* account and, for a flagged account,
walk past the re-auth the flag demands. A `ConfirmationRequired` refusal is the
one recoverable case: the app runs the same native re-auth its switcher runs
and retries `bind_account_confirmed`.

**The active pointer decouples from the session.** `fauna/index.active`
(owner: `long-term-store.md`) remains the single store-level pointer, but its
meaning narrows to two jobs: (1) the account a *plain* launch binds to, and
(2) the owner of the machine-singleton sync surfaces (below). A secondary
instance binds its session to its chosen account at launch and holds it in
process state; it does **not** move `active`, does not run the boot
legacy-mirror, and treats the index as read-mostly. An explicit switch (the
existing switcher affordance) still moves `active`, exactly as today. (The
legacy single-slot mirror this paragraph once kept in step retired
2026-09-24, and its transitional native-app survival 2026-09-28 —
`long-term-store.md` § Downgrade mirror + abandoned-append recovery.)

**The binding follows the account across a succession (ratified
2026-08-27).** A binding names an **account** by the actor id that identified
it at launch, and bound-or-refuse holds every later session build to that
id. A succession ceremony run from a bound instance re-points the account to
a successor and then switches to it — the same account, a new identity — so
the process that adopts the successor re-points its own launch binding
first, and the successor's session passes the gate exactly as the
predecessor's did; the retired id is what a bound launch refuses from then
on. The rule is shared, not per-app: it fires inside
`AccountRegistry::record_succession` — the one seam every adopter of a
successor crosses *before* it switches (tui's `adopt_successor`, the FFI
ceremony windows and apple ride, wasm's) — so no app carries a copy and none
can forget it (`fauna_client_accounts::rebind_session_launch_after_succession`).
Only a binding that names the retired id moves: a plain launch stays plain,
and a binding on an unrelated account is untouched. Without this the gate
refused the ceremony's own switch and the bound instance exited
mid-ceremony — not data loss,
since the successor seed is persisted before the sweep runs and a relaunch
comes back as the successor, but the closing act's shown-once kit render and
the in-memory sweep view the retry affordance reads died with the process;
and it was precisely the second same-account instance, the one device whose
refused engine produces a `no_engine` sweep, that could not survive producing
it. Two riders. **(1) This switch DOES move `active`**, the one exception to
the paragraph above: a succession is the one switch a secondary performs by
itself, and the retired identity can no longer authenticate anywhere, so a
store pointer left on it would only strand the next plain launch. **(2) A
bound launch whose named id has a recorded successor in this install's
registry binds to the terminal successor (ratified 2026-08-27, closing the
question rider 2 first queued).** The rule is the
same one as above, met from the other side: a spawn minted before this
install could observe a succession — a sibling seat ran the ceremony, a
phrase-only restore persisted the successor's row — names an account by an
id that is now retired, and the account is the successor. So
`AccountRegistry::resolve_launch_binding` walks the registry's
`succeeded_by` chain forward (`terminal_successor_of`) and re-points the
process binding, and every app calls it once where a registry is first in
hand and *before* the session account resolves from the binding; the holder
stays registry-free (it runs before any scoped state opens) and simply meets
the successor. **Refusal was rejected**: the spawner named an account, not
an id, and a refusal would strand a spawn the spawner cannot correct. Per
app: **tui** at `session::stored_account`/`session::launch_persistence`
(`session.rs`, called from `launch::start` and `launch::route`) — since tui's
own bound session build landed (2026-09-01, closing
`second-identity-in-its-own-window` outcome 2), it
calls `resolve_launch_binding` directly, same as macOS below (before the
build, tui's session resolved from `active` unconditionally, which happened
to agree with rider (1)'s successor-repointed `active` for THIS case only —
a coincidence, not the intended mechanism, and it disagreed the moment the
bound and active accounts genuinely differed); **linux** at
its bound session material / launch persistence reads (`main.rs::session_binding`)
— until then it resolved the retired id, passed the holder, and met the
nest's `superseded` refusal on connect; **macOS** at its bound-launch
resolution point (`FaunaAccounts.resolveLaunchBinding`) calls
`registry.resolveLaunchBinding()` in place of the bare environment read, so a
spawn minted with a retired id serves the successor instead of meeting the
nest's refusal; **windows** makes the same one call at its bound launch
(`App.xaml.cs`, `FfiAccountRegistry::resolve_launch_binding`, re-reading the
binding after it). iOS has no bound launch, so it owes nothing here.
A running bound instance whose
account is succeeded from another device *while it runs* is unchanged: its
next connect takes the own-device-fleet path
([`../../behavior/succession-aftermath.md`](../../behavior/succession-aftermath.md)
§ Propagation).

**Session identity resolves through the session's account (ratified
2026-07-22).** A process resolves its **session account** once, at
launch-binding resolution — the bound actor for a secondary instance, the
store-active account for a primary — and holds it in process state. From then
on, **every session-path identity read resolves through the registry for that
account**: the per-actor secret slots plus the account's index-entry
server-data cache, packaged as one shared per-account material read exported
over FFI/wasm — never a direct read of the legacy single slot. The rejected
alternative — scoping each read at its call site — is how the blocker arose
in the first place: those reads were written when "the current identity" and
"the active account" were the same thing, and per-call-site scoping keeps
account-correctness a per-call-site obligation forever, with no structural
point at which to enforce that a bound process never touches another
account's material. One accessor makes a wrong-account read unrepresentable
on session paths. For a primary instance the change is behavior-preserving by
construction: the shared read resolves the active account's own slots, which
is exactly what the retired single-slot mirror used to reflect
(`long-term-store.md` § Multi-account evolution), so a plain launch
reads the same values it always did — which is what makes the migration
mechanical on all seven apps. Three corollaries:

- **A secondary instance never enters the onboarding wizard.** Onboarding
  reads and writes the legacy scratchpad, which belongs to the primary
  (`long-term-store.md` § Multi-account evolution → the legacy keys' two read
  paths); a bound launch whose machine lands on a wizard entry refuses and
  exits, under the same terminal contract as a refused binding — never a
  fallback to a plain launch. The read-only blocking surfaces
  (identity-changed, offline / needs-update) render normally when bound.
- **Session-path writes and deletes follow the same seam** — per-actor slots
  through the registry, never a platform-side key of the app's own. (While the
  retired single-slot mirror existed, an app deleting a mirrored key directly
  was drift with teeth: the next boot re-mirror resurrected exactly what the
  delete removed — concrete instances in § Implementation status today.)
- **The launch machine's snapshot stays identity-free.** The observable
  snapshot is deliberately serializable state-without-material; the session
  is built from the store via the session account, never off the machine's
  observable.

**Sync surfaces stay active-account-only in the first delivery (ratified
2026-07-22).** File Provider domains, cfapi sync roots, and the per-OS-user
sync agent continue to serve exactly the store-active account; a secondary
instance runs the full UI/messaging/mail surface but does not provision sync
hosts (its folders are simply not served locally while it is
secondary). The serialized-single-account consequences in `file-sync.md`
§ Multi-account × File Provider remain in force unmodified, and the
`foreignDomainOwner` guard stays correct as-is. **The follow-on
("concurrent sync hosts") is a declared, separately-gated evolution:**
per-account capability slots with domain→owner routing and a multi-capability
agent. Its enabling mechanisms already exist (per-actor sync-state scoping;
the app-dead renewal grant in `sync-agent.md`), and the 2026-07-19 rejection
of per-account slots was reasoned from the switching-only model ("no live
session to renew a switched-away identity") — a premise concurrent instances
removes. That rejection is therefore **stage-scoped, not timeless**; revisiting
it requires its own ratification pass (amending `file-sync.md` § Multi-account
× File Provider) plus a security re-review of the credential-slot and
routing surface. Until then, single-slot is the law.

**Web (tab-pinning built 2026-09-01).** The browser is already a
concurrent-instance host: tabs share one origin store. Until this leg, every tab
read the (since-retired) legacy single slot — which the boot mirror rewrote from
the *global* `active` pointer on every page load — so tabs converged onto
whichever account was activated last and could not hold distinct identities. The
decoupling is the
same as native: **a tab binds its session to a chosen account and reads that
account's `fauna/{actor}/*` slots directly, leaving `active` and the legacy
mirror to explicit switches.**

The binding is a **per-tab pin**, web's twin of the native
`FAUNA_BOUND_ACCOUNT` launch wiring. A tab has no environment, so the pin lives
in `sessionStorage` — per-tab by construction, where `localStorage` is the
shared-origin store whose sharing is the problem. Normalization mirrors
`parse_bound_account` (trim, lowercase, explicit-empty reads as unpinned), and
validation stays at the single gate: the wasm `sessionMaterial()` read fails
closed for an unknown or removed account rather than falling back to another
account's material, exactly as `bind_account` refuses a malformed binding as
`UnknownActor`. **Rider 2 holds unchanged** — a pin names an *account*, so it is
walked through `terminal_successor_of` at boot and re-pointed at the terminal
successor, over the one shared chain walk rather than a second implementation
(the native `resolve_launch_binding` is `cfg`-ed off wasm because it reads a
process cell; the rule is not).

**Which tabs mirror.** An unpinned tab is the *primary*: it runs the boot
legacy-mirror exactly as before, then pins itself to what it resolved — that
self-pin is what makes it immune to a sibling tab's later switch. A tab pinned
to a **non-active** account is a genuine secondary and does **not** run the
mirror (it would move the store-active account's downgrade view under every
other reader). A tab pinned to the account that is *already* active is the
primary case wearing a pin, and still mirrors — which keeps the abandoned-
append-mode self-heal alive for single-account users, whose behaviour is
therefore byte-identical to before pinning existed. An explicit switch moves
`active`, re-mirrors, and re-pins the switching tab: per-tab account choosing
rides the same switcher affordance (element IDs are ui.yaml's domain, which
scopes `launch_instance_chooser` and `account-open-new-instance-button` away
from web — tabs *are* web's instance story, so there is no second surface to
build).

**What launching the app again means on web (ratified 2026-09-26, user
reading).** The desktop apps answer a
second plain launch with the chooser or by raising the instance already
running; web has no process to collide with and no handle on its sibling tabs,
so its second launch is **a new tab of the profile**, and the promise reads
"comes back up on the identity already running": the new tab resolves the
account already active, pins itself to it, and its switcher offers the other
identities — which is the primary-tab boot above, seen from the launch side.
Web does **not** raise the tab that is already open, and this ruling does not
ask it to; a browser-side `launch_handler` that focused an existing tab would
be an addition, not a debt. Witnessed by `test_account_tab_pin_web.py`'s
second-launch case, the feature catalog's `second-identity-in-its-own-window`
outcome 3 for the web column.

**What "the place you asked to go back to is already gone" means on web (ruled 2026-09-28, fable; advisory — refutable by the user).** The native promise is the chooser's focus-existing degrade above: the instance the user asked to go back to has exited, so the click re-probes the lock and continues as a plain launch. Web has no chooser and never raises a tab, so a window it "asked to go back to" cannot exist — and the promise still has a web door, because on web the instance and its account binding are one thing, the pin: **a tab whose pin names an account that no longer resolves boots normally instead of failing.** That is the tab the browser restores (reopen-closed-tab, session restore — both restore `sessionStorage`) after its account was removed in another tab or the profile signed out; remove-account refuses while a tab still serves the account, so the pinned tab was closed first. The boot walks the pin through the succession chain, and when the account is genuinely gone it drops the pin and reloads exactly once — onto the store-active account, or the launch flow when none is left — never a blank or refused tab, and never a loop (the pin is gone before the navigation starts). This is the same promise as the native degrade through web's own door, so the catalog's `second-identity-in-its-own-window` outcome 6 is worded door-neutrally ("the place you asked to go back to") rather than declaring web absent — an absence would claim web delivers nothing of the kind, which is false. Built (`store.ts`'s `accountsBoot` continuation); the web witness is pending.

**Which critical sections are real here, and why the native mapping does not
port verbatim.** The three exclusive sections are defined against a native
substrate (a file lock beside `mls_state.db`, one OS process per instance). The
browser inverts it: tabs are separate JS realms, so each already has its **own**
wasm `MlsEngine` and there is no shared in-process engine to elect an owner
over; but they share one **origin**, and because the device id persists to
`localStorage` they share one **MLS device leaf** and the single account-scoped
`provider` replica both CAS-put on the nest. The contention is therefore over
*stored state*, not process roles. **Registry mutation** is real and is guarded
(every registry mutator — the install-secret mint at boot, an add, a switch, a
remove, a cache refresh, a flag write — is a read-modify-write of the single
`fauna/index` every tab shares, and `localStorage` offers no cross-tab
transaction, so two tabs mutating together can lose an update): the shared
crate's wasm wrapper takes the origin-wide exclusive Web Lock
`fauna.accounts.migrate` around every mutator
(`fauna_client_accounts::with_web_mutation_lock`, at each `WasmAccountRegistry`
and launch-chunk mutator entry — a Web Lock is async and the shared mutator is
not, so it wraps from outside; mechanism, edges and the native twin are the
owner's: [`../long-term-store.md`](../long-term-store.md) § Multi-account
evolution → *Cross-process mutation lock*), degrading **open** where the API is
unavailable, the same rule the native lock states as degrading open on I/O
failure. This is web's whole substitute for the native *migration* section:
the one-shot migration this paragraph used to name is gone (2026-09-24), and
the lock name is the historical one, kept so tabs of an older and a newer build
still exclude each other. Witnessed by
`test_account_registry_mutation_lock_web.py` (a write queues behind a sibling
tab's hold and lands on release). The **engine / conversations-engine role** is
real but arrives differently: two engines advancing one device leaf is the fork
the device-owned-epoch invariant forbids, and the replica's three-way merge
resolves a true conflict last-writer-wins, which is lossy for ratchet state
(what that merge does with the conflict it reports — and why it neither alarms
nor re-elects — is the owner's:
[`../../behavior/devices.md`](../../behavior/devices.md) § Cross-device MLS
group-state sync → *A provider CAS conflict is reported, not repaired*).
**One MLS-writing tab per account is elected as of 2026-09-01**, closing this
section's last leg: the tab that takes the per-account Web Lock builds the
engine, and a tab that does not builds **none at all** and renders the standing
`served_elsewhere` refusal through the conversations page's existing
`error-message` — the same string, in the same first-precedence slot, that
linux's `page_error_text` and apple's `ConversationsVM` already paint for the
native role lock. It builds no engine rather than gating sends because the
manager's own construction re-seals the `provider` replica, publishes
crash-staged folder rotations and sweeps owner markers before returning: a tab
that reaches any of those has already forked the ratchet.

**This lock's posture is the opposite of the mutation lock's, deliberately.**
The mutation lock degrades **open** where `navigator.locks` is absent (above); the
engine role fails **closed** — a tab that cannot prove it is the only writer
does not write. That is the same distinction the native role lock draws for the
same reason: *"a lock-file I/O failure fails closed — unlike the account
instance lock's degrade-open posture, because this state is class 5
(user-irrecoverable) … degrading open would trade a near-impossible
availability corner for a silent ratchet fork"* (`libs/fauna-mls/src/storage.rs`,
`SqliteStorage::open`). The acquire is non-blocking for the same reason native
uses `try_lock` and returns `StateServedElsewhere` rather than queueing: the
holder releases only when its tab closes, so queueing would be a hang, not a
wait. Witnessed by `test_engine_role_election_web.py` (two tabs, one account —
`open_same_context_tab()`, since a twin page is a second *device* and shares no
origin store to contend over).

**The "a secondary never enters the wizard" corollary resolves differently here,
and it needs no refusal.** That rule exists because the onboarding scratchpad is
a single per-install slot the primary owns, so a second instance running the
wizard would write over it. On web the wizard owns no scratchpad at all — its
live state lives entirely in module-scope JS/wasm memory, never persisted to
disk or a shared store, so it is **per-tab** by construction, and the premise
the refusal is built on is simply absent. Web therefore does **not** owe a bound-launch wizard
refusal. **The wizard's terminal commit is a different question, and the
scratchpad argument never answered it (re-verified 2026-09-27):** an append-mode identity confirmation writes nothing (the shared
`persist_confirmed_identity`'s append rule), and the "Add account" terminal
then registers and switches through `accountsAdd` + `accountsSwitch` — two
read-modify-writes of the shared `fauna/index`, the same surface every other
tab's switch, remove, cache refresh and flag write touch. What makes that
commit race-free across tabs is the registry mutation lock above, not the
per-tab draft: two tabs completing two wizards queue their commits behind one
Web Lock and both accounts survive. A tab that adds an account moves `active`
as any tab does, and no *other* tab follows it, because each one is pinned.
The scratchpad half was verified 2026-09-01 while building the pin; recorded
so the absence reads as a resolved question rather than a missing leg.

**Process-wide identity singletons are compatible with this stage** (one
account per process): the FFI MLS engine singleton, per-process session
state, and platform equivalents hold. They become the blocking constraint
only at the next stage.

**An erase refuses while a sibling serves the account (ratified 2026-09-20).** Concurrent serving gave a second window of one account the run of the same stores, and sign-out — which erases *every* account-scoped store (§ *Erasure follows scope*) — never asked whether one was open. On POSIX the sibling kept its session, its bearer and its keys for an account the user had signed out of while the directory beneath it was unlinked: its conversations-engine role lock became a lock on an unlinked inode, so the next sign-in minted a fresh lock file at the same path and took the same role beside it — the race `libs/fauna-core/src/fs_lock.rs`'s "never delete a lock file" exists to close — and the sibling's path-named rollback journal landed beside the successor's database. On windows the sibling's open handles failed the erase instead, leaving the signed-out account's data on disk. The rule, one answer for every app:

- **The whole sign-out refuses, not just the erase.** A refusal confined to the filesystem half would still wipe the credential namespace, stranding the surviving stores under a writer key nobody holds — the same failure § *Erasure follows scope* names, reached from the other side. So the gesture stops before either half: nothing erased, no credentials wiped, still signed in.
- **The user is told, in one shared line per gesture**, each minted beside the rest of the sign-out copy in `fauna_client_accounts::erase_residue`: sign-out's (`settings.sign_out_blocked_other_window` — *still signed in, another Fauna window is using this account on this device, close it and sign out again*), remove-account's (`settings.remove_account_blocked_other_window` — *not removed, another Fauna window is using that account, close it and remove the account again*) and the unreadable-index floor's (`onboarding.launch.index_malformed_reset_blocked_other_window` — *nothing was removed, another Fauna window is using an account on this device, close it and start over again*). Each names the remedy because a refusal is otherwise indistinguishable from a gesture that silently did nothing, and the remedy names the button pressed: a user who pressed *remove* and is told to *sign out again* has been told to do something else. No new element: sign-out paints on the surface each seat already owns for sign-out residue, remove-account on the Settings page's `error-message`, the floor on its launch page's `error-message` (convention 2) — each with the gesture's own control still showing, so closing the other window and pressing it again is the whole remedy.
- **Refuse rather than hand off.** Handing the sign-out to the sibling over the per-account raise channel would need a command verb that channel does not have (it activates; it does not instruct), and tui claims no endpoint on it at all — so a hand-off could not be the same answer on all seven seats today, which is the only kind of answer this § accepts.
- **How the question is asked** (`fauna_client_accounts::sole_instance`): the exclusive take is the arbiter, exactly as it is at launch. For every account other than its own the erasing instance holds nothing, so an exclusive `try_lock` answers directly. For **its own** account under the concurrent law it holds a shared lock, so the probe would see its own reflection: it therefore puts that lock down for the length of the probe and takes it again afterwards (`SessionInstanceHolder::without_own_lock`), which is the narrowest window that can answer the question at all. A probe that cannot reach the lock file reports the account free — the same degrade-open posture every other reader of these locks takes, and the alternative is a device its owner cannot sign out of.
- **The guard sees every app, not only its own (ruled 2026-09-20 — the residual this bullet used to state is CLOSED for the seats named below).** The instance lock is keyed under each app's own install base (`<xdg-config>/fauna-tui` for tui, `<xdg-config>/fauna` for linux), while the account store a sign-out unlinks is shared by every app on the OS login (§ *Erasure follows scope*'s W6 paragraph) — so asked only at its own base, a tui sign-out could not see a live linux instance and erased the store out from under it, and wiped with it the **shared** `fauna-account-store` credential namespace that instance's store was running under (its writer key and principal bundles — that namespace is deliberately not per-app), which "close the other window" could never have covered. Ratifying that as the price of per-app install bases was weighed and refused: the corruption is the same one this ruling closed within an app, and the user has no way to know which of two apps is the dangerous one to sign out of. So there is a second lock, and it answers a different question from the first:
  - **The account serving lock — presence, not exclusion.** `<store root>/serving-<actor>.lock`, one per account, at the per-user account-store root every app resolves identically (`StoreRoot::platform()`). A serving instance holds it **shared** for as long as it serves; the erase's question is a momentary **exclusive try**. It never refuses anything but an erase: two apps on one account was never refused and this lock must not start. It is therefore **not a fourth exclusive critical section** — the count above stays three (two in the store, one beside each `mls_state.db`); it is the cross-app twin of the instance lock. Mechanism, file placement and posture are the store's (owner: [`../account-runtime.md`](../account-runtime.md) § Multi-instance concurrency → *The serving lock*).
  - **Who takes it, and when: the same holder, in the same call.** `SessionInstanceHolder` takes it immediately after the launch law admits the instance (`ServingBases` names both directories), swaps it with the instance lock on an account switch, and puts **both** down for the probe — a lone instance must not meet either of its own reflections. One holder rather than a second call a seat can forget.
  - **The launch path's per-app lock is untouched — neither subsumed nor re-keyed.** It still answers *is this app already showing the account* (raise it or offer the chooser — the `Exclusive` refusal left with that mode, § Concurrent instances), which is a per-app question with a per-app remedy; keying it on the shared root would make tui's chooser report accounts linux serves and offer a raise tui cannot perform. A launch the per-app lock refuses never touches the serving lock, so a refused launch does not flicker as a served account.
  - **The erase asks at both.** The install base is still probed because the shared root sees only instances new enough to declare themselves there: a sibling window from before this lock holds the instance lock alone. The two halves degrade independently, and each degrades **open**, for the reason the bullet above gives.
  - **The sync agent takes no serving lock; it is dismissed by command.** It mounts this same store, so the alternatives were both wrong: a lock it takes refuses every sign-out on a machine with a running agent, behind a line telling the user to close a window that does not exist; a lock it silently ignores leaves its mount erased under it. It is instead the one co-resident an erasing app can *instruct* — the un-provision command every desktop sign-out already sends ([`sync-agent.md`](sync-agent.md) § Control plane split) — which keeps *bearer-only co-residents never held this lock* (above) literally true. **The reply is the receipt (built 2026-09-23):** the agent's mount is down before the command replies, and the app awaits that reply before it erases — mechanism and budget are the verb's ([`sync-agent.md`](sync-agent.md) § Control plane split). Every desktop seat awaits it — tui, linux, apple (macOS), and windows last (2026-09-23: `App.SignOutHandler` and the e2e agent's `reset`/`logout` arms).
  - **Every erasing gesture asks the same question, because it is one function — about everything its erase reaches.** The probe lives inside `actors_served_by_another_instance`, which is the door every user-facing erase refuses through: sign-out, remove-account, and the unreadable-index floor — linux's `account-index-reset-confirm-button` and tui's twin, which both run the sign-out's erase (`trigger_sign_out` and `App::reset` respectively). Routed on tui and linux as of 2026-09-21, each asking **before its first destructive step**: remove-account before the registry removal that drops the account's secret slots (a refusal after it would leave the scopes on disk with nothing left to sign in to them), the floor before its launch window goes. **The actor list is the erase's reach, not the registry.** Remove-account asks about its one actor (`remove_account_blocked`). The all-accounts erase asks about the registry's accounts *plus every actor scope under the bases it sweeps* (`sign_out_blocked(registry, bases, swept_bases)`, over `fauna_account_store::db::account_scopes_under`), because the sweep removes every `<base>/<64-hex>/` scope whether or not the registry names it: a malformed index — the floor's whole situation — names nobody, and the shared store root holds accounts only a sibling app's registry lists, so a registry-fed gate asked about nothing on the floor and could not see an account only linux had signed in to from a tui sign-out. Each seat names its swept bases once (`erase_bases`) for its erase and its question alike. The e2e agent's `reset|logout` factory reset stays exempt — no user is in front of it. A seat that routes a new erasing gesture through the door gains the cross-app half with nothing further to wire. **No erase without a gesture behind it runs over the shared root at all.** The one that did — linux's startup re-sweep of pre-W6 flat residue on a fresh-wizard launch, which ran with no user to refuse to and until 2026-09-23 unlinked a running tui's or the sync agent's live store simply because linux was opened signed out — was first narrowed off the root's actor scopes and then retired outright with the flat-layout adoption (2026-09-25): linux's fresh launch routes to the wizard and sweeps nothing. *No credentials* is one app's fact, not the device's, so a future gesture-less erase must stay off the shared root's accounts or ask like any gesture.
  - **Remove-account also refuses the account THIS instance serves (ruled 2026-09-23).** The probe puts this instance's own locks down, which is right for sign-out — sign-out tears this process down afterwards — and blind for remove-account, which does not: a bound secondary serves an account that is not the registry's active one (§ *Session identity resolves through the session's account*), so a switcher keyed on the registry offered it for removal, the probe found nobody else on it, and the erase unlinked the stores this very process was running from — the harm this § exists to prevent, with no sibling needed. So `remove_account_blocked` asks first whether the account is the one this process serves (the process holder's actor, `fauna_client_accounts::process_session_account`) and refuses with its own line, `settings.remove_account_blocked_this_window` — *not removed, this window is using that account, close it and remove the account from another one* — before the sibling question; the probe itself is not bent. The UI layer matches: every switcher marks as "the account in use" — active indicator, no switch, no `account-remove-button` — the account this instance serves (`session_account`), never the registry's active pointer. Sign-out and the floor are unaffected: they erase this instance's own account by design.
  - **Seats: tui and linux declare themselves at the shared root as of 2026-09-20** — the pair that shares a store on a Linux desktop, pinned in both directions and for the lone instance (`sole_instance`'s tests). **The `fauna-ffi` seat declares itself there too as of 2026-09-23** (apple, windows), by both of its routes: the holder (`become_process_session_instance`, windows) and the raw lock object (`acquire_account_instance_lock_shared`, apple — backed by its own `SessionInstanceHolder`, so it carries the serving lock and the same put-down-and-restore). Each takes the root as the erase pair does, `store_container_dir` resolved in Rust (`None` = the platform root), so a test never writes into the developer's own root. The seat's door is `fauna-ffi`'s `sign_out_blocked` / `start_over_blocked` / `remove_account_blocked`: they ask about exactly the two bases `account_state_erase_all_scopes` sweeps, hand back the gesture's line with the decision, and take the raw lock as `own_lock` — a raw-lock seat that leaves it out meets its own reflection and refuses every sign-out, which the door's pins hold both ways. **apple's sign-out and remove-account gestures are wired as of 2026-09-23** (`SignOutSection.confirmSignOut`, asked before the push-row drop and before `StatusVM.signOut`'s account-runtime stop; `AccountSwitcherVM.remove`, now over the same unified `remove_account_blocked` door, `ownLock` threaded down alongside an explicit `servingHere` fallback so a lock-less seat still refuses removing the account it serves). **windows' remove-account is wired as of 2026-09-23** (`AccountSwitcherViewModel.Remove` → `AccountStateDir.RemoveAccountBlocked`, before the registry removal). **windows' sign-out and start-over gestures are wired as of 2026-09-23** (`SettingsAccountPage.SignOutConfirmButton_Click` → `SignOutBlocked`, asked before `App.SignOutHandler`'s teardown; `LaunchAccountIndexUnreadablePage.OnResetConfirmClick` → `StartOverBlocked`, asked before `App.ClearCredentialNamespace`), closing this seat's gesture wiring. **apple's unreadable-index start-over is wired as of 2026-09-26** (macOS `confirmAccountIndexStartOver` → FaunaKit `AccountIndexStartOver.confirm` → `start_over_blocked`, asked before `StatusVM.signOut`'s erase; the refusal paints the launch page's `error-message` with the confirm still showing), closing the last unrouted erasing gesture on every seat. ios and android admit one instance per app and share a store with nobody; web's analogue is the bullet below.
  - **Not ruled here, deliberately:** the probe takes and drops, exactly as the launch probe does, so an instance that starts between a clean probe and the unlink is not excluded. Holding the exclusive take across the erase would close it, at the price of a sibling launch blocking on a sign-out; nothing has measured that window as real, and the within-app guard has the identical one.
- **Web owes the same refusal through the lock it already has.** Tabs are concurrent-instance hosts here, and web's per-account analogue of the file lock is the MLS-engine role Web Lock (`engineLockName(actorId)`, `$lib/webLocks`): a tab signing out must find it free — `ifAvailable`, never a wait — or refuse with the same line. **Built 2026-09-23** (`$lib/webLocks::actorsServedByAnotherTab`, asked by `$lib/conversations::signOutBlockedByAnotherTab` before `identity.logout()` is entered, since `logout()` wipes the credentials synchronously ahead of its first `await`). It asks about every registry account plus the signed-in one — web's erase reaches only registry-named slots, so the registry *is* its reach. **Web consults its own role rather than putting it down:** a tab holding the account's role is itself the proof no other tab does, so that account is not probed, and a release-and-retake would hand another tab's receive poll a window to win the role this tab still serves; every other account is probed. It degrades open like every native reader (no lock manager → no tab can hold the role → nobody serves). A refused sign-out leaves the tab signed in on Settings, so the refusal lands on that page's `error-message` (the page-level banner), the confirm button still showing. Pinned both ways — refused beside a sibling holder, proceeding in the holder tab — by `tabPin.test.ts` and `test_sign_out_refused_other_tab_web.py`. **Remove-account followed the same day** (`$lib/webLocks::removeAccountBlock`, asked by `$lib/conversations::removeAccountBlocked` before `accountsRemove`): the account this tab serves refuses with the this-window line first, then an account another tab's role holds refuses with the other-window line, both on the Settings page's `error-message`. The switcher marks as in use — active indicator, no switch, no `account-remove-button` — the account this tab serves (its signed-in identity, which follows the per-tab pin), never the registry's active pointer, which a pinned tab does not follow. Pinned by `tabPin.test.ts` and `test_remove_account_refused_other_tab_web.py`. **The lock the erase asks is the conversations-engine role, and it is one of TWO per-account engine locks the serving tab holds (ruled 2026-10-01; advisory, refutable downstream).** The other is the runtime's engine-singleton election (`fauna_account_plane::web_host::WebElection` over `fauna_account_store::locks_web`), a different section under a different name by ruling — why they are two, and the one naming scheme both follow, are [`../account-runtime.md`](../account-runtime.md) § Multi-instance concurrency → *Election mechanics*. **Web's store takes no serving lock, and the erase asks the role alone, because on web the role's holder IS the runtime's host:** a runtime starts only in the tab that has just taken the account's conversations-engine role (`$lib/conversations`'s manager build takes the role first and calls `startAccountRuntimeFor` after; a refused tab builds no manager and hosts no runtime), and the tab's one teardown releases the role and stops the runtime together (`resetConversationsManager`; a sign-out stops the runtime first and the reset that follows the identity change releases the role) — so the store's own election lock could answer nothing the role does not, and a second probe would be a second place to get the order wrong. A store a stopping runtime still holds when the probe has already answered free is the bounded-delete residue of § *Erasure follows scope* (the web paragraph's fourth decision), not a case for a second lock; the native probe's own un-ruled window (*Not ruled here, deliberately*, above) is the same shape. **Not yet uniform, deliberately captured:** the role is taken in the SPA while its three sibling web locks are taken in shared Rust; the lift is the election section's *Not built* entry ([`../account-runtime.md`](../account-runtime.md) § Implementation status today) and keeps the name and this probe.

## Concurrent identities — direction

Not designed here. What this doc owns now is the constraint that keeps it
reachable: **no new process-wide singleton may carry per-identity state
without an account key.** The known offenders to be re-keyed when this stage
is designed — the FFI MLS `ENGINE` static, windows'
`ConversationsManagerHost.Instance`, each app's single-session state — are
recorded so new code joins the per-account carriers instead of the
singletons. The registry's PARKED "background cross-identity" capability
(`long-term-store.md`) is a precursor of this stage and stays parked.

## Testing interplay

Account-scoping does not soften e2e isolation: File Provider domains, cfapi
roots, and the agent socket remain machine-global namespaces even once their
entries are account-keyed, so `e2e-launch-isolation.md` convention 10 (an app launch is
isolated from the box) keeps gating them off in e2e — the apple FP reconcile
gate stays. The sanctioned route to live coverage of those surfaces is a
convention-12 `real_session` opt-in suite, not a weakening of the gate.
Concurrent-instances work adds one harness requirement: drivers must be able
to launch two isolated instances bound to different seeded accounts in one
test (the per-launch isolation of convention 10 already provides the
mechanics).

## Implementation status today

**Stages:** all seven apps ship serialized switching in product (surface status:
`long-term-store.md` § Implementation status) — a product claim, not a coverage one: which apps have a recorded e2e run is `docs/features/multiple-accounts.md`'s record (as of 2026-09-23 not all seven do). Concurrent instances: the
shared-Rust groundwork landed 2026-07-22 — the pure-read spawn gate
(`AccountRegistry::bind_account`/`bind_account_confirmed`, mirroring every
activation guard) and the bound launch adapter
(`RegistryLaunchPersistence::bound`: resolves the named account, never
consults or moves `active`, and — while the native apps' transitional
registries existed, until 2026-09-28 — never refreshed the legacy mirror nor
composed the CR-3 legacy-global fallback), both exposed on
`FfiAccountRegistry` (`bind_account`/`bound_launch_persistence`) —
unit-proven including a real-`LaunchMachine` bound route. The registry's
advisory cross-process mutation lock landed 2026-07-22 (mechanism + edges:
`long-term-store.md` § Multi-account evolution → Cross-process mutation
lock; `FfiAccountRegistry::new_with_lock_dir`), and **apple is the first
app constructing with it** (2026-07-22, `FaunaAccounts.registry()` over the
install-scoped `AccountStateDir.base`), **linux the second** (2026-07-22,
`main.rs::account_registry()` over `account_scope::install_state_base()`, the
same `<xdg-config>/fauna/` its instance-lock files and per-actor scope dirs sit
in — sweeping linux's nineteen bypassing constructions through that choke point
in the same change, since the flip only binds the writers that route through
it; see the census warning in `long-term-store.md` § Cross-process mutation
lock before flipping the next app); the other four apps (web, windows, android, tui — apple's one construction covers macOS and iOS) still construct unlocked
and their adoption rides the per-platform re-keying legs. The
(OS login, account) single-instance guard landed 2026-07-22 as shared Rust
(`AccountInstanceLock`, § Concurrent instances → the shared lock mechanism)
with **apple (macOS) as the first wired leg**: `completeAuthenticatedLaunch`
acquires for the resolved session account before opening its scoped state,
reuses across same-account rebuilds, swaps on switch, refuses terminally —
e2e-proven with two live instances (same-account primary/primary and
bound-onto-served both refused; different accounts coexist).
**apple is also RETIRED (2026-08-16, W5.6 — supersedes the refusal half of
its 2026-07-22 description above):** `FaunaAccounts.acquireInstanceLock`
calls the new `acquire_account_instance_lock_shared` FFI export (apple has
no app-level guard of its own to re-key, so its leg — like linux's and tui's
— is a one-line acquire-mode flip, just at this crate's lower-level raw-lock
export rather than the shared `become_process_session_instance` holder
windows consumes: apple never adopted that holder, so its own hand-rolled
reuse/swap logic in `completeAuthenticatedLaunch` is unchanged and still
correct); the conversations-engine role-lock refusal is wired at the one FFI
seam apple's `conversations_session_over_manager` call reaches
(`build_conversations_session` in `nest_client.rs`, which now arms the
caller-supplied manager's `set_engine_served_elsewhere` on
`MlsError::ServedElsewhere`, mirroring linux's `app.rs` `AuthSuccess` arm),
surfaced via `ConversationsVM.pageError`'s new top-precedence read (shared by
macOS + iOS); the switcher's "open as new instance" button renders on every
row, including the active one. Both refusal e2e cases flipped to the
coexistence proof (`test_apple_second_instance_on_the_same_account_coexists`,
`test_apple_bound_launch_onto_the_served_account_coexists` — the latter the
full three-observable proof, mirroring tui's/linux's pattern exactly).
`AlreadyServed` survives only against a pre-retirement binary's exclusive
hold; the bound-mismatch-to-an-unknown-actor refusal
(`test_apple_bound_launch_refuses_an_unknown_account`) is unaffected — it is
`bind_account`'s own gate, mode-independent. **And its STORAGE was corrected 2026-09-22 — the hand-rolled reuse/swap logic named above is right, the box it kept its state in was not.** `boundActorId`, `instanceLock` and `instanceLockActorId` were `@State` on `FaunaMacApp`, an `App`-conforming **struct**, which put all three in the captured-`self` bug class `apple-state-capture-check` exists to freeze ([`sync-agent-credentials.md`](sync-agent-credentials.md) § Implementation status today: *a value a callback-reached site must read does not live in `@State` alone*). Two consequences, neither hypothetical. **(1) The DEBUG-only `logoutKeepData` could not see the binding at all** — its sole caller is `handleTestCommand`'s `"logout"` case, so `boundActorId ?? registry.active()` read the `nil` the test agent's own captured copy was born with whatever the launch had resolved, and a bound secondary therefore took the primary's full logout path instead of the terminal refusal this section requires of it; the lock release beside it landed on that same copy, while the live instance went on holding the removed account's lock for the rest of the process. **(2) `completeAuthenticatedLaunch` is re-entrant**, and the re-entrant route through a differently-captured closure (`handleTestCommand`'s `silent_sign_in` → `performPostAuthSilentSignIn` → `runLaunch()`) read `instanceLockActorId` as `nil` and re-acquired a lock this process already held — harmless under the SHARED acquire retired to above, a terminal false refusal against a pre-retirement (exclusive) binary. **The remedy is the later fix's shape, not the earlier one's:** all three moved onto `MacAppState` outright, with no `@State` twin and no `live*` mirror, because no view renders any of them (all eleven use sites are App-struct lifecycle methods) and a second slot is precisely what turns this class from a curiosity into a bug. They stay off `ActorScope.dropAppOwnedState`'s canonical list on purpose — the binding is process-scoped, not actor-scoped, and the lock is swapped by `completeAuthenticatedLaunch` itself and released by `logoutKeepData`. Twelve sites left `apple_state_capture_baseline.json` (35 → 23): the seven this fix owed, plus five the pass had reasoned safe on a same-continuation argument rather than closed structurally. **linux is the second wired leg**
(2026-07-22): `app.rs`'s `AuthSuccess` arm acquires for the resolved
store-active account before any scoped state opens (reuse on same-account
rebuilds, swap-by-replacement on in-process switches, degrade-open,
terminal refusal), GApplication's D-Bus uniqueness is kept as the
plain-launch raise layer per the platform-affordance rule above (`NON_UNIQUE`
added exactly when a binding is present), wizard-routed bound launches
refuse in `build_ui`, and a binding that doesn't match the resolved account
refuses under bound-or-refuse — e2e-proven with two live drivers
(`test_account_instance_lock_linux.py`): a bound launch onto the served account
is refused with the first instance untouched, a **bound launch for a different
account COEXISTS** (each instance's `session.actor_id` resolving to its own
account — the assertion that catches the silent inert-bound-launch blocker,
green since linux adopted the shared per-account session read), and a plain
second launch of the install's only account renders the chooser with **zero**
offerable rows rather than exiting, per the ratified subsumption below.
⚠ Both of those last two asserted the *opposite* until 2026-07-23 — a terminal
refusal and a bound-mismatch refusal respectively — and both were red on
`origin/main` when re-run, each having been overtaken by a ratified change
(the chooser; the bound session build) rather than by a regression. **tui is the third wired leg** (2026-07-22): `session::establish`
acquires for the resolved account at the point it derives the actor id, before
any of that account's scoped state opens, and `launch::start` re-runs the whole
routing on every switch, so one call site covers acquire / same-account reuse /
cross-account swap; refusal is terminal (stderr + `tracing`, then exit).
Unlike windows and linux this was an **add**, not a re-key — a terminal is not
a window manager, so before it nothing whatever stopped two `fauna-tui`
processes from opening one account's scoped state — and it is e2e-proven with
two live drivers (`test_account_instance_lock_tui.py`: bound-onto-served and
bound-mismatch refused, and a plain second launch of the install's only
account rendering the chooser with **zero** offerable rows rather than
exiting; the first instance untouched throughout).
⚠ tui repeated linux's own 2026-07-23 lesson almost exactly, and the repeat is
the reason the pattern is worth stating twice: when tui's chooser landed
(2026-08-01) this file's plain-launch case still asserted a
terminal refusal, so it went red on `origin/main` having been **overtaken by a
ratified change, not a regression** — reshaped 2026-08-02 to the same
zero-rows shape linux uses. Its bound-onto-served case was red the same day for
the opposite reason, a **genuine defect the chooser introduced**:
`launch::start_or_offer_chooser` shipped without linux's step-1 bound-launch
gate, so a `FAUNA_BOUND_ACCOUNT` collision reached the chooser instead of the
guard's terminal refusal. The gate is now tui's too, and all three of its
conditions carry deterministic unit pins (`launch.rs`
§ `tests::collision_gates`) rather than resting only on a two-driver e2e run —
a chooser platform must gate on the binding *before* it probes.
**tui is also the first RETIRED leg (2026-08-15, W5.6 — supersedes the
refusal half of its 2026-07-22 description above):** `session::establish`
serves `ServingMode::Concurrent` at the same acquire point — the shared
lock, so bound-onto-served now **coexists**; the e2e case flipped (its
second overtaken-by-ratified-change flip) into the two-instance proof:
both authenticate, a muted-words add in one becomes visible in the other
over the shared store, and the non-role-holder's conversations page
refuses honestly on `error-message` while the holder's stays live.
`AlreadyServed` survives only against a pre-retirement binary's exclusive
hold (still a terminal exit, and still stderr-corroborated on the
bound-mismatch case); the plain-launch chooser case is unchanged — a
shared holder still probes as served. **windows is the fourth wired leg** (2026-07-23):
`App.OnLaunched` acquires through `FaunaApp.Core.Services.SessionInstance`
immediately after the secret loads into `ICryptoService` — the point the session
account resolves, and before the MLS store, feed drafts or backup-coordinator
paths derive from `ActorIdHex` — and `SwitchAccountHandler` re-runs the same
entry point after the crypto rebuild, so one call site covers acquire, reuse and
swap. The `Local\FaunaApp-SingleInstance` mutex is **kept** as the plain-launch
raise layer per the platform-affordance rule (a bound launch skips `TryClaim`
outright, windows' equivalent of linux's `NON_UNIQUE`), the bound launch reads
`session_material(bound)` + `bound_launch_persistence(bound)` through the
registry instead of the legacy slot, `bind_account` gates the binding with the
switcher's own fresh-flag re-auth retry on `ConfirmationRequired`, and a bound
launch routed to `WizardAt` refuses terminally. e2e-proven with two live drivers
(`test_account_instance_lock_windows.py`): same-account plain second launch and
bound-onto-served both refused with the first instance untouched, **and — as on
linux since its own bound session build, and tui since its own (2026-09-01) —
a bound launch for a different account COEXISTS, each instance's
`session.actor_id` resolving to its own account.** That third assertion is
the one that catches the inert-bound-launch
blocker, which is silent by construction: a second window that reads the legacy
slot looks correct and is the wrong account. **windows wired both 5d surfaces the
next day** (2026-07-23) — the second app to render the chooser and the second
to offer the switcher's spawn button; details in the 5d paragraphs below.

**The holder logic is shared too, as of tui's leg** (2026-07-22): the four
behaviours every native leg needs around the raw lock — bound-or-refuse, reuse
on a same-account rebuild, swap-by-replacement on a cross-account switch, and
degrade-open — were lifted out of linux into
`fauna_client_accounts::SessionInstanceHolder` /
`become_process_session_instance` (unit-pinned there), so windows' and
android's legs consume them rather than re-deriving them a fourth time
(priority #2; the "one implementation, not seven" reasoning that applies to the
lock applies to its holder). An app now supplies only its own install-scoped
state base and the two platform duties the goal doc assigns it: logging the
degrade and choosing the refusal surface. **The wasm surface + web tab-pinning
landed 2026-09-01** (§ Concurrent instances → *Web*): `WasmAccountRegistry`
gained the session-identity read `sessionMaterial()` — the browser twin of
`FfiSessionMaterial`, over the *same* shared `AccountRegistry::session_material`
every native leg reads, so web adds no second implementation — plus
`resolvePinnedAccount()` for rider 2's chain walk; the SPA gained the per-tab
`sessionStorage` pin (`$lib/tabPin`), a pin-aware `accountsBoot()`/`loadIdentity()`
/layout guard, and the Web Locks migration section (`$lib/webLocks`).
**Web's engine-role election landed 2026-09-01** — one MLS-writing tab per
account, over `tryHoldEngineRole` (a non-blocking, fail-closed acquire in the
shared engine-construction path `getConversationsManager`, the web analogue of
the native lock's place inside `SqliteStorage::open`); the losing tab runs no
engine and renders the shared `served_elsewhere` refusal, so web now paints the
same standing message for the same condition as linux and apple. Still open for
this stage: android's add, the last native leg onto the shared lock
(windows' re-key landed 2026-07-23). Both 5d surfaces were user-approved 2026-07-22 (IDs in
ui.yaml — the switcher's `account-open-new-instance-button` and the
`launch_instance_chooser` page); macOS wired the switcher button first (its
only reachable entry), and **linux is the first app to render the chooser**
(2026-07-22). **windows wired both surfaces 2026-07-23**; **tui — the last of
the three chooser platforms — wired its own 2026-08-01**,
lifting linux's shape minus the two affordances its declared absences rule out
(no add-account forward, no raise). All three chooser platforms are therefore
wired, and **web needs neither 5d surface**: ui.yaml scopes
`launch_instance_chooser` to `[windows, linux, tui]` and declares
`account-open-new-instance-button` structurally absent on web, because tabs are
web's instance story — per-tab account choosing rides the existing switcher.
What remains for this stage is android's guard (web's engine-role election landed 2026-09-01, above).

**Web's remove-account erase is short of § *Erasure follows scope* (found 2026-09-28).** `AccountRegistry::remove` on web deletes the `fauna/{actor}/*` slots and the index entry and nothing else, and the wasm wrapper's scope erase behind it (since 2026-10-01, next paragraph) takes the account store alone, where linux and tui follow the registry removal with `account_scope::remove_account`'s sweep of all the actor's stores. The account store aside (next paragraph), web opens no actor-keyed IndexedDB store (`fauna-spam` is the install-scoped model cache, `fauna-region` is install-scoped), so this paragraph's residue is the actor-keyed `localStorage` state web's wasm modules mint *beside* the registry namespace — the backup audit observation `fauna_backup_audit_{actor}` (class 1) and the four succession keys keyed by the successor — all of which survive both a remove and a sign-out today (the sealed `__config` replica `fauna_config_replica_{actor}`, class 4, left this list when it retired with the rail on 2026-10-02 — [`../config-dissolution.md`](../config-dissolution.md) § The `__config` dissolution schedule → *The closure order*, step (6)). The fix is a second arm of the wasm `account_scope` twin of the native one (`libs/fauna-wasm/src/account_scope.rs`'s `erase_actor_scope`; registry first, then one drift-guarded list of actor-keyed builders swept for the removed account, and for every account on sign-out); nest-side state, the identity's `__drafts` plane included, is never touched by a device-side removal.

**Web's sign-out and remove-account erase the account store (measured short 2026-10-01, built the same day; the rule is § *Erasure follows scope*, the web paragraph).** Until 2026-10-01 `fauna_account_store::indexeddb::IndexedDbBackend::delete` had no production caller: `WasmAccountRegistry::clear_all` and `remove` erased the registry's slots and nothing else. Measured in Chromium through the Settings sign-out, one dedicated account: `indexedDB.databases()` still listed `fauna-account-store/<actor>` right after the sign-out and 30 s later, while every actor-keyed `localStorage` key was gone, the store's writer key among them. **Built 2026-10-01 — decisions 1 to 3: the order, the record and the reach.** The record is `fauna_client_accounts::SignOutRecord` (shared Rust, pure over `SecretStore`, unit-tested natively), kept in the install-scoped `localStorage` key `fauna_sign_out_record`, outside the `fauna/` namespace the wipe removes. The erase is `fauna-wasm`'s new `account_scope` module — the wasm twin the paragraph above describes, created here with the store arm (`erase_actor_scope` → `IndexedDbBackend::delete(StoreRoot::platform().store_name(actor))`); its `localStorage` builders are still that paragraph's. `identity.logout()` (`apps/fauna-web/src/lib/store.ts`) now runs record → the runtime's sign-out stop, awaited under the stop budget ([`../account-client-lifecycle.md`](../account-client-lifecycle.md) § Implementation status today) → the credential wipe → `signOutFinish`, the one routine (`account_scope::finish_recorded`) that also serves the load: `accountsBoot()` and the onboarding page's launch sequence both await `signOutReconciled()` before they read the registry, and `loadIdentity()` answers no identity while the record owes its wipe. `WasmAccountRegistry::remove` records, removes, then erases that one account's store. The e2e agent's `reset` and `logout` actions await the product's sign-out (bounded) and then sweep any account store left in the origin. Witnesses, all red before and green after on web: `test_sign_out_web.py::test_web_sign_out_retires_the_enrollment_and_erases_the_account_store` (no database, no OPFS directory, no record, the named row's grant cleared, and the next sign-in's replica reading the signed-out writer `removed`), `test_sign_out_web.py::test_web_load_after_a_tab_closed_mid_sign_out_finishes_the_sign_out` (the record seeded as the closed tab leaves it; the load lands on onboarding with no registry key, no store and no record), and the `wasm-bindgen-test` `account_runtime::tests::a_sign_out_stop_asks_the_nest_to_retire_and_the_erase_leaves_no_store`. A sign-out and sign-in in one browser now reaches its rotation ring through the escrow recovery every native app uses. **Built 2026-10-01 — decision 4, the store half as a residue class.** Measured first: with a second connection holding the database (one that ignores `versionchange`), the delete request was answered `blocked` and never settled, so `identity.logout()` never returned and the Settings sign-out never reached onboarding. `IndexedDbBackend::delete` now answers `Err` on `blocked` and removes the segment directory only after the database is gone; `account_scope::erase_actor_scope` runs it under `ERASE_BUDGET` (5 s, `fauna_sleep::sleep`), and `finish_recorded` erases every planned account at once, so the budget bounds the sweep. `signOutFinish` resolves to `{ found, residue? }`, the residue being `EraseResidueView::from_survivor_count(n).copy(Absent)` — the count of accounts whose erase failed, never credentials. `$lib/accounts` keeps it as the `signOutResidue` store, overwritten by every finish, and the onboarding page derives its `error-message` from it on the `identity_choice` step while nobody is signed in (state the wizard does not own, so a wizard tick cannot wipe it). A load's reconcile first puts `signOutPlannedErase()` to `$lib/webLocks::actorsServedByAnotherTab` and passes the answer in; a refused sweep erases nothing and keeps every planned account. Witnesses: `test_sign_out_web.py::test_web_sign_out_that_cannot_erase_a_store_says_so_and_a_later_load_finishes` (red before: `create-identity-button` never appeared) and the store's `wasm-bindgen-test` `a_delete_another_connection_blocks_answers_instead_of_waiting`. **Built 2026-10-01 — the residue surface.** `residue_line` passes `EraseRetryAffordance::Rendered`, and a refused sweep answers `fauna_client_accounts::sign_out_residue_retry_blocked_copy()` in the count line's place. The onboarding page paints the `sign-out-residue` view on `identity_choice` from the `signOutResidue` store while nobody is signed in (`sign-out-residue-message` and `sign-out-residue-retry-button`), and no longer puts the line on `error-message`. The button calls `signOutFinish('retry')`, which probes the other tabs as a load does and runs `finish_recorded`. Witness: `test_sign_out_web.py::test_web_sign_out_residue_retry_refuses_beside_another_tab_and_finishes_the_erase` (red before: `sign-out-residue-message` never appeared), which holds the store open, has a second tab hold the account's engine-role lock for the refusal, and reads each press through the line it leaves. The two silent cases — a remove-account whose store erase fails, a load that lands signed in — stay silent by the rule's own text (the web paragraph's decision 4).

**windows' 5d leg** (2026-07-23). Collision detection runs in `App.OnLaunched`
**before `SingleInstanceManager.TryClaim()`** — the platform-specific constraint
below, and load-bearing here for the same reason as on linux: the mutex signals
the primary and exits this process, so a collision decided after it is never
decided at all. A collided launch therefore opts out of the mutex exactly as a
bound one does, and its focus-existing button *is* that redirect, reached
explicitly. The decision itself is the pure `LaunchCollisionGate`
(plain launch + resolvable store-active account + that account served), which
also carries the choosable-accounts projection; both fail toward the **ordinary
launch**, never toward a chooser, since a wrong "no" degrades to the terminal
refusal that shipped before while a wrong "yes" would strand a lone launch in a
list of accounts nobody holds. The pick calls `bind_session_launch_to` and
**re-enters the existing bound branch** rather than forking a second launch
path, which is what gets it the fresh-flag native re-auth retry for free —
windows is the first app whose chooser can pick a `require_confirm` account,
because it is the first with a bound session build behind the pick. Both
forwarding exits ride the mutex's own named events: focus-existing sets the
existing `Local\FaunaApp-Activate`, and add-account gets a **second** named
event (`Local\FaunaApp-AddAccount`) because an auto-reset `EventWaitHandle`
carries no payload — the primary must be able to tell "raise" from "raise into
the add-account wizard", where linux distinguishes them by D-Bus method. The
switcher's spawn button sets `FAUNA_BOUND_ACCOUNT` on a child of the running
process and pre-checks nothing, as apple's and linux's do. e2e-proven with two
live drivers (`test_launch_instance_chooser_windows.py`: collision → chooser
offering exactly the not-currently-served account → pick completes this process's
routing as the picked account, the served sibling untouched;
`test_account_switcher_windows.py::test_windows_open_as_new_instance_spawns_a_bound_sibling`:
the spawned sibling authenticates as its bound account while the parent stays on
its own).

**linux's 5d leg, and the mechanisms the other legs inherit.** Three pieces
are shared, not per-app, because all three chooser platforms hit them
identically. (1) The **display-only probe** the chooser's list is defined in
terms of: `AccountInstanceLock::is_served` / `not_currently_served`. It never
creates a lock file — a probe that created would mint one per registered
account just by rendering a list — and answers "not served" on every failure,
so a hiccup narrows the list rather than stranding the user. It holds the lock
for two syscalls, since `flock` admits no non-destructive test; that is
acceptable only at chooser render, and arbitration stays at `acquire`. (2) The
**process launch binding** `session_launch_binding` / `bind_session_launch_to`,
which `become_process_session_instance` reads instead of the environment
directly: a binding can arise *after* launch, because the chooser's pick makes
the colliding process the chosen account's bound instance, and an app that
re-read `FAUNA_BOUND_ACCOUNT` would silently ignore its own chooser. Since
2026-08-27 the cell has a second post-launch writer,
`rebind_session_launch_after_succession`, called from `record_succession`, so
a bound instance survives its own succession ceremony (§ Concurrent
instances → *The binding follows the account*; pinned at tier_1 in both
crates' cell tests and end-to-end by `test_account_instance_lock_tui.py`'s
fourth case, a succession driven from the bound seat of the two-instance
world). Reachable today on **tui, windows and linux** — the bound-launch apps
with a ceremony that cross the shared holder (linux's ceremony landed 2026-09-01;
its two cases are `test_account_instance_lock_linux.py`'s fourth and fifth); macOS
takes the shared lock directly (`FaunaAccounts.swift`) and has no per-switch bound-or-refuse gate, so the gate half of the fix never applied there. macOS survives its own bound-seat succession: `test_account_switcher_apple.py::test_apple_a_bound_instance_survives_its_own_succession[macos]` is green (2026-09-23). Its one defect was apple-specific and sat after the rebind. The successor's freshly built `FaunaClient` authenticated at `start()` from the legacy `.secretKey` slot, which is the ACTIVE account's downgrade mirror and which a bound instance never re-mirrors. Whenever that slot still held the retired actor, `authenticate` re-pointed the new client at it, and every call was refused `fauna.auth.superseded`. A `FaunaClient` now authenticates only with its own constructor secret (`FaunaClient.authenticateOwnSeat`, pinned by FaunaKit's `startupAuthSignsAsTheClientsOwnIdentityNotTheLegacyMirror`; commit message `fix(macos,ios): sign a FaunaClient's startup auth as its own identity, never the legacy mirror`).
(3) The **UniFFI surface for the apps that reach all of the above over the
seam** — windows now, android next. `accounts_not_currently_served` landed with
linux's leg; the holder trio (`become_process_session_instance`,
`session_launch_binding`, `bind_session_launch_to`) followed 2026-07-23, so an
FFI client consumes the same four shared behaviours (bound-or-refuse, reuse,
swap-by-replacement, degrade-open) rather than re-deriving them in Swift/C#/
Kotlin. The refusal's one-line reason crosses the seam with it, so all seven
apps log the same sentence; the held lock stays in Rust process state, so an
FFI caller keeps no handle alive and a swap releases the outgoing lock itself.

Two things are genuinely per-platform. **Collision detection must run before
the platform's single-instance affordance acts** — on linux, in `main()`
before the `GApplication` is built, because D-Bus uniqueness would otherwise
redirect the colliding process into the running instance and exit it first.
The colliding process then opts out of the redirect (`NON_UNIQUE`), which is
also what keeps its add-account forward coherent: the *running plain* instance
still owns the well-known name, so the forward is
`ActivateAction("add-account")` — the desktop's own channel, not a second IPC
surface. windows reaches the same exits through the channel *its* platform
layer already owns: a collided launch skips `TryClaim`, and its exits set named
events — the existing `Local\FaunaApp-Activate` for focus-existing, plus a
second `Local\FaunaApp-AddAccount` for the forward, because an auto-reset event
carries no payload where a D-Bus method name does.

**Remove-account refuses the account this instance serves — tui and linux,
2026-09-23; apple and windows 2026-09-23.** Both switchers key the active row (the
one with no switch and no `account-remove-button`) on
`fauna_client_accounts::session_account` — the process holder's actor; before
one is admitted, the process's launch binding, else the registry's active
account (a bound seat can render before admission: linux builds its Account
page once, ahead of the session's lock, and keyed on the pointer it offered
its own account for removal — caught 2026-10-06 by the native two-instance
erase witness, `tests/e2e-unified/tests/test_erase_refused_other_instance.py`)
— and both
`account_scope::remove_account`s refuse through `remove_account_blocked`'s
served-here arm before the sibling probe, pinned per seat
(`remove_account_refuses_the_account_this_process_serves_and_touches_nothing`)
and in `sole_instance`'s tests. **apple** (macOS + iOS, one FaunaKit
`AccountSwitcherVM`) takes the same rule over `fauna-ffi`'s unified
`remove_account_blocked` door, with the served account passed in rather than read
from the holder: apple guards its session with the raw per-account lock and never
fills the process holder, so its served account is the session's own actor
(`SessionState.actorId`, which is `bound ?? registry.active()`), falling back to the
registry's active account before a session is admitted. Its remove asks both
arms — served-here, then the sibling probe under its install base — before
the registry removal, pinned in `AccountSwitcherServedAccountTests`. **windows**, the
other seat with bound launches, fills the process holder
(`SessionInstance.Become`), so it reads the shared rule directly: the
`AccountSwitcherViewModel` keys its in-use row on `FfiAccountRegistry::session_account`
(the FFI face of `fauna_client_accounts::session_account`), and its `Remove` asks
the same `remove_account_blocked` door — no `own_lock`, no `serving_here`, so the
door reads the holder — before `Remove` on the registry, painting the case's line
on `error-message`; both arms pinned in `AccountSwitcherViewModelTests`.

**The raise channel — linux and windows are wired (both 2026-07-23); tui's leg
landed 2026-08-01 as a declared no-endpoint (below).** The gap it closes: an instance that is itself *bound* runs
`NON_UNIQUE` / skips the claim and owns no app-wide name, so a collision
against a bound sibling had nothing to call and surfaced `error-message`
instead of raising. Per § Concurrent instances → *The per-(OS login, account)
raise channel*, and off the one shared token `account_instance_token` (so every
platform's endpoint name and the lock file `instance-<token>.lock` cannot key
differently):

- **linux — wired.** `become_session_instance` claims
  `social.fauna.fauna.a<token>` beside the lock acquire (one site, so an
  in-process account switch swaps the name where it swaps the lock), owning it
  with a manual `bus_own_name` and exporting `org.freedesktop.Application`'s
  `Activate` — only `Activate`, since `ActivateAction` stays app-wide with the
  wizard. Focus-existing calls that name uniformly and keeps the ratified
  degrade: the call fails → re-probe the lock → no longer served, continue as
  a plain launch; still served, `error-message`. The add-account forward keeps
  the app-wide name and adds the no-owner rule — an unowned app-wide name means
  the server is bound, so this plain process is the primary and runs the wizard
  itself. The Flatpak manifest carries no name grant for this at all — the
  endpoint is a subname of the app id, which is the Flatpak app-id too;
  `APP_ID` is unchanged. Both ends are
  e2e-proven against a *bound* server
  (`test_launch_instance_chooser_linux.py::test_linux_focus_existing_raises_a_bound_sibling`:
  the raiser exits **and** the server's `raises_served` advances — a
  half-wired channel passes either alone).
- **windows — wired.** Every serving instance — plain, bound, switched, and
  even a degraded unguarded acquire — claims `Local\FaunaApp-Activate-<token>`
  from `App.EnsureSessionInstance`, the one funnel every session account
  resolves through (the C# twin of linux's one-site claim). The mechanism is
  headless `System.Threading` in `FaunaApp.Core::AccountActivationEndpoint`
  (claim / raise / switch-release) with only the window hop in the WinUI
  layer, so it is tier_1-testable — the WinUI assembly is compiled by no gate.
  Focus-existing routes through the shared `fauna_client_accounts::resolve_focus_existing`
  over UniFFI (`LaunchCollisionGate.ResolveFocusExisting` only adapts its two
  hooks into the `FfiFocusExistingSeat` callback interface) and keeps the
  identical ratified degrade — e2e-proven on the gone-instance arm too
  (`test_windows_focus_existing_onto_an_instance_that_has_gone_starts_normally`); the add-account forward stays on the
  app-wide `Local\FaunaApp-AddAccount`, unchanged. e2e-proven against a *bound*
  server (`test_launch_instance_chooser_windows.py`'s focus-existing journey —
  the case structurally unreachable over the app-wide name; the raiser exits
  **and** the server logs `[instance-endpoint] raised for`). ⚠ Two windows
  subtleties, both pinned: a switch must `Set()` the outgoing endpoint (a
  `Dispose()` mid-`WaitOne()` never frees the name), and the exiting buttons
  defer their `Environment.Exit(0)` off the click event (else a synchronous UIA
  `Invoke()` mid-teardown throws).
- **tui** — wired 2026-08-01: it claims no endpoint by design (no window
  manager can raise a terminal app), so its raise never lands and the
  ratified degrade decides every click — re-probe the lock; no longer served,
  continue as a plain launch; still served, print where the account is served
  and exit (the terminal analog of the raise). The re-probe landed 2026-09-22;
  until then the click printed and exited unconditionally, even onto an
  instance that had already gone. The decision is shared Rust —
  `fauna_client_accounts::resolve_focus_existing` — which linux's handler also
  routes through, and windows over UniFFI (`resolve_focus_existing` in
  `libs/fauna-ffi`). e2e-proven on both arms
  (`test_launch_instance_chooser_tui.py`).
- **macOS** — deferred by design (no colliding process to raise from). What a
  second launch *means* here is the plain-launch raise layer above —
  LaunchServices hands the running instance a reopen event instead of starting
  a process — and that raise is e2e-witnessed end to end
  (`tests/e2e-unified/tests/artifact/test_macos_app_bundle.py::test_launching_the_bundle_again_lands_on_the_running_instance`:
  `open` on the staged bundle advances the app's `reopens_handled` counter,
  linux's `raises_served` twin, with no second process), which is macOS's
  witness of the feature catalog's `second-identity-in-its-own-window`
  outcome 3.

The **second** genuinely per-platform thing is how a spawned or collided
process is *observed* under e2e, and it is worth recording because it is not
obvious: linux and apple hand the child its own free `FAUNA_E2E_AGENT_PORT`,
but windows' test agent is an outbound **poller** against a bridge URL rather
than a server on a port, so a child that inherited its parent's bridge would
steal the parent's commands and clobber its state pushes. windows' spawner
therefore never passes its own bridge down; a test that wants to observe the
child stands up a second, app-less bridge and names it in
`FAUNA_E2E_CHILD_BRIDGE`. Same guarantee, different channel — and it is the
only reason the spawn assertion can be "the child authenticated as its bound
account" on windows rather than merely "a process appeared".

The launch-wiring **read** seam landed 2026-07-22
(`requested_bound_account` + apple's `FaunaAccounts.resolveLaunchBinding`,
which resolves primary / bound / refused and runs the re-auth retry). Two app
roots consume it: apple's (below) and **linux's** — `main.rs::launch_credentials`
resolves `session_material(bound)` instead of the legacy single slot and
`launch_persistence()` returns `bound_launch_persistence(bound)`, so the whole
launch redirects at one seam. Web tabs exhibit the convergence hazard described
above. Concurrent identities: not started, direction only.

**⚠ Blocker found 2026-07-22 — an app's post-launch session build is
actor-blind.** Routing the *launch machine* on a bound account is not enough to
make a secondary instance work: after the machine reports `Online`, apple's app
root rebuilds the session from the **legacy single slot** (`FaunaMacApp.swift`
`completeAuthenticatedLaunch` → `keychain.load(.secretKey/.nodeUrl/.deviceId/
.cachedHandle)`; six such reads on that file's launch and re-auth paths), and
that slot is by definition the *active* account's downgrade mirror. Proven on a
real bound launch: the machine routed on the bound account and the session then
came up as the **active** one (`am_i_admin=false` for an admin binding). This is
**not** a serialized-switching bug — the mirror always tracks `active`, so every
shipped path is correct today — but it makes the bound launch inert, and the
other apps are built the same way (they were all written when "the current
identity" and "the active account" were the same thing). **Resolved 2026-07-22:**
the fork is settled in § Concurrent instances → *Session identity resolves
through the session's account* — one account-resolved accessor per process, not
per-call-site scoping. The shared read is
`AccountRegistry::session_material` (secrets + index-entry cache, legacy-aware
on both halves), exported as `FfiAccountRegistry::session_material`. **apple's
macOS app root is the first adopter** (same day): `runLaunch` resolves the
binding, `completeAuthenticatedLaunch` builds the session from
`sessionMaterial(boundActorId ?? active)`, the bound-wizard refusal and the
bound "use a different nest" refusal are in place, and the bound route is
e2e-proven (authenticates as the bound account; `active` unmoved; legacy
mirror not refreshed). **linux is the second adopter** (2026-07-22,
`main.rs::launch_credentials` — above), **e2e-proven 2026-07-23**
(`test_launch_instance_chooser_linux.py`: picking the chooser's only offered
row completes that process's routing as the picked account, the already-served
sibling untouched; `test_account_switcher_linux.py::test_linux_open_as_new_instance_spawns_a_bound_sibling`:
the spawned sibling authenticates as its bound account while the parent stays
on its own). **windows is the third adopter** (2026-07-23,
`App.OnLaunched`'s bound branch → `SessionMaterial(bound)` +
`BoundLaunchPersistence(bound)`), e2e-proven the same way and additionally by
the coexistence case — two live instances, two accounts, each resolving its
own `session.actor_id` — that windows reached first; linux caught up the
same day (`test_account_instance_lock_linux.py::
test_linux_bound_launch_for_another_account_coexists_as_that_account`) and
tui last (2026-09-01, its own bound session build — the case above's tui
adoption note — proven by `test_account_instance_lock_tui.py::
test_tui_bound_launch_for_another_account_coexists_as_that_account`). The other four apps'
app roots still read their legacy slots, and their legs ride the per-app
isolation tracks. The same recon
surfaced a latent instance of the seam's delete-path corollary, on shipped
serialized-switching surfaces: apple's "use a different nest" launch
fallthrough and its keep-data logout deleted only the legacy keys, which the
next boot re-mirror resurrects from the materialized per-actor slots on any
multi-account install. **Resolved 2026-07-22:** the registry now owns both
delete shapes and maintains the mirror through them — `remove` re-mirrors the
promoted account (or sweeps the legacy keys when the last account goes) and
the new `clear_nest_binding` drops an account's (nest_url, device_id) slots
with the mirror following (FFI: `remove` / `clear_nest_binding`; the
set-or-delete mirror rule now covers the binding keys). Both apple apps route
through them (`useADifferentNest` / `logoutKeepData`, macOS + iOS; a bound
secondary's logout is terminal — it removes its account, never touches the
machine-singleton sync surfaces, and exits), e2e-proven across relaunch
(logout promotes the next account; last-account logout stays signed out). A
2026-07-22 sweep of the other five apps found their plain logout /
walk-away paths CLEAN (namespace wipes, registry `clear_all` / `remove`) with
ONE instance of the resurrection idiom, **web's admin-nest re-point**, which
deleted only legacy `fauna_node_url`/cache keys and kept the per-actor slots.
**Resolved 2026-07-22 (web):** the admin-nest factory-reset handler now calls
`accountsClearNestBinding` (the wasm `clearNestBinding` export over
`AccountRegistry::clear_nest_binding`) instead of a direct
`localStorage.removeItem`, closing the sweep with zero known resurrection
idioms left. **linux adopted it 2026-07-22 too:** the launch screen's "Use a
different nest" fallthrough now calls `clear_nest_binding` before seeding the
onboarding wizard (`main.rs`'s `launch_silent_challenge_flow` fallthrough
closure), best-effort on `account_scope::active_actor_id_hex()` resolving.
Live e2e proof owed (`test_smoke_f_retry_surface_fallthrough_goes_to_handle_entry
--client linux`, held for a quiet-load window);
`cargo build`/`clippy`/`fmt` + the full unit suite are clean. windows/tui/
android still carry the divergence — their walk-away CTAs keep the nest
binding entirely (re-enter the wizard without clearing); their legs adopt
`clear_nest_binding` when they re-key.

*Moved 2026-09-28 to [`account-scoping-dispositions.md`](account-scoping-dispositions.md) § Implementation status today:* **Serialized-switching isolation — the PERSISTED dimension** (all seven apps complete), the **in-memory** dimension (all seven complete), and the shared placement-and-erasure mechanism in `fauna_sync_engine::db` (`actor_state_dir`, `erase_account_scope` / `erase_all_account_scopes`, the retired first-adopter adopters).

**Which seats tell the user, today (2026-09-09).** The shared decision —
`fauna_client_accounts::EraseResidueView`, § Erasure
follows scope — is built and pinned; the seats adopt it one at a time:

| seat | survivors reach the user? |
|---|---|
| tui | **yes, on the residue surface** (2026-09-25) — `account_scope::record_residue` saves the `SignOutResidue` record → `App::sign_out_residue` → the `sign-out-residue` view on `identity_choice` (`wizard::identity_choice::residue_elements`), with `Rendered` copy; Remove Again runs `account_scope::retry_residue` over the shared `retry_sign_out_residue`, and a signed-out launch re-sweeps first (`launch::route`'s `WizardAt` arm → `account_scope::recheck_residue_at_launch`). Covered end-to-end by `test_a_sign_out_that_cannot_erase_everything_says_so`, `test_the_sign_out_residue_retry_finishes_the_erase` and `test_the_sign_out_residue_outlives_the_app` |
| linux | **yes, on the residue surface** (2026-09-25) — `account_scope::record_residue` saves the record into the GTK-thread residue cell BEFORE the sign-out builds the wizard, and `identity_choice`'s `sign-out-residue` view repaints from that cell on every tick (`views/onboarding/identity_choice.rs::build_residue_view`) — state the wizard does not own, so its banner render cannot wipe it (the trap the ⚠ above documents, which the retired sticky notice worked around on `error-message`). Remove Again → `account_scope::retry_residue`; a signed-out launch re-sweeps first (`LaunchRoute::Fresh` → `account_scope::recheck_residue_at_launch`). Same three e2e journeys |
| android | **yes, on the residue surface** (2026-10-02) — `AccountStores.recordResidue` → the `fauna-ffi` residue face's `sign_out_residue_record` (the shared `SignOutResidue`, saved under `filesDir`) → `AppState.signOutResidue` → the `sign-out-residue` view on `identity_choice` (`IdentityChoiceScreen`'s `SignOutResidueView`), the wizard root a sign-out always lands on, with `Rendered` copy. Remove Again → `AppLaunchVM.retrySignOutResidue` → `FfiSignOutResidue::retry` over the shared `retry_sign_out_residue`, re-running the seat's own credential erase (`SignOutCredentialEraser`: `clear_all` → `SecureStorage.clear()` → `reverify_erase`) through the `FfiResidueCredentialEraser` callback; a signed-out launch re-sweeps first (the `Wizard` target → `AppLaunchVM.recheckSignOutResidue` → `sign_out_residue_recheck_at_launch`). The e2e journeys' android arms stay `skip_unbuilt` on the host-side fault injection (the scopes are on-device); `AccountStoresEraseTest` and `SignOutResidueViewTest` pin the record, retry, launch re-check and paint gate |
| macOS / iOS | **yes, on the residue surface** (2026-10-03) — `StatusVM.signOut` hands both erases' outcome (the `AccountStateDir.eraseAll` sweep and the registry's `clearAll()` read-back) to `SignOutResidueSurface.record` → the `fauna-ffi` residue face's `sign_out_residue_record` (the shared `SignOutResidue`, saved under the install base `AccountStateDir.base`) → `SessionState.signOutResidue`, handed onto `OnboardingVM.signOutResidue` by the app root (`takeSignOutResidue`, after a sign-out and after the unreadable-index start-over) before the wizard mounts → the `sign-out-residue` view on the shared FaunaKit `IdentityChoiceView` (one paint serves both targets), with `Rendered` copy — state the wizard's machine does not own, so no machine transition clears it. Remove Again → `OnboardingVM.retrySignOutResidue` → `FfiSignOutResidue::retry` over the shared `retry_sign_out_residue`, re-running the seat's own credential erase (`SignOutCredentialEraser`: the registry's `clearAll()`) through the `FfiResidueCredentialEraser` callback; presses are serialized, never dropped. A signed-out launch re-sweeps first (both app roots' `seedWizard(.identityChoice)` → `OnboardingVM.recheckSignOutResidueAtLaunch` → `sign_out_residue_recheck_at_launch`). Same three e2e journeys (the fault is a read-only scope directory, the POSIX shape); `SignOutResidueSurfaceTests` and `OnboardingVMSignOutResidueTests` pin the seat's plumbing and the view's state |
| windows | **yes, on the residue surface** (2026-10-02) — `App.SignOutHandler` hands both erases' outcome (`ClearCredentialNamespace`: the `AccountStateDir.EraseAll` sweep and the registry's `ClearAll()` read-back) to `SignOutResidueSurface.Record` → the `fauna-ffi` residue face's `sign_out_residue_record` (the shared `SignOutResidue`, saved under the install base `%LocalAppData%\Fauna`) → `ServiceClients.SignOutResidue` → `OnboardingViewModel.SignOutResidue` → the `sign-out-residue` view on `IdentityChoiceView`, with `Rendered` copy — state the wizard's machine does not own, so no observer tick wipes it (the trap linux's ⚠ above documents, which the retired `StickyResidueNotice` worked around on `error-message`). Remove Again → `OnboardingViewModel.RetrySignOutResidueAsync` → `FfiSignOutResidue::retry` over the shared `retry_sign_out_residue`, re-running the seat's own credential erase (`SignOutCredentialEraser`: the registry's `ClearAll()`) through the `FfiResidueCredentialEraser` callback; presses are serialized, never dropped. A signed-out launch re-sweeps first (`DispatchLaunchSnapshotAsync`'s `WizardAt(IdentityChoice)` arm → `SignOutResidueSurface.RecheckAtLaunch` → `sign_out_residue_recheck_at_launch`). The unreadable-index start-over runs the same erase and records the same way; the e2e reset/logout arms record nothing. Same three e2e journeys (the fault is a held SQLite lock on a file inside the scope — the windows-specific shape, since Windows' POSIX delete semantics unlink through an ordinary read handle); `SignOutResidueSurfaceTests` and `OnboardingViewModelSignOutResidueTests` pin the seat's plumbing and the view's state |
| web | **yes, on the residue surface, for the account store** (2026-10-01; the credential half is ruled out of the class) — `account_scope::finish_recorded` counts the accounts whose bounded erase failed → `signOutFinish`'s `residue` → `$lib/accounts`'s `signOutResidue` store → the `sign-out-residue` view on the onboarding page's `identity_choice`, derived on every render while nobody is signed in, with `Rendered` copy. Remove Again → `signOutFinish('retry')`, the same routine behind the other-tab probe. The sign-out record keeps the account, so the view outlives the page: a later load sweeps again first and repaints only if a store is still left. Covered by `test_sign_out_web.py::test_web_sign_out_that_cannot_erase_a_store_says_so_and_a_later_load_finishes` (a held IndexedDB connection that ignores `versionchange`) and `…::test_web_sign_out_residue_retry_refuses_beside_another_tab_and_finishes_the_erase` |

**The residue surface (§ Erasure follows scope → *the residue surface*) is on
all seven apps** (macOS / iOS, one shared FaunaKit seat, was the last, on
2026-10-03) — every seat paints the `Rendered` copy (web over its sign-out
record and its own sweep, the web paragraph's decision 4; android, windows and
macOS / iOS over the `fauna-ffi` residue face,
`libs/fauna-ffi/src/sign_out_residue.rs`, which owns the record, the retry and
the launch re-check and calls back into the seat's own credential erase). The
roll-out's scaffolding retired in the commit that landed the last seat: the
`EraseRetryAffordance` enum and its `Absent` arm, the three
`settings.sign_out_residue*_no_retry` strings, and the `fauna-ffi` exports
`erase_residue_copy` and `erase_residue_with_credentials`, which no seat called
once each took its line from `sign_out_residue_record`.

**The credential half reached all five seats on 2026-09-13** (§ Erasure follows
scope → *the credential half is a residue class too*). tui and linux build the
line from `fauna_credential_store::erase_all_credentials` — the registry erase,
the namespace wipe with its `Err` kept, and a read-back — after the filesystem
erase instead of beside it; the three FFI seats hand `FfiAccountRegistry::clear_all`'s
read-back to the residue face's `sign_out_residue_record` beside the sweep
(through `erase_residue_with_credentials` until the residue face replaced it;
windows'
`ClearCredentialNamespace`, apple's `StatusVM.signOut`, android's
`AccountSettingsVM.signOut`, which re-asks with `reverify_erase` after
`SecureStorage.clear()`). Pinned in shared Rust by `clear_all`'s refused-delete
tests and `erase_all_credentials_reports_what_a_store_refusing_the_erase_kept`
(a read-only credential directory); end-to-end by
`test_a_sign_out_whose_credentials_cannot_be_erased_says_so`, green on tui,
linux, macOS, iOS, and windows — apple's native build (FaunaKit + `mac-debug`)
passed too, and windows' native build
(`windows-debug` + `windows-cs-test`) passed too. Android's arm is `skip_unbuilt` (the host can neither read back nor deny writes
to the credential file the on-device bridge keeps in the app's own `filesDir`),
so android's *fold after the LAST wipe* order is pinned by
`AccountSettingsVMTest.signOut_foldsTheCredentialReadBackTakenAfterTheSecureStorageReset`
alone; web's arm is a declared absence.

**Every sign-out folds a residual sweep beside its per-actor loop** (tui since
2026-09-10, the shape linux already had — priority #4): an actor scope the
registry no longer names — precisely what a **failed** erase creates — is
swept by `erase_all_account_scopes` over every base the erase reaches, so a
stranded scope cannot outlive the next sign-out.

*Moved 2026-09-28 to [`account-scoping-dispositions.md`](account-scoping-dispositions.md) § Implementation status today, verbatim and in their original order:* One known gap, not blocking a leg (**the scoped MLS filename is not uniform**); anything that is not an actor scope stays put (the install-scoped survivors and the per-app `AccountStateDir` / `AccountStores` shape); the **Isolation-contract gap ledger** — one row per app for the persisted dimension (apple, windows, linux, tui, android, web) and one per app for the in-memory dimension (web, apple, windows, linux, tui and android `(in-memory)`); and the cross-app fixed points already correct.
