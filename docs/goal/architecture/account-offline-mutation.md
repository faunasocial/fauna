# The offline-mutation contract — target state

Owns: account-offline-mutation
Status: ratified — all four W4 phases are code (the per-kind classification 2026-08-10, the outbox and the `OfflineSafe`/`OfflineQueued` split 2026-08-13, UI desensitizing the same day) and the per-app fan-out of the desensitizing rule is still trickling down; split verbatim out of `account-data-plane.md` on 2026-09-06
Authority: **what an app may do while it cannot reach a nest, and how the UI says so** — the offline-mutation contract (the outbox, idempotency and reconnect-with-resume, the per-kind offline classification criteria) and the shared desensitizing rule `affordance(kind, connection_state)` with the whole per-app build-out that consumes it. **NOT owned here** — the plane a queued mutation drains onto → [`account-sync-plane.md`](account-sync-plane.md); the store that holds the outbox → [`account-data-plane.md`](account-data-plane.md) § The account store (W1); which process owns the engine while offline → [`account-runtime.md`](account-runtime.md); the per-app UI surfaces themselves → their own `ui/` docs; the charter and the cross-cutting status → [`account-data-plane.md`](account-data-plane.md). On conflict in those domains, raise it.

Last verified: 2026-09-06 (split verbatim; this is the fastest-growing of the five plane docs — the per-app desensitizing fan-out — and its growth decays as the last apps land)

Split verbatim out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06 — that doc had reached **686,434 B**, 2.62× the 262,144 B whole-file read ceiling, and no single seam could clear it (moving its status ledger alone left both halves breached, re-verified at three successive sizes). Its own `Authority:` line already enumerated the five concepts it owned; this is that list made structural, each concept taking its rule sections **and** its status-ledger entries together. A routing stub remains at each original location; prior history: `git log --follow docs/goal/architecture/account-data-plane.md`. The `W<n>` workstream labels and `R<n>` decision labels used throughout are defined in [`account-data-plane.md`](account-data-plane.md) § Workstreams and § The ratified decisions.

> **Reading this doc.** Its text was carried **verbatim** out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06, so an unqualified `§ <name>` citation inside it may name a section that is no longer a sibling on the page. Resolve any such name against the rest of the family first: [`account-data-plane.md`](account-data-plane.md) (the ratified decisions, the account store, the nest-side requirements and the cross-cutting status), then [`account-data-taxonomy.md`](account-data-taxonomy.md), [`account-sync-plane.md`](account-sync-plane.md), [`account-runtime.md`](account-runtime.md), [`account-replica-posture.md`](account-replica-posture.md). Positional words (“above”, “below”) inside a carried block point within this doc: every section moved whole, so an intra-section deictic could not break, and the boundary-crossing ones were scanned before the split.

## Section map

- **[The offline-mutation contract (W4)](#the-offline-mutation-contract-w4)** — the outbox, idempotency, and the classification criteria. **The heading is unchanged from `account-data-plane.md`**, so a `§ The offline-mutation contract (W4)` citation resolves by swapping the filename.
- **[Implementation status today](#implementation-status-today)** — W4 phases 1–4, and the per-app fan-out of the desensitizing rule, app by app.

## The offline-mutation contract (W4)

- **The outbox.** Mutations become store writes: apply locally where the
  kind's class allows (optimistic), and append a durable **intent** to the
  store's outbox. Intents replay on reconnect (to nest, or peer-gossip per
  the peer-leg scope) with **client-generated idempotency keys**; the sole
  durable outbound queue today is file-sync's `transfer_queue`, the shape
  generalized. The outbox is the one non-wipe-tolerant store component: an
  undrained outbox holds the only copy of the user's pending writes.

  **The outbox is its own durable component, and it holds `OfflineQueued`
  intents only (W4 phase-0 ruling, 2026-08-12; BUILT conforming 2026-08-13 —
  § Implementation status → *Built — W4 phase 1*).** The bullet above
  states a requirement, not a schema, and the
  class-2 plane's `publish_pending` proves the journal alone reaches the
  same durability for state entries — so the boundary is ruled, not
  assumed, on four discriminators:

  1. **An intent awaits a nest-assigned effect; a journal row is published
     truth.** An `OfflineSafe` mutation IS a store write: its journal row
     is its durable replay record, publication is own-log replay above the
     published watermark (`publish_pending` today; the same discipline for
     content rows when the own-record push leg builds), and it never
     enters the outbox. An `OfflineQueued` intent is not a store item at
     all — an MLS compose is a plaintext body that cannot be sealed until
     drain ([`../behavior/devices.md`](../behavior/devices.md) § Offline
     compose), a knock has no identity until the nest assigns one — so
     there is nothing to journal until the ack resolves the effect.
  2. **The outbox never syncs; the journal exists to sync.** An intent is
     replica-local — it drains only from the device that composed it —
     while per-writer logs are the sync plane's ground shape. Never-sync
     rows on the sync substrate would need a carve-out in every walker.
  3. **Ordering differs in kind, not degree.** Intents drain per-channel
     FIFO, and a permanent failure parks *only* that channel's remainder
     (devices.md § Offline compose owns the MLS instance); the journal is
     gapless per `(scope, writer)` with completion-is-deletion semantics
     forbidden by construction. A watermark cannot express a parked
     channel; a per-intent status column can.
  4. **Wipe-tolerance polarity is opposite, and the boundary must be
     structural.** The replica is droppable because the fleet holds its
     truth; the outbox is the only copy of pending writes. Intents live
     OUTSIDE the scope-keyed planes, so T2's eviction transitions are safe
     by construction: `drop_scope` deletes by scope, the outbox is not
     scope-keyed, and a scope departure therefore *cannot* destroy an
     undrained intent — a departed channel's intents survive and park as
     user-visible failed sends at drain, never silently dropped.

  **Precision the ruling adds to "the one non-wipe-tolerant component":**
  the unpublished suffix of the device's own journal (and its loose
  blocks) is equally the only copy until own-log replay publishes it. The
  never-drop carve-out covers both — the undrained outbox and the
  unpublished own-writer suffix — and § The account store's wipe-tolerance
  sentence is scoped by this ruling.
- **Nest-side durable idempotency — BUILT 2026-08-13.** The nest's per-connection `IdempotencyCache` is
  structurally useless for offline replay (empty on every reconnect — survey
  § 3.4); the nest now records each `ok` Reply durably per
  `(actor, idempotency_key)` (`bins/fauna-nest/src/db/rpc_idempotency.rs`)
  and the per-actor dispatch sink consults the table on an LRU miss,
  rebuilding the Reply with the current correlation_id — so an outbox drain
  (or `request_auto_retry`) re-presenting its stored key after a reconnect
  gets the first outcome, never a second effect, with no client-side change
  (the envelope already carried the key). Wire semantics + the three scoping
  rules (`ok`-only, `Read`-exempt, no anonymous tier) are owned by
  [`transport.md`](transport.md) § Idempotency and reconnect-with-resume;
  retention 7 days, swept. Proven by
  `tests/e2e-unified/tests/api/test_rpc_durable_idempotency.py` (red first:
  the same key on a fresh connection minted a second invite code) + the
  sink-layer pins in `ws.rs`, all mutation-graded.
- **Per-kind offline classification (the criteria survey Q7's inventory
  applies).** Every mutation kind gets exactly one class:
  1. **offline-safe** — ID is content-addressed or client-assigned, effect is
     commutative/CAS-retry mergeable: apply locally + sync as data (posts —
     `post_id = blake3(body)`; calendar/card UIDs; settings).
  2. **offline-queued** — replayable intent with nest-assigned effects or
     side effects: apply optimistically where safe, queue the intent, resolve
     on ack (sends, knocks, subscriptions).
  3. **online-only** — genuinely connection-requiring: auth ceremonies,
     admin/provisioning, payments, irreversible external effects (mail
     submission to foreign MX). UI desensitizes these offline (BUILT on the
     lead app 2026-08-13 — § Implementation status → *Built — W4 phase 4*).
  **How a surface asks (W4 phase 4, ratified 2026-08-13).** The desensitizing
  decision is one shared function, `fauna_protocol::offline_class::affordance
  (kind, connection_state)`, taking the transport state as the same lowercase
  word `fauna_core::format::connection_state_label` takes — so all seven apps
  read one rule and none keeps a per-app list of widgets-to-grey (priority #2).
  Three rulings it makes, each chosen so the gate never *over*claims:
  1. **Only class 3 desensitizes.** Classes 1 and 2 are precisely the ones
     that work without a nest, and `Read` is not a mutation — whether a read
     is *answerable* offline is a W3 projection question this table
     deliberately declines to encode (module docs), so a read is never greyed
     on its account.
  2. **An unregistered kind stays available.** A typo must surface as a
     failing bijection test, never as a dead button in a user's hands.
  3. **Only the *known* offline words count as offline** — the opposite
     polarity to `connection_state_label`, which reads an unrecognised word as
     `disconnected`, and opposite for the same reason. For a display the
     honest weaker claim is "disconnected"; for a gate it is "do not block the
     user", so an older app meeting a future state word keeps its controls
     live and at worst shows the error it would have shown anyway.
  The reason is carried per affordance (a `LocalizedText`, `common.needs_nest`),
  never a global "you are offline" banner — the same per-kind rule R11 states
  for a nest-*less* account, which is why one mechanism covers both.
  **Reading the "irreversible external effects" example correctly:** it is
  the *nest → foreign MX* leg that is online-only, and that leg is nest-side
  — already at-least-once, and not a kind any client issues. What a client
  issues (`fauna.email.send`,
  `nostr.events.publish_signed`) is a submission to the user's **own** nest,
  which owns the durable outbound queue (`enqueue_outbound_mail` /
  `fetch_outbound_due` / `mark_outbound_*`); those are offline-**queued**
  intents. The class turns on who arbitrates the effect, not on whether the
  effect eventually leaves the nest.
  **A policy the nest enforces is written online (ruled 2026-10-04 — a session ruling, refutable by the user).** A
  document one party authors and the nest enforces on another's requests —
  the feature plane's admin and self-limit documents, and a guardian's policy
  for a ward (`fauna.family.policy.update`, which carries the guardian's
  feature limits too) — is class 3, although its write is an idempotent
  whole-document replace. Three reasons, any one sufficient. *There is nothing
  to apply locally:* the document rests only in the nest's own tables and the
  app holds a read of it, never a replica, so class 1's "apply locally + sync
  as data" has no data to sync — a class-1 entry for such a kind leaves its
  control live with no nest and able only to fail. *A queued replace is
  stale by construction:* it replays the knobs as one device last read them
  over whatever another device saved since. *A restriction must not land
  silently late:* whoever sets one needs to know it is in force, and an
  optimistic "saved" over a limit the nest has not received is a false
  assurance on exactly the surface where that matters most. Composing stays
  free — a form's buffer edits issue no kind and are never gated; only the
  save desensitizes. The ruling reclassifies one kind
  (`fauna.family.policy.update`, `OfflineSafe` since the 2026-08-13 handler
  pass, which graded replay shape alone) and is deliberately no wider: the
  other nest-resident `OfflineSafe` writes were not re-examined here. Not
  built — § Implementation status today.
  **The inventory is BUILT (2026-08-10): `libs/fauna-protocol/src/offline_class.rs`**
  — all 596 registered kinds classified (234 read, 60 offline-safe, 37
  offline-queued, 265 online-only, as of the 2026-08-13 handler-verification
  pass), as a runtime lookup rather than a doc
  table because the outbox has to *ask*. Two tests hold it: a bijection with
  `KindRegistry::full()` (a new kind cannot skip the question), and a
  consistency rule reconciling the registry's own replay audit —
  `forbid_replay = true` refutes both *read* and *offline-safe*, so such a
  kind must be offline-queued or online-only. That rule is what catches the
  read-shaped consumers (`bridges.check_submission_quota`,
  `conversations.keypackage.fetch`). The module's own header records which
  classes are mechanically enforced, which derive from the registry's audit
  trail, and which await per-handler verification at W4 build time.
- **MLS compose (constraint from survey Q8).** Offline compose for MLS
  channels queues **plaintext intents** encrypted-at-send, never pre-built
  ciphertext for a future epoch (device-owned-epoch invariant). Detail —
  the intent shape, the drain-through-the-gated-send-path rule, the
  app-only drain carve-out, per-channel FIFO + loud permanent failure, and
  the application-messages-only scope — resolved 2026-08-10 (T6), owner
  [`../behavior/devices.md`](../behavior/devices.md) § Offline compose.


## Implementation status today

*The entries below were carried verbatim out of [`account-data-plane.md`](account-data-plane.md) § Implementation status today, which stays the home of the cross-cutting entries no single plane owns.*

- **Ruled, NOT built (2026-10-04): `fauna.family.policy.update` is
  `OnlineOnly`** (§ The offline-mutation contract → *A policy the nest
  enforces is written online*). The shared table still says `OfflineSafe`, so
  on every app that declares the kind the family screen's policy save — and,
  on tui, the guardian feature-limit editor's Save and Remove — stays live
  with no nest and fails into the page's error. The build is one table entry
  plus the tests that pin the old class (tui's
  `family_declares_the_exact_kind_per_gesture` names it three times); each
  app's gate follows from the table wherever the kind is already declared.

- **Built — W4 per-kind offline classification (2026-08-10):
  `libs/fauna-protocol/src/offline_class.rs`.** The inventory half of § The
  offline-mutation contract: `OfflineClass` + `offline_class(kind)` over all
  596 registered kinds, bijection-tested against `KindRegistry::full()` and
  cross-checked against each kind's `forbid_replay`. Both tests red-verified
  by mutation. **Not built:** the outbox itself, the nest's durable
  idempotency table, and the UI desensitizing — the classification is the
  input those three consume, not a substitute for them. The `OfflineSafe` /
  `OfflineQueued` split on the user-app mutation surface is recorded as
  refinable at W4 build time (module header § Verification status) — **that
  refinement is DONE: the *Built — W4 phase 2* entry below.** **The
  outbox's schema boundary is RULED 2026-08-12** (§ The offline-mutation
  contract, the W4 phase-0 ruling): its own durable component, per-intent
  rows, `OfflineQueued` only — not journal rows. **The outbox itself is
  BUILT to that ruling — the next entry.**

- **Built — W4 phase 1 (2026-08-13): the offline outbox — the store
  component + the generic drain.** The phase-0 ruling in code. The store
  half is `libs/fauna-account-store`'s `outbox` table behind
  `StoreBackend`: per-intent rows keyed by the client-generated 16-byte
  intent id, per-scope FIFO `channel_seq` assigned at append, the
  `transfer_queue` backoff pair, completion-is-deletion — and the table is
  deliberately OUTSIDE every scope-keyed plane, so `drop_scope` (T2
  transition 3) structurally cannot reach an undrained intent. The policy
  half is `fauna_sync_engine::outbox`: `enqueue_intent` refuses any kind
  that is not `OfflineQueued` (the phase-0 boundary at the door) and mints
  the id; `drain_outbox` runs in every full pump pass right after
  `publish_pending` (the two push-our-writes-out legs; nudge passes stay
  walk-only), with `AccountStoreHandle::enqueue_intent` as the composer
  door. The wire seam is `fauna_protocol::KeyedRpcRequester` — a subtrait
  (not a defaulted method: a silent fresh-key fallback would double-apply
  the moment dedup keys on it) both real transports implement (native
  `NestClient::request_with_key`; wasm `WsRpcClient` →
  `dispatch_typed_keyed`), so a replayed intent re-presents the SAME
  envelope idempotency key on every attempt; `offline_class_keyed`
  recovers the canonical static kind name from the classification table.
  Failure policy: a transport fault records an attempt and stops the pass
  (pass cadence is the backoff); a rejection — or an unresolvable kind or
  undecodable payload — parks the intent AND its scope's remainder (FIFO
  never reorders around a failure) as durable, user-visible state.
  `IntentDrainer::Mls` rows are listed, never sent: the gated MLS send
  path (T6's consumer, `devices.md` § Offline compose) owns that drain.
  Proven: 6 store tier_1 (restart durability verbatim, idempotent
  re-append, ack-is-deletion + double-ack no-op, per-scope FIFO
  independence, park-stays + attempt counting, and the phase-0 interaction
  — a departure proceeds AND cannot reach the outbox, red-verified by
  wiring the outbox into `drop_scope`) plus 5 over the real runtime
  (drain-once carrying the stored key, the enqueue-door refusals, a
  rejection parking the scope durably across passes, a transport fault
  retrying the same key next pass, MLS held not sent — the first
  red-verified by unplugging the drain from the pump). **Not built,
  stated:** ~~delivery is at-least-once until phase 3~~ (**phase 3 landed
  2026-08-13** — the *Built — W4 phase 3* entry below; replays now collapse
  to exactly-once with no client-side change, exactly as this entry
  predicted); the MLS drainer; the failed-sends surfaces (phase 4's UI
  desensitizing landed — the entry below).

- **Built — W4 phase 3: the
  nest-side durable idempotency table.** The record lives in § The
  offline-mutation contract → *Nest-side durable idempotency* (one owner);
  wire semantics in [`transport.md`](transport.md) § Idempotency and
  reconnect-with-resume. What it closes here: the outbox's stated
  at-least-once caveat above — a drain re-presenting its stored key after a
  reconnect now gets the recorded first outcome.

- **Built — W4 phase 2 (2026-08-13): the `OfflineSafe`/`OfflineQueued`
  split is handler-verified per kind.** All 100 kinds then in the two
  classes were traced to their handlers and graded on identity assignment,
  replay effect, and per-request side effects; 16 entries moved (11
  queued→safe — among them `sync.changes.record`, whose recorded rationale
  was contradicted by its own handler's durable exactly-once content check;
  2 safe→queued, incl. `folders.set_web_paywall`'s per-call feature-usage
  spend; `filesync.snapshot.prune_set_policy` → online-only beside its twin
  `prune`; `spaces.fast_forward`/`select` → read). Each moved entry and
  each contested keep carries its evidence at the declaration site, and the
  module's § Verification status records the three criteria rulings made in
  the pass (convergent value-derived external publishes do not disqualify
  OfflineSafe; nest-side refusals never drive class; nest-arbitrated
  namespaces queue). Current split: 234 read / 60 offline-safe / 37
  offline-queued / 265 online-only. The four `fauna.spaces.*` entries are
  the recorded exception (no handler surface exists — re-verify at Slice
  2). Two handler bugs found by the pass were nest-captured and are **FIXED
  2026-08-13**: the Bluesky write-through now
  refuses to republish an already-mapped content-addressed post
  (`AlreadyCrossposted`, guarded before any network step), and
  `conversations.group.send_message`/`.react` replays converge on the
  existing `posted_to` link and reply the same post id instead of
  "storage error" (those kinds were since retired with the group plane,
  2026-09-26 — `conversation-rooms.md` § The group plane's fate).

- **Built — W4 phase 4 (2026-08-13): UI desensitizing — the shared rule, and
  the lead app consuming it.** The charter's class-3 sentence in code. The
  rule is `fauna_protocol::offline_class::affordance(kind, connection_state)`
  → `Affordance::{Available, NeedsNest}` (+ `reason()`, a `LocalizedText`),
  with the three rulings recorded in § The offline-mutation contract; it sits
  beside the classification because that is what the classification is a
  runtime lookup *for*. All seven apps already depend on `fauna-protocol`, so
  none re-derives `class == OnlineOnly`. **tui consumes it structurally, not
  per widget:** each gesture declares the wire kind it issues
  (`Action::wire_kind`, exhaustive per page and over `Gesture` itself, so a
  new page cannot skip the question), and `App::page_elements` — the ONE list
  paint, the automation registry and the focus ring all read — runs the gate
  over every element it returns, the same seam the screen-time lock uses. A
  page author writes no gate code. Proven: 5 tier_1 on the rule (both
  directions over all four classes, every offline word, both rulings 2 and 3)
  and 8 on the app-side gate (desensitize + reason, offline-capable stays
  live, connected gates nothing, every actuable role, an inert element
  untouched, two reason-precedence cases, and the state word matching the
  `connection-status` indicator's), all mutation-verified; plus walk invariant
  **I6** — "no affordance whose wire kind is `OnlineOnly` is enabled while
  there is no nest" — asserted after every step of the exhaustive and random
  walks (`apps/fauna-tui/src/walk.rs`, convention 17), red-verified by
  unplugging the gate; and walk invariant **I7** — "a declared wire kind is one
  the table knows" — which exists because `affordance` reads an unregistered
  kind as available *by design* (ruling 2), so a typo in an app's own
  declaration silently ungates that affordance and I6 cannot see it;
  red-verified by misspelling a live kind.

  **The admin plane is swept (2026-08-13), and it refines the class-3 sentence
  above.** All 102 `admin::Action` variants declare exhaustively, and the
  deployment-mutating half is `OnlineOnly` exactly as "admin/provisioning"
  predicts — the DAV and mail enables, the six policy full-PUTs, user admission
  and eviction, invite minting, bridge approval and service-user rotation, the
  nest knobs, the domain plane, the deployment-seed rotation, the factory reset.
  **But "the admin page" is not the same set as "the admin plane":** a slice of
  `admin-dns` writes the *admin's own* DNS record (`fauna.state.dns`) rather than
  deployment state — the held DNS-provider credentials, the per-domain managed
  opt-in (and its deployment-wide sweep), the auto-renew opt-out, the CNAME
  renewal delegation, and the manual-issuance breadcrumb. Those end in a
  local account-plane write with no nest round-trip (the gates pass them as
  `fauna.account.state.put`; they declared the retired `fauna.config.put`
  until the rail retired 2026-10-02 — [`config-dissolution.md`](config-dissolution.md)
  § The `__config` dissolution schedule → *The closure order*, step (6)), which this
  table classifies `OfflineSafe`, so they stay live; the ACME round-trips they drive are with the CA, not the nest. Reading
  class 3 as "every control on an admin screen" would grey them on a guess,
  which is the over-claim rulings 1–3 exist to prevent. The one gesture whose
  class depends on state — certificate issuance, managed vs. manual — is
  gateable only because the discriminant is carried on the action itself, the
  general fix for the state-dependent case.

  **Media is swept too (2026-08-13), and it is the other pole of the same
  point:** nothing on that page desensitizes. Its whole control plane is
  `fauna.media.list` + `fauna.files.versions.list` to read and
  `fauna.sync.changes.record` to write, so upload, delete and restore are all
  `OfflineSafe` — the content-addressed replayable writes class 1 names, and
  exactly what the outbox exists to carry. Its byte movement (the upload's blob
  POST, the thumbnail and handoff GETs) rides the bulk-binary `/api/v1/blob`
  carve-out, which has no wire kind, so the external-handoff gestures declare
  `None` for a reason distinct from a local gesture's — worth stating, because
  "it touches the network" and "it has a kind this table classifies" are not
  the same claim.

  **Settings is swept (2026-08-13), and it is the counterweight the class-3
  sentence most needs.** All 214 `settings::Action` variants declare
  exhaustively — the largest gesture family tui has, and the one whose page
  names most invite the wrong prior. The rail's biggest sub-pages write the
  user's **own** documents, and this table classifies every one of those
  `OfflineSafe` or `OfflineQueued`, so they stay live with no nest: the folder
  rows (`fauna.folders.{update,delete,members.*}`), the mail
  filters and inbox mode, the muted-word list, the trained-factor registry and
  its sealed models, and everything that lands in `fauna.account.state.put`
  (then `fauna.config.put`, retired 2026-10-02). The
  sharpest case is the pair that reads most like a bridge call and is neither —
  revoking a mail credential and disabling mail both only rewrite the
  account's mail custody (`fauna.state.mail`) beside the per-credential blob
  deletes, because `mail-settings.md` § Disable mail is explicit that
  one user disabling their mailbox must not flip the deployment-wide subsystem.
  The genuinely `OnlineOnly` cluster is narrow and nameable: mailbox key
  provisioning and rotation, the alias/list/spam RPCs that live on the *bridge*,
  the atproto login plane, capability mint/renew/revoke, nest pairing, the
  backup-trust revokes, the share-link create/revoke pair (a link's URL is
  revealed only after its registration succeeds — [`../behavior/share-links.md`](../behavior/share-links.md) § Flows → Create), and — until the stack was deleted 2026-08-23 —
  WireGuard registration.

  Three rulings the leg earned, each of which generalizes past this page.
  **(i) A page-entry rail row declares `None`** — it is a view switch whose only
  call is the sub-page's hydrate, which is a `Read`; desensitizing navigation
  would strand a user on whatever page they were on when the link dropped.
  **(ii) A composite ceremony declares the kind that BINDS it.** `serve_set`
  (WebDAV) and `paywall_set` each flip a cheap `OfflineSafe` flag and then run a
  leg that genuinely needs a nest; declaring the permissive leg would leave a
  control live that cannot finish, which `serve_set`'s own contract warns about
  by name. **(iii) A surface can be network-shaped and still have no kind at
  all** — every mail-export gesture declares `None` because the export seam
  issues no wire call today: each RPC resolves to a client-side `unimplemented`
  rejection naming a kind that is not registered, so declaring it would red I7.
  That is the media-page point from the other direction (there, a live byte
  path with no kind; here, a kind-shaped name with no live path).

  Three settings gestures remain open under the state-dependent case — the alias
  and list submits (create vs. update) and the alias active toggle (revoke vs.
  enable) — each two `OnlineOnly` kinds apart, with the discriminant in form
  state rather than on the action. Closing them is the `IssueDnsCert` refactor,
  the same general fix.

  **Verification for this leg is type-level, because the walks cannot reach it.**
  I6/I7 check what a page *paints*, and the settings sub-pages paint almost
  nothing on the offline fixture — their controls render from machine snapshots
  that never hydrate without a nest, so the exhaustive walk reaches barely a
  dozen of the 214. A hand-built `every_action()` corpus carries one instance of
  every variant behind an `ACTION_COUNT` ratchet, and the registration check runs
  over all of them; the class assertions name the **exact** kind, not just the
  class, because sibling kinds share `OnlineOnly` and a class-only assertion
  still passes when two arms are swapped. A further ratchet pins the undeclared
  set to the documented one, so the next sweep cannot answer a genuine mutation
  with a lazy `None`. All five were mutation-verified, and I6 was re-red-verified
  by unplugging the gate — it now names a settings element.

  **Family and backups are swept (2026-08-13), and between them they close the
  state-dependent case in both directions.** Family's 16 variants split on the
  guardian/ward asymmetry rather than on anything about the screen: composing a
  ward's reach policy is writing the guardian's own document
  (`fauna.family.policy.update`, `OfflineSafe`), pre-approving a contact and
  deciding the reach queue are replayable intents, and the guardian-enrolled
  device marker is a config write — so the whole editor stays live with no nest,
  while every gesture that **re-points guardianship** (graduate, transfer,
  transfer cancel/accept/decline) is `OnlineOnly`, because the nest arbitrates
  who a ward answers to. Backups' 28 make the same point about weight: its most
  destructive-*sounding* verbs stay live — an ordinary snapshot delete is the
  48-hour soft delete (`OfflineQueued`) and a manual snapshot is a replayable
  intent — while what genuinely needs a nest is the enrollment plane plus the
  three irreversible snapshot verbs (hard delete, prune, restore). Its sharpest
  pair is **Remove vs. Keep** on a post-succession row: Remove is
  `fauna.backup.destination.remove` (`OnlineOnly`), Keep records the verdict at
  rest via `fauna.account.state.put` (then `fauna.config.put`, retired
  2026-10-02) and must stay live — reading the adjudication pair
  as one plane would grey half of it on a guess.

  **Ruling (iv), which backups earned and which closes ruling (i)'s open case:
  when one control paints several ceremonies, the discriminant belongs on the
  gesture.** `backup-destination-add-confirm-button` runs three — the peer-nest
  enroll, the client-custodian enroll, and an edit — and they are not one call's
  variations: the first two bind on `OnlineOnly` kinds while an edit is a
  `fauna.account.state.put` rewrite of one row of the owner's own state (then
  `fauna.config.put`, retired 2026-10-02). Spanning
  *classes*, an undiscriminated variant could only answer `None`, i.e. no gate at
  all. tui therefore decides the ceremony at paint time, where both halves of the
  discriminant are already in hand, and carries it on the action
  (`backups::SubmitTarget`) — the general fix `admin::IssueDnsCert`'s
  `single_issue` first demonstrated, now stated as the rule rather than as one
  page's trick. The three settings gestures named above are the same shape left
  open only because their arms share a class, so nothing is greyed wrongly today.

  **Conversations is swept (2026-08-13), which completes the app: all 16 of
  tui's gesture families declare per-action, and `Gesture::wire_kind` has no
  unswept arm left.** The page's headline is that almost nothing on it
  desensitizes, and that is the design working rather than the sweep missing —
  a conversation is the archetypal offline surface. Every reachable send
  resolves to an `OfflineQueued` kind, so composing, sending, reacting,
  renaming, deleting and evicting a member all stay live with no nest. Exactly
  **two** gestures grey, and both leave the conversations plane to do it: the
  ⋯ menu's *Mark as spam*, which reseals the tier-1 model through the mail
  bridge's `fauna.bridges.put_spam_model` (no server-train fallback exists for
  content the nest cannot read), and the **in-place** add-participant, which
  opens by fetching the newcomer's key package.

  ⚠ **The "three classes behind one send" reading recorded here before the leg
  ran was wrong, and the correction is the leg's most useful output.**
  `fauna.conversations.group.send_message` (since retired with the group
  plane) — the `OfflineSafe` kind that reading named — was issued by **no**
  rail backend: the MLS rail sends through
  `post_app_message` → `send_on_channel` → `fauna.conversations.channel.send`,
  and tui registers exactly two send-capable rails (`FaunaMlsBackend` in
  `ConversationsSession::from_parts`, `SmtpBackend` via `register_smtp` →
  `fauna.email.send`); Bluesky and Mastodon `send` return `NotSupported` and no
  Nostr sink is wired. Both live kinds are `OfflineQueued`. **So the rail
  selects the kind, never the class** — the design latitude this leg was sized
  for did not exist, and the gate on this page was already behaving correctly
  before it landed. This is the second instance of the standing lesson: *a cross-app finding you did not run is a
  hypothesis*.

  **Ruling (v), which conversations earned: when a gesture's exact kind turns
  on a routing detail no paint can observe, declare the primary form and PIN
  the alternative's class equality with a test.** `send_on_channel` picks
  `channel.send_remote` over `channel.send` for a channel homed on another
  nest, and that home map is backend-private — so eight gestures would have had
  to answer `None` (no gate, no I7 check) over a distinction with no gate
  consequence. Declaring the local form and asserting
  `offline_class(channel.send) == offline_class(channel.send_remote)` turns the
  guess into a checked invariant: the day either is reclassified, that test reds
  and names the assumption instead of leaving a silently wrong gate. Two
  residual under-claims stay, both deliberately in the safe direction (a control
  stays live rather than greying on a guess): a *first* send on an unbootstrapped
  MLS thread runs `keypackage.fetch` + `welcome.deliver` before appending, and
  boundness is likewise backend-private.

  **Verification across the three legs is type-level for the same reason
  settings' was, plus one paint-level pin.** Each page carries an
  `every_action()` corpus behind an `ACTION_COUNT` ratchet, a registration check
  (I7 at the type level), and **exact-kind** assertions — because the walks
  reach almost nothing on these pages offline (family paints nothing at all
  without a `fauna.family.status` reply; backups' dialogs open only from loaded
  rows; conversations' gestures all live inside a thread detail). The walk
  red-verify therefore necessarily names an older page, so the backups leg adds
  `the_add_destination_confirm_is_desensitized_offline_and_says_why`, which
  drives the real `page_elements()` into a state where an `OnlineOnly`
  affordance exists and pins both halves of the contract — desensitized, and
  saying why per affordance, with a live Cancel beside it so it cannot pass by
  greying the whole dialog. It red-verifies by unplugging the gate. All 16 of
  the three legs' declaration tests were mutation-verified.

  **The two boundary faces are built (2026-08-14), so the rule reaches the
  non-Rust apps.** `fauna-ffi`'s `offline_affordance(kind, connection_state)`
  and `fauna-wasm`'s `offlineAffordance(...)` both return the shared verdict's
  two halves at once — `{ available, reason }`, mirroring `Affordance` with the
  reason a `LocalizedText` each app resolves through its own i18n runtime.
  One call rather than two, because an app's gate is a single decision point and
  asking twice is how the halves come to disagree. The UniFFI export is
  `value-format`-gated, the established treatment for a bare `fauna_core`
  `LocalizedText` crossing the boundary, so the Go mail-bridge's
  `--no-default-features` build — which has no UI and therefore no gate — needs
  no binding regen.

  **Built — the linux leg (2026-08-14), which is the reference for the
  persistent-widget-tree apps.** tui gates where it rebuilds its element list;
  GTK widgets outlive the state that gated them, so linux's seam
  (`apps/fauna-linux/src/offline_gate.rs`) is a *registry* instead of a pass: a
  page declares what a control issues once at construction
  (`declare_wire_kind`, the only contribution a page author makes),
  `set_connection_state` re-decides every live declaration from the one place
  the app turns a `WsEvent` into the state word — so the `connection-status`
  indicator and the gate cannot disagree — and each declaration watches its own
  widget's `sensitive` property, so a page enabling a control while offline is
  re-gated at once rather than at a repaint that may never come. Effective
  sensitivity is *the page's own intent AND the verdict*, which is how tui's
  "an already-disabled element keeps the page's own reason" survives in a tree
  where the release is a real event: a reconnect restores the page's intent,
  never more. **An actor change retires every declaration of the outgoing
  window** (`offline_gate::retire_outgoing_window`, from
  `actor_scope::reset_actor_scoped_state`): the authenticated window's widget
  tree survives `destroy()`, so its controls never prune themselves, and
  without the retire every link flip re-decided every window the process had
  built — measured 2026-09-21 at 2 s per connection-state message, and
  UI-thread ticks of up to 13 s ([`apps/linux.md`](apps/linux.md) § Message
  Flow).

  ⚠ **One trap generalizes to every persistent-tree app and is silent when got
  wrong** (measured on linux 2026-08-14, caught by the tests it was written
  against). Watching the property means the gate hears **its own writes**, and
  the obvious separator — a flag held across the write — does not work: GTK
  delivers `notify::sensitive` for the gate's own `set_sensitive` *after* the
  call returns, so the flag is already cleared and the handler records the
  gate's verdict as the page's intent. The control is then dead forever: the
  reconnect restores an "intent" that was the gate's own `NeedsNest`. A
  time-scoped guard cannot distinguish the echo; only the value can — remember
  what was written, consume the first notification carrying exactly that, treat
  anything else as the page. The same reasoning binds re-declaration: a second
  `declare_wire_kind` must inherit the recorded intent rather than read the
  widget, whose current value may be the retired declaration's verdict. The reason rides the widget tooltip, linux's own
  disabled-with-a-reason idiom, and only where the page left it empty. Proven by
  9 tier_1 tests on the mechanism plus the I6/I7 twins in
  `apps/fauna-linux/src/walk.rs`, asserted after every step of the existing
  sweeps and guarded against the two vacuity modes an invariant over a registry
  has (an empty registry, and a sibling test leaving the shared state
  `"connected"`). **2026-08-19: all 11 named gesture families declare** — the ten
  smaller ones (admin-users, events, search, family, media, profile, nostr,
  conversations, feed, backups, bridges) via ten parallel per-family sweeps, then
  `settings/*`, the largest — more `Some(...)` arms in tui's own `wire_kind()`
  match than all ten others combined — across mail, account/recovery,
  privacy/personalization, devices/folders/custody, atproto and
  nests/capabilities/backup. ~209 `declare_wire_kind` sites in all.

  **Three findings from that sweep outlive it, because each is a way the obvious
  transcription is wrong rather than incomplete.** (1) *The oracle can be wrong.*
  Two tui `wire_kind` arms named kinds their own actions never issue —
  `UseOtherVersion` (`fauna.sync.conflicts.resolve`, which no app calls; the
  action reaches `changes_record` via `restore_version`) and
  `AtprotoMint`/`AtprotoRevoke` (the nest capability-grant kinds, where the
  machine calls `provision_app_credential`/`revoke_app_credential`) — both fixed
  at the source, so the apps that copy tui now copy the truth. Transcribing an
  arm is not verifying it. (2) *A matching element id is not a matching gesture.*
  linux's `wg-register-button` ran `p2p.rs::start_tunnel`, a local iroh
  endpoint, where tui's registered a WireGuard peer with the nest — so it declared
  nothing, and copying tui's kind would have greyed the tunnel in exactly the
  situation it exists for. (Both surfaces changed 2026-08-23: tui's page is
  deleted with the stack, and linux's button is renamed `p2p-tunnel-toggle`. The
  finding is kept because the trap it names — same id, different gesture — is
  what makes a shared id set worth auditing per app rather than per id.) (3) *A branch inside one gesture can be the whole
  question.* `folder-location-add-button` binds locally on an own-nest set and
  asks the set's home nest first on a cross-nest one, so it declares on the
  foreign branch alone.

  **Built — the ADMIN plane (2026-08-20), closing the largest remaining
  differential.** `views/admin.rs` — **including its admin-users section**: the
  prior framing here said admin-users was already swept as one of the eleven
  families, but the code disagreed (zero declarations there before this pass;
  admit/invite-create/approve/deny/registration-save/tier-update/evict/suspend/
  cancel-eviction/make-admin/remove-admin were all live) — plus the five
  `settings/admin_*.rs` pages now declare every `OnlineOnly` kind the tui oracle
  names, cross-checked against `fauna_protocol::offline_class`'s own table (not
  assumed from the oracle's `Some(...)` shape alone) so the admin's-own-config-
  document exception stays undeclared: `fauna.account.state.put` (OfflineSafe;
  `fauna.config.put` until the rail retired 2026-10-02) covers
  the held DNS-provider credentials, the managed-domain opt-in, auto-renew, CNAME
  delegation, and the manual-issuance breadcrumb, exactly as tui's own doc
  comment specifies. One composite control (`admin-dns-cert-issue-button`)
  dispatches either `fauna.tls.publish_cert` or a config-doc write depending on
  `single_issue`, fixed per render — declared only on the OnlineOnly leg, the
  persistent-tree form of "declare the leg that binds". Two alias controls
  (`MailAliasesSubmit`, `MailAliasesToggleActive`) stay undeclared on linux
  exactly as on tui: one control, two distinct kinds, decided by state the
  button does not carry. Recovery's escrow-reseal/stolen/veto kinds, custody's
  `hosting.remove`, `admin::Action::SaveRegion`/`WithdrawRegion`, and
  `ConfirmAsKeyRotate` are *unbuilt on linux*, not undeclared. **`views/moderation.rs`'s
  legal-takedown and moderation-train controls remain undeclared** — the prior
  text here attributed them to the admin plane, but they live in a different
  file entirely; a genuine small follow-up, out of this pass's scope. The
  flagged `offline_gate.rs` limitation stands: no "undeclare" primitive for a
  persistent widget whose kind is state-dependent.

  **Built — the apple leg's seam (2026-08-16), and SwiftUI IS a third shape.**
  The question the leg was scoped to answer — is SwiftUI tui-like or linux-like
  — resolves to *neither*, for a reason that generalizes: views are values
  re-evaluated from state, so linux's problem (a widget outliving the state that
  gated it) does not exist, but there is also no single element list to gate, so
  tui's seam does not exist either. What SwiftUI has and neither other toolkit
  does is a **propagating, non-revocable environment**: `.disabled` is cumulative
  down the tree and a descendant cannot re-enable. So the apple seam is one
  modifier at the point of declaration — `.faunaGate(kind)`
  (`apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/OfflineGate.swift`), applied
  where the control already stamps its automation id — and the toolkit performs
  the propagation the other two legs implement by hand. linux's "effective
  sensitivity is the page's own intent AND the verdict" therefore holds *by
  construction* here, and the echo trap above cannot arise at all: nothing is
  observed, so there is no own-write to hear.

  **The apple-specific hazard is the automation surface, not the paint.** apple's
  driver reads a control's enabled-ness from its own registry entry, whose
  `isEnabled` predicate the call site passes by hand — the documented convention
  being to "mirror the same predicate the `.disabled(...)` modifier uses". A
  convention two expressions must obey is exactly what drifts, and the failure is
  silent in the worst direction: the driver reads a control as clickable while
  the real UI has it greyed, which reads downstream as a product bug. The gate
  closed that by construction — and **since 2026-08-17 it does so without a
  mechanism of its own**: `_AutomationRegister` reads SwiftUI's cumulative
  `\.isEnabled`, so the gate's plain `.disabled(!available)` already reaches the
  registry, and the dedicated `faunaOfflineGateEnabled` environment key it briefly
  published through is **deleted**. That generalisation is worth more than the
  special case it replaces: it answers **every** cause of disablement (a
  busy-state container, a `Form` section) rather than only this one, and the
  deletion is the proof it subsumes it. Mechanism + its ancestors-only limit:
  [`apps/apple-e2e-automation.md`](apps/apple-e2e-automation.md) § The actuation
  gate. Proven by
  `tests/e2e-unified/tests/test_offline_gate.py`: nest down ⇒ the online-only
  control desensitizes, nest back ⇒ it returns, and an offline-capable sibling
  **in the same modal at the same moment** stays live — the pairing is the
  assertion, since a blanket disable would satisfy a one-sided check while
  breaking the contract. Red-verified by unplugging the gate.

  **Built — the apple ADMIN PLANE fan-out (2026-08-16), and a check that holds
  the declarations.** All 46 committing controls across apple's eleven admin
  surfaces declare their kind — the nest knobs, the DAV and mail enables, the six
  mail-policy full-PUTs, user lifecycle and admission, invite mint/delete, bridge
  approval and service-user rotation, tiers and membership, the apex actor, the
  forwarders, and admin-dns's nest-side domain plane and cert delivery. Three
  rules the sweep applied, each of which the tui admin sweep had already earned
  and none of which is a per-app judgement: **the commit gates, not the buffer**
  (a form's fields and its reveal/cancel stay live — only the Save, the confirm
  or a dispatch-on-change toggle declares); **arming is local** (`factory-reset`,
  `rotate`, `rename` and the invite form open with no nest, their confirms do
  not); and **admin-dns's own config document is NOT the domain plane** — the
  held credentials, per-domain managed opt-in, auto-renew and CNAME delegation
  all save through `fauna.account.state.put` (`fauna.config.put` until the rail
  retired 2026-10-02), which is `OfflineSafe`, so they carry no
  gate at all. Where a control's kind genuinely turns on a discriminant, the
  view passes the SAME expression the action uses rather than guessing:
  `admin-dns-cert-issue-button` declares `fauna.tls.publish_cert` on the
  managed/delegated path and `fauna.account.state.put` on manual phase 1, and the shared
  table — never a Swift class test — decides which one stays live.

  The declarations themselves are held by **`offline-gate-check`
  (a dedicated dev-fleet checker, cheap merge tier on both merge scripts)**: every
  kind an app hands the gate is registered, and each site names at least one
  `OnlineOnly` kind. It is the string-literal apps' twin of walk invariant I7 and
  exists for the same reason: ruling 2 reads an unregistered kind as *available*,
  so a typo silently ungates the control it was written to gate and no runtime
  signal exists anywhere. Ten stdlib tests cover it (including the vacuity guard
  — an unparseable table is a hard error, never a pass), plus a live red-verify
  on a real call site. `policyGroup(saveKind:)` shows the local stand-in for
  tui's exhaustive match: the container takes the kind as a *required* parameter,
  so a seventh mail-policy group cannot be added without answering the question,
  and the checker follows the parameter to its call sites.

  **Built — the apple USER-FACING fan-out (2026-08-16), and the two poles it
  settles.** Thirty-eight further declarations take the apple leg from the admin
  plane out into the pages a non-admin actually uses: nostr (link/unlink, the
  bunker mint and revoke), the generic bridge card, bluesky (the integration-level
  rungs and the transition confirm, app-credential mint and revoke, the connected-
  app and consent verbs, the external-apps toggle, both delegation verbs), family
  (all four transfer verbs plus the graduation confirm), the feed's paywall-link
  mint, profile (subscribe, claim mint), subscription settings (unsubscribe, claim
  redeem), account settings (handle change, the Bluesky link/unlink pair), linked
  nests (pairing add/revoke, grant mint/renew/revoke, the backup-trust revoke and
  the generation restore), web settings (the subdomain toggle, the paywall mint),
  the moderation queue's correction, and mail settings' own surface (the
  disable-mail confirm, per-credential revoke, the serve-here toggle, and the
  add-credential and key-rotation confirms). The three admin-sweep rules carried over
  unchanged, which is the evidence they were never admin-specific: the commit
  gates and not the buffer (every form's fields, every reveal, and the mint and
  graduate *arming* buttons stay live), and a control whose kind turns on a
  discriminant passes the same expression its action uses.

  **The two poles are now both instantiated at PAGE scale, and that is the
  finding.** The admin sweep's pole was "almost everything gates"; media's was
  "nothing does". Whole user-facing pages sit at the media pole *by table
  verdict*, not by omission: **devices** declares nothing because
  `fauna.sync.devices.delete` is `OfflineSafe`, and **folders** declares nothing
  because its entire control plane — create, update, delete, leave, share, member
  add/remove/set-access, places, schedule, content-key — is `OfflineSafe` or
  `OfflineQueued`, its only `OnlineOnly` kinds (`members.evict`, `lease.acquire`,
  `lease.release`) having no apple control at all. The feed is the same shape one
  level down: publish, unpublish and even the post-delete confirm are
  `OfflineSafe`, so the page's single desensitizing verb is the paywall token
  **mint**, sitting beside a plain link-copy that stays live. A future session
  finding these pages bare should read that as the rule having been asked and
  answered, not as an unswept page — the answer is recorded here precisely
  because a bare page is indistinguishable from a missed one by inspection.

  **A page control can be state-dependent too, and the discriminant rule scales
  to it.** `train-correction-button` gates on a **server** row (whose correction
  reaches the nest either way — the sealed client-write path ends in
  `fauna.bridges.put_spam_model`, the fallback is `fauna.moderation.train`, both
  `OnlineOnly`) and deliberately not on a **local** row, whose primary effect is
  client-side detection removal that succeeds with no nest and whose model train
  is best-effort by construction. The declaration rides the same `QueueRowSource`
  branch the view model itself takes. Gating the control flat would have greyed a
  verb that works — the over-claim rulings 1–3 exist to prevent — which is why
  the admin plane's cert-issue precedent is stated as a general rule rather than
  a DNS quirk.

  **Built — the apple MAIL SUB-PAGES + EVENTS fan-out (2026-08-17), and the
  discriminant rule closing two questions tui left open.** The two pages the
  previous pass named as real-work-not-empty are swept: mail's aliases, lists,
  list members and spam, and events across the shared card and both shells. The
  authoritative gesture→kind map was read off tui's `Action::wire_kind` and the
  apple call path verified to reach the same shared fn in every case (apple
  dispatches `MailAliasesAction`/`MailListsAction` into the same machine, whose
  `…Nest` seam trait documents each method's kind 1:1) — the route the admin and
  user-facing passes proved, now run a third time without a single mismatch.

  What is new here is that **two gestures tui leaves `None` are closed on
  apple**, and the reason is a property of the toolkit rather than a per-app
  deviation. tui's `MailAliasesSubmit` and `MailAliasesToggleActive` carry no
  mode on the action — the add-vs-edit mode lives in the form's state and the
  toggle's direction in the row's — so tui can only answer one of two kinds, and
  correctly declines to guess. The SwiftUI call site has both discriminants in
  hand at the point of declaration, so it passes the *same* expression the action
  itself takes: `isEditing ? update_account_alias : create_account_alias`, and
  `alias.disabled ? enable_account_alias : revoke_account_alias` (the lists
  submit is the first case one level over). Both arms are `OnlineOnly`, so no
  *behaviour* differs from leaving them undeclared — but the contract is the
  exact kind, and a later reclassification of one arm now reaches these controls
  for free instead of silently making a coin-flip wrong. This is the cert-issue
  precedent generalized: where a view can observe the discriminant, it declares
  through it; where it cannot, it declares nothing rather than guessing.

  **The system-presentation confirm is a class, not a one-off.** The
  delete-account `.alert` was recorded below as a single ungateable instance; the
  spam page's model-reset is a second, in a `.confirmationDialog`. On that page
  the opener is therefore gated rather than left local — the one deliberate
  departure from "arming is local", because the confirm it arms cannot itself be
  reached and opening a destructive confirm whose only button is doomed is worse
  than greying the opener. It claims nothing false (`reset_spam_model` cannot
  happen with no nest either way), and it moves down one level to the ordinary
  shape the moment the confirm becomes an inline overlay, as
  `mail-settings-disable-confirm` already is. The events page needed no such
  departure: its whole navigating half — panning the range, switching view mode,
  drilling into a day, opening a compose, and the reminder offset `<select>`
  whose draft applies on Set — issues nothing and stays live beside the gated
  writers, so the page carries its own live-beside-dead pairing, as the spam page
  does with report-sharing (`fauna.moderation.report_share.set`, `OfflineSafe`)
  sitting beside the baseline toggle.

  **The four remaining pages are now GRADED, and three of them owe nothing.**
  **media** sits at its own pole by table verdict: all three writes — upload,
  delete, restore — record one sync-change row through `fauna.sync.changes.record`
  (`OfflineSafe`), its detail reads are `Read`, and the external handoff's bytes
  ride the bulk-binary blob carve-out, which has no kind to classify at all. So
  media declares nothing, and it is the *original* instance of the pole this
  section names. **recovery** grades empty on apple for a different reason worth
  distinguishing: its five ceremonies are all `OnlineOnly`
  (`recovery.registration.submit`, `replacement.request`, `escrow.put`,
  `succession.submit`, `replacement.veto` — only the sweep-retry's
  `conversations.channel.send` is `OfflineQueued`), but **apple renders no
  settings-level recovery ceremony surface**; the only `recovery-kit-*` controls
  it paints are onboarding steps. Nothing is owed because there is no control,
  not because the table says live — so this one re-opens the day the surface is
  built. **onboarding** declares nothing by construction: the gate's premise is a
  connection state read from an established client, and onboarding is the flow
  that establishes one — gating its commits would grey exactly the controls whose
  purpose is to reach a nest, the over-claim rulings 1–3 in their sharpest form.
  **backups/restore** is the one that grades as real work, and it was already
  half-swept: the destination plane's enroll and deregister confirms gate
  (`backup.destination.register`, `backup.destination.remove`) while the row edit
  stays live on `fauna.account.state.put` (`fauna.config.put` until the rail
  retired 2026-10-02). The snapshot half is added here — the
  immediate-delete modal's confirm (`filesync.snapshot.delete_immediate`) and the
  prune preview (`prune_set_policy`, whose dry run has nothing to dry-run
  offline) — beside a create, a delete and an undelete that stay live because the
  shared table makes them `OfflineQueued`/`OfflineSafe`, and an integrity check
  that is a `Read`. It is the cleanest pairing page in the app: five live
  controls and two dead ones, all on one screen, decided entirely by the table.

  **Built — the system-presentation confirm class is DISSOLVED on apple
  (2026-08-17), and it was hiding a false declaration.** The class named in the
  pass above is closed at both the instances that owed a gate, and the move
  turned up something the previous passes had graded as *done*: the
  factory-reset confirm had been carrying `.faunaGate("fauna.admin.factory_reset")`
  **inside** its `.alert` builder since the admin-plane sweep. That declaration
  reads as correct, satisfies the kind checker, and can never desensitize
  anything — `.faunaGate` resolves `FaunaClient` from the SwiftUI environment,
  an `.alert`'s content is a separate presentation context that does not
  reliably carry it, and a missing client is deliberately read as an *unknown*
  connection word, which **ruling 3 answers `available`**. The over-claim
  protection that keeps a control live when the state is unknown is exactly what
  makes a mis-placed declaration silent. **A false declaration is worse than a
  missing one**: it also stops the next author looking.

  Both confirms are now inline overlays, where the environment simply reaches
  them, and both consume ids ui.yaml had **already declared**, so neither needed
  rule-A approval:

  - **`admin-factory-reset-confirm-button`** moves out of the `.alert` and its
    gate becomes real; `admin-factory-reset-cancel-button` (declared, and with
    no id at all inside the alert) exists on apple for the first time. The shape
    now matches web's own inline confirm.
  - **Account deletion adopts the declared type-to-confirm idiom**
    (`settings-delete-confirm-field`), which web/windows/tui already ship, so
    the fix *resolves* a recorded divergence instead of adding one
    ([`../ui/settings.md`](../ui/settings.md) § User actions). There is no
    opener left to arm: `settings-delete-account-button` **is** the commit, so
    it declares `fauna.account.delete` under rule 1, while the field beside it
    is buffer and stays typeable with no nest. apple thereby inherits
    `tests/e2e-unified/tests/test_delete_account.py` with no apple-specific
    change — the action layer is id-driven — which is the second half of the
    point: the app's most destructive gesture had no e2e at all while it lived
    in an alert.

  **The shape is now forbidden, not merely discouraged.** The offline-gate-kinds checker
  gained **rule 3** — no declaration inside a system-presentation content
  builder — red-verified against the pre-change `AdminNestView` (it flags the
  exact `.faunaGate` line, and passes once promoted). This is the mechanizable
  half of the forcing function the entry below still calls missing: source
  cannot say which kind a button *eventually* issues, but it can say perfectly
  well when a declaration sits somewhere its verdict cannot arrive. ~~The one
  remaining member of the class owes no gate move: the spam page's model-reset
  keeps its opener-gated departure until its confirm is promoted too, which
  needs a **new** element id and is therefore approval-gated.~~ Resolved below.

  **Built — the system-presentation confirm class is now EMPTY on apple
  (2026-09-25).** The spam page's
  model-reset was the class's last live member (`MailSpamView.swift`'s
  `.confirmationDialog`, flagged above). The fix needed **no new element id**:
  `mail-spam.md`'s own outcome 10 already specifies the mechanism ("the first
  press only arms ... the second press resets"), which is the same two-click
  inline arm/relabel-on-the-same-button idiom `mail-aliases-list-item-overflow-
  menu` and `mail-lists-list-item-delete-button` already use on apple (and
  every other app already uses for this exact spam-reset control) — not the
  separate-button overlay `mail-settings-disable-confirm` uses for a different
  gesture. `MailSpamView.swift` now reuses the shared `tapArmedDelete` helper
  on `mail-spam-reset-model-button` itself; the button's `.automationActivate`
  gained the `text:` closure the two-click delete buttons had been missing
  (`/element/text` silently falls back to `""` without one — a real,
  previously-untested gap this row's e2e coverage caught).
  `.faunaGate("fauna.bridges.reset_spam_model")` stays on the same
  arm-and-confirm control (never split, exactly as the delete buttons'
  "arming a confirm that cannot fire is worse than not arming it" already
  reasons) — no opener/confirm split exists on this page any more than it
  ever did on the delete buttons. The class's whole point was that it had more
  members than the one that found it — it now has none.

  **Built — the gate's effect is now measured at FRAME scope, not only on
  hand-picked controls (2026-08-17).** apple's automation server gained
  `GET /registry` (`AutomationRegistry.snapshot()`): one record per visible
  element with its `enabled`, `declares_enabled`, `actuable`, `editable`, scope
  and frame — the structured surface `driver.tree()`'s text dump could not
  serve, and the thing whose absence had kept any whole-frame reasoning about the
  gate out of reach. `helpers/registry_audit.py` turns two such frames (nest up,
  nest down, same page) into two assertions, and
  `test_the_admin_plane_desensitizes_with_no_nest` now makes both beside its two
  per-control ones: **monotonicity** — losing the nest may never *enable* a
  control, since `.disabled(!available)` only ever subtracts, which catches an
  inverted verdict or an enablement wired to the wrong half of the connection
  state — and **reach** — at least one control must actually have changed, which
  catches a gate that no-ops (a per-control assertion passes against that state
  whenever the control it picked is disabled for its own reasons). Rulings pinned
  at tier_1 in `tests/e2e-unified/tests/test_registry_audit.py`, each next to the
  legitimate case one letter away from it.

  ⚠ **The obvious phrasing was deliberately NOT built, and a future session
  should not "finish" it.** The tracked idea was *"on an admin page with no nest,
  every actuable control outside a named allow-list is disabled"*. That
  allow-list is a page's tabs, back button, text fields, cancel buttons and every
  local toggle — most of the page, churning on every UI change — which is the
  `focus-fanout == 1` mistake in a new costume: a checker that big is declared
  noise and deleted (`e2e-conventions.md` § convention 17's own adoption
  discipline). The differential needs no list at all and is true by construction
  of what a gate is.

  **Built — the web leg (2026-08-17), and a Svelte tree is a FOURTH shape.**
  Like SwiftUI it re-evaluates from state, so linux's problem (a widget
  outliving the state that gated it) does not arise; unlike SwiftUI it has no
  propagating `.disabled` environment, and unlike tui no single place that
  builds actuable elements — the SPA's controls are hand-written `<button>`
  markup across ~20 route files and ~36 components. **The question the leg was
  scoped to answer — does the SPA have one such place — is therefore answered
  no**, and inventing one (a `<GatedButton>` wrapper every call site routes
  through) would be a per-app component layer the other six apps do not have.
  So the seam is a **Svelte action that takes the call site's own predicate as
  a parameter** (`apps/fauna-web/src/lib/offline-gate.ts`):
  `use:offlineGate={{ kind, disabled }}`. The call site hands over the
  `disabled=` it would otherwise have written, which makes the action that
  property's **single writer** — so linux's echo trap (a `true` the gate wrote
  and a `true` the page wrote being indistinguishable by value) cannot arise
  here at all: the second writer was removed rather than arbitrated with, which
  a `MutationObserver` registry would have had to do with none of linux's
  ordering to lean on. linux's "effective sensitivity is the page's own intent
  AND the verdict" is then explicit rather than reconstructed.
  **The web-specific hazard is initialization order, not the paint.** The
  verdict runs in wasm, which the root layout initializes asynchronously, so a
  control rendered before `ensureWasm()` resolves has no verdict to read; the
  gate **fails open** there, the polarity ruling 3 chose for an unrecognised
  state word and for the same reason. The reason rides the control's `title`
  (web's disabled-with-a-reason idiom, the analogue of linux's tooltip and
  apple's `.help`) and only where the call site left it empty, so a more
  specific page reason is never clobbered — and is withdrawn on reconnect only
  if the gate is what wrote it. Eight controls declare: the `admin-nest`
  deployment-mutating family (pairing, serving port, NAT mode, host restart,
  seed-rotate confirm, factory-reset confirm), the backups remove confirm, and
  the Nests add submit. The opener/confirm split is web's own: unlike linux —
  whose confirm lives in a transient dialog it cannot register, so its *opener*
  declares — web's confirms are ordinary markup in the same tree, so the
  openers declare nothing and stay live, which is what the shared e2e asserts.
  **`admin-nest-seed-rotate-button` is the sharp case and it declares nothing
  deliberately:** it does issue a call, but `fauna.admin.admins.list` is a
  `Read`, which ruling 1 never greys — declaring it would gate nothing while
  reading as proof the control is gated, exactly the shape
  `check-offline-gate-kinds.py` rule 2 forbids. Proven: 11 tier_1 tests on the
  composition rule (`offline-gate.test.ts` — both directions, the call site's
  predicate winning, a reconnect restoring intent rather than blanket-enabling,
  fail-open, title ownership in both directions, subscription release), each
  mutation-verified against four mutations of the rule; the four shared
  `test_offline_gate.py` cases now marked `web`; and
  `check-offline-gate-kinds.py` extended with a web surface (the Svelte action's
  brace-delimited, single-quoted object literal), red-verified on both its rules
  against a misspelled kind and a `Read`-only declaration.

  **Built — the android leg's seam (2026-08-17), and Compose is web's shape
  reached from the other side.** The question the leg was scoped to answer —
  is Compose tui-like or linux-like — resolves the way SwiftUI's did, to
  *neither*: composables are re-evaluated from state, so linux's stale-widget
  problem cannot arise, and there is no single element list to gate. But
  Compose also lacks the one thing that made apple's seam a modifier: it has
  **no propagating disable**. `enabled` is an ordinary parameter of
  `Button`/`TextField`/…, not an environment an ancestor can set, and
  `Modifier.semantics { disabled() }` marks a node for accessibility without
  greying it or blocking its click. So there is nothing for a modifier to wrap,
  and the verdict has to reach the `enabled =` argument itself. That lands
  android on **web's seam — the call site hands over its own predicate and the
  gate composes it** — arrived at from the opposite direction (web because a
  registry would be fought by the framework; android because there is no
  interception point at all). The shape is a `@Composable` returning the
  verdict, `faunaGate(kind, enabled)` →
  `FaunaGateVerdict { enabled, reason }`
  (`apps/fauna-android/app/src/main/java/com/fauna/app/ui/util/OfflineGate.kt`),
  matching this app's existing `localized()` idiom; the returned boolean is
  already `the page's own intent AND the verdict`, so no call site writes
  `if (!available)` and linux's "never enables what the page disabled" holds by
  construction. The transport word comes from one `LocalConnectionState`
  provided at the shell off the **same** view model the `connection-status`
  indicator reads, so the two cannot disagree; its default is `null` = *unknown*
  rather than `connecting`, apple's missing-client reasoning exactly — a known
  offline word there would grey controls in a `@Preview` or before the shell
  mounts, while the nest is reachable.
  **android answers the visible-reason question the apple leg left open, and
  the answer needed no new element id.** apple recorded a *visible* iOS reason
  as still owed because it would need a `ui.yaml` id (approval-gated) and
  would restructure view hierarchies — and named android as facing the
  identical choice on a touch target with no hover. It does not: this app
  already has `DisabledControlReasonText`, deliberately **un-id'd chrome**
  (no `testTag`) built for [`../ui/README.md`](../ui/README.md)
  § Copy comprehensibility rule 5 — "every disabled control the user can see
  has an on-screen reason within eyeshot" — so the gate's reason renders
  visibly beside the control with no id-level change at all. That is worth
  reading as a general finding rather than an android quirk: the blocker apple
  identified was the *addressability* of the reason, not its visibility, and an
  un-id'd caption sidesteps it on any app whose harness reads text. Whether iOS
  adopts the same shape is apple's call.
  **apple ADOPTED it, 2026-08-23 — and the transfer is partial in a way worth
  recording.** `FaunaOfflineReason` (`FaunaKit/Core/OfflineGate.swift`) is the
  apple twin of `DisabledControlReasonText`: un-id'd chrome, no `ui.yaml` id, no
  rule-A approval, rendering on **both** targets from one FaunaKit view. It is a
  **sibling view the author places**, not part of `.faunaGate(_:)`, because the
  apple row's *second* premise was never refuted — folding a caption into the
  modifier would wrap every gated control and restructure hierarchies that
  existing `.accessibilityElement(children: .contain)` scoping depends on —
  and because placement is a judgement only the author holds: rule 5 asks for a
  reason *within eyeshot*, not one caption per control, and a caption under a
  button row reads as a statement about the destructive action rather than
  about the cancel button beside it. ⚠ **The android finding transfers on the
  APPROVAL axis and NOT on the VERIFICATION axis, and a leg that assumes
  otherwise will ship an untested mechanism.** android's harness reads arbitrary
  rendered text; apple's driver resolves elements through `AutomationRegistry`,
  which holds **registered ids only**, so un-id'd chrome is invisible to every
  apple driver-level test. apple therefore splits the mile rather than accepting
  a "you have to look at it" excuse: the caption's entire decision is a pure
  function (`FaunaOfflineReason.captionText(kind:connectionState:)`) pinned by
  `OfflineReasonCaptionTests` — captioned **iff** gated for every connection
  word the transport can report, nothing on an unknown word (ruling 3 made
  visible: a caption is a claim to the user that they are offline, so an
  environment miss must never produce one), nothing on an unregistered kind
  (ruling 2) — leaving the human only whether it *looks* right. **The kind is
  written twice** (once on the gate, once on the caption) and that drift is
  checked, not trusted: `check-offline-gate-kinds.py` gained **rule 5** —
  an unregistered caption kind, a caption whose kind no control gates, and a
  caption inside a system-presentation builder are all errors. ⚠ Rule 5's scan
  deliberately does **not** feed rule 4's `declared` set (a lone caption must
  never answer the coverage differential for a control carrying no gate); the
  cheap implementation — one more `open_call` — has exactly that bug, and a
  tier_1 test pins the separation. Red-verified in all three directions against
  the real tree, and 7 new checker self-tests (33 → 40).
  Declared so far: the backups remove confirm — the same pairing the shared
  e2e reads on the other apps, its cancel sibling deliberately declaring
  nothing and staying live. Proven by 9 Robolectric tests
  (`ui/util/OfflineGateTest.kt`) calling the **real** shared rule over host JNA
  rather than a Kotlin stand-in: both directions, the live-sibling pairing,
  `connecting` gating while an unknown state does not, the page's intent
  winning in both value and reason, and the production `RemoveConfirm` driven
  through `BackupDestinationsContent` with its click proven not to fire.
  **Red-verified by unplugging the gate** (forcing `gated = false`), and the
  split is the evidence rather than the count: **exactly the 4 desensitizing
  cases failed and exactly the 5 live-direction cases passed**, which is what
  separates "these tests watch the gate" from "these tests watch a
  blanket-disable that happens to agree with it" — a suite where all 9 went red
  would have been asserting the weaker property. Run with
  `just android-host-test`, the only path that supplies the host `.so` these
  need.
  `check-offline-gate-kinds.py` gained an android surface, which needed two new
  narrowing mechanisms because **Kotlin spells a call and a definition the
  same** (`faunaGate(` matches `fun faunaGate(kind: String, …)`, where Swift's
  `.faunaGate(` and Svelte's `use:offlineGate={` are self-separating): an
  `excludes` for the gate's own definition file, and a `require_fragment`
  pinning the scan to the app's package so android's **21 MB of checked-in
  UniFFI Kotlin across four flavors** is not walked (4.8s → 1.7s on the cheap
  tier). Both narrow the scan, so both carry the table's own vacuity guard —
  a stale exclusion or a moved package fails loudly instead of scanning
  nothing — under 8 new stdlib tests.
  **Built — the android ADMIN-NEST fan-out (2026-08-19).** Five more
  declarations, matching the set web declared on the same page, and the three
  rules the apple sweep earned carried over unchanged — further evidence they
  were never apple-specific. *The commit gates, not the buffer:* the serving-port
  input and both NAT radios stay live, so an admin can still compose a change
  offline. *Arming is local:* the factory-reset opener stays live, its confirm
  does not. *A dispatch-on-change toggle IS the commit,* so `admin-service-
  pairing-toggle` declares `fauna.admin.services.update` with no Save to carry
  it. The other three are the serving-port save, the NAT-mode save and the OS
  restart-now button. **Six, once the seed-rotate section arrived mid-flight.**
  That surface — the android leg — landed between this
  session's two batches, so its confirm is declared here too
  (`fauna.admin.deployment_seed.rotate`) while its ARM button deliberately
  declares nothing: arming resolves the inheritor roster through
  `fauna.admin.admins.list`, a `Read` that ruling 1 never greys, so declaring it
  would gate nothing while reading as proof the control is gated. web reached
  the same conclusion on the same section independently, which is the useful
  part — the arm-vs-confirm split is a property of the surface, not of a
  toolkit.
  **Compose dialogs are NOT apple's rule-3 hazard, and that is now measured.**
  The android surface in `check-offline-gate-kinds.py` deliberately carries no
  `presentation_calls`, which is only sound if a gate declared inside a dialog
  slot still sees the state. A test declares one inside a Material `AlertDialog`
  `confirmButton` and reads it desensitized — a Compose dialog's slots are
  ordinary composable lambdas whose sub-composition inherits CompositionLocals
  from where the dialog is declared, unlike SwiftUI's separate presentation
  context. Production still declares just outside the slot, for an unrelated
  reason: the dialog BODY is where there is room for the reason.
  16 Robolectric tests, red-verified again at the new size — exactly the 7
  desensitizing cases red, exactly the 9 live-direction cases green.
  ⚠ **A measurement trap this batch paid for:** with no nest, *every* gated
  control on a page renders the same reason string, so `onNodeWithText(reason)`
  is ambiguous and fails with "found 5 nodes" — which reads exactly like the
  reason being **absent** and invites a wrong diagnosis. Assert the count (tied
  to the page's own declaration list) or the delta across an interaction, never
  the bare node.

  **Built — the android LINKED-NESTS fan-out (2026-08-19).** Six more
  declarations, taking the page to apple's set bar one: the add submit
  (`fauna.pair.add`), the per-row unlink (`fauna.pair.revoke`), grant renew and
  revoke, the grant mint confirm, and the backup revoke. The last is android's
  first **discriminant** site and it follows the rule rather than re-deriving
  it: the row's `TrustBackupKind` decides between
  `fauna.backup.nest_key.revoke` and `fauna.backup.writer_grant.revoke`, so the
  gate is handed the *same* `when` the action takes — one expression, not two
  that can drift — and the shared table decides. Arming stays local
  throughout (the add-form opener and the mint opener stay live; their confirms
  do not), and the add form's input and cancel stay live beside a dead submit.
  **`fauna.backup.generation.restore` has no android control at all**, which is
  why the page stops one short of apple's seven — an absence, not a miss.
  ⚠ **The add-submit test types into the input first, and that is load-bearing**
  — the trap records from web's leg. That submit carries
  its own non-empty-input predicate, so on an untouched form it is disabled for
  a reason unrelated to the gate and the assertion would pass against an app
  with **no gate at all**. A third case pins the converse (empty form, nest
  present ⇒ still dead, and the gate stays silent because the page owns that
  refusal), so the pair can distinguish "the gate works" from "the gate took
  over `enabled`". 19 tests, red-verified — and that converse case is one of the
  **survivors** under the unplugged gate, which is what makes the discrimination
  real rather than asserted.

  ⚠ **android's e2e cannot witness this leg, twice over**, so no marker was
  added to `test_offline_gate.py` — and the second block is worse than "not
  yet supported". Beyond the host emulator gate that covers all android e2e,
  android's bridge serves no `/element/enabled` at all
  (`BridgeHttpServer.kt` has `/element/{text,visible,count,click,type,clear,
  select}` and `/session`) — while
  `HttpBridgeDriver.is_enabled` swallows the resulting 404 and returns
  **False**. So on android that read is False for *every* element, always, and
  the failure is asymmetric in the dangerous direction: a
  `assert not is_enabled(...)` passes **vacuously** while its positive twin
  fails. A marker would therefore have produced a half-green file reporting as
  coverage while asserting nothing, which convention 7 forbids. The same
  measurement corrected `is_disabled`'s docstring, which had listed Android
  among the apps surfacing through that contract.

  **Built — the android USER-FACING fan-out (2026-08-19), and it is the first
  leg driven by RULE 4 rather than by walking pages.** Ten more declarations
  across account settings, nostr and the profile/subscription surfaces:
  `fauna.profile.handle.change` and `fauna.account.delete`;
  `fauna.nostr.bunker.create_invite` and `.revoke`;
  `fauna.subscriptions.subscribe` and `.unsubscribe`;
  `fauna.payments.claims.mint`, `.redeem`, `fauna.payments.providers.set` and
  `.remove`. The three inherited rules held again without amendment (the commit
  gates not the buffer; arming is local; a control with no separate confirm IS
  the commit).
  **What changed is how the work was FOUND.** Every android batch before this
  one walked a page and asked which controls mutate. This one ran the rule-4
  oracle differential first, which named **110 OnlineOnly kinds android declared
  nowhere** — the actual remaining inventory, in one list, before a single file
  was opened. Closing ten of them took the count to 100, which is also the
  measurement that the differential is exact rather than indicative. A leg that
  starts here does not have to guess what is left; ordering the work is now the
  only open question, and the residue is dominated by `fauna.bridges.*` (56) and
  `fauna.admin.*` (17), i.e. the mail/bridge admin planes.
  **The shared table supplies this batch's live siblings, which is stronger than
  choosing them.** On the profile tiers tab
  `fauna.subscriptions.tiers.{create,update,delete}` are **OfflineSafe** and
  `fauna.subscriptions.requests.approve` is **OfflineQueued**, so ruling 1
  requires those controls to stay live beside the gated payments ones. A blanket
  disable therefore fails the same test that proves the gate — the pairing is
  enforced by the classification, not by this leg's judgement.
  ⚠ **Two of the ten needed a seam that did not exist — since closed.**
  `AccountSettingsScreen` was the one settings screen with no `*Content` split:
  it needs a Hilt VM, a `NavController` and a `FragmentActivity` (BiometricPrompt
  re-auth), so a gate declared inside it could not be proven at all. Rather than
  leave two declarations untested — the "this needs a human/emulator" shape the
  project's authoring rules forbid — the two gated regions were extracted first
  as `ChangeHandleCard` and `DeleteAccountConfirmDialog`, plain-parameter
  composables the Robolectric harness renders directly; that was the minimum to
  make the gates provable, not the uniform end state. **The whole-page split
  landed 2026-08-22**: `AccountSettingsContent` now
  takes plain parameters + callbacks like every sibling settings screen, with
  `ChangeHandleCard`/`DeleteAccountConfirmDialog` folded in as its public
  sub-sections (kept public, not private, so their own existing `OfflineGateTest`
  cases stay drivable at their original precision alongside the ones that now
  render the full page).
  ⚠ **ui.yaml said android's delete confirm was a native OS alert. It is not.**
  The `settings-delete-confirm-field` note listed linux and android together as
  confirming "behind a native OS Yes/No alert with no drivable id", and argued
  from that that the confirm "could carry neither a drivable id nor the offline
  gate's environment". True of linux; **false of android**, whose dialog is a
  Compose `AlertDialog` — ordinary composition, inheriting `LocalConnectionState`
  like any other node. The note was corrected in the same commit. What android
  actually lacks is the type-to-confirm FIELD and a ui.yaml id on the confirm
  button (its proof asserts by label, since inventing an id would be an
  unapproved element); neither is blocked by the shape the old note claimed. The
  general lesson is the one the apple sweep already recorded from the other
  direction: **a recorded per-app impossibility is a claim, and it ages** — this
  one had been true of a dialog android no longer had.
  19 more Robolectric tests (38 in the file), red-verified at the new size:
  with the gate unplugged, **exactly the 8 desensitizing cases of the 19 went
  red and the 11 live-direction cases stayed green**. The three *converse* cases
  are the ones worth naming — an empty handle buffer, and a claim-mint / provider-save
  with no tiers created — because each asserts a control is dead while
  CONNECTED, for the page's own reason, and each **survived** the unplug. That
  survival is what makes the pairs discriminating: without them the batch could
  not tell a working gate from one that had simply taken over `enabled`.

  **Built — the android FAMILY / WEB / MODERATION fan-out (2026-08-19).** Nine
  more declarations, again taken off the rule-4 differential rather than a page
  walk: `fauna.family.{graduate,transfer,transfer.accept,transfer.cancel,
  transfer.decline}`, `fauna.web.{set_subdomain_enabled,paywall.mint_token,
  set_apex_actor}`, and `fauna.moderation.train`. **This is the batch where the
  fan-out's convenient heuristics stop working, and all three failures are worth
  carrying to the remaining legs:**
  - **A "no" can be a commit.** Every earlier batch's cancel was the live
    sibling, so "cancel-shaped ⇒ local" had become a working rule of thumb. It is
    wrong here: declining a guardianship transfer tells the *proposing* nest the
    offer was refused (`fauna.family.transfer.decline`), so it needs a nest
    exactly as much as the accept does. Cancelling a pending proposal is the
    same. Ask what the control *tells the nest*, never what it looks like.
  - **A "copy link" can be a commit.** `web-published-post-copy-paywall-link-
    button` reads as a clipboard action and is one, eventually — but it first
    calls `paywallMintToken` and mints a capability token on the nest. The
    plain copy-web-link beside it really is local (it formats a known origin).
    This is the mirror of the trap the apple sweep recorded: there the *issuer's*
    name understated the user's gesture; here the *control's* name understates
    what it issues.
  - **One control can be two gestures.** `train-correction-button` gates per ROW:
    a local detection's correction is client-side only (the content is MLS-sealed
    and the nest cannot read it), a server row's is `fauna.moderation.train`. The
    gate is handed the same `row.source == LOCAL` the action takes, and the test
    renders both rows in one queue so the difference is measured rather than
    argued — a blanket disable fails it, and so would a gate that ignored the
    discriminant.
  ⚠ **`fauna.moderation.legal_takedown` is NOT an absence — it is a page-parity
  gap.** ui.yaml declares a full `admin-nest-takedown-*` console (arm →
  kind/restore form → confirm, dispatching the kind), android's generated `Ids`
  carry all of those constants, and no android composable renders any of them.
  So this one does not belong in an `absences` ledger claiming android renders no
  control for the gesture; it belongs behind the missing section, and the gate
  lands with it — the same disposition apple recorded for `fauna.admin.region.set`.
  A leg grading its own ledger should check for exactly this shape: **generated
  ids with no render site mean the SECTION is missing, not the gesture.**
  ⚠ **`ModerationQueueScreen` had no `*Content` split** (the second such screen
  this session, after `AccountSettingsScreen`), so the discriminant above could
  not be rendered under Robolectric at all. Its body was a pure function of
  `queue`/`isLoading` already, so the split is a small no-op refactor — unlike
  the account page, which still owes its own.
  14 more Robolectric tests (52 in the file, 52/52 green), red-verified with a
  clean 7/7 split: the 7 desensitizing cases went red under an unplugged gate and
  the 7 live-direction cases stayed green — including
  `thePaywallCopyStaysDeadWithoutAnOriginEvenConnected`, the converse that pins
  the gate as having composed with that button's own `origin != null` predicate
  rather than replaced it. Across the whole file the unplug reds 23 of 52.

  **Built — the android BACKUPS/SNAPSHOTS fan-out (2026-08-19), which closes the
  declarable remainder outside admin and bridges.** Three declarations —
  `fauna.backup.destination.register`, `fauna.filesync.snapshot.prune_set_policy`,
  `fauna.filesync.snapshot.delete_immediate` — and they are three because a
  **census** of the 35 non-`bridges` kinds found only three with an android
  control behind them. That census is the durable result of this pass and is
  recorded in full: ~14 kinds are page-parity gaps
  where the *section* is unbuilt (plus the five
  recovery ones owned by the cross-app Recovery Kit rows), 2 are background
  machinery that is not a user gesture, and admin (17) + bridges (58) are
  un-censused. **A remaining-kind count is therefore not a count of gate work**,
  and the earlier framing of this row's remainder as such was misleading.
  ⚠ **The destination register is NOT a discriminant, and checking mattered.**
  The confirm serves two visibly different forms (a nest destination and a
  custodian enrollment, chosen by a `custodian && editing == null` branch in its
  own click handler), which is exactly the shape that earned a discriminant
  declaration on linked-nests. But both paths bottom out in
  `destination_register_custodian` / the nest twin, and **both request
  `KIND_DESTINATION_REGISTER`** — one kind, so one declaration. The lesson
  generalises: a branch in the CLICK HANDLER is not evidence of a discriminant;
  follow it to the `request(...)` before splitting a declaration.
  ⚠ **The prune execute is conditionally RENDERED, not merely disabled** (policy
  APPLIED ∧ candidates non-empty ∧ not busy). A test that does not seed all three
  fails with "found no node", which reads like a gate bug and is not one.
  ⚠ **The immediate-delete confirm is the sharpest composition case in the whole
  fan-out.** Its friction bar is an architectural rule (`backups.md` § User
  actions, rule 4: never a one-click affordance), and its `enabled` comes from the
  MACHINE's predicate over the two typed confirmations. A gate that *replaced*
  that predicate instead of composing with it would turn the most destructive
  affordance in the app into a one-click button the moment a nest appeared. The
  test that pins this — friction bar unsatisfied, nest present, still dead —
  **survives the red-verify unplug**, which is what makes it a real guard rather
  than a restatement of the gate.
  `ImmediateDeleteConfirmModal` was split out of `SnapshotListScreen` to make
  that provable — the third such extraction this session, after
  `AccountSettingsScreen`'s two regions and `ModerationQueueContent`.
  8 more Robolectric tests (60 in the file, 60/60 green), red-verified 3 red /
  5 green; across the whole file the unplug reds 26 of 60.

  **Built — the android ADMIN fan-out (2026-08-20), which closes the admin
  plane.** Fifteen declarations, and the ratio is the finding: a census of the
  16 undeclared `fauna.admin.*` kinds found **15 of them already had an android
  control rendering**, on just three screens — where the preceding non-`bridges`
  census of 35 kinds had yielded only 3. So "expect a similar split" was the
  wrong prior to carry into a new plane: **a family's split is a property of
  whether that family's SECTIONS are built, not of the fan-out's stage.** The
  admin pages were built long ago and simply never declared; the recovery plane
  is not built at all (the wireguard plane, the census's other example, was
  deleted outright 2026-08-23). Census before scoping, per family.
  `AdminUsersScreen` carries eleven — `fauna.admin.invite_requests.{approve,
  deny}`, `.invite_codes.{create,delete}`, `.set_registration_mode`,
  `.users.{update,evict,suspend,cancel_eviction}`, `.admins.{add,remove}`;
  `AdminSettingsScreen` three — `.tiers.update`, `.membership_tiers.{set,clear}`;
  `AdminCustodyHostingScreen` one — `.custody_hosting.remove`.
  ⚠ **This pass CORRECTS the batch-5 census on one entry, and the correction is
  the generalisable part.** `fauna.admin.custody_hosting.remove` was recorded as
  a page-parity gap (an unbuilt section). It is not: `AdminCustodyHostingScreen`
  renders the row button, the confirm and the cancel, and its VM calls the real
  `remove_custody_hosting`. The miss came from grading a `fauna.admin.*` kind by
  grepping the *user-facing* custody ids — the two kinds
  `fauna.custody.hosting.remove` and `fauna.admin.custody_hosting.remove` name
  two different pages, and one grep answered for both. **When two kinds differ
  only by an `admin.` prefix, grade each against its OWN page.**
  ⚠ **The users-row tier select is a dispatch-on-pick commit**, like
  `admin-web-apex-actor-select`: picking a tier IS the
  `fauna.admin.users.update` call, so the select carries the declaration and
  desensitizing the anchor closes the menu with it. The same `TierDropdown`
  composable also serves the two invite forms, where it is a pure buffer — hence
  its new `enabled` parameter defaults to live and only the dispatching call
  site hands a verdict in. **One composable, two roles: gate at the call site,
  never inside the shared widget.**
  ⚠ **A DENY is a commit**, the third time this fan-out has met that shape after
  the family transfer-decline and the paywall copy-link. It keeps its own test
  case for exactly that reason.
  The membership Clear composes with the page predicate it already had (an
  undesignated row has nothing to clear) and the custody confirm with `working`,
  giving two converse cases that **survive the red-verify unplug** — guards
  rather than restatements. The one admin kind still undeclared is
  `fauna.admin.users.create`: its Admit section is genuinely unbuilt on android
, and the gate lands with the section.
  9 more Robolectric tests. **Red-verified 2026-08-24, and the file has since
  grown to 71 tests: baseline 71/71 green, and unplugging the gate
  (`val gated = false`) reds exactly 31 of 71.** The prediction written before
  the run graded exactly — all four named desensitizing cases went red
  (`everyAdminUsersCommitGates_whileItsBuffersAndRevealsStayLive`,
  `theDenyGatesToo_becauseRefusingAnAdmissionIsStillACommit`,
  `theTierAndMembershipCommitsGate_whileTheirDraftFieldsStayLive`,
  `theCustodyRemoveConfirmGates_whileItsArmAndCancelStayLive`) **and both
  converse cases survived**, which is the load-bearing half: had either reddened,
  the gate would have REPLACED a page predicate instead of composing with it —
  on `working`, that would turn a destructive custody remove into a live button
  mid-request. Three earlier attempts had returned no verdict (twice the
  pool-pressure sweep destroyed the dataset mid-run, once a reboot); the fourth
  was blocked at compile by an unrelated `ageBand` field-add that had left
  android's whole test module un-compileable — the gap that break exposed is
.

  **Built — the android admin-dns fan-out, and a corrected backups
  declaration (2026-08-24).** Eleven more kinds, taking android's undeclared
  remainder from 74 to 63 on the rule-4 differential. Nine are the `admin-dns`
  domain plane (`fauna.bridges.` `add_local_domain`, `remove_local_domain`,
  `restore_local_domain`, `set_catch_all_actor`, `set_role_address`, and the
  four primary-domain-rename gestures), the tenth is `fauna.tls.publish_cert`,
  and the eleventh is `fauna.backup.nest_key.grant` from the backups page.
  **`admin-dns` is the page where this gate could most easily over-claim, and
  that is why it is worth stating**: a whole slice of it writes the *admin's
  own* DNS record (`fauna.state.dns`) — the held provider credentials, the
  per-domain managed opt-in and its manage-all sweep, the auto-renew opt-out,
  the CNAME renewal delegation, the manual-issuance breadcrumb — all ending in
  `fauna.account.state.put` (`fauna.config.put` until the rail retired
  2026-10-02), `OfflineSafe`, so they must stay LIVE beside nine
  deployment-state commits that must not. Greying the page wholesale would have
  passed a one-sided check and broken the contract, so every case is a pairing
  and one test asserts the three surviving config writes explicitly.
  ⚠ **The cert-issue button is a discriminant, and the one this seam's own doc
  names.** `singleIssue` — the domain is effectively managed *or* delegated —
  picks the path: that branch runs the whole DNS-01 order and ends by
  DELIVERING the cert to this nest (`fauna.tls.publish_cert`), while manual
  phase 1 only opens the CA order with the ACME provider and stashes a
  breadcrumb (`fauna.account.state.put`). The gate is handed the same flag the click
  takes, so **one control id yields opposite verdicts in one composition** —
  the sharpest pairing in the android leg, since neither a blanket disable nor
  a blanket enable can fake it. tui rules the identical split on the identical
  flag. Manual *phase 2* is where that path finally reaches the nest, so it
  declares `fauna.tls.publish_cert` too, while its cancel sibling stays live:
  abandoning a suspended order must work with no nest.
  ⚠ **The backups destination-register declaration was WRONG, and its comment
  recorded the wrong reason — which is the more expensive half.** It asserted
  that both add branches issue the same kind, so "this is one declaration, not
  a discriminant". Three ceremonies share that submit composable, and each
  binds on a different call: the nest enroll opens an authenticated session to
  the destination and registers the nest-writer grant over it, so it binds on
  `fauna.backup.nest_key.grant` (the config write that *ends* it is
  `OfflineSafe`, and declaring that would leave a control live that cannot
  finish); the custodian enroll has no address to resolve and deliberately no
  key grant at all, so it binds on `fauna.backup.destination.register`; and the
  **edit** form binds on `fauna.account.state.put`, a rename of one row of the owner's
  own document. The old single declaration therefore greyed exactly the edit
  the shared layer goes out of its way to keep working offline
  (`backup_destination_edit` re-resolves only when the URL actually changed,
  "renaming an offline destination must still work") — a real behavioural
  defect with a test that failed before the fix, not a tidiness point. The two
  add kinds are both `OnlineOnly`, so no enabled-assert can separate them; the
  witness that the nest branch now names the grant is the rule-4 differential,
  which counted that kind undeclared on android until this batch. **The general
  lesson, and the reason this is recorded rather than quietly fixed: a comment
  saying the question was asked and answered is how a wrong premise survives
  every later reader.** Follow the branch to its `request(...)` — the rule batch
  5 already wrote — and when the answer changes, REPLACE the comment rather
  than amending it.
  **17 more Robolectric cases, each half of a pairing. Red-verified 2026-08-24:
  baseline 88/88 green, and unplugging the gate (`val gated = false`) reds 39 of
  88 — 8 of the new cases, and the 31 pre-existing ones batch 6 measured,
  unchanged in both directions.** All four of the new converse cases SURVIVED
  the unplug, which is the load-bearing half: the primary-domain remove and the
  blank-URL destination add stay dead *while connected* (so the gate composes
  with each page's own predicate rather than replacing it — a red on the first
  would offer an admin a removal the nest forbids), and the two OfflineSafe
  cases stay live *while disconnected*. **The discriminant is proven from both
  sides, and only one of them is the mutant:** the unplug reddens its
  managed half, while the manual-stays-live half is witnessed by the BASELINE —
  had the button declared `fauna.tls.publish_cert` for both branches, the
  baseline itself would have failed on the manual domain's `assertIsEnabled`.
  A mutant can only show that a gate reaches a control; that the two branches
  resolve to *different* kinds is a green-baseline fact.

  **Built — the android admin-mail fan-out (2026-08-24), and the shared-widget
  rule gains a second reason.** Eight kinds — the six policy full-PUTs, the
  deployment-wide `set_mail_enabled`, and `publish_spam_baseline` — taking the
  rule-4 remainder from 63 to 55. The page carries **two** shared composables and
  they resolve OPPOSITE ways, which is the transferable part. `ToggleRow` serves
  the gated enable **and sixteen pure drafts**, so it is the `TierDropdown` case
  again: gate at the one dispatching call site, or every per-policy boolean greys
  with no nest. `SaveButton` serves all six groups and **every** call site is a
  commit — there is no buffer to protect — yet the gate still cannot live inside
  it, because it would be handed a *variable* and
  `check-offline-gate-kinds.py` cannot distinguish a variable from the computed
  kind ruling 2 requires it to refuse. So: **the kind literal belongs at the call
  site either way, but for two different reasons — one about what the widget
  would grey, one about what the checker can read.** It takes the whole
  `FaunaGateVerdict` rather than a bare `enabled`, which is what makes it
  impossible for a call site to render the button and forget its reason.
  ⚠ **`fauna.bridges.set_auto_enable_mail_for_new_users` is deliberately NOT
  declared, and the prior reason recorded for that was wrong.** It is not a
  policy field riding a `save*` commit: android renders **no control for it at
  all** — `AdminMailScreen` never mentions it and `AdminMailVM` holds it only as
  a placeholder snapshot field — while tui carries it as its own toggle. That
  makes it a page-parity gap (unbuilt control): not declarable, and **not** an
  `absences` entry either, since an absence asserts "renders no control by
  design" and this is simply unbuilt. It stays in the undeclared count, correctly.
  4 more Robolectric cases. **Red-verified 2026-08-24: baseline 92/92 green; the
  unplug reds 40 of 92 — one new case, and the 39 from the batch before it,
  unchanged.** The converse (a save in flight stays dead while connected) and the
  sixteen-buffer case both stayed green, the latter being the assertion that
  would have caught a gate placed inside `ToggleRow`.
  ⚠ **A limit of rule 4 measured for the first time here, and it bounds every
  app's fan-out — CLOSED 2026-08-24.** The differential can only flag kinds the
  oracle declares, so where **tui itself** returns `None` for a gesture that
  genuinely issues an `OnlineOnly` kind, that kind is invisible on all seven
  surfaces. tui did this knowingly for three gestures whose kind turned on form
  state (`MailAliasesSubmit`, `MailAliasesToggleActive`, `MailListsSubmit`),
  refusing to guess one of two and naming the fix in its own comments — carry
  the mode on the action, as `IssueDnsCert { single_issue }` does. **Fixed**:
  all three now carry their discriminant (`editing`/`enabling`), declaring
  **five** kinds total — `create_account_alias`, `update_account_alias`,
  `enable_account_alias`, `create_account_list`, `update_account_list`
  (`revoke_account_alias` was already declared, via the dedicated revoke
  button). The immediate `check-offline-gate-kinds.py` rule-4 yield is **zero
  new flags**: apple already declares all five with the same discriminant, and
  today rule 4's `absences` mechanism applies only to apple — web, android and
  windows opt out, and linux is not a Surface at all. The real payoff is tui's
  own three controls, which no longer stay live with no nest (the
  coverage-half's actual purpose), plus a raised floor for whichever app next
  opts into rule 4: android's `MailAliasesVM` was the census this gap was
  measured against, and its own count undercounted
  at four dispatchers for exactly this reason — a re-derivation now sees the
  full seven. linux carried the identical open case as a live twin — **closed
  2026-08-25**: the submit buttons in
  `mail_aliases.rs:313-325` and `mail_lists.rs:214-223` now seed `Create` at
  construction (`FormMode::Add`'s own seeded state) and re-declare in each
  page's `open_add_sheet`/`open_edit_sheet`, the only two places the mode ever
  changes; `mail_aliases.rs`'s per-row `Enable`/`Revoke` toggle
  (`:850-867`) needed no re-declare at all, since `build_alias_row` is torn
  down and rebuilt fresh from the snapshot on every render (`view.disabled`
  read fresh each time). Unlike tui, linux's declaration is a registry entry
  (`offline_gate::declare_wire_kind`), not a paint-time discriminant on the
  dispatched action — a persistent GTK widget has no per-frame rebuild to
  carry it — so "re-declare on demand" is the retained-widget-tree form of
  the same rule. Named-declaration test:
  `apps/fauna-linux/src/walk.rs::the_offline_gate_holds_the_mail_aliases_and_mail_lists_submit_buttons`.
  Closed. Note the asymmetry, since
  it decides the shape per app: a Compose or SwiftUI call site already has the
  mode in scope and can pass the discriminant expression straight to the gate,
  so android closes these without any refactor; tui needed the mode carried on
  the action only because its gesture enum *is* the seam.

  **Built — the android MAIL SUB-PAGES fan-out (2026-08-27), and a declaration
  rule android does NOT share with linux.** Twenty kinds across the five pages
  reached from the mail-settings hub — `MailSpamScreen`, `MailListMembersScreen`,
  `MailListsScreen`, `MailAliasesScreen` and `MailSettingsScreen`, none of which
  carried a single `faunaGate` call before this pass. Re-measured with the rule-4
  probe at pickup and after: **60 undeclared → 40**, never subtracted. ⚠ The
  denominator had moved **up** since the previous batch left it at 55, by exactly
  the five kinds tui's own `None`-arm fix declared — so the "census counts are a
  floor" warning is now a measured arithmetic fact twice over, not a caution.

  ⚠ **The transferable rule, and it is a genuine per-surface divergence:
  android must NOT declare a kind that can never desensitize; linux must.**
  Three controls on these pages are OfflineSafe by verdict and have to stay live
  with no nest — the spam page's share-reports toggle
  (`fauna.moderation.report_share.set`, a plain write on the shared moderation
  manager, not a bridge call) and mail settings' credential revoke and
  disable-mail confirm (both `fauna.account.state.put`, `fauna.config.put` until
  the rail retired 2026-10-02: they rewrite the user's own mail state
  (`fauna.state.mail`), and `mail-settings.md` § Disable mail deliberately does not touch
  the deployment-wide flag). linux declares the kind on all three
  (`settings/mail_spam.rs:189`, `settings/mail.rs:244,1511`); android declares
  none of them, and `check-offline-gate-kinds.py` **refuses** the attempt — *"no
  declared kind is OnlineOnly … so this gate can never desensitize anything"*.
  Both surfaces are right, because a declaration is a different object on each:
  linux's is a registry entry that is simply inert for a non-class-3 kind,
  whereas `faunaGate` **returns an `enabled`** that the call site hands to the
  widget and the next reader believes is doing work. A never-gating call there is
  gate-theatre — it reads as the control having been gated when nothing gates it.
  So on android the question is recorded in a comment beside the control instead,
  and the only way an OfflineSafe kind may legitimately appear in a `faunaGate`
  call is as the other arm of a **discriminant** whose first arm is OnlineOnly
  (the cert-issue button's `fauna.account.state.put`). This is not a priority-#1
  deviation to resolve: the behaviour is identical on both apps, only the
  mechanism that records it differs.

  ⚠ **`fauna.bridges.provision_wrapped_mls_blob` is NOT an unbuilt section on
  android, and the census that said so was wrong — the FOURTH time this row's
  inherited prose has been.** The pass-45 census listed it among four kinds with
  "no android render site at all". `MailSettingsScreen` renders **three**
  controls that issue it: the add/enable-credential submit, the rotate-keys
  confirm, and the pending-rotation resume — reached through `MailSettingsVM`'s
  `enableMail`/`addCredential`/`startRotation`/`resumeRotation`, every one of
  which bottoms out in `MailSettingsMachine`'s
  `provision_wrapped_mls_blob`. The cause is the same one the bridges census
  already paid for once: **android renames the leaf**, so a name-derived search
  finds nothing and the absence looks real. The standing defence is unchanged and
  was applied here — read the VM's own `fun` list, follow each dispatcher through
  to its `request(...)`, and use the differential only as a cross-check.

  **Three discriminants land here, and none of them is observable.** The alias
  submit turns on `editing` *and* `isDisposable`, the alias row's Active toggle
  on `alias.disabled`, and the lists submit on `editing` — each handed the same
  expression its own click handler takes. Every arm is `OnlineOnly`, so no
  assertion can distinguish a correct split from a collapsed single-kind
  declaration; unlike the cert-issue button, whose two arms differ in class and
  whose **baseline** therefore witnesses the split, these are pure
  future-proofing against a later per-arm reclassification. ⚠ Note the alias
  submit is **one arm wider than the lead app's**: tui splits that job across
  `MailAliasesSubmit { editing }` and a separate `MailAliasesGenerateDisposable`,
  while android folds the disposable submit into the same control — so a
  two-way carry copied from tui would have declared `create_account_alias` over
  a click that actually issues `generate_disposable_alias`. Per-app control
  composition, not the shared rule, decides a discriminant's arity.

  **Two shapes worth naming for whoever sweeps the remaining `bridges` pages.**
  (1) `mail-spam-threshold-override-input` is the one place in the whole android
  fan-out where *the commit gates the buffer* — because they are the same widget.
  The field has no Save button: its IME `Done` action **is** the dispatch, which
  is why tui carries a dedicated `MailSpamCommitThreshold` gesture and linux
  declares on the `Entry` itself. A disabled Material text field still renders
  its value, so the persisted override stays readable offline; only
  editing-to-commit closes. (2) `mail-spam-reset-model-button` gates **whole,
  arming click included**, and that does not break the "arming is local, the
  confirm declares" rule. That rule protects *openers* because an opener reveals
  something worth reading offline; a two-click inline confirm has no opener and
  reveals nothing — one button relabels itself — so arming a control that cannot
  fire would be pure theatre. Both the lead app and linux gate the single control
  outright.

  22 more Robolectric cases. **Red-verified 2026-08-27: baseline 114/114 green;
  the unplug reds 55 of 114** — this batch's 15 desensitizing cases plus the 40
  from the batch before, unchanged in both directions, so nothing here disturbed
  an existing case. **All five converse cases survived**, which is the
  load-bearing half: a save in flight, an empty member address, a list page with
  no local domain, a disposable alias with no default domain, and — the sharpest
  — an already-revoked alias whose revoke button stays dead while the toggle
  beside it stays live. Had any reddened, the gate would have *replaced* a page's
  own predicate instead of composing with it. The pairings are unusually strong
  on these pages: mail settings puts a **dead** provisioning submit and a
  **live** destructive credential revoke in one composition, so a blanket grey
  fails loudly rather than passing a one-sided check.

  **Built — the android BRIDGE / DAV fan-out (2026-08-27), and the census's
  "unbuilt section" verdicts turn out to be wrong across the board.** Fifteen
  kinds over seven pages: the three deployment-wide DAV enables plus the CalDAV
  port save (`AdminCalendarScreen` 2, `AdminContactsScreen` 1, `AdminFilesScreen`
  1), the forwarder admin (`AdminAliasesScreen` 2), the pending/approved bridge
  roster (`AdminBridgesPendingScreen` 4), the unified Bridges page
  (`BridgesScreen` 2) and the folder destination places plus the WebDAV serve
  toggle (`FoldersScreen` 3). Re-measured either side, never subtracted: **40
  undeclared → 25.**

  ⚠ **`fauna.bridges.revoke_service_user` is the "rotate" button, and that one
  mismatch is why an earlier census filed the kind as an unbuilt section.**
  Rotating an approved bridge's service-user key *is* revoking it: the approved
  card's rotate confirm dispatches `BridgeApprovalAction::Rotate`, whose machine
  arm calls `nest.revoke_service_user`, after which the bridge re-enrols with a
  fresh key (`mail-bridge-lifecycle.md` § Service-user re-keying). The control
  and the kind share no vocabulary at all. **Grade a kind by following its
  dispatcher to the `request(...)`, never by matching the control's own name** —
  the leaf-rename defence, now paid for a third time and in its sharpest form
  yet: the first two instances were a *different* name for the same idea, this
  one is the opposite verb.

  ⚠ **Chasing that finding refuted the rest of the list too. All FOUR kinds the
  pass-45 census recorded as having "no android render site at all — unbuilt
  sections, NOT gate gaps" are built.** `provision_webdav_keys_blob` is the
  folder WebDAV serve toggle (declared here); `revoke_service_user` is the rotate
  confirm (declared here); `provision_calendar` is `EventsVM.createCalendar`
  reached from the events page's new-calendar affordance; and
  `put_event_ciphertext` is `EventsVM.createEvent` — *and* the RSVP buttons,
  since an RSVP is a read-mutate-rewrite that re-PUTs the sealed event, *and* the
  attendee invite, which persists the roster before forking the iMIP. The two
  calendar kinds are deliberately left for their own pass: `put_event_ciphertext`
  alone has at least four distinct controls, which is a census, not a leftover.
  **The transferable correction is about the method, not these four kinds: an
  "unbuilt section" verdict reached by grepping for a kind's own name is worth
  nothing, and a batch that inherits one silently ships a gap.**

  **A third reason to gate at the call site, distinct from the two the admin-mail
  batch recorded.** Both forwarder commits dispatch through the shared
  `ForwarderMachine`. The earlier two reasons were about what a shared *widget*
  would grey (`ToggleRow`) and what the checker can *read* (`SaveButton`'s
  variable kind); here the shared thing is not a composable at all, so there is
  no "inside" to put a gate in. Same answer, third route to it: **the kind
  literal belongs at the call site, always.**

  **What legitimately stays live on the unified Bridges page, and why it carries
  no declaration.** `fauna.bridges.add_follow` and `.remove_follow` are
  **OfflineQueued** and `.set_settings` is **OfflineSafe** — classes 2 and 1,
  which ruling 1 never greys — so the follows rail and every per-setting editor
  stay usable with no nest beside a dead unlink. Per the never-gating rule
  (§ *Built — the android MAIL SUB-PAGES fan-out*) they carry no `faunaGate` call
  either; the checker would refuse one. They are this page's live siblings.

  The folder WebDAV toggle is the **oracle-gesture grading rule** applied a
  second time, and it lands the same way apple's did: the Rust issuer is
  `reconcile_webdav_keys_blob`, which reads like a background job and nearly
  bought a false absence, while tui's gesture is `ToggleFolderWebdav` and android
  renders exactly that toggle. Serving is what seals the blob, so the toggle is
  the commit.

  18 more Robolectric cases. **Red-verified 2026-08-27: baseline 205/205 green;
  the unplug reds 66 of 205** — this batch's 11 desensitizing cases plus the 55
  from the batch before, unchanged in both directions. Every one of the 18
  predictions matched, and all three converse cases survived (a CalDAV save in
  flight, a forwarder form with no local domain, and a folder set whose actor
  holds no MSEK).
  ⚠ **The batch's own two false starts are worth more than the count, because
  both were the SUITE lying rather than the gate.** (1) `performScrollTo()` on a
  control inside `BridgeCard` throws *"Semantic Node has no parent layout with a
  Scroll SemanticsAction"* — a card is not a page and supplies no scroller — and
  that `AssertionError` reads exactly like a gate failure. (2) Worse, and the one
  to remember: seeding `FfiBridgeStatus.linkModes = null` makes the shared
  `bridgeLinkBlock` rule return `NoApplicableMode`, so `UnlinkedSection` renders
  a **blocked placeholder carrying the same `bridge-action-button` id with a
  hard-coded `enabled = false`** and returns before the real link button ever
  composes. The disconnected assert therefore passed against a build with **no
  gate at all**. It was the *live-direction* case that caught it — which is the
  whole reason this suite pairs every disabled assert with an enabled one, and
  the sharpest instance yet of the standing rule that a control's own predicate
  must be satisfied before its gate is asserted.

  **Built — the android EVENTS plane (2026-08-31), which is the batch that had to
  REFACTOR before it could declare.** Three kinds over eleven controls and two
  screens: `fauna.bridges.provision_calendar` (the create-calendar confirm),
  `fauna.bridges.delete_event` (the delete *confirm*, not its opener), and
  `fauna.bridges.put_event_ciphertext` — the VEVENT writers, which on android is
  the create-event submit, the three invited-event RSVPs, both detail RSVPs, the
  reminder Set and Remove, the attendee invite, and the `.ics` import. Ten
  `faunaGate` declarations; the undeclared count went **29 → 26** (three kinds,
  however many controls issue them).

  ⚠ **Re-measured at pickup, and the denominator had RISEN again — 25 → 29**, the
  third time in this leg. tui shipped `fauna.bridges.{start,pause,resume,cancel}_
  import_session` while batch 10 was in flight. The standing instruction to
  re-run the probe rather than subtract has now paid for itself three times.

  **Neither events screen had a stateless `*Content` split, so nine of the eleven
  controls sat inside VM-bound bodies no Robolectric case could reach.** The
  batch therefore split first (`EventsContent`, `EventDetailContent`) and
  declared second — the shape batch 3 used for `AccountSettingsScreen`.
  Declaring without splitting would have landed nine gates nothing could
  red-verify, and this row has measured twice that an unverified gate assertion
  is worth *less* than nothing, because it reads as coverage.

  ⚠ **The `.ics` import is beyond the oracle's gesture set entirely.** tui has no
  import control, so rule 4's differential will never flag it, on android or on
  any app. It was found by following android's own dispatcher to its
  `request(...)` — `import_calendar_ics` parses, seals and PUTs every VEVENT it
  reads (`fauna_ffi::caldav_client`) — the leaf-rename defence generalized: **the
  oracle is a floor on the fan-out, never a ceiling.** Its export sibling, an
  inch away in the same row, is a pure read and declares nothing; the two of them
  are this page's sharpest pairing.

  **The attendee invite declares `put_event_ciphertext`, not `fauna.email.send`,
  and the lead app's own arm settles it** — the invite persists the roster FIRST
  (the FFI doc says so in as many words, *"so the roster persists even if the
  send fails"*) and only then forks the iMIP `REQUEST` per attendee transport.
  The roster PUT binds the ceremony; the send is OfflineQueued, and declaring it
  would leave a control live that cannot persist anything. Same shape as
  `backup.nest_key.grant` on the backups enroll form.

  **A per-app deviation was resolved on the way, and it was load-bearing for the
  gate.** android's reminder control was three untagged `FilterChip`s that
  applied the preset **on tap** — the exact auto-apply variant
  [`../ui/events.md`](../ui/events.md) § Reminders records as resolved against
  iOS's `onChange` on 2026-06-24, and the only app still doing it. It carried
  none of the four `event-detail-reminder-*` ids, so the cross-app driver
  contract was unexecutable there. Rebuilt to the ratified two-state shape
  (`EventReminderControl`), which is what makes the gate land on the right half:
  the preset select is a **draft** and issues nothing — the lead app pins
  `SetReminderOffset` → `None` — while Set and Remove are the re-PUTs. Gating
  the chips instead would have cemented the deviation under a green test.

  21 more Robolectric cases across the two screens' contents and the two
  dialogs.

  **Built — the android BLUESKY / atproto plane (2026-08-31), the largest single
  page in the leg, and the batch that had to REFUTE a settled ruling before it
  could declare.** Nine kinds over eleven `faunaGate` declarations on
  `AtprotoSettingsScreen`, which carried none before:
  `atproto.set_integration_level` (the four depth rungs **and** the transition
  card's confirm — two sites, one kind), `.provision_app_credential`,
  `.revoke_app_credential`, `.revoke_session`, `.set_external_apps_enabled`,
  `.provision_authoring_delegation` (**both** call sites — the first-grant button
  and the live-row renewal), `.revoke_authoring_delegation`, `.resolve_consent`
  (approve and deny, one verdict), and `.delete_presence`. The undeclared count
  went **26 → 17**; the checker went 284 → 295 declarations.

  ⚠ **`atproto.delete_presence` was declared here against two standing notes
  saying not to.** The pass-45 census recorded the control as INERT — literally
  `onClick = {}` — and the pass-46 append closed it *"the inert button is
  DELIBERATE and uniform, not a wiring gap … it declares nothing until S5 wires
  the ceremony. Do not reopen."* Both were true when written; neither is true
  now. The trickle-down since wired the whole open/cancel/confirm ceremony,
  `AtprotoVM.confirmDelete` dispatches, and `AtprotoSettingsMachine::
  confirm_delete` calls `nest_api.delete_presence()`. **The transferable rule:
  a "do not reopen" note closes a question against the code AS IT WAS, and this
  row has now been wrong about its own inherited prose five times — a recorded
  impossibility ages exactly like a recorded possibility.** The cheap check is
  the one that caught it: read the VM's `fun` list before trusting any note
  about what a control does.

  **The selector and the transition confirm collapse onto ONE kind, and the lead
  app is why.** `select_level` reaches the nest directly only on the effect-free
  Off → Linked move and stages the transition card on every other pick; tui's
  `Action::wire_kind` puts `AtprotoSelectLevel` and `AtprotoConfirmTransition` in
  the same arm for exactly that reason — *"both paths that DO call end in the
  same kind, so the state this action does not carry cannot change the answer."*
  So both sites declare, and the selector's four rungs share one verdict and one
  reason rather than four.

  **This page's live half is unusually strong, which is what makes its coverage
  worth something.** Three separate reasons a control stays live are all present
  and all pinned: the **contest confirm** is a destructive red button that must
  survive the outage because it is signed and submitted on the device's own
  connection to the public PLC directory — no nest call at all, the whole point
  of a remedy for a nest that may *be* the attacker (tui pins
  `AtprotoRequestContest` → `None`); the credential **reveal** sits inches from
  the credential **revoke** and stays live on a *class* distinction rather than a
  local/remote one — then `fauna.config.get`, class Read, recovered from this
  device's own config under the D3 custody split; since the rail retired
  2026-10-02 the secret is read from the account-state plane kind
  `fauna.state.atproto` ([`config-dissolution.md`](config-dissolution.md)
  § The `__config` dissolution schedule → *The kinds*); and the DID-method radio and
  history-backfill checkbox are **drafts** the machine holds until a confirm
  commits them. A blanket grey of this page fails all three.

  16 more Robolectric cases, including two converse cases that discriminate a
  composing gate from a replacing one: a hosted rung on a non-public domain and a
  transition already in flight both stay dead **with a nest present**, so
  unplugging the gate must not move them.

  **Built — the android FAN-OUT IS COMPLETE, and android is OPTED INTO RULE 4
  (2026-08-31).** Six final declarations closed the sweep, and the remaining
  eleven oracle kinds were graded into the android surface's `absences` ledger,
  so `check-offline-gate-kinds` now runs its rule-4 differential against android
  the way it already did against apple: any future tui gesture whose android
  control lacks a gate reds the cheap merge tier instead of waiting for a sweep
  to notice. The checker went 295 → 301 declarations, and the undeclared count
  17 → 11 → **0 unaccounted**.

  The six: `fauna.nostr.zap_signers.add` / `.remove` (the NIP-57 trust root's
  designate and undesignate on `NostrScreen`),
  `fauna.filesync.snapshot.restore_message_kind` (the Backups local-restore
  confirm behind its friction bar), `fauna.bridges.put_spam_model` (the DM
  overflow's mark-as-spam), `fauna.conversations.keypackage.upload` (the
  Encryption page's key-package top-up), and
  `fauna.conversations.keypackage.fetch` (the add-participant confirm).

  ⚠ **Three of those six were sitting inside a set this row's own prose had
  signed off as "the non-declarable set, unchanged and well understood".** They
  were not: `put_spam_model` had been closed against the *moderation queue's*
  train-correction button — a different gesture, which correctly declares
  `fauna.moderation.train` — while the oracle's gesture is tui's conversations
  `MarkMessageSpam`, and android renders exactly that control
  (`dm-message-mark-as-spam-button`); both `keypackage.*` kinds had been written
  off as "background machinery" when android renders a user control for each (an
  add-participant confirm and a Refresh keys button). **The transferable rule,
  and this row has now proved it three passes running: a grade recorded against
  one control does not transfer to a different control that merely sounds
  related. Grade from the ORACLE's gesture — find tui's action, then look for
  the app's control for that action** — which is exactly the discipline
  `check-offline-gate-kinds`'s own header asks for, and exactly what nearly
  wrote a false reason for `provision_webdav_keys_blob` on apple.

  **The add-participant confirm is the leg's first gate that applies to only ONE
  arm of a discriminant, and the shared state carries the discriminant for
  precisely this purpose.** Confirming reaches the wire exactly when the target
  is a bound FaunaMls **group**, whose add commit opens by fetching the
  newcomer's key package; a FaunaMls 1:1 *forks* a new group whose first send
  bootstraps it, and a non-FaunaMls rail has no wire membership op at all — both
  succeed with no nest, so gating them would be the over-claim § *How a surface
  asks* forbids. `AddParticipantState::in_place_mls_group` exists to state that
  difference, stamped off the one shared
  `fauna_conversations::capabilities::is_in_place_mls_group` predicate that
  `confirm_add_participant` also acts on, so no app re-derives it; android
  applies the gate under that flag alone, the same split apple's
  `AddParticipantSheet` makes. It is the shape to copy wherever a control's two
  arms are "needs a nest" and "issues nothing" — a `faunaGate` call takes a
  kind, not an absence, so the *call* is what becomes conditional.

  **What the eleven absences say, and why the ledger is a tripwire rather than
  an exemption.** Five `fauna.recovery.*` share one expiring reason — android
  renders no recovery surface at all, ui.yaml declares `recovery-kit-*` and
  `recovery-entry-*` and UiIds generates every constant with no composable
  referencing one — which is the exact shape apple's five had before
  `RecoveryKitSection` landed and failed all five together. Four
  `bridges.*_import_session` name the unbuilt mail-import wizard;
  `bridges.set_auto_enable_mail_for_new_users` names a generated toggle id with
  no render site; `custody.hosting.remove` names the unbuilt user-plane
  custody-held card, and carries a warning against "fixing" it by pointing
  android's already-declared **admin**-plane `fauna.admin.custody_hosting.remove`
  at the other string. Every entry becomes an error the day android declares its
  kind, so the ledger cannot rot into a silent exemption.

  14 more Robolectric cases across three classes, four of them converse cases
  that discriminate a composing gate from a replacing one: a Dim-3 `zaps` deny,
  an unmatched restore friction bar and a publish already in flight all stay
  dead **with a nest present**, and the 1:1-fork add stays live **with no
  nest**. ⚠ The gate's cases now live in `OfflineGateTest`,
  `FoldersContentTest` **and** `ConversationDetailContentTest` — a red-verify
  filter naming fewer than all three silently drops cases from its split.

  ⚠ **Read "complete" at exactly the strength rule 4 has, which the next
  subsection states: a coverage FLOOR, not a proof.** Rule 4 is per-KIND, so a
  *second* android control issuing an already-declared kind without a gate stays
  invisible to it, and no probe can name a gesture tui has not built — the `.ics`
  import found during the events plane is precisely that shape, a real
  `put_event_ciphertext` site with no oracle gesture on any app. What is settled
  is the differential: every kind the oracle names is now either declared on an
  android control or graded in writing, and stays that way under the merge gate.
  What a later pass may still find is a control the oracle cannot see.

  **And one was found the same day, by hand, which is why that caveat is not
  boilerplate.** `nostr-unlink-button` on the Nostr page issues
  `fauna.bridges.unlink` — **OnlineOnly, and already declared** by
  `BridgesScreen`'s per-bridge unlink — so the differential read the kind as
  covered and never named the second control, which sat ungated through nine
  sweep passes. It is now gated, with its live clipboard sibling pinned beside
  it. **The generalisation worth carrying to web and windows before their own
  opt-ins: when a kind is reachable from two pages, the probe vouches for the
  first control it finds and goes quiet about the rest — so a page-complete
  sweep still owes a per-page read of the controls whose kind some *other* page
  already declares.** The cheapest form of that read is the one that found this:
  count a page's `faunaGate` calls against its click sites and account for the
  difference, control by control.

  **Built — the coverage half, and the lead app is the oracle (2026-08-17).**
  The gap recorded here until today — that a control issuing an `OnlineOnly`
  kind and declaring *nothing* is invisible to every check — was argued from a
  true premise to a wrong conclusion. The premise stands: which kind a Swift
  button's action eventually issues lives in the Rust call chain, not in the
  view, so no amount of reading the view answers it. The conclusion did not
  follow, because **another app already computes the answer exhaustively**:
  tui's `Gesture::wire_kind` is a fallback-free match whose sweep is complete,
  so the set of `OnlineOnly` kinds tui declares **is** the inventory of user
  gestures that need a gate. `check-offline-gate-kinds.py` **rule 4** is that
  differential — every oracle kind must be declared by a swept surface or carry
  a written absence in `Surface.absences` — plus its own vacuity floor
  (`_MIN_ORACLE_KINDS`, the lesson `_MIN_TABLE_ENTRIES` taught) and 8 tests.
  Being a differential against a compiler-checked list is what keeps it cheap:
  no Swift call-graph analysis, and no allow-list of legitimately-offline
  controls (tabs, cancels, text fields — most of any page), which is the
  cry-wolf shape convention 17's adoption discipline forbids.
  **Its first run found seven ungated apple controls**, in planes three earlier
  sweeps had walked: `admin-users-make-admin-button` and its remove twin (one
  line below a gated sibling), `admin-nest-seed-rotate-confirm-button`,
  `restore-confirm-button`, the two subscription payment-provider controls, and
  `folder-webdav-toggle` — plus `nest-trust-backup-revoke`, which declared only
  the seal grant while serving both row kinds and now declares through the same
  `backup.kind` discriminant its action takes. ⚠ **The grading rule the webdav
  one paid for: grade from the ORACLE's gesture, never from the Rust issuer's
  name.** Its issuer is `reconcile_webdav_keys_blob`, which reads like a
  background job and nearly bought a false absence; tui's gesture is
  `ToggleFolderWebdav` and apple renders exactly that toggle. That is also why
  `folders` no longer grades empty — the earlier "its three OnlineOnly kinds
  have no apple control" verdict was wrong about this one.
  Sixteen further oracle kinds are graded absences, each naming the apple
  control that does not exist; the five
  `fauna.recovery.*` entries are the **expiring** grade below, and an entry for
  a kind the surface has since declared is itself an error — so the day a
  settings-level recovery surface lands, the ledger fails until it is revisited,
  rather than the grade quietly outliving its reason.
  **One entry is not an absent control, and says so:**
  `fauna.conversations.keypackage.fetch`. apple renders
  `add-participant-confirm`, but tui gates only the **in-place** add — a 1:1
  fork issues nothing — and apple's shared `AddParticipantState` carried only
  `targetThreadId` + `picker`, so a blanket gate would grey a fork that works
  offline, the over-claim ruling 3 forbids. **The fix was shared Rust, not
  Swift:** carry the in-place flag on `AddParticipantState` the way tui's action
  carries it, and every app inherits the discriminant. This is the same shape as
  tui's own moderation Correct gate, which apple had already solved from the
  other direction and tui now carries too — the row source rides on the action,
  answered per branch (`git log --grep 'the moderation Correct gesture'`) —
  the two are a matched pair, and neither app should invent a per-app class
  test instead.
  ✅ **The shared half LANDED 2026-08-17.** `AddParticipantState` now carries
  `in_place_mls_group`, stamped by `open_add_participant` off the thread it
  already looks up (`fauna_conversations::capabilities::is_in_place_mls_group`,
  the single predicate `confirm_add_participant` also derives the wire decision
  from); tui reads it instead of re-deriving the `(rail, flavor)` test at paint,
  so the discriminant now has exactly one definition for all seven apps. Shape
  and the two traps it carries — it is not the wire authority, and not the
  fork-vs-mutate test — are owned by
  [`../ui/conversations.md`](../ui/conversations.md) § State & data shape.
  ✅ **And the apple half LANDED 2026-08-19, closing the kind.**
  `AddParticipantSheet`'s confirm is a `@ViewBuilder` that branches on
  `state.inPlaceMlsGroup` and applies `.faunaGate(
  "fauna.conversations.keypackage.fetch")` on the in-place arm only — the same
  shape `ModerationQueueView.correctButton` uses for its row-source branch, and
  deliberately not a blanket gate. The absence entry was deleted in that same
  commit, which is what rule 4's per-kind differential requires; removing the
  declaration while the entry is gone red-verifies (the checker names the kind
  and points at tui's `wire_kind` as the oracle). So the kind is now **declared,
  not graded** — and `conversations` is graded empty apart from mark-as-spam.
  **What rule 4 does NOT close, stated so nobody over-reads it:** it is
  per-KIND, not per-CONTROL. A *second* control issuing an already-declared
  kind without a gate is still invisible, and a kind no app declares yet cannot
  be named by an oracle built from gestures tui has built. It is a coverage
  floor, not a proof. Rule 4 applies per surface, and web and android are
  both deliberately opted out (`absences=None`, the field default) until their
  own sweeps are page-complete: web declares 8 kinds against the oracle's 120,
  so a ledger would be noise rather than a claim.

  **Built — the windows leg's seam (2026-08-24), and WinUI is GTK's shape, so
  this one really is a port.** The fifth toolkit, and the first that did not turn
  out to be a new shape: WinUI controls outlive the state that gated them exactly
  as GTK's do, so linux's registry transfers structurally —
  `FaunaApp.Core/Services/OfflineGate.cs` is `offline_gate.rs` with declare /
  re-decide / watch-the-property intact, the state word arriving from the single
  `INestRpcClient.ConnectionStateChanged` subscription that also feeds the
  `connection-status` indicator (`MainViewModel.OnConnectionStateChanged`), via
  the shared `ConnectionStateWord` export rather than a C# switch.
  **Where windows had to differ is the TEST boundary, and it shaped the file
  layout.** `FaunaApp.Tests` targets plain `net10.0` and references only
  `FaunaApp.Core`, so it cannot instantiate a `FrameworkElement` — a registry
  written against `Control` would have had no unit tests at all, and linux's 9
  in-crate mechanism tests no twin. So the registry lives in `FaunaApp.Core`
  behind a four-member `IGatedControl` seam (own enablement, the gate's caption,
  liveness, an enablement-changed event) and everything toolkit-specific — the
  weak `Control` reference, mapping `IsEnabledChanged`, inserting the caption —
  sits in the WinUI adapter (`FaunaApp/Helpers/OfflineGateExtensions.cs`). That
  is a general lesson for a family whose test host cannot load its own UI
  framework: put the decision where the tests can reach it and let the adapter be
  the untested-but-trivial part.
  ⚠ **The echo trap's toolkit answer is the opposite of convenient, and assuming
  it would have shipped a latent bug.** WinUI raises `IsEnabledChanged`
  *synchronously* from inside the property set, where GTK delivers it after the
  write returns — so the naive "I am writing now" flag, which is what the linux
  trap exists to forbid, **works on WinUI today**. Measured, not assumed: the
  mechanism test is a `[Theory]` over both orderings, and with the flag
  substituted for the value-match rule the deferred-echo case fails while the
  synchronous case passes. A windows leg that had reasoned "our toolkit is
  synchronous, a flag is fine" would therefore have been correct on the bench and
  silently wrong the first time any notification arrived deferred — which is why
  windows carries linux's value-match rule unchanged rather than a
  locally-justified simplification (priority #3), and why the proof is
  toolkit-independent rather than a pin of today's WinUI behaviour.
  **The reason is an inline caption, not a tooltip** — windows' own
  disabled-with-a-reason idiom, which `FoldersPage` had already ruled in so many
  words ("a tooltip alone is not discoverable on a control the user cannot
  focus"), matching [`../ui/README.md`](../ui/README.md) § Copy comprehensibility
  rule 5 and android's `DisabledControlReasonText`. Like both, it is deliberately
  **un-id'd chrome**, so it needed no `ui.yaml` id and no rule-A approval; the
  adapter appends it after the control in its parent panel and collapses it again
  on release, so sibling indices never shift.
  Declared so far: the backups remove confirm, the linked-nests add submit, and
  the admin pairing toggle — the three pairings the shared e2e reads, each with
  a sibling that declares nothing and stays live. Proven by 16 tests
  (`FaunaApp.Tests/OfflineGateTests.cs`) calling the **real** shared rule through
  the native `fauna_ffi` dll rather than a C# stand-in, with the fixture kinds
  read out of the shared table so a reclassification cannot turn them green for
  the wrong reason; `check-offline-gate-kinds.py` gained a windows surface
  needing android's two narrowing mechanisms for android's exact reason (**C#
  spells a call and a definition the same**, so `FaunaGate(` matches the
  extension method's own signature) plus a `require_fragment` keeping the scan
  out of `FaunaApp.Core`'s ~30 MB of generated UniFFI C# and `obj/`'s
  XAML-compiled page copies.
  **The windows run also found a leak in the SHARED test file, and the finding is
  about test hygiene rather than about windows.** The first case armed the remove
  `ContentDialog` and never dismissed it; the next case navigated the frame out
  from under it — which only a driver can do, since a real user's input is blocked
  by the modal — and WinUI then refused the *next* `ShowAsync` ("only a single
  ContentDialog can be open") inside an `async void` that swallowed the throw. The
  sibling failed with an invisible confirm button and an **empty** error string,
  which reads exactly like a gate fault and is not one: that test passes alone.
  Two things generalize. **(1) A toolkit where a dialog is a singleton converts
  "leaked UI state" from untidiness into a cross-test failure**, so the leak had
  been latent on the four apps already running this file. **(2) The dismissal
  belongs in a finalizer, not at the end of the body** — the red-verify pass is
  what proved it: with the gate unplugged the first case fails *before* any
  end-of-body cleanup, so the third case died on the leaked modal instead of on
  its own gate precondition, turning a clean three-way red into two real reds and
  one cascade. That is the same lesson this file's own nest-restore `finally`
  records ("restoring the nest is this test's own cleanup, not the next test's
  problem — found by the red-verify pass"), rediscovered one surface over: **a
  red-verify does not only ask "do these tests notice the mechanism", it exercises
  the failure paths where cleanup is skipped, which is where leaks become
  misdiagnoses.**
  ⚠ **windows now takes all four of `test_offline_gate.py`'s cases**, closing the harness gap the
  paragraph above used to describe: the FlaUI bridge now serves `GET /registry`
  (`flaui-bridge/Actions.cs::RegistrySnapshot`, mirroring `web-bridge/server.py`'s
  route field-for-field — id/index/enabled/`declares_enabled`/actuable/editable/
  scope), so `WindowsBridgeDriver.registry_snapshot()` answers a real frame and
  `assert_offline_gate_reach`'s whole-frame differential runs on windows like
  every other app. `declares_enabled` reads a UIA `ItemStatus` marker
  `ControlGate` stamps at declare time (`OfflineGateExtensions.cs`) — the windows
  twin of web's `data-offline-gate-declared` DOM attribute, since raw UIA
  `IsEnabled` cannot distinguish a declared control from a merely-defaulted one.
  **The red-verify surfaced a real, separate, previously-undiscovered gate gap**:
  `admin-service-pairing-toggle` (`fauna.admin.services.update`) had never been
  declared to the windows gate at all — the admin-plane case had simply never run
  on windows before (blocked on the missing route), so nothing had ever exercised
  it. Fixed in the same pass (`AdminNestPage.xaml.cs`'s constructor), matching
  linux's own declaration (`views/admin.rs:2844`). **Not built for windows:** the
  remaining gesture families (the same declare-as-each-page-is-swept footing
  linux, web and android are on, which is why its `absences` stays `None`).

  **Not built:** the per-control residual above — a second, ungated control for
  a kind some other control already declares — for which
  the frame-scope pair measures what the gate **did**, never what it
  **should have** covered; the residual confirm instance is the spam page's model-reset confirm, still
  a `.confirmationDialog` and so still opener-gated rather than
  confirm-gated; a **visible** iOS reason (apple carries it on the
  control today — macOS `.help`, accessibility hint on both — because a separate
  reason element needs a ui.yaml id, and android faces the identical choice); the
  recovery ceremony surface, whose grading above is conditional on its being
  built; the remaining linux gesture families; the remaining web gesture
  families (its leg gates the `admin-nest` family plus the two controls the
  shared e2e pairs, on the same "declare as each page is swept" footing as
  linux); the remaining windows and android gesture families (both seams are
  BUILT — see their own paragraphs above; this clause named "the windows and
  android legs" wholesale until 2026-08-24, which had been stale for android
  since its 2026-08-17 seam landed);
  and the failed-sends surface, which rides the first composer, not this phase.
  Conversations is graded and near-empty by contrast — its sends are
  `OfflineQueued`, and with the in-place add-participant now declared (above)
  only mark-as-spam remains, which apple renders no control for.
  All tracked internally.
