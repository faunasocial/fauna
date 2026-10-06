# The account data plane — target state

Owns: account-data-plane
Status: ratified — **partitioned by concept 2026-09-06 into five plane docs ([`account-data-taxonomy.md`](account-data-taxonomy.md), [`account-sync-plane.md`](account-sync-plane.md), [`account-offline-mutation.md`](account-offline-mutation.md), [`account-runtime.md`](account-runtime.md), [`account-replica-posture.md`](account-replica-posture.md)); what stays here is the charter, the ratified decisions, the account store, the nest-side requirements and the cross-cutting status ledger. Two more sections left on 2026-09-28, each with its status entries: § The client-side lifecycle (W3) → [`account-client-lifecycle.md`](account-client-lifecycle.md), and § The `__config` dissolution schedule → [`config-dissolution.md`](config-dissolution.md).** W0 charter (2026-08-10); W1+W2 contract detail resolved
same day (T1–T5, the second design pass — refutable until W1/W2 code lands);
per-replica storage posture + the nest role decomposition ratified same day
(R7+R8, the third design pass — refutable until W8 opens); T14 (the class-2
entry form + key schedule) frozen 2026-08-10 at the W2 scoping gate;
R9–R12 (the nest-as-app symmetry pass — materialization, nest hydration
policy, the no-nest profile, the multi-actor backend direction) resolved
2026-08-10 from the user's four symmetry objections + the file-sync
content-residency directive, refutable until their first builds;
the W2.6 design gates — the admission seam (witness shape) and the peer
leg's wormability walk — ratified 2026-08-11 ahead of W2.6's build
([`account-sync-plane.md`](account-sync-plane.md) § The peer leg); **W2.6 code landed 2026-08-12**, closing the seam's
until-code window (the walk's per-rule verdicts stay refutable by
the security review, which grades the landed obligations);
the content-scope string encoding ratified 2026-08-11 ([`account-sync-plane.md`](account-sync-plane.md) § Feeds and
cursors → *The scope string*) and FROZEN the same day, the feed walk
having landed and begun writing production rows under it;
R13 (the audience ladder: class 5 narrowed to roots + MLS state +
device-local keys, operational secrets ride the plane at the
never-delegable fleet-only rung, universal in-seal writer signatures,
groups are sibling scopes) ruled 2026-08-11 — the user-guided greenfield
pass that closed W2.5's gate — refutable until W2.5 code;
R14–R17 (content generations + escrow; the storage-group keying seam;
the box scope direction; the total-classification law) + the A3–A8
amendments (arbiter epochs, compactable seen-set, per-rung sub-scopes +
the kind cache, third-party kind manifests, custody receipts,
seal-where-free) ruled 2026-08-11 from the greenfield derivation's
deltas + adversarial pass
(`2026-08-11-greenfield-storage-derivation.md` §§ 8+11, internal plans
tree — tracked internally, not shipped; each
put to the user individually) — refutable until each's first build; the
wormability-walk rule-6 verdict CORRECTED the same day per the
the security review refutation.
The ratified decisions (§ The ratified decisions) and the contracts below are
target state. Most of the plane is now built — W1 (the account store), W2
(the sync plane + peer leg), W3's shared client-side runtime, W4 (the
offline-mutation contract), W5 (multi-instance concurrency) and W8
(custodian replicas) all have landed, e2e-proven code; per-platform rollout
(W6) is still trickling down app by app, and R9–R12 (the nest-as-app
symmetry pass) remain wholly unbuilt. § Implementation status today is the
append-only ledger of what is actually code versus still design — read it,
not this paragraph, for current state — and open detail inside a contract is
marked `TBD(owner)` inline and indexed in § Open questions; build nothing
against a `TBD` without resolving it first. The law this plane supersedes
(the account instance lock) has already retired on the lead apps; current
per-app status is owned by
[`apps/account-scoping.md`](apps/account-scoping.md) § Concurrent instances,
not restated here.
Authority: owns **the charter of the account-data plane** — the ratified decisions, the vocabulary, the account store contract (logical schema direction, physical placement, multi-process safety, hydration), the nest role decomposition, the box scope, the nest-side requirements, migration + compatibility, the workstream map and the open questions — together with the cross-cutting half of § Implementation status today, the entries no single plane owns. **Split out 2026-09-28, each with its status entries: the client-side lifecycle rules (W3) → [`account-client-lifecycle.md`](account-client-lifecycle.md); the `__config` dissolution schedule, the migration W7's rule governs → [`config-dissolution.md`](config-dissolution.md).** **NOT owned here either — the five plane concepts this doc's own Authority line used to enumerate were split out 2026-09-06, each taking its rule sections AND its status-ledger entries: what account data IS → [`account-data-taxonomy.md`](account-data-taxonomy.md); how it moves → [`account-sync-plane.md`](account-sync-plane.md); what an app may do offline → [`account-offline-mutation.md`](account-offline-mutation.md); which process holds the plane → [`account-runtime.md`](account-runtime.md); what a replica may hold → [`account-replica-posture.md`](account-replica-posture.md). Every original heading is unchanged there, so a `§ <section>` citation resolves by swapping the filename, and a routing stub remains at each original location.** The concepts that were already deferred elsewhere before the split: the replica boundary
(observation-defined; the fleet seen-set), the account-data taxonomy, the
account store contract (logical schema direction, physical placement,
multi-process safety, hydration), the general sync plane contract (ordering
model, feeds/cursors, merge-policy seam, nudges, and the device↔device peer
leg's data-plane rules), the offline-mutation contract (outbox, idempotency,
the offline classification criteria), the multi-instance concurrency target,
the store device principal (enrollment, credential slot, seed residency),
the app-side local at-rest posture, and the per-replica storage posture
(reading/custodian replicas, the custody floor, custody-grant plane
semantics). Defers: the P2P transport seam and
contact-plane P2P → [`../behavior/p2p.md`](../behavior/p2p.md); the device
model + `DeviceAuthorization` mechanics → [`../behavior/devices.md`](../behavior/devices.md);
the sync agent's lifecycle/credential model → [`apps/sync-agent.md`](apps/sync-agent.md);
the client scoping taxonomy + placement + multi-account stages →
[`apps/account-scoping.md`](apps/account-scoping.md) and
[`long-term-store.md`](long-term-store.md); the file-sync protocol (chunk
pipeline, config authority, nudge mechanics) →
[`../behavior/file-sync.md`](../behavior/file-sync.md); placeholders/hydration
mechanics → [`../behavior/on-demand-files.md`](../behavior/on-demand-files.md);
conflict auto-resolve/version retention → [`../behavior/conflicts.md`](../behavior/conflicts.md);
the reserved rails → [`../behavior/reserved-folders.md`](../behavior/reserved-folders.md);
the nest's segment stores → [`message-segment-store.md`](message-segment-store.md);
the nest at-rest sealed posture → [`encryption-at-rest.md`](encryption-at-rest.md);
canonical bytes/CIDs → [`serialization.md`](serialization.md) and
[`data-flow.md`](data-flow.md); wire evolution rules →
[`version-compatibility.md`](version-compatibility.md).
Provenance: user directive 2026-08-09 (five statements + requirement 7,
transcribed in the survey § 1), the in-session decision ratifications
2026-08-09/10 (§ The ratified decisions), and the peer-symmetry refinement
(user, 2026-08-10: app↔app as natural as app↔nest — the nest just happens to
be always-on). Current-state evidence base: the 2026-08-09 local-first
account-data survey (`2026-08-09-local-first-account-data-survey.md`,
internal plans tree — tracked internally, not shipped).

In one sentence: **every app instance mounts a per-account local replica —
complete in metadata, selectively hydrated in payloads, plaintext in the
user's OS context — and one peer-symmetric sync engine keeps all replicas
convergent, replica↔replica over iroh exactly as naturally as replica↔nest;
the nest is simply the always-on, durable peer holding the sealed store, and
the UI reads and writes the replica, never the wire.**

Apps become views over data. The app↔nest relationship becomes a lazy data
sync, not imperative synchronous commands. This is a multi-year rewrite
(workstream map in § Workstreams); this doc is the contract every workstream
builds against.

## The ratified decisions

Each decision below was a genuine tradeoff put to the user; the rationale
recorded here is the load-bearing part — a future session proposing to revisit
one must answer the rationale, not just the conclusion.

- **R1 — The replica boundary is observation-defined** (user, 2026-08-09).
  The replica holds what the account's apps have *seen, downloaded, or
  created* — never the potentially-fetchable universe (the un-browsed feed
  firehose stays outside). Plus delivery: content addressed *to* the account
  (mail, inbox items) is in-set at delivery, not first fetch — the nest
  already persists it per-actor. § The replica boundary.
- **R2 — Store ownership is hybrid: library-first, agent as optional host**
  (user, 2026-08-10). Every app links the store library over a
  multi-process-safe store; the per-user sync agent
  ([`apps/sync-agent.md`](apps/sync-agent.md)) may mount the same store as an
  always-on sync host on desktops. Web implements the same logical schema
  in-browser; mobile is library-only by OS construction. Rejected: agent-only
  (web/mobile can't reach it; every read crosses IPC; headless tui would
  require it), library-only (discards a shipped always-on asset).
- **R3 — The local replica rests plaintext in the user's OS context** (user,
  2026-08-10). § Local at-rest posture owns the statement + threat model.
- **R4 — Secrets ride provisioned capability, not seed-sharing** (user,
  2026-08-10). Additional same-account surfaces hold a device-scoped,
  root-signed, revocable capability bundle — never the identity seed. § The
  store device principal.
- **R5 — One device principal per store replica** (user, 2026-08-10).
  Co-located instances sharing a store share its device identity, hydration
  policy, and peer-plane node identity; the replica, not the process, is the
  device-scoped artifact. Rejected: per-app-install principals (inflates the
  device list, splits hydration policy across co-located apps, multiplies
  peer identities per machine).
- **R6 — The sync plane is multi-master: per-writer logs + frontier merging,
  with the nest as a distinguished replica** (this design pass, 2026-08-10;
  delegated to W0 by the track charter — refutable by the user, and any
  revisit must land before W2 code). [`account-sync-plane.md`](account-sync-plane.md) § The sync plane owns the mechanism;
  rationale there. Rejected: outbox-gossip-only under nest single-master
  (two offline devices would exchange pending intents but never converge
  *applied* state — failing the user's statement 2, "apps sync with the nest
  — and thereby with each other").
- **R7 — Storage posture is per-replica, and it is the principal's key
  reach** (user directive, 2026-08-10 — the general form of the retired
  storage-mode axis, per-account per-target this time). Every sync target
  of an account's plane is either a **reading replica** (its principal
  holds the account's read keys; plaintext materializes locally per R3) or
  a **custodian replica** (no read keys; holds and serves only the sealed
  canonical planes). Posture is never a stored flag, never asked at
  enrollment, and never a wire property — it is *derived* from what the
  replica's capability bundle / grant set can open: user-minted, revocable,
  audited. Defaults unchanged: own devices enroll reading; the nest is a
  custodian. § Replica posture owns the mechanism. Rejected: an at-rest
  plaintext posture for nests — a broadly-granted nest already holds the
  old plaintext mode's *capability* under the ratified grant model
  ([`nest/storage-modes.md`](nest/storage-modes.md)), and key reach over
  sealed bytes is the only posture in which revocation removes anything
  (plaintext, once held, cannot be un-disclosed). **Refined by R9 (same
  day):** what stays rejected is unsealing the *canonical* planes — now
  also identity-breaking, since a record's identity is its sealed block's
  CID — while *materialization of derived views* follows key reach
  uniformly, nest included.
- **R8 — The nest is a role bundle, not a kind** (user directive,
  2026-08-10 — nest-is-another-app, taken all the way). The nest =
  custodian replica of its resident accounts (generalized by R7 to any
  device) + the distinguished-replica arbitration/sequencing role (R6) +
  the **always-on internet endpoint roles** (mail bridges/MX, federation,
  public serving, the PDS, the relay) + multi-tenancy. Only the internet
  endpoint roles are permanently nest-only — they need a stable public
  address and always-on residency (user: the internet-vs-private
  distinction "will remain probably forever"). § The nest decomposed.
- **R9 — Materialization follows posture, uniformly — a user may flip
  their own nest's replica of their own account to reading posture**
  (the nest-as-app symmetry review, user objections 2026-08-10 evening;
  resolved same day — refutable until first build). What a replica
  *materializes* (readable projections, the readable search index) is
  the same fact as its posture on R7's axis: reading replicas
  materialize, custodians don't — app and nest alike, no nest carve-out.
  The owner's widest grant (the **materialization tier**, owner:
  [`encryption-at-rest.md`](encryption-at-rest.md) § Readable classes
  class 4 — bounds, revocation-deletes-views, own-actor-scopes-only,
  retroactivity all live there) is the per-user, per-box opt-in that
  flips it. Canonical planes stay sealed on every replica regardless —
  apps included, which is why this is symmetry rather than a new
  mechanism: an app-side reading replica is already sealed canon +
  plaintext projections (§ Store logical schema item 5). Rejected:
  unsealed canonical at-rest (identity-breaking per the R7 refinement;
  would fork the custody-safe uniform sync forms); pure
  unseal-per-read with nothing readable resting (the prior nest pin —
  it made the at-rest form dishonest about a standing grant's real
  reach and priced nest-side search at per-query unseal).
- **R10 — Hydration policy is per-replica for every replica, the nest
  included; nest dehydration is eviction under a confirmed-custody
  predicate** (user directive, 2026-08-10: file-sync must support
  app↔app content with the nest holding metadata only — "the file
  content on the nest is dehydrated"). Payload presence (hydration) and
  key reach (posture, R7) are orthogonal per-replica axes; the nest
  gets both. Per-set **content residency** is the first user-facing
  consumer ([`../behavior/file-sync.md`](../behavior/file-sync.md)
  § Content residency owns it); segment-file eviction mechanics live
  with the segment stores
  ([`message-segment-store.md`](message-segment-store.md) § Nest
  dehydration). The predicate: a nest may drop (or decline to hold)
  payload bytes only when the fleet's custody accounting confirms
  another replica holds them — or the owner explicitly accepted the
  reduced redundancy at opt-in. The nest's content-bootstrap role
  (§ Nest-side requirements item 3) degrades per scope accordingly:
  metadata bootstrap always works (the index is the always-present
  layer, statement 5 — on the nest too); payload hydration then needs a
  live holding peer, stated honestly in the opting UI.
- **R11 — "No nest" is a supported reduced-role profile, never a
  degraded error state** (user objection 3, 2026-08-10; resolved same
  day). [`account-sync-plane.md`](account-sync-plane.md) § Ordering model owns the statement. The peer-symmetric subset
  — class-1 records, class-3 blobs, commutative/LWW class-2, the
  seen-set — is fully functional with zero nests; what a nest-less
  account lacks is exactly R8's other roles, per kind, not a mode:
  nest-arbitrated kinds (no arbiter), MLS conversations (no delivery
  service — PQ-1,
  [`../behavior/p2p.md`](../behavior/p2p.md)), the internet
  endpoint roles (permanently nest-only, R8) — and the
  supervised-account plane: the guardianship link, guardian policy,
  and reach enforcement are nest-side by design, so child safety is
  **never-nest-optional** (ratified 2026-08-22; owner
  [`../behavior/family-safety.md`](../behavior/family-safety.md)
  § The account age band). Features declare their
  nest dependency per kind (the `offline_class` precedent), never as a
  global "offline mode".
- **R12 — The store's multi-actor future is a physical-backend swap,
  not a logical-API change** (user objection 1, 2026-08-10; resolved
  same day — direction, sequenced with R8's store-library
  convergence). The logical `AccountStore` API stays single-actor
  (opened for one actor; `adopt_identity`'s foreign-actor refusal
  survives — the structural can't-interleave-identities property), and
  the shipped per-actor placement stands today; the **shared
  multi-actor physical backend** — one DB/blob pool with an actor
  ownership column, block dedup across actors under the
  no-cross-actor-existence-disclosure rule (T12's ownership-index
  shape, [`nest/common.md`](nest/common.md) § Blob Store) — arrives as
  a `StoreBackend` swap when the store library converges with the
  nest's segment store (R8's licensed direction), which needs it
  anyway (the nest is definitionally multi-actor, and so is any
  app-as-nest host). Succession/drain and account deletion are
  preserved as logical per-actor erasure over the ownership index
  (`erase_all_account_scopes` semantics), replacing the
  `rm -rf <actor dir>` blast-radius property the per-actor placement
  provides today.
- **R13 — Audience is a per-kind ladder rung, orthogonal to class; class 5
  narrows to what genuinely cannot ride** (user-guided greenfield ruling,
  2026-08-11 — the W2.5 gate; refutable until W2.5 code). Every
  registered kind declares an **audience rung** beside its merge policy —
  two independent frozen registry columns — and rungs are enforced by
  *which key branch seals the kind*, never by client-side classification:
  **delegable** (per-kind keys a grant can hand out), **fleet-only** (a
  sibling derivation branch the grant machinery structurally cannot
  reach), **device-only** (never syncs), **ceremony-only** (the roots —
  nothing can rest in a store sealed under itself). Operational secrets
  (MSEK, provider credentials, rotation keys, period keys, content keys,
  deployment seeds) are thereby ordinary class-2 kinds at the fleet-only
  rung — replicated as ciphertext to every custodian for durability,
  openable by no grant; class 5 keeps only the genuinely unridable
  (roots, MLS state, device-local keys). Entries carry a **universal
  in-seal writer signature** ([`account-sync-plane.md`](account-sync-plane.md) § The class-2 entry form): symmetric AEAD
  alone makes every reader a potential forger, and the signature is what
  lets delegable-rung granting be generous. **Groups are sibling scopes,
  never rungs** — the account ladder is rooted in one account's root and
  re-keyed by its succession; shared-audience data publishes into a
  group-rooted scope (membership lifecycle, not succession lifecycle).
  Disposition table + rationale: § The audience ladder. Rejected: a
  separate credential-store substrate for operational secrets (it must
  either duplicate the plane's replication stack — identical
  ciphertext-on-custodian exposure, two stacks hardened forever — or not
  replicate, trading durability for nothing); an allow-list projection in
  client code (re-creates the classification hazard the seal-branch
  structure kills — an old binary deciding a new field's audience); a
  per-kind "signed?" flag (the same hazard reborn on the integrity axis).
- **R14 — Content keying gains a generation axis: random generation keys,
  device-set distributed, escrowed to chosen holders** (user, 2026-08-11 —
  the greenfield adversarial pass, finding A1;
  `2026-08-11-greenfield-storage-derivation.md` § 11, internal plans
  tree — tracked internally, not shipped). A
  purely root-derived schedule makes two invariants hollow: device
  *removal* re-keys nothing (any seedless replica holding `BackupKey`
  retains the content root forever — the proven wormability-walk rule-6
  refutation), and *deletion* can never crypto-shred (with promiscuous
  ciphertext replication, a "deleted" item at any ex-custodian stays
  openable forever by anyone who ever obtains the root — durability
  purchased with irrevocability). So: content sealing moves onto
  **generations** — a *random* key minted at cadence and at every device
  removal, distributed to the current device set via their enrolled
  bundles, HPKE-escrowed to explicitly-chosen escrow holders (default:
  the user's nest). Removal excludes the removed device from every
  future generation (real forward severance); deleting a generation =
  devices drop it + escrow holders delete the wrap (real
  crypto-shredding, at generation granularity, effective against every
  promiscuous custodian without their cooperation); recovery = ceremony
  + any one surviving escrow holder, redundancy user-tunable by
  escrowing to several — stated honestly in UI. In-system precedent,
  twice: subscription period keys, the index master key's generation
  axis. Accepted cost (put to the user with the ruling): total-loss
  recovery narrows from "seed + any custodian's ciphertext" to "seed
  ceremony + a surviving escrow holder". **Scope + gate:** generation 0
  is the existing frozen root-derived branches — the delegable
  preference cluster (secret-free, recreatable) may ship under it —
  but **no fleet-only kind and no content scope seals a production
  entry under a root-derivable-forever key**; their sealing waits for
  the generation schedule
  ([`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2
  owns the derivation mechanics). Ruled pre-consumer, so this is a
  birth property, not a migration. **Build design ratified 2026-08-13**
  (refutable until built): § The generation machinery (plane half) +
  `owner-key-material.md` § Path A-sibling-2 → *The schedule build
  design* (key half).
- **R15 — Storage-group keying splits from messaging crypto; the
  group-scope key root is a seam** (user, 2026-08-11 — finding A2, same
  pass). R13's "groups are sibling scopes" stands; what narrows is the
  parenthetical "(MLS-derived root)": **MLS roots messaging groups only**
  (conversations — where transcripts are the asset and FS/PCS earn their
  machinery). **Storage groups** (family spaces, shared data scopes,
  future collaborative kinds) key on a boring recipient-set scheme — a
  random scope generation key HPKE-wrapped to each member's key on every
  membership change: offline-tolerant, no epochs, no delivery service,
  composable with R14's generations. Rationale: rooting all sharing in
  MLS couples every shared byte to nest-CAS epochs and the then-unsolved
  PQ-1 delivery problem — a nest-less account would lose not just
  conversations but *every* shared scope — while FS/PCS for an at-rest
  archive whose members retain plaintext is near-valueless. Boundary:
  the shipped M2 folder MLS keying is untouched (it predates the
  plane's group scopes; its convergence is D4-era direction, [`account-sync-plane.md`](account-sync-plane.md) § Substrate
  settlements); the scheme's build detail is T20. PQ-1's resolution
  (2026-08-17, [`../behavior/p2p.md`](../behavior/p2p.md) § Offline share
  initiation) rides this seam: offline-initiated storage shares are born
  as group scopes under this scheme.
- **R16 — The box scope: the nest's own canonical state becomes plane
  data, as target-state direction** (user, 2026-08-11 — delta D1, same
  pass; § The box scope owns the statement). The ~259 bespoke `nest.db`
  tables (tenancy, admin config, policies, wrapped deployment blobs)
  dissolve into a box-rooted scope family + projections + enumerated
  ephemera; admin apps become reading replicas; box recovery = seed
  ceremony + replica bootstrap. Build sequenced far behind W2–W6
  (riding the R8/R12 store-library convergence); ruled now because
  every interim nest-side schema decision is better made knowing the
  destination. The box scope's key-root/admin-read-reach design is a
  named follow-on track (T19), never solved inline here.
- **R17 — The total-classification law** (user, 2026-08-11 — delta D2,
  same pass; owner:
  [`storage-classification.md`](storage-classification.md)). Every
  locally-resting byte on every store — nest and apps alike — is
  **canon**, **derived**, or **enumerated-local**, and each store owes a
  lintable inventory saying which. The charter's own classes and floors
  are the account-plane instance of it; the law itself lives with its
  owner doc.

## Vocabulary

- **The plane** — the whole mechanism this doc owns: replica + store + sync +
  outbox + concurrency + device principal.
- **Replica** — one materialization of an account's data set on one device
  (or one browser origin). Complete in metadata, selectively hydrated in
  payloads.
- **Store** — the on-disk (or in-browser) artifact holding a replica: record
  log + projections + outbox + blob store.
- **Item** — the unit of sync: a content record (immutable, CID-keyed) or a
  mutable-state entry (kind-keyed, merge-policy-governed).
- **Projection** — a queryable, rebuildable view derived from the record log
  (per-kind SQLite tables/indexes). Never a sync unit, never truth.
- **Writer** — one device principal appending to its own log.
- **Frontier** — a per-writer high-water-mark vector; the cursor primitive of
  the multi-master plane.
- **Device principal** — the per-replica identity + capability bundle (R5,
  § The store device principal).
- **Seen-set** — the grow-only per-account structure recording what the
  account has observed (R1).
- **Reading replica** — a replica whose principal holds the account's read
  keys; materializes plaintext projections + merged state locally (R3/R7).
- **Custodian replica** — a replica whose principal holds no read keys;
  stores and serves the sealed canonical planes only (R7, § Replica
  posture).
- **Custody grant** — an owner-minted **keyless** capability naming scopes,
  conveying sync-custody authority with no read reach; the third admission
  witness of the peer plane.
- **Custody floor** — the metadata a custodian necessarily sees: scope ids,
  writer ids/seqs, item CIDs + blinded class-2 item keys + sizes, tombstone
  presence, timing.

> **Reading this doc after the 2026-09-06 partition.** The sections that stayed still cite sections that left, unqualified. An unresolvable `§ <name>` here resolves in one of the five plane docs — [`account-data-taxonomy.md`](account-data-taxonomy.md), [`account-sync-plane.md`](account-sync-plane.md), [`account-offline-mutation.md`](account-offline-mutation.md), [`account-runtime.md`](account-runtime.md), [`account-replica-posture.md`](account-replica-posture.md) — or in the two sections that left on 2026-09-28, [`account-client-lifecycle.md`](account-client-lifecycle.md) and [`config-dissolution.md`](config-dissolution.md), whose headings are unchanged, so the name is the address and only the filename moved.

## Section map

This doc is the plane's **charter and its cross-cutting status**. The five concepts it used to hold are now one doc each; a routing stub sits at every original location.

| Section | Where it lives |
|---|---|
| § The ratified decisions, § Vocabulary | here |
| § The account store (W1) — incl. § Store logical schema (T3) | here |
| § The nest decomposed (R8), § The box scope (R16), § Nest-side requirements, § Migration + compatibility (W7) — its rule | here |
| § Workstreams, § Open questions | here |
| § Implementation status today — the cross-cutting entries (T20's listing, W8's contract slices, T15 eviction, T13 custody kinds, the A5 generation-machinery steps, the device-endpoints writer, W1 slice 1, principal succession) | here |
| § The account-data taxonomy (+ R13, R14); § The recipient-set scheme (T20), via the taxonomy since 2026-09-28 | [`account-data-taxonomy.md`](account-data-taxonomy.md); [`recipient-set-scheme.md`](recipient-set-scheme.md) |
| § The sync plane (W2), § The peer leg (requirement 7) | [`account-sync-plane.md`](account-sync-plane.md) |
| § The offline-mutation contract (W4) | [`account-offline-mutation.md`](account-offline-mutation.md) |
| § Multi-instance concurrency (W5), the W3 seat's build-out | [`account-runtime.md`](account-runtime.md) |
| § The replica boundary (R1), § The store device principal (R4 + R5), § Replica posture (R7) incl. § The custody grant + ceremony (T13), § Local at-rest posture (R3) | [`account-replica-posture.md`](account-replica-posture.md) |
| § The client-side lifecycle (W3), with its status entries (2026-09-28) | [`account-client-lifecycle.md`](account-client-lifecycle.md) |
| § The `__config` dissolution schedule, with its status entry (2026-09-28) | [`config-dissolution.md`](config-dissolution.md) |

**§ The replica boundary — observation-defined (R1) → [`account-replica-posture.md`](account-replica-posture.md)** (2026-09-06 concept partition).
The observation-defined boundary and the fleet seen-set moved there with the rest of the replica family (the store device principal, the per-replica storage posture and the custody grant, the local at-rest posture). The heading is unchanged, so a `§ The replica boundary` citation resolves by swapping the filename.

**§ The account-data taxonomy → [`account-data-taxonomy.md`](account-data-taxonomy.md)** (2026-09-06 concept partition).
The classes and their total classification, **§ The audience ladder (R13)**, **§ The generation machinery (R14)** and **§ The recipient-set scheme (T20)** moved there, together with the export-confidentiality axis entries from the status ledger. All four headings are unchanged, so a `§ <name>` citation resolves by swapping the filename; the `recipient-set-scheme` registry concept moved with them. **§ The recipient-set scheme (T20) moved on again on 2026-09-28**, with its status entries, to [`recipient-set-scheme.md`](recipient-set-scheme.md) — the taxonomy keeps a stub at its heading.

## The account store (W1)

- **Placement.** The store lives under the shipped per-actor placement,
  `<platform state base>/<actor-id-hex>/`, growing out of the
  `fauna_sync_engine::db` floor (its declared extraction trigger —
  [`app-guidelines.md`](app-guidelines.md) rule 9 — has arrived).
  **The platform state base is resolved ONCE, in shared Rust (W6 path
  unification, built 2026-08-15):** `fauna_account_store::root` owns the
  per-OS constant — win `%LOCALAPPDATA%\Fauna\sync`, mac
  `~/Library/Application Support/Fauna/sync` (the **user-domain** root —
  moved out of the app-group container 2026-08-25, since macOS 15+ TCC
  prompts a launchd-spawned agent there on every instance and no user
  decision ever binds the next one; the container is now solely the
  sandboxed File Provider extension's root, and that extension is **not a
  store consumer**, so every store consumer on the machine — the agent, tui,
  and the app when it hosts — still resolves ONE root; the per-domain law
  is owned by [`on-demand-files.md`](../behavior/on-demand-files.md)
  § Apple File Provider binding, *state unification*), linux/unix
  `$XDG_CONFIG_HOME/fauna/sync`
  — the same base the sync agent's `SyncPaths` ships (which now delegates
  here), so all three desktops share one `<install home>/sync` shape. (The
  macOS move carried no store: the seedless agent refuses to assemble
  without an app-enrolled `BackupKey` in the slot, and no macOS app hosts
  the runtime yet, so no macOS account store ever existed under the
  container.) Every
  desktop surface constructs `StoreRoot::platform()`; sandboxed mobile
  shells pass their app container (per-app IS per-user there, by sandbox
  construction); no human ever chooses the path (bucket (1) of the
  configuration rule). One root per user per machine is what makes the
  machine-shared T10 writer-key slot correct — two co-located store dirs
  under one `WriterId` is the journal-equivocation trap the 2026-08-14 ⚠
  Gap entry recorded. No store is adopted from an app's own pre-unification
  base: the move-don't-recreate adoption and its refusing husk were retired
  by the compat-remnant sweep ([`compat-remnant-sweep.md`](compat-remnant-sweep.md)
  § Program 4, tranche B1). It is
  account-scoped class-4 state ("nest-authoritative replica" in
  [`apps/account-scoping.md`](apps/account-scoping.md)'s taxonomy) for
  wipe-tolerance purposes — losing a replica loses no truth the fleet holds —
  while the outbox and the unpublished suffix of the device's own journal
  (with its loose blocks) are the store surfaces *not* wipe-tolerant until
  drained/published (§ The offline-mutation contract owns the precise
  carve-out — the W4 phase-0 ruling).
- **Logical schema direction (settles survey Q5).** A **record log plus
  projections**: ground truth is the append-only log of items — content
  records stored as their canonical CID-keyed blocks, byte-identical to nest
  and wire ([`serialization.md`](serialization.md)); mutable state stored as
  merge-policy-versioned entries — and everything the UI queries is a
  **projection**: per-kind SQLite tables/indexes derived from the log,
  rebuildable at any time. Rationale: the sync unit and the query shape
  evolve independently (projection schema changes are cheap local rebuilds,
  never wire events); it mirrors the nest's own shape (CARv2 segment logs +
  the `segment_records` SQLite mirror,
  [`message-segment-store.md`](message-segment-store.md)); and it is what
  makes the web backend feasible (same log semantics, different physical
  index). Detailed schema: § Store logical schema below (T3, resolved
  2026-08-10).
- **Physical form.** Desktop/mobile: SQLite in WAL mode + content-addressed
  blob sidecars, multi-process-safe (WAL + advisory locks; § Multi-instance
  concurrency). Web: the same *logical* schema over IndexedDB/OPFS via WASM —
  a parallel physical backend behind the same store API; a browser origin can
  never mount another app's directory, so "same on-disk representation" is a
  desktop property by construction (mobile sandboxing likewise keeps sharing
  per-app; tui does not ship there, so nothing is lost).
- **Hydration.** Full logical index in every replica; payload presence is
  per-device policy hanging off the device principal (R5) — the
  [`../behavior/on-demand-files.md`](../behavior/on-demand-files.md)
  placeholder model generalized from folders to every class-3 payload
  and, per kind, to a bulky class-1 record's block bytes (§ Store logical
  schema — the index is the always-present layer).
- **Ownership (R2).** Library-in-every-app over the multi-process-safe store;
  the desktop sync agent optionally mounts the same store as the always-on
  host (replica stays fresh with no app running; natural always-on peer-leg
  endpoint). No IPC read path exists: every reader reads the store.

### Store logical schema (T3 — resolved 2026-08-10, refutable until W1 code)

Five components. The load-bearing split: **ground truth is blocks + merged
state entries + tombstones; the journal is the bounded, ordered change feed
over them — never an infinitely-retained event source.** Correctness never
depends on log retention: incremental sync walks frontiers ([`account-sync-plane.md`](account-sync-plane.md) § The sync
plane), and **per-entry full-state reconcile is the backstop** (the same
law as file-sync's periodic rescan) — a replica that missed compacted
journal rows converges by exchanging entries, because class-2 merge is
state-based (`merge_user_configs` precedent), and class-1/3 presence is
content-addressed.

1. **Block plane** (classes 1 + 3). Content-addressed blocks keyed by the
   fixed 36-byte CID ([`serialization.md`](serialization.md) § CID shape):
   class-1 records as their canonical sealed block bytes — **byte-identical
   to nest and wire**, `record_id == block CID` (the shipped post identity:
   `post_id` *is* the `__post` segment block's CID) — and class-3 payload
   bytes (`0x55` raw blocks / chunk manifests). **The always-present layer
   is the index, not the blocks**: journal rows, the record index, and state
   entries exist in every replica (statement 5's "complete in metadata");
   block presence follows hydration policy, where a class-1 record's block
   bytes count as its payload — small-record kinds (posts, calendar, card)
   default to always-hydrated, bulky-record kinds (mail with attachments)
   may placeholder exactly like class-3. **Placement is a local detail, never an identity**: blocks live
   either in **adopted segment files** — nest CARv2 segments pulled verbatim,
   still byte-identical, indexed by their own CARv2 index — or as **loose
   blocks** (locally-authored records staged before any nest echo, and
   peer-received singles). A store may fold loose blocks into segments as
   local compaction; nothing observes the difference through the store API.
2. **The journal** — per-writer append logs, the sync plane's ground shape
   (R6). A row: `(writer_id, writer_seq, scope, op, item_ref)` where `op` is
   one of `record-added` (class 1: scope + CID), `state-put` (class 2:
   kind-scoped key + entry version), `tombstone` (either). `writer_seq` is
   assigned by the owning writer, monotonic, gapless per writer; other
   writers' rows arrive via sync and are stored verbatim. **A log's identity
   is `(scope, writer)`, never the writer alone** (precision ratified
   2026-08-12, found in the W3 build): `WriterId::NEST_SEQUENCER` is a
   reserved *name* standing for whichever nest sequences a scope, and the
   nest's counter is per `(scope, kind)` — every content scope legitimately
   starts at seq 1 under that one name — so row uniqueness and the
   equivocation refusal are per `(scope, writer, seq)` (store format 2;
   the v1 journal's writer-global key refused the second walked scope's
   first row). The journal never overwrites a held coordinate; what the
   account walk does when a FOREIGN writer's second row meets one under
   another item — carried, journaled nowhere, since 2026-09-16 — is
   [`account-replica-posture.md`](account-replica-posture.md) § The store
   device principal, refinement 11's (the class-1 record plane keeps
   refusing). A device writer publishes to one scope today, so its log
   stays totally ordered; if class-2 ever publishes to several scopes
   (A5's per-rung sub-scopes), the append counter's cross-scope story is
   that design's to rule. Class-3 hydration
   is **not** journaled — payload presence is device policy, not plane
   history. A writer may compact its own log's superseded class-2 rows and
   any class-1 rows whose records rest in adopted segments (the entry
   reconcile + content bootstrap cover late readers); tombstone rows persist
   until every enrolled device's frontier passes them.
3. **State entries** (class 2) — the merged current value per
   `(kind, key)`: canonical dag-cbor value bytes + the kind's merge-policy
   metadata (LWW stamp / per-field versions / CAS base per the [`account-sync-plane.md`](account-sync-plane.md) § Merge-policy
   seam table) + tombstone flag. State-based by design: the entry *is* the
   sync unit for full reconcile; journal `state-put` rows are its incremental
   transport. The seen-set is state entries of a union-merge kind (batching N
   references per entry is sound because union is order-free).
4. **Frontiers + store meta.** Per-scope frontier vectors (consumed-cursor
   state, [`account-sync-plane.md`](account-sync-plane.md) § Feeds and cursors), the store's `format_version` /
   `min_reader_format_version` pair (the standard two-number at-rest scheme,
   [`version-compatibility.md`](version-compatibility.md) § 2.2), the actor
   id, the device principal reference, hydration policy, and the cached peer
   dial candidates ([`account-sync-plane.md`](account-sync-plane.md) § The peer leg). The outbox sits beside these
   (contract + the `OfflineQueued`-only boundary: § The offline-mutation
   contract, the W4 phase-0 ruling; discipline: the shipped
   `transfer_queue` retry ladder generalized — per-intent rows, backoff
   column pair, completion-is-deletion).
5. **Projections** — per-kind query tables/indexes, derived from blocks +
   entries, **decrypted at ingest** (this is where R3's plaintext posture
   does its work: the UI queries plaintext projections; the block plane keeps
   the kinds' seals intact because canonical bytes must round-trip anyway).
   Rebuildable at any time: a per-kind projection version in store meta;
   rebuild = drop + re-walk scope segments and entries under the account's
   keys. Projection schema changes are local rebuilds, never wire events.
   The search index stays a class-4 derived view over the replica.

**How nest CARv2 segments map onto the local log (the bootstrap contract).**
Nest segments are the bulk container of class-1 truth; the journal is
ordering, not content. A fresh replica bootstraps per scope by **adopting
pulled segment files verbatim** (the custodian-pull mechanics —
[`message-segment-store.md`](message-segment-store.md) § Client-device
custodian (pull) — with a plaintext-adoption sink in place of the sealed
custody sink: whole segment files fetched with Range resume, CID-verified,
dropped into the store's segment area — each pair's actor **and kind**
checked against the scope it is filed under before admission, so a source
cannot relabel a bulky dehydrated kind as one the policy always hydrates),
rebuilding its local record index from each segment's sidecar `record_order`
(the mirror-is-rebuildable
property the nest itself relies on), then walking the scope's feed from a
zero frontier to materialize state entries and tombstones — instead of
issuing ~50 per-domain queries. Hydration policy scopes the pull: a
dehydrating replica materializes a scope's index from the feed walk alone
and fetches blocks on demand; verbatim segment adoption is the bulk path
for scopes the policy hydrates. Locally-authored records never wait for a
segment: they stage as loose blocks, journal on the device's own log, and
publish by own-log replay on reconnect (§ The offline-mutation contract,
phase-0 ruling — an `OfflineSafe` store write never enters the outbox; its
journal row is its replay record); whether the nest later folds them into
*its* segments is the nest's storage concern, invisible to block identity.

**Physical realization.** Desktop + mobile: one SQLite DB (WAL) per replica
holding journal/entries/frontiers/meta/projections + the segment/blob file
areas beside it, under `<state base>/<actor-id-hex>/` — the store grows out
of the `fauna_sync_engine::db` floor (already dependency-thin and
extraction-ready by its own module contract), whose device-identity, anchor,
and queue shapes carry over, behind a **trait-abstracted physical backend**
(the store API is the seam). Web implements the same logical schema over
IndexedDB (structured tables) + OPFS (segment/blob bytes) via WASM — the
"wasm `SyncDb` foundation that does not exist" today, and the named unblock
for web's gated mail-backup work; web's current localStorage rail (the
account registry) migrates into the store at W6, not before (the sealed
`__config` replica beside it retired with the rail on 2026-10-02 —
[`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *The closure
order*, step (6)). Android's Room-owned tables converge at W6 likewise.

**§ The client-side lifecycle (W3) → [`account-client-lifecycle.md`](account-client-lifecycle.md)** (split out 2026-09-28).
How an app assembles and runs the store, the planes and the bridge moved there whole — the home crate and *The trigger fired* (web's hosting program), the store-thread shape, the pump with its wake sources, *Commands and passes* and the barrier, the writer key, consumption and the pilot — with its four status entries. The heading is unchanged, so a `§ The client-side lifecycle` citation resolves by swapping the filename.

**§ The sync plane (W2) and § The peer leg — device↔device sync over iroh (requirement 7) → [`account-sync-plane.md`](account-sync-plane.md)** (2026-09-06 concept partition).
Both `##` sections moved whole — the ordering model (R6), the class-2 entry form (T14), feeds and cursors (T4), the merge-policy seam, kind namespacing, nudges and backstops, the substrate settlements, the admission seam and the wormability walk — with the W2.0–W2.6 and observation-intake (T1) status entries. Every heading is unchanged.

**§ The offline-mutation contract (W4) → [`account-offline-mutation.md`](account-offline-mutation.md)** (2026-09-06 concept partition).
The outbox, idempotency and reconnect-with-resume, and the per-kind offline classification criteria moved there with the whole W4 build-out — phases 1–4 including the shared desensitizing rule and its per-app fan-out, which was 121,477 B of this doc's status ledger on its own. The heading is unchanged.

**§ Multi-instance concurrency (W5) → [`account-runtime.md`](account-runtime.md)** (2026-09-06 concept partition).
The engine-singleton election and everything that follows from one engine per account moved there, with the W5/W6 build-out and the W3 per-app runtime seat's host-by-host status. The heading is unchanged. **§ The account store (W1) → *The client-side lifecycle (W3)* stayed here at that partition** — the rules the seat implements are the store's, only the seat's build-out moved; the rules themselves moved on to [`account-client-lifecycle.md`](account-client-lifecycle.md) on 2026-09-28.

**§ The store device principal (R4 + R5), § Replica posture (R7) and § Local at-rest posture (R3) → [`account-replica-posture.md`](account-replica-posture.md)** (2026-09-06 concept partition).
All three `##` sections moved whole, together with **§ The custody grant + ceremony (T13)** nested under Replica posture. Every heading is unchanged, so a `§ Replica posture (R7)` citation resolves by swapping the filename.

## The nest decomposed (R8)

Nest-is-another-app, taken all the way: "the nest" is a bundle of four
roles, and only one of them is permanently special.

1. **Custodian replica of resident accounts** — generalized by R7 to any
   device; the nest's *storage* role is no longer conceptually unique.
2. **The distinguished replica** — arbitration for nest-CAS kinds and
   channel sequencing (R6) need exactly one arbiter per account/scope.
   Nest by default. **Arbiter succession is ruled, built later** (2026-08-11,
   greenfield finding A3 — no longer "a plausible future direction": nest
   migration is a marathon-critical user journey, and box-loss +
   box-replacement already re-point the arbiter today in an undesigned
   way, making every arbitrated kind de-facto lock-in to one box
   identity). The mechanism: a signed **arbiter-epoch entry** in the
   account-state scope naming the arbiter and the frontier at which it
   assumes — itself nest-CAS'd on the *old* arbiter, or ceremony-forced on
   its death — and replicas refuse CAS from a stale arbiter. Ruled
   pre-W2.6 because it is cheap to rule before channel sequencing ships
   and expensive to retrofit after. The same seat machinery is
   instantiated **per MLS group** for the conversation delivery seat —
   holdable by a member device (PQ-1's messaging half, design
   2026-08-17; owner [`../behavior/p2p.md`](../behavior/p2p.md)
   § Offline share initiation).
3. **Always-on internet endpoint roles** — the mail bridges (MX),
   federation, public serving, the ATProto PDS, the relay sidecar: a
   stable public address plus always-on residency. **Permanently
   nest-only** (user, 2026-08-10) — this is what keeps "nest" a product
   concept instead of dissolving into "another app".
4. **Multi-tenancy** — many accounts per box; already orthogonal.

Long-run direction this licenses — **affirmed as the destination, not
merely a licensed direction (D5, 2026-08-11; still no workstream
commitment)**: the nest's segment store and the account store converge on
one store library, the nest mounting it in custodian posture — its
CARv2-segments + SQLite-mirror shape
([`message-segment-store.md`](message-segment-store.md)) and the store's
block-plane + projections shape are already siblings, and the greenfield
derivation lands on exactly one store library as the end state. **R12
rides this convergence:** the converged library's physical backend is
multi-actor (the nest is definitionally multi-tenant), which is exactly
the shared one-DB backend objection 1 asked for on devices — delivered as
a `StoreBackend` swap under the unchanged single-actor logical API, with
T12's ownership-index dedup and logical per-actor erasure (R12 owns the
decision; do not redo the per-actor placement before the convergence).

## The box scope (R16 — target-state direction, ruled 2026-08-11)

The nest's own canonical state — tenancy registry, admin configuration,
policies, wrapped deployment key blobs; today ~259 bespoke `nest.db`
tables with no feed, no replication, no enumerable boundary, and a
hand-maintained recovery dump — becomes plane data in a **box-rooted
scope family**: key root = the box's deployment identity plus admin read
reach; lifecycle = the box's admin set (a third family beside account
scopes and group scopes — succession, membership, and now *the admin
set* as the three lifecycle kinds). Consequences, each the point rather
than a side effect:

- **Admin apps are reading replicas** of the box scope — the admin UI
  reads and writes plane state like any other surface, and "the apps are
  the only configuration surface" stops needing a bespoke nest-side
  config store to be true.
- **Box recovery = seed ceremony + replica bootstrap** — the same two
  primitives as account recovery, replacing the hand-maintained dump
  ([`nest/box-recovery.md`](nest/box-recovery.md) gains a replica story
  it never had).
- **`nest.db` dissolves into scopes + projections + enumerated
  ephemera** (R17's classification applied to the nest itself): what is
  canon rides the box scope; what is derived rebuilds; what is
  genuinely ephemeral (locks, caches) is enumerated as such.

**Sequencing:** build far behind W2–W6, riding the R8/R12 store-library
convergence — ruled now so interim nest-side schema decisions are made
knowing the destination, never as a license to start the dissolution
early. **The key-root/admin-read-reach design is T19** — a named
follow-on design track (who holds the box root, how admin read reach is
granted/revoked, what the box scope's rung structure is), deliberately
not solved here.

## Nest-side requirements

The plane needs three nest-side capabilities that are prerequisites, not
part of the plane itself:

1. **An enumerable per-actor data boundary — BUILT 2026-08-11**
   (closing the deletion-orphans half): `bins/fauna-nest/src/db/actor_tables.rs`'s
   `ACTOR_TABLES` registry enumerates all ~150 tables carrying an actor-identifying
   column (the ~10+ spellings survey § 3.1 found), each tagged `Policy::Purge`
   (safe to hard-delete on account deletion) or `Policy::Retain(reason)` — posts
   (need the existing federation-aware per-post retraction, not a raw purge),
   financial/entitlement records (payment/subscription tables — retention needs
   its own ruling), and identity-lifecycle audit trails (`actor_successions`,
   `handle_cooldowns`) that must outlive the actor by design. A third verdict,
   `Policy::Partial(reason)` (2026-09-21), is for a table where some rows must
   go and some must stay and the registry — one table, one column — cannot say
   which: the purge walk carries a hand-written predicate and a dedicated test
   owns the row rule. `outbox` is the first (a deleted author's queued posts
   go; a queued deletion that can still be sent stays — ruling owned by
   [`../behavior/opaque-carrier-walks.md`](../behavior/opaque-carrier-walks.md)
   § The declared re-point axis); `abuse_reports` joined it 2026-09-26 — a
   deleted reporter's open reports are withdrawn, words gone and the home
   nest's copy followed through the queue, resolved rows stay whole; ruling
   owned by [`../behavior/moderation.md`](../behavior/moderation.md) § Where
   it lands, who acts, and with what). **A second hand-written leg reaches a
   table's SECOND person (2026-09-22)**, whom a registry
   keyed on one column cannot: a deleted reader's pending `subscribe` requests
   leave the author's `subscribe_requests` queue, paid or not. Each is only an
   instruction to grant a tier to an account that no longer exists, and a paid
   row's terms are not the payment's record, which is the provider's and the
   payee's. Their `unsubscribe` rows stay, because draining one removes their
   retained `subscribers` row. Two residuals are declared: the leg reaches only
   a reader this nest hosts, and the boot-time unlock fan-out reconcile can
   re-enqueue a `subscribe` row from that reader's retained, unexpired roster
   row. That row names only the id the roster still holds and carries no key.
   **Deletion reaches the account's predecessors (ruled 2026-09-22; BUILT 2026-09-24).** A succession leaves every `Stay` row under
   the *retired* id, so a purge walk taking only the id being deleted never
   reached a predecessor's rows. Rule: deleting an account deletes, under the
   same verdicts, every id this nest's recorded successions retired into it —
   the whole local chain, back to its first identity. Each predecessor's
   `Purge` rows go, and its `Partial` rows take their legs. `Retain` rows keep
   their own reasons, and `actor_successions` is one of them, so the chain
   outlives the deletion and a retried deletion walks it again. A predecessor
   is the same person under an earlier key, and rows a key rotation left
   behind are not rows the person meant to keep. The rule is for the whole
   `Stay` class, never re-derived per table. **Local** means the retired id
   holds a `users` row: a home-nest ceremony requires one and keeps it
   handle-less, while a peer-recorded succession refuses a locally-homed old
   id, so a peer chain's retired identity was only ever a remote actor here
   and its rows are not this person's local data. The walk runs inside
   `CacheDb::purge_orphaned_actor_rows` (so every caller of the purge gets
   it). Every account deletion is such a caller: they all finalize through
   `pending_actions::finalize_user_deletion`. That covers self-deletion, the
   admin deletions, and, since 2026-09-27, the eviction ladder's deletion
   step, which until then deleted the `users` row inline and never reached
   the purge (`../behavior/admin.md` § Cutting a user off). The post retraction before it needs none, because `content.author`
   moves at succession. **The predecessors' `users` rows go too (ruled
   2026-09-24).** They are `delete_user`'s, not the
   purge walk's, and `delete_user` deletes the whole chain's rows in one
   transaction — under the same verdict as the deleted id's own row, and for
   the reason the residual gave when examined: no table declares a foreign key
   onto `users`, so the row was never an FK target; the `Retain` rows that
   still name a retired id after the deletion (`actor_successions`,
   `audit_log`) name the deleted account's own id the same way once its row is
   gone; and a row left behind is a handle-less ghost in the admin's user list
   that only a by-hand deletion removes. Two things had to move before the row
   could go. *Order:* the purge walk's locality test IS those rows, so
   `delete_user` runs AFTER the walk and atomically — a deletion that dies
   before it re-walks the whole chain on retry, one that dies after it has
   nothing under those ids left to walk (`nest/common.md` § Client-state
   recoverability). *The retired key's refusal at the registration doors:*
   it used to rest on that row alone (`actor_exists`), so every door that can
   create a `users` row now consults `actor_successions` itself
   ([`../behavior/identity-succession.md`](../behavior/identity-succession.md)
   § Enforcement on the home nest, step 4), which `Retain` keeps.
   `CacheDb::purge_orphaned_actor_rows`
   consumes it from `pending_actions::finalize_user_deletion`, closing ~140 of
   the ~147 previously-orphaned tables (the `Retain` set stays open, tracked in
   the registry itself, not a second list). **The posts half of that `Retain`
   set is now BUILT 2026-08-13**:
   `pending_actions::retract_actor_posts` retracts every post the deleted actor
   authored through the same per-post path a user's own `fauna.posts.delete`
   takes (`routes::delete_post_core`), so all four federation legs — nostr
   kind-5, Bluesky write-through, paired replica, ActivityPub `Delete` — fire
   for each one; previously nothing performed that retraction, so a deleted
   account's posts stayed live and servable indefinitely. **Ordering is
   load-bearing and now structural:** the retraction runs *before* the purge
   sweep, because `nostr_accounts` (the signing nsec) and `ap_post_map` (the AP
   push witness) are themselves `Policy::Purge` — retracting afterwards could
   never reach the federated copies. `finalize_user_deletion` takes
   `&Arc<AppState>` rather than a bare `&Arc<CacheDb>` precisely so no caller
   can reach the sweep without having retracted first, and it sits *after* the
   guardianship/admin fail-safes so a refused deletion never destroys content.
   A storage failure leaves the pending action unexecuted so the executor
   retries (retraction is idempotent); a post whose stored author contradicts
   the row that listed it is logged and skipped rather than blocking the
   deletion forever. Posts stay `Retain` regardless: a raw purge of a skipped
   post would destroy it without telling the federated copies. **The succession
   third of the follow-on below is BUILT 2026-08-12:** `record_succession`
   executes its bulk legs from `actor_tables::plain_move_legs()`, so a table
   joins the re-point by declaring `Succession::Move(MoveShape::Plain)` and by
   nothing else; three tables declare `Bespoke` with a stated reason because
   they do strictly more than move rows. **Each entry also carries the actor
   column's *key encoding* (`ActorKey::Blob` / `ActorKey::Hex`) — added
   2026-08-11 after the registry walk turned out to be a silent no-op on
   sixteen tables.** Most of `nest.db` binds the raw 32 bytes as a `BLOB`, but
   the three bridge families (`nostr_*`, `bluesky_*`, `ap_*`) plus
   `bridge_search_policy` declare the column `TEXT` and store lowercase hex, and
   SQLite applies no affinity conversion between a blob operand and a `TEXT`
   column: the delete matched nothing, reported success, and left the rows —
   including `nostr_accounts.encrypted_privkey`, the **deposited nsec**. The
   encoding is registry *data* rather than a convention so a future bridge
   table cannot silently join the broken half, and two tests enforce it: the
   declared encoding must match the column's own declared affinity, and the
   seeded end-to-end purge sweep now seeds each table in *its* spelling (seeding
   a blob everywhere is what kept the sweep green — a `TEXT` column accepts a
   blob, so the test agreed with itself while production rows survived). A
   dedicated completeness gate (`tests::every_actor_shaped_column_is_registered_or_excluded`,
   added) walks every table this connection can
   see for an actor-shaped column and fails loudly if one is absent from
   `ACTOR_TABLES` and not a reasoned exclusion — the doc-comment claim that
   test existed was false until then; a `Policy::Retain`-survival pin
   (`deleting_an_actor_does_not_purge_a_retained_table`) closes the other
   direction, asserting every retained table's rows are still present after
   the purge, not merely absent from the sweep by construction. ⚠ **The
   per-actor export is converged as of 2026-08-15**:
   `export_routes.rs` keeps its 11 hand-written shaped domains — a registry
   entry declaring `Export::Shaped` is skipped by the walk rather than
   exporting the same rows twice, which cluster 9 (2026-08-16) found is
   earned by only two of the tables they read: the other nine domains are
   partial, so their tables ride `Verbatim` and the shaped JSON is the
   friendly view beside the complete record, not instead of it — and gains a
   registry-driven leg
   beside them that emits every `Verbatim`/`Redacted` entry, so a table can
   no longer drift out of the export by being forgotten; what it can still
   do is sit in the `Unreviewed` backlog, which the manifest now declares by
   name. The nest-wide logical dump (`export/logical.rs`, ~25 hardcoded
   names) is deliberately **not** converged. **⚠ The dump third is a TRAP,
   and the remedy is NOT a walk:**
   `DUMP_TABLES`'s narrowness is a *ratified confidentiality boundary*, not
   drift — the S9 self-backup ruling
   ([`encryption-at-rest.md`](encryption-at-rest.md) § Carve-outs;
   [`path-sealing.md`](../behavior/path-sealing.md)) bounds the pre-scrub
   plaintext window by the dump carrying **no name plane at all**, and
   `folders`/`sync_changes`/`snapshots`/`snapshot_files` are all actor-scoped,
   so converging the dump on this registry would pull them in and break that
   bound. A standing gate
   (`export::logical::tests::the_dump_carries_no_sealed_plane`) now reds if it
   is attempted. Any convergence of either export has to carry a **per-table
   confidentiality disposition**, which is a separate design question from the
   deletion/succession axes this registry already answers — an over-broad
   *export* leaks where an over-broad *move* is a security regression.
   **That disposition is RATIFIED 2026-08-15 as the registry's FOURTH AXIS**
: a **required
   `export: Export` field on every `ACTOR_TABLES` entry** — not a separate
   export registry, which would be a second same-keyed list and exactly the
   drift surface this registry was built to remove; a *required* field makes
   the omission-by-forgetting failure a **compile error**, the strongest form
   of the demanded walking gate. Four rules bind the axis (variant mechanics
   live on the `Export` enum's own docs in `db/actor_tables.rs`, like the
   other axes): **(1) Vocabulary**, each verdict with a written reason —
   `Verbatim` (all columns join), `Redacted` (join minus named columns — the
   dump's `content`-sans-`payload` precedent), `Shaped` (the rows already
   reach the export through a named shaped domain of `export_routes.rs`; the
   verdict asserts that domain carries the table **whole**, and since
   2026-08-16 it carries a `covers` column list that
   `every_shaped_verdict_covers_every_column` checks against the live schema —
   so a domain that drops a column or row-filters must be extended, or the
   table ruled `Verbatim`/`Redacted`; a partial `Shaped` is not expressible),
   `WithheldSecret` (credential/key/escrow material never joins: the export
   is retrievable with an **eviction export token** — the weakest credential
   the endpoint accepts — so its contents must be safe under that credential,
   and an export must never mint a new resting place for a secret),
   `WithheldDerived` (re-derivable projections), `WithheldOperational`
   (nest-internal serving state about the actor, not the actor's data), and
   `Unreviewed` (rule 3). **(2) Every verdict is individually human-ruled,
   never derived** from `Policy`/`Succession`/the schema — that inference is
   what the trap paragraph above forbids; registry-*driven* emission is fine
   once verdicts exist, because the walk executes declared verdicts rather
   than inferring them. A wrong Withheld is the status quo; a wrong
   `Verbatim`/`Redacted`/`Shaped` is a leak — bias accordingly, and when
   unsure leave `Unreviewed`. **(3) The honest interim** is
   `Export::Unreviewed` (status quo: absent from the export) plus an
   exact-count down-only ratchet
   (`tests::the_unreviewed_export_backlog_only_shrinks`, the
   `Succession::Unruled` precedent) — the tail is named and non-growable, and
   a new table must declare a real verdict at migration time. **(4) The
   export declares its own partiality:** while any table is `Unreviewed`, the
   export manifest says so (additive JSON fields on `manifest.json`, format
   stays 1: a partiality flag + the unreviewed table names), and withheld
   tables are likewise declared by name and reason class — knowing what the
   nest holds but does not export is part of
   [`principles.md`](../principles.md) § The user always controls
   their data. **Sealed columns ride `Verbatim`/`Redacted` in their at-rest
   form** — the nest never unseals for export, and the S9 dump bound does
   **not** transfer here: the dump's narrowness exists because it rests
   unencrypted in the self-backup store, while the per-actor export goes to
   the data's owner over their own channel — withholding the owner's own
   sealed rows would invert the invariant this export serves. **Universe:**
   the axis governs the actor's own rows (this registry's scope); content
   reachable via membership (group messages by other authors) is served by
   the shaped domains under their own access rules, blobs stay
   content-addressed behind `include_blobs`, and the root `users`/handle
   rows ride the `profile.json` shaped domain (they are not registry
   entries). **The membership half of that sentence gained its second
   instance 2026-08-17** (measured while
   ruling cluster 19): `segment_records` is one table whose `scope_id`
   carries two KINDS of identity — `mail`/`post`/`calendar`/`card` scope to
   an actor, `conv` scopes to the **channel** — so the registry walk's
   `scope_id = ?actor` was correct and complete for four kinds and
   structurally blind to the fifth, and the owner's conversation records
   reached no export at all. The fix is this paragraph's own rule rather
   than a new one: a **`conversations` shaped domain**
   (`actor_tables::gather_conv_records` → `export/conversations/records.ndjson`)
   resolves `actor_channels` membership explicitly, then reads those
   channels' `conv` rows. ⚠ **Three constraints are load-bearing and a
   future session must not "simplify" any of them.** (1) The table keeps its
   single actor-keyed `Verbatim` verdict — **a second registry entry on a
   channel column is the family plane's forbidden shape**, a shared scope
   walked as though it were the actor's, which the registry's own rules
   would then bless because *"the rows are the actor's"* reads true. (2) The
   domain is **not** an `Export::Shaped` verdict: it row-filters to one
   kind, and rule 1 makes a partial `Shaped` inexpressible — so a table may
   legitimately carry an emitting verdict *and* be read by a shaped domain
   on a different key, which is a new combination this instance introduces.
   (3) Disclosure is bounded twice: a member already receives every record
   of their channels over the ordinary serving door
   (`segments::conv::read_after_seq`, which does not filter by author), and
   conv rows persist **no sender at all** (`insert_conv` writes
   `sender_dom`/`spam_disp`/`is_own_submission` as NULL — the same fact that
   denies the moderation path an author to key an obligation row to), so
   there is no third party in them for the export to disclose. The bodies
   those `record_cid`s address ride for the four actor-scoped siblings
   since the same day (the build, next paragraph), and **for `conv`
   through its own door since 2026-08-17**: conv segments are
   channel-scoped files, so the per-actor byte walk cannot reach them, and
   a whole-pair export is ruled out — the pair carries records compaction
   has not yet dropped, where the serving door reads live records. The
   conv-bodies door is therefore the records domain one level down: behind
   `include_blobs`, resolve `actor_channels`, then read each channel's
   live records' sealed payloads through `segments::conv::read_after_seq`
   — the SAME primitive every conv serve surface flows through — so
   tombstone exclusion and the legal-obligation relay-withhold are
   inherited from the single read gate rather than reimplemented (a
   withheld record's absence stays visible: its records row carries the
   takedown reference). Bodies land at
   `export/conversations/bodies/<channel>/rec-<seq>` in at-rest sealed
   form (the nest never unseals for export), the gather's RAM bound is
   the corpus — the blob arm's ruled class, decision (4) below, since the
   gather already holds the whole records corpus — and the manifest
   declares the door (`conversations.bodies_included`) while
   `segment_store.channel_scoped_not_included` goes on naming the
   pair-level absence, now deliberate rather than a gap.
   **Payload stores (RULED 2026-08-17 —
   the segment-store leg of `include_blobs`):** four decisions, one per
   question the row posed. **(1) One flag.** `include_blobs` means *the
   payload bytes my records address ride too* — the blob store and the
   actor-scoped segment planes (`mail`/`post`/`calendar`/`card`) both answer
   to it; which store serves the bytes, and that their reapers differ, is
   nest-internal mechanics no user would ever choose between (the
   configuration-surface invariant — a second knob would be a knob without a
   human). The flag's name is historical; its meaning is payload bytes
   generally. **(2) The eviction export token pulls the same archive a
   session token pulls — segment bytes included. Auth strength does not
   tier the archive; the at-rest seal is the safety mechanism.** Every
   payload byte the archive can carry is the at-rest form — sealed record
   payloads whose keys never rest on the nest (every plane but `post`;
   decision (6) below) — so the archive is safe
   under the weakest credential *by construction*: a token thief gains
   ciphertext addressed to keys they do not hold, strictly less than the
   plaintext index that already rides `Verbatim`. The `WithheldSecret`
   boundary stays **content-class-based** (live credentials and key
   material ride for nobody, under any credential), never caller-based —
   a credential-tiered archive would be a second export shape protecting
   nothing that matters, while defeating the token's whole purpose: its
   one legitimate holder is an account under eviction, for whom this
   archive is the **last copy before scheduled deletion**; an index-only
   archive there is data destruction with a receipt. Named caveat:
   pre-Phase-3 legacy plaintext residue rides in whatever form it rests
   until the S4 boot backfill converges — already true of every payload
   plane, and the backfill, not archive tiering, is the remedy. **(3)
   At-rest form verbatim** — the sealed-columns rule above extended to the
   payload stores: the `.dat`+`.meta` pair rides byte-identical to the
   on-disk file (CARv2 framing plaintext, record payloads sealed;
   [`message-segment-store.md`](message-segment-store.md) § At-rest vs
   transport), the sidecar included because `record_order` is what lets
   the owner's own tooling re-admit the pair (the bootstrap contract). A
   pair is never rewritten: where one record may not ride (decision (6)),
   the whole pair stays home and the manifest says so.
   **(4) Size:** the blob arm's only bound is corpus size (it pre-buffers
   each blob in RAM before the blocking writer — acceptable because the
   gather already holds the whole row corpus); segments deliberately do
   NOT copy that shape — they are local files, so the blocking zip writer
   streams them from disk, O(1) memory, no new bound invented. **Manifest
   visibility:** an additive `segment_store` object on `manifest.json`
   (format stays 1) declares `included`, the actor-scoped `kinds` walked,
   any raced-away files as `skipped`, and `channel_scoped_not_included:
   ["conv"]` — the byte plane a per-actor walk structurally cannot reach,
   declared by name (its records ride via the `conversations` domain above;
   its bodies ride the membership-resolved bodies door since 2026-08-17 —
   row 161, the Universe paragraph above), so `coverage.partial: false` can
   never be read as "you have everything". **(5) The app side: the full archive is the
   DEFAULT and the only shape an app requests — there is NO user-facing
   toggle.** Every app's
   "Export My Data" fetches `include_blobs=true`
   ([`fauna_nest_http::paths::account::EXPORT_FULL`](../../../libs/fauna-nest-http/src/paths.rs));
   the flag is a wire detail the user never sees, not a choice offered to
   them. Four reasons, in the order they bind. **(i) The button already
   promises it.** The shipped strings read *"Download a copy of all your
   data"* (`settings.export_subtitle`) and *"Download all your data as a zip
   archive."* (`data_export.description`) — so the flag is what makes
   existing copy true, and a toggle would mean editing the promise
   *downward*. **(ii) Decision (2) forces one archive shape.** Auth strength
   does not tier the archive, and for the eviction token this document
   already rules an index-only archive *"data destruction with a
   receipt"* — a shape that indefensible under one credential cannot be
   the sane default under the other, because there is only one archive.
   **(iii) The configuration-surface invariant.** *Would a user ever want
   to choose this?* A "leave my content out" checkbox exists only to make
   the download smaller, and the user who wants a subset already has a
   richer dedicated surface — the mail-export wizard's format / mailbox /
   date-range controls (`mail-export.md`, ui.yaml `mail-export`). A second,
   cruder subsetting knob on the account page is divergence, not choice.
   **(iv) It is decision (1) one layer out.** If splitting the flag per
   store would be "a knob without a human", re-exposing the same flag to
   the user under a friendlier name is the same knob with a coat on.
   *Size* is real (a mail corpus can be tens of GB) and is answered by
   telling the user, not by asking them: the button's own description says
   the archive carries content and may be large, and `manifest.json`
   declares coverage. Cross-app uniformity is guarded by
   `tests/e2e-unified/tests/test_account_export_carries_payload_bytes.py`
   (tier_1, source-level — the requested URL is its one observable; the
   button's ID and its journey test landed 2026-08-19, and `ui/settings.md`
   § Data export owns both and the per-app implementation split). **(6) The `post` plane is
   the one payload store whose at-rest form is NOT sealed — and a `post`
   pair holding a legally taken-down post is withheld whole and declared
   (RULED 2026-09-10).** Decision (2)'s premise
   — sealed payloads whose keys never rest here — holds for `mail`,
   `calendar`, `card` and `conv`, but public post bodies rest in plaintext
   in `__post` segments (`segments::post`: the segment block *is* the raw
   canonical post body; only a restricted post carries the sealed
   preview-body envelope). What the weakest credential really pulls from
   this plane is plaintext public, quarantined and taken-down posts. Two of
   the three are safe by a different argument than the seal: a public post
   is public by the author's own act, quarantine is author-visible, and the
   eviction token's one legitimate holder is the author's own account —
   both already ride the `posts` domain under either credential. The third
   is not: [`../behavior/moderation.md`](../behavior/moderation.md) § Legal
   takedown → *Posts* withholds a taken-down body from **every** viewer,
   the author included, and the nest re-serving compelled bytes breaks that
   rule whoever receives them. Decision (3) then bites: a pair cannot be
   byte-identical *and* withhold one record. **Ruled: withhold the whole
   pair, and declare it.** Any `post` pair whose segment holds a currently
   taken-down record stays out of `export/segments/post/` — both halves,
   since a `.meta` alone re-admits nothing — and `manifest.json`'s
   `segment_store` gains an additive `withheld` array, one
   `{kind, segment_id, reason: "legal_takedown"}` per pair, so the owner
   sees which pair is missing and why, the way `skipped` and
   `channel_scoped_not_included` already name what did not ride. The plane
   stays walked (`kinds` still lists `post`); every other pair rides
   verbatim; the same author's other posts in that segment still reach
   them through the `posts` domain, so no post body the author may hold is
   lost. The cost is the re-admission contract for that one segment while
   the takedown stands — and it lasts as long as the compelled bytes do: a
   takedown sets `content_meta.legal_takedown_ref` and never
   segment-tombstones the record, so compaction keeps it and an overturn
   puts the pair back in the next archive. **Corrected 2026-09-11 — it is
   not “exactly as long as the flag”, and reading it that way was the gap:**
   the author's own delete removes the flag's row while leaving those
   bytes for compaction, so the pair rode again with the compelled body
   inside. The withhold set is now the taken-down posts among the author's
   own **unioned with the posts they deleted while taken down**, and it
   resolves them through a tombstone-inclusive segment lookup, since a
   deleted record's mirror row is tombstoned the moment the delete lands
   ([`../behavior/moderation.md`](../behavior/moderation.md) § Legal
   takedown → *Posts* owns the mechanism and why the fact is captured in
   the delete rather than reconstructed later). The withhold set is the
   taken-down posts among the author's own, resolved to their segment
   through the mirror's `record_cid` index *after* the plane's manifest is
   read, so a compaction racing the gather moves the record into a segment
   the walk did not enumerate, never out of the withhold. **The union holds
   regardless of which came first (2026-09-13):** a takedown issued AFTER
   the author's own delete (`../behavior/moderation.md` § Legal takedown →
   *Posts*, "deleted, then the order arrives") writes the identical
   `legal_takedown_deleted_posts` row through the same tombstone-inclusive
   read, so the pair withhold treats both orderings — takedown-then-delete
   and delete-then-takedown — as one union, never two mechanisms. **Rejected:**
   *take `post` out of the pair leg beside `conv`* — `conv` is out for a
   structural reason (channel-scoped, unreachable by a per-actor walk),
   while a `post` pair is the artifact the recovery-nest restore
   (`segments::post::restore_from_manifest`, the cross-location-backup
   use case) re-admits; dropping the plane would cost every author that
   path to spare the rare taken-down segment. *Rewrite the pair without
   the withheld record* — breaks the verbatim rule and the re-admission
   contract for every pair (a rewritten pair matches neither its sidecar
   nor its manifest) and makes the export a second segment writer that
   must stay bit-compatible with the store's own; withholding one pair
   whole costs strictly less. Witness: `bins/fauna-nest/tests/export_api.rs`
   `a_taken_down_posts_body_never_rides_the_segment_pair_leg_either` — the
   real endpoint, both halves absent, the mail pair still verbatim, the
   manifest's declaration, and the pair riding again after the overturn.
   Per-actor blob
   scoping/ownership (survey Q9) is a **separate** mechanism riding the same
   underlying need — **resolved 2026-08-10 (T12): ownership index over the
   one shared dedup pool, per-actor pools rejected; owner
   [`nest/common.md`](nest/common.md) § Blob Store → Per-actor ownership**
   (incl. the no-cross-actor-existence-disclosure rule) — still unbuilt, and
   does not consume `ACTOR_TABLES` (blobs are content-addressed, not
   actor-column-keyed). The note that a deleted account's posts sat
   `Policy::Retain` with their federation-aware retraction unwired is
   superseded above: that retraction is now BUILT — see this item's own account-deletion paragraph
   above for the mechanism (`pending_actions::retract_actor_posts`, the
   structural before-the-purge ordering, and the partial-failure handling).
   **`folder_channel_claims`'s deletion disposition — settled 2026-08-12** (the succession axis's completeness
   walk had registered it `Policy::Retain` with the deletion question
   explicitly left open; [`succession-aftermath.md`](../behavior/succession-aftermath.md)
   § Propagation → the name-heuristic blockquote records the finding).
   `Policy::Purge` now, but the registry flip is not the fix by itself: a
   claimed channel exists only to serve the ONE folder its claimant bound
   to it (a set never re-binds to a different group), so
   once that owner's `folders` row is gone, `content_key.put` /
   `members.evict` / a roster-managing MLS Commit (owner-only member-side —
   `federation.md` § Cross-nest shared folders + channel append) can never
   again find a caller == claimant — the channel is permanently dead. Purging only the claim row would have
   left every OTHER member's own `actor_channels` roster row (added by the
   owner's `welcome.deliver` share) surviving as an un-rotatable, un-leavable
   stub with no owner left to evict them — a client-causable unrecoverable
   state (`nest/common.md` § Client-state recoverability) hiding behind what
   looked like a one-line registry edit. The real fix lives in
   `sync_storage::delete_folder_rows_in_tx`, shared by both
   `delete_folder_for_user` (one shared set) and
   `delete_all_folders_for_actor` (the whole account): for a bound set it
   tears down `folder_channel_claims` / `actor_channels` /
   `folder_member_access` / `folder_content_keys` for the WHOLE channel —
   every actor on it, not only the owner's own rows — so the `Policy::Purge`
   flip above is a defense-in-depth backstop, not the primary mechanism.
   Pinned by `sync_storage::tests::deleting_the_claimants_account_does_not_strand_other_members`
   and `::deleting_one_shared_set_does_not_strand_other_members`.
2. **The generalized item feed** ([`account-sync-plane.md`](account-sync-plane.md) § The sync plane): `sync_changes` grown to
   all account-data kinds with the blob-vs-manifest discriminator, plus the
   nudge from every writing handler.
3. **Durable idempotency + account bootstrap.** The durable idempotency
   table — **BUILT 2026-08-13** (§ The offline-mutation contract owns the
   record) — and an account-bootstrap path
   for a fresh replica — the ratified client-device custodian pull
   ([`message-segment-store.md`](message-segment-store.md) § Destination
   capability) is the content bootstrap: a new device pulls segment replicas
   instead of issuing ~50 per-domain queries.
4. **The generation escrow doors (R14 build design, 2026-08-13) — BUILT the
   same day.** `fauna.generation.escrow.{put,get,delete}` + the holder-signed
   durable receipt — § The generation machinery → *The escrow doors* owns
   the contract; the nest is the v1 default holder and the receipt signer
   (deployment identity). Build record: § Implementation status today →
   *Step 4 is CODE*.

## Migration + compatibility (W7)

The plane lands **beside** the imperative RPC surface — additive everywhere,
both live for at least a full major version, per-surface cutovers behind the
shared store, no flag day ([`version-compatibility.md`](version-compatibility.md)
governs; the wire is already versioned for exactly this). No user-data loss
throughout ([`../principles.md`](../principles.md) § No user-data loss) — the
outbox in particular must never be dropped by an upgrade.

**§ The `__config` dissolution schedule → [`config-dissolution.md`](config-dissolution.md)** (split out 2026-09-28).
The principles P1–P6, the kinds, the phases and gates (E0, E1, E3, M, C) and *The closure order* moved there whole, with the schedule's status entry. The heading is unchanged, so a `` § The `__config` dissolution schedule `` citation resolves by swapping the filename. The rule above it stays here.

## Workstreams

The build partition (evidence + per-workstream prior art: survey § 5).
Sequencing: W0 (this doc) → W1+W2 in tandem (store schema and feed contract
co-design) → W3 surface-by-surface (pilot: the preference cluster — ruled
2026-08-12, § The account store → *The client-side lifecycle*, superseding
the original "notifications or contacts" suggestion because W2.5 built the
cluster's kinds + bridge first) → W4 → W5/W6 as they unblock → W7
throughout.

| Id | Deliverable | Contract section |
|---|---|---|
| W1 | The account store (shared Rust; web backend) | § The account store |
| W2 | The general sync plane + peer leg | [`account-sync-plane.md`](account-sync-plane.md) § The sync plane, § The peer leg |
| W3 | Read-path inversion (managers read the store) | § The account store |
| W4 | Write-path inversion (outbox; classification) | § The offline-mutation contract |
| W5 | Multi-instance + secrets | § Multi-instance concurrency, § The store device principal |
| W6 | Per-platform adoption + path unification | § The account store |
| W7 | Migration/compat discipline (spans all) | § Migration + compatibility |
| W8 | Custodian replicas (key-less custody on any device) | § Replica posture |

W8 sequences after W2's same-account leg and behind the PQ-2-class
hardening of the admission core; what W1/W2 must hold for it is listed at
the end of § Replica posture. **W8 OPENED 2026-08-15 (user decision)** —
both gates already met; the opening resolved T13/T15/T16 (§ Replica
posture → The custody grant + ceremony / Custody policy; UI in
[`../ui/nests.md`](../ui/nests.md) + [`../ui/devices.md`](../ui/devices.md)).

Noted for the W5/W6 era (D6, 2026-08-11 — no ruling needed): per-device
operational state that today rests machine-local — location bindings,
photo-ingress policy, optionally TOFU pin observations — becomes
**device-keyed fleet-only class-2 kinds**, so every device's operational
state is visible and manageable from any of the user's devices.

## Open questions

Indexed register of every `TBD` above; resolve in the owning section, retire
the row. **T1–T5 resolved 2026-08-10** (second design pass) in their owning
sections — T1/T2: § The replica boundary; T3: § Store logical schema; T4:
[`account-sync-plane.md`](account-sync-plane.md) § Feeds and cursors; T5: [`account-sync-plane.md`](account-sync-plane.md) § The peer leg — rows retired, numbering not
reused. **T6/T7/T9/T10/T11/T12 resolved 2026-08-10** (third design pass) —
T6: `devices.md` § Offline compose; T7: `devices.md` § Device-signed
authoring; T9: § Multi-instance concurrency (election + poll-with-poke
bus); T10: § The store device principal (slot mechanics); T11:
`apps/sync-agent-credentials.md` § Credential model (W5 convergence); T12:
`nest/common.md` § Blob Store → Per-actor ownership — rows retired,
numbering not reused. **T8 user-decided 2026-08-10** (resident-where-
onboarded; stricter postures = post-W5 opt-ins) — § The store device
principal; row retired. **T14 resolved 2026-08-10** (the pre-W2 freeze, W2
scoping session) — [`account-sync-plane.md`](account-sync-plane.md) § The class-2 entry form +
[`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2; row
retired. **T13/T15/T16 resolved 2026-08-15** (the W8 opening, user
decision) — T13: § Replica posture → The custody grant + ceremony; T15:
§ Replica posture → Custody policy; T16: [`../ui/nests.md`](../ui/nests.md)
+ [`../ui/devices.md`](../ui/devices.md) custody facets, registered through
[`../behavior/participants.md`](../behavior/participants.md) — rows
retired, numbering not reused. **T20 resolved 2026-08-17** (the T20
design pass, demand-activated by the PQ-1 resolution) — mechanics:
§ The audience ladder → The recipient-set scheme; key-material half:
[`key-material-hierarchy.md`](key-material-hierarchy.md) § Audience: a
storage group; all three transcribed constraints discharged in the design
((a) structurally, (b) explicitly at the scheme's last bullet, (c) via the
reception-key rotation coupling), the two 2026-08-17-added deliverables
included — row retired, numbering not reused.

| # | Question | Owner / when |
|---|---|---|
| T17 | Materialization grant impl shape: scope string, nest-side holder identity, materializer worker cadence, per-actor materialized-area layout, opt-in UI + IDs. Constraint transcribed 2026-08-13 (the R14 composition — stated nowhere until now): once content scopes seal under generations, a *standing* scoped-read/materialization grant must keep opening newly-minted generations — yet generation keys distribute only to enrolled fleet devices (mint wraps + top-ups) and to escrow, so the grant plane needs its own continuing per-generation key vehicle at capability positions; the built mail content-sealing epoch machinery (client-minted bounded per-epoch key handovers, `encryption-at-rest.md` § Capability tiering) is the in-system template. The design must also state the severance asymmetry honestly: a device removal severs the removed device's future reach while a standing grant continues across the same mint — two deliberately different axes (fleet membership vs. granted positions), and conflating them would either break R9 at every mint or quietly turn grants into fleet members | encryption-at-rest.md + ui/nests.md, first build (unscheduled) |
| T18 | Nest dehydration build detail: the per-set residency column, eviction worker, and the custody-receipt parameters (N-of-M, aging margins, re-verification cadence) — the receipts *contract* itself is ratified (A7, 2026-08-11; message-segment-store.md § Nest dehydration), only its numbers and build shape remain | file-sync.md + message-segment-store.md, first build (unscheduled) |
| T19 | The box scope's key-root + admin-read-reach design (R16 follow-on): who holds the box root, how admin read reach is granted/revoked, the box scope's rung structure | this doc § The box scope, before any box-scope build (unscheduled — far behind W2–W6) |

## Implementation status today

**The `__config` blob rail retired 2026-10-02 (closure step (6))**. The cross-cutting status of the migration: every `UserConfig` field rests on its plane kind, `fauna.config.{get,put}` and the whole-record `UserConfig` are gone, and the dissolution schedule is complete — [`config-dissolution.md`](config-dissolution.md) § Implementation status today owns the entry.

**The status entries of the two sections that left on 2026-09-28 moved with them.** To [`account-client-lifecycle.md`](account-client-lifecycle.md) § Implementation status today: *RULED 2026-09-26 — § The client-side lifecycle → The trigger fired*, *RULED + BUILT 2026-09-25 — the runtime's own push arm and the `__config` nudge*, *RULED + BUILT 2026-09-22 — Commands and passes, the local-write wake and the explicit barrier*, and *BUILT 2026-09-24 — every pump-owned write audited and promoted*. To [`config-dissolution.md`](config-dissolution.md) § Implementation status today: *RESCHEDULED 2026-09-27 — § Migration + compatibility → The `__config` dissolution schedule*.

**RULED 2026-09-22, BUILT 2026-09-24 — § Nest-side requirements item 1, *Deletion reaches the
account's predecessors*.** `purge_orphaned_actor_rows` runs its whole walk
for the deleted id and for every local predecessor
`successions::local_predecessors` resolves, pinned by
`deleting_an_account_purges_its_local_predecessors_stay_rows` (a two-hop
chain, a bystander and a peer-recorded predecessor) and by
`deleting_an_actor_does_not_purge_a_retained_table`, now seeded with a
predecessor too. The same item's subscriber leg is BUILT
(`subscribe_requests_purge_for_deleted_subscriber`, pinned by
`a_deleted_readers_pending_subscribe_requests_leave_the_authors_queue`).
**The predecessors' `users` rows — RULED + BUILT 2026-09-24:** `CacheDb::delete_user` deletes the chain's rows with the
account's, in one transaction, after the purge walk; pinned by
`deleting_an_account_deletes_its_local_predecessors_users_rows` (the chain
and the bystander, and the `Retain` chain surviving). The registration doors
consult `actor_successions` themselves (`auth_core::successor_of`, mapped to
`fauna.auth.superseded` on `fauna.account.register`,
`fauna.account.invite_request.submit` and `fauna.auth.claim_admin`), pinned by
`a_retired_key_is_refused_at_every_registration_door_after_its_successor_is_deleted`
— the whole production deletion path (`finalize_user_deletion`) followed by
all three doors. The eviction worker's phase-3 deletion was a separate,
pre-existing gap: it deleted the `users` row inline and never ran the purge
walk at all. It was closed 2026-09-27. Phase 3 now
marks the account `deleting` and the worker finalizes it through
`finalize_user_deletion`, retrying a refused finalize each tick. This is pinned
by `eviction::tests::an_evicted_account_is_finalized_through_the_purge_walk`
(the account, its local predecessor and their `Purge` rows gone, a bystander
kept, the deletion audited) and
`a_refused_finalize_holds_the_row_and_retries_next_tick`.

**Unbuilt (ratified 2026-09-05, the build rulings 2026-10-02): the third-party kinds of [`third-party-kinds.md`](third-party-kinds.md)** — the manifest-as-document-member, consent-time rung
derivation, the own-domain / foreign-kind rules, the `ext` sub-scope and the record doors;
the A5 per-rung partition they waited on is built, and the per-kind sub-scope is that doc's. Sequencing:
[`third-party.md`](third-party.md) § Implementation status today.

**Built (2026-09-02): the device term of the frontier ceiling's sizing is a
real bound** ([`account-sync-plane.md`](account-sync-plane.md) § Feeds and
cursors → *The walk is bounded, and the spin refusal is not the bound*).
`fauna.sync.register` reads the caller's tier and refuses a
**new** device past `max_devices` with the typed
`fauna.sync.device_limit_exceeded` (`bins/fauna-nest/src/sync_handlers.rs`), the
count and the insert share one connection lock, and `validate_tier_caps` bounds
the admin-settable value a ratio below `fauna_sync_engine::MAX_FRONTIER_WRITERS`
so the two numbers are related in code rather than only in prose
(`bins/fauna-nest/src/admin_ws_handlers.rs::MAX_TIER_MAX_DEVICES`). Two carve-outs
are pinned by `bins/fauna-nest/tests/conformance_device_tier_cap.rs`: a
re-register of an id the actor already holds always succeeds (the row is an
upsert — refusing it would strand a device at the cap), and the nest's own
WebDAV pseudo-device does not consume a user slot. The cap is off on the
embedded desktop nest (`enforce_tier_quotas`), where a tier ceiling on the
owner's own machine is nonsense.

**Built on the nest leg (2026-09-13), deferred on the store-served legs:
frontier compaction for retired succession writers.** The ceiling's other
terms stay unbounded — one retired identity per succession, kept permanently —
so an honest frontier can in principle reach the ceiling after a long enough
succession history on one scope. Against a nest that honours the serve-order
watermark that is no longer a refusal; on the store-served legs, and against a
nest predating the watermark, it stays a pass that refuses, re-grows and
refuses again with no user-facing remedy — reachable only after a lifetime of
successions, so a stated gap, not a live defect. Mechanism, the deferral and
the build's status: [`account-sync-plane.md`](account-sync-plane.md) § Feeds
and cursors → *Compaction is a serve-order watermark* and its
§ Implementation status today → *Built — frontier compaction as a serve-order
watermark, nest leg*.

**Partly built (2026-08-17): the recipient-set scheme (T20 — § The audience
ladder → The recipient-set scheme).** *Built — the scope's identity + the
roster lattice (the sealing-agnostic half):* the `group:<scope-id-hex>` scope
family (`fauna_protocol::scope` — a SHARED plane like `folder`, so neither
wide `AdmittedScopes` form admits one and its only door is the explicit
`Named` list); the birth record and its content-derived scope id, the roster
records and their content-derived entry ids, `join_group_roster`, and the
verified `RosterView` (`fauna_core::group_scope`). The lattice is the R14
machinery **transplanted, not re-invented** — the join shares
`generation::two_phase_winner` and `RosterView` is `FleetView`'s twin — and
re-admission mints a fresh entry id, so add-wins resurrection is
unrepresentable rather than merely refused. The authority root is a
*parameter* of `RosterView::build`, which is the T19 reuse seam in code.
*Built — the membership witness (the admission seam's FOURTH kind is code):*
`fauna_peer_share::admission::verdict_for_group_membership` behind the
`GroupRosterState` seam, with `WITNESS_GROUP_MEMBERSHIP` on the wire
vocabulary. It is deliberately **not** unified with the M2 arm — M2 is a claim
the evaluator resolves from local state, this is a carried certificate plus a
local supersession check, and the claim-list dispatch refuses the group kind
*by name* rather than admitting on a claim alone. PT-1b runs before any
signature work; the authority root comes from the evaluator, never the
witness; `expires_at` is `None` because severance is the next admission
evaluation, per this section's own bound. One verification implementation
serves both readers (`fauna_core::group_scope::verify_enrolled_entry`), so a
witness can never be admitted by rules the plane would refuse. *The carriage
slot is CODE too (2026-08-17):* `PeerShareAdmitRequest.group_witnesses` —
additive, one carried certificate per claimed group scope, an old encoder's
request decoding with the slot defaulted (pinned) — with the admit handler's
certificate arm behind a pump-fed `GroupRosterState` seam (fail-closed
unwired; wired in production 2026-09-23 — `account-data-taxonomy.md`
§ Implementation status today owns that entry), a per-connection verdict slot that MERGES the two families (union
of `Named` scopes, sooner expiry — never a clobber, never a widening of a
non-`Named` form), `admit_group_over` as the dialer half, and the inner
certificate decode in the PQ-2 hardening coverage. Family separation is
pinned: a group verdict never opens the folder door for the same 32 bytes.
*Built — the generation half (slice 1b, 2026-08-17):* the group generation
kinds re-targeted from R14 in `fauna_core::group_generation` — mints
(authority-device-signed with the `DeviceAuthorization` carried inline, the
roster-entry carriage), actor-keyed per-healer member top-up cells, the
actor-keyed unkeyable signal, and `resolve_admissible_group_tip` with **both
ruled deltas in code**: wraps target roster-entry reception keys, and
admissibility is roster coverage (every listed entry enrolled — the
severance-forcing arm — AND every enrolled entry covered by an inline wrap or
a verifying member top-up), with no escrow kinds on the plane. The wrap doors
and pure builders live in `fauna_mls::wrapped_blob::group_generation_wraps`
(entry-id slots, group-context commitment check at every open); every context
string is a new frozen group-named sibling, so nothing made in one plane
verifies in the other (law-tested). **The machinery-sealing ruling
(2026-08-17) is code** (owner:
[`key-material-hierarchy.md`](key-material-hierarchy.md) § Audience: a
storage group, the machinery-root bullet — stratification argument and
exposure stated there, never restated here): `GroupMachineryRoot` + its
schedule (`fauna_core::crypto`), the `machinery_root_commit` field on
`GroupBirthRecord` (the record is now **frozen**), and the joiner-side
commitment check. **The registry-home question is DECIDED and built:**
`fauna_protocol::group_state` is the group plane's own two-column registry
(merge policy + `GroupSealing::{MachineryRoot, GenerationTip}`), a sibling
table deliberately not rows in the account-state `POLICIES` (its
`AudienceRung`/home-scope columns are account-plane concepts — the module
doc owns the argument); `apply_class2` carries the group kinds' merge arms.
The **group-reception keypair kind is code** —
`fauna.state.group-reception-key`, the account plane's fourth
`GenerationTip` row (Immutable, pubkey-digest keys, old rows retained for
old wraps) with the actor-signed published half; its fleet-mint rotation
*call* rides the R14 step-7 trigger wiring the engine still owes.
*Built — the ceremony's carrier-agnostic half (slice 3 first half, same
day):* the signed offer/accept/deliver payloads
(`fauna_core::group_ceremony` — the W8 sender-binding idiom; the offer earns
its scope id from the carried birth record, the accept carries the
recipient's own signed reception half, the deliver's machinery snapshot
adopts through ordinary `apply_class2` strictness) and the admission-bundle
doors (root + retained generations sealed to the joiner's reception key,
root commitment verified in-door against the birth record). *Built — the ceremony state machine (same
day):* `fauna_client_capabilities::group_ceremony` over the new
`UserConfig::group_shares` sub-record (record-then-act; the union merge
arm carries the held root until its plane row lands — since 2026-09-28 the
`fauna.state.group-share-ceremony` kind, [`config-dissolution.md`](config-dissolution.md) § The `__config`
dissolution schedule → *The kinds*), with the two-party
in-memory capstone proving begin→offer→accept→deliver→admit end to end.
*Built — the peer-channel carriage (slice 3's last piece, same day):*
three `fauna.peer.share.ceremony.*` kinds on the share serve set,
initiator-originated, first-contact admitted by the receive-act
expectation, capstone re-proven over a real channel —
[`../behavior/p2p.md`](../behavior/p2p.md) § Offline share initiation →
*Built — the ceremony carriage* owns the mechanism + walk. *Built — the
bind door (2026-08-17):* the listener composition with rule 7's brake made
structural, plus the affordance's shared paint decision —
[`../behavior/p2p.md`](../behavior/p2p.md) § Implementation status today
owns it. *Still design-only:* group content-kind sealing (arrives with the
`p2p-share` data plane as `GroupSealing::GenerationTip` registry rows) — the
tui affordance itself (slice 4) landed 2026-08-18, both roles (below).
Consumer chain: [`../behavior/p2p.md`](../behavior/p2p.md) § Offline share
initiation (build split declared there).

**The group plane's own W2.4 equivalent is BUILT (2026-08-17 — the
registry-without-a-store finding, closed).** *Store table:*
`fauna-account-store` gained `group_entries`, keyed `(scope, kind, key)` —
one member holds many scopes and every scope carries a `fauna.group.birth`
row at the same logical key, so the account table's `(kind, key)` PK could
never hold them (the two-scope collision is regression-pinned); journal
rows, frontiers, and relay rows for group scopes ride the existing
scope-keyed tables under the `group:<scope-id-hex>` scope string
(`fauna_protocol::scope::GroupScope`). *Plane:*
`fauna_sync_engine::group_state_plane::GroupStatePlane` — the account
plane's mechanics at the one ruled structural difference: entries seal
(form v1) under the scope's `GroupMachinerySchedule` — possession of the
birth-minted root — rather than the owner-`BackupKey` schedule;
`GroupSealing::GenerationTip` is a registered seam whose write door refuses
and whose v2 feed rows count `unopened` until group content-kind sealing
lands. **Pull-only by construction**: no nest write kind exists for a group
scope — the feed is the share serve set's relay pull (transport wiring:
[`../behavior/p2p.md`](../behavior/p2p.md) § Cross-user shared-set
transfer), so a write lands journal + entry + sealed relay row and the own
frontier slot never advances. *Applier:* `adopt_rows` (the ceremony
bootstrap — coordinate-less snapshot rows through `apply_class2`
first-contact strictness, winners re-authored on the adopter's own log) and
the `walk`/`reconcile` pair (machinery-root trial-open → merge → ingest +
frontier accounting). *Driver glue:* `write_held_root_row` /
`write_reception_key_row` write the two custody kinds through the member's
fleet-scope account plane (both `GenerationTip`-sealed; the A5 door and R14
gate enforce routing and severance). *Proof* —
`fauna-sync-engine/tests/group_plane_restart.rs`: the full two-seat
cross-account ceremony, custody + plane writes, store **restart**, then the
scope's roster + tip resolved from persisted rows alone with the admission
bundle re-opened from the reloaded reception secret; and the serve shape —
one member's sealed relay rows walked onto another member's replica, and
provably dark to a holder without the root (the custodian posture).
**The listing landed 2026-08-18 (tui, the lead app):** an app reaches this
plane through `AccountStoreHandle`'s four group doors — `put_group_held_root`
and `put_group_reception_key` (the two fleet-scope custody rows, through the
plane's real R14 door), `adopt_group_rows` (the ceremony snapshot; scope and
sealing schedule both taken from the one held-root record, so no caller can
pair one scope's rows with another's root), and `group_scope_states` — and
paints `fauna_sync_engine::group_scope_view::summarize_group_scope`, which
answers `None` for a scope with no birth row (or one that is another scope's — recipient-set-scheme.md § *Scope birth + the authority seam*). That asymmetry is the rule the
surface enforces: **the store is the listing's source, the ceremony record
never is**, since a record whose rows never landed names a scope the device
cannot read a byte of. Consumer + the affordance's own build state:
[`../behavior/p2p.md`](../behavior/p2p.md) § Offline share initiation →
*Built — the affordance, both roles*. The write side of these doors is now
proven by a live ceremony too, not just unit and restart tests: the
co-present dial's addressing gap is resolved, and the tier_3 two-seat journey
drives a real dial through two real apps to the recipient's folders page
listing the shared set — `adopt_group_rows` reached for real
([`../behavior/p2p.md`](../behavior/p2p.md) § Offline share initiation →
contract point 1, the addressing resolution + its proof). *Still open:* the
share serve set does not yet serve a group scope's state-entry feed (the
`p2p-share` workstream wires the transport this plane's walk already
speaks).

**W8's MECHANISM IS COMPLETE: all six contract slices are CODE
(2026-08-15/16 — witness + admission arm, capability-plane lifecycle,
registry kinds, ceremony, custodian runtime leg, nest custody door), and
since 2026-08-16 T15's POLICY is code too (W8.7 — budget, metering,
payload-only eviction, and the A7 receipt they mint). What remains of the
custody surface is the T16 UI facet, the receipt's carriage +
owner-side fold, and the recorded residuals below.** *Built — W8.1:* the `CustodyGrant`/`CustodyScopeSet`
witness + `verify_custody_witness` (`fauna_core::custody_grant` — the
`DeviceAuthorization` twin's four rules; owner-signed `EmbedAsBytes`
carriage); the carve-out predicate `is_co_authored_scope` as the one owner
of the shared-audience vocabulary (`fauna_protocol::scope` — a deliberate
blocklist so single-principal coverage never silently decays); the third
admission arm (`fauna_peer_sync::admission` — the predicate-shaped
`AdmittedScopes::AllOfAccountSinglePrincipal` for the `Account` form,
`Named` for the explicit list, `evaluate_witness` as the one by-name
dispatch both exchange halves share); the serve/pull custody-revocation
views (`PeerSyncServerConfig::custody_revoked` / `admit_over_as` — **no
view fail-closed-refuses custody witnesses**, so wiring the synced-log
snapshot is what enables custody serving, owed to the W8.5 runtime leg);
and the admit exchange's inner witness parsers (`device-authorization` +
`custody-grant`) in `KIND_PAYLOAD_COVERAGE` with committed corpus. The
key-binding rule and the carve-out are mutation-red-verified (each
mutation reds exactly its pin). *Built — W8.2:* the custody KIND on the
capability plane — `fauna_client_capabilities::custody_grants` as the ONE
owner of the scope vocabulary's two mappings (`CustodyScopeSet` ↔
nest-blob `ScopeTuple`s under `ScopeTuple::CLASS_CUSTODY`, and ↔ log
`GrantEventScope`s with explicit scope strings riding `tier` — the frozen
event type's rider-field discipline); the keyless `derive_scope_payload`
arm; the record-then-deposit mint door (`custody_mint_blob` over the
`UndepositedGrant` type); and the succession sweep's custody arm (before
the bridge-roster gate — a keyless grant has no wrap to PQ-downgrade, so
the log's ceremony-pinned holder IS the re-mint target; the re-signed
WITNESS stays the ceremony's re-offer leg, and a re-minted row admits
nothing until it delivers). The unmodified nest capability handlers
accept + revoke the custody row (tier_3-proven over the real doors —
zero nest-side changes, the spam-model keyless precedent held).
*Built — W8.3:* the two custody registry kinds are REGISTERED —
`fauna.state.custodies-held` (custodian-side; witness + owner pinned
NodeIds/candidates + nest URL + the accepted `retained_bytes_cap`) and
`fauna.state.custodian-endpoints` (owner-side; the accept-bound custodian
key + candidates), both FleetOnly + whole-record-LWW + **tip-sealed**
(location data — the `device-endpoints` severance reasoning; the
tip-sealed pin test asserts the three-kind roster), value shapes in
`fauna_core::{custodies_held,custodian_endpoints}` (tolerant decode both
skew directions; entry key = grant id hex,
`custody_grant::custody_entry_key`), the R14-gate refusal conformance-
pinned. *Built — W8.4 (2026-08-16):* the ceremony, end to end. The
carriage is `ChannelMessageBody::Custody` — verbatim canonical
`fauna_core::custody_ceremony::CustodyCeremonyMessage` bytes riding an
established conversation channel (the `GroupMetaMessage::Succession`
idiom: an effect arm, never a bubble, additive-legal because older
decoders skip the record; store-and-forward comes free). The payloads
(`CustodyOffer` / `CustodyAccept` / `CustodyDeliver`) are sender-bound
signed envelopes — unlike the Succession claim, a ceremony step IS its
author's act, so verification binds signer to the MLS-authenticated
sender and an offer's addressee to the reading actor. Durable state is
the account plane's `fauna.state.custody-ceremony` kind, one row per
ceremony side-record (cut plane-only 2026-09-30 —
[`config-dissolution.md`](config-dissolution.md) owns the kind; it was
`UserConfig.custody` before) (record-then-act: every consumed payload is
captured before any action; monotone progress marks merged by OR across
devices, a fresher receipt, deliver or mint carrying its own; expiry
decay computed from state, never a timer); the machine +
idempotent owed-action driver live in
`fauna_client_capabilities::custody_ceremony` (the mint-on-accept runs
the interactive door in the record-then-deposit order; the effective set
is the intersection under the carve-out predicate), the conversations
seam is `CustodyCeremonySink`/`send_custody_payload`
(`fauna-conversations`, sink glue in `fauna-client-conversations`), and
both registry rows write through the REAL R14 writer door
(`fauna_sync_engine::custody_rows`, typed `AccountStoreHandle` puts).
The admit exchange gained its additive per-session `endpoints`
re-exchange slot (absent = byte-identical old shape; W8.5 left it
unfilled, and *Built — the admit endpoints re-exchange* below is what
fills and consumes it). Proven tier_3 over the real nest
(`conformance_custody_ceremony_client.rs`): offer→accept→mint→deliver
through the production stack, the keyless nest row, the custody-shaped
Mint event on the owner's grant log, the owner's second device converging on the
custodian-endpoints row via the fleet walk under a real generation (the
W8.3-deferred proof), the held witness verifying for the accepted
device, budget carried, both drives settling to zero.
*Built — W8.5 (2026-08-16):* the custodian
runtime leg — hold, serve, re-serve. `PeerSyncServer` carries the
served-accounts registry (admission peeks the witness's CLAIMED account
and verifies against exactly the claim; custody grants admit only against
the granting account's own fleet — no custody-of-custody; every serve
routes its store by the verdict's account, and each served account
answers the admit reply with ITS OWN witness — a custodian answers an
owner-fleet dialer with THE CUSTODY GRANT, the "sides' kinds may differ"
sentence made concrete). The custody-revocation view is live on BOTH
sides — the serve predicate and the dialer's reply evaluation share one
pump-refreshed snapshot derived from the account's grant-event log (as
built, the device-local `__config` replica's, which both principals could
open with the `BackupKey`; since the rail's 2026-10-02 retirement the
`fauna.state.succession-ledger` event rows read off the runtime's own store
— [`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *The
kinds*), and the refresh cadence IS the ratified honest bound. `custody_pull` is the schedule-free relay pull
(verbatim `record_relay_row`, a durable META cursor — the frontier law is
journal-bound and a custodian never journals, its held-set IS the relay
plane); the custodied store opens keyless under
`StoreRoot::store_dir(owner_hex)` with the machine's device principal as
its never-authoring writer tag. The pump gained the custody serve-refresh
+ custodian dial pass; the owner-side dial pass gained
custodian-endpoints targets through the ONE shared dial-target
composition (PT-4 + LAN hygiene identical for every endpoints source).
Proven tier_3 (`custody_convergence.rs`, three pump-driven runtimes, no
nest anywhere): A seals → C (keyless, another account) pulls under the
real witness → B converges through C alone; C's store holds sealed forms
only (structurally asserted); the carve-out refused a conv-scope
changes.list ON THE WIRE while state served; the revoke written to the
owner replica severed C at the next admission evaluation. **Recorded
residuals:** the `Account` form's content-scope enumeration (a keyless
custodian cannot yet NAME the owner's content scopes — the
content-coordinate relay tranche owns the mechanism; explicit-list
grants pull their content scopes today) and the custody QUIC transport
variant (the leg's QUIC realism is proven by the sibling assembly test).
The admit `endpoints` re-exchange slot this entry left unfilled is
BUILT — *Built — the admit endpoints re-exchange* below.
*Built — W8.6 (2026-08-16):* the nest custody door — and with it T13's
two-stores rule enforced at BOTH stores. `fauna.auth.custody_handshake`
(pre-identity; the device-handshake PoP shape + the witness inline)
mints a bearer whose actor IS the custodian key; there is **no custody
session registry** — `CallerClass::Custodian` derives per dispatch from
the live custody-class capability rows, reaches exactly
`fauna.sync.changes.list`, **`fauna.segments.list` — and, since stage (c)
of the custodian-nest runtime (2026-08-17), `fauna.custody.receipt.deposit`:
the class's ONE write door, item 6's ratified receipt arm, whose handler
verifies the receipt signature against the live capability row's holder
key BEFORE staging and writes only the owner's latest-per-grant
receipt-staging buffer, never the owner's store — those three kinds and no
others** (pinned by
`bridge_method_allowlist::tests::the_custodian_class_reaches_exactly_its_declared_kinds`,
which fails on a widening rather than letting one land silently; note that
`fauna.segments.compact` is deliberately NOT among them, because custody's
data reach is read-only and compact mutates the owner's store), and the feed's custody
arm (the additive
`of_owner` addressing field) re-derives the verdict from the row's own
scope tuples on EVERY request, so `fauna.capabilities.revoke` (row
delete) severs a LIVE session at its very next dispatch — stateless, and
stricter than the ratified next-evaluation bound. One scope-admission
vocabulary everywhere: `AdmittedScopes` lifted to
`fauna_protocol::scope` (the peer seam re-exports),
`custody_scope_set_from_tuples` beside `ScopeTuple`'s custody consts
(mint-side round-trip pinned). The client leg is
`mint_bearer_over_custody_handshake` / `custody_nest_client` — the
device-principal client's custody twin; `custody_pull` addresses the
owner via `of_owner` on the nest leg. Proven tier_3 over the real nest
(`conformance_custody_nest_door_client.rs`): a custodian with NO account
credential handshakes and pulls the owner's sealed plane; revoke refuses
the SAME live session's next request and the next handshake; a PoP by
the wrong key refuses (mutation arm); an explicit-list grant serves
exactly its named scopes at the door.
**Recorded residuals (W8-wide):** the `Account` form's content-scope
enumeration (both legs — **RULED 2026-08-16**, § The custody grant +
ceremony → *Coverage enumeration*: the covered content set is a pure
function of the witness's own `owner` field, so the build is the
custodian-side derivation in `pullable_scopes` plus the content walks it
feeds; the earlier "the content-coordinate relay tranche owns the
mechanism" note was stale — that tranche is built, *Built — W3 the peer
content-coordinate relay*), content scopes at the nest door (a nest-area
build: **the record-cid arm LANDED 2026-08-16** — `fauna.sync.changes.list`'s
class-1 arm now honours `of_owner`, so an `Account`-form custodian walks the
owner's own-actor content scopes at the door and an explicit-list one walks
exactly its named scopes. Authorization is the same live-row re-check the
class-2 arm uses, lifted to one shared helper so the two arms cannot drift
(`sync_handlers::custody_admits_scope`), **plus the account binding that
`AdmittedScopes` structurally cannot make**: the enum answers the scope half
only, and `content:mail:<stranger>` shape-checks and is not co-authored, so the
content arm additionally requires `scope_id == owner` — the ruling's "pure
function of the row's owner" in code, and without it any `Account`-form row
would have served a third party's plane. Coverage uses the verifier's
**blocklist** predicate (`is_co_authored_scope`), not the custodian's
`OWN_ACTOR_KINDS` allowlist, per the ruling's *"the verifier's predicate stays
authoritative"* — an older custodian pulls less, honestly. A `conv` scope under
an `Account` row refuses loudly, from that same shared predicate; a dedicated
arm for it was written, found unreachable by mutation grade, and deleted (the
fn's doc records why, so it is not re-added). Proven tier_3 by
`conformance_custody_nest_door_client::the_custody_door_serves_the_owners_content_walk_and_refuses_every_wider_ask`
(mutation grade 3/3 exact). **The blob/block half LANDED 2026-08-16**, closing
the residual: the walk yields record-CID *coordinates* only (`entry: None`),
and the bytes they name live in the nest's **segment** store, whose two doors —
`fauna.segments.list` and `GET /api/v1/segments/{kind}/{actor}/{id}[/meta]` —
were owner-only, so a custodian could see what it held and never fetch it. Both
now admit a custodian, through the *same* predicate the feed arms use: the
four-door verdict moved to its own module (`custody_admission`) at the moment
the callers went from two to four, and the bulk plane's `(kind, actor)`
addressing is translated into the content scope `content:<kind>:<actor_hex>`
rather than judged separately. That translation is itself a security property:
the feed takes `scope` and `of_owner` as independent fields that must be
checked against each other, whereas the bulk plane has ONE actor field playing
both roles, so `scope_id == owner` holds by construction and a request for a
third party's bytes is **unrepresentable** — naming a stranger asks for a grant
over the stranger, which the custodian does not hold. Both refusals are
byte-identical to the ones an unrelated stranger gets (`not_owner` /
`403 non-owner`), so neither door is an oracle for the shape of someone else's
grants; and `fauna.segments.compact` deliberately did NOT open, because it
mutates the owner's store and custody is a read capability. **The client half
needed no new mechanism** — `NestBootstrapSource` already addresses the owner
through its `ScopeBinding`, and `AccountStore::bootstrap_scope_segments` is the
bulk entry point — which is why this slice is nest-side only. Proven tier_3 by
`conformance_custody_nest_door_client::the_custody_door_serves_the_owners_record_bytes_and_refuses_every_wider_ask`
(mutation grade 3/3 exact). Two honest limits, neither this slice's to close (the second one
**CLOSED 2026-08-17** by the record-identity cutover legs: every kind now
files under `Cid::of_dag_cbor(envelope_bytes)`, so the standing limit was
the *serve* gate alone — closed per-kind through 2026-08-18, when the last
kind (`conv`) joined; every adoptable kind now reaches the segment plane
(current per-kind status: [`message-segment-store.md`](message-segment-store.md)
§ *Which kinds the two planes serve*)), and the pump-step composition
of the custodian's NEST leg — **BUILT 2026-08-16, see *Built — the
custodian's nest leg in the pump* below**, which discharges this residual
and leaves the two that entry names: the leg's *cadence*, and the
*segment* fetch the blob/block half immediately above has just made
possible but which the leg does not yet compose. The admit `endpoints`
re-exchange slot is no longer among these residuals. The custody QUIC
transport twin is **BUILT 2026-08-16**
(`libs/fauna-iroh/tests/custody_admission_over_quic.rs`): the third
admission arm admits an owner-signed grant and refuses a
stranger-signed and an expired one over a real QUIC handshake against
the runtime-assembled listener — closing the residual that the arm had
only ever been exercised over `MemTransport`.
*Built — W8.7 (2026-08-16):* **T15's custody policy** — the budget the
acceptance names is now read, metered and enforced. `retained_bytes_cap`
had reached the custodian's `custodies-held` row at the ceremony and then
nothing consumed it; bytes accumulated unbounded. Now
`fauna_core::custody_policy` (pure, wasm-safe — the runtime that enforces
and the T16 facet that renders must not drift into two answers to "how
full am I") carries the per-scope-family meter and
`plan_custody_eviction`, largest evictable pool first (T15 leaves the
ordering heuristic to the build; within a family the store walks oldest
`writer_seq` first). **Three budget states, not two** — `Ok` /
`OverBudget` / `AtFloor`: the last is the honest answer when the
always-present floor alone exceeds the cap, and it reports the overage
(`unreclaimable`) rather than eating the floor to hit a number.
**Eviction is payload-only by construction:** `relay_evict_payload` is
`UPDATE relay_rows SET entry = NULL`, so the coordinate tuple that
frontier accounting and the served shape are made of survives, tombstone
rows keep their envelopes (T15 names them floor), and the pull's existing
entry-less branch handles the result with no new code path — dehydration,
never deletion. Content-scope bytes ride the existing R10 hydration axis
(`AccountStore::dehydrate`), not a second eviction path — **except the
segment pairs a custodian adopts, which `dehydrate` refuses (an immutable
file is not per-block reclaimable)**: until 2026-09-29 the meter was the
relay plane alone, so from the segment adoption onward those bytes
overran the cap unseen by the plan, the brake and the host's `held_bytes`.
*Built — the segment plane joins the budget (2026-09-29):* `custody_meter` adds one `segment` family per scope (`.dat`
evictable, `.meta` floor), eviction drops whole `.dat` files oldest first
(`StoreBackend::segment_evict_dat`, all three backends), and the pull
adopts only within the cap's headroom, which also bounds each download
(`account-replica-posture.md` § Custody policy owns the rules). The pass runs
per custody in the dial pass regardless of dial outcome, since a custody
that pulled nothing may still be over cap from last pass — **and, since
the nest leg landed, regardless of whether a peer transport is bound at
all, or the row names any owner device**: the budget is not a peer-plane
concern, and a nest-anchored custody reachable only through its owner's
nest must be metered exactly like a dialed one. Keyless
throughout: byte counts and cleartext coordinates only, and `floor_ops`
is caller-supplied so the store layer keeps not interpreting `op`. One
build-time refinement worth its own sentence: **a cap of 0 is read as a
MISSING cap, not "evict everything"** — the row's field is
`#[serde(default)]`, so a pre-carriage row decodes as 0 and a literal
reading would wipe a custody's whole payload on a missing field; it falls
back to the ceremony default, test-pinned. *Built — the T15 INGEST brake
(closed 2026-08-17):* the cap's missing ingest side, and the
fail-open metering arm's closure. Per held custody, in-memory, written
only by the (unconditional) budget arm and read by BOTH pull legs
(`fauna_sync_engine::custody_leg::IngestBrake`): a custody whose last
budget pass reported `AtFloor` pulls nothing until a pass meters
otherwise — the budget arm always running is exactly how the brake
releases (cap raised, eviction possible) — and a metering failure is
weather once but closes the ingest side after three consecutive failures
(a Rust constant), because its likeliest cause — full disk, wedged store
— is precisely the condition the cap exists to bound. Holding and
SERVING are never braked: the brake bounds accumulation, not
availability. Both pass reports surface `braked` + `metering_failed`, so
repeated failure reaches the T16-visible surface rather than only a warn
line. Proven at the real passes: a tombstone-floor custody stops pulling
and resumes when the owner raises the cap; mutation witnessed red.
And *Built — W8.7 leg 2 arc
1:* the **A7 custody receipt** (`fauna_core::custody_receipt`) — the
signed, dated attestation, **derived from the metering pass rather than
assembled beside it** (`CustodyBudgetOutcome::receipt`), which is what
makes "eviction is always receipt-visible" structural: it carries
`evicted_bytes` + `unreclaimable_bytes` beside coverage, so a shrinking
custodian cannot read as merely small, and `is_degraded()` is A7's
honest-failure predicate in one place. Signed by the custodian's device
principal; verification checks both the signature and that the key is
*this* custody's custodian. The pass re-meters after eviction rather than
subtracting from its own stale snapshot — an attestation must not report
coverage nobody measured.
And *Built — W8.7 leg 2 arc 2:* the receipt's **carriage and owner-side
fold**. A receipt rides its own `ChannelMessageBody::CustodyReceipt`
body, deliberately **not** a fourth `CustodyCeremonyMessage` variant:
semantically the ceremony is a one-shot handshake whose strict decoder
must refuse to guess, while a receipt is periodic; mechanically, that
strictness lands differently at the two levels — an unknown *ceremony
payload* surfaces on an older host as a counted payload FAILURE, once per
receipt forever, whereas an unknown *body* is skipped by the shared poll
loop, which is the same additive-legality argument the `Custody` body
already makes (`version-compatibility.md` § Dim 2). An older owner
therefore simply never sees receipts and renders *no receipt yet* — a
state the T16 rows must render anyway. Owner-side, `ingest_receipt`
verifies against the custodian key **its own accept bound** (not anything
the receipt asserts, and not the MLS sender — the transport sender is the
host account while the signer is the bound device), refusing an unknown
grant, a wrong channel, an unaccepted ceremony and an unbound signer; it
is **monotone in `attested_at`**, so a replayed attestation cannot make
coverage look fresher or thinner than it is. The record-then-act split is
the ceremony's own: the sink records into the ceremony state (as built,
`__config` in one CAS update; since 2026-09-30 the
`fauna.state.custody-ceremony` rows — [`config-dissolution.md`](config-dissolution.md) § The `__config`
dissolution schedule → *The kinds*), `drive_ceremonies` folds it onto the `custodian-endpoints` row (one door,
because that row is whole-record LWW). `GrantedCustody` gained
`latest_receipt` + `latest_receipt_at` + `receipt_row_written`, and the
config merge treats the receipt as **freshest-wins, not a monotone OR**,
with the written-mark travelling with the receipt it describes — OR-ing
it would tell the next drive that a newer receipt had already reached the
row on the strength of an older one's write, and the row would render
stale coverage forever.
And *Built — W8.7 arc 2, the check-in cadence (2026-08-16):* the
custodian now **schedules itself**, in the ceremony machine's own
record-then-act shape. The pump's mint step
(`fauna_sync_engine::custody_leg::mint_due_receipts`, right after the
custodian dial pass so it attests that pass's post-eviction meter) mints
when the pure rule `fauna_core::custody_receipt::receipt_due` says a
receipt is owed — never attested; `CUSTODY_RECEIPT_INTERVAL_MICROS`
(24 h, a constant — nobody would configure an attestation rate) elapsed;
**this pass evicted payload** (T15's "never silently", promptly, not at
the next scheduled check-in); or the **degraded verdict flipped** in
either direction — and records the signed envelope on the `HeldCustody`
ceremony record (`receipt` + `receipt_posted` + `receipt_minted_at` +
`receipt_degraded`; config merge = freshest-mint-wins with the
posted-mark travelling with its mint, the owner-side rule's host twin).
`drive_ceremonies` grew the symmetric arm: an unposted receipt posts
verbatim through its own poster door
(`CustodyPayloadPoster::post_receipt` →
`ConversationsSession::send_custody_receipt`) and is marked. Two
consequences worth naming: **only the accept-bound device mints** (a
fleet sibling's receipt would be refused owner-side, and its meter
describes the wrong store — checked against the recorded accept before
any mint), and the split means an **app-dead agent keeps attesting**
while the owed post rides whichever conversations-capable session next
drives — the owner honestly renders *stale* meanwhile, which T18's aging
margins and the T16 three-state row both already expect. Tier_3:
`conformance_custody_ceremony_client.rs` — an over-budget custodian's
receipt reaches the owner over the real nest + real MLS channel and the
registry row reads degraded.
**Still unbuilt:** the receipt
*parameters* that gate dehydration (N-of-M, aging margins,
re-verification cadence) are **T18's**, unscheduled — W8.7 emits receipts
and deliberately does not build their consumer. Claims those implement
stay refutable until that build lands. (The custody UI itself — row 52
leg 3, T16 — is built on tui as of 2026-08-16/17: the three Devices-page
families, the five gestures, and the `custody-mint-*` offer initiation;
`ui/devices.md` § Implementation status today owns the per-app split.)

*Built — the custodian's nest leg in the pump (2026-08-16):* W8.6's door
had a client (`custody_nest_client` + `custody_pull { of_owner }`) that
**no production path ever built** — its only caller was the tier_3 door
proof, which is what the residual meant by "v1 is app-glue-driven". The
pump now runs it: `custody_leg::CustodyLegState::nest_pass` walks every
held custody whose row names an `owner_nest_url`, pulls each
account-state scope the witness covers and walks each content scope with
`ContentScopePlane::of_owner`, and keeps the custody session **cached
across passes**, dropping it on any error so the next pass re-handshakes
— which is how a revoked custody stops without this leg carrying a
revocation view of its own (the door re-derives per dispatch). As landed
it pulled **coordinates only, no record bytes** — the segment composition
below closed that. The
leg is deliberately **not** gated on the peer transport — an
always-on anchor whose whole value is the owner's devices being asleep,
unreachable, or not yet named by the row would be worthless behind a
peer-plane gate — and it is sequenced BEFORE the dial pass so the bytes
it lands meet the same pass's T15 budget. Which exposed a hole the nest
leg made acute and this build closed: **the budget was reachable only
through a peer dial** (inside both `if let Some(transport)` and a
no-targets `continue`), so a custody with no dialable device grew
unmetered; `dial_pass` now takes an optional transport, the pump calls it
unconditionally, and the dial half alone is what the transport gates.
`CustodyLegState` also carries the device **signing** key now rather than
its public bytes, deriving the id from it — the store's writer tag and the
key the owner's nest verifies in the handshake PoP can no longer drift.
Proven tier_3 over the real nest
(`conformance_custody_nest_door_client::the_pump_pulls_a_custodied_owner_from_the_owners_nest_with_no_peer_leg`):
a custodian's real `AccountStoreRuntime` converges on the owner's sealed
plane with `peer_transport: None` **and** an empty `owner_devices`, so
neither the dial pass nor any peer leg can be what moved the data, and the
custodied store still journals nothing. **One residual remains — the
CADENCE:** the pump is nudge-driven off the *custodian's own* account plus
an hourly backstop (and the scope nudge is walk-only, so it never runs
this leg at all), and a custody session gets no push from the owner's nest
(the custody class reaches the read doors and nothing else), so a
custodian's freshness on the owner's plane is bounded by its own unrelated
traffic. Recorded where the next W8 track will read it
.

*Built — the nest leg's segment composition (2026-08-17):* the leg above
landed the owner's coordinates; the bulk **segment** plane — opened to
`CallerClass::Custodian` the same day by the blob/block half — was the
nest-side answer to the peer leg's `pull_missing_blocks`, and the two were
never joined. They are now: `custody_leg::adopt_scope_segments` runs
**before** each content scope's coordinate walk (the bootstrap contract's
order), enumerating `fauna.segments.list` and fetching the segment pair
over a `SyncClient` built from the custody session's own `AuthClient`, so
one custody bearer serves both planes and neither outlives the other. The
sink is `AccountStore::bootstrap_scope_segments` → `adopt_segment` →
`segments::admit`, which verifies CARv2 framing, **re-hashes every block**
against the CID it is filed under, and binds the sidecar's actor **and
kind** to the scope it is filed under — and decrypts nothing, which is
precisely what lets a keyless custodian hold
the owner's sealed record bytes while remaining unable to read one. So a
nest-anchored custodian is now a **restore-from-custody**, not merely the
coordinates a re-presenting reconcile pulls *by*.

**The bound, since the 2026-08-17 cutover legs: EVERY kind — `post` +
`mail` + `calendar` + `card` + `conv`.** Adoption is gated on
`fauna_account_store::segments::ADOPTABLE_KINDS`, which is a statement
about `admit`'s re-hash and *not* about what a nest serves: a kind is
adoptable exactly when its record id IS the hash of its bytes. `post`
qualifies from birth (`Cid::of_dag_cbor(body)`); **`mail`, `calendar`,
`card` and `conv` qualify since their record-identity cutover legs** —
each kind's append derives `Cid::of_dag_cbor(envelope_bytes)` and the boot
reconcile tombstones every record filed under the retired upstream-minted
id (the user-approved alpha reset), so an admitted custodian now adopts
the bulk of what a user would want custodied; a *pre-cutover source's*
segments simply fail `admit`'s re-hash — inert, never dangerous. For
**four of the five kinds adoptable and reachable now coincide**: the
serve-plane wiring (2026-08-17, row 69) added `calendar`/`card` to the
wire kind gate `segment_manager_for_kind` beside `mail`/`post`, so those
four are enumerable and fetchable by an admitted custodian —
tier_3-proven per kind by the adoption arms in
`conformance_custody_nest_door_client.rs`. **`conv`'s divergence CLOSED
2026-08-18 (ruled and built the same day)**: conv joined the wire
gate under the member-mint contract — no channel-enumeration door (a
custodian names only what its explicit-list grant covers), the member-mint
rule at the custody door (§ The custody grant + ceremony →
*Shared-audience carve-out*), the home-nest coverage bound — so **all five
adoptable kinds are enumerable and fetchable by an admitted custodian**,
tier_3-proven per kind in `conformance_custody_nest_door_client.rs`. (The
two lists still answer different questions, and `ADOPTABLE_KINDS`' own doc
comment owns why they are kept apart.) Owner:
[`message-segment-store.md`](message-segment-store.md) § *Record identity
per kind* (the ruling, the reset that replaced the
expand→migrate→contract path, and what stands per kind). The bulk half is absorbing
by design — it returns a count, never an error — because the coordinate
walk that follows is the leg's authoritative verdict on the session; a
revoked custody refuses both halves, and one error path beats two racing
to drop the session. Proven tier_3 by
`conformance_custody_nest_door_client::the_pump_pulls_the_owners_record_bytes_from_the_owners_nest_with_no_peer_leg`,
the sibling of the coordinate case above and staged identically
(`peer_transport: None`, empty `owner_devices`): it asserts the record's
bytes are readable out of the custodied store AND that they arrived by
segment adoption on the owner's post scope, so neither a peer leg nor some
other route can be what moved them.
*Built — the admit endpoints re-exchange (2026-08-16):* T13 ceremony step
4, the piece W8.4 carried and W8.5 left unfilled. Both admit halves now
carry the sender's live `DeviceEndpoints` — assembled by the ONE mapping
every consumer shares (`device_endpoints_writer::endpoints_of`, the same
value the fleet-only plane entry publishes), fed to the listener as
pump-fed truth (`PeerSyncServer::set_own_endpoints`, the `set_custodied`
idiom) so the admit path itself does no I/O. Consumption closes the
freshness hole the ceremony-time seed left, and does it **from every
session, in both directions**: whatever a pass observed about a peer —
carried IN by a dialer (drained from the listener) or answered OUT to
our own dials — lands in whichever registry row already names that node,
`custodian-endpoints` on the owner side and `custodies-held`'s
`owner_devices` on the custodian side, through the R14 door on the
pump's own path (`custody_rows::{put_custodian_endpoints,
put_custodies_held}` — the former's "per-session candidate refreshes"
convergence case, finally driven). Both directions matter because a
machine whose every stored address has gone stale cannot dial out at
all: an inbound session is the only teacher it has left, so a
reply-only refresh would strand exactly the box that needs one. **The security rule is
that the carried `node_id` is advisory and never identity**
(`fauna_peer_sync::bind_carried_endpoints`): a value naming anyone but
the channel-proven peer is refused outright — the precedent
`sibling_dial_targets` already sets for a plane row whose `node_id`
disagrees with its key — so a re-exchange can move only WHERE a known
peer is reachable, never WHO it is; a custodian row's identity stays a
ceremony act. A device that did not answer keeps its previous entry: a
dial failure never erases the only address held for it, and the slot is
carried only when there are candidates to carry — a node_id-only value
would tell a peer nothing the channel had not already proven. Absent
stays byte-identical (the W8.4 additive pin is untouched and still
green). **Both registry kinds are fleet-only and `GenerationTip`-sealed,
so the write-back is generation-gated like every other put on this
plane:** a device with no resolvable tip leaves the refresh *pending*
rather than sealing it (correctly — it is the same door that refuses its
`device-endpoints` publish), and the candidates it learned reach the row
once a tip resolves. Proven tier_3 in `custody_convergence.rs`: owner
device B, holding a `custodian-endpoints` row whose candidates are the
ceremony's and structurally unable to read the custodian's fleet-only
`device-endpoints` kind, learns C's live bound address over the admit
reply alone, bound to the channel-proven key. The write-back rule itself
is pinned tier_1 in `custody_rows::tests` (both sides).

*Ruled — the nest-custodian identity fact (2026-08-17; § Replica posture
→ The custody grant + ceremony, the device-or-nest bullet):* the accept
can name the host's NEST as the bound custodian — `custodian_nest_url`
on the accept, `custodian_key` = the host's pinned nest actor identity
(the offer-side advertisement flag left 2026-10-04 — the bullet's item 3).
Build staging is the bullet's item 6: stage (a) — the wire fields, the
registry-row URL carry, the owner-side
fold split, and the tui Nests-page `nest-trust-custody-*` render — is
**BUILT with the ruling (2026-08-17, tui the lead app; the byte-stability,
nest-form, gate, fold-split, and render tests pin it)**; stage (b) is
**BUILT (2026-08-17)** — hosting registration: the
`fauna.custody.hosting.{register,list}` pair (`fauna_protocol::custody`),
the keyless `custody_hosting` nest table (Burn-on-succession, plaintext
rest — its migration doc owns the reasoning), the User-class doors whose
register refuses any row the pump could never use (witness must admit
THIS nest's identity, owner and grant id must be the witness's own,
URL policy at ingest per the counterparty-URL ruling), and the host-side
`CustodyHostingClient` seam, pinned by
`conformance_custody_hosting_client.rs` (tier_3: deposit → rewrite →
read-back, five refusal arms, host scoping, restart survival); and the
nest pull PUMP: `fauna_nest::custody_hosting_worker` — the
`NestBackupWorker`-mold interval loop driving the client custody leg's
own pull core, now shared ungated (`custody_leg::{NestLegSession,
pull_from_owner_nest, pullable_scopes}` moved out of `account-runtime`;
one core, two custodian kinds), authenticating over the existing custody
handshake with the NEST's deployment identity as the custodian key, URL
policy re-checked every pass, T15 budget + metering write-back, and a
`test-hooks` run-now poke — pinned by
`conformance_custody_hosting_pump_client.rs` (tier_3, two real nests:
the owner's sealed row reaches the custodian nest's keyless store with
no host device running; revoke severs at the next handshake).

Stage (b)'s **bounds are BUILT (2026-08-17)**, closing the hosting-bounds
ruling's first and fourth pieces: `MAX_RETAINED_BYTES_CAP` (= the accept-time default) and
`MAX_CUSTODY_HOSTING_ROWS_PER_HOST` in `fauna_core::custody_ceremony`,
with the decision itself shared beside them as `bound_hosting_deposit` — so
the host-app surface the admin piece brings can render "at the cap" /
"budget clamped" without a second copy of the rule — enforced at the
register door (rows refuse, bytes clamp, a rewrite of a held row still
admitted at the cap) and re-clamped independently in the
pump; and the nest-scoped dial policy —
`fauna_core::counterparty_url::DialScope`, with the plaintext-loopback
carve-out withdrawn on a public deployment as read by
`AppState::is_public_deployment`. Pinned by the scope matrix and
alias-agreement tests in `counterparty_url.rs`, the `hosting_bounds_tests`
module beside `bound_hosting_deposit`, the count probe in
`db/custody_hosting.rs`, and two tier_3 arms in
`conformance_custody_hosting_client.rs` (PROBE-379-A as a standing test;
a public rig refusing a loopback URL while still admitting `https`).
The **tier-bounded held-bytes accounting** ruled above is BUILT
(2026-08-17): `fauna_core::custody_ceremony::effective_hosting_cap` (row
number → per-row ceiling → the host's remaining tier headroom, a pure fn
beside `bound_hosting_deposit`), `CacheDb::sum_custody_hosting_held` (the
derived per-host figure), the door's over-bound refusal for a NEW row
(rewrite exempt; red-verified tier_3 arm in
`conformance_custody_hosting_client.rs`), and the pump passing the squeeze
from its own enumeration with `meter_and_evict` taking `Option<u64>` so a
squeeze to `Some(0)` holds the floor instead of inheriting the default cap.
The **admin surface** (piece 3) is BUILT nest-side (2026-08-17):
`fauna.admin.custody_hosting.{list,remove}` — Admin-class, the nest-wide
host-attributed list the per-caller door deliberately is not, and a remove
that drops the row and, only with the `(host, owner)` pair's LAST row, the
custodied store beneath it (the pair's grants share one store dir). No
credit-back anywhere — the held-byte counter is derived. Every hosting-bounds
nest bound is now enforced AND recoverable from an admin session. **The tui
APP leg landed 2026-08-18 (tui as lead app)**: the Admin-class client
seam `fauna_client_capabilities::custody_hosting::AdminHostingClient` (the
neighbour of its User-class twin, not a resident of `fauna-client-admin` —
one wire module, one teardown rule), the shared projection
`view_model::admin_hosting_rows` (heaviest-hold-first, tie-broken on
`(host, owner, grant)` so the rendering never comes to depend on the
registry query's `ORDER BY`), and the `admin-custody-hosting` page — a
contextual detail page on the admin nav rail rendering host / owner / dialled
URL / budget / metered hold / stop mark / three-state receipt freshness, with
an inline arm-confirm remove addressing the `(host, grant)` PAIR rather than
a painted index. 14 element IDs, rule-A approved 2026-08-18. The six-app
trickle-down is tracked as a batched row. The **reclaim is BUILT (2026-08-17)** — the § Replica
posture *Reclaim* bullet as code: the host's own
`fauna.custody.hosting.remove` (User-class; one teardown shared with the
admin door — the store falls only with the pair's last row), the pump's
expired-store GC (`hosting_store_reclaimable` +
`HOSTING_EXPIRED_STORE_GC_GRACE_SECS` beside the other hosting bounds in
`fauna_core::custody_ceremony`; expired rows are never dialed, reclaimed
pairs' metered figures zero, rows stay as the visible record), the
host-side `CustodyHostingClient::remove` seam, and the tui stop control's
stopped-state label carrying the "bytes remain until removed" half.
Pinned by the pump's four-verdict GC test and two door conformance arms
(last-row store fall over the production client; host scoping). **Its APP
half landed 2026-08-18 alongside the admin surface** (widened
batch): `custody-held-remove-button` on the Devices custody-held card, wired
as `fauna_client_custody::CustodyAct::Remove` — a NEST-form custody comes
down through the hosting door, a DEVICE-form one by zeroing and stopping this
account's own `custodies-held` row, and only on the teardown's success does
the ceremony record take the monotone `HeldCustody::removed` mark (the
`declined` idiom: the record stays so a re-ingest cannot resurrect a torn-down
custody, while the card stops rendering it). Marking first would let a failed
teardown hide a still-pulling custody from the surface that could retry it.

**The deliver door holds the host's consent (landed 2026-09-29)** — `account-replica-posture.md` § Replica posture → *A deliver never re-opens consent* owns the rule. `fauna_client_capabilities::custody_ceremony::ingest_payload` refuses a deliver on a `declined`/`removed` record, and one whose witness names scopes outside the accept's effective set, reaches past `now + offer.duration_secs + CUSTODY_WITNESS_MINT_SKEW_SECS` (a hard-coded day), or (superseding) outlives the held witness. The drive's arm (6) skips terminal records and takes the row's cap/stop from `runtime_knobs` over the new `HeldCustody::host_knobs`. `fauna_client_custody`'s Stop/budget act writes those knobs to the record (`record_host_knobs`) BEFORE the runtime row. The config merge now ORs `removed`, which it had silently dropped, and merges `host_knobs` freshest-wins.

**The `admin-custody-hosting` page's linux leg landed 2026-08-18** — same shape as tui's, eager-fetched alongside every other admin data source rather than on a nav-edge hydrate (linux's own convention). **`custody-held-remove-button` did NOT land with it**, deliberately: `apps/fauna-linux/src/views/devices_folders/custody.rs`'s own module doc rules the held-for-others card (the button's home) store-gated until the W3 account store reaches an app other than tui — building a remove-only card would be a narrower shape than the ratified piece 3, not an addition to an existing one. `remove_held_custody` itself needs no store; the card it would sit on is what is missing.

**The web leg landed 2026-08-19**. Unlike tui/linux — native Rust, calling `AdminHostingClient` directly — web crosses through wasm, and no export existed for it: this leg added `libs/fauna-wasm/src/rpc.rs`'s `adminHostingList`/`adminHostingRemove` (folding through the same shared `admin_hosting_rows` projection, `receipt_state` degraded to a `"fresh"`/`"stale"`/`"no_receipt_yet"` string at the boundary since the enum itself carries no `Serialize`). The same gap exists for the three UniFFI apps (android/windows/apple): this session also landed `libs/fauna-ffi/src/admin.rs`'s `custody`-feature-gated `FfiAdminClient::custody_hosting_{list,remove}` (an `FfiAdminHostingRow`/`FfiReceiptState`/`FfiAdminHostingRemoveReply` mirror) so their own paint-only legs no longer need to discover and build this boundary themselves. `custody-held-remove-button` did NOT land on web either, for the same store-gated reason as linux.

**The android leg landed 2026-08-19** (fourth app, same session as web) — the first consumer of the FFI boundary above. Caught two real bugs before they reached android/windows/apple: (1) `FfiAdminHostingRow`'s byte-count fields were first declared `u64`, breaking this file's own established FFI convention (`i64` everywhere else, to dodge Kotlin `ULong`/Swift `UInt64` friction) — fixed to `i64` with a lossless `as i64` cast at the mirror boundary; (2) the first Kotlin draft called `ValueFormat.byteSize`/`com.fauna.ffi.shortId` directly inside the Robolectric-tested `Content` composable, which throws `NoClassDefFoundError` under a plain JVM test host (no native library loaded) — fixed by injecting both as plain functions from the stateful `Screen`, the same pattern `AdminBridgesPendingScreen`'s injected `displayName` already established. `custody-held-remove-button` did NOT land on android either, same store-gated reason as linux/web.

**The apple leg (macOS + iOS) landed 2026-08-26** — one shared FaunaKit `AdminCustodyHostingVM`/`AdminCustodyHostingView` consumed by both targets, over the `FfiAdminClient.custodyHostingList`/`custodyHostingRemove` boundary the web leg built. A new `AdminPage.custodyHosting` case (contextual detail page, same shape as `bridgesPending`). `custody-held-remove-button` did NOT land with it either — re-verified rather than assumed from the linux/web/android precedent: a `grep` of apple's app source outside generated FFI/i18n found zero `HeldCustody`/`custody_held` references, confirming no apple UI reads the W3 account store's held-for-others surface yet, so the same store-gated exclusion applies.

**The windows leg landed 2026-09-09** — `AdminCustodyHostingViewModel`/`AdminCustodyHostingPage` over the same `FfiAdminClient.CustodyHostingList`/`CustodyHostingRemove` boundary the web leg built (`nest.Admin().CustodyHostingList()`/`.CustodyHostingRemove()`), a new contextual rail entry (`AdminNavigation.CustodyHosting`, between BridgesPending and Logs). `custody-held-remove-button` did NOT land here either, same store-gated reason as the other six — re-verified: zero `HeldCustody`/`custody_held` references in windows app source outside generated FFI/i18n. **All 7 apps now own this leg.**

Stage
**(c) is BUILT (2026-08-17)** — the receipt deposit arm:
`fauna.custody.receipt.{deposit,list}` (`fauna_protocol::custody`), the
`custody_receipts_staged` table (latest-per-grant, monotone in
`attested_at`, verbatim envelopes), the Custodian-class deposit door
(the class's ONE write door — the W8.6 entry above owns that census;
capability row re-derived per request, signature verified against the
row's holder key AT the door), the pump's receipt leg (`receipt_due`
cadence under the nest identity, deposited over the same custody
bearer, bookkeeping advancing only on an acked deposit so failures
redrive), and the owner-side fold — `ingest_receipt_from_nest` (the
channel rule swapped for the nest-form-accept rule, all else shared
with the channel path) fetched by `fauna_client_custody::spawn_drive`
before each ceremony drive pass. Pinned tier_1 (both carriage arms +
staging monotonicity) and tier_3 (the pump test's receipt arms: deposit
→ staged → verifies under the NEST identity; a forged deposit refused
at the door). **The HOST-SIDE CHOICE ships (2026-08-17, tui the lead
app) — item 6 is COMPLETE**: the consent card's
`custody-offer-target-select` (ID user-approved 2026-08-17) renders
only for an offer with a reachable owner nest URL AND a
pinned nest identity in hand — absent otherwise, never disabled;
`build_accept_nest` enforces its floors again at the gesture;
`CustodyAct::Accept{on_nest}` binds the PINNED identity; the ceremony
drive's arm (6) routes a nest-form accept to the hosting deposit
(never a `custodies-held` fleet row — no host device serves or
pulls), and the owner's facet refresh fetches staged receipts itself
(`ingest_nest_receipts` rides `load_custody_facet`, the reconcile
sweep's cadence precedent). Budget/stop rewrites route to whichever
runtime row the accept's form owns. Covered by
`test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
(tier_3, both tui UIs: select → nest accept → hosting deposit → poked
nest pump pull + receipt deposit → the owner's Nests page reads
fresh, with the Devices family excluding the row). The six other
apps' consent cards render no target select yet (trickle-down, with
the rest of the custody facet).

**As of 2026-08-13, the W1 slice-1 store floor, the W4 per-kind offline
classification + the outbox itself (phase 1: the store component + the
generic drain — *Built — W4 phase 1* below), W2.0–W2.4 (the class-2 entry primitives, the block
plane, segment adoption + bootstrap, the nest-side generalized feed, and
the client walk + merge seam), and W2.5 (the R13 crypto amendments, the
preference-cluster registrations + CAS-blob bridge, the rails nudge, and
the seen-set + device-endpoint exemplars) are built — with the
content-scope feed walk (2026-08-11) the bootstrap contract is whole, and
W2.5's remaining debt is production *placement* (W3), not mechanism.
Everything else of the plane is target state.** R9–R12 (the symmetry pass) are unbuilt but for one leg
— no materialization tier, no nest hydration policy or dehydrated sets, no multi-actor backend; the
per-kind "needs a nest" reason IS built (`common.needs_nest`, `account-offline-mutation.md`); T17/T18
hold the first-build detail. **W2.6 — the peer leg — is BUILT
2026-08-12 against both design gates** (the *Built — W2.6* entry below owns
the detail): the admission seam is code (verdict-consuming core, the
`DeviceAuthorization` witness verifier beside the cert's own mechanics),
the allowlisted pull/serve engine exists (`libs/fauna-peer-sync`), and the
walk's obligation verdicts (rules 3, 5, 7, 8) have their named pieces
landed and pinned — with rule 5's store-safe witness *column* sequenced to
the first artifact that compiles the leg (none does at W2.6; the entry
states it). Grading is with the security review. **R13's crypto half is BUILT
2026-08-11 (W2.5 item 0), landed before any kind sealed a production
entry — so it is birth-shape for generation 0, not a migration.** The
envelope (`libs/fauna-core/src/account_entry_crypto.rs`) carries the
universal in-seal writer signature and verifies it on every open; the
schedule forks into KAT-pinned delegable and fleet-only branches with
grant minting reachable only from a delegable-only type
([`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 owns
the derivation shape and the precise built/not-built line); the audience
rung is a second frozen registry column beside the merge policy
(`fauna_protocol::merge_policy`), routed to its branch by `kind_keys`;
and the Secret-type conformance check exists as
`fauna_protocol::secret_free` — a compile-time pin per delegable payload
type, plus a test that fails if a delegable kind is registered without
one. **The kind→type binding is the COMPILER's too (closed
2026-08-17):** each delegable kind has a typed constant
(`merge_policy::records::*`, `RecordKind<T>`) whose payload-type parameter
the generic plane doors take their `T` *from*
(`fauna_sync_engine::preference_surfaces`) and whose parameter the pin
table's const bind block checks against the pin — so swapping a delegable
kind's payload type at a call site is unwritable, and swapping it at the
constant fails the bind (then the new type's exhaustive pin) to compile;
the `KIND_*` strings are derived (`.name`) so the spellings cannot drift.
The mutation was witnessed red at build. The first delegable kind is
registered — `fauna.state.moderation`
(latest-wins), the preference cluster's opener. **W2.5 item 1 is BUILT
2026-08-11:** the three siblings are registered beside it
(`fauna.state.sync-prefs` / `.personalization` / `.delegation`, each with
its Secret-free pin in the same edit) and the **CAS-blob bridge** exists —
`fauna_sync_engine::preference_bridge` (feature `preference-bridge`): the
pure decision core, the `bridge_tick` executor, and `put_preference` (the
plane-first dual write), with the shared anchor field
`UserConfig::plane_mirrors` and its stamp-ranked merge arm
(`format_user_config.rs`), proven blob↔plane over the real handlers by
`bins/fauna-nest/tests/conformance_preference_bridge.rs` (mechanism owner:
[`account-sync-plane.md`](account-sync-plane.md) § Substrate settlements → *The CAS-blob bridge*). **The bridge was DELETED 2026-10-01** (closure step (5) — [`config-dissolution.md`](config-dissolution.md) § Implementation status today): the module is `preference_put`, the plane write alone, and `plane_mirrors`, `bridge_tick` and that conformance file are gone. **W2.5 item 2 is BUILT
2026-08-12:** `fauna.state.seen-set` is registered — **delegable** (the
ruling: § The audience ladder → *The seen-set rung*), union CRDT, one
entry per referenced scope — with its value shape and join law in
`fauna_core::seen_set` (per-writer watermarks + itemized scope-feed
coordinates; elision inside the join, per the A4 realization in § The
replica boundary), its Secret-free pin in the same edit, and the merge arm
delegating to that join. It is the plane's first delegable CRDT kind, so
the conformance CRDT-merge tests now drive the real writer door
(`conformance_account_state_walk.rs` — item 0's decision (e), discharged),
including a feed-level proof that watermark compaction converges. **What
item 2 did not include — half-closed 2026-08-12:** the production writer is
now the **auto-in-set producer** (*Built — W3 the auto-in-set seen-set
producer*, below), which discharges the rung ruling's refutability; the T1
body-rendered *trigger* (the browse producer) stays gated on the first
coordinate-bearing browse surface (§ The replica boundary → T1's producer
decomposition), and T1's own ruling stays refutable until it builds. **W2.5 item 3
is BUILT 2026-08-12, closing the slice's registrations:**
`fauna.state.device-endpoints` is registered — **fleet-only** (the ruling:
§ The audience ladder → *The device-endpoints rung*), whole-record LWW,
one entry per device keyed by its writer id — with its value shape in
`fauna_core::device_endpoints` (NodeId + LAN/public addresses + relay URL
per T5; tolerant decode both skew directions, pinned) and the per-device
feed behavior + the R14 refusal proven in
`conformance_account_state_walk.rs`. Being fleet-only it is
production-unsealable until the generation schedule lands — deliberate
(removal severance for location data), and the reason **production peer
discovery sequences behind the generation schedule** while W2.6's proofs
stage rows door-lessly. **Item 1's
placement debt began discharging 2026-08-12 (W3 slices 1–2, the *Built — W3
slice 1* / *slice 2* entries below):** the client-side lifecycle is code
(`fauna_sync_engine::account_runtime`, per the ratified § The client-side
lifecycle) and **tui assembles it** — **all four** preference save paths now
route plane-first through the store when the runtime is up, and since
2026-09-28 so do linux's and the `fauna-ffi` seat's (the W3 trickle-down —
[`config-dissolution.md`](config-dissolution.md) § *The closure order*, step
(1)); and since 2026-09-30 so do web's, over the runtime it now hosts
(step (2); `config-dissolution.md` § Implementation status today). **What R13 still owes:** the
*authoring-chain* check above the signature — the signature proves the
stated writer signed these bytes, not that the account ever authorized
that device, which needs the device registry
([`../behavior/devices.md`](../behavior/devices.md) § Device-signed
authoring, W4's T7) — and per-rung sub-scopes. The `UserConfig` CAS blob's
dissolution was **scoped (2026-08-12)** and is complete: the rail retired
2026-10-02 ([`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule
owns the phases; → *The closure order*, step (6)). **E0 is DONE the same day**: the
pre-R13 whole-record registration is retired — `fauna.state.user-config` is
gone from `fauna_protocol::merge_policy` (the tombstone comment there owns
the story; the string is retired-never-reuse, pinned by
`the_retired_user_config_kind_is_not_registered`), and the conformance
test-vehicle role moved onto the seen-set (door-less/poisoned shapes) and
device-endpoints (the R14 gate refusal). What the schedule has built since —
the E1 cluster with its bridge, closure steps (1) and (4), E3's first act,
the kinds-table refresh, and E3's lead slice, `fauna.state.group-share-ceremony`
(both 2026-09-28) — its own status entry owns; the E3 gate
on the R14 generation schedule lifted 2026-08-13 (the *RESCHEDULED 2026-09-27*
entry, now in [`config-dissolution.md`](config-dissolution.md)). **The R14 generation schedule's BUILD DESIGN is
ratified 2026-08-13 (§ The generation machinery + `owner-key-material.md`
§ Path A-sibling-2 → *The schedule build design*; refutable until built) —
build steps 1–2 of 7 are CODE (same day):** the sealing-epoch registry column
(`fauna_core::crypto::SealingEpoch`, `Gen0`/`GenerationTip` — device-endpoints
was the sole tip kind at this build step; the tip set has since grown to seven
severance-needing kinds, pinned by
`merge_policy::the_tip_sealed_set_is_exactly_the_severance_needing_kinds`),
all five machinery kinds
registered fleet-only + Gen0
(`fauna.state.{device-set,generation-mint,generation-wrap,escrow-target,escrow-receipt}`,
`fauna_protocol::merge_policy`), the two lattice joins + value types
(`fauna_core::generation` — Removed/Shredded absorbing, byte-level winners,
laws red-verified incl. the add-wins-resurrection refusal), the
`state-fleet` scope constant + `home_scope_for_kind` rung→scope routing;
**and the form-v2 generation-sealed envelope** (step 2:
`fauna_core::account_entry_crypto` — `SEALED_ENTRY_V2` carries the 32-byte
cleartext generation id AAD-bound between version byte and nonce,
`seal_entry_v2`/`peek_generation_id`, `open_entry` dispatches both forms;
id-tamper and wrong-generation-keys refusals pinned by test; **no
production writer emits v2** until the mint step).
**Step 3 is CODE (2026-08-13):** the device-set reader —
`fauna_core::generation::FleetView`, the verified fleet view whose
contract the machinery §'s device-set bullet now states (cert-verified
membership incl. succession-crossing, unconditional exclusion, advisory
attribution); the plane-level remove-wins conformance legs incl. the
red-verified add-wins-resurrection check across the real feed
(`conformance_account_state_walk.rs`); and the walk's row-content
hardening — `MergeError::is_row_content` skips (never aborts) hostile or
malformed permanent rows, and `validate_adoptable` refuses at FIRST
CONTACT any row whose own content no merge could handle (junk CRDT
values, stampless LWW rows, CRDT tombstones), closing the
arrival-order-decides-truth divergence any fleet-key holder could
otherwise mint by front-running a key.
**Step 4 is CODE (2026-08-13, same session as step 3):** the escrow
doors — `fauna.generation.escrow.{put,get,delete}`
(`fauna_protocol::generation_escrow`; nest handlers User-class and
account-derived; `generation_escrow_wraps` table, composite-PK
idempotent, `Policy::Purge` + `Succession::Burn` per the succession
rider) and the **holder-generic receipt contract in shared Rust**
(`fauna_core::generation`: the frozen `ESCROW_RECEIPT_SIG_CONTEXT` tag,
`sign_escrow_receipt` / `verify_escrow_receipt` — v1 profile reads
`holder_id` as the Ed25519 key itself, nothing assumes the deployment
key), with the two build rulings recorded in the escrow-doors bullet
(byte-identical receipts on re-deposit; account-is-the-admission).
tier_3: `bins/fauna-nest/tests/generation_escrow.rs`.
**Step 5 is CODE (2026-08-13):** the mint — the random
`fauna_core::crypto::GenerationKey` + frozen commit context
(`commit = BLAKE3(gen_key)`, KAT-pinned), the content-derived generation
id over the canonical `MintCore` (covers the commitment, so same-shaped
concurrent mints fork into distinct ids), the two machinery keypairs
(`fauna_core::generation::derive_{device,escrow}_xwing_keypair` — both
halves deterministic from the device Ed25519 secret / the identity seed
under frozen contexts, KAT-pinned), the generation-N fleet schedule
(`FleetOnlySchedule::derive_for_generation` — the same frozen context
strings over new key material, pinned by test), the X-Wing wrap
seal/open + mint assembly (`fauna_mls::wrapped_blob::generation_wraps`
— every open verifies the commitment; substitution is refused at the
AEAD's `(generation, device)` binding or at the commitment, both
red-verified), the engine's **deposit-first** mint sequence + the
seed-holding escrow-target publication
(`fauna_sync_engine::generation_mint` — refuses precisely while no
`fauna.state.escrow-target` row is published; the verified receipt is
*input* to the returned mint/receipt rows, so an unescrowed generation
is unrepresentable as its output), and the tier_3 chain over the real
doors (mint → deployment-signed receipt → a second device unwraps its
inline wrap → seed-ceremony recovery through the get door —
`bins/fauna-nest/tests/generation_escrow.rs`).
**Step 6 is CODE (2026-08-13):** the tip-resolution view
(`fauna_core::generation::resolve_admissible_tip`, pure over merged
mint + receipt rows + the FleetView, the three resolver rulings in the
gate bullet above, supersession + candidacy mutation-red-verified) —
**and the writer door consumes it**: the boolean gate is REPLACED by the
sealing-epoch dispatch (`fauna_sync_engine::account_state_plane` —
`admit_origination` + the seal dispatch; the store seam + the writer's
own-wrap opening are `fauna_sync_engine::generation_tip`). `Gen0` kinds
— every delegable kind and the machinery kinds themselves — are
admitted (machinery rows now write through an ordinary fleet-scope
`put`); a `GenerationTip` origination resolves the tip and refuses only
when none resolves, seals **form v2 under the resolved tip's key** (the
writer opens its own inline/top-up wrap with the device KEM secret
derived from the plane's `writer_key`), and the walk's open path routes
v2 rows by their cleartext generation id — integrity-only (content-id
readback + commitment at the unwrap), deliberately no admissibility at
read time: historical generations legitimately name now-removed members
(`generation_tip` module docs own the statement). The door also enforces
the A5 home-scope partition for what this replica originates. The plane
now takes a `GenerationTrust` (root + prior ids + trusted escrow
holders); the production runtime passes an **empty holder set until
step 7's plumbing** — fail-safe: no tip ever resolves there, exactly the
prior gate. Conformance: the step-6 section of
`conformance_account_state_walk.rs` (door refusals incl. no-tip and
wrong-home-scope, v2 seal + cross-replica open via inline wrap, top-up
self-healing on both the read and seal sides, the wire-level form-v2
peek, and the pre-door v1 row's read-side compat carve-out — restricting
the v1 trial chain to `Gen0` kinds is a **step-7 decision**, taken with
the production re-staging of the door-less fixtures).
**Step 7 is CODE (2026-08-13) — the build's seven steps are complete.** The
first production consumer is proven end to end in
`conformance_account_state_walk.rs` § step 7: two devices enroll through the
door, a seed-holding surface publishes the escrow target, and
`fauna_sync_engine::generation_mint` runs its deposit-first sequence against
the **real** `fauna.generation.escrow.put` handler — so the receipt lifting
the door is one the nest signed with its deployment identity, and the trust
accepting it is that same pinned identity — after which
`fauna.state.device-endpoints` seals form v2 under generation 1 and opens at
the peer. The pre-mint refusal is asserted in the same flow, *and* asserted
to leave no durable local row (door, not seal time). **Production trust
plumbing:** `AccountRuntimeParams::trusted_escrow_holders` carries the app's
pinned nest identity — trust comes from the pin, never the nest's claim about
itself. Both production assembly sites read it through the ONE door
`trust::trusted_escrow_holders`: `apps/fauna-tui/src/session.rs`, and
`bins/fauna-sync-agent/src/account_host.rs`, which hosts the runtime for the
other six apps (it open-coded the same pin read until 2026-08-17, which is why
the e2e trust seed reached tui alone — `e2e-automation-surface-gating.md`
§ The e2e trust seed). The succeeded-from `prior`
ids were read in-runtime from the device-local `__config` replica until
2026-09-13; **since then they are the app's ATTESTED predecessor set**
([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation
machinery → *The source of `prior`* owns the ruling, and what the set is
for since it stopped signing in the fleet view):
`AccountRuntimeParams::attested_predecessors`, built
from `AccountRegistry::predecessor_backup_keys_by_actor`
(`AttestedPredecessors::from_registry`; since 2026-10-01 the value carries
each identity's delegable schedule beside its id —
[`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md)
§ Implementation status today owns that entry), resolved once at the
post-auth hook on tui and linux — the same registry walk their retired keys
come from — and handed to the seedless agent over the additive
`SyncCapability.predecessor_actor_ids` beside those keys
(`bins/fauna-sync-agent/src/account_host.rs` reads both lists into the same
params; a changed list re-mounts the stint). The replica's `prior_actor_ids`
is read by no trust consumer any more. **The three UniFFI apps (windows,
macOS/iOS, android) attest through the same walk**:
`fauna-ffi`'s `start_account_runtime` takes the app's `FfiAccountRegistry`
and `account_runtime::install` resolves the session's own actor's
predecessors off it in Rust, and `FfiSyncAgentProvisioner::build` carries the
ids for the two agent-hosting apps (windows and macOS); an identity that
never succeeded still resolves empty, which is fail-safe there, as the
ruling states. Each half degrades fail-safe
alone. Two step-6 riders were taken with it: the **v1
epoch restriction** ([`account-sync-plane.md`](account-sync-plane.md) § The class-2 entry form → form v2 owns the ruling) with
the A5 read-side mirror, and the re-staging of the door-less device-set
fixtures onto `state-fleet`. **Trigger (a) is CODE (2026-08-13), and with it the production path from a
first `GenerationTip` write to a minted generation.** The writer door no
longer refuses forever: `AccountStatePlane::resolve_or_mint` mints
whenever **no candidate tip resolves for this observer** — the
candidate-aware first-need ratified with the ST-007 fix (the initial
landing's narrower guard, "refuse whenever any mint row exists", is
retired; § The mint protocol owns the rationale and the leaves-as-parents
heal semantics) — by running `generation_mint` against the real escrow
door, publishing the mint and receipt rows through that same door,
re-resolving, and letting the origination proceed. It sits at the door
rather than in a background pass because the refusing write must be the
one that then succeeds, and because the door keeps no durable local row;
the seal path stays mint-free (`publish_pending` re-enters it every pass)
and the peer leg never mints.
**The ST-007 resolver re-arm is CODE (2026-08-13).** The finding
of the R14 escrow-chain security grade (ST-007, 2026-08-13) is closed
at the trust boundary: `resolve_admissible_tip` enforces the full
candidacy predicate (non-empty member set, `minter ∈ member_ids`, the
required in-value **minter signature**, view-verified members, trusted
ack, observer-keyability), `build_mint` signs, and the wedge shapes are
pinned red-first — unit pins in `fauna_core::generation` (the probe reds,
now permanent) and the two real-door conformance pins in
`conformance_account_state_walk.rs` § ST-007 (first-mint race +
retirement). Consequence A (permanent sealing wedge) is closed for every
author class; the minter signature also closes Consequence B's
forged-authorship enabler at the resolver, while B's operational gate —
**whether a properly-removed device retains escrow-`put`-door auth at the
pinned nest** — was traced 2026-09-19. The answer depends on whether the removed device holds the seed.
A *seedless* principal removed on both legs cannot reach the door: `fauna.sync.devices.delete` tombstones its grant key, so no session is minted for it (the device handshake checks the tombstone both before and after minting a bearer). A mint it made before removal becomes inadmissible wherever the `Removed` row merges, and a new mint made after removal can never collect an escrow ack. A *seed* holder keeps full account access, by design: R14 severs only seedless replicas (above), and succession is the remedy for a copied seed. A removal whose nest leg lands but whose fleet leg does not leaves the device a fleet member. [`account-data-taxonomy.md`](account-data-taxonomy.md) owns the two removal legs. Still owed
with the triggers: (c). (b) is built (2026-10-01) as the remover's closure — the remover closes every generation the removed device may key, and the next origination mints ([`account-data-taxonomy.md`](account-data-taxonomy.md) § Implementation status today) — and (d) is built (2026-09-28, `fauna_account_plane::generation_reescrow`). **The W5-era top-up
self-heal pass is BUILT as of 2026-08-15 — its writer half; see the entry
below for what it does and the end-to-end proof it still owes.**
The production runtime carries what the mint needs to find: a `state-fleet`
plane beside the delegable one (`walk_one_scope` walks both), and a
bootstrap inside `start()`'s readiness barrier that self-enrolls the device
into `fauna.state.device-set` and writes `fauna.state.escrow-target` from
the identity seed — both write-if-absent, both local-only there (the
prologue's first step publishes them: the barrier is the stretch a sign-out
cannot cut, [`apps/account-scoping.md`](apps/account-scoping.md) § Erasure
follows scope), and both in the runtime rather
than behind an app surface because they are seed-holding derived key
material no human chooses (product-invariant bucket (1); an app knob for
either would be configuration theatre).
**The device-endpoints writer is CODE (2026-08-13) — the first production
`GenerationTip` originator, discharging T5's publish half.** Every full pump
pass ensures this device's own `fauna.state.device-endpoints` entry
(`fauna_sync_engine::device_endpoints_writer`, the pump step after the
fleet walk): write-if-changed from the writer identity plus observed
transport facts (since W5.7 the assembly seam's bind self-feeds them —
bound LAN candidates + the nest's relay URL — with the app-fed
`AccountStoreHandle::set_endpoint_facts` Cmd as the explicit override; a
replica with no bound leg still publishes the floor row of `node_id`
alone, honestly "this device exists, no dial paths yet");
**re-published when the resolved tip supersedes the one the row was sealed
under** — the retained-window re-seal, scoped to this kind, detected
statelessly by the current tip's derived wire item key being absent from
the replica's own relay plane. That discipline is load-bearing, not
polish: generation 1 is minted with the founding device alone and sealing
resolves at seal time, so without it a later-enrolled device could never
open the founder's row. The step skips quietly when no tip resolves and no
escrow holder is trusted (the fail-safe posture — a put would run the mint
sequence to a post-deposit refusal every pass). The first publish on a
real account is what trips trigger (a) end to end — tier_3:
`conformance_account_runtime.rs` § V5 (two runtimes over the real escrow
doors: the founder mints alone, the joiner heal-mints the covering tip,
the founder re-seals, both open both rows). Discovery
(`fauna_peer_sync::discovery`) thereby has its production feed; what the
entries lack until W5 is dial candidates from a live bound transport.
**Still owed, and deliberately
later:** the mint protocol's triggers (b) removal-observed and (c) cadence
constant ((d) succession is BUILT 2026-09-28 — `owner-key-material.md`
§ Implementation status today); and
the Merged-arm re-publish of a `GenerationTip` CRDT kind would abort a walk
at a replica that can read but not seal — unreachable today (the only tip
kind is LWW with per-device keys, never Merged), to revisit when the first
fleet CRDT kind registers as `GenerationTip`. **R14–R17 and the A3–A8
amendments (ruled 2026-08-11) are ruled, and of R14 both the machinery and
its first-need trigger are now code end to end** (trigger (a), above — the
writer door is the production surface that invokes the mint): the R14
admission stays
**enforced rather than merely recorded** — the account-state plane's writer door refuses to originate a
`GenerationTip` entry while no admissible, escrow-acked tip resolves
(`fauna_sync_engine::account_state_plane`; it deliberately does not refuse
*relaying or merging* a row another writer already put on the feed, which
would starve the item rather than protect anything). No arbiter-epoch
entry, no `ext:<kind>` sub-scope rows (the per-rung partition itself is built; the per-kind family is ruled in `third-party-kinds.md`), no
kind-manifest machinery, no custody receipts, no keychain-sealed local
replica on any platform, no box-scope code (and none is licensed —
R16 is direction, sequenced far behind W2–W6); the seen-set's kind, value
shape, and join are built birth-compactable per A4 (W2.5 item 2, above)
while its production trigger stays W3's; storage-classification
inventories and lint (R17) do not exist yet
([`storage-classification.md`](storage-classification.md) § Implementation
status today). The 2026-08-09 local-first account-data survey
(`2026-08-09-local-first-account-data-survey.md`, internal plans tree —
tracked internally, not shipped) is the code-level evidence base. What
exists, and what the plane grows from:

**The export-confidentiality axis status entries → [`account-data-taxonomy.md`](account-data-taxonomy.md)** (2026-09-06 concept partition).
They moved with the classes they grade — a status ledger travels with its concept, not on its own.

- **Built — W1 slice 1 (2026-08-10): `libs/fauna-account-store`.** The
  `fauna_sync_engine::db` floor extracted per its contract (re-exported, no
  call site broke; `app-guidelines.md` § crate layering records the
  execution), plus the store's own planes per § Store logical schema:
  per-writer journal (gapless local append, verbatim gap-tolerant ingest
  with equivocation refusal), state entries (atomic entry+journal write,
  opaque canonical value bytes — key-less per R7), per-scope frontier
  vectors (never-regress by construction, accounted-walk-only advance),
  store meta with the § 2.2 `format_version` pair, and the
  `engine.lock` filename reserved for W5 — all behind the AFIT
  `StoreBackend` trait, which compiles for wasm32 (web's backend is not
  built). **Not built:** consumer wiring (W3), the sync walk itself
  (W2.4+), projections. tier_1-proven (`cargo test -p
  fauna-account-store --lib`).

**The W4 status entries — the offline outbox, the `OfflineSafe`/`OfflineQueued` split, and the whole per-app desensitizing fan-out → [`account-offline-mutation.md`](account-offline-mutation.md)** (2026-09-06 concept partition).
The largest single run in this ledger.

**The W5 and W6 status entries — engine-singleton election, the seedless host, hosting and renewal, the top-up self-heal, the peer-leg assembly and dial passes → [`account-runtime.md`](account-runtime.md)** (2026-09-06 concept partition).

**The W2 and T1 status entries — class-2 entry primitives, the block plane, the generalized feed, the client walk + merge seam, the content-scope string, the peer leg, and the observation intake → [`account-sync-plane.md`](account-sync-plane.md)** (2026-09-06 concept partition).

**The W3 status entries — the `fauna-ffi` seat and the linux, android, apple, windows and tui hosts → [`account-runtime.md`](account-runtime.md)** (2026-09-06 concept partition).

- **Ruled + built 2026-10-01 — principal succession's third trigger: the machine's own `Removed` device-set row.** [`account-replica-posture.md`](account-replica-posture.md) § The store device principal → *Principal succession after a device delete*, decision 1, owns the rule, how the evidence travels and why a sign-out's own row is not it. What is code: the enrollment step reads the own row first (`account_driver/enrollment.rs` — `RemovedFromAccount` with `PumpReport::own_row_removed`, or `SignedOut` when the store's sign-out stamp names the held writer); a seed-holding serve under the rotation cap reassembles and keeps the finding on the driver (`AccountDriver::own_row_removed`); and `principal_succession::ceremony_probe`, handed it by both assemblies (`fauna_sync_engine::account_runtime`, `fauna_account_plane::web_host`), rotates with no nest leg. The sign-out's stamp is `AccountStore::mark_writer_signed_out`, written by the retirement ahead of the severance and cleared by a seed-holding host start. Proofs are listed at the owner; the tier_3 case is `conformance_account_plane_bind::the_guardian_marked_device_removed_on_the_fleet_plane_returns_as_a_successor`.
- **Built — principal succession (2026-08-15): delete-then-sign-in
  revives the machine as a NEW fleet device with its un-pushed tail
  preserved.** The § The store device principal succession ruling as code,
  under its eight build-time refinements (recorded at the ruling). The
  pieces: **the append-time writer guard** — every local append
  (journal insert, class-2 put, class-1 stage, outbox enqueue) verifies
  `META_WRITER_ID` inside the append transaction and refuses typed
  (`fauna_account_store::store::StaleWriter`); ingest is deliberately
  unguarded. **The fence** — `rotate_writer_identity` re-stamps the writer
  meta + the pending-re-author marker + the permanent retired-writers
  memory in one transaction (refinement 8; the walk's self-echo guard
  reads it so a revived machine never re-ingests its own former rows);
  the runtime worker's assembly/serve body is a loop, so a stale process
  reassembles
  from the shared slot (planes, peer leg, fleet bootstrap all rebuilt)
  while app-fed state survives. **The trigger** — the distinct
  `fauna.sync.device_grant_revoked` wire code
  (`RpcError::CODE_SYNC_DEVICE_GRANT_REVOKED`; `is_device_grant_revoked`), carried by
  the ceremony probe every seed-holding assembly runs grant-first,
  budget-bounded and onto the row decision 2's third condition allows
  (refinement 9); `EnrollmentPass::Revoked` surfaces the evidence on
  seedless pumps, and a seed-holding worker treats it as reassemble.
  **The rotation** (`fauna_account_plane::principal_succession`, run by
  every host's assembly — natively re-exported at
  `fauna_sync_engine::principal_succession`) — under a
  Held migration section: re-validate, mint + persist the successor
  (read-back compared), re-sign the `DeviceAuthorization`, fence, restart
  assembly. **The lost-slot arm** (refinement 10, 2026-08-27 —
  `principal_succession::lost_slot_heal`, called by the assembly between
  the backend open and the store open): the stamped writer read from the
  backend, a disagreeing slot key fenced onto under a Held section (the
  slot re-read there; a retired writer in the slot re-mints and restarts,
  capped once per runtime worker), the marker multi-valued
  (`pending_writer_reauthors`, additive second meta key). ⚠ **The re-mint cap
  counts mints, not restarts** (fixed 2026-08-29): the restart answer is two
  variants, `RemintedIntoSlot` and `RestartAssembly`, because a benign sibling
  race — the slot moving under the section, minting nothing — used to burn the
  budget on the one shared variant, after which the *designed* retired-writer
  path refused instead of healing and ended the runtime thread. Pinned by
  `a_sibling_race_restarts_assembly_without_claiming_a_mint`. Proven by three
  runtime-level scenarios over the stateful fake — the lost slot heals
  with its un-pushed row re-authored under the successor and published as
  it, an install a pre-fix build already stranded heals with no third
  identity minted, a slot restored to a retired writer is never reused —
  and store-level marker tests. **The journal-bound writer** (refinement 11,
  2026-09-15 — the same `lost_slot_heal`, now on the mint-or-load resolver's
  `WriterKeyProvenance` and on the stamp read beside the walk's burnt verdict in
  one transaction, `stamped_and_burnt_writer`): a key LOADED over a store with
  no stamped writer re-mints, stamps the fresh store and retires the loaded key
  (`retire_unjournaled_writer`); a stamped writer the walk marked burnt
  (`WalkReport::own_burnt` → `AccountStore::mark_writer_burnt`; the pump
  reassembles once per worker on it) is fenced onto a fresh mint through
  `rotate_writer_identity`. The stateful fake refuses a reused coordinate as the
  real nest now does (`StateEntryError::SeqReused`, same `stale_writer_seq`
  code; db + conformance pins). Proven by two more runtime-level scenarios: a
  surviving slot over a deleted store dir retires the key and a second replica
  walks both lives without equivocation, converging on the newer write; a
  journal restored from an older backup is found burnt by the walk, rotated, and
  the pre-backup life's later row lands as retired history; and where the burnt
  life had written, the tail re-author compacts the burnt predecessor's carried
  rows once their entries re-put, so the previous life's row at a coordinate the
  burnt journal held under another key lands as well (the rule and its named
  residue: `account-replica-posture.md` § The store device principal,
  refinement 11). The residue below that bound was ruled and built
  2026-09-16: own rows publish in journal order (every local write goes
  through `publish_pending`, so an inline publish can no longer raise the
  own slot past an unsent row — which also closed a general reconnect-race
  strand of offline rows), and the retired burnt writer's rows at held
  coordinates are carried rather than echoed (merged through the ordinary
  apply, re-journaled as an own row only when the entry changes, the burnt
  life's shadowed relay row retired — `WalkReport::retired_carried`,
  `StoreBackend::relay_retire_shadowed`), stable across passes even where
  the feed serves two items at one coordinate (a peer relaying the burnt
  life's row beside the nest's); the residue that
  stands is named in the same paragraph. The last detectable shape below the
  bound closed 2026-09-16: a served own row at a held coordinate under the
  SAME item with OTHER content is now read through the ENTRY — an own row is
  re-sealed from the entry's current plaintext for as long as `entry_version`
  agrees, so other content there was authored by another journal — which
  catches the commonest restore shape of all (a backup whose next writes hit
  the keys the previous life also wrote) and, undetected, was a permanent
  publish wedge rather than a lost value. The same duplicate met at every
  OTHER replica — a foreign writer's second row at a held coordinate, which
  used to abort every full pass's zero-frontier reconcile as journal
  equivocation — is carried too since 2026-09-16
  (`WalkReport::double_served`; the ruling is refinement 11's). The e2e relaunch
  carry carries the replica beside the slot from the same day
  (`e2e-launch-isolation.md` convention 10). **The tail re-author** — a marker-gated pump step re-puts
  each distinct un-pushed entry's current value under the successor
  (`merge_meta` preserved; fresh coordinates + publish seal) before the
  same pass's publish. Proven: store-level guard/fence/marker tests
  (mutation-red-verified); the runtime-level heal + the full
  delete→revive scenario over a stateful fake (scope-level publish +
  feed-writer + ghost-free pins, mutation-red-verified); nest conformance
  pins the typed code positive AND never-fires (capability-missing,
  bad-signature); tier_3 **V13** (`conformance_account_runtime.rs`) runs
  the whole journey over a real listening nest — real delete handler,
  real tombstone, real WS error classification, the successor's row on
  the real feed, the dead writer absent. **Not built, stated:** class-1
  local-authorship re-author (no production class-1 author exists; the
  duty is anchored in `tail_reauthor_pass`). ⚠ **The pass filters the pending
  predecessors against the store's LIVE STAMPED writer, never a handle's cached
  one** (fixed 2026-08-29): `AccountStore::writer()` is a snapshot taken at
  `open` and never reassigned — `rotate_writer_identity` takes the backend, not
  the store — so after a co-located sibling fenced `A -> B`, a still-open
  `A`-handle filtered the marker's own `A` out as if hand-damaged, fell into the
  no-marker arm and **deleted both marker keys**, silently and with no error.
  `A`'s un-pushed tail was then re-authored by nobody, since the successor's
  pump finds nothing pending — decision 3's invariant broken by the very path
  that implements it, on the lost-slot heal's own flagship scenario. Pinned by
  `a_siblings_fence_does_not_let_a_stale_handle_delete_the_reauthor_marker`,
  mutation-red-verified against the cached-writer comparison. ⚠ **And the pass
  takes that pair as ONE SNAPSHOT, then deletes the marker only by
  compare-and-delete against it** (fixed 2026-08-30): the operand fix above left
  the *serialization* absent — the stamp and the marker were still two
  unserialized reads against a fence that writes them in one transaction, so a
  fence landing between them served a pre-fence stamp beside a post-fence
  marker and the filter dropped the marker's own predecessor exactly as the
  cached handle had, reaching the same loss through a race. Reordering the reads
  cannot fix it (marker-first reads empty pre-fence, and the empty arm then
  clears the marker the fence has just written), and a consistent read alone
  cannot either (the decision is taken on a snapshot, and the fence can land
  after it) — so both halves are the mechanism: `pending_reauthor_snapshot`
  reads the stamp and both marker keys in one backend transaction
  (`meta_get_all`), and `clear_writer_reauthor_if_unchanged` deletes only while
  the marker still holds what that snapshot read
  (`meta_delete_all_if_unchanged`), on **both** of the pass's clears — the wide
  empty arm and the narrow post-walk one, where a fence landing after the last
  re-authored append is not covered by the append guard. A refused clear is a
  benign verdict, not an error: the marker stays for the next pass, whose
  re-walk is idempotent by decision 3's own argument. Pinned by
  `a_fence_between_the_snapshot_and_the_clear_leaves_the_marker_alone`,
  `a_fence_after_the_walk_and_before_the_clear_leaves_the_new_marker_alone`
  (both firing the fence from a real second connection inside an interleaving
  window, and asserting the window fired) and
  `the_marker_clear_refuses_a_picture_that_changed_under_it`. ⚠ **The walk's
  self-echo exception reads the same pair LIVE, on both `own` sites** (fixed
  2026-08-30): the 2026-08-29 fix corrected one consumer, but the defect is a
  property of the *type* — every reader of `AccountStore::writer()` /
  `retired_writers()` inherits it — and the peer leg's "is this row mine?"
  decision was the other unprotected one. Ingest is deliberately unguarded
  (succession decision 4 fences *appends*), so nothing stopped a stale handle
  from walking: after a sibling fenced `A -> B`, an `A`-handle classified a row
  authored by **`B` — the store's own current writer** — as another replica's,
  and the exception did not fire. Two consequences on opposite branches, both
  from the one misclassification: where the ingest re-derivation happens to
  collide with the local `entry_version` the row ingests idempotently and the
  tail advances `B`'s frontier slot **on a peer's word alone** — and that slot
  is `publish_pending`'s published-to-the-nest high-water, MAX-merge and never
  regressing, so an un-pushed tail is lost to the nest for good; where it does
  not collide, `ingest_state` bails "journal equivocation" against the store's
  own history and the whole walk aborts until the handle reassembles. The
  classification now goes through `AccountStore::writer_relation`, which reads
  the stamped writer and the retired set from the store meta at decision time,
  in ONE transaction — the tear between two reads misclassifies the successor
  or the predecessor for its width, and a per-page or per-walk snapshot would
  be the same defect with a longer one. **Ruled:** the equivocation `bail!` needs no remedy of its own **in the
  OWN-writer case** — it is a correct detector of genuine equivocation, and
  a row recognized as this store's own never reaches it. (The FOREIGN-writer
  case — a second row at a coordinate the feed serves under two items (a
  nest's row plus a peer's relay of a burnt writer's refused row), met by
  every replica but the healed one — is a different shape and
  is carried, not refused, since 2026-09-16:
  [`account-replica-posture.md`](account-replica-posture.md) § The store
  device principal, refinement 11 → *a foreign writer's second row at a held
  coordinate is carried*.) Pinned at the plane level by
  `a_peers_echo_must_not_move_the_successors_publish_watermark` and
  `a_stale_handles_walk_does_not_abort_on_its_own_stores_history`
  (`stale_handle_self_echo.rs`), one consequence each, both mutation-red-verified
  against the cached-pair expression. The group plane's twin site is fixed in
  the same change: its harm is **latent** — that plane has no publish leg, so
  nothing reads the slot a misclassification moves — but the latency is a
  property of today's callers, not of the decision, and whoever lands its
  publish leg would otherwise inherit a watermark a stale handle can already
  move. **Still stated:**
  a host process's cached
  device-principal NestClient (`account_host.rs`/tui session) keeps
  handshaking as the dead key until process restart — renewal re-reads
  the slot per pass, so only the long-lived data client lags; an
  offline sign-in that later reconnects re-probes only at its next
  assembly (the ruled sign-in trigger point).
- **Shipped assets:** the per-actor placement + `fauna_sync_engine::db` floor
  on all 7 apps; the file-sync engine (feed + anchor + nudge + causal
  frontiers + conflicts); the rails' CAS + CRDT merges (the offline
  `UserConfig` replica they shipped with retired with the rail on 2026-10-02 —
  [`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *The closure
  order*, step (6)); the nest segment stores + the ratified client-device
  custodian pull; the iroh transport seam + Y.1 `PeerChannel`; the sync
  agent + its persisted-capability credential model; `DeviceAuthorization`.
- **Replica posture (R7/R8):** nothing of the custodian leg is built —
  no custody grant and no key-less store mode. The sealed per-writer class-2
  form itself is no longer a gap: both its derivation and its envelope landed
  at W2.0 (entry above); what is missing is every *position* that would hold
  or relay one. Shipped precedents it generalizes: held-for-friends backup custody
  (destination stores opaque under the owner's `BackupKey`), the rails'
  sealed-CAS-client-merge shape, the grant plane's keyless-scope shape.
- **Headline gaps (survey § 4):** no general sync
  plane (feeds cover files + rails only; push `seq` discarded; recovery =
  re-fetch) — the local content store this gap used to head is now built
  (the W2.1/W2.2 entry above), but nothing yet *syncs* into it; no outbox
  and connection-scoped idempotency; the per-actor
  boundary not enumerable nest-side; deliberate per-app divergence (roots,
  keyring namespaces, filenames, android's Room schema, web's storage void);
  the instance lock no longer refuses same-account concurrency on any
  desktop app (W5.6 landed 2026-08-15: tui/linux/macos serve concurrently,
  windows joined them 2026-08-24; the conversations-engine role lock — the
  guard that had ridden the refusal — is kernel-enforced in the shared
  engine-construction path, § Multi-instance concurrency);
  web needs the parallel backend; ordering is single-master by
  construction.
- **The at-most-one-instance-per-account law is superseded per app**
  (account-scoping.md § Concurrent instances, re-ratified 2026-08-16):
  retired on tui/linux/macos/windows — every app that ever enforced it. It
  survives only as the stricter half of the version-skew interlock, for as
  long as a pre-retirement binary can still hold an exclusive lock.
