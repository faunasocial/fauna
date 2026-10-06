# Long-term identity store contract — target state

Owns: long-term-store, multi-account
Status: ratified
Authority: the per-app long-term identity store — the three-slot contract (secret_key / device_id / node_url), three-case launch routing, the per-app secure-store implementations + server-data cache + save-failure convention + cleanup contract, and the multi-account evolution (AccountRegistry index/namespacing/additive migration, legacy mirror + abandoned-append recovery, per-account re-auth); defers the wizard / pending-invite / awaiting-DNS slots to [`../behavior/onboarding.md`](../behavior/onboarding.md) § Long-term store contract, the silent-challenge ceremony to [`../behavior/login.md`](../behavior/login.md), and at-rest encryption properties to [`encryption-at-rest.md`](encryption-at-rest.md). Provenance: the 2026-04-28 onboarding-persistence-cleanup design + the 2026-07-01 multi-user-clients design (frozen work products).

The long-term identity store is the per-app surface that survives
launches. This document is the canonical cross-app description of
what the store holds, how each platform implements it, and the
invariants every app honours so the wizard, launch flow, and
authenticated session behave identically.

The store is **not** a wizard scratchpad. The onboarding state machine
runs in memory only — it accepts an in-memory store at construction
time, never reads from disk, and never resumes mid-wizard. Long-term
state is only ever written at two specific moments:

1. **Confirm-identity** (generated or imported): write `secret_key`.
2. **Complete-login** (after `nest_login` succeeds): write `node_url`
   and `device_id` (creating `device_id` if not already present).

Anything else is a bug.

## The three slots

| Slot         | Type        | Set by                          | Read by                                                  |
|--------------|-------------|----------------------------------|----------------------------------------------------------|
| `secret_key` | 64-hex ed25519 secret | `confirmGeneratedIdentity` / `confirmImportedIdentity`  | every authenticated request (signs auth challenge)       |
| `device_id`  | UUID-shaped (server-issued or generated) | `completeOnboarding` / `complete-login` flow | sync register, push registration, telemetry             |
| `node_url`   | absolute URL of the home nest | `completeOnboarding` / `complete-login` flow | every API call's base URL                                |

Slots are independent — each may be set or absent without affecting the
others. Partial state is intentional: it's how "force-quit between
confirm-identity and complete-login resumes at HandleEntry" works.

## Three-case launch routing

Every app's launch flow reads the three slots and routes the user:

1. **All three present** → silent challenge → home (authenticated session).
2. **`secret_key` present, `node_url` empty** → onboarding wizard,
   pre-seeded via `seedIdentity(secret)` so the user lands at HandleEntry
   without re-pasting their secret.
3. **None present (or `secret_key` empty)** → onboarding wizard from the
   top.

`device_id` does not gate routing. It's an "if present, use it; if
absent, generate one at complete-login time" field.

**The read contract mirrors this at the source (ratified 2026-08-18).** A store read of the
three-slot trio returns `None` whenever `secret_key` is absent, *even if* `node_url`/`device_id`
exist on their own (e.g. an abandoned append that wrote a stray field before the identity import
step completed) — never a triple carrying an empty-string `secret_key` a caller might mistake for
a real one. This is case 3 enforced by the read, not left to every routing call site to
re-derive it correctly. When `secret_key` *is* present, an absent `node_url`/`device_id` still
reads as `""` — the load-bearing case-2 shape (the secret-only seed branch). Linux implements
this in `fauna_credential_store::load_credentials_file` and its libsecret twin
(`apps/fauna-linux/src/client.rs::load_credentials_in`); other platforms' equivalents should agree.

## Per-app implementations

All seven apps implement the three-slot contract on top of
platform-native secure storage (tui shares linux's `CredentialStore`):

### Apple (`KeychainStore`, `apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/KeychainStore.swift`)

Three Keychain items, one per slot, distinguished by `kSecAttrAccount`
(`secret_key`, `device_id`, `node_url`). Failure-mode: save errors logged
via `logMessage(level: .error, target: "fauna.onboarding", message: ...)`
(the shared capture lift replaced the earlier raw `print(...)`; see
[`apps/observability.md`](apps/observability.md) § The emit API) and
swallowed; load returns `nil`.

### Linux + tui (`libs/fauna-credential-store`, `apps/fauna-linux/src/client.rs`)

The registry's rows and nothing else (2026-09-24): libsecret items in the
user's default collection sharing `application=fauna-desktop` (tui:
`fauna-tui`), one per registry logical key — `account=fauna/index` and
`account=fauna/{actor}/secret|nest_url|device_id|…` verbatim
(`CredentialStore`; a JSON file per namespace under
`FAUNA_E2E_CREDENTIAL_DIR` for e2e). The relaunch-display cache (handle /
domain / tier) is the active account's index entry
(`AccountRegistry::update_cache` / `session_material`), read through
`client::load_account_cache` with an in-process mirror for a locked keyring.
The pre-registry `secret_key` / `device_id` / `node_url` / `handle` /
`domain` / `tier` items and the single-blob migration are gone. Failure-mode:
save errors logged via `tracing::error!` (best-effort + log); durability,
where it matters, is proven by read-back (`persist_confirmed_identity`).

### Windows (`apps/fauna-windows/FaunaApp/FaunaApp.Core/Services/LogicalSecretStore.cs`)

Windows Credential Manager generic credentials, one per registry row, reached
through the shared multi-account seam: `LogicalSecretStore` implements
`FfiSecretStore` over a `SecretKeyMap` (logical key → `Resource`) and a
swappable backend (Credential Manager through the shared Rust arm, or a JSON
file under `FAUNA_E2E_CREDENTIAL_DIR` for e2e). Every row is a logical key
stored verbatim (2026-09-25) — the `legacy/*` remap and the typed `ISecretStore`
face that read through it are retired, and the app reads its identity through the
registry alone. The backend is owned by
[`apps/windows.md`](apps/windows.md) § Key storage. Failure-mode: writes are
best-effort by contract — a failed write is logged, never surfaced to call sites
(durability, where it matters, is proven by read-back — see CR-1).

### Web (`apps/fauna-web/src/lib/onboarding/machine.svelte.ts` + `apps/fauna-web/src/lib/store.ts`)

The registry's rows and nothing else (2026-09-24): `localStorage` keyed by
every registry logical key verbatim — `fauna/index` and
`fauna/{actor}/secret|nest_url|device_id|…` (`LocalStorageSecretStore`;
names in [`apps/web.md`](apps/web.md) § localStorage Keys). The device-id
slot is filled lazily by the derived per-account id's get-or-create and never
at onboarding ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md)
§ Credential model) — an "if present, use it" field per § Three-case launch
routing, so it changes no routing. The wizard wrappers
(`confirmGeneratedIdentity`, `confirmImportedIdentity`) call the shared
`persistConfirmedIdentity(secret, append)` (fire-and-forget, `console.warn`
on failure); the SPA resolves its identity through
`accountsSessionMaterial` (the active account, or a pinned tab's own). The
pre-registry `fauna_secret` / `fauna_node_url` / `fauna_handle` / … keys are
neither written nor read.

Web's **server-data cache** (handle / domain / tier) is the active account's
index entry (`updateCache`), so the settings UI shows the user's identity
instantly on relaunch without a network round-trip. These are explicitly
*not* part of the long-term identity store contract — a UX nicety, populated
from the registration response, refreshed on every cold launch via a
single-flight `silentSignIn` call from `identity.init()`, with the server as
the source of truth (so the cache can drift if a record changes out-of-band
between launches).

## Cross-app server-data cache

The server-data cache lives alongside the long-term store on each
platform — same backend, distinguished by naming and intent. Apple uses
`KeychainStore` keys `cachedHandle/cachedDomain/cachedTier`; Linux uses
libsecret items with `account=handle/domain/tier` (tui shares this backend
via the same `CredentialStore`); Windows keeps them in the active account's
registry index entry, in its Credential Manager store. All seven apps write the handle when
registration completes (or via the test-agent session-patch path) and
load it at launch so the user sees their identity immediately rather
than waiting for `/api/v1/account`.

The `fauna.auth.verify` WS-RPC kind returns `tier` alongside `handle`/`domain`,
so a single round-trip refreshes the entire cache. All seven apps
do this refresh on every cold launch:

| App     | Launch-time refresh entry point                                  |
|---------|------------------------------------------------------------------|
| Web     | `identity.init()` → single-flight `silentSignIn` after the localStorage read |
| Linux   | `launch_authenticated` → `FaunaClient::silent_sign_in` after `authenticate()`; result piped through `DataMessage::IdentityRefreshed` |
| Apple   | `checkKeychainOnLaunch` (iOS + macOS) → background `Task` calling `APIClient.silentSignIn(secret:)`, writes results into `KeychainStore.cachedHandle/Domain/Tier` and `SessionState.handle` |
| Windows | `App.xaml.cs.OnLaunched` → `LaunchMachine.Start()` via the shared `RegistryLaunchPersistence` (`FfiAccountRegistry.LaunchPersistence()` — the per-app `SecretStoreLaunchPersistence` this row once named was deleted 2026-07-14, § Implementation status today); the silent-challenge ceremony returns `handle`/`domain`/`tier`, and the persistence's `SaveAuthenticated` callback writes them into the active account's `ISecretStore` cache slots (the launch glue reads `LoadCachedHandle()` for `_handle`) |
| Android | `LaunchMachine.start()` via the shared `RegistryLaunchPersistence` (`LaunchModule.provideLaunchPersistence` over `FfiAccountRegistry`, apps/fauna-android/app/src/main/java/com/fauna/app/di/LaunchModule.kt — the per-app `LaunchPersistenceImpl` this row once named was deleted, § Implementation status today); snapshot consumed by `AppLaunchVM` and rendered by `FaunaNavHost` |
| tui     | `session::establish()` → `spawn_domain_refresh` → `silent_refresh` → `AccountRegistry::update_cache`; result posted via `DataMessage::SelfAddressRefreshed` |

Failures (network, nest down, 404 unregistered) are logged and
swallowed — the cache values from the previous successful refresh stay
visible.

## Save-failure handling

Every app follows the same convention: save failures are
**best-effort + log**. A failed save means the user has to re-onboard
on the next launch, which the wizard already handles. Crashing the UI
on save failure (the pre-fix Windows behaviour) is the worst-of-both:
the user loses their secret AND has to file a bug. Silent drop (the
pre-fix Apple `try?` and Web `try { ... } catch { /* swallow */ }`
behaviour) makes diagnostics impossible.

| App     | Logging surface                                          |
|---------|----------------------------------------------------------|
| Apple   | `logMessage(level: .error, target: "fauna.onboarding", message: "[OnboardingVM] keychain save … failed: \(error)")` |
| Linux   | `tracing::error!("[onboarding] committing the confirmed identity failed: {e:#}")` (append mode: `"[onboarding] persisting the appended identity failed: {e:#}"`) |
| Windows | `Debug.WriteLine($"[VaultSecretBackend] set {resource} failed: {ex.Message}")` + a twin `ShellLog.Error("SecretStore", …)` |
| Web     | `console.warn('[onboarding] localStorage.setItem(fauna_secret) failed:', e)` |
| Android | `ShellLog.w("IdentityCreatedVM", "persist_confirmed_identity (registry commit + read-back) failed: …")` (+ the import twin), and `ShellLog.w(TAG, "persistLoggedIn failed: …")` / `"persistAwaitingDns failed: …"` at the wizard terminals in `core/OnboardingHost.kt` |

## Cleanup contract

`DeleteAll()` / `delete_credentials()` / `logout()` wipes the app's whole
credential **namespace** in one call, matching what "Sign Out" (or `--reset` on
Windows) does. After cleanup, the next launch routes to case 3 (fresh
onboarding). The persistence-v2-clean orphan-cleanup flag (`fauna_persistence_v2_clean`)
has been fully retired — apple and windows removed their one-shot legacy-scratchpad wipe
first, and web and linux removed theirs in the same compat-remnant sweep pass
([`version-compatibility.md`](version-compatibility.md) § Dimension 2); android never carried it.

**Under multi-account, "all three slots" means the whole credential
namespace** — *every* account's `fauna/{actor_id}/*` slots and the
`fauna/index` blob — not just the active account's. Sign-out is the
all-accounts erase; removing one identity while staying signed in is the
switcher's per-row affordance (`AccountRegistry::remove`). Two properties are
load-bearing, and an app that clears only the active account's slots violates
both:

1. **The signed-out secret is gone.** A `fauna/{actor}/secret` that outlives
   sign-out leaves the user's private key recoverable on a shared browser or
   desktop — sign-out did not sign them out.
2. **The next launch really does route to case 3.** `fauna/index` is what
   launch routing reads (natively via `RegistryLaunchPersistence`, on web via
   `accountsBoot`). An index that outlives sign-out still names the signed-out
   account active, so the next launch routes on it — silently signing the user
   back in as the identity they signed out of, and shadowing any identity
   onboarded since.
3. **No credential *derived* from the identity outlives it either.** A cached
   bearer is a credential: any auth-token cache is keyed on `(nest, identity)` —
   never on the nest alone — and is dropped on sign-out. An app whose sign-out
   navigates *client-side* (web `goto`s rather than reloading) keeps module state
   across the transition, so the next identity to sign in within that same document
   would otherwise inherit the signed-out actor's bearer and talk to the nest **as
   that account** — authenticating as someone who just signed out. Web:
   `tokenKey(base, secretHex)` + `clearTokenCache()` from `logout()`
   (`apps/fauna-web/src/lib/api.ts`).

The erase is **delete-only**: it must never create a slot on the way to
deleting one, so a crash mid-erase can never resurrect the identity being
erased (the hazard was concrete while the retired single-slot migration
existed: reading the index through a migrating accessor copied the old secret
into a fresh per-actor slot first). The shared implementation is
`AccountRegistry::clear_all()`; the two freedesktop apps (linux, tui) *also*
wipe their own namespace with `CredentialStore::delete_namespace()`, which
erases every item carrying the `application` namespace — a superset of the
enumerated keys, in one D-Bus round trip rather than one per key.

**Both, in that order, and neither alone** (2026-09-01, § Implementation status
today hole 3). The namespace wipe is a superset only *within* one namespace, and
per-actor material lives in two: the app's own, and the shared
`fauna-account-store` where this machine's writer key and each account's
principal bundle sit. Only the per-actor sweep crosses that line — and it must
run first, because it enumerates through the registry index the wipe destroys.
Widening the wipe instead is not the alternative it looks like: `delete_namespace`
on the shared namespace would take *every* app's accounts on the machine, not
this app's. The sequence has one home, `fauna_credential_store::erase_all_credentials`,
which both freedesktop apps call.

**The erase reports what survived it, because the store will not.**
`SecretStore::delete` returns nothing and every arm swallows its own failure, so
`AccountRegistry::clear_all()` reads back every key it deleted — in exactly the
namespaces it deleted it from — and returns a `CredentialSweep` naming what still
reads. `erase_all_credentials` keeps `delete_namespace`'s `Err` beside it
(`wipe_failed`) and re-reads the survivors after the wipe (`reverify`); a seat
whose platform store resets itself after `clear_all` (android) re-reads the same
way. Property 1 is otherwise unobservable: a locked keyring, a refused keychain
delete, or a read-only credential file left the identity seed in place behind a
clean "Signed out". Where that outcome reaches the user is
`account-scoping.md` § Erasure follows scope → *the credential half is a
residue class too*. One limit, stated rather than over-claimed: a store that
refuses reads as well as deletes reads as empty, so on the FFI seats — which run
no wholesale wipe whose error could stand in — such a store still reports
clean.

**The delete-only hazard has a mirror image: an erase is not atomic against a
concurrent writer, so an app must quiesce its writers before it wipes.** No
secure store offers delete-by-attribute in one transaction, so
`delete_namespace()` searches, deletes, and searches again until the namespace
reads empty. That still cannot beat a writer already *mid-write* when the erase
begins. An erase landing inside such a window leaves behind whatever the writer
emits after it — up to and including the identity secret, defeating property 1.
So the ordering is part of the contract: drop the launch machine, the client,
and any registry reader **first**; erase **last**.

**Ordering is necessary but was never sufficient, and on the file-backed arms
it is now backed by a lock.** A client cannot actually quiesce every writer: it
has un-cancellable workers of its own, and the sync agent is a *separate
process* writing the same namespace (under e2e `FAUNA_KEYRING_APP` collapses
the two namespaces onto one file, so the collision is not even hypothetical).
On the file and sealed backends every mutation is a whole-file
read-modify-write, so an erase and a concurrent mutation must never interleave —
otherwise properties 1 and 2 rest on a process the erasing client has no handle
on. Both file arms
therefore take a per-namespace kernel-arbitrated advisory lock
(`fauna_core::fs_lock`, the same mint as the account-registry and instance
locks) across **both halves** of every mutation and across the erase itself, and
write via temp-file-plus-rename so an unlocked reader never sees a torn map
(the invariant since 2026-08-19). Two
limits worth stating rather than over-claiming: the **keyring** arms
(libsecret / Keychain / Credential Manager) have no such lock — there the
ordering discipline above is still the whole guarantee — and the lock is
per-*mutation*, not per-*transaction*, so a foreign writer may still interleave
between two `set`s of one registry mutator. What it does buy is absolute: no
single write can be lost, and no write can straddle an erase.

The contract has a second, structural half, because ordering alone cannot hold
it. **A read never writes** (`AccountRegistry`'s type-level invariant): no
read path persists anything. It was once violated by the (since deleted)
one-shot migration of the pre-registry single slot — four sequential `set`s
triggered by an ordinary read, reachable from any thread — which made every
reader a potential mid-wipe writer, including readers an app cannot cancel (a
silent challenge on a detached worker). No call-site ordering can fix that; only
removing the write can. One consequence worth stating, since it is
load-bearing:

- **An un-cancellable reader that races a wipe is harmless.** A post-wipe
  `save_authenticated` finds no active account (the index is gone) and writes
  nothing — the guard it always had, no longer defeated by a read that
  resurrected the identity behind it.

*History (2026-08-25 → 2026-09-28):* while the pre-registry single slot could
still be migrated lazily, "the mutators run migration first" was enforced at the
registry's write chokepoint (`AccountRegistry::writable_index()`), because an
index naming an actor with no per-actor slots behind it left the old slot as
that identity's only copy — client-only-resident key material
(`../principles.md` § No user-data loss) the mirror then overwrote. With the
slot, the migration and the mirror deleted, no write can name such an actor;
the chokepoint keeps its other job, refusing to overwrite an unreadable index
([`version-compatibility.md`](version-compatibility.md) § 5 item 9).

## Multi-account evolution (target state)

> The registry below is the credential/identity half of the multi-account
> story. The broader client data-scoping taxonomy, the capability stages
> (serialized switching → concurrent instances → concurrent identities), and
> the per-surface scoping dispositions are owned by
> [`apps/account-scoping.md`](apps/account-scoping.md) — note its
> stage-name disambiguation: the "Stage 0–3" vocabulary in this section is
> the *implementation* staging of the switching surface, not the capability
> stages.

> Design: the multi-user-clients design (2026-07-01, tracked internally).
> One client install (one OS-user context) may hold **several identities
> (actors)** and switch between them — e.g. a social identity plus a
> separate admin identity. This is a purely client-side layer over the
> three-slot contract above; the **nest is untouched** (two identities =
> two actors, two bearer tokens, independent auth).

The three slots become **per-actor**, namespaced by actor id, plus one
**non-secret index blob**:

- `fauna/index` — the account list + active pointer + each account's
  server-data cache (handle/domain/tier) and per-account flags. Authoritative
  for *which accounts exist* and *which is active*, so no platform needs to
  enumerate its secure store. JSON, non-secret.
- `fauna/{actor_id}/{secret,nest_url,device_id}` — the same three slots as
  above, namespaced per actor — plus the optional `fauna/{actor_id}/reach_ipv4`
  (added 2026-08-29): the freshly-provisioned box's public IP, kept beside
  `nest_url` until the domain first connects and deleted then; semantics owned by
  [`../behavior/onboarding.md`](../behavior/onboarding.md) § Reach hint. Absent
  on every account that did not provision its own box.
- a `fauna-parked/{logical key}` namespace and its merge
  (`AccountRegistry::adopt_parked_index`) existed 2026-08-27..2026-09-25 for
  apple's retired keychain-service copy-forward; both were removed with it by the
  compat-remnant sweep ([`version-compatibility.md`](version-compatibility.md)
  § Dimension 2, the fourth ratified exception). No platform writes one.

A **single-account** app is exactly the `accounts.len() == 1` case — this
is backward compatible, not a new mode.

**One nest binding per actor — re-adding re-homes (recorded 2026-08-11).** The
registry holds exactly one `nest_url` per actor, and `add_account` with an
already-known actor id writes the new binding **unconditionally** while pushing
no second index row (pinned by `add_is_idempotent_for_same_identity`) — so
importing the same identity against a different nest silently re-points that
actor's home binding, last login wins, with no UI trace of the old one. That is
the deliberate shape, not a gap: "one user on several nests" is **not** a
registry concern — nest-side it is per-user multi-homing / linked nests
([`../behavior/linked-nests.md`](../behavior/linked-nests.md), reached by
transient authenticated peer connections, never a second durable binding), and
several-identities-in-one-install is the multi-account layer above. Don't grow
the actor row into a nest list.

**No migration from the pre-registry single slot.** *History:* until
2026-09-24 a first load with no `fauna/index` but a pre-registry single-slot
secret derived the actor id, copied the old `secret`/`nest_url`/`device_id` into
the per-actor namespace and wrote an index naming it, never deleting the old
keys so a downgraded client kept reading them. The compat-remnant sweep retired
that job ([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program 4: no
pre-registry install exists), and its code was deleted 2026-09-28 with
android, the last app still on the slot (§ Downgrade mirror + abandoned-append
recovery).

**Shared seam.** All of the above — the index and the per-actor namespacing —
lives in the shared crate `libs/fauna-client-accounts` (`AccountRegistry` over a
logical-keyed `SecretStore` trait). Each platform implements only the thin
`SecretStore` glue, storing every logical key verbatim in its native store. This subsumes the per-platform typed persistence logic
into shared Rust (priority #2). The existing typed
`fauna_launch_machine::LaunchPersistence` seam is reconciled onto this registry
by the shared adapter `RegistryLaunchPersistence` (also in `fauna-client-accounts`):
it implements `LaunchPersistence` by reading the slots of the registry's
**active** account, so a platform implements only `SecretStore` — one underlying
foreign seam, not two parallel foreign traits. The launch machine's four-case
routing (`machine.rs` `start()`) is unchanged but now reads whichever identity is
active; switching accounts is `set_active` + a client teardown/rebuild of the
launch machine (design Decision 1, switch-first). **Every app routes launch
through `RegistryLaunchPersistence` — web included** (since CR-3, 2026-07-13:
the `fauna-wasm-launch` chunk constructs the machine over the registry store
Rust-side, so web cannot hand it a bespoke single-slot store; web's remaining
direct-`AccountRegistry` consumption is the switcher glue in
`fauna-wasm/src/accounts.rs`, over the SAME `LocalStorageSecretStore`). The shared load-bearing piece is the registry + the
`AccountIndex`/`AccountEntry` wire contract. The wizard-resume slots
(`onboarding.md` § Long-term store contract) become per-actor here:
`fauna/{actor_id}/pending_invite`, `fauna/{actor_id}/awaiting_dns` and
`fauna/{actor_id}/pending_factory_reset`. All hold opaque JSON the registry never parses,
and both are swept with their identity by `remove()` / `clear_all()`.

**Downgrade mirror + abandoned-append recovery — RETIRED 2026-09-24; the
registry is the only store.** The `legacy/*` single slot, its boot/switch mirror
(`AccountRegistry::mirror_active_to_legacy()`), the one-shot `migrate_legacy()`
and the read fallbacks that served an un-migrated install are no longer part of
the contract on any platform. What replaced them is the **onboarding hand-off
writing the registry directly, with the append rule inside the shared moment**:
`persist_confirmed_identity(registry, secret, append)` registers and activates a
first-run identity (moment 1, with its read-back), and with `append` it writes
**nothing** — the appended identity stays in the wizard machine
(`effective_secret()`) until its own terminal registers and switches
(`persist_logged_in` / `persist_pending_invite` / `persist_awaiting_dns`, then
the app's switch). The one write before that terminal is custody, not
routing: a provisioning run's mint registers the appended identity *inactive*
beside its pending-provision slot before `create_server`, and the
pending-provision writer never moves the active pointer (owner:
`behavior/onboarding.md` § Multi-account → *Append-mode deferred/incomplete
states + abandonment*). That is the crash-safe single decision point
(`nest/common.md` § Client-state recoverability): an abandoned append leaves the
routing exactly as it was — the active pointer and the live account's rows
untouched, at most one inactive custody row beside them — so there is nothing
to shadow the active account and nothing to heal; launch routes on the
registry's active account alone
(`RegistryLaunchPersistence`), and every in-run read of the identity — the
awaiting-DNS write, the terminal's device id, the cached handle — goes through
the registry (`secrets()` / `session_material()`), never a single-slot key.
tui had this shape since 2026-07-19 ([`apps/tui.md`](apps/tui.md) § Append-mode
"Add account" — "an append-mode run writes nothing to the store until
`add_account`"), and it was lifted into shared Rust as the rule for all seven
apps rather than kept as one app's property. Web's in-memory identity recovers
on the same page load as before, now by re-reading the registry's active
account. *History:* the mirror once had two jobs — keeping a downgraded
single-identity client reading the `legacy/*` keys (retired by the
compat-remnant sweep, [`compat-remnant-sweep.md`](compat-remnant-sweep.md)
§ Program 4: no pre-sweep client exists) and healing the single
slot every app's append confirm used to overwrite with a not-yet-registered
identity, which without the heal mis-routed the next launch into that identity's
wizard. **The transition closed 2026-09-28, with android.** From 2026-09-24
until then android still read its native single-slot rows at launch, in the
wizard and in its views, so the UniFFI seam built its registry with a
transitional `AccountRegistry::with_legacy_single_slot()` flag that kept the
mirror, the migration and the fallbacks live there; web, linux and tui never set
it, and windows and apple (2026-09-25) stopped reading or writing a `legacy/*`
key while the flag still ran behind them. android's leg, the last, moved its
reads onto the registry's session material and deleted, with the flag, the
`legacy/*` constants, the mirror and migration, the FFI
`persist_confirmed_identity_mirrored` / `mirror_active_to_legacy` /
`ensure_migrated` exports, android's key map and the cross-language guard. No
registry reads or writes a `legacy/*` key.

*History — the mirror's own rules while it existed (deleted 2026-09-28). Kept
because the reasoning still explains shapes the registry keeps.*

**The mirror only ever runs from a materialized index — and refuses on its own
(2026-07-14).** The legacy keys are a mirror of the per-actor slots, never a
source, so with no `fauna/index` there is nothing to mirror *from*: the legacy rows
are themselves the only copy. Running anyway is not a harmless no-op but a
**destructive** one, and the path is easy to walk into: `active()` still resolves
with no index (the in-memory legacy-derived index, § above), so the mirror does not
bail; it then finds no per-actor pending slot, and the set-or-clear rule *clears*
the legacy fields to match — erasing the claim code of an already-wiped box on the
first boot after upgrade. That is **CR-1 reached through the upgrade door**: exactly
the loss the CR-3 legacy bridge below exists to prevent. `mirror_active_to_legacy()`
therefore **gates itself** on the index being present *and readable*, rather than
each app gating its own call site (linux originally did the latter, and its gate
is now belt-and-braces). Consequence for a fresh / single-account / mid-onboarding
install: untouched, and its lazy legacy→account-#1 migration is unchanged — the same
outcome as before, but now guaranteed by the shared crate instead of by four
platforms each remembering an `if`. Native apps therefore call the mirror
unconditionally at boot and switch. Unit-proven:
`a_boot_mirror_on_an_unmigrated_store_preserves_the_legacy_pending_slots`.

**The mirror rescues an identity the legacy slot alone still holds (2026-08-25).**
The gate above covers the *un-migrated* install. It does not cover a store an
older build left **half-materialized** — an index that names the legacy actor
with no per-actor slots behind it (§ A read never writes → the chokepoint note).
Migration is past that store's reach by design: it refuses once an index exists.
So for it the legacy slot is still the identity's only copy, and this mirror is
the write that destroys it. Immediately before replacing the slot, the mirror
therefore materializes whatever identity it still holds — **only if the index
names it and its per-actor secret slot is missing**. Both conditions are
load-bearing: rescuing an identity the index does *not* name would resurrect the
abandoned append this same mirror exists to heal, and the missing-slot check
keeps the steady state a single pure read. Additive, never a delete
(`../principles.md` § No user-data loss). Unit-proven:
`the_mirror_rescues_an_indexed_identity_the_legacy_slot_alone_still_holds` and
`the_mirror_never_rescues_an_identity_the_index_does_not_name`.

**The legacy keys are read on exactly two paths (2026-07-22).** With session
identity resolving through the registry (`apps/account-scoping.md`
§ Concurrent instances → *Session identity resolves through the session's
account*), the `legacy/*` keys keep both write jobs above but are **read** in
only two places: (1) the **onboarding wizard scratchpad** — the wizard predates
the registry and works the single slot directly through identity create/import
and the append handshake ("Add account" reads the wizard's legacy writes back
to register the new identity); primary-instance-only, by the bound-wizard
refusal that doc owns; and (2) the **shared crate's own fallbacks** —
`secrets()`'s un-migrated-install fallback and the CR-3 legacy bridge — which
exist so reads stay pure while migration is lazy. Any other read of a legacy
key in client code is drift against that rule. A client-side **delete** of a
legacy key alone is likewise never a credential drop: once the per-actor slots
are materialized (any multi-account install — `add_account` materializes the
legacy identity first), the next boot mirror rewrites the legacy keys from the
active account, resurrecting exactly what the delete removed — credential
drops go through the registry's per-actor surface. Per-app adoption of the
session-identity seam is tracked in `apps/account-scoping.md`
§ Implementation status today.

**Adding refuses a secret the store did not keep (2026-09-26).** `add_account` reads the
secret slot back straight after writing it and, when the store kept nothing, returns
`AccountError::NoStoredSecret` **before** writing the other slots or the index — so `Ok` from
it is the promise that the account can launch, and a refused add leaves no
indexed-but-unlaunchable row behind for `set_active` to meet. This is the add-side twin of
the activation refusal below, and it is what lets every caller that persists an identity it
cannot regenerate — the succession ceremony's successor seed above all — take the
show-the-key branch instead of switching onto a key it never saved (`../ui/settings.md`
§ Recovery kit → *The persist-failure message survives the page*). `persist_confirmed_identity`'s
own read-back predates it and is now redundant but harmless. Unit-proven:
`add_account_refuses_a_secret_the_store_did_not_keep`; the e2e fault that stages it is the
registry bridge's `refuse_secret_writes_for_test` (`../behavior/onboarding.md` § E2E bridge
contract).

**The index is bounded: a change that would grow it past one credential item is refused before anything is written (ruled 2026-10-04).** `fauna/index` is one credential item that grows with every account and every succession link, and the tightest backend, Windows Credential Manager, holds 2560 bytes per item behind a write that reports nothing — an over-cap index there keeps its previous value, and a secret slot written ahead of it is an identity the device holds and cannot reach. The bound is `fauna_client_accounts::MAX_INDEX_VALUE_BYTES` = 2560 bytes **on every platform**, one number rather than a per-OS surprise (the same rule as the retained-keys carriage, [`account-runtime.md`](account-runtime.md)); `fauna-credential-store` depends on the registry, so it pins that constant equal to its own `MAX_ITEM_VALUE_BYTES` at compile time. **What that holds, measured** (fixed 64-hex actor ids, `index_bound_tests.rs`): 12 accounts added and never signed in; 11 signed-in accounts, each caching an 8-character handle on a 16-character domain; or 4 signed-in accounts that each succeeded a predecessor whose retired row the device still holds (8 rows, both halves of every link). A change to the entry's serialized shape moves these numbers, and the test pins them. **The shape is refuse, not split.** The registry encodes the index it is about to write first and, when that is over the bound **and longer than the index the store holds now**, returns the typed `AccountError::IndexFull { needed, limit }` and writes nothing — no secret slot, no other slot, no index. The check sits on the single index write every mutator goes through, so the add, the succession links, the cache write and the flags all meet it, and a mutator that writes slots (the add) runs it before its first slot. Splitting the index across several items was rejected: the index is what makes every identity secret reachable, one item is written whole or not at all, and a list spread over items can be torn by a crash between two writes into a state no app can read — a client-causable unrecoverable state (`nest/common.md` § Client-state recoverability) traded for room past a count no install is expected to reach; a split stays possible later as an additive change under the index's two-number scheme. **Growth is what is refused, never the index's size as found:** a rewrite that leaves the index no longer than it was always lands, so removing an account, switching and clearing a flag work on a full index, removal makes room for the next add, and an index a pre-bound build wrote past the cap on a roomier store can shrink back under it instead of being locked. A succession link refused for room is the same best-effort loss its callers already log; the account's launch binding still follows the successor.

**Activating refuses an account it cannot launch as (2026-07-14).** `set_active` is the
point of no return — the caller follows it by tearing the live session down and relaunching
as the target — so it is the last place a switch can fail safely. It therefore refuses an
account that is *listed but unlaunchable*: present in the index, but with no resolvable
secret (`AccountError::NoStoredSecret`). That state is reachable, not theoretical —
`SecretStore::set` is infallible **by signature**, so a keystore write that silently dropped
(or a row the OS or user removed) leaves exactly this shape. Activating it anyway strands
the client: the caller tears the session down, relaunch finds no identity and routes into
the onboarding wizard, and the switcher — which lives behind an authenticated session — is
now unreachable, so the healthy account cannot be selected back. That is a half-state a user
cannot escape from their client, which the crash-safe corollary forbids (`nest/common.md`
§ Client-state recoverability). It is enforced in the shared crate rather than in each
app's switch handler because four platforms each remembering the same `if` is four
chances to forget it. Unit-proven:
`activating_an_account_whose_secret_is_gone_is_refused_and_leaves_the_live_one_active`.
**The refusal is painted, never swallowed (2026-09-22):** the seat shows the shared line
`fauna_client_accounts::switch_refused_copy(err, label)` on the page's `error-message` —
it names the target, says the user is still on the identity they were using, and for
`NoStoredSecret` names the way back (add the identity again with its secret key or recovery
kit). The UniFFI seats receive it ready-made: `FfiAccountRegistry::set_active` /
`set_active_confirmed` reject with `FfiError::General` whose message IS that line, so a seat
paints the error's message field (windows: `Strings.Error`, never the exception's aggregated
text). tui, linux, web, macOS, iOS and windows paint it, each e2e-proven by its
`test_<app>_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused`; android is
unchecked as of this writing.

**Cross-process mutation lock (2026-07-22).** Concurrent instances
(`apps/account-scoping.md` § Concurrent instances) make registry mutation
genuinely multi-process: every mutator is a read-modify-write over the single
`fauna/index` blob, and no platform secret store offers cross-process
transactions — so two instances mutating concurrently could lose an update
(one index rewrite swallowing the other's) or interleave an erase with a
migration. Every registry **mutator** therefore serializes under an advisory,
**install-scoped** OS file lock (`<state base>/account-registry.lock`, std
`File::lock` — `flock`/`LockFileEx`, the same kernel-arbitrated mechanism as
the sync agent's instance lock; no third-party dependency), acquired exactly
once at mutator entry when the platform constructs the registry with one
(`AccountRegistry::with_mutation_lock`; FFI
`FfiAccountRegistry::new_with_lock_dir`). Advisory is sufficient because every
mutation path on every platform flows through the shared crate **and every
adapter over it inherits the registry it was minted from** — the
`LaunchPersistence` adapter is built by `AccountRegistry::launch_persistence()`
/ `bound_launch_persistence()` and has no store-taking constructor, precisely so
it cannot invent a second, unlocked registry behind a platform's back. That
matters because the adapter owns `save_authenticated`, itself a full index
read-modify-write on the busiest write path there is: a store-taking
constructor would have left every platform's *launch* writer unserialized while
every other mutator locked, invisibly. So one construction site per app
decides locking for all of its writers, and there is no second writer
implementation to forget it. Three deliberate edges: **reads and
the `bind_account` spawn gate never lock** ("a read never writes" extends to
"a read never locks", so launch stays wait-free against a wedged holder — and
the OS releases the lock if its holder dies, so a crash cannot orphan it); **a
lock-file I/O failure degrades to unserialized mutation** (pre-lock behavior)
rather than a broken sign-out or switch — the lock narrows a race, never
widens a failure; and the lock is **per-mutation, not per-journey** (a
switch is one `set_active` acquisition; it was two while the deleted mirror
followed it). **Web's leg (BUILT 2026-09-27).** Tabs are web's concurrent instances and share one
`localStorage`, so the same lost-update race is real there — but a Web Lock is
asynchronous and the shared mutators are not, so the lock cannot live inside
`AccountRegistry` (web's registry carries `NoopMutationLock`). It lives one
layer out, still in the shared crate: `fauna_client_accounts::with_web_mutation_lock`
(`web_mutation_lock.rs`, wasm32) takes the origin-wide exclusive Web Lock
`fauna.accounts.migrate` around one synchronous mutator, and every wasm-facing
mutator entry runs inside it — the `WasmAccountRegistry` methods (the
install-secret mint included), the launch chunk's slot writers and its
`save_authenticated` (which the trait lets write asynchronously), the
succession ceremony's successor persist and the supervision-snapshot write — so
no TS caller can reach an unlocked mutator. Reads never take it. It degrades
open where `navigator.locks` is absent, as the file lock does on I/O failure.
The name is the historical migration-lock name, kept so tabs of an older and a
newer build still exclude each other; the web-side edges and witness are the
concurrent-instances web leg's ([`apps/account-scoping.md`](apps/account-scoping.md)
§ Concurrent instances → *Which critical sections are real here*).
Unit-proven (`mutation_lock_serialization`:
deterministic lost-update pair, exactly-once acquisition per mutator,
read/bind purity, degrade; plus an FFI-seam pin that both launch adapters
inherit their registry's lock). *Status:* shared crate + FFI constructor landed
2026-07-22. **apple constructs with the lock** (`FaunaAccounts.registry()`, over
the install-scoped `~/Library/Application Support/Fauna/` — the same base
`AccountStateDir` scopes accounts under), and **linux is the second**
(2026-07-22, `main.rs::account_registry()` over `<xdg-config>/fauna/`, the base
`account_scope::install_state_base()` also gives the instance lock and the
per-actor scope dirs); the other four apps (web, windows, android, tui)
still construct unlocked, and their adoption rides each platform's (OS
login, account) single-instance re-keying leg.

⚠ **"One construction site per app" is the design, not a fact about any
app's code — census before you flip.** This paragraph used to tell the next
adopter that each app "has exactly one registry construction site to flip";
linux had **nineteen**, behind a choke-point function whose own rustdoc called
itself "THE one place". Flipping that function alone would have left every
other writer — the account switcher, the wizard's post-onboarding saves, the
factory-reset code mint, remove-account, sign-out — silently carrying the no-op
lock while the rest serialized. Nothing observable fails in that state, which
is what makes it worth a census rather than a reading: the bypass *is* the
absence of behavior. So each adopting app sweeps its own constructions
through its choke point **in the same change** as the flip, and pins the result
against regrowth (linux's pin is a source-level test,
`account_registry_census_test`, which names the offending file and line; apple
needed no sweep — `FaunaAccounts.registry()` is verifiably its only
`FfiAccountRegistry` builder). Provenance: the fan-out was caught by a security
review of the adoption, not by the adoption itself — the choke point read as
single to everyone who looked at it, including the doc you are reading.

**The MLS engine is identity-scoped, so a switch must end it (2026-07-14).** A switching app
outlives any one identity, so whatever object holds an identity's MLS state must be ended — not
reused — when the account changes. Reusing it leaves the incoming account encrypting and
decrypting its conversations through the **signed-out** identity's engine: a silent
cross-identity bug.

**There is no longer a process-wide MLS singleton to get this wrong (2026-07-22).** This rule was
originally written against the UniFFI `libs/fauna-ffi/src/mls.rs` plane — a `Mutex<Option<..>>`
`ENGINE` static reached through `mls_init_engine` / `mls_shutdown_engine`, whose set-once
predecessor caused exactly the bug above. That whole plane is **deleted**: its last production
caller (apple's `MlsManager`) went away 2026-07-19 and the caller-less remainder followed. Every
app now builds a **per-session** `MlsEngine` through the conversations rail
(`build_conversations_session`, `libs/fauna-ffi/src/nest_client.rs`), which gives the engine an
identity lifetime *by construction* rather than by remembering to call a shutdown function.
Mechanism owner: [`../behavior/devices.md`](../behavior/devices.md) § Cross-device MLS
group-state sync. Nothing may re-derive a standalone-engine path.

**The principle is app-shaped, not seam-shaped: whatever object carries an identity's MLS
state, a switch must end it.** Windows' carrier is `ConversationsManagerHost.Instance` — a
*process-wide* manager holding that identity's MLS/SMTP rails, observers and thread store — so
every actor-change teardown replaces the host (`ConversationsManagerHost.ResetForActorChange()`,
reached from the one canonical `App.DropActorScopedState()`).
Reusing it would have the incoming account encrypting and decrypting through the signed-out
identity's rails: the same silent cross-identity bug as apple's, through a different door. Note
the shared `clear_for_test()` is **not** that seam — it explicitly preserves registered backends
and observers, which are the identity-bound parts. Every other app already gives the manager
a per-identity lifetime, so this brings windows into line rather than inventing a shape.

**Eager vs. lazy migration at native boot — CONVERGED on lazy (windows, 2026-07-19); history since 2026-09-28, when the migration itself was deleted. The
no-ghost-row property it served is still owned here.** Linux, apple and windows all leave boot migration **lazy**: the
mutators run `migrate_legacy()` themselves, and every *read* already resolves through
the in-memory legacy-derived index, so a pre-registry install routes, authenticates and
renders its single switcher row without an index ever being written; materialization
happens on the first write that needs it (`save_authenticated` → `update_cache`, or the
first `add_account`). Windows previously called `FfiAccountRegistry::ensure_migrated()`
eagerly at boot (`App.xaml.cs`), persisting the index up front — as web did until its
`ensureMigrated` export was retired on 2026-09-24 (web never migrates now: its registry
never sets the legacy flag, `libs/fauna-wasm/src/accounts.rs`) — until the reasoning below
drove the drop.

Lazy is correct: eager migration mints a real index entry for a **mid-onboarding
identity the user abandoned** — the wizard writes the legacy secret slot at
identity-create, *before* the identity is ever registered — so if the user quits and
later onboards a *different* identity, the abandoned one persists as a ghost row in the
account switcher (handle-less, nest-less, and activatable). Eager migration's original justification — "the
boot re-mirror is index-gated, so the index must exist first" — no longer holds either:
the re-mirror now gates *itself* (§ above), and on an un-migrated install there is
nothing for it to heal anyway (an append is reachable only from an authenticated
session, which has already written the index). Windows' own boot-registry comment block
(`App.xaml.cs`, "Registry boot: seed CLI overrides → mirror") records this reasoning at
the call site; `test_windows_abandoned_create_identity_does_not_ghost_the_switcher`
pins the no-ghost-row property.

**What keeps the ghost away is no longer laziness — it is a retraction at the next
commit (2026-08-31).** This paragraph used to finish "the lazy path cannot produce it:
no mutator runs until an authentication succeeds, and by then the legacy slot holds the
identity that actually signed in." **That guarantee is gone, and had been for two weeks
before anyone noticed.** Moment 1 (§ Multi-account evolution) made
`persist_confirmed_identity` write and *activate* a real registry row the instant the
user clicks Continue — deliberately, because the alternative is losing a freshly
generated secret that exists nowhere else — so a mutator now runs long before any
authentication, on all seven apps as of 2026-08-15. Dropping eager migration had removed
one producer of the ghost; moment 1 quietly added a better one, and nothing on the
abandon path retracted it: `OnboardingMachine::back` only rewrites the wizard's
in-memory `step`. Measured on windows 2026-08-31 (two switcher rows where the test
demands one), and reproduced on tui the same day — it was never windows-specific, the
registration is shared Rust.

The property still holds, by a different mechanism: **moment 1 retires every *other*
provisional row as it commits** (`AccountRegistry::retire_superseded_provisionals`,
called from `persist_confirmed_identity` after its read-back). The reasoning:

- **The retraction cannot live on the abandon path.** The ways away from a
  half-onboarded identity are unbounded — Back, quitting, a crash, a kill, closing to
  tray and never returning — so a cleanup hung on any one of them is a cleanup the
  other doors walk past. Only the *next commit* sees them all.
- **Confirming a different identity is the retraction signal.** Append mode never
  reaches moment 1 (§ Multi-account evolution, the append carve-out), so a caller is a
  *first-run* wizard, which has exactly one identity in flight. A second secret-only row
  can only be an earlier run of that same wizard, and it is dead by construction: it
  authenticated to no nest, so it owns no data anywhere, and nothing can reach it again
  once a different identity is active.
- **"Provisional" is deliberately paranoid, because the retirement deletes.** A row
  qualifies only when it is a secret and *nothing else*: no other slot on
  `PER_ACTOR_KEY_BUILDERS` (derived, not hand-enumerated — apps row 492, after a
  hand-list of the seven slots current when this was first written silently fell
  behind four new builders); no cached handle/domain/tier; neither re-auth flag; in
  no succession chain from either end; and no unknown `extra` keys, since a row a
  newer build wrote fields into is a row this build cannot judge. A false negative
  leaves a ghost the user can remove by hand; a false positive destroys key
  material — so every doubt resolves to *keep*.
- **The durability moment 1 exists for is untouched.** The abandoned secret stays
  readable for the whole time the user is inside the wizard, across force-quit and
  crash (case 2 of § Three-case launch routing resumes it); it is retired only at the
  moment that same user confirms a different identity in its place.

`test_windows_abandoned_create_identity_does_not_ghost_the_switcher` still pins the
property end-to-end; its shared-Rust root is pinned by
`superseding_an_abandoned_identity_leaves_no_ghost_row` and the retirement's blast-radius
guards (`launch_persistence.rs`), and at the app layer on the lead app by tui's
`abandoning_a_created_identity_leaves_no_second_switcher_row` — which red-verifies
against the reverted fix. **The general lesson, which is why this is written out rather
than patched away: a ruling whose stated mechanism is a property of *another* section's
design outlives that design silently.** Moment 1 was ratified on its own merits by a
session that had no reason to re-read a migration ruling three hundred lines up.

**A second, narrower migration gap — not eager-vs-lazy, a genuinely orphaned per-actor
slot — was found and fixed on windows 2026-08-15 (history:
the migration it rode is deleted).** The
import flow's early secret persist (`SecretStore.SaveSecret`, before the nest is known)
can be enough by itself to trip a *later* mutator's `migrate_legacy_locked()` — e.g. the
pending-invite-slot clear that runs on every `LoggedIn` hand-off — which migrates the
identity into the registry **before its nest_url is known**. Once migrated,
`migrate_legacy()`/`EnsureMigrated()` is a permanent no-op (`IndexState::Readable` short
-circuits it), so a *subsequent* `SecretStore.SaveNestUrl(...)` call — which only ever
wrote the **legacy** key — never reaches the per-actor `nest_url` slot
`RegistryLaunchPersistence::load_nest_url()` reads (that read has no CR-3 legacy
fallback, unlike `load_pending_invite`). The orphaned slot then permanently blocks
`LaunchMachine::start()`'s `(identity, nest_url, pending)` routing from ever attempting
`SilentChallenge` — the one path whose `save_authenticated` would otherwise self-heal it
— so the app falls to `WizardAt(HandleEntry)` and a just-approved invite-request user is
bounced back to the very first onboarding page with no error. Fixed at windows' post
-wizard hand-off (`App.xaml.cs`'s `onOnboardingCompleted`, the non-append branch): call
`FfiAccountRegistry::AddAccount(secret, nest_url, device_id)` there instead of
`EnsureMigrated()` — idempotent, and (unlike `EnsureMigrated`) it ALWAYS (re)writes the
per-actor `nest_url` for the given secret, migrated-or-not — mirroring linux's own
`LoggedIn` hand-off (`views/onboarding/mod.rs`: `registry.add_account(&secret_hex,
Some(nest_url.as_str()), None)`) and windows' own append-branch a few lines above in the
same function, which already called `AddAccount` correctly. **Any client whose
`LoggedIn`/first-login hand-off writes `nest_url` through a bare legacy-key setter
instead of a registry `add_account`/`set_nest_url` call is exposed to this same
class of bug** if anything on that identity's path can migrate it early (an import flow
is the known trigger; there may be others) — worth an audit on apple/android if either
is ever seen landing a just-approved invite requester back on its own handle-entry page.

**Both pending-resume slots are now machine-carried; neither is per-app glue.**
*(Ratified 2026-07-11, superseding the "two remaining slots stay single-slot,
deferred until Stage 3" position — its premise, that the slots were working
per-app glue worth preserving, proved false on inspection.)*

- **Pending-encryption-mode: retired outright, not merely slot-less (no-modes,
  ratified 2026-07-12).** The routing this bullet described is gone, not just
  the slot: `LaunchWizardEntry` carries no `EncryptionModeChoice` variant, and
  `VerifyReply.storage_mode_pending` was read off the silent challenge but
  never routed on, and left the wire 2026-09-24 — a successful verify lands
  `Online`. See
  `storage-modes.md` § The transition contract rule 4 and `onboarding.md`
  § App-launch routing *Admin-claimed, mode-unresolved*.
- **Awaiting-manual-DNS: a per-actor registry slot, read through
  `LaunchPersistence`.** `fauna/{actor_id}/awaiting_dns` holds the opaque-JSON
  `AwaitingDnsRecord` (`nest_url`, `handle`, `dns_records_json`, `claim_code`) —
  the same opaque-JSON contract as `pending_invite`, and swept with the identity
  by `remove()` / `clear_all()`. `LaunchPersistence::load_awaiting_dns()` is what
  the machine reads to route `WizardAt{AwaitingManualDns}`, **before** the
  silent-challenge row. The nest cannot report this state (while DNS is pending it
  is unreachable by definition), which is why it is a *slot* and not a verify-reply
  field like the mode — but that argues only against nest-authority, not against
  the machine carrying the row.

Why the reversal: the per-app slots this paragraph once protected were **write-
only dead storage** — linux/web/windows/apple each wrote 3 fields, every one of
them **omitting `handle`**, which `seed_awaiting_manual_dns(nest_url, handle,
dns_records, claim_code)` requires; android wrote none; and **no app ever read
its slot back or rendered the surface**. There was no working per-app glue to
preserve, so "keep it per-app" meant "write the same broken glue a seventh
time" (against priorities #1/#2/#4). Being per-actor also makes the
abandoned-append hazard structural rather than merely inert: the slot is namespaced
to the identity that owns it and dies with it.

**The pre-registry GLOBAL wizard-resume slots bridge across the upgrade (CR-3,
2026-07-13).** Re-keying the three wizard-resume slots (pending-invite /
awaiting-manual-DNS / pending-factory-reset) from the old clients' global
single-slot layout to `fauna/{actor_id}/…` is an **at-rest layout change**, so
the registry carries a legacy bridge — without it, a factory reset dispatched
on the old client version and interrupted by a crash would lose its claim code
on upgrade (CR-1 through the upgrade door;
`architecture/version-compatibility.md` § 2 — additive-only within a major).
**That upgrade job was retired 2026-09-24** by the compat-remnant sweep
([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program
4): no pre-registry install exists, and since the same day a first
account enters the registry through the wizard's own `persist_confirmed_identity`
(§ Downgrade mirror above), so the no-index fallback read and `migrate_legacy()`
served nothing; both were deleted with the transitional
`with_legacy_single_slot()` flag when android, the last native leg, landed
(2026-09-28). The rules below are that bridge, recorded as history.
The bridge is per-FIELD logical keys (`legacy/pending_factory_reset_nest_url`,
`…_handle`, `…_claim_code`, and the pending-invite / awaiting-DNS
equivalents — every pre-registry client stored these slots as separate native
keys), so the platform `SecretStore` stays a trivial key→key map and all
composition lives in `fauna-client-accounts` (`legacy_slots`). Four rules:

- **Fallback read, gated on the index being absent.** When no `fauna/index`
  exists (a pure-read first boot after upgrade), `RegistryLaunchPersistence`
  composes the record from the legacy field keys in memory — a pure read. Once
  an index exists the per-actor slots are authoritative and the legacy keys are
  **never read back** (they are only a downgrade mirror; reading them on an
  indexed install could hand one account another account's slot).
- **Migration materializes.** `migrate_legacy()` copies the global slots into
  account #1's per-actor keys alongside the identity; legacy keys stay.
- **Downgrade mirror, set-or-clear.** The legacy field keys always hold *the
  active account's* slot view: per-actor slot writes/clears for the active
  account re-mirror them, and `mirror_active_to_legacy()` (boot + switch)
  sets-or-clears them with the cache keys — so the single-slot launch routing
  follows the active account and never resumes another identity's claim.
- **Sign-out sweeps them** with the rest of the legacy mirror (`clear_all`).

The Stage-3 capability that *was* deferred is unchanged and still PARKED: resuming
an **inactive** account's incomplete provisioning while another account is active
is **background cross-identity** (design Decision 6). Per-actor slots are a
precondition for it, not a delivery of it — the append rule (an append confirm
writes nothing, § Downgrade mirror + abandoned-append recovery) still means an
abandoned append never shadows the active account.

**Per-account re-auth.** Each account carries a `require_confirm_to_activate`
flag; when set, activating that account requires a re-auth confirmation
(biometric / OS prompt). Default off; an app turns it on for its admin
identity. The registry never learns admin-ness itself — that stays a nest
`am-i-admin` concern. The specifics, ratified 2026-07-16 (user-approved):

- **Enforcement is in the registry, not the app.** `set_active` refuses a
  flagged account with `AccountError::ConfirmationRequired`; the post-re-auth
  path is the separate `set_active_confirmed(actor_id)` (additive — plain
  `set_active` keeps its signature, so unflagged call sites are untouched). The
  registry cannot verify a real re-auth happened — the app asserts it by
  choosing the confirmed call — but routing every activation through the gate
  turns "forgot to prompt" into a hard error, never a silent skip.
  `set_active_confirmed` call sites are the audit surface: each must sit
  adjacent to the platform's re-auth prompt. Guard order: launchability first —
  an account whose secret is gone reads `NoStoredSecret` even when flagged (no
  prompt could fix it).
- **The toggle** is per-account, on the account's switcher row:
  `account-require-confirm-toggle`, scoped within `account-switcher-item`
  (beside `account-remove-button`), shared `elements` — all apps converge
  on it. It renders on **every** row, the active one included: the natural
  target is the user's admin identity, which is usually the account you are
  already on (and the one the auto-default flags). Setting the flag never
  prompts; only *activating* a flagged account does.
- **The confirm surface is the platform's native re-auth prompt** wherever one
  exists — apple `LAContext` (`deviceOwnerAuthentication` — biometric with
  passcode fallback), windows Hello, android `BiometricPrompt`. An OS dialog
  carries no test ID, so those platforms render no ui.yaml element for it.
- **Platforms with no native re-auth affordance (linux, web, tui) render the
  in-app surface instead** — shape ratified by the linux slice 2026-07-16;
  web + tui adopt it (priority #1: one uniform in-app confirm for the
  no-native-prompt platforms). Three elements, mirroring the established
  `file-version-restore-confirm-modal` family: the
  `account-activate-reauth-prompt` view, plus
  `account-activate-reauth-confirm-button` /
  `account-activate-reauth-cancel-button` (IDs user-approved 2026-07-16). It is
  a **confirmation, not a credential check** — the ratified degradation where
  the OS offers nothing better; the gate's value is that activation cannot
  happen without a deliberate second act. The prompt names the account it is
  switching to, and cancel/Escape/close all take the decline path.
  - **The gate resolves before the app-owned switch seam**, so the app's
    mutation-first invariant (registry write before teardown) holds unchanged.
  - **Apps must read the flag fresh at activation and at render.** An app
    that caches it at build time can both skip the prompt on a stale `false`
    (the registry still refuses — fail-closed, but the click reads as dead) and
    render a stale `false` over a flag the auto-default has since set, which
    makes the flag **impossible to turn off**: the user's tap on an
    OFF-looking toggle writes ON. Linux hit exactly this (its Account page is
    built once) and re-reads on page-visible; any app with a build-once
    settings surface needs the same.
- **Declining is a pure no-op**: the app prompts *before* calling
  `set_active_confirmed`, so nothing is mutated and no session is torn down —
  the user stays on the current account, with no error banner (they cancelled
  it themselves).
- **The admin auto-default** ("an app turns it on for its admin identity"),
  ratified + landed 2026-07-16: `AccountEntry` carries an additive
  `require_confirm_user_set` marker — `set_require_confirm` (the toggle) sets
  it, and `auto_enable_require_confirm(actor_id)` flips the flag ON **iff the
  marker is unset** (idempotent; never turns the flag off; does not consume
  the override right). An app calls it on every `am-i-admin = true`
  observation for the active account — the *app* decides when an account
  counts as admin (apple: the nav gate that reveals `admin-tab`); the registry
  still never learns admin-ness. An explicit user OFF therefore sticks
  forever against the auto-ON.
- **The succession link**, landed 2026-08-03: `AccountEntry` carries an
  additive `succeeded_by` — the actor id of the identity that succeeded this
  one, written by `AccountRegistry::record_succession` at the two places a
  client learns it (the ceremony's own `adopt_successor`, and the phrase-only
  restore that writes recovered predecessors back as ordinary rows). It records
  the **immediate** successor and is never rewritten; `predecessors_of(actor)`
  walks the chain and returns *every* ancestor. Its consumer is the
  post-succession corpus re-seal, which opens at-rest blobs under a retired
  identity's `BackupKey` and so must know which rows are predecessors — the
  behaviour is owned by
  [`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md)
  § Re-key scope. The link exists because the alternative answer, "every other
  account", leaked: an unrelated account's key legitimately opened *its own*
  device-local `__config` replica (retired with the rail 2026-10-02 —
  [`config-dissolution.md`](config-dissolution.md) § The `__config`
  dissolution schedule → *The closure order*, step (6)), so a pass fed the
  whole registry would have folded a different account's deployment seeds
  into this one. An index written before
  the field loads with `None`, i.e. no predecessors. **The successor's row
  carries the same link, 2026-09-20:** an additive `succeeded_from` — every
  retired identity this one succeeded from, nearest hop first, ids only —
  because `succeeded_by` lives on a row that is legitimately absent (a device
  that never held it, or the user removing the retired account) and a link
  stored only there dies with it. `record_succession` writes both;
  `record_predecessors` merges a link proven from the landed statement; and
  `predecessors_of` answers the union, the witnessed walk first. Why the
  profile needs it, and what counts as proof:
  [`../ui/profile.md`](../ui/profile.md) § After an identity succession.

## Implementation status today

**The index bound (§ Multi-account evolution → *The index is bounded*) is built (2026-10-05).** Every index write goes through `AccountRegistry::encode_index`, which refuses growth past `MAX_INDEX_VALUE_BYTES` with `AccountError::IndexFull`; the add encodes before its secret slot, and the removals encode before their deletes; `fauna-credential-store` pins the constant to `MAX_ITEM_VALUE_BYTES` at compile time. The five tests in `libs/fauna-client-accounts/src/index_bound_tests.rs` run over a store that drops over-cap writes: the refused add writes nothing, every growing mutator is refused, a full index still switches and removes, an index already past the cap shrinks but never grows, and the measured capacity is pinned. The refusal's painted line is shared copy: `fauna_client_accounts::add_refused_copy` (`add_refusal.rs`) selects the translated "account list is full" line for `IndexFull` and nothing for any other add error, which keeps the seat's own wrapper. **Built on tui (2026-10-05)** — onboarding's sign-in, the add-another-account sign-in and the successor adoption paint it (`session.rs::add_refusal_line`, pinned by `a_sign_in_on_a_full_account_list_paints_the_list_full_line`). Not built on the other six apps, which still show the error's own English message inside their generic "couldn't save your new account" line.

**The legacy single slot is retired on all seven apps (the compat-remnant
sweep's sub-tranche B1.1: web, linux and tui 2026-09-24; windows and apple
2026-09-25; android 2026-09-28, the last native leg).** Shared Rust:
`persist_confirmed_identity(registry, secret, append)` carries the append rule
(`an_append_confirm_writes_nothing_and_the_launch_still_routes_on_the_active_account`),
and `AccountRegistry` touches no `legacy/*` key: the transitional
`with_legacy_single_slot()` flag, the mirror, the migration, the fallbacks and
the FFI's transitional exports were deleted with android's leg. Moved:
tui (`session::confirm_identity_sink` carries the mode), linux
(`client::load_credentials` / the account cache read the registry;
`store_credentials*` and the single-blob migration are gone; the test agent's
session patch is `add_account` + `set_active`), web (`accountsBoot` runs no
migration and no mirror; `store.ts` resolves the identity through
`accountsSessionMaterial`; the pre-derivation push id's adoption + retired-id
drain are gone), the e2e harness (every `CredStore.inject_identity`, the web
session patch and the pending-factory-reset seeders write the registry shape),
and `fauna-credential-store` (no legacy mapping). windows moved 2026-09-25:
its wizard commits through the FFI `ConfirmIdentity(secret, append)` in both
modes and runs `PersistLoggedIn` at its `LoggedIn` terminal; boot, the switch,
the views and the test agent read the served account's `SessionMaterial` (and
the launch persistence's resume rows) through one read-only view
(`RegistrySessionAccount`); the test agent's session patch is `AddAccount` +
`SetActive`; `SecretKeyMap` remaps nothing. apple (macOS + iOS) moved
2026-09-25: `OnboardingVM` commits through the FFI `confirmIdentity(secret,
append)` in both modes, reads every in-run secret off the machine
(`effectiveSecret()`), and records the handle in the registry cache at its
`LoggedIn` terminal; the append terminal (`completeAppendedAccount`) registers
the identity from the wizard machine with a `deviceIdForActor` id; launch, the
wizard seeding, the re-onboard fallbacks, the views and the watch read the served
account's session material (`FaunaAccounts.sessionMaterial`, or
`FaunaClient.sessionMaterial` keyed on the client's own actor); the e2e session
patch is `addAccount` + `setActive` / `setNestUrl` / `updateCache`
(`SessionPatchAccounts`); `KeychainSecretStore` remaps nothing and
`KeychainStore.Key` keeps only its own bookkeeping row. android moved
2026-09-28: its identity screens commit through the FFI `confirmIdentity(secret,
append)` in both modes; `OnboardingHost` reads every in-run secret off the
machine, runs `persistLoggedIn` (and records the handle in the registry cache)
at its `LoggedIn` terminal, and persists its append-mode awaiting-DNS exit
through `persistAwaitingDns` before switching; the append terminal
(`AccountSettingsVM.completeAddAccount`) registers the identity from the wizard
machine at the exit's nest with a `deviceIdFor` id for the appended actor; launch
(`AppLaunchVM`, all three resume rows through the launch persistence), the
views, the workers and the engines read the active account's session material
through one read-only view (`SessionAccount`, implemented by `SecureStorage`);
the test agent's session patch is `addAccount` + `setActive`, and its
`reset` / `logout` arms clear the registry; `LogicalSecretStore` stores every
key verbatim (`SecretKeyMap` is deleted). Known linux
ordering wart: `handle_wizard_done` runs `persist_logged_in` in append mode
too (a page handler with no mode flag), a moment before the append arm's own
`add_account` + switch re-affirms both.

**The per-actor erase had THREE independent holes — all three closed
2026-09-01.** They were in *which keys* it deletes, *which actors* it deletes
them for, and *which store* it deletes them from. Keep them written down
together: each was invisible behind the one below it, and the third was
invisible to every test in the tree.

**Hole 3 — the store (CLOSED), the one that subsumed hole 1 in production.** A
credential key is a *(namespace, key)* pair, and every backend binds the
namespace into the physical row (Keyring: the service name; File: one file per
namespace; Foreign: a `{app}/{account}` prefix). `AccountRegistry` is built over
the **app's own** store (`fauna-desktop` on linux, `fauna-tui` on tui), while
`principal_bundle` and the T10 writer key are written through
`production_credential_store()` = `CredentialStore::new(CRED_NAMESPACE)`,
namespace **`fauna-account-store`**. So in a production build the sweep deleted
`fauna-desktop/{actor}/device-auth` — a row that was never written — while the
real row under `fauna-account-store` survived `remove`, `clear_all`, *and*
`delete_namespace` (both production callers wipe only the app's own namespace).
**No test could witness it**: every e2e driver sets `FAUNA_KEYRING_APP` for run
isolation and *both* constructors honour it, so the two namespaces collapse into
one physical store under test — which is why the e2e below was green and the
unit tests, writing through the registry's own store handle, had only ever
pinned the key *strings*.

**The e2e blind spot itself is now closed too (2026-09-02).**
`fauna_credential_store::apply_namespace_override` derives the account-store's
`FAUNA_KEYRING_APP` override (`{override}-account-store`) instead of reusing the
app's verbatim, so the two namespaces no longer collapse under the harness —
apple needed no such derivation, since neither apple driver sets an override at
all and its two namespaces were already distinct (a separate
`fauna-account-store.json` file on macOS; `fauna-account-store/`-prefixed rows
in the shared `keychain.json` on iOS). `test_sign_out_erases_the_credential_namespace`
and `test_smoke_g_factory_reset_leaves_no_identity_for_the_next_launch`
(`tests/e2e-unified/tests/test_sign_out.py`,
`test_onboarding_launch_routing_smoke.py`) now assert the account-store
namespace is empty too, via `attach_account_store`/`LaunchHarness.account_store`
(`tests/common/cred_store.py`, `launch_harness.py`) — covering linux, tui,
windows, macOS, iOS and — since 2026-10-05 — android, whose account-store rows
ride the foreign seam into its on-device e2e credential file (prefixed
`fauna-account-store/`, as on iOS) and read back over the bridge's
`GET /credentials`; web has no account runtime and therefore no second
namespace to attach to.

**The fix keeps the namespaces split and widens the erase, because the split is
correct.** One store device principal per machine means the app and the
app-dead sync agent beside it must reach the *same* row
([`account-data-plane.md`](account-data-plane.md) § The store device
principal), so merging `fauna-account-store` into each app's namespace would
mint a writer key per app. Instead `AccountRegistry` carries an **erase-only**
list of auxiliary stores that `clear_all` and `remove` sweep alongside the one
they index. The registry cannot build that list itself — a platform implements
only `SecretStore`, one foreign seam and not two — so the list, the namespace
constant, and the constructor that pairs them all live one crate up, in
`fauna-credential-store`, the crate that owns namespaces:
`account_registry{,_with_lock}` is the single place a namespace joins the erase,
and every erase-capable native registry is built through it (`fauna-ffi` for
windows/macOS/iOS/android, linux's own choke point, tui's). Web builds the plain
constructor and is right to: it has no account runtime, so it has no second
namespace. Two details are load-bearing —

- **The aux store resolves its backend on every call, not at construction.** On
  the phones the account-store namespace rides the app's own store over the
  foreign seam, installed process-globally at start-up, and an app may mint its
  switcher registry first; a store built at that moment would snapshot the
  *inert* backend and silently erase nothing for the life of the process.
- **The two freedesktop apps erase by wiping their own namespace**
  (`delete_namespace()`), which by construction cannot reach a second one — and
  `delete_namespace` on the shared namespace would be far too wide, taking every
  app's accounts on the machine, not this app's. So linux and tui now run the
  per-actor sweep *first*, then wipe; both enumerate through the registry index
  the wipe destroys, so the order is part of the fix.

Witnesses: `clear_all_reaches_the_account_store_namespace` and
`remove_reaches_the_account_store_namespace` (`fauna-credential-store`) build
both stores through the **env-routed** constructor production uses and redirect
only the *backend*, so the two namespaces stay two — this was the one shape
that could assert this at all before `apply_namespace_override` closed the
e2e blind spot above; `test_sign_out_erases_the_credential_namespace` and
`test_smoke_g_factory_reset_leaves_no_identity_for_the_next_launch` now assert
it end to end too. `clear_all_still_erases_the_app_namespace` guards the
additive half, and `both_erases_sweep_the_auxiliary_stores`
(`fauna-client-accounts`) pins the registry-side mechanism. linux's
construction census (`account_registry_census_test.rs`) now fails the build on
a bare `AccountRegistry::new` anywhere but the choke point, so the erase's
width — like the mutation lock's — is decided in exactly one file.

**Hole 2 — the enumeration (CLOSED).** `clear_all` read the raw `fauna/index` blob to
learn which per-actor namespaces exist, and an absent blob defaulted to *zero
accounts*. But an absent blob is not "no accounts": it is the ordinary shape of
a client still running on the legacy single slot, where `index()` derives the
account on the fly and — being a pure read — never persists what it derived. On
that path the erase deleted the two routing pointers and swept **no per-actor
slot whatsoever**, leaving every identity-derived secret behind them on disk.
Fixed by unioning the legacy-derived actor into the swept set; the erase still
must not write (`clear_all_never_writes_to_the_store`), so it derives without
persisting rather than migrating first the way `clear_nest_binding` does.
Witness: `clear_all_sweeps_a_legacy_derived_actor_the_index_never_named`.
⚠ The residual limit is unchanged and still real: a **corrupt** index blob
leaves per-actor slots unreachable, because the `SecretStore` seam cannot scan a
namespace. Only the two routing pointers are guaranteed gone there.

**Hole 1 — the key shapes (CLOSED).** § Cleanup contract owes the erase *every* per-actor
slot, but `AccountRegistry`'s single-sourced builder list
(`libs/fauna-client-accounts/src/lib.rs` `PER_ACTOR_KEY_BUILDERS`) knew only the
`fauna/{actor_id}/*` shape it mints itself. `fauna-sync-engine` writes against
the same namespace in two other shapes, and both survived a sign-out: the four
`principal_bundle` slots at `{actor_id}/{suffix}` (`device-auth`, `backup-key`,
`generation-keys`, `grant-registered` — fixed), and the T10 store
**writer signing key** at the bare `{actor_id}` (fixed 2026-09-01). The writer
key is a real Ed25519 secret derived from the identity, so property 3 above puts
it in the erase; erasing it cannot strand a replica dir that outlives it, because
refinement 10's lost-slot heal
([`account-replica-posture.md`](account-replica-posture.md) § The store device principal)
turns an empty slot over a stamped store into a mint-and-fence. The direction of
the crate dependency is why this recurred twice — `fauna-sync-engine` depends on
`fauna-client-accounts`, never the reverse, so its key constants cannot be
imported and must be duplicated literally in the builder list. Witnesses: unit
tests `clear_all_sweeps_the_principal_bundle_attributes` and
`clear_all_and_remove_sweep_the_bare_actor_id_writer_key_slot`, plus the
cross-app e2e `test_sign_out_erases_the_credential_namespace`, which asserts the
whole namespace and is what reproduced both survivals.
⚠ `clear_nest_binding` deliberately stays off that list: walking away from a nest
keeps the replica, whose stamped writer still needs its slot.

**`fauna/{actor_id}/reach_ipv4` (§ Multi-account evolution, ratified 2026-08-29) is PARTIALLY BUILT (2026-08-30)** — the registry slot and the `persist_logged_in` parameter both exist (`libs/fauna-client-accounts/src/lib.rs:338,1097,1107,1118`; `launch_persistence.rs:529`), captured at the `LoggedIn` terminal on tui + web, with linux/android/apple's own call sites explicitly passing `None`/`null`/`nil` pending their leg and windows not yet calling it at all. **The `LaunchPersistence` read landed 2026-08-30** — `load_reach_ipv4` / `delete_reach_ipv4`, answered from this slot by `RegistryLaunchPersistence`, so tui and linux consume the hint with no per-app code; the dial rule that reads them is `LaunchMachine::run_silent_challenge_phase`. **Still NOT built: the per-app capture legs** (linux, android, apple pass `None` explicitly; windows has no call site), so on those apps the slot stays empty and the read finds nothing. Semantics and capture: [`../behavior/onboarding.md`](../behavior/onboarding.md) § Reach hint / § Implementation status today.

The three-slot contract + server-data cache + save-failure convention are
**fully implemented on all seven apps** (§ Per-app implementations).
Multi-account state, per surface (last verified 2026-07-20):

| Surface | shared Rust | linux | web | windows | apple | android | tui |
|---|---|---|---|---|---|---|---|
| `AccountRegistry` + additive legacy migration (Stage 0) | ✅ 2026-07-01 (`libs/fauna-client-accounts`, unit-tested); legacy **pending-slot** bridge (CR-3) ✅ 2026-07-13 | consumes | consumes | consumes | ✅ consumes (2026-07-14) | ✖ | consumes (direct Rust, shares linux's `CredentialStore`) |
| UniFFI seam (`FfiSecretStore` callback + `FfiAccountRegistry` + `launch_persistence()` handle) | ✅ 2026-07-13 (`libs/fauna-ffi/src/accounts_registry.rs`, behind default-on `accounts-registry`; Go build drops it) | n/a (direct Rust) | n/a (wasm) | ✅ 2026-07-14 — the seam works end-to-end in **C#** (see gotcha below) | ✅ 2026-07-14 — the seam works end-to-end in **Swift** | ✅ 2026-07-18 — the seam works end-to-end in **Kotlin** (`LogicalSecretStore` over `EncryptedSharedPreferences`; CR-3 upgrade case proven through the real `LaunchMachine`) | n/a (direct Rust, like linux) |
| `SecretStore` platform impl | trait | ✅ `CredentialStore` (`libs/fauna-credential-store`, shared with tui) | ✅ `LocalStorageSecretStore` (`libs/fauna-wasm/src/accounts.rs`) | ✅ 2026-07-14 `LogicalSecretStore` + `SecretKeyMap` (`FaunaApp.Core/Services/`), two backends: Credential Manager \| file (`FAUNA_E2E_CREDENTIAL_DIR`) | ✅ 2026-07-14 `KeychainSecretStore` (FaunaKit; 17 `legacy/*` keys → the native Keychain rows, `fauna/*` verbatim; unit-tested incl. the CR-3 upgrade case). E2e-seedable: `KeychainStore` already reads a pre-seeded `{FAUNA_E2E_CREDENTIAL_DIR}/keychain.json` and flushes every mutation back to it, so a test seeds a ≥2-account registry with **no app code** (`drivers/macos.py` `seed_credentials`; apple's native legacy names are byte-identical to `LINUX_LEGACY_KEYS`, so the default `legacy_keys` applies) | ✅ 2026-07-18 `LogicalSecretStore` + `SecretKeyMap` over the shared `fauna_secure_prefs` (17 `legacy/*` keys → the native `secret_key`/`node_url`/slot rows, `fauna/*` verbatim; golden-tested + CR-3 upgrade case). File-backend e2e seeding is leg B (host-gated) | ✅ `CredentialStore` (`libs/fauna-credential-store`, shared with linux — see linux cell) |
| Launch wiring | ✅ `RegistryLaunchPersistence` 2026-07-02 (+ real-`LaunchMachine` routing test) | ✅ all 3 construction sites | ✅ 2026-07-13 (CR-3) — `fauna-wasm-launch` builds the machine over the registry store Rust-side; the TS slot stores wrap the `registry*` accessors | ✅ 2026-07-14 — all 3 construction sites on `FfiAccountRegistry.launch_persistence()`; `SecretStoreLaunchPersistence` **deleted** (no second persistence path) | ✅ 2026-07-14 — all 3 sites via `FaunaAccounts`; the hand-rolled `KeychainLaunchPersistence` is **deleted** (one persistence path, per CR-3) | ✅ 2026-07-18 — `LaunchModule` provides `FfiAccountRegistry.launch_persistence()`; `LaunchPersistenceImpl` **deleted** (one path); lazy boot (no `ensure_migrated()`) + boot `mirror_active_to_legacy()`. `AdminNestVM`'s factory-reset mint rides the same registry persistence | ✅ foundational — `RegistryLaunchPersistence` folded into `launch::start()`'s single re-entrant seam (boot + post-unlock + post-switch + the 2026-07-19 boot re-mirror all ride this one call site) |
| Account switcher UI + append-mode "Add account" | ui.yaml IDs landed | ✅ 2026-07-02 (`settings/account.rs`; tier_3 `test_account_switcher_linux.py`) | ✅ 2026-07-02 (Account sub-page; tier_3 `test_account_switcher_web.py`) | ✅ 2026-07-19 — `AccountSwitcherViewModel` (over the uniffi `IFfiAccountRegistry`, faked in 12 unit tests) + the inline section on `SettingsAccountPage`; row title from the SHARED `AccountDisplayLabel` (windows was the last app not consuming it). Switch = `SetActive` → **read the TARGET's `SessionMaterial` by its actor id** (not the session account, which still names the outgoing identity until the instance lock swaps; until 2026-09-25 this was a re-read of the legacy mirror) → teardown → fresh `LaunchMachine` → `DispatchLaunchSnapshotAsync`, with an `Interlocked` re-entrancy guard; append = the wizard over the live session → `AddAccount` → the same switch path. Teardown additionally **replaces `ConversationsManagerHost`** — windows' identity-scoped MLS carrier, not `mls_shutdown_engine()`. Boot `EnsureMigrated()` **dropped** (lazy convergence + the ghost row, § Eager vs. lazy). tier_3 `test_account_switcher_windows.py` **3/3 green** (list+switch+reveal-admin, append, remove-live — the twin of the linux module; the switch journey asserts the live reconnect through session `actor_id`, not just `admin-tab` visibility). Gotcha the first run surfaced: the switcher `ItemsControl` needed `AutomationProperties.Name` or FlaUI reported it `count=0`/absent — the same rule as DataTemplate roots and standalone container Borders | ✅ 2026-07-15 — shared FaunaKit `AccountSwitcherSection` (both apple apps); switch = `setActive` → teardown → `runLaunch()`; append = the wizard in a sheet over the live session → `addAccount` → the same switch path. tier_3 `test_account_switcher_apple.py` (the generalized twin of the old macOS-only file) **3/3 on BOTH macos and ios** (`drivers/ios.py` gained `SIMCTL_CHILD_FAUNA_E2E_CREDENTIAL_DIR` seeding, mirroring `drivers/macos.py`). The iOS leg surfaced a real bug — see the `FaunaApp.runLaunch()` ordering gotcha below — now fixed | ✅ 2026-07-18 — stateless `AccountSwitcherSection.kt` (Robolectric-tested, 4/4 green, FFI-free); switch = `registry.setActive` → `mirrorActiveToLegacy` → `api.clearAuth()` → `appState.isOnboarding = true` re-enters launch routing (no relaunch — `AppLaunchVM.connectActiveSession()` reconnects as the new account); append = `OnboardingHost.machine.reset()` + a fresh wizard NavHost overlay (`appState.isAddingAccount`) → `addAccount` → the same switch path. Also fixed a standing regression: `ApiClient.authenticate()` had no production caller since the 2026-05-04 `LaunchMachine` rewrite (nestClient stayed permanently null); `connectActiveSession()` is the fix, wired at `NavTarget.Authenticated`. `:app:testDebugUnitTest` 620/620 green. Cross-app e2e stays host-emulator-gated (no android e2e has run yet) | ✅ 2026-07-19 (M8) — `settings/account.rs::account_switcher_elements`, refreshed at the nav edge (not per-frame); append = `account-add-button` wizard-over-live-session slice, same day (`session::adopt_appended`). tier_3 `test_account_switcher_tui.py` 4/4 |
| Abandoned-append recovery (boot re-mirror — retired 2026-09-24/28, replaced by the append rule, § Downgrade mirror + abandoned-append recovery) | ✅ shared `mirror_active_to_legacy()` 2026-07-05 (lifted from web's wasm impl); **self-gating on a materialized index 2026-07-14** (§ The mirror only ever runs from a materialized index) | ✅ durable heal + relaunch e2e | ✅ durable + **current-load** heal | ✅ boot re-mirror wired 2026-07-14 (the append it heals arrives with the switcher UI) | ✅ boot re-mirror 2026-07-14 (`FaunaAccounts.bootLaunchPersistence`); append wizard itself is leg B | ✅ 2026-07-18 boot `mirror_active_to_legacy()` in `LaunchModule` (self-gating; the append it heals arrives with the switcher, item 4) | ✅ 2026-07-19 — single call at the top of `launch::start()` covers every entry point (boot, post-unlock, post-factory-reset, post-switch) in one seam, unlike the separate boot+switch call sites elsewhere |
| Sign-out clears the whole credential namespace (§ Cleanup contract) | ✅ `AccountRegistry::clear_all()` (delete-only, unit-tested) + `CredentialStore::delete_namespace()` (freedesktop sweep, searches until empty) — combined via the shared `fauna_credential_store::erase_all_credentials()`, shared with tui | ✅ `delete_credentials()` → `erase_all_credentials()` (sweep → wipe → read-back; file backend fixed); tier_3 e2e ✅ 2026-08-11 (the cross-app erase test below) | ✅ `logout()` → `accountsClearAll()`; tier_3 e2e `test_sign_out_web.py` (UI-driven, red-green falsified) + the cross-app erase test below. `doDeleteAccount` (`+page.svelte`) no longer calls `identity.logout()`/navigates to onboarding — the 2026-08-26 ruling's bug ([`../ui/settings.md`](../ui/settings.md) § Where logic lives → *Account deletion*) is fixed: deletion only schedules a cancellable pending action and clears nothing at request time; the erase happens on the user's later explicit sign-out | ✅ 2026-07-14 — sign-out + the e2e `reset`/`logout` arms all route to `ClearAll()` (`App.ClearCredentialNamespace`); tier_3 `test_sign_out.py[windows]` — `test_sign_out_erases_the_credential_namespace[windows]` ✅ 2026-08-12 (the cross-app erase test below; 2/2 green) | ✅ 2026-07-14 — `StatusVM.signOut` → `clearAll()`; `KeychainStore.deleteAll()` now sweeps the **service**, not `Key.allCases` (an enum cannot name a registry row). `AccountSettingsVM.deleteAccount` no longer calls `clearAll()` — the 2026-08-26 ruling's bug ([`../ui/settings.md`](../ui/settings.md) § Where logic lives → *Account deletion*) is fixed: deletion only schedules a cancellable pending action and clears nothing at request time; the erase happens on the user's later explicit sign-out | ✅ 2026-07-18 — `AccountSettingsVM.signOut` → `registry.clearAll()` alongside the existing `secureStorage.clear()` + `api.clearAuth()` (closes a previously deferred item). `deleteAccount`'s same-erase bug (same 2026-08-26 ruling) is fixed: it only calls the delete API and shows a transient "scheduled" receipt now, with no registry/secure-storage/actor-scope clear at request time | ✅ `SignOutConfirm` → `App::reset()` → `erase_all_credentials()` (`clear_all()` sweep → `delete_namespace()` wipe → `reverify()` read-back; explicitly modeled on windows/android/linux per the function's own doc comment) |
| Quiesce-before-wipe (§ Cleanup contract) | ✅ a read never writes — `migrate_legacy()` is explicit, `index()`/`secrets()` are pure (unit-tested: `a_read_never_writes_to_the_store`, `save_authenticated_writes_nothing_once_the_account_is_gone`) | ✅ both sign-out paths erase last (`main.rs` — the settings handler + the agent `reset`/`logout` arm) | ✅ `clear_all()` reads the index blob directly; since 2026-10-01 `logout()` awaits the account runtime's sign-out stop, under the stop budget, before the wipe ([`apps/account-scoping.md`](apps/account-scoping.md) § Implementation status today, the 2026-10-01 web paragraph) | ✅ inherited (shared `clear_all()` is the only erase path) | ✅ inherited — apple holds no persistence logic of its own to order (its only seam is the k/v store) | inherits with Stage 1 | ✅ `reset()`'s explicit teardown-then-wipe ordering (every writer dropped before `delete_namespace()`, documented as load-bearing against a concurrent silent-challenge write) |
| Re-auth-on-activate (Stage 2) | ✅ 2026-07-16 — `set_active` refuses `ConfirmationRequired` on a flagged account; `set_active_confirmed` is the post-re-auth path (FFI + wasm exposed; unit-tested). Admin auto-default: `require_confirm_user_set` marker + `auto_enable_require_confirm` (same day; unit-tested, an explicit user OFF sticks) | ✅ 2026-07-16 — **the in-app confirm reference** (`account-activate-reauth-prompt` + confirm/cancel; `settings/account.rs::request_switch_account`), the shape web + tui adopt. Toggle on every row incl. the active one; gate resolves before the switch seam and reads the flag fresh; decline = pure no-op; admin auto-default hooked at the Admin-sidebar nav gate (`app.rs`). Its build-once Account page re-reads the flag on page-visible — without that the auto-defaulted flag renders stale-OFF and can never be turned off. tier_3: decline-then-approve + auto-default journeys green on `--client linux`, driving the real prompt (no file seam) | ✅ 2026-07-16 — adopts the linux in-app shape: `account-require-confirm-toggle` on every row (checkbox with `data-state`); both activation paths route through the gate (`requestSwitchAccount`, settings `+page.svelte`) — flag read fresh from the registry, flagged → the in-app `account-activate-reauth-prompt`, confirm → `accountsSwitchConfirmed` (the only confirmed call site), cancel/backdrop/Escape one pure-no-op decline; admin auto-default at the `+layout.svelte` nav-gate probe, keyed to the OBSERVED identity's actor id (never the registry's active pointer — a mid-append transient identity can't flag the wrong account), re-fired by every load incl. the post-switch reload. tier_3: decline-then-approve + auto-default journeys green on `--client web`, driving the real prompt (no file seam) | ✅ 2026-07-20 — `account-require-confirm-toggle` on every switcher row incl. the active one (`SettingsAccountPage.xaml`, render-echo-guarded); native **Windows Hello** confirm (`Services/AccountReauth.cs` `UserConsentVerifier`, fail-closed; e2e seam `{FAUNA_E2E_CREDENTIAL_DIR}/reauth-result`). The switcher VM reads the flag FRESH from the registry and the app branches `SetActive`/`SetActiveConfirmed` (the `ConfirmationRequired` refusal is the registry backstop); the Hello gate is injected into the VM as a `ConfirmReauth` seam because WinRT is unreachable from `FaunaApp.Core`'s plain-`net10.0` TFM (16 VM unit tests: decline/approve/fresh-read/fail-closed/no-prompt-when-unflagged); decline = pure no-op; admin auto-default at `MainPage.CheckAdminStatusAsync` (`App.AutoEnableRequireConfirmForActiveAdmin`). tier_3 `test_account_switcher_windows.py` decline-then-approve + auto-default journeys green on `--client windows` (5/5 with the Slice-2 trio) | ✅ 2026-07-16 — `account-require-confirm-toggle` on every switcher row (shared FaunaKit); native `LAContext` confirm (`AccountReauth`, fail-closed; e2e seam `{FAUNA_E2E_CREDENTIAL_DIR}/reauth-result`); decline = pure no-op; admin auto-default hooked at the **app-root** nav-gate `am-i-admin` probe on both apple apps (iOS's moved out of `SettingsView` 2026-08-02: probing from inside Settings meant the auto-default's registry write could land while the Account page was already rendering the pre-write row, and it left `isAdmin` unresolved for any user who never opened Settings). **The read-fresh-at-render rule above is satisfied since 2026-08-02** by `.faunaAccountRegistryChanged` — the auto-default posts it on an actual write and `AccountSwitcherSection` reloads (apple's peer of linux's page-visible re-read); before that apple was the build-once surface the rule warns about, and on iOS the auto-defaulted flag rendered stale-OFF with the user's tap swallowed. tier_3: decline-then-approve + auto-default journeys green on macos + ios | ✅ 2026-07-18 — `account-require-confirm-toggle` on every switcher row (`AccountSwitcherSection`, incl. active); native **`BiometricPrompt`** confirm (`core/AccountReauth`, `BIOMETRIC_STRONG\|DEVICE_CREDENTIAL`, fail-closed; e2e seam `reauth-result` beside the e2e credential file in the app's filesDir — android's `{FAUNA_E2E_CREDENTIAL_DIR}`, derived from the `FAUNA_E2E_CREDENTIAL_FILE` launch extra since an intent-launched app inherits no env, written by the bridge's `POST /reauth-result`); the switch handler pre-reads the flag fresh and branches `setActive`/`setActiveConfirmed` (the `ConfirmationRequired` string-error is the registry backstop); decline = pure no-op, anchored for e2e by the `session_generation` / `activation_gestures` counters (`ActorScope.dropActorScopedState` / `AccountReauth.activationGesture`); admin auto-default at the `SettingsAdminGateVM` `am-i-admin` nav-gate. Robolectric toggle-render + `e2eVerdict` / seam-dir / counter unit tests green (host `.so` bindgen); the tier_3 decline/approve + auto-default journeys are wired for `--app android` (`test_account_switcher_apple.py`'s `STAGE2_APPS`, the registry read back over the bridge's `GET /credentials`) but not yet run — android e2e has never run, and its run venue is the A2 harness (`testing.md` § Default app and nest mode → *Android's run venue*) | ✅ 2026-07-19 (ratified 2026-07-16; tui counted as an adopter) — `account-require-confirm-toggle` on every row; `set_active_account` branches `set_active_confirmed`/`set_active` on the flag read fresh; admin auto-default `auto_enable_admin_confirm` keyed to the session actor, not `registry.active()`; tier_3 decline/approve + auto-default journeys green on `--client tui` |

Gotchas that survive the compression:

- **An admin's require-confirm auto-default could destroy that identity's only
  stored secret, and a succession is where it showed (fixed 2026-08-25).** The
  admin auto-default fires at every `am-i-admin = true` observation on all seven
  apps, and an admin is exactly who runs an identity succession. On a store still
  in the pre-registry shape it was the *first* write to persist an index — from
  the legacy-derived shape, without migrating — so the row existed with no
  per-actor slots. `add_account` for the successor then migrated nothing (an index
  now exists), and activating the successor re-mirrored over the legacy slot: the
  predecessor's seed was gone from the very device that had just succeeded from
  it. **The failure is silent by construction** — `predecessors_of` still answers
  from the index, so the post-succession aftermath cleared its `NotASuccessor`
  gate and only then found no material, reporting `NoKeyOpensIt` (`offered=0`)
  forever and leaving every config-bound leg `ConfigStillOwed`. Measured on macOS
  2026-08-25 through `test_the_successor_is_asked_about_the_trust_it_inherited`. Both halves of the fix are in shared Rust — the chokepoint (§ A read
  never writes) and the mirror's rescue (§ Downgrade mirror) — so no app changed.
  The lesson generalizes past this bug: **a mutator that neither adds nor
  activates still persists an index**, and every such write is a chance to name an
  actor whose material was never written down.

- **Moment 1 is conformant on all seven apps only as of 2026-08-15 — tui was the
  outlier for five weeks, and the sentence above ("fully implemented on all seven
  apps") was over-claiming until then.** tui's first-run wizard discarded the
  confirm-identity return and wrote all three slots together at
  `wizard_outcome() == LoggedIn`, so **case 2 of § Three-case launch routing was
  unreachable on tui** and a force-quit anywhere between confirm-identity and
  completed-login destroyed a freshly generated secret that existed nowhere else.
  It dated to tui's first onboarding milestone and survived its parity milestone
  because `apps/tui.md` § Append-mode "Add account" independently praised the
  deferral — as a *stronger* property than linux's legacy-slot heal — which is
  true of the **append** path and was wrongly generalized to the first-run wizard
  (corrected there the same day). Two things make the confusion worth recording:
  the generalization was already false of tui's own code by 2026-08-11 (the
  per-actor pending-invite and awaiting-DNS resume slots had both been committing
  the secret mid-wizard through `add_account(secret, None, None)`), and the
  deferral reads like *stricter* store hygiene while actually being the one write
  whose absence loses an account.
  **The write now has a shared home:** `fauna_client_accounts::persist_confirmed_identity`
  (`launch_persistence.rs`, the moment-1 sibling of the two resume-slot helpers),
  which also **reads the secret back** — `SecretStore::set` is infallible by
  signature, so a keystore that silently kept nothing would otherwise return
  success at the one point where that loses the account. **All seven apps
  consume it as of 2026-08-15**: tui and linux directly; web
  through the wasm `persistConfirmedIdentityMirrored` (its synchronous
  `persistSecret` legacy write survives as an in-run immediacy shim beside the
  authoritative async registry commit — the mirror rewrites the same key, so
  the two cannot disagree) **except in append mode, which is exempt — see the
  append carve-out below**; android/apple/windows through the UniFFI
  exports (since their 2026-09-25/28 legs the one `confirm_identity(secret,
  append)`; the transitional `persist_confirmed_identity(_mirrored)` forms are
  deleted) (the apple/windows call-site
  swaps landed compile-verified).
  The linux
  wiring is e2e-pinned: `test_confirm_identity_commits_through_the_shared_registry`
  asserts `fauna/index` exists after confirm-identity and BEFORE complete-login
  (the one window that separates the shared commit from the legacy-only
  partial), mutation-graded — reverting linux to `store_credentials_partial`
  reds exactly it, closing the call-site blind spot the row named.
  *History:* an app that read the identity back through the retired single
  slot within the same run took a `_mirrored` variant
  (`persist_confirmed_identity_mirrored`), which refreshed the downgrade mirror
  **after** the read-back so it could not mask a dropped write; linux, then
  android, were its callers, and it was deleted 2026-09-28 with the last of
  them.
  **Moment 1 also retracts the previous run's abandoned identity** — writing a
  real, activated row at Continue is what made the abandoned-wizard ghost row
  possible, so the same helper sweeps it; the ruling, its paranoid "provisional"
  test and the reasoning for putting the retraction here live in § Eager vs.
  lazy migration at native boot, which owns the no-ghost-row property.
  **Append ("Add account") mode is outside moment 1 and must stay that way.**
  tui exempts it via `session::confirm_identity_sink`; linux passes `append`
  to the shared `persist_confirmed_identity`, which writes nothing in that
  mode — the appended identity stays in the wizard machine until the append
  terminal registers it and switches (its `LoggedIn` terminal skips moment 4 in
  append mode too, `views/onboarding/mod.rs::persist_logged_in_terminal`).
  web exempts it via `setAppendMode()` (`lib/onboarding/machine.svelte.ts`),
  which the onboarding page declares on EVERY mount from the `?add=1` entry it
  actually took — a one-sided setter would leave the module-scoped flag stuck
  after an append and exempt the *next* wizard, which needs the commit. Its
  append terminal reads the raw identity-secret slot.
  Each remaining app must check how its own append path persists before copying
  the seam — the split is not universal. **This is not hypothetical:** web
  copied the seam WITHOUT the exemption on 2026-08-15 and
  produced precisely the failure the exemption exists to prevent — a registered
  half-account (`handle`/`domain` null) with `active` moved to it at the wizard's
  import step, which the boot re-mirror cannot heal because the stranded identity
  is now the *registered active* one, not an unregistered mirror target. Caught
  by `test_web_add_account_abandon_recovers_prior_identity_on_current_load`,
  fixed 2026-08-16. **windows shipped the identical hole and did not check
  either — the call-site swap routed BOTH `ConfirmGeneratedIdentity`
  and `ConfirmImportedIdentity` through `persist_confirmed_identity_mirrored`
  unconditionally, with no append gate at all, despite `App.IsAppendingAccount`
  already existing and being threaded into the VM for an unrelated reason (the
  cancel-button's own visibility).** Found and fixed 2026-08-26 with an `IsAppendMode` branch (a
  raw single-slot write); since 2026-09-25 windows calls the shared
  `ConfirmIdentity(secret, append)` in both modes (append writes nothing) and
  its append terminal (`App.xaml.cs`'s `_appendingAccount` arm) registers and
  switches from the wizard's `LoggedIn` outcome. apple followed the same day:
  `OnboardingVM.persistConfirmedSecret` calls `confirmIdentity(secret, append)`
  in both modes and its raw append-arm write is gone. windows' append-mode
  `AwaitingManualDns` exit skipped `PersistAwaitingDns` until 2026-09-25, leaving
  the appended identity's secret and the parked box's claim code in process
  memory through the "Almost ready" wait. It now persists
  unconditionally (as tui does) and hands the registered actor to the shell,
  which leaves append mode and switches: the same adoption as its pending-invite
  submit (`OnboardingViewModel.OnAwaitingDnsPersisted`, one shared handler in
  `App.xaml.cs`). android's exit skipped the write the same way until
  2026-09-28; it now persists unconditionally and its append arm switches away
  from the outgoing account — the adoption its pending-invite submit runs
  (`OnboardingHost.handleWizardExit` / `persistPendingInviteSlot`).
- **The cross-app erase assertion exists as of 2026-08-11, and the citation it
  replaces was over-claiming.** `test_sign_out.py` had asserted only that the app
  re-roots onboarding, while its own docstring — and this table's windows cell —
  described it as proof that the credential namespace was wiped. Those are
  different properties: an app that navigates to onboarding while leaving
  `fauna/{actor}/secret` behind satisfies the first and violates § Cleanup
  contract's properties 1 and 2. `test_sign_out_erases_the_credential_namespace`
  now reads the store back through the *live* namespace the app fixture's driver
  launched with (`common.cred_store.attach_cred_store`, over the cross-driver
  `_resolved_credential_dir` / `_resolved_keyring_app` pair) and asserts it empty,
  with a **non-empty precondition** first — every file-backed adapter reports an
  unreadable store as an empty set, so without it a mis-resolved path would pass
  as a successful erase. Green on `[tui, linux, web, windows]` (windows 2026-08-12,
  2/2, no code change needed); macos/ios need only a run on their machine;
  **android is a declared `skip_unbuilt`** — its credential
  file is written by the on-device bridge into the app's own filesDir, so no host
  path exists; the app's own namespace now reads back over the bridge
  (`GET /credentials`, which `attach_cred_store` binds), but the shared
  account-store file the app process writes beside it still has no read-back,
  and this test asserts both.
- **Web's `logout()` does erase `fauna_node_url` — no divergence from the other six
  apps (verified 2026-08-12; a prior draft of this bullet claimed the opposite).**
  `identity.logout()`'s own explicit `removeItem` calls skip it, but it also awaits
  `accountsClearAll()`, the shared `AccountRegistry::clear_all()` — and that
  function's `delete_legacy_keys_locked()` sweeps `LEGACY_NEST_URL` unconditionally
  alongside the other five legacy keys (`libs/fauna-client-accounts/src/lib.rs`,
  pinned by `clear_all_wipes_every_account_the_index_and_the_legacy_mirror`), which
  web's `native_key()` maps to `fauna_node_url` (`web_store.rs`). So all seven apps
  already converge on erasing it; there is no pending uniformity fix here. (A stale
  read of `logout()`'s explicit calls alone — without tracing into
  `accountsClearAll()` — is what produces the false "1-vs-6" claim; don't re-derive
  it.) `WebCredStore.stored_accounts()`'s test-harness key list
  (`tests/common/cred_store.py`) still omits `fauna_node_url` from its scope, which
  only means the harness doesn't *assert* on that key — not that the app leaves it
  behind.
- **Windows/apple/android Stage 1 is tracked internally** (per-platform `SecretStore`
  over the UniFFI callback-interface seam, launch rewire, switcher UI, append
  mode, switch-teardown/rebuild) — the pattern is proven on both a UniFFI
  app (linux) and a WASM app (web). **Windows and apple both landed the store +
  launch half on 2026-07-14**, independently and in parallel; **android landed its
  store + launch half 2026-07-18** (`LogicalSecretStore` + `SecretKeyMap` over
  `EncryptedSharedPreferences`, `LaunchPersistenceImpl` deleted, CR-3 upgrade route tested).
  Switcher UI (item 4) **landed on android 2026-07-18** and on
  **windows 2026-07-19**; the platform half is ~100 lines of key→key map plus a
  factory, and everything else is already shared. **With windows' Stage 2 landing
  2026-07-20, all seven apps now have the full multi-account surface — store,
  launch, switcher UI, append mode, and re-auth-on-activate.**
- **The UniFFI seam is proven in TWO foreign languages — android need not re-derive it
  (windows in C#, apple in Swift, both 2026-07-14).** The one thing that could have sunk the
  design does work in both: `FfiAccountRegistry.launch_persistence()` returns
  `Arc<dyn LaunchPersistence>`, a **cross-crate** trait object from `fauna_launch_machine`,
  and the bindings emit it as the *same* foreign type `LaunchMachine`'s constructor takes
  (not a re-declared twin), so it feeds straight into the machine. The `with_foreign`
  `FfiSecretStore` callback works in C# and Swift alike. Shapes worth copying rather than
  reinventing: (1) the platform store is a **trivial key→key map plus a raw key/value
  backend** — a golden-table test pins the `legacy/*` rows, and an end-to-end test through
  the real `LaunchMachine` proves a pre-upgrade pending-factory-reset row still routes to
  the pre-filled claim (break one mapping and both go red; apple's twin is
  `KeychainSecretStoreTests`); (2) give the store **two backends** — the OS secret store and
  a file backend selected by `FAUNA_E2E_CREDENTIAL_DIR` — which is what lets an e2e seed a
  whole multi-account registry before launch with no app code (linux's shape;
  `tests/common/accounts.py::build_registry_seed` + the per-app `*_LEGACY_KEYS` names).
  **Apple has (1) but not yet (2)**: its Keychain store's e2e mode is a process-static dict
  with an optional `keychain.json` backing, so seeding a multi-account registry is leg-B work.
- *History (the maps and the guard below were deleted 2026-09-28, with the single
  slot they served):* **Key the platform store off its own typed slot names, not string literals** — apple maps
  `legacy/*` → `KeychainStore.Key` cases, so renaming a slot is a compile error rather than a
  silently orphaned user row. The map is the one place a typo destroys data — and it used to
  pass every Rust test in the tree. Since 2026-08-22 it does not: `fauna_client_accounts::LEGACY_KEYS`
  is the enumerable owner, and `libs/fauna-client-accounts/tests/legacy_key_cross_language.rs`
  reads the three hand-written maps' production source (Kotlin/C#/Swift) and asserts each carries
  **exactly** that set — a key missing from an app strands the row it names, an unowned key in an
  app is a typo or a half-finished rename, and both are now red. The guard needs no
  Kotlin/Swift/C# toolchain. linux is deliberately outside it (it maps six by design — the eleven
  wizard-resume keys never had linux native names); web is covered by a coverage-only arm, since
  its map is Rust and the compiler already catches renames.
- Switch = `set_active` → teardown → rebuild, live in-session (design
  Decision 1, no relaunch); web's teardown/rebuild is a full reload.
- **iOS's optimistic-entry launch must read the legacy Keychain slot AFTER the
  boot mirror, not before (fixed 2026-07-15).** `FaunaApp.runLaunch()` (unlike
  `FaunaMacApp`, which gates behind a spinner and never reads ahead of the
  machine) renders home tabs straight off the legacy slot before the machine
  settles — its documented posture. `FaunaAccounts.bootLaunchPersistence(keychain:)`
  mirrors the ACTIVE account into that same legacy slot as a side effect of
  construction, and the file used to call the optimistic read (`enterOptimistically`)
  BEFORE constructing `bootLaunchPersistence`. Harmless on a cold boot (the legacy
  slot already matches the active account), but on a same-process account SWITCH
  — which moves the registry's active pointer via `setActive` without touching the
  legacy slot itself — the pre-mirror read built `client`/`session.actorId` from the
  OUTGOING identity's stale secret, while `session.handle` (re-read later in
  `completeAuthenticatedGlue`, by which point the async mirror had long since
  landed) correctly showed the INCOMING identity — an incoherent, never-self-correcting
  session (`dispatchLaunch`'s `.online` case only re-enters `enterOptimistically`
  when `client == nil`, which it never was). Fix: construct `LaunchMachine`/
  `bootLaunchPersistence` (the mirror) BEFORE the optimistic read, matching the
  invariant `FaunaMacApp` already held by construction. Any future app design
  that renders optimistically off a legacy/cached slot ahead of the machine must
  preserve this same ordering across a switch, not just a cold boot.
- **iOS's gated `admin-tab` cannot be e2e-verified by List-row visibility** — it
  sits inside the Settings ROOT `SwiftUI List`, several sections below the fold,
  and the in-process driver's `/scroll` is a documented stub for a lazily-rendered
  `List`/`Form` (`docs/goal/architecture/apps/apple-e2e-automation.md` rule 6;
  the same pre-existing gap that already excludes iOS from
  `test_admin_nav.py::test_admin_tab_visible_for_admin`). `test_account_switcher_apple.py`'s
  switch journey verifies the live reconnect through session state
  (`actor_id`/`handle` now matching the target admin account) on mobile instead —
  the same "honest mobile substitute" `test_family.py` uses for `family-tab`.
- Driving an append wizard from e2e requires registering its
  `OnboardingMachine` with the e2e agent (`set_active_onboarding_machine`) —
  a per-app e2e-support requirement windows/apple/android mirror.
- **"Add account → a WORKING session as the new identity" is proven on tui
  (2026-08-14), web (2026-08-17), apple and windows (2026-08-26). linux's red is fixeda desktop's two nest credentials were racing for its one `sync_devices` row
  and the store principal lost; android still asserts nothing about it.**
  Every app's append test stopped
  at the persisted `AccountIndex`, so design Decision 1's *user-visible* half —
  you land in a working session, not merely a stored row — was unproven
  fleet-wide, and one session lost a full run diagnosing the resulting dead end
  as a product regression. What blocked the assertion is harness-side: an
  appended account's `nest_url` is derived from the typed handle domain and is
  therefore uniform https (`fauna_provisioning::probe`), which a plain-HTTP
  tier_3 nest cannot serve. **The answer is the `provider_base_urls["nest"]`
  override seam** — one `driver.set_provider_base_urls({"nest": nest["url"]})`,
  which mirrors into the process-global store-read dial the **native** apps'
  launch-from-store resolves through (`fauna_launch_machine::dial`, reached via
  its `WsAuthConnector`) — **not a `serve_tls=True` nest**, a prescription that
  predates that seam and is now superseded. Each leg is that one call plus the
  shared `helpers.waiting.await_session_actor` assertion, and also pins that the
  store keeps the *derived literal*, since a leaked override would be at-rest
  corruption rather than a test-only wart.
- ⚠ **web reaches that seam through its OWN twin, not the Rust one (2026-08-17).**
  The sentence above is native-shaped: web builds its nest clients in TypeScript
  from `$lib/api`'s `nodeUrl()`, so the Rust override cannot reach web's socket —
  it lives in a `static` inside whichever wasm chunk wrote it, and wasm chunks
  share no linear memory, so the copy the harness writes (through the *onboarding*
  chunk's `set_provider_base_urls`) is never the copy a consumer would read. web's
  twin is therefore `nodeUrl()` itself, resolving a test-only override that the
  same `driver.set_provider_base_urls({"nest": …})` gesture installs; the literal
  keeps the name `storedNestUrl()` and is what every user-facing rendering reads.
  Same seam, same one-call harness gesture, same "redirects the socket, never the
  truth" rule — a different implementation language. A session porting this leg to
  windows / apple / android should expect the native shape, not web's.
- ⚠ **A second barrier sat behind the dial on web, and it is the class worth
  naming (2026-08-17).** web's e2e state provider published
  `session.authenticated` from an identity-store field (`registered`) that the SPA
  had stopped setting — derivable, a comment reasoned, from the cached handle. So
  the field was `undefined` for *every* real session and could be made true only
  by the harness's own `set_state` injection: the cross-app "this identity reached
  a WORKING session" observable structurally could not be true, and presented as a
  launch that would not connect. The fix restores it as the *live* signal — set
  when the silent challenge verifies against the nest, never persisted, never
  derived from cache. Any app whose `authenticated` can be true without a round
  trip has the same hole (e2e-conventions.md point 11).
- ⚠ **The first thing that assertion found: on linux the append did NOT switch
  the live session** — the app stayed authenticated as the outgoing account while
  the registry reported the new one active (**still red**).
  **Why no test caught it: `handle_wizard_done` moves `active`
  before the switch is even attempted, so the registry reaches its final state
  whether or not the switch runs** — and the registry was the only thing being
  asserted. Treat a passing index assertion as evidence about the *store*, never
  about the session; the same caution applies to the four apps whose legs are
  still owed.
- **The cause, measured 2026-08-16 — and it is NOT the switch glue.** The append
  never reaches `trigger_switch_account` at all: `handle_wizard_done`'s append
  branch re-reads the identity triple via `client::load_credentials()` and calls
  `registry.add_account(&secret_hex, …)`, which fails with **`invalid secret:
  expected 32 bytes (64 hex chars), got 0`** — an *empty* secret — and takes the
  `Err` arm that dismisses the wizard. So the live session correctly still belongs
  to the outgoing account; nothing ever asked it to change. Two corollaries for
  whoever picks this up: `load_credentials()` returned `Some` carrying an empty
  secret rather than `None`, which is what let an unusable triple reach
  `add_account` in the first place; and the failure arrives in a **hot loop**
  (thousands of identical lines in ~50 ms, an 18 MB `app.err`), so the append
  branch is being re-entered repeatedly rather than once. **Both of the
  original candidate causes are refuted** — the GTK-thread `block_on` stall and
  the unregistered-handler no-op — and so is the guess that a sibling app could
  present this observable by launching synchronously on its UI thread: checked
  2026-08-16, tui's session/launch paths carry no `block_on`, windows'
  `AccountSwitcherViewModel` no `.Wait()`/`.Result`/`GetAwaiter().GetResult()`,
  and apple's only `DispatchQueue.main.sync` is the automation server's own hop to
  main. The four owed legs should look at what their append feeds `add_account`,
  not at their switch glue.
- ⚠ **windows' cause was in the switch glue after all (2026-08-26)** — the one
  case the sentence above did not anticipate, so it is worth stating why linux's
  finding did not transfer.
  `App.SwitchAccountHandler`'s "already active — nothing to tear down" guard
  read the **registry's** `active` pointer, not the live session: `AddAccount`
  had already moved `active` to the just-appended identity (unlike linux, where
  `add_account` never even ran), so by the time the append's own
  `await SwitchAccountHandler(addedActor, false)` checked, the store already
  agreed with itself and the guard skipped the ENTIRE teardown+rebuild — the
  outgoing session torn down nowhere (this half genuinely is a no-op, correctly),
  the incoming one never launched. The registry-only assertion passed (the same
  "evidence about the store, never the session" trap two paragraphs up), and the
  live app was left authenticated as **neither** identity, indistinguishable from
  the outside from a hung launch. Fixed by checking the LIVE `_cryptoService`
  identity instead of the store read —
  the switcher-row click the guard exists for keeps its correct no-op, since a
  live row's account IS the loaded crypto by construction. The identity-theft
  recovery succession (`SettingsAccountPage.IdentityStolenButton_Click` →
  `SwitchAccountHandler(successorActorIdHex, false)`) shares the exact same
  shape — a brand-new identity handed straight to this call — so the fix
  protects that path too, not just the append. **The generalizable rule for the
  one still-owed leg (android):** check BOTH candidates — an unusable secret
  reaching the registry write (linux's cause) AND an "already active" guard
  reading the store instead of the live session (windows' cause) — a
  registry-only assertion cannot tell them apart, and neither excludes the
  other.
- linux deferred residue: `build_ui`'s five-case match reads the active
  account's slots through `launch_credentials` (registry-backed since
  2026-09-24; the single-blob migration it once carried is gone); switcher
  live-refresh after an in-place `remove` is pending (build-once page).
- Per-actor awaiting-DNS slot: DEFERRED to Stage 3 (2026-07-05 call) — the boot
  re-mirror renders the strand inert; see § Multi-account evolution. (Its
  former pending-encryption-mode sibling is retired outright, no-modes
  ratified 2026-07-12 — not merely deferred; there is no slot or routing left
  to carry across accounts.) Background cross-identity concurrency (the only
  nest-touching piece) is PARKED.
- **Stage 2 (re-auth-on-activate) — shared-Rust gate + the apple reference
  landed 2026-07-16; the other apps' surfaces are the remaining work.** The
  enforcement shape is ratified and implemented (§ Multi-account evolution,
  "Per-account re-auth"): `set_active` refuses a flagged account with
  `ConfirmationRequired`; `set_active_confirmed` is the post-re-auth path,
  exposed through `FfiAccountRegistry` and the wasm `setActiveConfirmed` (wasm
  also gained the previously-missing `setRequireConfirm` write path).
  Unit-tested in `libs/fauna-client-accounts`
  (`set_active_refuses_a_flagged_account_…` and siblings). Apple (shared
  FaunaKit, both apps) is the reference: row toggle + `LAContext` confirm
  (`AccountReauth`) + the `reauth-result` e2e seam + the tier_3
  decline-then-approve journey. **Linux landed 2026-07-16 and is the in-app
  reference** for the no-native-prompt platforms: it ratified that surface's
  shape (§ Multi-account evolution → *Per-account re-auth*) — the
  `account-activate-reauth-prompt` view plus its confirm/cancel pair — so
  **web and tui adopt it rather than inventing one**. **Web landed 2026-07-16**
  (the linux in-app shape end-to-end — toggle, gate, prompt, admin
  auto-default; see the table row). **Android landed 2026-07-18** — the row
  toggle + the native `BiometricPrompt` gate (`core/AccountReauth`, the same
  `reauth-result` file seam apple uses) + the admin auto-default at the
  `SettingsAdminGateVM` nav-gate; the app surface is Robolectric- and
  unit-verified, and its tier_3 journeys are wired (the driver's bridge
  read-back and verdict verbs, 2026-09-27) but unrun: android e2e has never run,
  pending the A2 run venue. Still open per remaining app (§ status
  table's Stage-2 row): the `account-require-confirm-toggle` row toggle + the
  platform's re-auth surface — **windows Hello** natively. tui: its
  `let _ = set_active` call sites fail closed for flagged accounts (gate holds);
  its account UI adopts the same in-app shape, adapted to a terminal prompt,
  when it arrives.
  **The admin auto-default landed 2026-07-16 in shared Rust + apple** (the
  `require_confirm_user_set` marker + `auto_enable_require_confirm`, exposed
  FFI + wasm; apple hooks it at both nav-gate `am-i-admin` probes; tier_3
  `test_apple_admin_auto_default_flags_admin_and_explicit_off_sticks`). The
  remaining apps wire the same call at their own `am-i-admin` observation
  points as part of their Stage-2 slices.
- Real-keyring test harness is shared at `tests/common/keyring.py`.
- **Ending a session is not the erase.** tui's `session::sign_out()` and linux's
  account-switch teardown drop the in-memory session and keep credentials on
  purpose. § Cleanup contract governs the *erase* — the "Sign Out" affordance
  and the factory reset — not every path that tears a session down.
- **The freedesktop apps wipe by namespace, not by key.**
  `CredentialStore::delete_namespace()` (`libs/fauna-credential-store`) is
  the one primitive linux (`delete_credentials_in`) and tui (`App::reset`) both
  call; `AccountRegistry::clear_all()` is the portable key-wise erase web needs.
  Both satisfy § Cleanup contract's delete-only rule. Cross-app coverage is
  `test_onboarding_launch_routing_smoke.py` case G (reset → the namespace reads
  empty → the relaunch is a fresh install), green on `[linux, tui]`.
- **Web's erase is pinned by its own module, not by case G**
  (`tests/e2e-unified/tests/test_sign_out_web.py`, tier_3): case G drives
  `driver.reset()`, and web's `reset` sweeps the `fauna/` prefix out of
  `localStorage` **itself** (`web-bridge/agent.js`), so a reset-driven assertion on
  web would go green on the *test agent's* cleanup and stay green with
  `accountsClearAll()` reverted — it would pin nothing. Case G's force-quit half has
  no browser analogue either (a driver teardown discards the whole profile,
  namespace included). The web module therefore drives the real Settings sign-out UI
  and reads `localStorage` back through the page. Falsified, not just asserted:
  reverting `logout()`'s `accountsClearAll()` leaves
  `['fauna/{A}/nest_url', 'fauna/{A}/secret', 'fauna/index']` behind and the test
  fails on exactly that.
- **tui (the 7th app, absent from the six-app table above until the parity
  milestone) satisfies § Cleanup contract in full:** `App::reset()` drops the
  launch machine, *then* calls `delete_namespace()`; `App` owns its credential
  store as an injected handle, so no unit test can sweep a developer's real
  `fauna-tui` keyring namespace.
- **Both halves of quiesce-before-wipe are now closed**, and the
  structural half is the one that holds it: a read never writes (§ Cleanup
  contract), so an un-cancellable reader can no longer resurrect the identity a
  wipe just erased. The ordering half — every app's sign-out path erases
  *last* — is what the table above records per app; it is a defence in depth,
  not the guarantee. Do not re-derive the old "reorder linux's callers" fix: it
  was the weaker half, and on its own it could not beat a writer no caller can
  cancel.
