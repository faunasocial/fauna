# Replica posture — what a replica is and what it may hold — target state

Owns: account-replica-posture
Status: ratified — the replica boundary is observation-defined (R1), the per-replica storage posture and the custody grant + ceremony (R7/T13) were ratified 2026-08-11 and resolved 2026-08-15, and W8's six contract slices are code; split verbatim out of `account-data-plane.md` on 2026-09-06
Authority: **what a replica is and what it may hold** — the observation-defined replica boundary and the fleet seen-set (R1), the store device principal (R4 + R5: enrollment, the credential slot, seed residency), the per-replica storage posture (R7: reading and custodian replicas, the custody floor, the custody grant + ceremony and its plane semantics), and the app-side local at-rest posture (R3). **NOT owned here** — what the classes being held ARE → [`account-data-taxonomy.md`](account-data-taxonomy.md); how they arrive → [`account-sync-plane.md`](account-sync-plane.md); the nest's own at-rest sealed posture → [`encryption-at-rest.md`](encryption-at-rest.md); the device model and `DeviceAuthorization` mechanics → [`../behavior/devices.md`](../behavior/devices.md); the charter and the cross-cutting status → [`account-data-plane.md`](account-data-plane.md). On conflict in those domains, raise it.

Last verified: 2026-09-06 (split verbatim; each ruling carries its own date in the sections below)

Split verbatim out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06 — that doc had reached **686,434 B**, 2.62× the 262,144 B whole-file read ceiling, and no single seam could clear it (moving its status ledger alone left both halves breached, re-verified at three successive sizes). Its own `Authority:` line already enumerated the five concepts it owned; this is that list made structural, each concept taking its rule sections **and** its status-ledger entries together. A routing stub remains at each original location; prior history: `git log --follow docs/goal/architecture/account-data-plane.md`. The `W<n>` workstream labels and `R<n>` decision labels used throughout are defined in [`account-data-plane.md`](account-data-plane.md) § Workstreams and § The ratified decisions.

> **Reading this doc.** Its text was carried **verbatim** out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06, so an unqualified `§ <name>` citation inside it may name a section that is no longer a sibling on the page. Resolve any such name against the rest of the family first: [`account-data-plane.md`](account-data-plane.md) (the ratified decisions, the account store, the nest-side requirements and the cross-cutting status), then [`account-data-taxonomy.md`](account-data-taxonomy.md), [`account-sync-plane.md`](account-sync-plane.md), [`account-offline-mutation.md`](account-offline-mutation.md), [`account-runtime.md`](account-runtime.md). Positional words (“above”, “below”) inside a carried block point within this doc: every section moved whole, so an intra-section deictic could not break, and the boundary-crossing ones were scanned before the split.

## Section map

- **[The replica boundary — observation-defined (R1)](#the-replica-boundary--observation-defined-r1)** — what makes a replica a replica.
- **[The store device principal (R4 + R5)](#the-store-device-principal-r4--r5)** — enrollment, the credential slot, seed residency.
- **[Replica posture (R7)](#replica-posture-r7)** — reading and custodian replicas, the custody floor, and **[The custody grant + ceremony (T13)](#the-custody-grant--ceremony-t13--resolved-2026-08-15-refutable-until-w8-code)**.
- **[Local at-rest posture (R3)](#local-at-rest-posture-r3)** — the app-side on-disk posture.

**All four `##` headings are unchanged from `account-data-plane.md`**, so a `§ Replica posture (R7)` or `§ The store device principal (R4 + R5)` citation resolves by swapping the filename.

## The replica boundary — observation-defined (R1)

An item enters the account's data plane at **first observation or creation by
any of the account's apps** — or at **delivery to the account** (mail,
inbox items: they are addressed to the account and the nest already persists
them per-actor). From then on it syncs fleet-wide: every replica carries its
metadata (the full logical index of statement 5), payload hydration stays
per-device policy. The un-browsed universe — feed posts never rendered,
federated content never fetched — stays outside; no replica mirrors "what
could be fetched".

The boundary is materialized as the **fleet seen-set**: a per-account,
grow-only set of item references (CID or kind-scoped key), CRDT by
construction (grow-only union), itself part of the replica and synced on the
same plane. It is a genuinely new structure — nothing today records "what
this account's clients have observed" (survey § 2.2).

Residual precision, design detail not user gates — both resolved 2026-08-10
(second design pass; refutable until W1 code lands — **T1 is no longer
refutable: it is code-confirmed end to end since 2026-08-14**, see its own
note; T2 still carries the clause):

- **T1 — the "seen" trigger for browse content is *body-rendered*.
  CODE-CONFIRMED 2026-08-14** — both halves are built and the whole path is
  proven by a two-device tier_3 convergence
  (`conformance_account_runtime.rs` V6). A browse
  item enters the seen-set when its **body is materialized for display** —
  the record's content is handed to a visible view (a post card rendered on
  screen, a message opened, a preview expanded) — never when its existence
  merely transits a list buffer, a virtualized scroll's overscan, or a
  prefetch. The trigger lives in **shared Rust at the snapshot/render
  boundary** (the point a body leaves the data layer for a view), so all 7
  apps inherit one rule and no app hand-implements it. Delivery-class items
  are in-set at delivery regardless (R1).

  **The producer decomposition (W3 build finding, 2026-08-12).** R1's three
  admission routes make "the seen-set producer" two producers, not one.
  **(1) The auto-in-set producer — BUILT** (§ Implementation status →
  *Built — W3 the auto-in-set seen-set producer*): every item of an
  own-actor scope is creation- or delivery-class, so the account runtime
  raises each such scope's `NEST_SEQUENCER` watermark to the store's
  accounted frontier after its content walks
  (`fauna_sync_engine::seen_set_producer`) — no render event anywhere, and
  the seen-set rung ruling's "refutable until a production writer ships" is
  discharged by it. **(2) The T1 body-rendered trigger governs browse
  content only** — member/subscribed scopes (`conv` today; followed content
  scopes as they land) — and stays **itemized: no producer ever earns a
  browse scope a watermark**; the one thing that raises one is the seen-set's
  own budget fold (T2 transition (4) → *The budget*), once a writer's itemized
  references outgrow their share of the entry. It is
  **gated on the first browse surface with client-side scope-feed
  coordinates — a gate OPEN since 2026-08-13**: `conv` joined the nest's
  served-kind set behind the channel-roster admission (§ Implementation
  status → *Built — conv on the content-scope feed*), so a joined channel's
  member replicas walk its coordinates and the trigger's seam has a live
  consumer to build against (followed scopes remain unbuilt). **The intake
  half is BUILT** (§ Implementation status → *Built — T1 the observation
  intake*), and its placement is no longer refutable — it is code: a shared
  **observation intake** beside the account runtime
  owns the entire rule — browse classification, coordinate resolution via
  the local journal's scope-feed coordinate, dedup against the merged entry,
  the class-2 put — while the render layer reports only the platform fact
  shared Rust cannot know, *"this body was handed to a visible view"* — the D3
  reveal-state division ([`render-model.md`](render-model.md)):
  manager-reported where the manager owns the moment (a thread opened, a
  preview expanded), shell-reported for scroll visibility with overscan
  excluded by the reporter. **The reporting half is BUILT on tui, the lead app
  (2026-08-14** — § Implementation status → *Built — T1's reporting half on
  tui*): its conversation detail reports the bodies its terminal actually
  painted, so a production render now records browse observations. **The other
  six apps still owe the same leg**; each reports for itself,
  because "which realized rows are on screen" is per-shell and is exactly what
  no shared layer can answer. An observation whose coordinate the journal
  cannot yet resolve is dropped and re-recorded on a later render
  (grow-only set semantics make the retry free) — never CID-keyed pending
  state.

  **What the reporting half taught, and what every app's leg owes
  (2026-08-14).** Two constraints the division above does not say outright, but
  which tui's build made unavoidable, and which the next six legs inherit:
  **(a) the record's plane identity has to reach the app at ingest.** An app
  reports `Observation { scope, record }`, but a conversation message arrives
  over `channel.fetch` as `(seq, envelope)` — no CID. It is derivable, not
  fetchable: the nest hashes `blake3(channel ‖ seq ‖ envelope)` when it
  appends, and only the ingest layer still holds all three (the decrypt
  consumes the envelope, and a *sender* can never re-derive its own record at
  all, since it cannot MLS-decrypt its own application message). So the
  identity is derived once, at ingest and at send, and carried on the message
  (`fauna_conversations::plane` → `MessageSnapshot::plane_ref`) — one shared
  derivation, calling the very function the nest calls. **(b) "realized" is not
  "on screen", and an app's own registry will lie about it.** tui registers
  every message of a thread while painting a screenful; reporting from the
  element list would have credited the account with reading messages that never
  appeared — permanently, the set being grow-only. The honest witness is the
  frame's own painted geometry. Each app's leg must name what plays that role
  for it, and prove it with the negative test (an off-screen body is NOT
  reported), not just the positive one.
- **T2 — eviction: four transitions, and no fifth.** (1) **User delete**
  tombstones on the plane — the item leaves every replica, and the tombstone
  drives the kind's own nest-side deletion semantics (owned per kind, not
  here). (2) **Dehydration** drops a payload on one device — a class-3 blob,
  or a bulky class-1 record's block bytes (§ Store logical schema); the index
  stays — the only per-device shrink (statement 5). (3) **Scope departure**
  (leaving a channel, a shared set unshared, membership revoked) ends the
  subscription and drops that scope's items from the replica — the data
  belonged to the membership. **BUILT 2026-08-12** (§ Implementation status →
  *Built — W3 scope departure*), and its build settled one thing the
  transition's one sentence does not say: **departure is concluded, never
  inferred.** A replica may drop a scope only from an *affirmative* membership
  answer diffed against a durable record of what it last subscribed to — never
  from the absence of a scope in a derivation (a still-loading membership
  source, or an app that wires none at all, both derive no member scopes while
  having said nothing about membership). The seen-set entry for a departed
  scope survives the drop, per transition (4) below. **What the replica remembers of a departure (ruled 2026-10-01; built 2026-10-03, `departure::departed_scopes`):** it keeps the scope in a device-local *departed* list, from the pass that concludes the departure until an affirmative answer names the scope again. The list is local for the reason the subscription marker is: each device concludes from its own membership state, and a synced list would let one device's stale view act for another. The entry survives in the store, and what leaves with the departure is the entry's row at a nest: [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md) § Delegable-scope reclamation, parts (6) and (7), owns which rows a departed scope's replica retires and who may write them back, and reads this list to do it. (4) The **seen-set itself is grow-only as a
  *set*, compactable as *bytes*** (amended 2026-08-11, greenfield finding
  A4): grow-only union stays the merge law — membership never shrinks —
  but the representation coalesces references into ranges / per-scope
  watermarks once the referenced items are locally indexed, so the
  structure is compactable **by construction** rather than a
  forever-growing itemized attention log resting on every custodian's
  floor. Realized (W2.5 item 2, 2026-08-12) by recording each reference
  as its **scope-feed coordinate** `(writer, seq)` — resolving a
  coordinate back to its CID / kind-scoped key is the local record
  index's job — which makes the watermark elision part of the **join
  itself** (pure arithmetic, no index consulted), so compacted and
  itemized replicas converge on identical shrinking bytes
  (`fauna_core::seen_set` owns the law). **The budget (ruled
  2026-09-15).** Elision alone bounds
  nothing for a *browse* scope: no producer earns one a watermark, so its
  entry grew with every non-contiguous read until it outgrew the plane's
  per-entry cap ([`account-sync-plane.md`](account-sync-plane.md) § Feeds
  and cursors → the W2.3 ruling (c)) — at the writer door on one device
  (the intake's put refused once the entry outgrew it: about 800 references
  under a real writer id while it encoded as a 32-integer array), and at
  the walk's merge door for two, where a union of two under-cap entries
  sealed past the cap and the merged row no nest accepts stalled
  `publish_pending`. So the seen-set carries a **per-scope element budget**
  (`fauna_core::seen_set::SEEN_SET_ELEMENT_BUDGET`, 1000 watermarks plus
  references, sized so a full budget of widest-encoding elements — 55 bytes
  each, the writer id a 34-byte byte string like every fixed-width id,
  [`serialization.md`](serialization.md) § Canonical IPLD dag-cbor →
  *Fixed-size byte arrays* — seals under the cap with headroom; it was 640
  while the writer rode as an integer array of up to 66 bytes),
  enforced **inside the join itself** as a closure: each writer present in
  the value gets an equal share of the budget for itemized references, one
  watermark slot each; past its share a writer's **oldest** references fold
  into its watermark. The fold is the one sanctioned **upward**
  over-approximation — the boundary grows to cover the folded prefix,
  unobserved gaps included — chosen because every alternative loses more:
  refusing the merge breaks grow-only-union convergence *and* leaves the
  producer dark past the cap, an *under*-approximated boundary (the one that
  lets a custodian drop something the user did observe); sharding a scope
  over several entries keeps precision but is exactly the forever-growing
  itemized log this transition rules out, and a sibling kind besides. The
  fold is extensive, monotone and idempotent, so the bounded merge is still
  a join — commutative, associative, idempotent, convergent — asserted in
  `fauna_core::seen_set`; a value an older build merged unbounded sits
  below its own fold and converges with a bounded peer on the next
  exchange, no wire or at-rest change. The one axis the budget leaves open
  is a scope's writer population (one watermark per writer, never dropped),
  bounded elsewhere — the nest sequencer plus the account's enrolled
  devices; behind every kind's join the walk's merge door sizes the merged
  value as the writer door sizes a put and skips, never journals, one that
  still outgrows the cap ([`account-sync-plane.md`](account-sync-plane.md)
  § Implementation status today → *the merge door beside the writer door*).
  Product-level **read markers**
  (which want per-item precision and history) are *not* the seen-set:
  they register as their own ordinary class-2 kind with its own
  retention policy — for conversations, `fauna.state.read-marker`
  ([`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md)).
  Observed items never silently leave a replica otherwise.


## The store device principal (R4 + R5)

One **device principal per store replica**: an Ed25519 device keypair + a
root-signed `DeviceAuthorization` covering it + a renewable bearer + the
owner `BackupKey` — the sync agent's shipped credential model
([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model: persisted
capability, app-dead bearer renewal via `fauna.auth.device_handshake`,
revocation = device-row deletion) promoted from "the agent's model" to **the
machine's model**. The identity seed is never part of the bundle.

- **Enrollment ceremony.** The first sign-in of a machine into an account
  mints the store's device keypair and obtains the root-signed
  `DeviceAuthorization` (signed wherever the seed is at ceremony time —
  today, the surface the user pasted it into; target, an existing device
  approving the new machine's request so the seed never travels). The bundle
  persists in **one shared per-user credential slot** (the agent's
  single-slot pattern generalized). Every co-located app mounts the
  store and uses its principal; the enrolling app retains nothing the others
  lack.
- **Slot sharing mechanics (T10 — resolved 2026-08-10, refutable until
  W5/W6 code).** The slot is `libs/fauna-credential-store` under one shared
  namespace keyed by account (`fauna-account-store`, account attribute =
  actor id hex) — replacing nothing: the agent's `fauna-sync-agent`
  namespace stays until its convergence (next bullet). Per platform:
  linux Secret Service (per-user by construction; the sanctioned 0600-file
  backend for headless boxes), windows Credential Manager (per-user by
  construction), macOS the crate's login-Keychain arm — the slot the
  co-located agent and tui share today; the keychain access group a signed
  app writes remains the *app-side* plane's target and is **app-only**, so
  the sandboxed extensions never share it (`apps/ios.md` § Credential
  Storage owns the plane; the pre-2026-08-26 wording here, "shared by …
  extensions", contradicted that ruling and is retired) — and iOS / android
  **the app's own platform store, lent to the crate over the registry's
  `FfiSecretStore` seam** (`apps/common.md` § Credential storage → *The
  shared Rust credential slots on the phones*, ratified 2026-08-26: no
  co-located second Fauna process exists on either, and neither has a
  native arm, so the crate's foreign arm is the slot there — rows prefixed
  `fauna-account-store/`). The
  slot persists the **durable** bundle — device secret key, its
  `DeviceAuthorization`, the owner `BackupKey`, and (R14 build design,
  2026-08-13; carriage code as of W5.4a — § Implementation status today) the
  account's **retained generation keys**, same custody class as the
  `BackupKey` beside them (`owner-key-material.md` § Path A-sibling-2 →
  *The schedule build design* → bundle carriage). Session bearers are
  deliberately **not** shared: each process mints its own via
  `fauna.auth.device_handshake` against the principal's grant, so there is
  no cross-process refresh race, and the sessions list stays an honest
  per-process record.
- **Web's principal (ruled 2026-09-26; built
  2026-09-29 —
  [`account-client-lifecycle.md`](account-client-lifecycle.md) § The client-side
  lifecycle → *The trigger fired* owns the program and the host).** A browser profile
  that hosts the account runtime is a machine like any other: it mints a
  device keypair at its first sign-in, obtains the root-signed
  `DeviceAuthorization` from the seed it holds, and its device row counts
  against the tier cap exactly as a desktop's does. The SPA keeps the
  bundle in the same per-origin, per-actor slot its account registry
  already rests in (`fauna_client_accounts::LocalStorageSecretStore`, the
  crate's web arm), under the localStorage posture § Local at-rest posture
  already accepts for web — and it is the **same bundle** every native
  machine keeps, the shared `principal_bundle` over a storage seam, never a
  web twin of its format ([`account-client-lifecycle.md`](account-client-lifecycle.md)
  § The client-side lifecycle → *The trigger fired*, ruling (4)'s build
  decision (c)). Two consequences are stated, not softened:
  browser storage is evictable, and an evicted bundle is a lost device —
  the next sign-in enrolls a fresh principal, the stale row is the user's
  to remove from the devices page, and the cap notice renders there as on
  every other app; and web's escrow-holder trust is the serving origin's
  nest identity as the SPA **pinned** it — the one TOFU pin it keeps per
  origin (`fauna_client_core::nest_trust::LocalStoragePinStore`; on web the
  origin is the nest, so one pin per nest shared by every account on it,
  exactly native's per-host pin on the medium the SPA has; the 2026-09-26
  wording "pinned per actor" meant this pin and is corrected 2026-09-29) —
  never what the nest claims about itself at a later handshake.
- **Agent-grant convergence (T11 — resolved 2026-08-10 in
  [`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model, the owner
  of that flow):** the agent's existing `[RenewBearer]` grant converges
  into the store principal — one principal per machine, not one per
  process. **Built 2026-08-15** (renewal preference, then the legacy
  grant's retirement over `fauna.sync.device_grant.revoke`). T11's
  further *"the machine appears once in the devices list"* is **true in
  code as of 2026-08-15** wherever a host names the machine's
  row: the topology was RULED 2026-08-15 at the owner — the
  principal is a credential of the machine's *named* device row, never a
  device beside it — and the build landed the `fauna.sync.device.adopt`
  move kind, the row-aware enrollment latch and the client-side gate. ⚠
  The remaining gap is wiring, not design: only tui and the sync agent
  pass an enrollment target today, so a machine running one of the other
  six apps and no agent still enrolls on the placeholder. The owner
  section's RULED block carries the decisions and rejected shapes; its
  § Implementation status today owns exactly what is wired. **Superseded
  2026-09-28:** the owner's RULED 2026-09-28 block retires the legacy grant,
  the placeholder and the adopt kind outright — one credential, one row, on
  every platform; built 2026-09-29, the owner's § Implementation status
  today recording the build.
- **Principal succession after a device delete (RULED 2026-08-15, row 49)
  — revival is a fresh enrollment, never a resurrection.** The user's
  delete tombstones the machine's writer key forever (the revocation
  memory is the control; its hardening is owned by
  [`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model). What
  makes the machine usable again is a **successor principal**, minted by
  the next ceremony-capable sign-in on that machine — the same silent
  first-sign-in ceremony above, plus exactly the mechanics that unstick
  the slot and store from the dead identity. Five decisions:
  1. **Trigger: enrollment-time, evidence-gated, automatic.** The ceremony
     first re-registers the held slot key idempotently (ordinary
     enrollment); a typed `GrantRevoked` answer for the held key — the
     nest's own memory that this identity was ended — triggers the
     successor mint. No new UI knob: the seed holder signing in *is* the
     authority, exactly as at first enrollment; the removed-state surface names sign-in as the path. The information flow is already
     right: only an authenticated register learns "revoked" — the
     handshake path stays deliberately opaque to a thief probing a dead
     key. REJECTED: un-tombstoning on "legitimate" re-enrollment (it
     re-opens the replay the tombstone exists to close — a
     captured root-signed grant must stay dead even after the user
     re-adds the machine); an expiring tombstone (a timer silently
     re-opening revoked authority — the no-expiry decision's own logic,
     inverted). **A second trigger, no nest round-trip (ratified
     2026-08-27, refinement 10 below): the shape *slot empty, or naming a
     key the stamped store does not* is evidence by itself** — the slot
     was lost while the store dir survived (a reset login keychain /
     Credential Manager / Secret Service collection, a backup older than
     the 2026-08-26 exclusion restored to a phone, a keyring swept by a
     tool) — and every assembly, seedless included, fences the store onto
     the slot's key rather than refusing the store it cannot open.
     **A third trigger, the fleet plane's own evidence (ruled 2026-10-01; built 2026-10-01): this machine's own `fauna.state.device-set` row reads `Removed` in its merged state.** That row is absorbing ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery, the device-set kind), so the principal is ended as a fleet member for good and no register can bring it back — the same fact the nest's `GrantRevoked` answer states, held on the device. A seed-holding assembly that reads it mints the successor by decision 2 unchanged, and the successor enrolls on the machine's own named row as a new fleet device (decision 5). It is its own trigger because the nest's answer does not always follow the row: a removal by key through the member-addressed door sends nothing nest-side until a sibling's pass revokes the grant ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (4) → *The nest half follows merged state*), a rebuilt box has no revocation memory, and a guardian-marked row keeps its grant by rule — measured there: the removed machine goes on authenticating, reads on by escrow recovery, and stays a non-member with nothing to tell it ([`../behavior/family-safety.md`](../behavior/family-safety.md) § Full visibility for young children → *The device marker* owns that case and what the marker promises). Three rules bind the build. The pump's enrollment step reads the own row **ahead of the registration latch** — consulted first, the latch answered `Current`, which is why the measured machine never noticed — and answers `RemovedFromAccount`. The authority is decision 1's: a seed-holding runtime reassembles and rotates (refinement 7), a seedless host surfaces the loud state and rotates nothing. And refinement 7's cap holds as it stands — one rotation per assembly chain, re-armed by a healthy enrollment answer, which a successor whose own row reads `Enrolled` gives — so an insider that removes each successor as it appears draws one rotation per removal it writes and no more: the accepted vandalism posture, paced by the vandal. **How the evidence travels.** The ceremony probe runs before the backend opens and cannot read merged state, so the reader is the pump's enrollment step — a full pass's and a seed pass's alike — and its finding rides the reassembly the worker already performs on that answer: the driver keeps the writer the row was read for beside the rotation cap, and the next assembly's probe, handed a finding that names the writer it holds, rotates before asking the nest anything. The probe is one shared function, so the native worker and web's host both carry it. Nothing durable holds the finding: the row is absorbing, so a crash between the read and the rotation costs one pass, and a cold start learns it from its first pass, one reassembly later. With the cap spent the probe registers nothing for a writer the finding names — a removed machine never re-registers. **The evidence reaches the machine where the nest's answer does not.** Removal evidence leaves a nest only by a secondary-leg run that found it at every linked replica ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 7), and at the bound nest only when the retention gate allows: a row is retired once every counted walker has walked past it, a mark counting while its key's grant stands ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (1)). So a removed machine whose grant stands is served its own removal before its nest gives the row up, and one whose grant is gone meets the nest's revoked answer instead. **A sign-out is not a removal (decided at the build).** A sign-out writes this same row for its own machine, ahead of the erase, and a seed-holding runtime that minted a successor on it would leave the signed-out machine a fleet member under a key the erase destroys, with a live grant on its row that nothing could retire. A sign-out is refused while a sibling instance serves the account ([`apps/account-scoping.md`](apps/account-scoping.md) § Concurrent instances — ratified design → *An erase refuses while a sibling serves the account*), but that probe degrades open, and the signing-out runtime itself can run a pass between its retirement and its shutdown — so the stop is the runtime's own. The retirement stamps the store's local meta with its writer **before** it writes the row, and the enrollment step, reading the row beside a stamp that names the writer it holds, answers `SignedOut` in every runtime on that store: no register, no reassembly. The stamp is the one thing this trigger adds at rest. It is local and never published, which is why it is the discriminator and the row's `removed_by` is not: any enrolled device writes that field, so a ward's device could pass its removal off as the guardian's own sign-out. It is erased with the store, and a seed-holding host start clears it: a sign-in outranks a sign-out whose erase never landed, and the surviving row then heals as any removal does. **The seedless host's answer moves with the read.** Behind the latch it answered `Current`; it now answers `RemovedFromAccount` at every pass, loudly, and rotates nothing. Its connect gate is opened from the latch at assembly and never closes, so a removed machine that was reading goes on reading — the verdict moved, the session did not. Proofs: `fauna-sync-engine`'s `a_latched_machine_whose_own_row_reads_removed_answers_removed_with_no_rpc` (the read, on the seedless host), `a_seed_holder_whose_own_row_reads_removed_mints_a_successor_unasked` and `a_sign_out_beside_a_second_seed_holder_is_not_undone`; tier_3 `conformance_account_plane_bind::the_guardian_marked_device_removed_on_the_fleet_plane_returns_as_a_successor`. Nothing new is reachable by this: a removed seed holder already keeps full account access, a machine the user deletes from the Devices page already returns this way once the nest answers, and succession stays the remedy for a seed in the wrong hands. Not adopted: rotating a seedless host on this evidence (it cannot sign the successor's grant), and treating the row as a reason to sign the app out (any enrolled device can write it, so a ward's device could then sign the guardian's out).
  2. **The mint is an in-place writer rotation inside the store's
     migration critical section** (the W5.3 mint-or-load race lesson
     binds — probe-then-act on shared state): mint a fresh keypair; move
     the slot bundle to the successor's namespace (the
     `DeviceAuthorization` re-signed by the ceremony; `BackupKey` and the
     retained generation keys carried over — same custody class,
     drop-on-`Shredded` unchanged); re-stamp the store's `META_WRITER_ID`;
     re-run enrollment per the topology (re-creating the named row
     the delete removed — re-registration is never blocked, the tombstone
     keys on the dead public key alone). The dead key is abandoned, never
     re-used.
  3. **The un-pushed tail is RE-AUTHORED, never resurrected** (the
     no-user-data-loss invariant is why wipe-and-re-bootstrap is not the
     answer): local journal rows and outbox intents not provably held
     remotely re-journal under the successor writer — they are this
     machine's own facts, and re-authoring is safe-by-construction on the
     plane's semantics (class-1 by CID is idempotent; class-2 re-puts as
     a fresh version of the same value and converges), so the bound is
     conservative: re-author anything not provably pushed. Rows the fleet
     already holds stay exactly as the old writer's authored history —
     history is never re-stamped. **Live-predecessor bound (ratified
     2026-08-27):** the predecessor may still be authoring elsewhere —
     the revocation trigger ends it, but the lost-slot trigger (decision
     1's second trigger) does not: a replica restored from an older
     backup rotates while the original device lives on. The bound holds
     unchanged, because the two mechanisms it rests on are already
     one-directional: the predecessor's LATER rows ingest normally (the
     walk's self-echo guard, refinement 8, covers a retired own writer
     only at or below what this replica already holds — a row beyond that
     is new history and lands), and a re-authored duplicate carries the
     SAME value under the SAME `merge_meta` (refinement 5), so LWW
     converges both copies to one entry fleet-wide. The whole cost of a
     live predecessor is a duplicate journal row; no write is lost and no
     write is reverted.
  4. **Live co-located processes are fenced by an append-time writer
     guard:** every local append checks `META_WRITER_ID` inside the
     append transaction and refuses typed (`StaleWriter`) on mismatch; a
     refused process reassembles, resolves the successor from the slot,
     and continues. This also hardens the W5.3 divergence class in
     general — a process holding a stale key gets typed refusals, never
     interleaved logs.
  5. **Fleet semantics: the ended device stays ended.** R14 severance of
     the old device stands; the successor self-enrolls as a NEW fleet
     device through the ordinary bootstrap and regains sealed access
     through the built escrow/top-up/heal machinery — never by
     inheriting the old device's standing. The old device-set entry's
     `Enrolled` rows are retired by the reclamation pass
     ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The
     generation machinery → *Fleet-scope reclamation*, clause (3)(d)),
     unchanged here.

  REJECTED as the default path: wipe-and-re-bootstrap (destroys the
  un-pushed tail; it stays the documented last-resort manual recovery for
  a corrupt slot). Nothing here needs a new nest kind — detection and
  refusal are existing typed answers (one existing answer gained a
  distinct CODE; refinement (c) below); the whole mechanism is
  client/store-side and additive. **BUILT 2026-08-15** —
  sequenced independently of the topology build (this mechanism
  also heals today's placeholder-row strand, so neither build gates the
  other); mechanism + verification in § Implementation status today →
  *Built — principal succession*. Eleven build-time refinements, recorded
  here rather than silently diverged from (the W5.6 discipline):

  1. **The slot is ACCOUNT-actor-keyed, not writer-keyed** (the T10
     layout: writer secret at the bare `actor_id_hex` root attribute,
     bundle items at `<actor-hex>/<suffix>`), so decision 2's "move the
     slot bundle to the successor's namespace" is a **re-stamp in
     place**: overwrite the root writer secret (read-back COMPARED),
     re-sign + store the `DeviceAuthorization`, and touch
     `backup-key`/`generation-keys` not at all — they are already
     writer-independent. No custody move exists, so no trim-seam widening
     and no resurrect-from-memory hazard.
  2. **The `META_WRITER_ID` re-stamp is the FENCE and comes first**
     within the mutation sequence: the append-time guard (decision 4)
     reads the meta inside the append transaction, so re-stamping before
     the tail walk guarantees no late old-writer row can slip past the
     conservative bound. The fence stamps a durable pending-re-author
     marker in the SAME transaction, making the re-author crash-resumable
     and runnable by whichever process next holds the pump — no seed
     needed to re-journal.
  3. **The typed evidence needed a distinct wire code**:
     `fauna.sync.device_grant_revoked`
     (`fauna_protocol::RpcError::CODE_SYNC_DEVICE_GRANT_REVOKED`; client
     classifier `fauna_client_sync::is_device_grant_revoked`) — the generic
     `invalid_grant` also covers capability-missing and bad-signature,
     either of which must never fire a rotation. Additive both ways: an
     older nest keeps the generic code and a new client stays in today's
     retry loop; the anonymous handshake stays opaque.
  4. **The ceremony probe is the trigger's carrier, and it is
     GRANT-FIRST.** The enrollment latch is content-addressed on the
     grant wire, so a nest-side delete never invalidates it — only an
     un-latched ceremony-time re-register can surface the evidence.
     The probe leads with `device_grant.register` (the nest's
     revocation-memory check runs ahead of its row check), so a probe on
     a deleted machine learns "revoked" WITHOUT re-creating the row the
     user just deleted; only a genuine `not_found` falls back to
     register-then-grant. Bounded (a named budget), so an unreachable
     nest delays no sign-in: an offline sign-in skips, and the evidence
     re-derives at the next ceremony-capable assembly. **The latch is also
     a memory about one nest replica (ruled 2026-09-30, built 2026-09-30:
     the enrollment step's `latch_void` on a bind verification, the device
     client's `NotRegisteredHook` wired by every production host to
     `principal_bundle::void_grant_registration`, and the `Removed` read
     ahead of any register — proven by `conformance_account_plane_bind`).** It
     records that *a* nest holds this grant and names none, so on a second
     nest of the same account, or on a box rebuilt with its identity and an
     empty database, it reads current with no network call, the device
     never registers there, and the device handshake answers
     `not_registered` to a connect retry that loops for ever. Two rules
     close it: the latch is void whenever the bound replica differs from
     the store's settled replica, and the register-then-grant step runs
     again ahead of that pass's other legs
     ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg owns
     the replica id and the settled replica); and a `not_registered` answer to
     the device handshake itself voids the latch and re-registers through
     the owner session by this same grant-first probe (so a machine the
     user deleted still learns "revoked" and is not re-created), never a
     bare retry. A rebuilt box has lost its revocation memory, so one
     refusal moves to the device: a machine whose own device-set row
     reads `Removed` in its merged state never re-registers, on any
     replica — and registering needs the owner session, which a removed
     seedless machine never had (the removed principal never does; on a
     seed-holding machine its successor does — decision 1's third trigger). A host that cannot register —
     the seedless one — waits for a signed-in app on the same machine,
     as it does for escrow recovery: which process registers, and when,
     is the seed-leg role's
     ([`account-runtime.md`](account-runtime.md) § Multi-instance
     concurrency).
  5. **Re-authoring preserves the LWW stamp and re-seals at publish.**
     The store holds plaintext entries; the T14 seal is per-publish and
     AAD-binds `{writer, seq, scope, item}` — so the tail re-puts each
     distinct entry's CURRENT value with its `merge_meta` verbatim
     (a re-stamp would outrank fleet writes the original never beat),
     fresh coordinates, fresh envelope. Predecessor rows stay in the
     journal as local-only history (a burnt predecessor's carried rows
     excepted — refinement 11 compacts them); class-1 local authorship has no
     production path yet, so its re-author leg is a recorded duty at the
     walk, not code. The outbox needs no walk — verified at build: no
     production intent producer exists and payloads replay verbatim.
  6. **The rotation refuses a Degraded migration lock** (unlike the
     mint's degrade-open): succession always has a working fallback —
     keep today's state, retry at the next sign-in — so an unserialized
     half-rotation is never worth the fork risk.
  7. **A seed-holding runtime treats a `RemovedFromAccount` pump answer
     as reassemble** — a reassembly IS a ceremony (the seed holder's
     presence is decision 1's authority), so a live session whose device
     is deleted mid-run heals without waiting for an app relaunch; a
     seedless host surfaces the loud state and rotates nothing. **Capped
     at one rotation per assembly chain**, re-armed only by a HEALTHY
     enrollment answer: a fresh successor key cannot have been
     legitimately tombstoned, so a nest that revokes it too is broken or
     hostile — without the cap it would draw an unbounded
     reassemble→mint loop out of one session (found as a livelock under
     the sticky-revoking test nest). **The cap counts rotations, so the
     reassembly is paced beside it: one per backstop tick.** A
     reassembly whose probe rotated nothing — its register faulted, or
     its budget lapsed — leaves the cap armed, and the new assembly's
     prologue is answered removed again; read by the cap alone, that
     answer reassembled on every prologue with no wait in the loop
     (measured against a test nest
     that answers revoked and faults by turns: 4,407 registers in 30 s
     and no command served). So a runtime that has reassembled on a
     removed answer stays loud on the next one and reassembles nothing
     until a healthy enrollment answer or the backstop tick. The tick's
     pass, full or seed, may reassemble once more, which is how a probe
     that skipped once against a slow nest still heals; a relaunch
     starts armed. Proof: `fauna-sync-engine`'s
     `a_removed_heal_whose_probe_rotated_nothing_waits_for_the_backstop_tick`.
  8. **The store keeps a PERMANENT retired-writers memory, stamped
     atomically with each fence, and the walk's self-echo guard covers
     it.** Found by the tier_3 proof (V13): a retired identity's
     published rows live on the fleet's feeds forever, this store holds
     them in their locally-AUTHORED journal form (`item_ref` carries the
     local entry counter), and an ingest re-derives the wire form
     (`entry_version` = origin seq, the provenance rule) — so a
     revived machine walking its own former rows false-equivocated
     against its own history on every pass. A row of a retired own
     writer is now the self-echo case: held, accounted (its frontier
     slot advances — nothing publishes as it again, so the slot has no
     published-high-water second meaning), never re-ingested.
  9. **The probe's register target follows decision 2's third condition
     of the topology, and the latch is preferred only where the
     app's answer allows it** (found + built 2026-08-26). `Some(row)`
     from `enrollment_target_device_id` keeps the latch-first target the
     pump's enrollment step uses; `None` — this provision mints, or may
     still mint, a legacy grant onto the machine row — targets the
     writer-pub-hex placeholder regardless of the latch, the one row a
     legacy mint can never name. The evidence gate is untouched by the
     move: the tombstone check keys on `(actor, auth_device_key)` ahead
     of the row check, so it answers on any row. The race it closes, its
     reasoning and the three bounds on it are subject matter and
     live at their owner — [`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md)
     § Credential model → the RULED 2026-08-15 block, its
     2026-08-26 ⚠ notes.
  10. **The lost-slot arm — the rotation fired on the shape itself
     (ratified + built 2026-08-27).** Until then
     an empty slot over a stamped store minted a fresh writer the store
     then refused, and every later launch found the slot FULL of that
     doomed key — a permanent silent strand (`apps/common.md`
     § Credential storage → *The shared Rust credential slots on the
     phones* named the residual). Now the mint-or-load stays exactly as
     it was — an empty slot still mints — and the heal is **one check at
     every assembly, right where the backend opens and before the store
     does** (`principal_succession::lost_slot_heal`): the store's stamped
     writer is read; equal to the slot's key, or unstamped, nothing
     happens (the common path takes no lock); otherwise, under a Held
     migration section (refinement 6's refusal on Degraded — the slot is
     re-read there, and a sibling's change restarts assembly), **the
     fence lands from the stamped writer onto the slot's key** — the same
     `rotate_writer_identity` the revocation arm lands, so the retired
     memory, the append-time guard and the pending-re-author marker all
     follow, and the pump's next pass re-authors the tail under the
     slot's key. Because the check keys on *disagreement*, not on
     emptiness, the same arm heals every install a pre-2026-08-27 build
     already stranded (slot = the doomed writer, store = the old one — no
     third identity is minted), the revocation rotation's own crash
     window between its slot write and its fence, and the loser of an
     unserialized W5.3 mint race. Three bounds: **(a) a RETIRED writer
     found in the slot is never put back to work** (the ruling's
     "abandoned, never re-used"; the nest may hold its tombstone) — the
     heal mints a fresh key into the slot and restarts assembly, capped
     at one such re-mint per runtime worker (a relaunch re-arms it — a
     slot that comes back retired after a fresh mint is a credential
     store not retaining writes, refinement 7's livelock in a new coat);
     **(b)
     the grant follows the ordinary ceremony arm, not the heal**: a
     seed-holding assembly has already minted a grant over the slot's
     key inside its section block before the heal runs, and a seedless
     host (the agent after a desktop keyring reset) fences without one —
     its store opens and its tail is preserved, its data client lags
     until the next seed-holding sign-in re-runs the ceremony, exactly
     refinement 7's seedless posture; **(c) the predecessor's device row
     on the nest lingers** (a lost slot revokes nothing nest-side; only
     the user's delete does) — the same trigger-(b) removal gap decision
     5 already records, one more row wide. **The pending-re-author marker
     is multi-valued from the same day**: a fence landing while an
     earlier rotation's marker is still pending (two rotations with no
     pump pass between — a crash window, or a lost slot right after a
     revocation rotation) keeps the earlier predecessor in the walk
     instead of overwriting it, additively (the newest predecessor stays
     in the original 32-byte key an older binary reads; the rest sit in
     a second key it ignores — its walk clears only the newest, and a
     newer binary later walks what it left).
  11. **The journal-bound writer — the inverse arm, and the burnt-journal
     heal (ratified + built 2026-09-15).** Refinement 10 heals *slot lost, store kept*; nothing healed
     the inverse, *slot kept, store lost*, and the shape is reachable
     (a config dir deleted by hand while the platform credential store
     keeps the key; a phone reinstalled with its keychain intact; every
     pre-2026-09-15 e2e relaunch, whose carry restored the slot alone).
     The rule that decides it: **a writer's seq counter has exactly one
     home, its journal, so a writer lives exactly as long as its
     journal.** A fresh journal under a surviving key re-issues seqs
     `1..` the previous life already used; the nest refuses the ones
     whose item has a head (`stale_writer_seq`, wedging
     `publish_pending` for good) and — its head being per item —
     ACCEPTS the rest at coordinates already on the feed, which every
     other replica then meets as journal equivocation, since row
     uniqueness and the equivocation refusal are keyed per `(scope,
     writer, seq)`. Three arms, one ruling:
     **(a) A key LOADED from the slot over a store with no stamped writer
     is abandoned on the spot**: the assembly's heal arm
     (`principal_succession::lost_slot_heal`, keyed on the mint-or-load
     resolver's provenance — the key this very assembly minted is the
     first launch and exempt) mints a fresh writer into the slot, stamps
     the fresh store with it and records the loaded key as retired
     (`retire_unjournaled_writer`: no re-author marker, there is no
     journal to re-author), and restarts assembly. The machine is a NEW
     fleet device; the old device row lingers exactly as bound (c) of
     refinement 10 already records. Seedless hosts included — the mint
     signs nothing and the grant follows at the next seed-holding
     ceremony, bound (b) — and the re-mint cap of bound (a) counts it.
     **(b) For the CURRENT writer, every own row on any feed is held at
     its coordinate under the same item — and, while the held row is
     still the entry's latest write, with the entry's own content; a row
     that is not — above everything the journal holds, at a coordinate
     below that high-water the journal holds nothing at, at a held
     coordinate under a different item, or at a held coordinate under the
     same item with OTHER content — is the burnt-journal signature** (a
     store dir restored from an older backup while the slot kept the key;
     a pre-ruling build that already reused a slot over a fresh dir). The
     SAME-ITEM arm — the last of the four — was ruled and built
     2026-09-16, and it is the one no other arm can see: it is also the
     commonest restore shape, since
     a restored backup's next writes hit the same handful of preference
     keys the previous life wrote after it, leaving every served
     coordinate held and every item matching. The journal row carries no
     value to compare with — but the ENTRY does. An own row is re-sealed
     from the entry's current plaintext for exactly as long as
     `entry_version` agrees (`publish_pending`'s own equality, the same
     one it re-seals by), so a served row that is still the entry's
     latest write and yet carries other content was authored by another
     journal: positive evidence, not a heuristic. Undetected, the cost is
     not a lost value but a permanent WEDGE — the nest refuses the reused
     coordinate forever, and since every local write publishes in journal
     order, that one row holds every later one behind it and nothing this
     machine writes reaches the fleet again. Conservative in the other
     direction, exactly as the different-item arm is: a row the entry has
     moved past, an item the entry no longer holds, an unopenable row,
     and a class-1 `Cid` at the coordinate are all NO evidence and stay
     echoes — a rotation is a heavy act and fires on positive evidence
     only. **Every arm fires on an OPENED row (ruled 2026-09-16).** `origin_writer` and `origin_seq` are feed metadata
     outside the seal — the nest's word about a row, which a hostile nest
     or a same-account sibling putting under this writer's id may simply
     make up — so the two arms that have nothing but those coordinates to
     reason from must open the envelope before they rule, exactly as the
     two plaintext-comparing arms already do: a row that opens carries
     the in-seal writer signature and so was sealed by a journal holding
     this writer's key, which only a previous life of this store can be;
     a row that does not open was sealed by nobody, and it is accounted
     `unopened`, left to be re-served, and otherwise ignored. Both arms
     are live, not theoretical: the high-water arm meets a row a forger
     places above the journal, and the gap arm meets one placed below it,
     since a writer's seq counter is cross-scope while the high-water is
     per-scope — so every seq this writer spent on the sibling fleet
     scope is a legitimate empty coordinate here — and backstop 2 walks
     from a zero frontier on every pass, which re-presents a planted gap
     row for ever. Ruling on the coordinates alone let the nest by itself
     force a rotation: a reassembly and a fresh enrollment once per
     runtime worker and once more per relaunch, for one fabricated row.
     The unopened row is not relayed onward either, which is the relay
     plane's one carve-out and is ruled below (*a refused row's relay
     residue*). CURRENT writer only: a FOREIGN writer's same-item row with
     other content at a held coordinate is corruption or a forged relay
     and keeps its refusal (*a foreign writer's second row at a held
     coordinate is carried*, below, which says so). The
     walk refuses it (`WalkReport::own_burnt`): never ingested — an
     ingest would seed the counter past it by accident, the pre-ruling
     reading "a replica restored from a backup that predates the row
     falls through and ingests it" was exactly the burnt case — never
     echoed, the frontier left below it so it stays served and counted,
     and the writer stamped BURNT in store meta. A RETIRED own writer
     keeps the live-predecessor bound unchanged (at or below held: echo;
     above: new history, lands) — except the retired writer the burnt
     verdict names, whose held coordinates are CARRIED (*the retired burnt
     writer's rows are carried*, below). **(c) The heal is the ordinary fence:**
     the pump reassembles on a burnt verdict (once per worker), and the
     heal arm meets *stamped == slot == burnt* and rotates it onto a
     fresh mint — `rotate_writer_identity`, so the retired memory, the
     append-time guard and the re-author marker all follow, and the next
     pass re-journals the un-pushed tail under the successor with its
     `merge_meta` preserved; the burnt verdict is inert once the stamp no
     longer names it. **Nest half:** `fauna.account.state.put` refuses a
     `(scope, writer, seq)` it already holds on ANY item, live or
     collapsed, under the same `stale_writer_seq` code (the client's
     remedy is the same; `StateEntryError::SeqReused`) — defense in depth
     for the pre-ruling clients still in the alpha fleet, and compat-safe:
     no legitimate client re-sends an accepted coordinate, and a
     per-writer *monotone* head was REJECTED because pre-ruling clients
     publish out of seq order legitimately (an inline `put_preference`
     publish while an earlier row is still pending). Since 2026-09-16 the
     refusal is also STRUCTURAL: `sync_changes` carries a UNIQUE index
     over `(folder_id, origin_writer, origin_seq)` —
     `idx_sync_changes_writer_coordinate_unique`, superseding the plain
     index of the same columns — so a reused coordinate is
     unrepresentable rather than merely refused at one write path. Its
     creation WARNS on failure and never aborts the boot, because the
     refusal covers new puts only: a nest that ran a pre-ruling build may
     still hold a duplicate, and one that does keeps the plain index and
     logs the duplicate count instead — which is how a long-lived box
     reports its own residue (c) without a hand read of its database.
     Nothing removes a duplicate; both rows keep serving
     (`principles.md` § No user-data loss), and how a replica MEETS one is
     the client-side arm's ruling — *a foreign writer's second row at a
     held coordinate is carried*, below. Pinned by
     `a_reused_writer_coordinate_is_refused_at_the_schema` and
     `an_upgrade_over_a_pre_ruling_duplicate_boots_and_keeps_both_rows`
     (`migrations.rs`). **Success, as
     ruled:** a store opened under a surviving key over an empty journal
     never publishes at a coordinate the previous life used, accepted or
     refused; never raises equivocation against its previous life at this
     or any other replica (a second replica walks both lives as ordinary
     history); and the previous life's rows are ordinary history under
     the plane's own merge rule — a newer local write wins by its LWW
     stamp, never by writer identity, which is how the retire arm and the
     live-predecessor bound agree. REJECTED: seeding the counter from the
     nest's head, and walking before the first write — an offline first
     launch cannot, and the nest is not the whole record (a peer may hold
     a coordinate the nest never saw: the relay plane's own premise).
     Pinned by `a_surviving_slot_over_a_fresh_store_retires_the_key_and_a_second_replica_walks_both_lives`
     and `a_journal_restored_from_an_older_backup_is_found_burnt_by_the_walk_and_rotated`
     (`account_runtime.rs`, over the stateful fake, which now refuses a
     reused coordinate as the real nest does) and the nest's
     `a_coordinate_this_writer_already_used_is_refused_on_any_item`
     (db + conformance). **The e2e relaunch carry now carries the replica
     beside the slot** — [`e2e-launch-isolation.md`](e2e-launch-isolation.md)
     convention 10 says which — so a relaunch models a true restart
     rather than exercising arm (a) on every module boundary. **The burnt
     residue, compacted** (2026-09-16): once the heal retires the burnt
     writer, the walk echoes its rows at or below what the journal holds,
     so a burnt row at a coordinate the previous life used for a different
     item would shadow the fleet's row there at the healed replica for
     good. The tail re-author therefore compacts a burnt predecessor's
     carried rows: for the predecessor the walk's burnt verdict names (the
     verdict outlives the fence), every class-2 row above its published
     high-water — the bound the re-author itself reads — whose entry it
     re-put under the successor is deleted by exact coordinate and item,
     together with the relay-plane row recorded at that coordinate (so no
     peer is served it), in one transaction that refuses the store's
     current writer (its append counter would re-issue the freed seqs),
     committed before the marker's compare-and-delete clear (a crash leaves
     the marker and the compaction owed; a re-run is idempotent). The
     authority is [`account-data-plane.md`](account-data-plane.md) § Store
     logical schema — a writer may compact its own log's superseded class-2
     rows, and a carried row is superseded by its re-put, which keeps its
     value and `merge_meta`. With those rows gone the predecessor's held seq
     falls back to its high-water, and the previous life's rows at the freed
     coordinates walk in above held as retired history. Never compacted:
     class-1 rows (no re-author path, refinement 5), a row whose entry
     vanished, and the carried rows of a writer no walk found burnt. Pinned
     by
     `a_burnt_lifes_row_at_a_coordinate_the_old_life_used_for_another_key_is_compacted_and_both_replicas_converge`
     (`account_runtime.rs`: both replicas agree on both keys, and every row
     of the retired writer the fleet serves sits under the same item in the
     healed journal) and the store-level compaction tests in
     `principal_succession.rs` and `store.rs`. **The residue below the
     bound, ruled (2026-09-16):** two
     shapes stayed shadowed at the healed replica — (i) a burnt row the
     nest refused *below* a slot a later out-of-order inline publish had
     raised (neither re-authored nor compacted; the fleet's row at its
     coordinate dropped as an echo for good), reachable on a current build
     after an offline restore, since the prologue publishes before it
     walks, an offline prologue walks nothing, and the pump serves queued
     commands ahead of the reconnect pass; (ii) a store healed by a build
     predating the compaction, its marker cleared and its retired slot
     raised by the walk's echoes. Two rulings close them. **The ordered
     own publish:** every local write on the nest leg — a put, a
     tombstone, the walk's merged value — publishes through
     `publish_pending`, every unsent own row before its own, so the own
     slot is always the contiguous ATTEMPTED prefix and no inline publish
     can raise it past an unsent row. This also closed a general strand
     (an accepted inline publish racing the reconnect pass silently
     orphaned every offline row before it) and makes (i) unreachable from a
     current build against any nest; the nest's leniency toward
     out-of-order publishes stands for pre-ruling clients. One write
     stands outside it: the sign-out severance's `Removed` row, a dying
     writer's last word, sent even when the drain ahead of it fails —
     ruled in [`account-data-taxonomy.md`](account-data-taxonomy.md) §
     The generation machinery → *Fleet-scope reclamation*, clause (4).
     **A row refused for room is parked (ruled 2026-10-01; built 2026-10-02 — [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md) § Implementation status today).** Measured on the delegable scope at its cap of 4,096 live rows: a device read one new conversation, the nest refused that marker's put `scope_full` at every pass, and nothing the device wrote afterwards was sent, a preference record the nest already held a row of included. A refusal for room says nothing against the row, and on that scope nothing is owed an order across items. So there the publish does not stop at a `scope_full` answer. The device records the row's coordinate durably as *parked*, beside the slot, and goes on to the next row. The own slot is then the prefix of rows that are settled: acked, superseded locally, or parked. A parked row is owed by name. Every publish retries the parked rows ahead of the rows above the slot, a preference record's first and the rest oldest first, and stops retrying them at the first one refused for room again, so a full scope costs one refused put a pass. A parked row leaves the list on three events and no other: the nest accepts it; the walk meets it as an own echo, which is a sibling's diff having pushed it; or the entry has moved past it, and then the later row carries the value as for any superseded row. A retry the nest answers `stale_writer_seq` stays parked for that echo to settle, and does not stop the rest. The tail re-author counts a parked row as part of the un-pushed tail, for a rotation and for the burnt-journal heal alike, and the compaction of a burnt predecessor's carried rows covers it. **Why the order may break on that scope.** Its items are independent of one another: four latest-wins records and two joins, and a put's `replaces` list names only rows the put's own value covers (`delegable-scope-reclamation.md` § Delegable-scope reclamation, part (2)). The order within one item is kept, because a later row of the same item supersedes the parked one locally before either is sent. The nest takes a writer's rows out of seq order (the nest half above refused a per-writer monotone head), and a walker does too: gaps in a writer's run are legal, and every full pass's reconcile serves a row that an incremental walk's slot would skip. **The fleet scope keeps the stop.** Its puts are ordered against each other: the reclamation pass counts a covering row that is still local-only precisely because the publish cannot send the `Shredded` marker ahead of it, and a full fleet scope is given room by that pass's retires, which need none (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope reclamation*, clause (1)). **A store opened by a build older than this rule** ignores the list and finds its slot already past the parked rows. They are then residue (a)'s shape below: the value lives in the entry and the next local write to the item carries it. **Refused.** *Leaving the slot below the parked row and sending what lies above it:* the walk's own echo of any later row raises the slot past the parked one at the next reconcile. *Deleting the refused row from the journal:* its coordinate would be issued again, and the peer leg may already have served the row at it. *Stopping as today and making room first:* what makes room is a conversation being left, which may never happen, and until then the device's preference writes reach no one.
     **The retired
     burnt writer's rows are carried, never echoed:** for the RETIRED
     writer the walk's burnt verdict names (`burnt_writer_id` outlives the
     fence and is read live, per row, as `writer_relation` is), a held
     coordinate vouches for nothing — the row there is the burnt life's,
     which the fleet may serve under another item, under the same item
     with another value, or not at all — so the served row goes through
     the ordinary class-2 apply and, when it changes the entry, is
     re-journaled as this replica's own row with its `merge_meta` verbatim
     (the re-put shape); nothing is ever written at the retired
     coordinate, and the burnt life's relay row there under another item
     is retired for the fleet's (`StoreBackend::relay_retire_shadowed`); a
     coordinate the burnt journal holds nothing at (freed by the
     compaction) ingests as retired history. Idempotent — every later
     presentation is a `KeepCurrent` — which a compact-and-re-put could not
     be: wherever the feed serves BOTH rows at the coordinate for good (a
     peer still relaying the burnt life's row beside the nest's — the
     double-served shape below), the sketched compaction would re-put a
     fresh successor row on every walk.
     Counted as `WalkReport::retired_carried`. Pinned by
     `an_inline_put_publishes_the_unsent_own_rows_before_its_own`,
     `a_burnt_row_below_a_raised_slot_is_carried_and_the_healed_replica_converges`
     (a forged legacy store in shape (i): the fleet's row converges, the
     relay plane serves the fleet's item at the coordinate, the journal is
     untouched, the successor's log is stable over further passes) and
     `a_double_accepted_coordinate_is_carried_without_a_repeated_re_put`
     (`account_runtime.rs`; the stateful fake's `accept_reused_coordinates`
     serving both rows on one leg, the stand-in for the two-leg shape),
     and the relay primitive's test in `store.rs`. Until the CAS-blob
     bridge was deleted 2026-10-01 it dissolved the shadow on its own for a
     bridged preference kind, so the carry arm's real subjects were then
     the non-bridged class-2 kinds; no kind bridges since
     ([`config-dissolution.md`](config-dissolution.md) § The `__config`
     dissolution schedule → *The closure order*, step (5)). **Residue that stands, named:** (a) on a LEGACY store in shape
     (i) the burnt life's refused values never reach the fleet by
     themselves — they live in the entry, and the next local write to the
     item carries them; a current build cannot produce the shape, so this
     is accepted; (b) a same-item coordinate under other content was an
     echo in both arms — the journal row carries no value — but the ENTRY
     does while the row is its latest write: RULED and built 2026-09-16,
     the signature's same-item arm above; (c) a coordinate a pre-ruling
     nest served under two items is met by every OTHER replica as a
     foreign writer's second row — RULED and built 2026-09-16, the arm
     below. The nest's UNIQUE coordinate index (the nest half above) means
     no nest serves the shape by itself any more, but the shape outlives
     the nest's part in it: a peer relays it (the arm below says how), so
     the carry arm, not the index, is what makes a replica safe; (d) a permanently refused own row's relay row stays servable
     to peers — RULED and built 2026-09-16, *a refused row's relay
     residue* below. **A foreign writer's second row at a held coordinate is carried
     (ruled 2026-09-16; its live
     source restated at the 2026-09-27 compat-remnant sweep).** The nest refuses a reused
     coordinate, but the relay plane records an own row BEFORE its send
     (*a refused row's relay residue* below), so a peer that pulls a burnt
     writer's row during a nest outage keeps relaying it after the nest
     refuses that coordinate for good (another life spent it): the
     refusal retires the row only from the burnt replica's own relay
     plane, and a peer is told nothing. So a feed can serve two items at
     one `(scope, writer, seq)` for good — the nest's row and the peer's
     relay — and every replica but the healed one meets that
     double-served coordinate as a FOREIGN writer's rows, on either leg,
     in either order: the first ingests, and the second met
     `ingest_state`'s equivocation refusal, which aborts the walk's page.
     The incremental walk self-cleared — the first row's frontier advance
     gates both rows off the next page (the nest's `s <= slot` serve
     gate) — but the full pump pass's zero-frontier `reconcile` re-met the
     row every pass, forever: no walk and an `errors` entry per pass, a
     never-healing client-causable wedge over a relayed duplicate
     (under [`nest/common.md`](nest/common.md) § Client-state
     recoverability it fails the per-object carve-out's *contained*
     condition). The refusal buys class-2 no security: a second row at a
     coordinate must be AAD-bound to it, so only a fleet-key holder can
     produce one, and such a holder can write any value at a fresh
     coordinate and have the LWW/CRDT merge take it — the refusal is a
     corruption tripwire, not a defense (row-level writer authentication,
     [`account-sync-plane.md`](account-sync-plane.md) § Admission is
     carrier-level, keeps the AAD binding, which the carry preserves). So
     the walk, once it has opened a foreign row and finds this journal
     holding that `(scope, writer, seq)` under a DIFFERENT item, carries
     it exactly as the retired burnt arm does: the ordinary class-2 apply,
     re-journaled as this replica's own row only when it changes the
     entry, nothing written at the coordinate, idempotent across passes,
     the walk paging on; counted `WalkReport::double_served` — its own
     counter, so genuine equivocation stays visible — and warned per row.
     Two things the arm deliberately does NOT do. It retires no relay row:
     `relay_rows` is keyed `(scope, writer, item)`, both duplicates were
     already relaying onward, and a peer must be served exactly what the
     nest serves so it meets the same shape with the same arm (the burnt
     arm's `relay_retire_shadowed` is for a row the nest REFUSED, which is
     nobody's row — retired at the burnt replica only, since the shadowed
     row's journal lives there). And it does not cover a same-item row with OTHER
     content at a held coordinate: the nest collapses per `(item,
     writer)`, so that shape is corruption or a forged relay, and the
     refusal still aborts on it. That carve-out is about a FOREIGN
     writer, and stays exactly as written: under the CURRENT writer the
     same shape is not corruption but the burnt signature's same-item
     arm (bound (b) above), because there the entry says which journal
     authored the served row and a foreign writer's does not.
     Class-1 is untouched — the record plane
     keeps refusing a changed cid at a held coordinate, which
     [`message-segment-store.md`](message-segment-store.md)'s
     conv-migration constraint (i) rests on — and the older ruling that
     the `bail!` needs no remedy of its own
     ([`account-data-plane.md`](account-data-plane.md) § Implementation
     status today → *principal succession*) is the OWN-writer case and
     stands as such. Pinned by
     `a_second_replica_walks_a_double_served_coordinate_cleanly`
     (`account_runtime.rs`: three full passes on a second replica, no
     `errors`, `double_served` = 1 each, both items' values converge, one
     journal row at the coordinate, both relay rows kept, the successor's
     later rows landed, no repeated own row). The duplicate is two feeds'
     rows, never one nest's, so there is nothing nest-side to dedup: the
     coordinate index keeps a nest's own feed free of it. **A third carry
     arm, an attested predecessor identity's delegable row (built
     2026-10-01):** a generation-0 row that opens under a predecessor's
     delegable schedule and not under this replica's own is carried the same
     way, counted `WalkReport::inherited`, with the predecessor writer's
     frontier left where it was; the rule, its limits and its tests are
     [`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md)
     § Re-key scope's (*The account-state plane's generation-0 delegable
     rows are carried by the successor's walk*). **The fleet scope's
     inherited carry (built 2026-10-01):** the same arm carries one
     fleet-only kind, a predecessor's mint record for a generation whose key
     the walking device holds; the rule and its gate are the succession
     rider's ([`owner-key-material.md`](owner-key-material.md) § Path
     A-sibling-2 → *Rotation*). **A refused row's relay residue (ruled 2026-09-16).** The relay plane records this
     replica's own row BEFORE the send (`AccountStatePlane::publish`)
     because a row a nest OUTAGE keeps un-published is exactly the row the
     peer leg exists to carry; the premise that a peer may hold a
     coordinate the nest never saw (the REJECTED seeding above) is about
     rows the nest has not seen YET. A `stale_writer_seq` refusal is the
     nest's FINAL word on the coordinate — it will never hold this row
     there — so the rule: **the relay plane serves what the nest holds or
     will hold, never a row it refused for good.** At the refusal the
     publish retires every relay row at `(scope, writer, seq)`
     (`StoreBackend::relay_retire_at`, one delete shared with the
     compaction and the shadowed-row carve-out); the local row stays, its
     value lives in the entry, and the walk's own verdict settles the
     rest. The WHOLE wire code is the refusal, not a split of it: the nest
     deliberately collapses a non-advancing head and a reused coordinate
     into one code because the client's remedy is the same, and both are
     permanent for that coordinate — what differs is what the nest holds
     instead, which only the walk can see. Its two arms: **a replay** (the
     nest recorded the row but the reply was lost; the re-send is refused)
     — the row sits above the un-advanced own slot, the walk serves it
     back, and the own echo, being proof the nest holds it, re-records the
     relay row from the served envelope and advances the slot (the echo
     arm records unconditionally: a no-op under the store's newer-seq
     guard whenever the row is already there, so it is the ONE repair
     path, and a backfill of own rows for a store from before the relay
     plane); **a burn** (another life spent the coordinate) — the walk's
     burnt verdict rotates the writer and the heal re-authors the value
     under the successor, whose publish records the successor's relay
     row; and every burnt verdict on the current writer makes the relay
     plane at that coordinate the fleet's word at the verdict — this
     replica's rows there retired, the served row recorded — not first at
     the heal's compaction, so no peer meets the burnt row in the window
     between (a peer served both would meet a foreign writer's second row
     under the same item with other content, the one shape whose refusal
     still aborts its walk). **The carve-out, from the same day's
     open-first ruling (arm (b) above):** a row served under this
     replica's OWN CURRENT writer, at a coordinate this journal cannot
     vouch for, that opens under NO key this device holds is recorded in
     the relay plane not at all — the single exception to the plane's
     otherwise unconditional rule that every coordinate-valid row is
     relayed verbatim, `unopened` rows included, because a relay does not
     editorialize. Relaying ANOTHER writer's unopenable row honestly
     mirrors what the nest serves, and some reader somewhere holds the
     key; this one would be asserted onward to every peer as this
     writer's own word, and no reader anywhere can ever vouch for it. It
     is the narrowest shape the plane refuses to record, and the only one. **Recording stays unconditional; forgetting is a separate, later act (ruled 2026-09-27).** The plane's other exits all remove rows it has already recorded: a row the nest confirmed retired, a row the nest refused for good (the *refused row's relay residue* below), and a row dead everywhere — sealed under a generation merged state reads `Shredded`, or a gen-0 item the reclamation pass forgets, whoever wrote it ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (3)(h), which owns that rule). Each is idempotent, so when a reconcile or a lagging peer serves such a row again, the walk records it again and the next forget removes it again. None of them withholds anything at the record point.
     **The one trade, accepted:** `relay_rows` is keyed `(scope,
     writer, item)`, so the refused publish's upsert had already replaced
     the writer's accepted row for that item, and the retire leaves the
     item with no relay row under that writer until the nest serves the
     accepted row again — the incremental walk for a replay (above the
     slot), the full reconcile's zero-frontier pass for a row below it; in
     that window a peer meets nothing for the item rather than a row
     nobody can vouch for, which is the honest state. **Legacy-only
     surface:** `publish_pending`'s cap-skip arm retires the same way (the
     door refuses an over-cap row before the local write today, so only a
     pre-cap build's row reaches it; a peer then holds nothing for an item
     the nest holds nothing for either — its value lives in the entry and
     the next write carries it, as residue (a) accepts). **What stands:** a
     LEGACY store's refused row BELOW its slot at a coordinate the fleet
     serves nothing at (residue shape (i): never re-sent, never
     contradicted) keeps its relay row under the retired writer — bounded,
     harmless after the rotation (the successor's rows carry the values),
     reachable only from a store healed before 2026-09-16 — accepted.
     Pinned by
     `relay_retire_at_deletes_every_item_at_the_coordinate_and_nothing_else`
     (`store.rs`),
     `a_lost_reply_is_refused_as_a_replay_and_the_own_echo_restores_the_relay_row`
     and
     `a_refused_rows_relay_row_is_retired_and_a_peer_meets_only_what_the_nest_serves`
     (`account_runtime.rs`: the replay's relay row is gone at the refusal,
     before any walk, and back once the echo has served it; after the
     burn's heal every relay row under the retired writer is one the fake
     nest serves at the same `(seq, item)`; the outage case,
     `failed_publish_is_recovered_by_the_next_pass`, keeps its row), with
     the real handler's code pinned to the client's classifier constant in
     the nest's `conformance_account_state.rs`.
- **Seed residency (T8 — user-decided 2026-08-10).** The seed **stays
  resident where onboarded** — today's law, unchanged: it rests wherever
  the user imported it (key-hierarchy rule #6's custody posture), and
  device-principal enrollment narrows its spread organically — a machine
  approved from an existing device simply never receives it; no active
  policy, no designation ceremony, nothing new at onboarding. The two
  stricter postures — **designated-primary** (exactly one seed-holding
  device; enrollment routes to it) and **ceremony-input-only** (resident
  nowhere; every root ceremony fetches the recovery kit) — are recorded as
  **post-W5 opt-in narrowings** (a Settings choice, app UI when built),
  not defaults and not W-blocking. Nothing in W1–W5 depends on the choice.
- **Device-signed authoring — a named prerequisite for seedless surfaces.**
  Content envelopes today verify against the ActorId (root key). For a
  surface holding only a device principal to *author*, verifiers (nest,
  federation, peers) must accept device-key envelopes carried with a
  covering capability grant — **resolved 2026-08-10 (T7): the acceptance
  rule (chain verify, capability→kind mapping, carriage, revocation
  authority) is ratified at its owner,
  [`../behavior/devices.md`](../behavior/devices.md) § Device-signed
  authoring** (the D10 server-held sub-key generalized). Until W4 wires
  it, authoring surfaces hold the seed exactly as today; the plane's data
  sync does not wait for this.

## Replica posture (R7)

**A replica's storage posture is the key reach of its principal's
capability bundle** — one axis, two postures, every sync target somewhere
on it. A **reading replica** (own devices, by default) holds content-key
reach and materializes plaintext locally (§ Local at-rest posture). A
**custodian replica** (the nest, by default; any device, by grant) holds no
read keys: it stores and serves the sealed canonical planes and nothing
else. Posture is **derived, never stored and never asked** — no
enrollment-time posture question (works-out-of-the-box), no flag anywhere
(the branch checks what the bundle can open — the
[`nest/storage-modes.md`](nest/storage-modes.md) no-flag rules stay fully
intact, because there is no flag), and no wire property (the plane
transports the same sealed canonical forms to every peer; a serving path
never forks on the target's posture). It is per-account per-replica: one
device may host a reading replica of its owner's account beside custodian
replicas of two friends' accounts. Analysis + cost/benefit: the 2026-08-10
per-replica storage-posture doc (internal plans tree — tracked internally,
not shipped; frozen).

- **What a custodian holds:** per-writer journal rows verbatim; class-2
  entries in their sealed per-writer canonical form (§ The sync plane —
  custody-safe forms); class-1/3 sealed blocks per its custody policy;
  tombstones; frontiers. **What it cannot do:** read, merge (class-2 merge
  happens only at reading replicas — the rails' shipped
  client-merge-over-sealed-CAS model generalized), or author. Relay
  correctness is free under R6: a custodian relays other writers' rows
  verbatim and receivers account the walk exactly as if they had synced
  directly. Outbox **intents do not ride custody** — custodians relay
  applied truth, never pending writes (ruled at T13's resolution: an
  intent relay is permanently a dedicated contract, never a custody-grant
  extension — § The custody grant + ceremony below).
- **Admission — three witnesses, one engine.** The pull/serve core admits
  same-account peers by `DeviceAuthorization`, shared-set peers by M2
  membership ([`../behavior/p2p.md`](../behavior/p2p.md) § Cross-user
  shared-set transfer), and custodians by an owner-minted **custody
  grant** — a *keyless* capability naming scopes (grant primitive:
  [`encryption-at-rest.md`](encryption-at-rest.md) § Capability tiering;
  keyless-scope shape:
  [`key-material-hierarchy.md`](key-material-hierarchy.md)). Custody is
  two-sided consent: the owner mints, the host accepts. Shape + ceremony:
  § The custody grant + ceremony below (T13 — resolved 2026-08-15). The
  seam shape all three witnesses plug into — verdict contract,
  key-binding, carriage, validity — is ruled at § The peer leg → The
  admission seam.
- **The custody floor is the honest cost.** A custodian sees the shape of
  the data — scope ids, writer ids/seqs, CIDs, blinded class-2 item keys,
  sizes, tombstones, change timing — never content, and (T14's item-key
  blind) never *which setting* a class-2 change touched. **The R14 axis
  adds (2026-08-13, with the form-v2 envelope): the cleartext generation
  id on every generation-sealed entry — so a custodian also sees the
  account's rotation cadence and, since removals trigger mints, its
  device-removal events.** Stated rather than hidden: the id is what makes
  reads a lookup instead of a trial walk, and mint timing was already
  floor-visible as change timing on the machinery rows. A strict subset of
  the nest's plaintext floor
  ([`encryption-at-rest.md`](encryption-at-rest.md) § Plaintext floor).
  Choosing custodians is choosing who sees that shape.
- **Same-account relay-only enrollment** is the same axis: an own device
  enrolled with a bundle *without* content-key reach is a custodian
  (a semi-trusted kiosk, an old laptop). Nothing but the bundle differs.
- **A key-less store is a first-class store (W1 constraint).** The store
  API must open and operate with no read keys — journal, sealed entries,
  blocks, frontiers, **no projections**; projections are a
  reading-replica-only plane, never load-bearing for store integrity.
- **The nest-side "plaintext end" of the axis is grants — and the widest
  grant flips the nest's replica to reading posture (R9).** A
  read-granted nest reads at capability positions (server-side search,
  scoring — the shipped grant plane) over sealed bytes; under the
  owner's **materialization grant** it is a reading replica of that one
  account and keeps readable derived views (projections, the owner's
  search index) at rest, exactly as an app-side reading replica does.
  Canonical planes stay sealed on every replica either way — what
  "reading" changes is materialization, never canon
  ([`encryption-at-rest.md`](encryption-at-rest.md) § Readable classes
  class 4 owns the at-rest property, bounds, and revocation-deletes-views
  duty). Custodian *devices* refuse materialization grants in v1 —
  whether a friend's device may hold your readable views is a two-sided
  consent question deferred to the W8 custody surfaces (T16).
- **Hydration policy is the second per-replica axis, and the nest has it
  too (R10).** Posture (key reach) says what a replica can open;
  hydration policy says which payload bytes it holds. A nest replica may
  be payload-dehydrated per scope — holding the always-present index +
  journal while payload bytes live only on other replicas — under R10's
  confirmed-custody predicate; per-set content residency
  ([`../behavior/file-sync.md`](../behavior/file-sync.md) § Content
  residency) and segment eviction
  ([`message-segment-store.md`](message-segment-store.md) § Nest
  dehydration) are the owners.
- **Class-5 never rides custody** (unchanged — the general plane never
  carries it; a custodian additionally holds no MLS anything beyond blobs
  already opaque to it).
- **Availability is honest:** custodian sync is opportunistic
  store-and-forward — a friend's iPad relays when it is on and reachable;
  the always-on durability anchor remains the nest (R8).
- **Kinship, not ownership:** a backup destination (held-for-friends,
  sealed under the owner's `BackupKey`, destination stores opaque) is a
  retention-tuned custodian avant la lettre; convergence of the two planes
  is a direction, and backups stay owned by
  [`../behavior/backup-restore.md`](../behavior/backup-restore.md).

Build home: **W8** (custodian replicas), after W2's same-account leg;
gated on the PQ-2-class hardening of the admission core
([`../behavior/p2p.md`](../behavior/p2p.md) § Wormability posture — a
custodian is a listener serving non-same-account pulls). What W1/W2 must
hold now so W8 stays cheap: the key-less store constraint (above), the
custody-safe sync forms (§ The sync plane), and the three-witness
admission-agnostic core (§ The peer leg). **W8 OPENED 2026-08-15 (user
decision):** both gates were already met (W2.6 landed the admission core;
the PQ-2-class hardening landed 2026-08-12), and the opening resolved the
design residuals T13/T15 below + T16 in its UI owners.

### The custody grant + ceremony (T13 — resolved 2026-08-15, refutable until W8 code)

The third admission witness, shaped by exactly what the admission seam
demands (key-binding + named scopes, inline self-contained carriage, a
validity bound) plus the two transcribed register constraints: coverage
must not silently decay as the account's scope set grows, and the ceremony
is also the *discovery* seam (non-fleet peers cannot read the fleet-only
`device-endpoints` kind).

- **The witness is a signed envelope, not a `GrantBlob`.** `CustodyGrant {
  grant_id, owner: actor_id, custodian_key: device-principal Ed25519 pub
  (= its peer-plane NodeId, R5), scopes: CustodyScopeSet, minted_at,
  expires_at }`, signed by the owner's actor identity key over the
  canonical dag-cbor encoding. Any same-account replica verifies it
  against nothing but its own account identity — self-contained, no
  registry lookup, per the seam's carriage rule. It is deliberately *not*
  a `GrantBlob`: the blob is an HPKE key-conveyance carrying no owner
  signature, while this witness conveys no keys and is all signature. The
  custodian stores the witness (plus the owner's pinned NodeIds and dial
  candidates) in a fleet-only class-2 kind of its **own** account plane —
  "custodies held" — and presents it inline in the admission exchange;
  the witness parser joins the pre-auth PQ-2 fuzz-coverage surface
  (`KIND_PAYLOAD_COVERAGE`) with the exchange's other kinds. **The witness
  carries the owner's removed-device exclusion list (ruled 2026-09-26; built):** an additive
  `removed_devices: [device_key…]` field, `#[serde(default,
  skip_serializing_if = "Vec::is_empty")]` so an empty list is absent on
  the wire and an absent key decodes empty, sorted and deduplicated (the sign door
  refuses any other form), signed with the rest of the witness, filled at
  mint from the minting device's own **verified** fleet view
  (`fleet_removal::removed_device_ids`, reached through the ceremony
  driver's registry-writer seam) and never from a peer's say-so; a device
  that cannot derive the view mints an empty list rather than refusing the
  mint. The list is re-derived at every signing of the witness — the
  deliver re-sign after a failed post included — never recorded: `Removed`
  is absorbing, so a later signing's list is a superset of an earlier
  one's and a custodian holding either copy converges. It is the
  custodian's only exclusion set for the custodied account — its
  `device_removed` view unions the lists of every grant it holds for that
  account (expired, stopped and revoked ones included: a list is the
  owner's signed statement of a removal, which never reverses; later lists
  are supersets) at its admit door, per request and on its own custody
  dial, and an account whose held grants list nothing admits as today
  (absent admits, the seam's additive-across-skew rule). Why the grant and
  not the exchange: the removed device is itself admitted at the custodian,
  so any list an admitted device could author is one it authors first
  ([`account-sync-plane.md`](account-sync-plane.md) § The admission seam →
  *Validity and severance*, the residual). A grant minted after a removal
  therefore excludes that device for its whole life; one minted before it
  admits the device until expiry or re-mint — which makes *revoke, then
  re-grant* the durable incident-response lever for a seedless removed
  replica (rule 6(b) there).
- **The window is BOTH bounds, and only a decoder can say so.** A capability
  grant is time-bounded (`principles.md` § The user always controls their
  data), and `GrantWindow` is `[epoch_start, epoch_end]` — the owner's signed
  `Mint` event carries `window_start` as a first-class field. The nest's
  `capability_grants` table, however, stores only `epoch_end`, so **every
  storage-level "live" filter expresses "not expired" and structurally cannot
  express "already started"**. Therefore: **any site that decodes a
  `GrantBlob` to make an authorization decision checks the window itself, via
  the one shared `fauna_mls::wrapped_blob::grant_window_is_open`** — never by
  trusting the row it came from to have been window-filtered. Ratified
  2026-08-16, where the start bound was enforced at the
  custody handshake and at none of the three consumers that re-derive
  authorization from a decoded row, so a grant post-dated by a month
  authorized pulls on the day it was minted. The general rule this instance
  teaches: **when a storage schema cannot express an invariant, the invariant
  survives only where a decoder happens to be holding the whole object — so
  name the check, share it, and call it from every decode site**, because the
  filter's own name ("live grants") will read as complete to everyone after
  you. Pinned by
  `a_post_dated_custody_row_must_not_admit_before_its_window_opens`
  (`bins/fauna-nest/tests/conformance_custody_nest_door_client.rs`).
- **Scope shape — decay designed out.** `CustodyScopeSet` is either
  **`Account`** — the owner's whole *single-principal* scope set, current
  **and future**, computed by the verifier from the account's live scope
  set exactly as the `DeviceAuthorization` arm computes its account-wide
  verdict (so a scope joined tomorrow is covered with no re-mint) — or an
  **explicit list** of scope strings / family patterns, the subset form
  (policy: § Custody policy below). Default mint = `Account`.
- **Shared-audience carve-out** (the T20-constraint-(b) principle —
  cross-user stakes never inherit a single-account acceptance silently):
  `Account` deliberately **excludes** scopes whose plane other accounts
  co-author (a `conv` channel; any future group scope). Such a scope
  enters a *cross-account* custody grant only as an explicit list entry —
  a deliberate per-scope owner action — and the first **group** scope must
  answer group-side consent at T20's coordination before its plane rides
  cross-account custody at all. Fleet custodians (own relay-only devices,
  the nest) are outside the carve-out: they admit by
  `DeviceAuthorization`/residency, and every channel member already
  accepts each member's own devices and nest holding the shared plane
  (the held-for-friends D7 N-copies acceptance is the same fact).
  **The conv serve door applies this carve-out — the member-mint rule
  (ruled 2026-08-18; BUILT the same day —
  `custody_admission::custody_admits_co_authored_scope`, tier_3-pinned by
  the conformance conv arm's leave-severs case):** a conv explicit-list
  entry admits at the nest
  custody door iff the row's owner is a **current member** of the named
  channel — membership read off the **floor roster**
  ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
  § The floor roster — the door's authority was the retired group plane's
  `group_members` until 2026-09-09, with a per-channel removal path,
  `fauna.conversations.group.remove`, built 2026-08-18: until then the
  roster was append-only per channel and the only severance was the
  account-deletion cascade, so this sentence over-claimed), and
  **never `actor_channels`** (that is the
  channel's *routing* roster, which a single `channel.send` auto-writes
  for its sender, so a row there proves knowledge of the channel id, not
  membership — the original 2026-08-18 spelling of this sentence named it
  and was refuted before the build landed). Membership is re-derived on
  every request beside the live-row re-check, so leaving the channel
  severs serving exactly as revoke does — pinned by the conformance conv
  arm's leave-severs case, which drives the production roster-report door (a
  report that no longer names the leaver), not a test-side row delete. A
  channel class with no `group_members` rows — a DM — failed **closed**:
  honest under-coverage until a non-self-assertable DM roster existed,
  recorded at the serve plane's coverage bound. **That roster was ratified
  (2026-09-08) and BUILT (2026-09-09): the room model's floor roster — one per
  room on its home nest, authoritative for community rooms and member-reported
  for end-to-end rooms — is what this door now reads for every class, the
  `group_members` read above gone
  ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
  § The floor roster); re-homing this consumer was the first code step of that
  doc's § The group plane's fate, taken before any group kind or table was
  retired. The door asks MEMBERSHIP, not rank — a policy-less room's members carry
  no role at all, and a door keyed on the role would fail closed for exactly
  the rooms that exist today. "Every class" became true for a **DM** only on
  2026-09-10, when the client began reporting every end-to-end room's roster
  at its birth — before that a 1:1, which carries no policy and never a
  membership commit, had no floor roster at all and this door failed closed
  for it exactly as before the roster build; a 1:1 born before that change is
  backfilled on the poll (2026-09-11), so the door no longer fails closed for
  it either (that doc's § Implementation status today, the birth-report
  bullet). No shipped app mints a conv-scope custody grant
  today, so what the DM admission closes is a latent hole, not a live outage. What the roster's own write door does and does
  not guarantee — the ratchet that makes it non-self-assertable, and the
  declared bootstrap bound — is that doc's § Implementation status today.**
  **The client's report seam is wired too (2026-09-09):** `NestConversationsRpc`
  / `WsConversationsRpc` implement the reporter beside `ConversationsRpc`, and
  all four glue sites (`fauna-ffi`, `fauna-wasm`, linux, tui) install it, so a
  membership commit on a governed room now actually reaches this door's
  roster rather than leaving it perpetually empty. One declared gap remains:
  no app has a conversation self-leave gesture yet, so a departure reaches
  the roster only through another member's next commit
  ([`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md)
  § Implementation status today). Membership is the whole authority: the D7
  acceptance above is the standing consent basis, and a keyless
  custodian is strictly less reach than what membership already conveys
  to every member's own devices. This sets **no precedent for T20 group
  scopes** — their group-side-consent gate above stands unchanged.
  Serve-plane wiring, addressing, and the home-nest coverage bound:
  [`message-segment-store.md`](message-segment-store.md) § *Which kinds
  the two planes serve*.
- **Coverage enumeration — how a keyless custodian names what it pulls
  (ruled 2026-08-16, refutable until the enumeration build).** A
  custodian may learn **exactly the scope ids its verified grant covers,
  and nothing beyond**: a covered scope's id is custody-floor metadata
  (§ Vocabulary — the custodian necessarily sees it the moment it holds
  the plane), so naming it discloses nothing holding would not; an
  *uncovered* scope's id — above all a `conv` channel id under an
  `Account` grant (the carve-out above) — is precisely the "which
  conversations does this account have" metadata
  [`principles.md`](../principles.md) § *The user always controls their
  data* keeps sealed, and no enumeration surface may answer wider than
  the caller's own verified coverage. Enumeration is therefore an
  **authorization computation, never an observation computation**: the
  covered set derives from the grant (plus the live scope set where the
  form demands it), computed by a party that verified the witness —
  never inferred custodian-side from whatever coordinates, relay rows,
  or storage keys happen to be visible (visible-coordinate inference has
  no authorization boundary: its answer set silently widens with every
  plane that comes into view). Three consequences. **(1) The `Account`
  form needs no disclosure mechanism at all today:** its covered content
  set is the own-actor derivation (§ Feeds and cursors → *Scope
  partition*; `fauna_sync_engine::scope_set`), a pure function of the
  witness's own `owner` field, so the custodian computes it locally with
  zero wire disclosure. Version skew is honest under-pull: an older
  custodian not knowing a newly-registered own-actor kind pulls less —
  the verifier's predicate stays authoritative, and receipts surface the
  shrunken coverage as degraded redundancy (§ Custody policy). **(2)** A
  future single-principal family whose scope ids are *not*
  actor-derivable (a ruled `folder` family, say) re-opens the mechanism,
  not the rule: the serving side answers a grant-derived enumeration at
  serve time — the verifying replica (a fleet device from its live scope
  set; the nest door from its capability row plus the shared
  `AdmittedScopes` predicate) answers exactly the covered set, per
  session, so "current and future" drift needs no re-mint. A stored
  cleartext scope manifest is **rejected**: it outlives every session,
  answers no verified request, and widens the at-rest cleartext surface
  for no availability gain. **(3)** The explicit-list form is already
  self-naming — the owner disclosed exactly those ids at mint as a
  deliberate per-scope ceremony action. Nest-door corollary (the W8-wide
  residual's nest-door item inherits this): under an `Account` row the
  door's content coverage is the same pure function of the row's owner;
  a `conv` scope requested under an `Account` grant refuses loudly (the
  record-cid arm's loud-refusal rule), never serves and never
  empty-answers.
- **Same-account custodians need no grant.** An own device enrolled
  relay-only admits by `DeviceAuthorization` — posture is bundle reach,
  the witness kind does not change. The custody grant exists for
  **non-fleet** custodians only (a friend's device or nest, an archive
  box).
- **Lifecycle rides the shipped capability plane — one trust primitive.**
  `grant_id` lives in the capability-grant id space; mint/renew/revoke are
  signed `GrantEvent`s in the client-authoritative log — inheriting the
  Now/History lenses, the honest-bound copy regime, the ~90-day default +
  blessed auto-renew, the succession re-mint sweep, and record-then-deposit
  ordering (log-first on the widening verbs, nest-first on revoke) — and
  each verb also drives a **keyless nest-side capability row** via
  `fauna.capabilities.{mint,renew,revoke}` (the `content.label-write` /
  spam-model keyless-row precedent: an authorization + audit record, never
  a key conveyance).
- **Two revocation stores, one rule.** A fleet replica evaluates admission
  against the synced grant-event log; the nest — which cannot read the
  sealed log — evaluates against its own capability row. Both sever at the
  next admission evaluation (the seam's ratified bound). The log is the
  authority and the nest row its nest-readable shadow; the ids-only
  reconcile sweep already re-narrows a disagreeing nest. Honest bound,
  stated not hidden: revocation stops future carriage and future serving
  on honest boxes; ciphertext already held stays held — and stays sealed
  forever, the keyless tier never having had read reach to lose. A fleet
  replica that has not yet synced the revoke may serve for one
  config-sync convergence window.
- **What a custodian serves.** A custodian serves pulls of the custodied
  account to that account's **fleet** (`DeviceAuthorization` verified
  against the owner actor id the witness itself names). Serving M2
  members of a shared scope is *not* custody's contract — a member pulls
  the shared plane from its own account's replicas.
- **The ceremony — offer / accept / mint, and it is the discovery seam.**
  1. **Offer (owner → host):** a signed proposal — proposed scopes,
     duration, the owner's current dial candidates + advertised relay
     URL — delivered over an existing authenticated channel to the host
     *account* (the contact plane or an established conversation channel;
     never the account plane, which the host cannot read).
  2. **Accept (host):** the host's consent surface (T16) states the
     custody floor and the ask; accepting **binds the serving device** —
     the accept returns that device's principal pubkey (= NodeId), its
     dial candidates, the host-side byte budget (§ Custody policy), and
     possibly a narrowed scope set. Two-sided consent is per-scope: the
     effective set is the intersection.
  3. **Mint (owner):** the owner's app mints the witness naming the
     accepted key + set, records the `Mint` event (log-first), deposits
     the nest row, and delivers the witness back over the same channel.
     The owner's fleet records the custodian's NodeId + dial candidates as
     a fleet-only class-2 entry, so every fleet replica learns whom to
     serve and how to dial it.
  4. **Endpoint refresh:** the fleet-only `device-endpoints` kind never
     reaches a non-fleet peer; each custody session re-exchanges current
     candidates over the authenticated channel (the `node_info` pattern),
     and the relay serves rendezvous as usual — for a custodian whose
     account lives on the owner's nest; one homed on another nest has no
     relay toward the owner's devices
     ([`../behavior/p2p.md`](../behavior/p2p.md) § The relay → *Across
     nests*). A custodian unreached past
     witness expiry decays to re-offer.
  5. **A grant id names a ceremony, not a record — the same id may sit on
     BOTH sides of one account's state.** The id is chosen by whoever
     *offers*, so a peer holding an id we minted as owner can re-use it in
     a counter-offer, and an account that both grants and holds custody
     then carries that id in its owner-side and host-side sets at once.
     This is legitimate and must stay representable (mutual custody is the
     expected buddy topology). **Consequence for every consumer: resolve a
     ceremony record by `(side, grant_id)`, never by id alone.** An id-only
     lookup that picks one side and stops leaves the other side's progress
     marks landing on the wrong record, where they are silently dropped and
     the owed act re-fires on every drive pass forever — the shape found
     2026-08-17.
- **A custodian is a DEVICE or a NEST — the accept says which (the
  nest-custodian identity fact; ruled 2026-08-17, refutable until the
  custodian-nest runtime build).** Ceremony step 2 binds a serving
  principal, and the nest-shaped half of the T16 facet
  ([`../ui/nests.md`](../ui/nests.md) § Trust facet — custody rows: a
  friend's nest, an archive box) needs the accept to be able to name a
  NEST as that principal. The fact is structural, not a flag: an accept
  carrying `custodian_nest_url` (additive, default-absent) binds the
  host's nest — `custodian_key` then names the host's nest **actor
  identity as the host's app has it pinned** (TOFU state, never the
  nest's own claim about itself — the R14 escrow-holder trust rule
  generalized; `EscrowReceiptRecord.holder_id`'s holder-generic contract
  is the precedent), and the URL is the dial anchor. An accept without
  it binds the accepting device, exactly as before. Consequences:
  1. **Consent stays in the apps; the trust chain is the host's
     signature.** Only the host's signed accept nominates the host's
     nest — a nest can never self-enroll as custodian. The owner already
     trusts the host account (the ceremony rides an MLS-authenticated
     channel), and the signed accept extends exactly that trust to the
     host's naming of its own infrastructure, so the owner holds a
     verified key↔URL binding with no TOFU of its own. Host-side
     fail-safe: no pinned identity → the nest choice is absent — never a
     self-reported key (the escrow-holder rule's no-pin arm).
  2. **The witness does not change.** `CustodyGrant.custodian_key` is 32
     key bytes and holder-generic; admission is possession of that key
     on every arm (peer QUIC, the nest door), never a key-kind dispatch.
  3. **No offer-side advertisement (removed 2026-10-04).** The offer once
     carried a flag saying the owner's machine understood the nest form;
     every owner build sets it, so the flag and its two refusal arms left
     with the compat-remnant sweep
     ([`compat-remnant-sweep.md`](compat-remnant-sweep.md), the 2026-10-04
     paragraphs). A host app surfaces the nest choice for any offer that
     names the owner's nest (item 6's reachability floor) while it holds a
     pinned nest identity.
  4. **Endpoints invariant kept.** A nest-form accept still carries
     `custodian_endpoints.node_id == custodian_key`, with zero dial
     candidates and no relay URL — the nest URL is the only anchor. The
     fleet's `custodian-endpoints` registry row carries the URL
     (additive) so every owner replica knows the restore dial anchor and
     the render split without re-opening the accept.
  5. **Render split (owner side).** URL present → the custody renders in
     the Nests-page family, keyed by the nest identity; absent → the
     Devices-page family ([`../ui/nests.md`](../ui/nests.md) /
     [`../ui/devices.md`](../ui/devices.md) own the two shapes). One
     custody never renders in both.
  6. **The custodian-nest RUNTIME is a staged build, and the fact lands
     first** — wire-evolution order: owners must understand the field
     before any host sends it. Staging: **(a)** the fact + owner-side
     fold + the Nests-page render; **(b)** hosting registration — the
     host's app deposits a keyless custody-hosting row on its own nest
     (witness verbatim + owner-fleet snapshot + `owner_nest_url` +
     budget; the keyless capability-row precedent), the nest's pump runs
     the pull leg against it, and stop/budget rewrites ride the same
     row; **(c)** receipts — the nest mints A7 receipts under its pinned
     identity and DEPOSITS them at the OWNER's nest custody door (a
     custodian-class receipt arm; the owner's nest stages
     latest-per-grant, the owner's fleet folds and re-verifies at sync
     exactly as today). The staging order held: the host app's accept
     surface offered the nest choice only once (b)+(c) existed — a
     choice that stores nothing would have been configuration theatre —
     and all three stages are BUILT (§ Implementation status today owns
     the ledger). REJECTED: receipt relay via
     the host's app over the ceremony channel (redundancy would read
     degraded whenever the host's app sleeps — a false-alarm channel,
     violating the A7 honesty receipts exist for), and owner-side
     receipt pulls from the custodian nest (a new always-on dial
     obligation on every owner fleet for no honesty gain over the
     deposit arm).
  7. **Scope.** The fact covers CROSS-ACCOUNT nest custodians (a
     friend's nest, an archive box's account). A literal same-account
     second nest is residency/enrollment, not custody — the "non-fleet
     custodians only" rule above is untouched. **What it is instead was
     ruled and built 2026-09-30:** a nest the user has linked is a
     secondary replica that the account's own seed-holding devices
     complete over a second connection, never a custodian and never fed
     nest-to-nest — [`account-sync-plane.md`](account-sync-plane.md)
     § The bind leg, ruling 4, owns it. The custodian pull leg was weighed
     as its carrier and refused there.
- **Intent carriage: NO — permanently a separate contract.** Custodians
  relay applied truth only. The W4 phase-0 ruling made intents
  structurally non-scope-keyed and never-synced, so a friend-relay intent
  path ("phone writes offline, the iPad carries the intent to the nest")
  is a **dedicated intent-relay contract with its own witness and
  replay/expiry/idempotency design** — a fresh register row when someone
  wants it — never an extension of the custody grant shape. This closes
  the "revisit in T13" note for good: the grant shape needs no v2 for it.
- **Succession:** custody grants join the succession re-mint sweep
  (`remint_grants`) — the successor identity re-offers/re-signs; witnesses
  signed by the retired key die with that key's trust.
- **What a custodian will dial (ruled 2026-08-17; GRADED and
  the ruling CONCURRED by the security review —
  amended there, below).** A
  counterparty-supplied nest URL (`CustodyOffer.owner_nest_url` today;
  `CustodyAccept.custodian_nest_url` inherits the policy at its first dial
  site) makes one user's device open connections on another user's
  say-so, so it is bounded by policy, enforced by the shared predicate
  `fauna_core::counterparty_url::validate_counterparty_nest_url` at
  **three doors**: the ceremony refuses at `begin_offer` (a well-meaning
  owner learns early) and at `build_accept` (the adversarial path — a
  crafted, validly-signed offer), and the custody dial loop re-checks
  every pass (`CustodyNestPass.refused_url` — a row already at rest,
  or rewritten later per the LWW deliver-ingest, never reaches
  `connect()`). The dial loop is the load-bearing one: it is the only door
  a row already at rest must pass. The policy is **identity before bytes,
  never an address-class block** — a home-LAN nest on a private address is a
  first-class deployment: origin-only shape (scheme + host[:port];
  no userinfo/path/query/fragment — the dial appends the fixed route, so
  a counterparty can never steer request bytes at an arbitrary endpoint);
  `https`/`wss` anywhere (the transport-trust stack — WebPKI or the
  graduated SPKI pin, `security.md` § Transport trust — means nothing
  past the TLS hello reaches an endpoint that cannot prove the named
  identity); plaintext `http`/`ws` **loopback-only** (the dev/e2e norm).
  **Accepted residuals, stated:** fixed-shape, payload-free TCP/TLS
  connect attempts at a counterparty-chosen host:port from the
  custodian's network position, cadence-bounded by the pump pass (the
  user-device sibling of the two accepted nest-side SSRFs); and the same
  fixed-shape knocks at the custodian's own loopback ports via the
  plaintext carve-out. Failure detail stays local — the owner-visible
  surface is the existing coarse fresh/stale/no-receipt state, so dial
  outcomes leak no per-address oracle back to the counterparty.

  **Amendment — how the origin-only clause is held.** By an **allowlist of the legal `host[:port]` charset**, never a
  denylist of separators, because a separator a denylist does not enumerate
  is one the parsers still honour: for the four special schemes this policy
  admits, every WHATWG parser — including the `url` crate the dial reaches
  through reqwest — reads `\` as a path separator, so a denylist cut could
  both falsify the origin-only promise above and diverge
  `fauna_anon_client::trust::authority_of` (the SPKI/TOFU **pin-store key**,
  whose contract is that it names the host a URL parser would actually
  connect to) from the dialed host. Both are closed: the charset allowlist
  holds the shape, and the authority terminates on `/` or `\` alike.
  **The structural point, which outlives both bytes:** the
  shape check lives in a WASM-safe crate with no URL parser while the dial
  composes `format!("{nest_url}/api/v1/…")` for a WHATWG one, so two parsers
  read one string by construction — their **agreement is pinned, not
  assumed**, in `libs/fauna-client/tests/counterparty_url_parser_agreement.rs`
  (the lowest crate holding both the policy and a real parser). Any future
  counterparty-URL consumer joins that pin rather than re-deriving a
  separator list.

  **Amendment — the acceptance is RE-TAKEN for a nest, not inherited
  (ruled 2026-08-17).** Everything above is one
  ruling for both dial loops — origin-only shape, the allowlisted
  `host[:port]` charset, TLS anywhere — and must not fork; the scope is a
  **parameter** on the shared predicate
  (`fauna_core::counterparty_url::DialScope`), never a second copy. Exactly
  one clause changes. The **plaintext-loopback carve-out is withdrawn on a
  public nest deployment**, because both premises of the residual accepted
  above change when the dialler is a nest: the loopback whose ports get
  knocked on is *the nest's*, where nest-private surfaces live, and the
  counterparty is no longer someone the victim chose — a custody-hosting
  depositor mints **both ends** of the ceremony themselves (the witness
  verifies under the owner it names, and an attacker generates that owner
  keypair), so the door needs only an account on the nest. The deployment
  fact is **publicness**, the nest's existing uniform test
  (`resolve_handle_domain(d).is_public_dns_name` — the same predicate the
  plain-HTTP boot guard and the enable-email default use), ⚠ deliberately
  **not** "is TLS enabled": TLS terminates in the SNI router on a fronted
  deployment, so `tls_enabled` is false on plenty of real public nests and
  would leave the carve-out standing exactly where it must not. A local /
  e2e nest keeps the carve-out unchanged, which is why the tier_3 posture
  (nests on `127.0.0.1`, plaintext) needs no runtime knob and no
  build-profile split. **Device dials are untouched** — the dial policy stands
  as ratified for them.

  **New residual, stated with its trigger.** A **domainless** box — no identity
  domain configured — reads as not-public and so keeps the carve-out, including
  on a bare public IP. This is deliberate consistency with the uniform
  publicness test (the plain-HTTP boot guard shares it: it too fires only on a
  *configured* public domain), and it is what keeps bare-IP dev/e2e deployments
  working. It is written down rather than left implicit **because the
  re-taken acceptance above exists precisely as the cost of an unstated
  inherited residual**: if a bare-IP public
  deployment ever becomes a supported shape, the publicness predicate is the
  named place to revisit, and this clause is the trigger.

- **Two-sided bounds.** The grant names scope reach (the owner's bound on
  what the custodian may pull); the **acceptance names the budget** — a
  `retained_bytes_cap` the host chooses at accept (default a hard-coded
  Rust constant; adjustable per custody in the host's UI). Bound retained
  bytes, never just quota counters (the nest member-cap lesson).

  **The host's number is bounded on BOTH axes (ruled
  2026-08-17), and the bounds are hard-coded Rust constants** — bucket 1
  under [`principles.md`](../principles.md) § One configuration surface: no
  human would choose either, so neither is config and neither is app UI.
  **(1) A per-row ceiling equal to the accept-time default** — the default
  IS the maximum; a host narrows the budget, never widens it. Deliberately
  not a larger distinct number, because "how much may one host be allowed
  to hold" *is* a human choice and therefore belongs to the tier/quota
  surface below, not to a constant a later session would bump. **(2) A
  per-host row cap**, which bounds more than disk: a nest's hosting pump
  dials every registered row once per pass, so the row count is also the
  per-host outbound dial fan-out and the number of on-disk custodied
  stores. **Rows refuse and bytes clamp**, deliberately: a row-cap hit is a
  caller error worth surfacing, while a too-large budget is honoured at the
  ceiling so a legitimate host is never locked out by a number their own app
  suggested. A **rewrite** of a row the host already holds is admitted at
  the row cap — otherwise a host sitting at the cap could never *stop* one
  of its own rows, and the cap itself would become unrecoverable state. Both
  bounds are enforced at the register door **and** independently in the
  pump: the pump is the backstop for a row already at rest, planted under a
  build predating the door. ⚠ The pump's half of the **row** cap arrived
  hours after this claim did: the byte ceiling
  had its per-pass backstop from the start while the row cap was a
  door-only check, and this sentence (plus the constant's own doc and the
  fixing commit's message) recorded the backstop as present the whole
  time — the doc-drift lesson worth keeping past the fix. Both
  sides now read one cap through
  `fauna_core::custody_ceremony::hosting_pump_admits_row`, tallied per host
  rather than by run length so the bound cannot come to depend on the
  registry query's `ORDER BY`; excess rows are counted into the pass
  report's `refused_fan_out`, because rows the door would refuse today are a
  signal and not a silent skip.

- **Held custody bytes are accounted in their OWN derived counter, bounded by
  the depositing host's tier (ruled 2026-08-17).** The figure is
  `SUM(custody_hosting.held_bytes)` for that host — **derived**, not
  accumulated, since the pump already meters each row every pass — and the
  bound it is checked against is the host's own tier `max_storage_bytes`
  ([`../behavior/admin.md`](../behavior/admin.md) § 2 Users owns the ledger and
  the rule that *the tier IS the quota*, so "how much may a host hold" stays an
  admin choice with a surface that already exists, never a new constant).
  Enforced at the register door (a new row is refused once the host's sum
  already meets its bound — the learn-early half, the shape the URL and cap
  gates already have) and in the pump, which squeezes a row's effective cap to
  the host's remaining headroom. The pump's enumeration is ordered, so the
  policy is stated rather than incidental: **first-registered keeps its space**,
  later rows absorb the squeeze. Over-bound is not a data-loss event — the
  existing payload-only eviction is the response and `AtFloor` its honest
  terminal state.

  ⚠ **This RETRACTS an earlier same-day ruling that these bytes join
  `users.storage_bytes_used`.** That was wrong on evidence this doc set already
  carried. (1) That column is **one shared counter the sync plane enforces**
  (`sync_storage` refuses a write when `used + delta > max_storage_bytes`), so
  charging custody there makes a friend's custody silently refuse *the host's
  own file writes* — a cross-plane wedge, and precisely the failure
  [`../behavior/file-versions.md`](../behavior/file-versions.md) § Retention
  names when it rules that **reclamation must precede metering, or the quota
  wedges with no app-side remedy**. (2) An accumulated charge needs a
  credit-back on removal; a **derived** sum needs none — dropping the row drops
  the figure — which also dissolves the claimed coupling between this bullet and
  the admin-removal bullet below.

  **Why not the held-for-friends shape**, which is the nearest ratified
  precedent (`admin.md` § 2 Users: a backup guest is admitted as a handle-less
  `users` row on a `backup` tier and charged there). That plane's guest is a
  **client of this nest pushing its own backup**, so it has a `users` row by
  nature. A custody *owner* never authenticates here — the nest authenticates
  **to them** under a grant, which is the whole point of the no-host-device
  pull. Minting a `users` row for a foreign identity purely as an accounting
  handle would add an admission step to a ceremony deliberately designed to
  need none, and put an admin in the middle of the host's out-of-the-box path.

- **The hosting registry is admin-visible and admin-removable.** The
  register door is `User`-class and its rows drive standing outbound dials
  and disk holds, so *No client-causable unrecoverable nest state*
  ([`../principles.md`](../principles.md)) requires an admin **list +
  remove** for them, independently of the caps above: without it the only
  recovery from a registry filled this way is DB surgery plus `rm -rf`
  under the custody-hosting root, which the invariant forbids. The
  per-caller `hosting.list` read-back is host-scoped by design and is not
  this surface. Any knob it grows is app UI in all 7 apps. The page tells
  *not yet answered* from *answered, nothing held*: the empty line
  (`admin-custody-hosting-empty`) and the count appear only once the
  registry read has returned, never while it is in flight — an admin who
  reads "nothing held" must be reading the nest's answer, not a page that
  has not loaded (built on the apps and pinned by their unit tests;
  recorded here 2026-10-01, no journey asserts it yet).
- **Metering.** The custodian meters per custody — bytes and item counts,
  by scope family — rendered host-side ("what I hold for others") and
  reported owner-side through custody receipts. **Both planes it retains
  are metered** (2026-09-29): the relay rows, and the owner's segment
  pairs it adopted (the bootstrap contract's bulk half) as one `segment`
  family per scope (`fauna_core::custody_policy::SEGMENT_ITEM_CLASS`) —
  held `.dat` bytes plus every `.meta` sidecar, of which the `.dat` alone
  is evictable (the sidecar is the index floor). A meter blind to either
  plane lets it overrun the cap with every control reading it as empty.
- **Eviction.** Over budget, a custodian evicts **payload bytes only** —
  journal, frontiers, tombstones, and the index floor are always-present
  (the R10 hydration axis applied custodian-side) — and eviction is
  always **receipt-visible**: coverage shrinkage surfaces owner-side as
  degraded redundancy (A7's honest failure mode), never silently.
  Eviction, tombstone application, and scope-departure GC are keyless by
  construction (the constraint the frozen posture analysis holds W1/W2
  to). Ordering heuristics are W8 build detail. On the segment plane the
  unit is the **whole `.dat`, oldest segment first** (a CARv2 file is
  immutable — `message-segment-store.md` § Nest dehydration's law); the
  segment's row, sidecar and index rows stay, so its records remain
  complete in metadata and a re-offer is "already held", never a
  re-download.
- **Admission (the ingest side of the segment plane).** Adoption is
  **budgeted**: a pull adopts segment pairs only within the headroom the
  cap leaves over what is already held, spending it pair by pair, so a
  custodian stops adopting new segments at its cap rather than overrunning
  it and waiting for eviction. The same headroom bounds each download — a
  segment has no legal size ceiling (`message-segment-store.md`
  § Segment size), so an offer declared larger than the headroom is
  skipped before any byte moves, and a body longer than the headroom (or
  than its own declared size) is refused mid-read, never buffered whole.
- **Reclaim (the hosting registry's lifecycle end, row 67).** *Stop is a
  pause* — a stopped row keeps its custodied bytes and its registry slot.
  *Remove is the reclaim*: the host's own `fauna.custody.hosting.remove`
  (User-class, host-derived from the connection like its siblings) — or
  the admin twin — drops the row and, only with the `(host, owner)`
  pair's **last** row, the custodied store beneath it (the pair's grants
  share one store dir; everything under it is derived and re-pullable).
  *Expiry is bounded*: a row whose witness has been expired for more than
  one grace window (the T-window vocabulary — a hard-coded 30-day Rust
  constant, `HOSTING_EXPIRED_STORE_GC_GRACE_SECS`) has its pair's store
  reclaimed by the pump once **every** row of the pair is past the grace;
  the rows themselves stay as the host's visible record, and expired rows
  are never dialed. Keyless like every custodian-side GC.
- **Scope subsets.** The explicit `CustodyScopeSet` form; hosts may narrow
  at accept; renew widens nothing — widening scope or budget is a fresh
  offer→accept round.
- **A deliver never re-opens consent.** The host takes a delivered witness —
  the first or a fresher one (a re-sign, a dial-candidate refresh) — only
  inside what it accepted: scopes within the effective set, a term no longer
  than the offer's duration, and a superseding witness never outliving the
  one already held. A deliver on a ceremony the host declined or reclaimed is
  refused. The host's own knobs (Stop, budget) live on its ceremony record,
  so a re-deliver that rebuilds the runtime row keeps them.
- **Receipts.** The custodian's periodic check-ins **are** the A7 custody
  receipts — signed, dated attestations covering scope/CID ranges plus a
  held-bytes summary; the parameters (N-of-M, aging margins,
  re-verification cadence) remain T18's first-build detail
  ([`message-segment-store.md`](message-segment-store.md) § Nest
  dehydration owns the receipt mechanics). Receipts feed the dehydration
  predicate and both UIs.
- **Abuse posture.** A custodian refusing, narrowing, or evicting is
  legal — availability is honest, opportunistic store-and-forward. Lying
  in receipts is the offense the re-verification cadence exists to catch;
  a dead or lying custodian is degraded redundancy the owner *sees*.

## Local at-rest posture (R3)

This section states the **reading replica's** posture — the default for
the user's own devices (R7 owns the axis; custodian replicas materialize
no plaintext).

**The app-side replica rests plaintext in the user's OS context — sealed
where the OS keychain makes sealing free** (ratified 2026-08-10; amended
2026-08-11, greenfield finding A8). The threat model stands: the replica
lives inside one OS user's boundary — OS disk encryption and user-context
isolation are the assumed defenses, matching the most sensitive client
store already shipped (`mls.db`, plaintext SQLite) and web's documented
`localStorage` posture — and sealing defends nothing against a same-user
process. What the plaintext posture *is* exposed to is the
**non-adversarial leak class**: home-directory cloud sync, backup tools,
disk images, support bundles, `grep` accidents. On platforms whose
keychain holds a store key with transparent unlock at login (macOS, iOS,
Android, Windows), sealing the replica under a keychain-held key is
nearly UX-free and converts that whole leak class into ciphertext — so
there the replica **seals**; where the OS offers no free unlock
(Linux-without-LUKS is the genuinely exposed case either way), it rests
plaintext. **Posture is per-platform and never a user question** — no
knob, no prompt; the platform's capability decides.

What stays sealed regardless: the credential slots (platform secure store), and every
nest-side copy (the sealed posture, [`encryption-at-rest.md`](encryption-at-rest.md),
is untouched by this plane — statement 6's nest half was already law). The
sealed `UserConfig` local replica stood first on this list until the
`__config` rail retired 2026-10-02; its values rest on their account-plane
kinds ([`config-dissolution.md`](config-dissolution.md) § The `__config`
dissolution schedule → *The closure order*, step (6)).

**The key-material kinds follow R3 — and R3's trigger is the credential
store, not a platform list (ruled 2026-10-02, refutable at the first build;
the sealed `UserConfig` replica's retirement is what forced the question).**
That replica was sealed because it carried key material, and its values now
rest as rows of their own fleet-only kinds: the MSEK (`fauna.state.mail`),
the deployment seeds, the subscription period keys, the folder keys, the DNS
credentials. The replica stores each row's *opened* value — the plane
unseals once at ingest (`AccountStatePlane`'s `trial_open` → the store's
`ingest_state`) and keeps the sealed envelope beside it only to relay
(`relay_rows`) — so on a plaintext replica those keys rest as plaintext,
inside the same OS-user boundary as `mls.db`'s leaf keys and, on a headless
box, the `0600`-file `BackupKey` that derives generation 0: a boundary
sealing cannot defend. What the plaintext posture exposes is R3's
non-adversarial leak class, and for these rows that class prices higher
than for any other — a leaked home directory plus the ciphertext the nest's
own admin already holds reads every folder and every mailbox. So the rule
stands and its platform list goes: **the replica seals under a store key
held in the credential store the app already uses, wherever that store
hands the key over without a prompt** — macOS, iOS, Android, Windows *and*
the Linux desktop (Secret Service, unlocked at login; the
"Linux-without-LUKS" line above conflated the keyring with disk encryption)
— **and rests plaintext only where the store key would rest beside the
data** (the `0600`-file backend of a headless box, where a seal is theatre).
Posture stays per-platform and never a user question. **Implementation
status: NOT built on any platform** — `libs/fauna-account-store`'s SQLite
is plain `rusqlite` with no store key on any `cfg(target_os)`, so today
every replica, Linux included, holds these rows unsealed; the build is one
shared-Rust change in the store crate.
