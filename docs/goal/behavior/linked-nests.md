# Linked nests — target state

Owns: linked-nests
Status: ratified — design 2026-05-25 (tracked internally); built on all 7 apps (tui landed 2026-07-24; see § Implementation status today)
Authority: the user-facing side of nest pairing — the surface in a user's *own* settings where they link one of their nests to sync their account (per-user multi-homing), list their linked nests, and unlink them; owns the surface's behavior, the shared `LinkedNestsMachine`, the link/list/unlink flows, and the one-action-seeds-both sequencing. The pairing record, capabilities, sync surface, and nest-to-nest auth → [`../architecture/nest/private-mode.md`](../architecture/nest/private-mode.md) (registry: `nest-pairing`); the admin pairing-policy knob → [`../architecture/nest/public-mode.md`](../architecture/nest/public-mode.md) § Nest Pairing Policy + [`admin.md`](admin.md) § N Nest; the Nests-page UX (elements, layout, trust facet) → [`../ui/nests.md`](../ui/nests.md); `tests/e2e-unified/ui.yaml` owns the `nests` page IDs + per-page element scope. On conflict in those docs' domains, raise it.

## Goal

A user sees **one place** in their own settings — the **Nests** page (formerly "Linked nests") — that answers "which of my nests sync my account, and how do I add or remove one?" Pairing is the user's call, device-like and client-revocable (priority: `principles.md` § The user always controls their data — "user always controls their data" + "new data shapes default to user-revocable from the user's own app"). It is **not** an admin action; the admin's only pairing control is the nest-level pairing-policy knob ([`admin.md`](admin.md) § N Nest).

This is conceptually adjacent to device management ([`devices.md`](devices.md)): a device is a *client instance* holding the user's keys; a linked nest is a *server* that syncs the user's account data. Both use the same "authorize from your own app" pattern. The relationship is generalizable to **N** nests — the `nest_pairings` schema is N-per-actor (`private-mode.md` § Pairing Flow).

## The surface

A page in the user's own settings (not the admin shell), rendering the shared `LinkedNestsMachine` (§ Where logic lives). It does three things:

- **List** the user's active pairings — for each: a label, the linked nest's id (`private_nest_id`, abbreviated), the capabilities it may sync — the `default_self_sync()` set: `mls_pull` / `namespace_sync` / `post_forward` / `mail_pull` / `nostr_push` (owner: `private-mode.md`; `mail_pull` is what lets a linked relay pull sealed mail, `nostr_push` the Nostr proxy-delegation equivalent; and `account_replica`, the sixth, whose linked nest keeps a sealed copy of the account plane — § Implementation status today) — and an optional expiry. Sourced from `fauna.pair.list` (owner-implicit).
- **Link** a nest — the user supplies the other nest in the add form and confirms. The value is classified (shared `classify_link_input`): a nest **address** (URL) links both ends in one action (§ One action seeds both ends — the common case for a nest the user is also logged into), while a bare 64-hex Ed25519 **identity** authorizes that one nest (the out-of-band single-end path, for a nest the client need not connect to). Either dispatches `fauna.pair.add` (bearer, owner-scoped). If the admin has pairing disabled, the nest rejects and the surface shows the policy error. Default capability set is the user's full self-sync; per-capability scoping is a future refinement.
- **Unlink** a nest — dispatches `fauna.pair.revoke` (bearer, owner-scoped); revocation is immediate (`private-mode.md` § Pairing Flow). Unlinking ends that nest's replica of the account plane: when it can be reached, the account's escrow wraps there are deleted first (owner: [`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § The bind leg, ruling 4).
- **See — and retry or drop — the user's own post-forward queue** on the connected nest, when it is a home nest whose relay is refusing: the list reply's additive `forward_queue` and the `fauna.pair.forward_{retry,discard}` actions. The ruling (whose page, why the user's) is `private-mode.md` § Post Forwarding; the page shape is [`../ui/nests.md`](../ui/nests.md) § Forward queue.

The peer-authed pairing handshake plays no role here — authorization is purely the user's bearer `fauna.pair.add`.

**One action seeds both ends.** Pairing is "log into 2+ nests, then pair them": when the user links two nests they are authenticated to, a **single Link action seeds the authorization row on *both* nests** — the client, holding (or opening) a connection to each, writes `fauna.pair.add` to both. This is what makes the asymmetric home-with-public-relay deployment work end-to-end without manual per-nest steps: the public nest's row authorizes the private nest to pull, and the private nest's row is what makes its sync/relay worker run (`private-mode.md` § Pairing Flow; [`../architecture/nest/deployment-home-with-public-relay.md`](../architecture/nest/deployment-home-with-public-relay.md) § Pairing).

The user supplies the **other nest's address**; the client opens an authenticated connection to it (the user's same identity, registered on both), discovers both nests' Ed25519 ids via the anonymous `fauna.nest.info` kind, and writes the reciprocal rows — `{private_nest_id: other_id}` on the connected nest, `{private_nest_id: this_id}` on the other. **No new nest surface:** discovery is `fauna.nest.info` (callable on the authed connection) and the writes are ordinary `fauna.pair.add`; both rows default to the full self-sync set. Because each nest stores a row naming the *other*, the model is symmetric regardless of which nest is public/private. The shared `classify_link_input` routes the link form's value — a 64-hex identity → `Link` (the out-of-band single-end path), any other value → `LinkBoth` by address — so every app routes the same input identically. Per-app status: § Implementation status today.

**The link carries the identity's recovery state to the other nest (ruled 2026-10-01).** Before it writes a pairing row, `LinkBoth` reconciles the RecoveryKey registration chain between the two nests, and when the linking identity has predecessors it first submits their succession statements at the other nest. A link that finds two different chains is refused with the page's `error-message`. The rule, its reasons and what the runtime's passes do afterwards are owned by [`succession-linked-nests.md`](succession-linked-nests.md) § Every nest the identity is linked to. The single-end path, which opens no connection to the other nest, carries nothing; the runtime's next full pass does, for a row that carries an address.

## Element IDs

Ratified in `tests/e2e-unified/ui.yaml` (UI rule A). The page + IDs renamed `linked-nests` → `nests` on 2026-07-07; the **full page ID set (linking + the trust facet) is owned by [`../ui/nests.md`](../ui/nests.md) § Element IDs** — do not duplicate it here. The linking-half IDs this doc's behavior drives are:

- Page `nests` (in user settings): `page-heading`, `error-message`, `nests-add-button`.
- Add form: `nests-add-input` (nest address or 64-hex identity), `nests-add-submit-button`, `nests-add-cancel-button`.
- Component `nests-item` (indexed): `-label`, `-nest-id`, `-capabilities`, `-expiry`, `-unlink-button`.

Same IDs on all seven apps (priority #1). **Rename complete:** linux, web, and windows emitted the renamed `nests-*` IDs from 2026-07-10; android (`LinkedNestsScreen.kt`) picked it up with its 2026-07-13 trust-facet shell, and the shared FaunaKit `LinkedNestsView` (macos + ios) with its 2026-07-15 shell — no `linked-nests-*` holdouts remain (tui's nests page, born 2026-07-24, emitted `nests-*` from day one).

## Where logic lives

Per priority #2, shared Rust by default; per-app shells render + dispatch only.

- **Shared Rust (`libs/fauna-client-pair`)** — the `LinkedNestsMachine` (snapshot + dispatch, mirroring `DnsManagementMachine`): `Refresh` → `fauna.pair.list`; `Link { nest_id, capabilities, expiry? }` → `fauna.pair.add`; `LinkBoth { other_nest_url, … }` → the both-ends sequencing (discover via `fauna.nest.info` → `connect_peer` → reciprocal `fauna.pair.add` writes: peer first, then the connected nest, then re-list); `Unlink { nest_id }` → `fauna.pair.revoke`. The `LinkedNestsNest` seam carries `this_nest` + `connect_peer` (native: a second `NestClient::new(url, keypair).connect()`; wasm: a second authenticated `WsRpcClient`, its bearer minted over the CORS-exempt anonymous WS `fauna.auth.handshake`). Renders the pairing list, the link form state, and the admin-policy error. Exposed over UniFFI (native) + WASM (web); the per-app shell renders the snapshot and dispatches actions with no pairing logic of its own.
- **Nest** — the bearer kinds `fauna.pair.{add,revoke,list}` (owner-scoped); the admin-set `pairing` policy gate on `add`. Wire shape, capabilities, and storage are owned by `private-mode.md`.
- **Per-app** — render the snapshot, dispatch actions; no pairing logic in any shell.

## Evolution: the Nests page (ratified 2026-07-06)

This surface **evolved into the first-class Settings → Nests page** — every nest the user's
content lives on (home + linked), each row adding a **trust facet** (content-processing
grants + revoke + the Now/History grant-event-log lens), **role badges** (e.g. backup
destination), and a read-only **quota display** on top of the link/list/unlink behavior
this doc owns. The participant model, vocabulary ("trust", "delegated tasks" — never
"capability" in the UI), and the sibling-pages decision are owned by
[`participants.md`](participants.md); the trust-facet v1 first landed 2026-07-07 (web) —
the per-app frontier is owned by [`../ui/nests.md`](../ui/nests.md) § Implementation status
today. The **page UX (elements, layout, the trust-facet copy + Now/History behavior) is owned by
[`../ui/nests.md`](../ui/nests.md)** (ratified 2026-07-07); the ui.yaml page + IDs renamed
`linked-nests` → `nests` the same day (per-app rename status: § Element IDs above), and the
Settings rail entry renames "Linked nests" → "Nests"
([`../ui/settings.md`](../ui/settings.md) § Navigation model). **v1 scope is the trust facet
only** (grants + revoke + Now/History); role badges + quota display are Phase-2 facets
([`participants.md`](participants.md)). No contradiction with this doc: linking is how a nest
becomes a row on that page, and everything here (the `LinkedNestsMachine`, one-action-seeds-both,
the admin knob) carries over unchanged.

## Relationship to neighboring docs

- **`../architecture/nest/private-mode.md`** owns the pairing record, capabilities, the paired sync surface, and nest-to-nest auth. This doc is its app face.
- **`../architecture/nest/public-mode.md`** § Nest Pairing Policy owns the admin knob.
- **`admin.md`** § N Nest owns the admin pairing toggle's page home (`admin-service-pairing-toggle` on `admin-nest`; the former Services page was removed 2026-06-04). The former `admin-private-nest` page is removed; this surface replaces its relationship role, and provisioning moves to onboarding/installer.
- **`devices.md`** is the sibling "authorize from your own app" pattern (devices, not nests).

## Implementation status today

**Built 2026-09-30 — the `account_replica` capability.** A linked nest holds a sealed copy of the account plane, delivered by the user's own devices on all seven apps, so it can serve recovery of a lost box; it is in the default set a new link carries, and an unlink deletes the account's escrow wraps at that nest when it can reach it. Owner: [`../architecture/nest/private-mode.md`](../architecture/nest/private-mode.md) § The account-plane replica. **The capabilities line names it in user voice — built 2026-10-01 on tui, web, linux and android**: the shared `LinkedNestRow` carries `capability_labels`, one `LocalizedText` per capability from `fauna_client_pair::capability_label` (`account_replica` → the i18n string `nests.capability_account_replica`; a capability with no label keeps its wire name, so the other five read as before), and each shell resolves and joins them — no element id changed. Windows, macos and ios still join the wire names until their lifts; the record field carries a default, so they build unchanged. Android's edit is not yet compiled: the host build its compile check needs was red on main that day. The app tour's Nests entry says what the linked copy is for. A linked nest the account administers also has its own deployment seed custodied by the user's devices (owner: [`../architecture/nest/box-recovery.md`](../architecture/nest/box-recovery.md) § Implementation status today, which also owns the open desktop gap). A pairing made before the capability existed carries five capabilities, and relinking it adds the sixth.

**The link carries the registration chain (built 2026-10-01) and delivers the predecessors' statements first (built 2026-10-02).** `LinkBoth` reconciles the RecoveryKey registration chain between the two nests once it has connected to the other one and before it writes either pairing row, through the `LinkedNestsNest` seam's `registration_chain` / `submit_registration`; a fork is refused as `PairDispatchError::RecoveryKeysDiffer`, whose text is the string `nests.link_recovery_keys_differ`, shown in the page's `error-message` on every app with no shell change. Before it connects to the other nest, a linking identity that has predecessors submits their succession statements there over an anonymous connection. It reads them from the connected nest's additive `predecessor_statements` (on `fauna.recovery.succession.status`) through the seam's `predecessor_statements` / `submit_succession_at`, so no app's wiring changed, and a refusal never stops the link. After a succession the successor's account runtime also carries the statement to every nest the burned pairings named, in its full pass (built 2026-10-02). Status and build pointers: [`identity-succession.md`](identity-succession.md) § Implementation status today.

**Everything else is built; the two open items are verification/rename legs, not code.** Substrate: the `fauna.pair.{add,revoke}` wire types + the canonical capability constants (`default_self_sync()` = `mls_pull` + `namespace_sync` + `post_forward` + `mail_pull` + `nostr_push`, pinned by `default_self_sync_is_the_canonical_set`) live in `libs/fauna-protocol/src/pair.rs`; the `LinkedNestsMachine` + `LinkedNestsNest` seam + `classify_link_input` live in `libs/fauna-client-pair`, exposed over UniFFI (`fauna-ffi/src/pairing.rs`) + WASM (`fauna-wasm`); the nest handlers (`bins/fauna-nest/src/pair_handlers.rs` — bearer `User`, owner-scoped, `add` empty-caps → `default_self_sync()`, gated by the admin `pairing` service knob in `services.rs`, default-on) and the `nest_pairings` schema incl. the `label` column + `PairingRow.label`/`nest_url` are live. The legacy admin approval path and the retired peer handshake are removed.

| App | Render (single-end + both-ends) | Notes | e2e (`test_linked_nests.py`, 4 tests) |
|---|---|---|---|
| linux (lead) | ✅ 2026-05-29 / 2026-06-04 | `settings/linked_nests.rs` over native `build_linked_nests_machine` | ✅ green `--client linux` (incl. `test_link_both_seeds_both_nests`, `test_admin_knob_off_rejects_link`) |
| web | ✅ 2026-05-29 / 2026-06-04 | `NestsSection.svelte` over `WasmLinkedNestsMachine`; both-ends via a second authenticated `WsRpcClient`, bearer minted over the CORS-exempt anonymous WS (`challengeVerify`); `connect_peer` waits (bounded) for the peer client so an unreachable peer fails in ~1 s | ✅ green `--client web` |
| windows | ✅ 2026-05-29 / 2026-06-04 | `Controls/NestsPanel` over UniFFI; render-only | ✅ green `--client windows` (2026-06-12, 5+ consecutive; UIA-pattern clicks, no SendInput flake) |
| android | ✅ 2026-05-29 / 2026-06-05 | `LinkedNestsScreen.kt` over UniFFI, reached via a settings sub-page entry; emits the renamed `nests-*` IDs since its 2026-07-13 trust-facet shell | ⏸ emulator-gated on the emulator host (test is app-agnostic; un-gates when the harness exists) |
| macos | ✅ 2026-06-13 | shared FaunaKit `LinkedNestsView`/`LinkedNestsVM` (mail-relay machine builder), embedded in Preferences; emits the renamed `nests-*` IDs since its 2026-07-15 trust-facet shell | ✅ green 4/4 `--client macos` (2026-06-13, post-reboot; the bridge `materialize` fix reaches the embedded click paths — not shell-gated) |
| ios | ✅ 2026-06-13 | the same shared FaunaKit view, pushed as a Settings sub-page (`NavigationStack`); renamed `nests-*` IDs since the same 2026-07-15 shell | ✅ green 4/4 `--client ios` (2026-07-15, alongside macos, no per-platform fix — one shared view, one shared bug set) |
| tui | ✅ 2026-07-24 | `settings/nests.rs` over `build_linked_nests_machine_with_mail_relay_and_trust` (direct Rust, no FFI hop — tui *is* Rust); the whole page landed in one slice (linking half + the v1 nest-trust facet + backup trust rows) | no declared tui absence — the four tests run on tui like any app; reported 4/4 green `--client tui` at the 2026-07-24 landing ([`../ui/nests.md`](../ui/nests.md) § Implementation status today), and the feature ledger's latest tui record (`docs/features/ledger/tui.json`, linux, 2026-09-25, `standalone` nest mode) is **4/4 passed**, `test_admin_knob_off_rejects_link` included — it replaces a 2026-09-14 `live`-mode record (three errors, one failure) that did not reproduce |

Dated gotchas: the both-ends web blocker was cross-origin auth — minting the peer bearer over HTTP was CORS-blocked; the fix adopted the WS-RPC auth bootstrap (`fauna.auth.handshake` on the anonymous WS, CORS-exempt — transport.md § Pre-identity) (2026-06-04). The earlier "macos click paths are off-screen-unhittable until the Settings shell lands" prediction was wrong — the apple-bridge `materialize`/ancestor-scroll fix reaches PreferencesView-embedded controls (2026-06-13). Proof for both-ends: crate `FakeNest` unit tests + the two-nest tier_3 `bins/fauna-nest/tests/conformance_cross_nest_pairing_client.rs` + `test_link_both_seeds_both_nests`. Cross-app follow-up: the page-based apps (android + windows + ios) reach the surface from a per-app settings-entry nav affordance; if a canonical ui.yaml element is wanted for it, that's a coordinated spec addition across those apps.
