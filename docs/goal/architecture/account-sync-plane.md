# The account sync plane — target state

Owns: account-sync-plane
Status: ratified — W2 landed with e2e-proven code (W2.0–W2.6, the last on 2026-08-12); the class-2 entry form (T14) is frozen, the content-scope string encoding ratified and frozen 2026-08-11, and the peer leg's two W2.6 design gates ratified 2026-08-11 ahead of the build; split verbatim out of `account-data-plane.md` on 2026-09-06
Authority: **how account data moves between replicas** — the general sync-plane contract (ordering model R6, the class-2 entry form T14, feeds and cursors T4, the merge-policy seam, kind namespacing, nudges and backstops, the substrate settlements) and the device↔device **peer leg**'s data-plane rules (requirement 7: the admission seam and the wormability walk), together with their build-out. **NOT owned here** — the P2P transport seam itself → [`../behavior/p2p.md`](../behavior/p2p.md); what the plane carries → [`account-data-taxonomy.md`](account-data-taxonomy.md); which process runs the engine → [`account-runtime.md`](account-runtime.md); what an app may do while the plane is unreachable → [`account-offline-mutation.md`](account-offline-mutation.md); what a replica may hold → [`account-replica-posture.md`](account-replica-posture.md); the charter and the cross-cutting status → [`account-data-plane.md`](account-data-plane.md). On conflict in those domains, raise it.

Last verified: 2026-09-06 (split verbatim; per-slice build dates are in the status entries below)

Split verbatim out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06 — that doc had reached **686,434 B**, 2.62× the 262,144 B whole-file read ceiling, and no single seam could clear it (moving its status ledger alone left both halves breached, re-verified at three successive sizes). Its own `Authority:` line already enumerated the five concepts it owned; this is that list made structural, each concept taking its rule sections **and** its status-ledger entries together. A routing stub remains at each original location; prior history: `git log --follow docs/goal/architecture/account-data-plane.md`. The `W<n>` workstream labels and `R<n>` decision labels used throughout are defined in [`account-data-plane.md`](account-data-plane.md) § Workstreams and § The ratified decisions.

> **Reading this doc.** Its text was carried **verbatim** out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06, so an unqualified `§ <name>` citation inside it may name a section that is no longer a sibling on the page. Resolve any such name against the rest of the family first: [`account-data-plane.md`](account-data-plane.md) (the ratified decisions, the account store, the nest-side requirements and the cross-cutting status), then [`account-data-taxonomy.md`](account-data-taxonomy.md), [`account-offline-mutation.md`](account-offline-mutation.md), [`account-runtime.md`](account-runtime.md), [`account-replica-posture.md`](account-replica-posture.md). Positional words (“above”, “below”) inside a carried block point within this doc: every section moved whole, so an intra-section deictic could not break, and the boundary-crossing ones were scanned before the split.

## Section map

- **[The sync plane (W2)](#the-sync-plane-w2)** — the ordering model, entry form, feeds and cursors, merge-policy seam, the kind-namespacing stub (the rules moved to `third-party-kinds.md` 2026-10-02), the bind leg (how a nest a runtime binds to is made a complete replica) and substrate settlements.
- **[The peer leg — device↔device sync over iroh (requirement 7)](#the-peer-leg--devicedevice-sync-over-iroh-requirement-7)** — the admission seam and the wormability walk.
- **[Implementation status today](#implementation-status-today)** — W2.0–W2.6 and the observation-intake (T1) build-out.

**Both `##` headings are unchanged from `account-data-plane.md`**, so a `§ The sync plane (W2)` or `§ The peer leg` citation resolves by swapping the filename.

## The sync plane (W2)

### Ordering model (R6 — settles survey Q13)

The plane is **multi-master**. Each writer (device principal, and the nest
itself) appends to its own log; convergence is frontier exchange + per-kind
merge, the shape `fauna_sync_engine::causal` already implements for file
paths (`CausalStamp`, `PathFrontiers`) generalized to items. The nest is a
**distinguished replica**: durable, always-on, the content bootstrap for new
devices, and the **arbitration point for the minority of kinds that genuinely
need central assignment** (nest-arbitrated kinds, § The offline-mutation
contract). The distinction is **operational, never protocol-level** (user
directive, 2026-08-10): one peer-symmetric sync protocol, and a replica syncs
with whichever peers are reachable — nest or app alike. A "cursor" on this plane is a **frontier vector** (per-writer
high-water marks), of which the nest's existing global `sync_changes.seq` is
the nest-writer degenerate case — so the nest-mediated star topology is the
*first build stage* of the full model, not a different protocol: W2 can land
nest-first and add the peer leg without changing the contract.

Why not single-master + outbox gossip: it preserves today's ordering
assumptions but cannot converge applied state between two offline devices —
requirement 7's whole point. The multi-master cost — per-kind merge
discipline — is a cost the plane pays anyway (the merge-policy seam below
exists regardless), and the commutative merges the codebase already ships
(the per-kind merges lifted out of the retired `merge_user_configs`,
`merge_history_slices`) are exactly frontier-safe.

**The arbiter seat is trusted, not proven — ACCEPTED for this plane
(ratified 2026-08-11).** An authorization boundary must never be mistaken
for a cryptographic one, so the seam is stated with its bounds and its
status. What the seat *cannot* do even dishonestly: forge, splice, or
replay — class-1/3 records authenticate by content address, class-2 entries
are sealed per-writer and authenticate their exact position in their
writer's log, and a nest-arbitrated kind's convergence backstop is the
client-side merge over sealed content (as the `__config` CAS's was until its
2026-10-02 retirement — [`config-dissolution.md`](config-dissolution.md)
§ The `__config` dissolution schedule → *The closure order*, step (6)), so
the arbiter never manufactures a
state a replica accepts as some writer's work. What it *can* do: order the
nest-arbitrated minority dishonestly among concurrent requests, and
withhold — silence, and stale-frontier service, which the user's own
devices can notice by comparing frontiers across the peer leg but which no
proof pins. This is **accepted here rather than scheduled for closure**, on
the plane's own shape: it is a single-account plane whose writers are the
user's own devices and whose arbiter is the user's own distinguished
replica, so the reachable harm is reordering or withholding the user's own
writes — an availability/staleness harm the replica set already bounds,
with no cross-user stakes riding the seat. The verifiable-ordering
dimension (changelog commitments, operation proofs, member-verified
ordering) is owned by
[`encrypted-spaces.md`](encrypted-spaces.md) and is bought exactly where
multi-party stakes make the arbiter's honesty load-bearing; this plane
deliberately does not pay for it. **Revisit trigger:** any future kind
that puts cross-user or adversarial stakes on this plane's arbitration —
a shared-scope CAS, a dispute-bearing ordered kind — must either move to
the Space posture or re-open this acceptance; it must not inherit it
silently. (The first storage-group scope — T20's scheme, designed
2026-08-17 — deliberately registers **no** arbitrated kinds, so it does
not trip this trigger: § The audience ladder → The recipient-set scheme,
last bullet.)

**"No nest" is supported, as a reduced-role profile (R11 — resolves the
ambiguity two sessions could have read opposite ways).** Nothing in this
ordering model *requires* a distinguished replica to exist: frontiers,
per-writer logs, and per-kind merge converge the peer-symmetric subset
(class-1 records, class-3 blobs, commutative/LWW class-2, the seen-set)
with zero nests, and the peer leg is a full sync path, not a cache of the
nest path. What an account without a nest lacks is enumerated per kind,
never as a mode: nest-arbitrated kinds have no arbiter (they stay
desensitized exactly as they do offline — the `offline_class`
machinery already expresses this), MLS conversations have no delivery
service (the PQ-1 problem, [`../behavior/p2p.md`](../behavior/p2p.md)
§ Cross-user shared-set transfer → Offline share initiation — designed
2026-08-17: the delivery seat becomes a re-pointable role a member
device can hold; unbuilt, so still the blocker for nest-less
*conversations* until that build), and R8's internet endpoint
roles (mail, federation, public serving) are definitionally absent.
Cold bootstrap of a *new* device without any nest also remains a
declared non-goal (§ The peer leg — enrollment reaches the account
where its seed or an approving device is). UI surfaces "needs a nest"
per feature, from the kind's own dependency — never a global banner. The plane's sync units are
the **sealed canonical forms**, uniformly to every peer: class-1/3 as
sealed blocks (already forced by CID round-trip), class-2 as **sealed
per-writer entries** (key schedule per kind, the retired `__config` blob's
BackupKey-derivation precedent; form + schedule frozen — next subsection).
Merge materializes only
at reading replicas; a serving path never requires unsealing; posture never
appears on the wire. This is what lets a custodian replica (§ Replica
posture) hold and relay every plane item with zero read reach.

### The class-2 entry form (T14 — frozen 2026-08-10)

The sealed per-writer entry is the class-2 sync unit — at rest on every
replica's sealed planes and on the wire to every peer, uniformly (R7). It
is frozen **before any W2 feed code** because it is both wire and at-rest
form: changing it after W2 ships is a migration, and evolution inside the
freeze is additive-everywhere like any wire shape
([`version-compatibility.md`](version-compatibility.md); the envelope's
leading form-version byte is the additive axis).

- **Key schedule (registry owner:
  [`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 —
  context strings, derivation mechanics, rotation and KAT discipline live
  there, not here).** Per-kind **entry keys** seal values; per-kind **item
  blinds** name items: the journal/feed item key of a class-2 item is
  `keyed_hash(item_blind(kind), logical_key)` — stable for routing,
  supersession and latest-per-writer retention, opaque to every key-less
  position. Both derive off the owner `BackupKey`, which every reading
  principal's bundle carries (§ The store device principal), so a seedless
  device derives the full schedule; a capability grant hands one kind's
  `{entry_key, item_blind}` pair and confers exactly that kind
  ([`encryption-at-rest.md`](encryption-at-rest.md) § Capability tiering) —
  **delegable-rung kinds only** (R13, § The audience ladder): the
  fleet-only branch is outside the grant-mintable universe by
  construction.
  Item keys are **blinded, unlike file-sync's floor `path_hash`**: the
  class-2 vocabulary (registered kinds × field names) is enumerable, so an
  unkeyed hash would let any custodian dictionary *which setting* changed;
  the file-scope `path_hash` carve-out is untouched. **R14 adds the
  generation axis over this schedule** — generation 0 is these frozen
  root-derived branches; fleet-only kinds and content scopes seal under
  random generation keys (`owner-key-material.md` § Path A-sibling-2 owns
  the mechanics and the sealing gate).
  **The learned `item_key → kind` map is store contract, not an
  optimization** (ruled 2026-08-11, greenfield finding A5): a reader
  recovers a row's kind by trial-opening under each registered kind, and
  the store keeps the resolved mapping as a projection so the trial chain
  is first-encounter-only — without the named cache, cold walks pay
  K-kinds × N-rows AEAD opens and the cost silently grows with the
  registry.
- **Envelope.** `[form version][random 12-byte nonce][ChaCha20-Poly1305
  ciphertext+tag]`, sealed under `entry_key(kind)`, with **AAD = the
  canonical encoding of the entry's plane coordinates `{form_version,
  writer_id, writer_seq, scope, item_key}`**. The AAD is what makes relay
  trust unnecessary for integrity: no relay — nest included — can splice
  one entry's ciphertext under another's coordinates, and a replayed old
  ciphertext cannot pose as a newer row (its `writer_seq` is bound into
  the tag). The nonce is random, never derived: an entry value is mutable
  under a fixed item key, exactly the case the shipped sealed-label law
  (`libs/fauna-core/src/path_crypto.rs`) reserves `seal_random` for.
  **Form v2 — the generation-sealed envelope (R14 build design, ratified
  2026-08-13, refutable until built):** `[form version = 2][generation id:
  32 bytes][random nonce][ciphertext+tag]`, the generation id joining the
  AAD coordinates. Only generation-sealed entries write v2 — it names which
  retained generation key opens the entry, so reads are a lookup, never a
  trial walk across generations — and **v1 remains the gen-0 form forever**
  (absence of the field *is* the generation-0 marker; nothing ever
  re-writes v1 rows). The cleartext generation id is a deliberate,
  recorded custody-floor cost (§ Replica posture). Old binaries never
  opened generation-sealed kinds anyway (the R14 gate predates every such
  row), and an unknown form version already skips gracefully at
  `trial_open` — additive by construction.
  **"v1 is the gen-0 form" binds the READER too (step-7 build ruling,
  2026-08-13 — code):** the v1 trial chain is restricted to `Gen0` kinds, so
  a v1 envelope of a `GenerationTip` kind opens nowhere and is skipped as
  `unopened`. Step 6 had pinned the opposite as a compat carve-out; the set
  it protected is provably empty — the boolean R14 gate refused *every*
  fleet-only origination, and it landed before the only `GenerationTip` kind
  was registered — while leaving it open cost the severance property this
  whole axis exists for: a removed device retains `BackupKey` forever (the
  exposure the stratification bullet states), so it could keep sealing v1
  rows of a tip kind that every reader accepted, making removal enforceable
  at the writer's own door and nowhere else. The **A5 partition gains its
  read-side mirror** in the same step: a row riding a scope its kind never
  seals into is skipped (counted `unmergeable`), the reader's answer to what
  the writer door already refuses to originate.
- **Sealed payload.** Canonical dag-cbor: the true `(kind, key)`, the
  kind's merge-policy metadata (LWW stamp / per-field versions / CAS base
  echo — § Merge-policy seam), the opaque canonical value bytes, and the
  tombstone marker when the entry is a deletion. A reader cross-checks the
  opened `(kind, key)` against the outer item key by recomputing the blind
  — defense in depth over the AAD.
- **Universal in-seal writer signature (R13, amended 2026-08-11).** The
  sealed payload additionally carries the writing device's signature over
  the canonical payload bytes + the AAD coordinates — the device subkey
  certified by the identity key (the device-signed authoring chain,
  [`../behavior/devices.md`](../behavior/devices.md) § Device-signed
  authoring; succession-crossing verification per the shipped
  forged-entry-defense pattern). Rationale: the AEAD key is symmetric, so
  without a signature every reader is a potential forger — the signature
  separates read capability from write capability, gives entries durable
  attribution, and is what makes delegable-rung granting safe to be
  generous with. **No per-kind opt-out**: a discretionary "signed?" flag
  would re-create the classification hazard R13 exists to kill, and a
  kind whose write rate makes signatures matter is mis-homed on this
  plane. Amended into the frozen form while it had **no consumer**
  (verified 2026-08-11: schedule + envelope built with KATs, nothing
  seals production entries — `owner-key-material.md` § Path A-sibling-2
  status), so generation 0 includes it from birth; this is not a
  migration.
- **One shared schedule, per-writer identity.** "Per-writer" is the
  entry's *coordinates* (`writer_id` + `writer_seq`, multi-master per R6),
  never per-writer keys: all of an account's writers seal under the same
  per-kind entry key, and random nonces make cross-writer reuse a
  non-issue. Merge metadata rides inside the seal because only reading
  replicas merge.
- **Tombstones are sealed entries too.** A class-2 deletion is an ordinary
  sealed entry whose payload carries the tombstone marker; the journal
  row's cleartext `op = tombstone` exists only so key-less custodians can
  apply tombstone retention (§ Store logical schema). A relay cannot
  fabricate a tombstone that opens.
- **Cleartext floor per entry** (the class-2 slice of the custody floor):
  scope id, writer id, writer seq, op discriminator, blinded item key,
  ciphertext size, timing. Nothing else — kind names, logical keys, merge
  stamps and values are all inside the seal.
- **Nest-arbitrated kinds.** The CAS base a nest arbitrates on rides the
  *write RPC* cleartext as a version counter (the `__mls` CAS shape, as the
  retired `__config` CAS's was); the feed relays only applied truth, so the entry form carries no
  arbitration fields — the sealed payload echoes the base for reader
  verification.
- **What this closes, and what it deliberately does not.** AEAD under keys
  no relay holds gives readers end-to-end value integrity, splice
  resistance, and forge-proof tombstones regardless of relay honesty.
  The R13 signature extends this against *key-holding* positions: a
  granted reader can open its kind but cannot mint or alter entries that
  verify — write authority belongs to the signing device, never to the
  key. A withheld row is **not** detectable from the sequence (corrected
  2026-10-01 — this sentence used to say a gap is visible per writer): a
  writer's counter is gapless only in its own journal; on the feed it is
  spent across scopes and the relay keeps one live row per
  `(item, writer)`, so a gap is legal at ingest (`AccountStore::ingest_row`)
  and no reader tells a withheld row from a collapsed one — withholding
  is the residual the paragraph *The arbiter seat is trusted, not proven*
  above accepts; *fabricating* non-class-2
  journal rows (a fake `record-added`) is admission-plane territory,
  part of the W8 gate's PQ-2-class hardening (§ Replica posture), not
  this form.

### Feeds and cursors (T4 — resolved 2026-08-10, refutable until W2 code)

Per-scope item feeds, mirroring the nest's per-audience-scope stores
(`__mail/<actor>`, `__conv/<channel>`, `__post/<author>`, …,
[`message-segment-store.md`](message-segment-store.md)) plus a per-account
state scope for class-2 items, all consumed under one per-account frontier
vector. The generalized feed grows out of `sync_changes` (nearly generic
already: opaque 32-byte routing key, tombstones, causal watermark — survey
§ 3.5) with an explicit blob-vs-manifest discriminator.

**Scope partition — three scope families, one feed contract.** A scope is
the unit of subscription and of frontier tracking; a replica's scope set is
itself account data (derived from memberships, class 2) and changes as the
account joins and leaves things.

1. **The account-state scope** — exactly one per account. Carries every
   class-2 entry (settings-class, relationship-class, read/ack markers, the
   seen-set, device-endpoint entries) as `state-put` / `tombstone` items.
   The reserved rails' successor: at W2 the rails' thin CAS layer swaps onto
   this scope's feed (§ Substrate settlements), their entries becoming
   ordinary kind-keyed class-2 items.
2. **Content scopes** — one per `(kind, scope_id)` the account participates
   in, exactly the segment-store scope table
   ([`message-segment-store.md`](message-segment-store.md) § Layout is the
   authoritative kind list — never restate it): own-actor scopes (mail,
   calendar, card, own posts) plus member scopes (each joined `__conv`
   channel; followed/subscribed content scopes as kinds land on the plane).
   Feed items are `record-added` (CID) and `tombstone`.
3. **Folder scopes** — the shipped file-sync plane's per-set feeds,
   unchanged in mechanics ([`file-sync.md`](../behavior/file-sync.md) owns them) and
   recognized as the degenerate case this contract generalizes: its scalar
   per-set anchor is a one-writer frontier vector.

**The scope string (ratified 2026-08-11; FROZEN the same day — the feed walk
now writes production rows under it).** A scope's name is one canonical string,
and that one string is three things at once: the store's at-rest key (every
journal, frontier, state-entry and adopted-segment row —
`fauna-account-store`'s schema), the feed request's `scope`, and the
vocabulary admission verdicts and custody grants enumerate (§ The admission
seam). The grammar is **family-first**: the first `:`-segment names the
family, and each family owns its remainder —

- `state` — the account-state scope, the whole string
  (`fauna_protocol::account_state::ACCOUNT_STATE_SCOPE`, already frozen).
  **Since the A5 partition landing (2026-08-13, with the R14 build design —
  § The generation machinery): `state` is the *delegable* sub-scope**, its
  frozen string grandfathered with every row it ever carried.
- `state-fleet` — the fleet-rung account-state sibling (ruled 2026-08-13,
  refutable until the R14 build): a whole-string family like `state`,
  holding every fleet-only kind — the generation machinery and the
  generation-sealed data kinds — from each kind's first row. Grants never
  subscribe it; custody grants and admission verdicts name it as an
  ordinary scope string.
- `content:<kind>:<scope-id-hex>` — a content scope: the kind tag
  **verbatim** from the segment-store kind table
  ([`message-segment-store.md`](message-segment-store.md) § Layout stays the
  only authority — never the `__`-prefixed *directory* name derived from
  it), then the 32-byte scope id that table assigns the kind
  (owner/recipient/author actor; the MLS channel for `conv`) as exactly 64
  lowercase hex chars.
- `ext:<kind>` — the per-kind sub-scope of one manifest-admitted
  third-party kind, the `ext.*` kind string verbatim after the tag (ruled
  2026-10-02; frozen at the first row under it). Its rules — per kind not
  per publisher, own-account and never co-authored, which door serves it —
  are [`third-party-kinds.md`](third-party-kinds.md) § The `ext` sub-scope's.
- The folder family is **deliberately unruled** at W2 (its scopes keep
  the shipped per-set mechanics); `folder` is reserved as its family tag,
  and T17's materialization-grant scope string likewise takes its own
  family tag — neither ever lands under `content`. New families cost a
  ruling here; new *kinds* cost only a § Layout table row (below).

Five rulings a reader should not re-derive (constructor/parser:
`fauna_protocol::scope` — `Scope` / `ContentScope`):

1. **Absolute, never store-relative.** The string always carries the scope
   id, own-actor scopes included. A scope is a *shared* subscription unit: a
   `conv` channel's member replicas belong to **different accounts**, and
   the admission seam's scope enforcement and the peer legs' frontier
   exchange compare scope names by string equality across those stores — a
   store-relative "my mail" spelling has no meaning there, so there is only
   `content:mail:<recipient-hex>`, and the own-actor kinds follow the same
   rule rather than earning a shorter special case.
2. **The name is the data's, never the relationship's.** A member scope of
   someone else's content is the same string the owner's own replica uses
   (`content:post:<author-hex>` for a followed author's posts).
   Membership/subscription lives in the replica's scope *set* — itself
   class-2 account data — never in the scope's name.
3. **One spelling, refused not repaired.** The constructor emits only the
   canonical form; the parser refuses non-canonical spellings (uppercase
   hex, wrong id length, missing id, `__`-prefixed or malformed kind,
   unknown family) rather than normalizing — a normalizing parser would
   admit two spellings of one scope past the string-equality comparisons
   every consumer performs. Parsing checks *shape*, not kind knowledge:
   "well-formed" and "known to this binary" stay separate questions (the
   `ItemClass::from_wire` compat posture), so a newer kind's scope survives
   an older binary's hands in a grant, a peer exchange, or a stored row.
4. **Nest-gated per door, and refusal is never emptiness.** A nest serves a
   content scope only for kinds it knows, with the refusal shapes each door
   already has (`fauna.segments.unknown_kind` on enumeration, HTTP 404 on
   the segment byte planes, coded `invalid_request` "unknown scope" on the
   class-2 doors — and the same on the generalized feed when content scopes
   join it). To a bootstrapping replica a refusal means *this nest cannot
   serve this scope yet* — version skew, not absence: an empty scope is a
   success answer (an empty list/page), so the replica keeps a refused
   scope, reports it unserved (bootstrap/walk report), and retries when the
   nest changes — it never records refusal as converged-empty.
5. **Frozen on first write.** A scope string is frozen the moment any
   replica writes a row under it — the same law as the class-2 kind string
   (§ Implementation status, W2.4 ruling (a)). A new kind therefore costs
   exactly: its row in the § Layout kind table, and nests that admit it
   (ruling 4); the family grammar takes new kinds without change. Kind tags
   are never edited and never reused.

**Multi-writer fit (channels + shared sets).** A scope's writer set is
whoever appends: account scopes carry the account's device writers plus the
nest; a `__conv` channel scope keeps the **home nest as its sequencing
writer** for content (channel record order is nest-assigned — the shipped
per-channel `seq` — and MLS epoch ops are nest-CAS anyway, class 5), so
member replicas consume it as a one-writer feed even while their own
account scopes are multi-writer; shared-set scopes keep their shipped
multi-writer attribution. Peer-wise convergence of a channel scope's
*records* (block transfer between two members' replicas) is safe because
records are immutable — ordering authority stays the nest's.

**The frontier vector — the cursor primitive.** Per scope, a map
`writer_id → high-water writer_seq` (dag-cbor: a map of 32-byte writer keys
to u64), where `writer_id` is a device principal's public key or the nest's
key. The nest's global `sync_changes.seq` is the nest-writer's log seq —
today's scalar `since` cursor *is* the frontier `{nest: since}`, which is
what makes the star topology the first build stage rather than a fork.
Frontier semantics are the shipped anchor's accounting law generalized
per-writer ([`file-sync.md`](../behavior/file-sync.md) § Offline Catch-Up owns it): a
writer's high-water asserts every row ≤ it is applied or deliberately
skipped; only an accounted walk advances it; it never regresses.

**The walk is bounded, and the spin refusal is not the bound** (ruled
2026-09-02). Every walk of this feed refuses to spin — a page that moves the
cursor nowhere would be re-fetched forever, so it fails loudly with the row
shape that caused it. That is a **stall detector**, and it answers only *"did
this page move us?"*. It cannot answer *"is this feed ever going to end?"*, and
on the custodian's relay pull the feed is served by an **owner device over an
admitted peer channel** — the counterpart chooses the rows, their count and
their coordinates, and a key-less custodian cannot verify that a row's
`origin_writer` names any real device. Two bounds therefore sit beside the spin
refusal, both hard-coded Rust constants of the shared page loop, sized so no
honest walk reaches either:

- **A page budget per walk.** A feed that advances by the *minimum* on every
  page satisfies the spin refusal on every page. The budget is what guarantees
  the loop terminates, for both cursor shapes. It is safe to reach because a
  walk is resumable: the class-2 planes re-derive their paging cursor from the
  durable frontier, which advanced for every row actually held, and the
  custodian's pull checkpoints per page — so the refusal ends a *pass*, never
  the walk's progress.
- **A ceiling on the frontier's writer count.** For the multi-writer shape a
  row naming a *fresh* writer is progress under the spin refusal, because the
  applies insert the key before validating anything — correct (the row was
  seen), but free to the counterpart. The frontier rides in every subsequent
  request, and the custodian's pull persists it per page, so unbounded growth
  is quadratic wire and quadratic disk, ending at a cursor too large for the
  WS frame: a *stored* cursor whose walk can never run again, which is the
  client-causable unrecoverable state
  ([`nest/common.md`](nest/common.md) § Client-state recoverability) rather
  than merely a slow walk. The ceiling is sized against the honest writer set
  — live device writers (`max_devices`), plus the retired identities the store
  keeps permanently ([`account-replica-posture.md`](account-replica-posture.md)
  § The store device principal → *Principal succession after a device delete*,
  refinement 8), plus the nest, plus
  the same again per member on a shared-set scope. **Of those terms exactly one
  is bounded, and the sizing says which** (ruled 2026-09-02): the device term
  is, because the tier cap is enforced where devices are admitted
  (`fauna.sync.register` refuses past it — [`../behavior/admin.md`](../behavior/admin.md)
  § 2 Users owns the quota, [`../behavior/devices.md`](../behavior/devices.md)
  § Step 4 the refusal) and the admin-settable value is itself bounded a
  ratio below this ceiling, so the relationship the sizing assumes exists in
  code rather than in this paragraph. The **succession term is not bounded**:
  the store keeps one retired identity per succession permanently, and
  `MAX_SUCCESSION_PATH` caps a chain's *depth*, not an account's lifetime
  count. So the honest headroom is large but finite, and a frontier that
  reaches the ceiling honestly is a **real state, not an impossible one**.
  An overgrown frontier is refused **above** the checkpoint, so
  the durable cursor stays at the last page under the ceiling and stays
  sendable; the pull resumes on its own once the counterpart stops. For a
  *hostile* counterpart that is the whole remedy, plus revoking the custody
  grant from the owner's app — never a stored row deleted by hand. For an
  **honest** frontier at the ceiling there is no counterpart to stop and no
  grant to revoke: on a leg with no watermark the pass refuses, re-grows and
  refuses again. That case is reachable only after a lifetime of successions
  on one scope, and the answer is the compaction ruled below (*Compaction is
  a serve-order watermark*) — one this ceiling makes *necessary* rather than
  optional, **built on the nest leg** and deferred on the store-served legs
  (§ Implementation status today → *Built — frontier compaction as a
  serve-order watermark, nest leg*).

**Compaction is a serve-order watermark, never a forgotten entry (RULED
2026-09-13; refutable at build).** The remedy this ceiling owes an honest
frontier is *not* the shape this paragraph used to name — "a frontier that
forgets a retired writer whose rows all sit at or below the durable
checkpoint" — for two reasons a build would have met on its first day:

1. **Forgetting an entry is a request for that writer's whole live row set.**
   Every serve of this feed reads a writer absent from the frontier as
   high-water 0 — the nest's class-2 serve
   (`bins/fauna-nest/src/db/account_state.rs`, `get_account_state_changes`)
   and the store's `relay_rows` behind both the peer serve
   (`fauna_peer_sync::server`) and a custodian — and the walk's applies put
   the key straight back on the first row served. A requester-side drop is a
   full re-serve followed by the same frontier, not a compaction: the
   mechanism has to be one the *serving* side honours.
2. **No entry is provably final from the requester's side.** For a
   multi-writer scope the durable checkpoint *is* the frontier (the store's
   `frontiers` table; the custodian's meta cursor), so "at or below the
   checkpoint" holds of every entry by construction and proves nothing. And
   "retired" is a **per-store** fact — this store's retired-writers memory
   ([`account-replica-posture.md`](account-replica-posture.md) § The store
   device principal → *Principal succession after a device delete*,
   refinement 8) — never a fleet fact: under the lost-slot arm the
   predecessor lives on and its later rows must still ingest (the
   live-predecessor bound, decision 3 there). A frontier entry can only ever
   be *redundant*, never *closed*.

**What makes an entry redundant is a serve order.** An entry `W → h` tells
the counterpart which of W's rows to skip. The counterpart knows that
without being told exactly when it has a total order over its own feed and
the requester holds every row through a point in it: every writer's rows at
or below that point are then skippable whether or not the writer is named.
The nest has that order — `sync_changes.seq`, already stamped on every
served row (`SyncChange.seq`), and the ordered-prefix-per-writer law already
makes any `seq` prefix an ordered prefix of every writer's rows. A
store-served leg does not: `relay_rows` orders by `(writer, writer_seq)`,
and a store puts no cross-writer order on the wire. Hence one mechanism on
the nest leg and a stated deferral on the others:

- **Wire — additive both ways.** The request gains `held_through_seq`
  (optional `i64`): *"every row you hold with `seq` at or below this, from
  any device writer I did not name in `frontier`, I hold."* A named writer
  keeps its per-writer gate unchanged; an unnamed one is served from above
  the watermark instead of from 0; absent, today's semantics byte-for-byte.
  The reply gains `complete_through_seq` (optional `i64`): *"this page is
  complete through this `seq` — every live row at or below it that your
  request did not gate is in this page or an earlier one"* — the last served
  row's `seq` on a frame-truncated page, the scope's log tip on a full one
  (an empty page carries the tip, which is how a converged walk banks the
  whole log). The reply field is also the honour signal: an older nest
  echoes neither, so a requester that never sees it never projects. Neither
  field touches the shipped file-sync arm, whose scalar `since` is already
  this watermark for its one writer.
- **Serve — the nest.** The class-2 filter gains a third gate beside the
  per-writer slot and the nest slot: an unnamed device writer's row is seen
  iff `seq ≤ held_through_seq`. The reply stamps `complete_through_seq` from
  the page it actually cut, never from the page it computed.
- **Requester — the engine and the store.** The store keeps **one watermark
  per scope** — one, because a scope has exactly one sequencing nest (*Multi-
  writer fit*) — in meta, raised only from a `complete_through_seq` the
  sequencer echoed and only after the page it covers is applied; a
  reconcile-from-zero sends none, and banks what its pages echo. The stored
  `frontiers` table stays **complete**: it is the accounting law's record
  and the store-served legs' cursor, and its growth is one row per writer —
  linear, never the problem; the request blob and the custodian's per-page
  checkpoint were. What a walk *sends* is a **projection** of it: given an
  honest watermark any subset of the stored frontier is a correct request,
  so the request names only the writers whose naming still does work. The
  projection is re-taken **within** a walk too — from each page's echo,
  after that page's applies and before the spin and ceiling refusals — so a
  single page naming more writers than the ceiling (a first walk, a
  reconcile from zero) is paged by serve order instead of refused.
  **Built policy (2026-09-13; the ruling's recommendation refined):** a
  request names exactly the writers whose rows this store has *seen but not
  accounted* — a foreign writer whose relay-plane high-water sits above its
  stored slot, because a row of it was left unopened or unmergeable — at
  that stored slot. The nest therefore re-presents such a row exactly as a
  whole-frontier walk would, and a watermark banked past it never claims the
  store holds it. Every other writer is served from above the watermark; an
  own writer never counts as unaccounted, since a relay high-water above an
  own slot is the published-high-water lag, not an unheld row. The
  recommended refinement — also naming writers whose slot rose from a
  peer-learned row — was not built: its only effect is sparing the nest a
  one-time re-serve of a row a sibling delivered first, which the walk
  applies idempotently (the pump's reconcile re-serves every live entry each
  pass regardless), and knowing it would need a per-`(scope, writer)`
  raise-source record. Under the built policy a retired writer drops out of
  the request after one completed page, and the request never grows with
  history. Only the nest leg projects: the peer leg walks the very store the
  watermark lives in, and sends the whole frontier and never takes an echo.
  The custodian's nest-leg pull (`of_owner`) takes the same watermark under
  its own key in the custodied store's meta, names nobody once one is banked
  (a keyless custodian holds what its relay plane holds, and its cursor
  already moves past what it cannot hold), and keeps the shared pull cursor
  — the owner-device arm's — complete. **A bank this walk has not itself
  reconfirmed with an echo, and that goes unechoed on this walk's first
  reply, is UN-BANKED (2026-09-14):** the nest that banked it has
  stopped honouring the watermark — rolled back to a pre-watermark image —
  so the in-walk cursor falls back to naming the whole frontier for the rest
  of that walk (the older-nest shape, one full re-serve) and the persisted
  watermark is cleared (`AccountStore::clear_nest_watermark`, a second path
  beside `drop_scope`) rather than merely left un-raised, so the *next* walk
  reopens on the whole stored frontier instead of narrow against the same
  non-honouring nest. A bank this walk has already reconfirmed is never
  undone by a later reply that merely lacks an echo, or carries a negative
  one — those still mean nothing, as before. The same fallback fires on the
  replica rules of § The bind leg, ruling 2, and an empty page whose reply
  voided the bank is not convergence: the walk pages on with the whole
  frontier.
- **Ceiling.** `MAX_FRONTIER_WRITERS` stays, unchanged in value and
  placement: on a watermark-honouring nest the request frontier is bounded by
  writers *active since the watermark*, so an honest frontier no longer
  approaches it there; on the store-served legs it remains the guard it is
  today.
- **The store-served legs are DEFERRED, reason named.** The peer leg and the
  custodian's owner-device pull would need the same watermark taken in the
  store's own append order — a monotone modification counter on the relay
  plane and the journal, and a floor per `(scope, counterpart store)` — a
  second cursor family beside the frontier, which the one-cursor ordering
  model (§ Ordering model) deliberately does not have. On those legs an
  honest frontier reaches the ceiling only after a lifetime of successions
  on one scope, and the custodian's per-page checkpoint blob grows with the
  same count. That headroom is accepted; the build is owed only if the
  ceiling's sizing terms move, and it is not scheduled.
- **Inherited exposure, not a new one.** A watermark is a coordinate in the
  nest's log, exactly as the shipped scalar `since` is: a nest whose log is
  rolled back re-issues coordinates below a requester's anchor, and whatever
  answers that for `since` answers it for the watermark in the same stroke.
  Nothing here reads a sealed byte, deletes a stored row, or adds a knob.

**What stays accepted.** Neither bound is a clock, deliberately: the page loop
is runtime-agnostic — no `Send` bound, no timer named, so every requester impl
keeps inferring its own — and a peer request already defines the **caller** as
the one who races a deadline. So a
hostile counterpart may still spend a walk's whole page budget before the walk
returns; bounding *how long* that takes is the dialing caller's per-target
budget — which the sibling dial pass has always had and the custody dial pass
now has too. A counterpart that merely answers slowly, within budget, forever
is the same class as a nest that refuses to answer: weather, not a breach.

**Feed row + wire evolution (additive-everywhere, sketch — field names
finalize at impl time).** The generalized feed row extends the shipped
`sync_changes` row, never replaces it: existing columns keep their meaning
(`seq` = the nest-writer's log; `path_hash` = the opaque 32-byte item key —
for class-1 items the record CID's digest; `device_id` = today's
attribution column), joined by additive columns/fields for `item_class` —
the explicit discriminator: `chunk-manifest` | `direct-blob` | `record-cid`
| `state-entry` — `origin_writer` + `origin_seq` (the authoring writer's
log coordinates, for rows the nest relays from device writers), and the
class-2 kind key. The catch-up request gains an optional `frontier` beside
`since` (an omitted frontier *is* `{nest: since}`); the reply pages in
nest-log order with per-row origin coordinates, and a page is an ordered
prefix per writer (close early, never skip — the cross-nest relay's shipped
paging rule). Serving reserved scopes through this feed with the explicit
`item_class` supersedes the current infer-from-reservedness routing and the
sync plane's reserved-set refusal — nest-side requirement 2. The nudge
contract extends unchanged: every path that records a feed row owes the
best-effort nudge ([`file-sync.md`](../behavior/file-sync.md) owns nudge mechanics),
scope-tagged so a replica pulls only the scope that moved.

### Merge-policy seam

Every kind declares exactly one merge policy; the engine dispatches, the
policies are closed-set. Registration declares the kind's **audience rung**
in the same breath (§ The audience ladder owns what the rungs mean) — two
independent frozen columns, never one table doing both jobs:

| Policy | For | Shipped precedent | Admits a tombstone? |
|---|---|---|---|
| **Immutable** | class-1 records | block identity (CID) | no |
| **CRDT per-field / union** | irrecoverable class-2 (settings keys, seen-set) | `merge_user_configs` (retired 2026-10-02; its rules live on as the per-kind merges) | no |
| **Latest-wins** | recreatable class-2 | rail LWW ([`../behavior/reserved-folders.md`](../behavior/reserved-folders.md)) | yes (stamped) |
| **Three-way + loser retention** | text/file bodies | [`../behavior/conflicts.md`](../behavior/conflicts.md) | no |
| **Nest-CAS** | nest-arbitrated kinds | `__mls` CAS retry (and the `__config` CAS's, retired 2026-10-02) | yes |

**Tombstone admissibility is part of the policy contract, declared per policy
and enforced at the WRITER door** (ratified 2026-08-11; `MergePolicy::
admits_tombstone` + the plane's `write_local_and_publish`). A deletion is
orderable only where the policy already totally orders values — a stamp rank,
or nest order — so the two right-hand columns are one rule, not two lists. The
enforcement point is the writer because the reader has no cheap answer left:
the nest collapses per `(item_key, writer)`, so a published row its author
never supersedes is **permanent**, and the writing replica never notices (it
does not merge its own rows). A reader meeting one anyway — hostile device, a
build predating the door — **skips and counts it** (`WalkReport::unmergeable`)
rather than failing the walk: aborting would starve the replica of every other
writer's rows forever, which is a client-causable unrecoverable state
([`nest/common.md`](nest/common.md) § Client-state recoverability), while
skipping costs that one item and stays visible in every walk report. Per-item
or per-field deletion for a policy that does not admit one is a design slice to
be specified, never something inferred from an arm written for a different
question.

### Kind namespacing + third-party kinds (D3/A6 — ruled 2026-08-11)

**Moved verbatim to [`third-party-kinds.md`](third-party-kinds.md) § Kind namespacing + third-party kinds on 2026-10-02** (this doc stood nine days from the whole-file read ceiling), together with TP7's four sharpenings, when the S8a design pass added the six rulings a build needs beside them — the `ext.<publisher>.<name>` grammar, the manifest's wire form and signature, the closed `kinds` vocabulary, the `ext` sub-scope family, principal write authority and the record doors. One sentence stays here because this doc's registry owns it: **`fauna.*` is reserved for first-party kinds, which are compiled registrations; every other kind is an `ext.*` kind admitted by a signed manifest under that doc's rules.**

### Nudges and backstops

The proven lazy loop generalizes unchanged: best-effort change nudge
(`fauna.sync.changed` shape) + cursor catch-up walk + periodic rescan as
correctness backstop ([`../behavior/file-sync.md`](../behavior/file-sync.md)
owns the nudge mechanics). Every kind that lands on the plane owes the nudge;
the rails firing it is the cheapest first win — **taken 2026-08-11 for
`__config`** (W2.5 item 1b), whose put fired the nudge at the writer's own
devices until the rail retired 2026-10-02
([`config-dissolution.md`](config-dissolution.md) § The `__config`
dissolution schedule → *The closure order*, step (6)). The other rails still
do not push.

**The `__config` nudge reaches store-backed seats too (ruled + built
2026-09-25; its purpose retired 2026-10-01; history).** The untagged nudge
for the `__config` set had a second consumer, the account runtime's own push
arm, which mapped it to the account-state scope's walk
([`account-client-lifecycle.md`](account-client-lifecycle.md) § The client-side lifecycle,
the pump bullet's wake source (1), owns the arm), so that a marker-blind blob
write was imported onto the plane by the CAS-blob bridge at nudge latency;
the bridge was deleted 2026-10-01 (§ Substrate settlements → *The CAS-blob
bridge*). Nudge and mapping retired with the blob rail on 2026-10-02 (§ Implementation
status today; [`config-dissolution.md`](config-dissolution.md) § The
`__config` dissolution schedule → *The closure order*, step (6)).

### The bind leg — a bound nest is made complete (ruled 2026-09-30; rulings 1–4 built 2026-09-30, rulings 5, 6, 7 and 8 built 2026-10-01)

**The requirement** is [`nest/box-recovery.md`](nest/box-recovery.md) § The plane-era recovery floor, (a): **a bind completes the replica.** Whenever an account runtime is bound to a nest, that nest ends the first settled pass holding every live row of the account's two state scopes that the device holds and may vouch for, and an escrow wrap for every generation the device keys, receipted under the nest's current identity. Three cases: a second nest of the same account, a rebuilt nest (same identity, empty database), a rotated nest (same database, new identity). Nothing else returns these to a rebuilt box: read 2026-09-30, a backup destination carries the segment kinds and attached folders, never the state scopes, the escrow wraps or the device registrations ([`../behavior/backup-restore.md`](../behavior/backup-restore.md) owns what a backup holds), so the devices that outlived the box are the only source.

**The principle.** A device keeps five memories about "the nest", and the store is per account, not per nest: the published high-water of its own writer, the banked serve-order watermark, the serve coordinate each relay row was recorded at, the enrollment latch, and the belief that a receipted generation's wrap is held. Each was written against one nest replica and none names it, so each is silently wrong on any other. The rule that replaces them: **what can be asked of the bound nest is asked; what must be remembered is keyed by the replica it was learned from, and is void on any other.**

**1. Rows — the publish step diffs against what the nest holds.** Every pass already runs the full-state reconcile (backstop 2 of § Nudges and backstops), and its answer is every live `(item, writer)` row the nest holds. After it, the device compares that listing with its relay plane by `(writer, item)` and pushes through `fauna.account.state.put`, **verbatim**, every relay row whose pair the listing lacks or holds at a lower `writer_seq` — so a newer tombstone always follows a stale row an out-of-date replica pushed first. Verbatim because a row's coordinates are its identity (the seal's AAD binds them, § The class-2 entry form) and nobody but its writer can re-seal it; the door already stores any writer's row for the account without opening it. The nest leg thereby becomes as symmetric as the peer leg, whose serve is the same source: a row a sibling wrote reaches the nest through whichever replica holds it first. The journal-driven publish of a fresh local write is unchanged. Four rules ride with it. **(a) What may be pushed.** The device's own rows, always. A sibling's row only when this device opened it, the walk verified it, and its writer is a member in the device's verified device-set view. A row the device cannot open or verify is never pushed: a walk records whatever coordinate-valid row a nest serves, and the put door checks neither membership nor content, so an unverified relay would let one hostile nest use honest devices to plant rows on another under a sibling's writer id and wedge that sibling's real puts there — today a hostile nest can only damage its own leg, and that stays true. The cost, stated: a row only an absent sibling can open reaches a second nest when that sibling, or a member that keys it, binds there. And a row whose writer has left the fleet is never pushed: measured 2026-10-01 on the delegable scope, a removed device's row that alone carried a preference record reached no second nest. What carries such an item there is a member's hand-over row ([`delegable-scope-reclamation.md`](delegable-scope-reclamation.md) § Delegable-scope reclamation, part (3); ruled 2026-10-01, built 2026-10-02). **(b) A covered row is skipped, and so is a dead one.** A sibling's row the reclamation pass calls covered — its item's merged entry is carried at the tip's item key ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*) — is one its writer has retired or will; pushing it to a nest with no memory of that retirement would put a superseded generation back in use there. On the delegable scope the covered rows are the ones [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md) calls below a listed cover: none is pushed, whoever wrote it, and the relay copy is forgotten (ruled 2026-10-01, built 2026-10-02 — `publish_diff.rs`, `delegable_reclaim::listed_cover_above`). **A dead row (ruled 2026-09-30):** a row the reclamation pass forgets as dead from this replica's merged state alone — the set clause (3)(h) of the same section owns — is never pushed, whoever wrote it, by that pass's own predicate and no second spelling of it. Its writer retires it under the rule this replica has just applied, so no nest is owed it: one that remembers the retirement refuses the push, and one that does not would take a dead row back into its live count. The skipped set is exactly the forgotten set, so a sibling's copy the diff skips as dead is gone by the end of the same pass and none lingers. **(c) A refused push is final for that copy.** The nest refuses a coordinate it already holds or has retired, and a seq at or below its writer's head for the item; either way the pushing replica retires its copy, as it does today for its own refused put, and the next walk re-records whatever the nest holds live for that pair. That is also how a replica learns of a sibling's retirement nobody tells it about. No nest change is needed and none would help: the seal is randomized at every publish, so a writer publishing a row a sibling relayed first never sends the same bytes; that put is refused once and the walk's self-echo settles the writer's own slot. **What the learning path costs, and why it stays (ruled 2026-09-30).** A retire inserts nothing in the feed, so a replica holding a live sibling's row that its writer has since retired sees only an absence in the listing, and an absence reads the same on a nest that never held the row — the very nest this leg exists to complete. The refused put asks the one party that knows, and its cost is bounded: one put per retired row per member replica, once, because the refusal retires the copy. Measured on the 12-seat sweep (30 sign-out cycles, 720 passes): 1650 refused puts as first built, and 380 — about one per replica per cycle — once dead rows are skipped under (b), exactly what a probe that ran the pass's forget ahead of the diff had predicted. What is left is two classes a replica cannot call from its own merged state: a `Shredded` generation's mint row, which its writer retires and no sibling's pass forgets (280), and a sibling's superseded device-endpoints row the pushing replica could not yet call covered (100). **Refused: reading an absence at or below the listing's tip as a retirement**, by the serve coordinate the row was recorded at. A nest restored from a snapshot keeps its replica id and loses rows (ruling 2); once its log has regrown past the coordinate, the lost row reads as retired on every sibling for good, and unlike the watermark's residue no later pass repairs it — the row returns at best when its own writer next binds there, which is the recovery floor's own case failing silently. **Refused: a nest-side signal.** A refusal that tells "retired" from "held newer" saves no put. A reconcile reply that names retired coordinates grows with every row the scope ever retired — the nest keeps them all as its seq-reuse memory — or needs a second cursor over retirements, which have no place in the serve order. A batched probe of coordinates would be a kind both sides carry for the rest of the major, to shave a cost that is bounded and paid once. And the put stays right where an inference cannot be: a restored nest that lost the row stores it. **(d) "Published" is what this pass saw.** Every retire and shred that rests on a covering row requires that row in this pass's listing, or acked in this pass — never the writer's frontier slot alone, which says only that some nest once acked it. Reclamation is never skipped wholesale for an incomplete diff: at the entry cap a push is refused `scope_full`, and reclamation's retires are the only thing that frees headroom. **On the delegable scope a push refused for room does not end the diff (ruled 2026-10-01; built 2026-10-02, the departed-scope withhold 2026-10-03 — § Implementation status today).** On the fleet scope the first `scope_full` ends the diff for the pass, as built. On the delegable scope the journal-driven publish parks a row refused for room and goes on ([`account-replica-posture.md`](account-replica-posture.md) § The store device principal, refinement 11 → *A row refused for room is parked*), and the diff does the same in its own terms: after the first such refusal in a pass it withholds the pushes that need room, which are those whose pair the listing lacks and that name no listed row, and still sends the rest. A full scope so costs the diff one refused put a pass, and a row the nest has room for is never held behind one it has not. The diff also pushes no row of a departed scope's item ([`delegable-scope-reclamation.md`](delegable-scope-reclamation.md) § Delegable-scope reclamation, part (6), which owns that rule).

**2. The replica id — what is remembered is keyed.** A nest mints a random replica id with its database and returns it on the account-state feed reply (additive both ways; an older client ignores it, and a nest that sends none is read as changed at every assembly). A rebuilt box has a new id under an unchanged identity; a rotated box has the same id under a new identity. The store records the **settled replica** — the pinned identity and the replica id of the nest it last completed a pass against. *The serve-order watermark* (§ Feeds and cursors → *Compaction is a serve-order watermark*) is banked with the replica id and cleared when a reply names another, names none where one is stored, or echoes below the banked value: today the only reset signal is a reply with no echo, which a nest sends only while the scope is empty, so a non-empty second nest confirms a stale watermark by echoing its own tip and rows below it are skipped silently. *A relay row's serve coordinate* (`RelayRow::feed_seq`, which the reclamation pass reads against the gate's watermark — [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (1)) is a position in that same replica's log and is void whenever the watermark is (ruled 2026-09-30): every coordinate of the scope is cleared before a row of the new replica is recorded, and that replica's pages and put replies stamp afresh. A coordinate kept across the change withholds a retire against the new nest's gate by the old nest's numbering. *The enrollment latch* is void on a replica other than the settled one ([`account-replica-posture.md`](account-replica-posture.md) § The store device principal owns it). On a pass whose bound replica differs from the settled one the device clears the one and re-registers the other ahead of its other legs, and records the new settled replica only when that pass completes, so a verification cut short runs again. *The holdings belief* is not keyed at all — it is asked, at every assembly ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *A holder change re-receipts and never mints*) — because a nest restored from a snapshot keeps its replica id. **The residue, stated:** such a rollback is invisible to the watermark when the restored log has regrown past the banked value; rows skipped by a nudge walk for that reason are repaired by the next pass's full reconcile, which sends no watermark.

**3. The scopes stay per account.** A kind whose blob copy differed per box because `__config` was per nest is one row on the plane, unless what it holds is a fact about one box — then the row names that box in its key, and the scope is still the account's. Read 2026-09-30, `fauna.state.dns` is sound as one row — every field inside it is keyed by domain or zone, none names a nest — while the backup destination list is a per-box fact and is keyed by its source box (ruled 2026-09-30: [`../behavior/backup-destinations.md`](../behavior/backup-destinations.md) § State & data shape → *Destination data model*; not built — [`config-dissolution.md`](config-dissolution.md) § Implementation status today, the backup entry).

**4. Linked nests are secondary replicas, completed by the device (ruled 2026-09-30; built 2026-09-30).** Rulings 1 and 2 make whole the nest a runtime is *bound* to, and a device binds to one nest: read 2026-09-30, the account registry keeps one nest URL per identity, no app surface re-binds a signed-in identity, and sign-out erases the store. So a nest the user has linked ([`nest/private-mode.md`](nest/private-mode.md) § Pairing Flow) but never bound a device to would hold nothing. The ruling: **a linked nest whose pairing carries the `account_replica` capability is a secondary replica, and every seed-holding runtime completes it.** Beside its bound nest the runtime opens a second owner-authenticated connection to each such nest it can reach — the channel binding checked against the pairing row's nest id before any account data moves — and runs against it the legs it runs against the bound nest's state scopes, with **that nest's own listing as the evidence**: the full-state reconcile (its rows applied as any walk's are, opened and verified, since no nest is trusted for content), the publish diff of ruling 1, the escrow deposit and holdings check, and reclamation's retires. It runs in every full pass — the first after assembly, then the backstop's (and a reconnect's) — never per nudge; it sends no watermark and needs no device principal, because every door it uses admits the authenticated account. Sealing is unchanged: a tip is acked by the bound nest's receipt alone. **Retires follow the rows.** A retire issued against the bound nest is issued against each linked nest whose listing shows that row live — by the cell, where the two nests hold the writer's row for the item at different coordinates (ruling 6) — so each writer retires its own rows wherever they rest and a secondary converges downward as well as upward; the one case in which a member retires rows it did not write is a generation merged state reads `Shredded` ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, the second-holder ruling). Two devices bound to different linked nests converge through either one. **Refused: nest-to-nest replication** over the pairing channel — the custodian pull leg repointed at the user's own account. A nest cannot author a plane row, so the receipt its deposit earns could never reach merged state; it cannot open or verify what it would relay, which is ruling 1(a)'s hostile-nest exposure with no device in the path to stop it; the channel is dialed one way, so it needs new federation kinds in both directions; and nothing in it carries a retire or a shred to the other side. **Refused: an app surface that re-binds a device to each of the user's nests in turn** — it moves the primary at every switch, costs a new surface on seven apps, and leaves the other nests stale between visits. **The stated bounds.** A secondary is as fresh as the last time a seed-holding device could reach it; a home box reachable only on its own network is completed when a device is at home. A seedless host runs no secondary leg. What the linked nest learns is the custody floor — scope, writer ids, blinded item keys, sizes and timing — and nothing else. Unlinking ends the replica: the unlink deletes the account's escrow wraps at that nest when it can reach it, after which the sealed rows left there open for no one who lacks a device's keys; a nest that could not be reached at unlink, or one the account simply stopped using, keeps what it was given until the account is deleted there.

**5. The secondary leg runs wherever a seed holder runs, and its retire evidence rests in the store (ruled 2026-10-01; built 2026-10-01).** Ruling 4 made the leg a step of the pass, and a pass runs only in the process that holds the engine role — on a desktop, ordinarily the seedless sync agent, which runs no secondary leg. So with the agent up, no process ran it: measured 2026-10-01, the app beside the agent ran no pass and its linked nest stayed empty and uncustodied. Which process runs a leg that needs the seed is [`account-runtime.md`](account-runtime.md) § Multi-instance concurrency's to say — the seed-leg role: the leg, and the custody arm riding it, run in the co-located seed holder that holds that role, inside its pass when it also pumps and in its seed pass when the agent does. Ruling 4's bounds are unchanged: a seedless host runs no secondary leg, and a secondary is as fresh as the last time a seed-holding device could reach it. One thing in the leg read a memory only the pumping process had. *Retires follow the rows* re-issues the retires the bound planes sent, and those were kept per plane, in memory, for one pass. **The retire record moves into the store.** Every retire a bound plane sends is recorded there by its exact coordinates, its belt and the nest's answer, by whichever process sent it. The leg reads the record, re-issues at each linked nest it reaches every entry whose row that nest's listing shows live (at which coordinates is ruling 6's to say), and clears what it read when its run ends, whether or not every linked nest was reached: one leg run deep, as the pass's record was one pass deep. There is one path — a runtime that holds both roles records and drains through the store too. The record is derived state, bounded by a constant: a machine whose agent pumps for weeks with no app running keeps the newest entries and drops the rest. What losing an entry costs is ruling 6's to say — a round for a row reclamation still names, a redundant row for one it does not — and a shred never depends on the record, because the shred half reads merged state. Everything else in the leg already reads the store and that nest's own listing.

**6. A retire follows the cell, and a lost entry is re-made (ruled + built 2026-10-01).** A retire names a row by the coordinates the *bound* nest held it at, and two nests need not hold a writer's row for an item at the same coordinate: a nest collapses a writer's rows per item, so the bound nest may have retired a row a linked nest never saw, over one that nest still serves. Measured 2026-10-01 on a sign-out: the departing device writes its `Removed` row over its own `Enrolled` row — same item, same writer — the bound nest collapses the enrollment under it, and the pumping pass that walks the removal in retires it there and forgets its relay row in that same pass, no enrollment being left at that nest to wait for ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (3)(d)). The linked nest holds the enrollment, at its own lower coordinate; the record names the removal row's; and no diff carries the removal row across, because the diff never pushes a departed writer's row (ruling 1(a)) and the copy is forgotten. Mirrored by exact coordinates, the leg left the linked nest serving an enrollment with no removal beside it, and a reader of that nest alone folded the signed-out device a member. **The rule: a retire follows the cell.** The leg re-issues a recorded retire of a writer's row for an item at seq N at each linked nest whose listing shows that writer's row for that item at N **or below**, at the seq that nest lists; a row listed above N is a newer word and stays. A lower seq in the same cell is, by the collapse rule, a row the retired one superseded, so the retire leaves that nest's cell as it left the bound nest's — what the diff followed by an exact retire would have left — behind that nest's own gate and the same belt. One retire per cell, however many entries name it. **A lost entry is re-made: it costs a round.** The record is one leg run deep and capped, so entries are lost — routinely, on a device that runs its leg away from a home box. The next leg run that reaches the nest has nothing to re-issue for such a row, but its reconcile records the row that nest still lists in the relay plane; the pumping pass's reclamation then names it again under the rule that retired it and asks the bound nest, which answers that it is gone; that answer is recorded like any other, at the coordinates the secondary holds; and the leg run after it re-issues it. So a secondary converges downward within two leg runs that reach it with a pumping pass between them, record or no record, for every row the pass's reclamation names again — proven for a removed device's rows, which it names for as long as the removal stays in merged state, and that is for good; the path is the same for any row its rules name from the relay plane. A row it no longer names stays, redundant, its generation held in use at that holder. Until the round completes, a reader of that nest alone reads what the bound nest served before the retire, as it does of any secondary no leg has reached since. **Refused: holding the forget until a leg has run.** Every relay row a pass retires would then rest in the plane until a leg ran — for good on a machine whose pumping process runs none (the seedless agent, with no app opened beside it) — where the diff and the peer serve would each have to tell it from a live row, for a hole the cell rule closes with no state at all. **Refused: pushing the removal row from a kept copy** — the same residue, and an exception to ruling 1(a) for a departed writer's row. (Ruling 7 adopts the push and the exception without the residue: there the row is not retired at the bound nest until it has been carried, so the copy that is pushed is an ordinary live row.) **Left to ruling 7: a removed-device arm that reads merged state**, as the shred half does. It is sound — `Removed` is absorbing in merged state as `Shredded` is — and needs no record; the round above converges without it, and it is ruled where the wider question is, of how a removal reaches the devices *bound* to a linked nest.

**7. Removal evidence leaves a nest only once every replica holds it, and the leg run that carried it is what retires it (ruled 2026-10-01; built 2026-10-01).** A removal travels as a row — a departing device's own `Removed` row, written over its enrollment at sign-out, or a member's `Removed` row for a device that was lost — and a replica reads a device removed only by reading one: `Removed` is absorbing in merged state, but nothing else puts it there. **Measured 2026-10-01** on one account with two nests each linked at the other, two devices bound to A and one to B: one of the two at A signs out, or is removed by its sibling; the sibling's passes run, then both remaining devices' in turn; and the device bound to B still counts the departed device a verified member after six passes of its own, each with a leg run that completed A — while a reader holding only the seed reads it gone from either nest. Four things combined, as the code then stood. The reclamation pass retired the removal row at the bound nest and forgot its relay row in the pass that walked it in, behind the enrollment ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (3)(d)) and ahead of the leg, so the leg's diff had nothing to push. The diff would not have pushed a departing device's own row anyway (ruling 1(a)). The gate that licenses a retire counts the walkers bound to that nest, and a device bound to B is none of them — its leg at A leaves no mark. And ruling 6's retire at the linked nest inserts nothing in that nest's feed. So the device bound to B was served the removal only if its own leg reached A in the moments A still served it. What that cost: the device wrapped every later generation to the departed one, admitted it wherever its fleet view admits a member, and never found every member holding the tip, so at its view nothing was reclaimable and no generation shredded — for a lost device, a removal that did not take. **The rule: removal evidence is retired at a nest only by a leg run that found it at every linked replica of that nest.** *Removal evidence* is a row in a device-set item that opens as `Removed`, for a device this replica's merged state reads removed. Three parts. **(a) The reclamation pass retires none.** Clause (3)(d)'s enrollments, reach and device-scoped rows go as they do today; the evidence stays live at the bound nest and in the relay plane — an ordinary live row to every walk, diff and peer serve, with nothing to tell apart. **(b) The diff carries it.** A member's removal row for another device is that member's own row and goes under ruling 1(a) as written. A departing device's own removal row is the one exception to 1(a)'s member test: a row this device opened and the walk verified, resting in its writer's own device-set cell and reading `Removed` by that writer, is pushed although its writer is no longer a member in this device's view — the row is the evidence of exactly that. Ruling 1(a)'s exposure does not arise: the row is opened and verified, never relayed blind, and its writer has no later put for it to wedge — it is that writer's last word. At a nest that holds the enrollment in the same cell the push collapses it, as the sign-out did at the bound nest. **(c) The leg run retires it — the removed-device arm.** A run of the secondary leg that reached every linked replica its bound nest lists — none listed is every one reached — ends with this arm. For each evidence row the relay plane held when the run began: it is *carried* when the bound nest has served it — a row this replica knows only from a linked nest's feed is first pushed to the bound nest by the pass's diff — and every replica's listing, after that run's reconcile and diff, shows that writer's row for the item at that coordinate, or the replica refused the push for good, which says it held the row and has retired it. A carried row is retired at the bound nest and, in the same run, at each replica that lists it — each behind that nest's own gate, so each nest keeps it until the walkers bound there have read it — and its relay row is forgotten once the bound nest answers that it is retired or gone. The arm reads merged state and the run's own listings, as the shred half does, and needs no record. A linked nest that itself lists a replica the run did not reach keeps the row for a run that reaches both. A run that missed a replica retires no evidence anywhere, and neither does a process that runs no leg. **Why it holds.** A device is bound to one nest and walks that nest's feed. Evidence leaves a nest only behind that nest's gate — every walker bound there has read it — and only after each replica that nest lists held it; so, taking the retires of one row in the order they happen, every nest another names as its replica has had the row in its feed and keeps it until its own walkers have read it, and every device reads the removal from the nest it is bound to. And a nest goes on serving the row until some run has carried it, so a device's own leg reads it there meanwhile: two devices bound to different linked nests converge through either one (ruling 4) for a removal too, also when only one of them can reach the other's nest. **What it costs, and its bounds, stated.** One row per removal rests at each nest until a seed-leg holder bound there completes a leg run: on a machine whose pumping process runs no leg, until an app is opened beside it or another device bound to that nest runs; behind a replica no device bound here can reach, until one can or the user unlinks it — unlinking is the control, as removing a device is for a stranded walker. A row live at one nest and retired at the other costs the refused put of ruling 1(c) per leg run until the second gate opens. The guarantee runs along the pairing rows: a nest linked from one end only ([`nest/private-mode.md`](nest/private-mode.md) § Pairing Flow, the single-end fallback) is carried to in the direction its row names, and a device bound to the end no row names reads a removal written at the other only through its own leg, while the row is still served there. An older build retires the evidence in its pass as before, so the guarantee holds for a nest once every device bound to it runs this rule; nothing changes on the wire or at rest. **What it changes in ruling 6.** The sign-out measured there no longer takes that shape — the removal row is not retired ahead of the leg, and the push in (b) collapses the enrollment at the linked nest — and the cell rule and the re-made entry stay for every cell two nests hold at different coordinates. **Refused: a member's removal row written on demand**, wherever a leg finds a nest listing an enrollment with no removal beside it. It reads the need off a listing, and an enrollment retired there without evidence — ruling 6's retire, today — shows none; it needs a device that knows of the removal to reach that nest, so a device whose own leg reaches the origin learns nothing once the origin has reclaimed; and it writes a row per removal per nest where carrying the one that exists writes none. **Refused: a walker mark left by the leg**, so that the origin's gate waits for a device bound elsewhere. A mark counts for a key the nest holds a live grant for ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (1)), so every device would register at every linked nest, where today a leg needs no device principal (ruling 4), and every removal would have to reach device rows on nests the remover is not bound to; and it still puts nothing in the replica's own feed, for the walkers there that run no leg.

**8. A removed member's grant is revoked at every linked replica, ahead of the leg's retires (measured red + ruled 2026-10-01; built 2026-10-01).** Every retire a leg issues waits behind the gate of the nest it is issued at (rulings 4, 6 and 7), and a gate counts a walker until that nest's grant for its key is gone. The removal's nest half reached only the nest the remover is bound to. Measured 2026-10-01: a lost device bound to B, removed from a device bound to A, kept its grant and its counted mark at B, so everything above that mark waited there for good — ruling 7's evidence included. The rule, its reasons, its bounds and its two refusals are [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (4) → *The nest half follows merged state*: a runtime revokes, by key, the grant of every roster row whose principal its merged state reads removed, at every nest it completes. The leg's part is one step per linked nest, after that nest's reconcile — merged state then holds whatever removal that nest served — and before its retires, so the run that revokes is the run whose retires land. Like the rest of the leg it needs no device principal: the revoke is a door that admits the authenticated account.

**Proof.** Three cases against real nests, each ending with a reader that holds only the identity seed: a device custodies a row on nest A, binds to nest B and settles, A is lost, and the reader recovers the row from B with no generation minted by the rebind; A's database is replaced by an empty one under the same identity, the device's next pass makes it whole, the device is lost, and the reader recovers from the rebuilt A; A rotates its identity, the running device keeps sealing and escrowing with no restart, and a fresh device that pins the successor recovers a generation still receipted only under the predecessor. Ruling 4 adds two: a device bound to A with B linked never binds to B, A and the device are lost, and the reader recovers from B; and a generation shredded on A is dataless and wrap-free on B after the next secondary leg. Ruling 6 adds two, each ending with that reader folding the fleet from B alone: a device signs out at A, the pumping passes retire its rows there, and one leg run later B serves no enrollment of it — since ruling 7 by that run's push of the removal row, which it then retires at both nests; and the same with B away while the record is spent — the departed device's reach and device-scoped rows, the cells the cell rule still owns, leave B a round after it returns. Ruling 7 adds three, on two nests each linked at the other with two devices bound to A and one to B, each ending with the device bound to B reading the departed device removed and with neither nest serving a device-set row of it: one of the two at A signs out; the same while its sibling cannot reach B, read through the far device's own leg at A; and a lost device is removed by its sibling at A.

### Substrate settlements

- **The rails ride the generalized feed.** The deferred
  channel-vs-`fauna.sync.changes` substrate decision
  ([`../behavior/reserved-folders.md`](../behavior/reserved-folders.md)
  § UserConfig Sync; [`../behavior/devices.md`](../behavior/devices.md)
  § Implementation status) is **settled (2026-08-10): the generalized account
  feed is the substrate direction** for device-sync of account data; the
  rails' deliberately-thin CAS layer swaps onto it at W2. The dormant
  `DeviceSyncChannel` stays dormant (its MLS bootstrap circularity stands,
  devices.md's 2026-07-05 ruling; a future live device-event channel remains
  orthogonal). **R13 (2026-08-11) refines the target shape:** `__config`
  dissolves into per-kind entries at declared audience rungs (§ The
  audience ladder — the disposition table is the split line); the swap's
  first slice admits the delegable preference cluster, operational secrets
  follow at the fleet-only rung, and the dissolution's migration
  sequencing was then unscoped (user-deferred); it was scoped 2026-08-12
  and completed when the CAS rail retired 2026-10-02
  ([`config-dissolution.md`](config-dissolution.md) § The `__config`
  dissolution schedule → *The closure order*, step (6)).
- **The CAS-blob bridge (W2.5 item 1, built 2026-08-11; DELETED 2026-10-01 —
  history).** While an admitted preference kind had a copy on two rails —
  the `__config` blob, written whole over CAS, and a per-kind plane entry —
  the bridge kept the two one value: a shared anchor in the blob
  (`UserConfig::plane_mirrors`, one marker per kind naming the plane stamp
  and the value hash the blob's copy last agreed with) let any bridging
  device read the direction of a divergence from data, mirroring a plane
  move into the blob and importing a marker-blind blob write onto the plane.
  It is deleted at closure step (5) of the dissolution schedule: the plane
  is the cluster's only home, `plane_mirrors` and its merge arm are gone,
  and what carries the two cases the bridge used to carry is
  [`config-dissolution.md`](config-dissolution.md) § The `__config`
  dissolution schedule → *What replaces the bridge's two carriages* (its
  § Implementation status today owns the deletion's record). Two rules the
  bridge stated still hold for the cluster on the plane alone
  (`fauna_sync_engine::preference_put`): the plane entry's value is the
  sub-record's canonical dag-cbor, and a tombstone on a preference kind is
  refused — "cleared" is a default *value*.
- **The plane does not ride ECS** (settles survey Q11, refutable). The
  audit-gated ECS CRDT (`fauna.spaces.*`) is a *shared-space* substrate with
  its own audit/consent semantics; the account plane is single-principal
  (one account's devices) and needs none of that machinery. They coexist;
  revisit only if W2's merge seam proves insufficient for some kind.
- **File-sync's convergence onto scope families is affirmed as direction,
  and stays deliberately unruled at W2** (D4, 2026-08-11): the greenfield
  derivation confirms the `folder` family is the same substrate (content
  = blobs, path state = entries, engine = projections), and the ruling
  stays exactly where the charter left it — direction only, taken up when
  the plane has proven itself at W6+, never as a side effect of a W2
  slice.

## The peer leg — device↔device sync over iroh (requirement 7)

**App↔app sync is as natural as app↔nest** (user directive, 2026-08-10):
apps never *have to* sync directly, but they always *can*, with no mode
switch — the sync protocol is peer-symmetric, and the nest is simply the
peer that happens to be always-on and durable. A nest or internet outage is
therefore not a special mode; it is merely the situation in which the peer
leg is the only reachable peer set. And "app↔app" means precisely
**store↔store** (user clarification, same directive): the syncing parties
are per-account store replicas — the store on one device converging with the
store on another, one peer endpoint per replica (= per device principal,
R5), never app processes; whichever co-located process holds the
engine-singleton role (§ Multi-instance concurrency) speaks for its store. The peer leg is **the same sync contract
over a different transport** — one protocol, N transports — riding the existing seam
(`fauna-transport` / `fauna_iroh::IrohTransport` / `fauna-peer-channel`'s Y.1
`PeerChannel`, owner [`../behavior/p2p.md`](../behavior/p2p.md)); it is a new
*consumer* of that seam, not a new transport.

- **Identity (settles the identity half of survey Q14).** The peer-plane
  node identity of a replica is its **device principal's keypair** (R5) —
  *not* the actor key. The contact-plane rule "iroh `NodeId` is the Ed25519
  actor key" (PT-1b, [`../behavior/p2p.md`](../behavior/p2p.md)) cannot serve
  same-account devices (they would all share one NodeId); p2p.md carries the
  carve-out pointer.
- **Authorization.** Mutual proof of same-account membership: each side
  presents its root-signed `DeviceAuthorization` covering its device key
  (mechanics owner [`../behavior/devices.md`](../behavior/devices.md)) over
  the established QUIC channel; removing a device (its row in the devices UI)
  severs its peer-plane admission — *Validity and severance* below owns the
  bound and the mechanism. This is the first
  of **three witnesses** the same admission core accepts — M2 membership
  (the cross-account twin below) and the custody grant (§ Replica posture)
  are the other two; the engine is one, the witness swaps.
- **Scope.** What converges peer-wise without the nest: class-1 records,
  class-3 blobs, commutative and LWW class-2 state, and the seen-set.
  **Nest-arbitrated kinds do not**: they travel as outbox intents (gossip) at
  most, applied only when a nest applies them. Class-5 never rides the peer
  leg (MLS state syncs only via its own plane, which is nest-CAS by design).
- **Discovery — the replica is its own peer registry (T5 — resolved
  2026-08-10, refutable until W2 code).** The transport seam deliberately
  configures no discovery substrate (no third-party relay/DNS/mDNS — the
  iroh endpoint builds on an empty preset; dial candidates are
  caller-supplied, [`../behavior/p2p.md`](../behavior/p2p.md)). The account
  plane supplies them **from itself**: each replica publishes a
  **device-endpoint entry** — its device principal's public key (= its
  NodeId), current LAN + last-known public addresses, and the relay URL its
  nest advertises — as a class-2 LWW state entry on the account-state
  family's fleet-only sibling scope (`state-fleet` — the kind's rung routes
  it there, § The audience ladder → *The device-endpoints rung*), so every
  replica learns its siblings' dial candidates *from the replica*, offline
  included (the entries synced while connectivity lasted). The plane-published entry also carries the device's own statement of the nest row it enrolled on — not a dial candidate, and never on a carried copy; owner [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (4). **An entry is a dial candidate only while its device is a verified member of this replica's fleet view** (ruled 2026-10-02) — enrolled under the account root, not removed (owner of membership: [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery, the device-set bullet). Nothing forgets a merged entry, so the entry of a removed device, of a signed-out device whose enrollment row has retired, and of every predecessor device after a succession (enrolled under a retired root, which the view never verifies) outlives its device and stays in merged state; the dial pass skips it rather than spend a failing dial on it every pass and show this device's addresses to whatever answers at that node id. The admission seam would refuse such a peer after the dial; this rule refuses it before. Forgetting the entry instead was rejected: only the replica that sends the retire would forget it. The accepted cost: a sibling whose device-set row this replica has not merged yet is not dialed until it has — both rows ride the fleet-only scope and reach a replica from the nest together, and peer-only cold bootstrap is a declared non-goal (below). The owner-side custodian dials are not sibling candidates and are not filtered by membership (a custodian is no fleet member). LAN candidacy applies the shipped arithmetic (RFC-1918 + shared
  /24, [`../behavior/p2p.md`](../behavior/p2p.md) § LAN detection) to those
  cached endpoints — no broadcast, no scanning. A fresh replica learns its
  first peers via the nest during enrollment; **peer-only cold bootstrap is
  a declared non-goal** (enrollment already requires reaching the account —
  § The store device principal).
- **The relay: NAT rendezvous assist — never a data middleman, never
  load-bearing.** The self-hosted relay sidecar
  ([`../behavior/p2p.md`](../behavior/p2p.md) owns it: runtime-gated,
  advertised as `NestInfoReply.iroh_relay_url`) serves the peer leg exactly
  as it serves the contact plane: hole-punch rendezvous and relayed
  transport of end-to-end-encrypted QUIC when no direct path exists (as
  built, two devices behind two NATs never get one — measured, p2p.md
  § NAT hole punching; the ruling that the relay serves address discovery
  once measured to help, and that the leg's bytes ride direct paths only
  while the nest path stands, is p2p.md § The relay → *Address discovery,
  and what rides a relayed path*)
  ("never a data middleman" means never a party to the data, never that no
  traffic passes through it — p2p.md § The relay, which also rules who runs
  the relay and who may use it: the devices of the nest's own members, and
  nobody whose account lives on another nest, so every pair this leg's
  relay serves is a pair on one nest). The
  outage matrix: same LAN with the WAN down → direct paths work, the relay
  is unreachable *and unneeded*; different networks with the nest *process*
  down but its box up → a relayed connection that already stood keeps
  bridging, a new one is refused (the relay asks the nest about every
  endpoint that connects — p2p.md § The relay → *Who may use it*); box
  fully down → devices on different networks have no rendezvous, and the
  peer leg does not pretend otherwise — best-effort is the contract, and
  the always-on durable peer is the availability answer. The advertised
  relay URL reaches the endpoint builder since the dial pass
  (2026-08-15): `PeerLegFactoryInputs` carries it — own-nest provenance by
  construction — into the shared factory body both tui and the sync agent
  wrap (§ Implementation status today → *Built — the peer-leg dial pass*).
- **Web: declared absence at W2.** A browser origin cannot run the QUIC
  seam, so web replicas converge nest-mediated only — mirroring web's other
  declared platform absences. The lift is additive by construction: the
  peer leg is a *consumer* of the transport seam ("one protocol, N
  transports"), so a later browser transport (a relay-WebSocket or WebRTC
  impl of the seam) removes the absence without touching this sync
  contract.
- **The cross-account twin (2026-08-10).** The same machinery — seam
  consumer, want-list chunk pull, multi-source over convergent sealed bytes
  — serves the *contact-plane* case: offline p2p transfer of a shared file
  set between two **different users'** devices (the "holiday videos between
  spouses" scenario). That contract is owned by
  [`../behavior/p2p.md`](../behavior/p2p.md) § Cross-user shared-set
  transfer (admission = M2 group membership by actor key, not
  `DeviceAuthorization`; two members whose accounts live on different nests
  have no relay between them — p2p.md § The relay → *Across nests*); W2's engine work should keep the pull/serve core
  admission-agnostic so both legs — and the custody leg's third witness
  (§ Replica posture) — share it. The listener-hardening rules for
  *both* legs are owned by [`../behavior/p2p.md`](../behavior/p2p.md)
  § Wormability posture — the peer leg adds the same listener surface and
  must satisfy the same eight rules (per-rule compliance record:
  § Wormability walk below; the share leg's own record + build contract:
  p2p.md § Cross-user shared-set transfer, first design pass 2026-08-17).

### The admission seam — one engine, witness-shaped (W2.6 gate 1; ratified 2026-08-11, refutable until W2.6 code)

The shape that keeps "three witnesses, one engine" (§ Replica posture) true
in code, ruled once for all three consumers:

- **The core consumes a verdict, never a witness.** The pull/serve core's
  entire admission contract is a per-connection **verdict**: `(account,
  admitted scope set, validity bound)`. Witness evaluation lives outside the
  core, one verifier per witness kind, each owned where its mechanics live
  ([`../behavior/devices.md`](../behavior/devices.md) for
  `DeviceAuthorization`; [`../behavior/p2p.md`](../behavior/p2p.md)
  § Cross-user shared-set transfer for M2 membership; T13 for the custody
  grant). The core's only admission duty is scope enforcement: every pull or
  serve checks its scope against the verdict's set. A witness kind the core
  can name is the redesign this ruling exists to prevent.
- **A witness admits a proven key, never a bearer.** Every witness kind must
  bind to the channel-proven identity (`PeerConn::peer_identity` — the
  completed-handshake rule PT-1/PT-1b): same-account admission requires the
  root-signed `DeviceAuthorization` whose `device_key` *is* the proven
  NodeId → verdict scope set = all scopes of the named account; M2
  membership is checked against the proven actor key → exactly the shared
  set's scope; a custody grant must name the custodian's proven key and its
  scopes (shape ruled at § Replica posture → The custody grant + ceremony —
  the seam requires key-binding and named scopes, nothing more) → the named
  scopes. Possession of a witness envelope alone conveys nothing.
- **Sides admit independently, and their witness kinds may differ.** Each
  endpoint holds its own verdict for the remote before serving or accepting
  anything beyond `fauna.peer.node_info` and the admission exchange itself;
  an own device syncing against a friend's custodian presents a
  `DeviceAuthorization` and receives a custody grant. "Mutual" (the
  Authorization bullet above) means both directions independently admitted,
  never witness-kind symmetry.
- **Carriage is inline and self-contained.** The witness envelope travels
  in the admission exchange; a peer listener never depends on a registry
  lookup — the same across-trust-boundary rule
  [`../behavior/devices.md`](../behavior/devices.md) § Device-signed
  authoring already sets for authoring chains.
- **Admission is carrier-level, never writer-level.** A verdict admits a
  replica to *transport* the account's plane within its scopes. Row-level
  writer authentication stays where it is: T14's AAD binding + the
  journal's accounting (equivocation refusal), with verbatim relay of other
  writers' rows correct under R6. The `capabilities` vec on
  `DeviceAuthorization` is the *authoring* axis
  ([`../behavior/devices.md`](../behavior/devices.md) § Device-signed
  authoring) and is not consulted for sync admission — a
  `[RenewBearer]`-only sync-agent device is an admitted carrier. The peer
  leg is thereby **not an authoring-acceptance surface** (journal-row
  writer auth is this seam + T14, per that section's own carve-out), so
  its revocation bullet's direct-peer-ingest revisit stays owed to any
  future surface that accepts *authoring chains* peer-wise, not to W2.6
  (the storage-group plane is the first such surface, and that bullet now
  records its re-visit).
- **Validity and severance.** Verdict lifetime ≤ min(connection lifetime,
  witness expiry); a connection outliving its witness's `expires_at`
  re-presents before continuing past it. Revocation severs at the next
  admission evaluation (the ratified next-handshake bound); content
  severance on removal is per key-custody class (§ Wormability walk,
  rule 6). **A withdrawal is never carried by the witness.** Every witness is
  self-contained — and the fleet `DeviceAuthorization` carries no expiry at
  all, removal being its only control — so *whether the account has since
  withdrawn this one* is the evaluating side's own question, answered beside
  the verifier from its own merged state: the custody-grant arm from the
  synced grant-event log, the `DeviceAuthorization` arm from the **served
  account's merged `fauna.state.device-set` exclusion set**. Three properties
  bind both arms. **(a) Per request, not per connection** — a replica that
  learns of the withdrawal severs a connection already holding a verdict at
  its next request, not only at a next handshake the removed device need
  never offer. **When it learns is the bound.** Each arm's evaluating side
  reads a *withdrawal snapshot* it re-derives from its own merged state once
  per pump pass, right after the fleet walk and before the ensure step, so a
  withdrawal severs at the first request after the first pass that follows
  its record reaching this replica: at worst one backstop interval
  (`DEFAULT_BACKSTOP_INTERVAL`, a Rust constant — 300 s today) past that
  record's own sync, sooner on any event-driven pass. A snapshot that has
  **never derived** — before its first successful refresh, or while every
  refresh so far has failed — refuses every witness of its arm, so no bound
  serve side or dialer ever answers from an empty set that would admit
  every withdrawn one; the next successful refresh reaches already-bound
  servers and live connections in place, with no rebind. A refresh that
  **fails after one succeeded** keeps the last derived set in force: a
  withdrawal merged since then waits for the next success, for as long as
  that local read keeps failing (surfaced as the pass's step error).
  **(b) Both directions** — the dialer applies its own view to
  the responder's reply witness, so a sibling never walks a removed device's
  relay plane either. **(c) Refuse on the withdrawal record only** — for
  devices, a `Removed` row; an *absent* row admits, which is what keeps the
  check additive across version skew (a device enrolled before the
  plane-side device-set existed has no row at all). **Residual, stated
  rather than silently held:** a custodian evaluating an owner device's
  `DeviceAuthorization` for a custodied account cannot read that account's
  sealed `state-fleet` rows, so the plane gives it no exclusion set for that
  account, and a removed owner device that no held grant lists (a grant
  minted before the removal, or by an owner build that predates the list)
  is still admitted **at the custodian**. Failing closed there would sever
  custody serving wholesale (exactly as a missing custody-revocation view
  does, deliberately); the owner's own siblings, which hold the rows, sever
  at their next evaluation. **The cut from the plane alone is not
  durable, and the projection that narrows it is ruled (2026-09-26).** A custodian never learns
  a removal from the plane, and the fleet `DeviceAuthorization` never
  expires, so revoking the account's custody grants cuts this channel only
  while none is outstanding: absent the grant's own list, any custodian
  granted afterwards would admit the removed device again from its first
  pass — the rule-6(b) reader who concludes that revoking custody suffices
  is right only through the re-grant the list rides on.
  **Refused as the narrowing: an admitted owner device carrying the
  account's removals to the custodian** (additively on the admission
  exchange, or by any channel an admitted device authors). The removed
  device is itself an admitted owner device there — that is the residual —
  so such a channel is one it can use first, naming the honest fleet as
  removed at every custodian it reaches; the custodian holds no anchor to
  arbitrate (the plane's `Removed` signal is unverifiable by design, rule
  (c)), and the result turns a generation-0 confidentiality residual into
  an availability attack on the recovery channel by the same adversary.
  **The one sound projection is the grant itself:** `CustodyGrant` carries
  the owner's removed-device exclusion list, minted from the minting
  device's verified fleet view and owner-signed with the rest of the
  witness (shape and rules:
  [`account-replica-posture.md`](account-replica-posture.md) § The custody
  grant + ceremony → the witness bullet). The custodian's `device_removed`
  view for a custodied account answers from the lists of the grants it
  holds for that account, so a grant minted after a removal excludes that
  device for its whole life, in both directions, at the admit door and at
  the next request; a grant minted before it admits the device until it
  expires (the ~90-day window) or is revoked and re-minted. A seedless
  removed replica — the modal one — cannot forge a grant; a seed-holder
  can, and is succession's case (rule 6(b)) as before. What the list tells
  the custodian is which of the writer coordinates it already holds in
  the clear are retired — nothing it could not otherwise infer.
  Built: the custodian derives one exclusion
  map per custodied account — the union of the lists on every grant it
  holds for that account, each witness signature-verified and keyed by its
  signer — each pump pass from its own `custodies-held` rows, and reads it
  live at its admit door, at every request on a live connection, and on
  its own custody dial to the owner fleet; an account whose held grants
  list nothing admits as before. **Its
  reach:** the removed device is served what the custodian holds of the
  account — the entries and blocks of every scope the custody covers,
  sealed exactly as the owner's devices sealed them, beside their relay
  floor in the clear (scope, item class, writer coordinates, the item key —
  blinded for an entry, the record's CID for content — op, size) — and it
  can open exactly what its retained keys open (§ Wormability walk,
  rule 6): nothing sealed to a generation minted after its removal
  (rule 6(c)), but, for a device that kept `BackupKey`, every entry of a
  kind sealed at generation 0, post-removal entries included (rule 6(b)) —
  today the delegable preference cluster and the generation machinery's own
  records (device set, mints, wraps, escrow, reach; the `SealingEpoch::Gen0`
  rows of `fauna_protocol::merge_policy`'s registry). For those kinds this
  residual is the custody channel among the ciphertext channels rule 6(b)
  enumerates, and removal alone does not cut it — nor does revocation alone,
  past the next grant (the paragraph above).
- **The admission exchange is pre-auth surface.** Rule-2 discipline binds
  it (memory-safe stack, strict dag-cbor decode, no hand-rolled parsers)
  and PQ-2's fuzz coverage extends to its kinds
  ([`../behavior/p2p.md`](../behavior/p2p.md) § Wormability posture).

**A fourth witness kind is registered at design level (2026-08-17, PQ-1
resolution): group-scope membership** — proves the channel-proven actor key
is a member of a T20 recipient-set group scope, verdict scope = exactly that
scope. Same verdict-shaped core, same key-binding rule; the witness *form*
is designed (T20 resolved 2026-08-17 — § The audience ladder → The
recipient-set scheme, membership-witness bullet). Consumer:
[`../behavior/p2p.md`](../behavior/p2p.md) § Offline share initiation.
Enumerations reading "three witnesses" elsewhere describe the *built* set.

### Wormability walk — the peer leg against the eight rules (W2.6 gate 2; ratified; refutable by the security review)

The rules are owned by [`../behavior/p2p.md`](../behavior/p2p.md)
§ Wormability posture; this is the peer leg's per-rule compliance record.
**By construction** = the specified shape satisfies the rule; **obligation**
= W2.6's build must land the named piece for the verdict to hold.

1. **Admission before parsing — by construction.** The admission seam
   (above) is evaluated before anything beyond `node_info` and the
   admission exchange; endpoints are learned only from the admitted plane's
   device-endpoint entries (T5 — no broadcast, no scanning, no global
   address space).
2. **Memory-safe pre-auth stack — by construction.** Same stack
   (Rust + quinn + rustls over the seam); the only new pre-auth parsing is
   the admission envelope, itself strict-dag-cbor and inside PQ-2's fuzz
   scope. The standing dependency-audit obligation carries unchanged.
3. **Least-kind dispatcher — obligation.** The peer-leg dispatcher
   allowlists the sync-transfer kinds only (walk, pull/serve, frontier
   exchange); no config, capability-mint, admin, or key-material kinds.
   Class-2 state rides as sealed *content* inside transfer kinds, never as
   imperative kinds — a fully compromised peer can at most feed sealed rows.
4. **Received content inert at the transfer layer — by construction.** The
   plane transports sealed canonical forms end-to-end (T14 envelopes,
   hash-addressed self-verifying blocks — the F9 anti-poisoning twin);
   forged rows fail AEAD at reading replicas; custodians can forge nothing
   (no keys). Recorded interaction: the post-transfer **merge seam's
   hostile-row posture** (the tombstone-refusal ruling owed before W2.5) becomes *listener-reachable* here; its
   sequencing ahead of W2.6 is part of this verdict.
5. **No listener when off; compiled away when excised — by construction
   plus one obligation.** The same-account leg is **not** a
   controversial-class registry member (own-account data meets none of
   [`dynamic-features.md`](dynamic-features.md)'s criteria, and account
   sync sits with core communication) — the `p2p-share` excision claim
   ("no share data plane at all") is scoped to the cross-user share
   surfaces. Obligation: W2.6 keeps the two legs' kind families and gate
   surfaces separable so the store-safe artifact witness stays provable —
   concretely, the witness gains a **third column** analogous to the
   payments/zaps "Damus" column
   ([`dynamic-features.md`](dynamic-features.md)): a store-safe flavor
   must show same-account sync PRESENT and `p2p-share` ABSENT
   (the security review's rule-5 reinforcement) — and keeps the
   listener's existence coupled to the peer node lifecycle's
   off (no unconditional bind); web remains structurally absent. **The
   "off" itself is the per-device participation control
   ([`../behavior/p2p.md`](../behavior/p2p.md) § Per-device participation,
   ratified 2026-09-25): `peer_leg::ensure_bound` reads it before the brake
   and drops the node, serve side and transport when it is off.**
   **Column-meaningfulness sequencing (clarified 2026-08-12,
   from the placement-tranche recon; dep claim CORRECTED 2026-08-15 at the
   W5.7 landing):** the "unconditional dep" reading was a
   manifest misread — `fauna-peer-sync` was only ever a *dev*-dependency of
   `fauna-sync-engine` (its own W2.6 entry below said so: "none does at
   W2.6"), so no artifact compiled the leg until **W5.7 promoted it to a
   real dependency under `account-runtime`**; since then tui (and every
   artifact enabling that feature) carries the `fauna.peer.sync.*` kind
   family, making the "same-account PRESENT" half true. But the
   "`p2p-share` ABSENT" half has **nothing to prove absent yet**: today
   `p2p-share` is only a feature-registry *key* + tier bounds
   (`GatedFeature::P2pShare` — metadata present in every flavor); the
   cross-user share **data plane** those bounds gate is the W8 share twin,
   which no code implements. A three-column witness landed now would carry a
   **vacuous** absent-half — exactly the failure the store-safe checks'
   third column exists to prevent — so the *meaningful* column is sequenced
   to the first artifact that compiles a real `p2p-share` surface (W8), not
   merely the same-account leg. The separability that makes it provable
   (distinct `fauna.peer.sync.*` kind family) is landed and pinned; what a
   store-safe check may assert today is at most the one-sided
   same-account-present property, which is already true by construction.
6. **Revocable per-device authority — admission by construction; content
   severance is honest only per key class** (CORRECTED 2026-08-11 —
   the security review refutation;
   the prior "by construction, with an interpretation" text over-claimed).
   Admission severance is the seam's next-evaluation bound — that half
   stands. Content severance splits **three** ways, and the boundary is
   *"holds `BackupKey`"*, not *"holds the seed"*: (a) M2-keyed planes
   rotate-on-removal — cryptographic severance. (b) A removed device that
   held the identity seed — or **any seedless reading replica holding
   `BackupKey`**, the modal enrolled device the prior text omitted —
   **permanently retains the root-derived account-state schedule**:
   ordinary device removal re-keys nothing on this plane
   (`fauna.sync.devices.delete` runs the MLS rotate only for shared file
   sets), generation-0 keys are `BackupKey`-derivable forever, and a
   seed-holder additionally re-auths at will. For generation-0-sealed
   data, content severance is an **access-control property** resting on
   an enumerable set of ciphertext channels being cut — backup-destination
   credentials, custody grants, swarm-admission latency — never a
   cryptographic one. **The custody-grant cut is durable only through a
   grant minted after the removal** (ruled 2026-09-26): a custodian never learns removals from the plane, so revoking
   every grant cuts the channel only while none is outstanding, and the
   lever that lasts is *revoke, then re-grant* — the re-minted grant carries
   the owner's exclusion list and refuses the removed device for its life
   (§ The admission seam → *Validity and severance*, the residual); a grant
   minted before the removal admits the device until it expires or is
   replaced. That lever holds for a seedless removed replica; a seed-holder
   can mint grants of its own, which is why the only root-level
   cryptographic severance is
   identity-**succession**
   ([`../behavior/identity-succession.md`](../behavior/identity-succession.md)),
   which device removal does not perform. (c) **R14's generation keys are
   the ratified forward fix**: removal mints a new generation excluding
   the removed device, so post-removal content is cryptographically dark
   to it — the residual is exactly the generations it already held, stated
   rather than hidden. Incident response for a compromised/lost device on
   generation-0 data therefore needs succession or the enumerated
   channel cut, not removal alone.
7. **Version brake — obligation.** The peer leg lights up behind a nest
   capability advertisement, so the existing fleet-wide brake covers it; a
   nest-unreachable fleet cannot receive the brake, which is the rule's own
   accepted limit (client update latency), not a new exposure.
8. **Bounded fan-out — obligation.** Per-peer and per-window
   connection/rate quotas at the shared listener, covering
   admission-refused attempts (the pre-auth DoS bound) and all witness
   kinds alike.


## Implementation status today

*The entries below were carried verbatim out of [`account-data-plane.md`](account-data-plane.md) § Implementation status today, which stays the home of the cross-cutting entries no single plane owns.*

- **Built 2026-10-05 — the peer leg's bytes ride direct paths only**. The dial pass's want-list block pull asks the shared byte gate before every request and, over a relayed connection while this pass's fleet walk reached the nest, leaves the blocks to the nest path, counting them in `DialPass::blocks_relay_deferred`; the row walks are untouched. The rule, its one spelling and how each pump feeds it are p2p.md § The relay → *Address discovery, and what rides a relayed path*, ruling 4.

- **2026-10-02 — the untagged `fauna.sync.changed` nudge for the `__config` set is gone, both halves**. The nest's `fauna.config.put` handler that sent it retired with the rail, and `nudge_scope_for_push` now wakes only the scope a tagged push names; an untagged push wakes nothing ([`config-dissolution.md`](config-dissolution.md) § Implementation status today, the closure-step-(6) entry).
- **RULED 2026-10-01; the skip BUILT 2026-10-02, the departed-scope withhold BUILT 2026-10-03 — § The bind leg, ruling 1(d), the diff at a full delegable scope.** On the fleet scope `publish_diff` still breaks at the first `Pushed::ScopeFull`. On the delegable scope (`ACCOUNT_STATE_SCOPE`) it goes on: after the pass's first refusal for room it withholds every push `publish_diff::needs_room` answers true for — the listing lacks the row's pair and the row names no listed row — and counts them in `DiffPush::withheld_for_room`, while a row whose pair the listing holds at a lower seq is still pushed (`libs/fauna-account-plane/src/publish_diff.rs`). The predicate takes the row's named rows as an argument, and the diff passes it the listed rows the push names (`delegable_reclaim::rows_a_push_names`, delegable-scope reclamation part (2), built 2026-10-02), so a push that names a listed row goes past a full scope. The diff also leaves to the publish every own row the delegable publish holds parked, as it leaves the rows above the slot. On the delegable scope the diff also withholds every row of a departed scope's item, whoever wrote it, keeping the relay copy and counting it in `DiffPush::departed` (`departure::departed_scopes`, delegable-scope reclamation part (6), built 2026-10-03; proven in `libs/fauna-sync-engine/src/account_runtime.rs`'s `leaving_a_channel_retires_this_devices_rows_and_a_members_row_stands`, seen red with the withhold off: two refused pushes). **Proven** in `publish_diff.rs`'s unit tests, each seen red first: `on_the_delegable_scope_scope_full_withholds_only_the_pushes_that_need_room` (one refused put, the listed pair's newer row stored, the other new pair withheld unasked, the parked own row never sent), `a_refused_push_retires_its_copy_and_scope_full_stops_the_fleet_diff` (the row after the refusal not asked), and `a_push_needs_room_only_for_a_new_pair_that_names_no_listed_row` (the predicate as the ruling words it, the named-row branch included). The measurement is in [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md) § Implementation status today; it bound no peer leg, so the diff's own stop was read from code, not measured.
- **RULED + BUILT 2026-09-30 — § The bind leg, rulings 1–4.** **Built — ruling 1 and ruling 2's replica id and watermark:** the nest mints a 16-byte replica id with its database (`nest_replica`, schema 98, seeded by `seed_genesis_rows`) and stamps it on every class-2 feed reply (`SyncChangesListReply::replica_id`); each full-state reconcile banks its answer as the plane's listing (`AccountStatePlane::listing`, raised by every put ack), and `fauna_account_plane::publish_diff` pushes behind it, verbatim (`AccountStatePlane::push_verbatim`), every relay row the listing lacks or holds lower — own rows at or below the published slot, a sibling's only when opened, verified, on this scope and by a verified member, and never one `generation_reclaim::covered_at_tip` calls covered — retiring the local copy on `stale_writer_seq` and stopping on `scope_full`; the pump runs it after each reconcile. Reclamation's own-arm, hand-over and veto evidence is `listed_at_or_above` (the listing or this pass's acks), never the frontier slot. The watermark is banked with the replica id (`AccountStore::raise_nest_watermark(scope, replica, seq)`, which re-keys from nothing on another id; `nest_watermark_replica`), and `page_walk::WatermarkCursor` voids it on another id or none where one is banked, and on an opening echo below it. Proofs: `conformance_account_state_walk::a_second_nest_settles_to_every_live_row_byte_for_byte` and `a_row_relayed_before_its_writer_publishes_settles_by_the_writers_self_echo` against the real handlers; the 12-seat sweep, whose pass now reconciles and diffs as the pump does, lands no diff push — what it does send is ruling 1(c)'s learning path, a sibling's row the nest already retired refused once per replica (380 refused over 30 cycles, measured 2026-09-30 with dead rows skipped; 1650 before), and it asserts that count stays at or under two per seat per cycle; `serve_order_watermark::a_watermark_banked_on_one_replica_never_hides_a_row_on_another`; the `publish_diff` and `generation_reclaim` unit pins. **Built 2026-09-30 — ruling 1(b)'s dead-row skip and ruling 2's void of the relay rows' serve coordinates.** The reclamation pass's dead tests live in one evaluator, `generation_reclaim::ReclaimState` (`dead_item` for a dead gen-0 item's copies, `relay_row_dead` for any relay row, a row sealed under a `Shredded` generation decided from its header unopened); the pass's forgets and the diff's `Dead` verdict (`DiffPush::dead`) both ask it, on the fleet scope the pass reclaims, for this device's own rows as for a sibling's, and a `Shredded` generation's own mint row stays pushed (`publish_diff::tests::a_siblings_shredded_mint_row_is_still_pushed`). Every reply of a nest-leg walk or reconcile is checked BEFORE its rows are applied (`AccountStatePlane::void_coordinates_with_the_bank`): one that names another replica than the bank's, or that the cursor finds voids an inherited bank (`FrontierCursor::voids_bank`), voids the bank through `AccountStore::void_nest_watermark`, which clears every `relay_rows.feed_seq` of the scope on all three backends (`relay_clear_feed_seqs`) ahead of the watermark and its replica key, so the new replica's first page stamps afresh and keeps its stamps. Proofs: `conformance::voiding_the_watermark_clears_every_feed_seq_of_its_scope` (SQLite and memory natively, IndexedDB through `tests/web.rs`); `generation_reclaim::tests::a_coordinate_from_another_replica_never_withholds_a_retire`; the coordinate assertions of `a_watermark_banked_on_one_replica_never_hides_a_row_on_another`. **Built — ruling 2's settled replica and the holder half:** every pass first re-reads the pin (`account_driver::TrustedHolderSource`, into the shared `generation_tip::TrustedHolders` every plane of the assembly borrows) and asks the bound nest its replica id over the owner session (`bind_leg::probe_bound_replica`, a class-2 feed request that selects no row; the nest names its replica on an empty feed too). The pair is compared with the store's settled replica (`AccountStore::settled_replica`, `bind_leg::BoundReplica`): nothing settled is a first bind, adopted when the pass completes; a different pair is the bind verification, ahead of the enrollment step — both watermarks cleared and the holdings check re-armed once per target (`bind_leg::BindMemo`), the grant latch void so the grant-first probe runs against the bound nest — and the pair is recorded only when that pass completes with the grant on the nest; a nest that names no replica is verified once per assembly. The ancestry the recovery pass admits is read from the bound nest's rotation chain at the first pass of every assembly and after every verification, best-effort. The holder half and the latch clause are [`account-data-taxonomy.md`](account-data-taxonomy.md)'s and [`account-replica-posture.md`](account-replica-posture.md)'s own status entries. Proofs: `conformance_account_plane_bind` — a second nest, a rebuilt nest and a rotated nest, each against real handlers and production runtimes and each ending with a reader holding only the identity seed, red-verified arm by arm; `an_empty_scope_still_names_its_replica`; the `bind_leg` unit pins. **Built 2026-09-30 — ruling 4, the secondary leg and the `account_replica` capability:** `fauna_protocol::pair::capability::ACCOUNT_REPLICA` is the sixth member of `default_self_sync()`. Every full pass of a seed-holding runtime, right behind reclamation, lists the account's pairings on the bound nest's owner session and completes each row carrying the capability (`fauna_account_plane::linked_leg`, run from the pass as `linked_pass`): the host's connector (`account_driver::LinkedNestConnector`, carried on `AccountRuntimeParams::linked_nests` natively and `WebRuntimeParams::linked_nests` on web) opens an owner-authenticated connection and reads the identity that connection is bound to, and a connection bound to another identity than the pairing row's is refused before anything is sent (`LinkedOutcome::IdentityMismatch`). Over it the leg reconciles both state scopes through a linked plane (`AccountStatePlane::new_linked`: no watermark sent, banked or voided, no serve coordinate stamped, our published high-water never moved, no walker mark left) and pushes the publish diff against that nest's listing; deposits every generation it keys at that holder (`generation_reescrow::ensure_deposited_at_linked`: the receipt verified against the pairing row's nest id, written in that holder's own cell through the bound fleet plane, never a mint); re-issues every retire the bound planes sent this pass whose row that listing shows at the same coordinates — or, since ruling 6, below them (`AccountStatePlane::take_issued_retires`, `linked_leg::mirrored_retires`); retires every listed row sealed under a generation merged state reads `Shredded`, whoever wrote it, then its mint rows behind the dataless belt and its receipts behind the sweeping one (`linked_leg::shred_retires`); and deletes that holder's wraps of every shredded generation its holdings answer still names. The hosts: all seven apps — tui, linux and the four UniFFI apps through `fauna_client_account_runtime::build_params` (`native_linked_nest_connector`, a second `NestClient` and `fauna_client::trust::connection_bound_identity`), web through `fauna-wasm`'s connector over the Nests page's `make_peer_connect` and `read_bound_identity` against the origin's pin; the seedless agent passes none and runs no secondary leg. **Ruled + built 2026-10-01 — ruling 5:** on a desktop whose sync agent is up, the secondary leg and the custody leg riding it used to run nowhere (found 2026-10-01 by the journey's first run: the app that has the connector ran no pass). The leg now runs in the seed holder that holds the seed-leg role, inside its pass or in its seed pass ([`account-runtime.md`](account-runtime.md) § Implementation status today owns that entry), and the retire record rests in the store: `AccountStatePlane::retire` on a bound plane records each retire it sends (`AccountStore::record_issued_retire` — coordinates, belt, answer; best-effort, a failed record is logged and the retire's verdict stands); the leg's run (`account_driver::pass::secondary_leg`) reads the record (`AccountStore::issued_retires`), hands it to `linked_leg::mirrored_retires` at each linked nest it reaches, and clears through the last entry it read when the run ends (`clear_issued_retires_through`), so an entry another process recorded meanwhile stays for the next run. One path: the per-plane in-memory list and its per-pass drain are gone, and a runtime holding both roles records and drains through the store too. The record is the derived table `issued_retires` on all three store backends (SQLite, IndexedDB — schema version 2 — and memory), keyed by the retire's coordinates so a deferred retire asked again every pass holds one entry, ordered by a rising token, and trimmed at every put to its newest `store::ISSUED_RETIRES_CAP` (2048) entries. Proofs: `conformance::the_retire_record_keeps_the_newest_and_a_reader_clears_only_what_it_read` on all three backends (IndexedDB through `tests/web.rs`); `conformance_account_plane_bind::a_retire_the_seedless_holder_sent_reaches_the_linked_nest_through_the_stores_record`, red-verified; the `linked_leg` unit pin `a_retire_issued_at_the_bound_nest_is_issued_at_a_linked_nest_listing_the_row`. **Ruled + built 2026-10-01 — ruling 6:** `linked_leg::mirrored_retires` owes a linked nest one retire per cell — a recorded retire's `(writer, item)` — whenever that nest's listing shows the cell at the recorded seq or below it, at the seq that nest lists; a second entry for a cell already owed adds only its `settled`. The re-made entry is no new code: a linked plane's reconcile records the rows that nest lists in the shared relay plane (`AccountStatePlane::take`, unconditional), `generation_reclaim`'s pass names them from it, and a bound plane's `retire` records the `Gone` it is answered as it records any answer. Measured before the cell rule: the linked nest served the signed-out device's enrollment after the first leg run and dropped it after the next agent pass and leg run. Proofs: `conformance_account_plane_bind::a_signed_out_devices_enrollment_is_retired_at_the_linked_nest_in_one_leg_run` (red before the cell rule) and `a_retire_whose_record_entry_was_spent_while_the_linked_nest_was_away_reaches_it_a_round_later` (red-verified with a `Gone` answer not recorded), each ending with a seed-only reader of the linked nest alone; the same `linked_leg` unit pin, which now also pins at-or-below, never above, and once per cell. **Ruled + built 2026-10-01 — ruling 7: removal evidence leaves a nest only by a leg run that found it at every linked replica.** *(a)* `generation_reclaim::Retirer::retire_removed_device` retires a removed device's enrollments, reach and device-scoped rows and no `Removed` row; `generation_reclaim::removal_evidence` lists the evidence the relay plane holds — for each device merged state reads removed, the rows of its device-set item that open as `Removed`, with their coordinates and the bound nest's serve coordinate. *(b)* `publish_diff::Vouch::judge` pushes a non-member's row in exactly one case (`own_removal_row`): the row opens, rests in its writer's own device-set cell, reads `Removed` by that writer, and the fleet view reads that writer removed; and the diff names the rows a nest refused for good (`publish_diff_naming_refusals`). *(c)* The arm is `linked_leg::retire_carried_evidence`, run by `account_driver::pass::linked_pass` at the end of every leg run — a full pass's and a seed pass's alike. The evidence is read before the first linked nest is connected; each completed nest reports where it stands on every row once its fleet scope is reconciled and diffed (`LinkedCompletion::evidence`: `Carried::Listed` at that exact coordinate, `Refused`, or `Missing` — `None` when the scope was not read whole, which the arm reads as a replica not reached). A row no replica misses is retired at each replica listing it — after that nest's own `fauna.pair.list`, asked over the linked connection, names no replica other than the bound nest and the nests this run reached; a list that cannot be read holds the row too — and at the bound nest through `AccountStatePlane::retire_unrecorded`, which enters nothing in the retire record, so ruling 6's cell rule never re-issues it round the pair-list check. The bound nest's retire is withheld without a request while the row's serve coordinate sits above the gate's watermark, as the pass's own retires are. The counts ride `LinkedPass::evidence` (`retired`, `deferred`, `held`). **Three things the build settled.** *The bound nest must have held the row too.* The rule is that evidence leaves a nest only once every replica that nest lists held it, and the bound nest is a replica of each nest that lists it; part (c) names only the replicas' listings. As first built the arm retired at a replica a row the bound nest had never been served — one this replica knew only from that replica's own feed — which the diff's exception masked and its red-verification exposed: with the exception reverted, the far device's leg retired the row at A although B's feed never held it. The arm now retires nothing, anywhere, for a row the relay plane holds with no serve coordinate of the bound nest (`EvidenceRow::feed_seq`, stamped by a walk of the bound feed or by a put it acked, never by a linked plane); the pass's diff pushes such a row to the bound nest first, and the run after it finds it there. A runtime handed no connector (`linked_nests: None` — the seedless agent) runs no leg and retires no evidence; a seed holder with no linked replica runs the arm with every row carried, and retires in the same pass. And once the bound nest has retired a row and the relay copy is forgotten, this device does not ask a replica that deferred again: the copy that replica's next reconcile re-records is refused by the bound nest at the next pass's diff and dropped, so it is never in the plane when a run begins. That replica sheds the row through a seed-leg holder bound to it — or in the first run, when no walker is bound there — which is the ruling's stated bound, and until then costs the refused put it names. Proofs: `conformance_account_plane_bind::a_sign_out_at_one_nest_is_read_by_a_device_bound_to_the_other`, `a_sign_out_is_read_across_nests_when_the_sibling_cannot_reach_the_other_nest` and `a_removal_at_one_nest_is_read_by_a_device_bound_to_the_other`, each ending with both remaining devices reading the departed one removed and a seed-only reader finding no device-set row of it at either nest; each part red-verified by reverting it — with the pass retiring the evidence again the device bound to B never reads the removal, in all three; with the diff's exception gone neither nest sheds a signed-out device's row, in both sign-out cases (the member's removal row goes under ruling 1(a) and that case stays green); with the arm gone both nests keep the row, in all three; the unit pins `publish_diff::tests::a_departed_devices_own_removal_row_is_pushed_and_nothing_else_of_it`, `linked_leg::tests::evidence_is_retired_only_by_a_run_that_found_it_at_every_replica` and `a_linked_nest_listing_a_replica_the_run_did_not_reach_keeps_the_evidence`, and `generation_reclaim::tests::a_removed_devices_enrollment_is_retired_before_its_removal_evidence`, whose order now spans the pass and the arm. **MEASURED RED, RULED AND BUILT 2026-10-01 — ruling 8: a removed member's grant is revoked at every linked replica.** Measured before the build: `fauna.sync.devices.delete` goes to the nest the removing app is bound to (`fauna-devices-machine`'s nest API), a member no roster row there accounts for is removed by its fleet row alone (`AccountStoreHandle::remove_fleet_member`), and with the one device bound to B lost and removed by key from a device bound to A, after six rounds, each with a leg run that completed B, B still minted for the removed key and served a gate watermark of 16 — the lost device's mark — under a tip of 17. What is code now: step 5 of `linked_leg::complete_linked_nest` runs `removed_grants::revoke_removed_grants` over the linked connection, after that nest's reconcile and diff and ahead of its retires, and reports on `LinkedCompletion::removed_grants`; the pass runs the same function at the bound nest. Proof: `conformance_account_plane_bind::a_lost_device_removed_at_the_other_nest_stops_counting_at_its_own_nests_gate`, red-verified by reverting the step — B stops minting for the removed key, its gate counts no mark, and it keeps the row. Ruling 7 leans on each nest's own gate, so its cases with a lost device bound to the far nest need this step. The rule's owner and its own status entry: [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (4) → *The nest half follows merged state*. Unlinking deletes the account's escrow wraps at the unlinked nest when it can reach it and its connection is bound to the row's id (`LinkedNestsMachine`'s `Unlink`, `LinkedNestsNest::escrow_generations`/`escrow_delete`). Proofs: `conformance_account_plane_bind::a_linked_nest_is_completed_without_ever_being_bound` (a seed-only reader recovers a box's seed from a nest no device was bound to), `a_linked_address_answered_by_another_identity_is_never_completed` and `a_shred_reaches_a_linked_nest` (a shred that landed while the linked nest was away reaches it in one pass, rows and wrap), each red-verified; the `linked_leg` unit pins; the `fauna-client-pair` unlink-sweep pins.
- **Built 2026-10-01 — the secondary leg also carries the RecoveryKey registration chain.** A leg run visits every pairing row that carries an address, replica or not: each gets the chain reconcile over the connection the leg opens, behind the same channel-binding check, and only the rows carrying `account_replica` are completed as ruling 4 says. The rule and its status are [`../behavior/identity-succession.md`](../behavior/identity-succession.md) § Enforcement on the home nest → *Every nest the identity is linked to*, clause (b).

- **Built — W2.0 the class-2 entry primitives (2026-08-10):
  `libs/fauna-core/src/account_entry_crypto.rs` + the key schedule in
  `libs/fauna-core/src/crypto.rs`.** The frozen T14 form in code: the
  `[form version][random nonce][ChaCha20-Poly1305]` envelope over canonical
  dag-cbor, AAD-bound to `{form_version, writer_id, writer_seq, scope,
  item_key}`, with sealed tombstones; and the Path A-sibling-2 schedule
  (`AccountStateKeySchedule` → per-kind `AccountStateKindKeys`, the
  capability-grant unit) with **KATs pinning both frozen context strings**.
  `seal_entry` derives the item key from the payload's own logical key, so a
  routing key disagreeing with its contents is unrepresentable; `open_entry`
  cross-checks kind + blind after decrypt, catching a malicious *sealer* the
  AAD cannot. Splice / replay-as-newer / cross-kind / blind cross-check are
  red-tested, and the AAD binding, the cross-checks and a context string were
  each mutation-verified to bite exactly their own tests. **Not built:** grant
  minting, and the reader-side consumption W2.4 supplies. The **store wiring**
  landed at W2.1/W2.2 and the **feed row** at W2.3 — both entries below.

- **Built — W2.1 the block plane + W2.2 segment adoption and bootstrap
  (2026-08-10): `libs/fauna-account-store`.** The local content store of
  § Store logical schema component 1, in both placements. W2.1: loose
  blocks + the always-present record index behind the same AFIT seam,
  with presence *derived* from the block plane (so an index row and its
  bytes cannot drift apart), hydration policy in store meta, and
  content-address verification on every block write. W2.2
  (`segments.rs`): a scope bootstraps by adopting whole CARv2 segment
  files **verbatim** — admitted only after the pair proves it carries
  exactly the records its sidecar declares, each hashing to the CID it is
  filed under, for this actor, in a *finalized* container — then indexed
  from the sidecar's append-ordered `record_order`, which is also how a
  replica that lost its index rebuilds without re-fetching a byte.
  **Placement is not observable above the store:** a read is answered
  from the loose area or from the segment's own CARv2 index, and
  adoption does not journal (a segment is bulk transport of truth the
  origin already journaled). A kind the hydration policy dehydrates is
  never *fetched* in bulk — that scope's index comes from the feed walk
  instead. Both compile for wasm32.

- **Built — W2.2's transport, end to end (2026-08-11):**
  `fauna_sync_engine::bootstrap_source::NestBootstrapSource`. The seam the
  store declared (`BootstrapSource`) now has a production implementor over a
  real nest: enumeration on the shipped `fauna.segments.list` control plane,
  bytes on the shipped segment byte plane, plus the sidecar door that plane
  gained the same day (`message-segment-store.md` § *The pair door*) — the
  `.meta` half is not optional for adoption, because `record_order` lives
  nowhere else. It lives in the engine, not the store, because the store's
  floor may hold no network dependency. Three rulings a reader should not
  re-derive: **(a)** the `(kind, scope_id)` a scope covers travels in a
  caller-supplied `ScopeBinding`, *not* parsed out of the scope string — no
  goal doc fixes that string's encoding and it is at-rest in every journal
  row, so this slice deliberately minted no naming convention and a source
  refuses any scope but its own; **(b)** the fetch verifies the `.dat`
  against the BLAKE3 the listing advertised — belt-and-braces over adoption's
  own per-block check, and the layer where a truncated transfer says
  "re-fetch" instead of "malformed container"; **(c)** the kind that reaches
  this plane first is **post**, not mail — adoption re-hashes every block
  against its CID, which post satisfies by construction
  (`Cid::of_dag_cbor(body)`) and mail cannot (a sequenced record id). Proven
  by `bins/fauna-nest/tests/conformance_account_bootstrap.rs` (tier_3): a
  fresh replica bootstraps a scope off a real nest over both real planes and
  serves the records back through the ordinary read path. **Not built:** the
  peer-side source of W2.6 (the feed walk that follows the bulk half landed
  2026-08-11 — the entry below).

- **Built — T1 the observation intake, the browse half of the seen-set
  producer (2026-08-13, row 27):** `fauna_sync_engine::observation_intake`
  is the shared rule T1's producer decomposition names (§ The replica
  boundary). `record_observation` takes one `Observation { scope, record }`
  — an app's assertion that *this record's body was handed to a visible
  view* — and owns everything downstream of it: **classification** (browse =
  not an own-actor scope, derived by negation so a new member/subscribed kind
  is governed the day it starts walking, rather than falling outside every
  producer until a list is extended), **coordinate resolution** through the
  new `StoreBackend::coordinate_of_item` (the scope-feed `(writer, seq)` of
  the *introducing* journal row, per scope; a new `journal_scope_item` index,
  index-only so an older binary reads the same rows), **dedup** against the
  merged entry, and the **class-2 put**. It only ever calls `insert_ref`, so
  a browse scope's entry stays itemized — a watermark would assert a prefix
  reading one message does not earn (mutation-red-verified: swapping the
  insert for a watermark raise fails the "only the rendered record is in-set"
  guard) — up to the seen-set's per-scope budget, the one fold that ever
  raises a browse scope's watermark (owner:
  [`account-replica-posture.md`](account-replica-posture.md) § The replica
  boundary → T2 transition (4), *The budget*; built 2026-09-15, *the merge
  door beside the writer door* below). Unresolvable observations return `Unresolved` and write nothing;
  the never-clobber rule on an undecodable entry matches the auto-in-set
  producer's. **Its first caller landed 2026-08-14** — see the next entry;
  the two-device tier_3 convergence proof landed with it.

- **Built — T1's reporting half on tui, the lead app (2026-08-14, row 46):**
  a production render now records browse observations, so T1 is code-confirmed
  end to end (§ The replica boundary → T1). Three pieces:

  **The seam.** `AccountStoreHandle::record_observation` is the app-facing face
  of the intake, running on the store thread beside every other write so an
  observation cannot race the walk that resolves its coordinate. Local-first:
  durable on return, published by the next pass. Repeats are free by contract
  (`AlreadyIn`, nothing published), which is what lets a shell report its whole
  visible set every frame instead of mirroring what it already sent.

  **The identity.** `fauna_conversations::plane::plane_ref` derives a conv
  record's plane identity at ingest and at send — through
  `fauna_mls::segments::derive_record_cid`, which routes to the very
  `encode_record` mint the nest files under (since the 2026-08-17
  record-identity cutover the identity is the content hash of the record
  envelope; `seq` is no longer part of the pre-image) —
  and carries it on `MessageSnapshot::plane_ref` (additive `Option`, so the
  at-rest `ChannelHistorySlice` stays byte-identical without one). It is
  derived in the two places that hold the sealed envelope and nowhere else.
  `Observation::parse` lifts the string pair back to typed values, so no app
  hand-parses hex or hand-builds a scope string — and it is where
  `fauna-conversations` (deliberately wire-type-free) and `fauna-protocol` have
  their spelling of a content scope checked against each other.

  **The report.** tui's event loop reports from the frame's own hit
  regions — built by walking the *visible band* of the scrolled paragraph, so a
  region exists for an element iff one of its painted lines landed inside the
  viewport (`apps/fauna-tui/src/observation.rs`). Reporting from tui's element
  list instead would report list-buffer transit, which T1 names as a
  non-observation: the list *is* tui's automation registry and holds every
  message of a thread. The observation rides the body element alone, so every
  suppressed arm — deleted, legally withheld, muted-collapsed,
  content-collapsed, content-blocked — excludes itself by never registering a
  body. tui has no overscan to subtract (ratatui paints the band and nothing
  beyond it); a shell that does must subtract it before reporting.

  Proven by: the red-verify (12 bubbles at a 24-row terminal report 3; the same
  app at 80 rows reports all 12 — so the filter is the viewport, not the tail),
  a suppressed-body negative that asserts the positive first, and tier_3
  `conformance_account_runtime.rs` V6 — two records walked, one reported, only
  that one in-set with no watermark, and the sibling device converging on it.
  **Still owed: the same leg on the other six apps**, each
  naming its own witness for "on screen" and proving it with the negative.

- **Built — conv on the content-scope feed, behind the channel-roster
  admission (2026-08-13, row 100):** channel members' replicas now walk
  `content:conv:<channel-hex>` through the ordinary `record-cid` arm — the
  enabling slice T1's producer decomposition names (§ The replica boundary),
  un-gating the browse seen-set trigger. The admission rule, ruled here so no
  session re-derives it: **the authoritative nest-side fact for "is this actor
  a member of this channel right now" is the `actor_channels` roster**
  (`is_actor_in_channel` — written at Welcome delivery / first send, evicted
  only by the folder plane's owner-driven rotate-on-removal), the same fact
  `channel.actors` gates member-scoped metadata on. MLS leaf state cannot be
  the fact — it is E2EE client state the nest cannot read — so the roster is
  the nest's *projection* of admission, and for conv-born channels an MLS
  Remove is nest-invisible: a removed-but-not-evicted member keeps receiving
  coordinates + tombstones, bounded strictly below what `channel.fetch`
  already serves any authenticated local actor. The gate's marginal work is
  therefore real but defense-in-depth: never-admitted actors lose activity
  metadata, and a folder-evicted member loses it durably — F1 eviction
  durability extended to this door. Four properties a reader should not
  re-derive: **(a)** the door **never writes the fact it checks** — no
  auto-register, unlike `channel.fetch`'s routing-parity write; with one, any
  authenticated actor could self-admit to any unclaimed channel's feed by
  asking. ⚠ **Scoped to this door only — measured 2026-08-18, and the
  self-admit it describes is the posture TODAY, reached one door over.** The
  siblings write the fact *for* the caller: `channel.send` and `channel.fetch`
  each auto-register the caller on any **unclaimed** channel — every
  conversation, the claim-read gate gating only *claimed* folder channels —
  and a `channel.send` refused at ingest still leaves the row (the register
  runs before ingest, is never rolled back). This widens nothing above the
  bound already stated — `channel.fetch` serves that same actor strictly more
  on the same precondition, knowing the channel id — but it fixes the gate's
  **reach**: real against a passive never-admitted actor, ~zero against a
  deliberate one, so a conv-feed admission is never evidence of channel
  membership. Pinned by `sync_handlers::feed_admission_tests::the_conv_feed_gate_is_lifted_by_the_callers_own_refused_send`.
  **This is DESIGN-ACCEPTED, not an open question and not debt** — graded as a
  pre-existing residual by the nest security review (the feed door
  that reads the roster without writing it) and re-affirmed by a later pass
  (the severance that needs a door nobody built), which enumerated this door and
  `channel_actors_handler` together and refuted both as novel. **Do not re-file
  it.** Nor was the swap to the (since-retired)
  `group_members` authority put on the **custody** arm a
  mechanical option: a DM had no `group_members` rows, so it would have failed
  every DM channel closed. The test above exists because this had been re-derived by
  hand four times — the door now carries its own witness; **(b)** a lapsed or absent membership serves a **flat `forbidden`**,
  identical for never-admitted / evicted / nonexistent (no existence oracle) —
  the replica records the scope unserved (scope-string ruling 4), and dropping
  its local data stays the departure seam's decision, taken only from an
  affirmative membership answer (T2 transition 3), never from this refusal;
  **(c)** membership is checked **before** the Plan-9 pure-backup gate (whose
  refusal mirrors `channel.fetch`'s — a member admitted onto a pure-backup
  destination's empty mirror would read converged-empty forever), so a
  non-member learns nothing of this nest's backup role; **(d)** admission is
  an **exhaustive fail-closed dispatch** (`sync_handlers::admit_content_scope`)
  pinned to `FEED_SERVED_KINDS` by
  `every_feed_served_kind_has_a_ruled_admission`, so a kind grown into the
  served set without an admission arm reds a test instead of shipping — the
  growth-contract pin the mail row lacked. **Same-nest only**: a cross-nest
  member syncs via its home nest, which does not hold the channel log; the
  feed's federation relay (twin of the shipped `channel.fetch` relay) is a
  declared follow-on, and until it lands a foreign-homed channel's scope keeps
  reporting unserved on the member's home nest — today's behavior, never a
  converged-empty. Proven by `conformance_account_bootstrap.rs` (tier_3, the
  production `welcome.deliver` kind writing the roster row the door then
  admits on): a member converges coordinates + tombstones through the relay-ack
  purge; a non-member is refused with nothing registered; an evicted member's
  next walk refuses.

- **Built — the content-scope feed walk, the bootstrap contract's second half
  (2026-08-11):** `fauna.sync.changes.list`'s `record-cid` arm nest-side
  (`sync_handlers::serve_content_scope_feed`) + `segment_records.changed_seq`
  (schema v33) + `fauna_sync_engine::content_scope_plane`. **Content scopes now
  join the generalized feed** — the shape this slice's design gate chose over
  making `fauna.segments.changed` the content feed, because that push is
  *segment*-granular (`Finalized` / `CompactedIn` / `CompactedOut` /
  `Tombstoned`) and a per-record delete fires none of its variants, so the very
  event the walk exists for would have been invisible until a compaction run;
  the feed also keeps content scopes uniform with the class-2 arm, which is
  what § Feeds and cursors' "one feed contract" and the frozen `record-cid`
  item class already anticipated. Five rulings a reader should not re-derive:
  **(a)** the feed is **derived from the record mirror, never a second event
  log** — `segment_records` gets one additive cursor column, advanced on append
  *and on tombstone*, so the feed cannot drift from what the nest serves and a
  `since = 0` walk is the class-1 twin of the class-2 zero-frontier reconcile
  (current truth, one row per record, not a replay); **(b)** the feed's unit is
  the **record**, not the mirror row — compaction files a record into a fresh
  segment and tombstones the input row for the *same* CID, so a row-keyed feed
  would report a live post deleted (the door groups by `record_cid`: live if
  any row is, coordinate the newest across them); **(c)** a content scope is
  **one-writer — the nest** (§ Feeds and cursors → *Multi-writer fit*), so the
  cursor is the shipped scalar `since` and the store's frontier slot is the
  reserved `WriterId::NEST_SEQUENCER` name rather than a nest key the client
  would have to discover (W2.3 ruling (a)); **(d)** the wire carries the record
  CID's **digest** in `path_hash` and the replica rebuilds the full CID, every
  record on this plane being dag-cbor-coded — the same assumption the mirror's
  own `record_cid` widening backfilled under; **(e)** the served-kind set is
  `post` / `calendar` / `card` / `mail` (row 4 below) — and, since 2026-08-13,
  `conv` (the *Built — conv on the content-scope feed* entry above owns the
  admission rule): its scope id is an MLS channel whose admission is
  membership, not the owner/recipient/author equality the other served kinds
  reduce to, which is why it joined last and behind its own admission arm
  rather than the blanket own-actor check. Proven by
  `bins/fauna-nest/tests/conformance_account_bootstrap.rs` (tier_3): a replica
  bootstraps a scope's segments, the nest deletes one record and adds another,
  and the walk converges the replica's index on the nest's live set. The
  nudge *trigger* wiring this entry used to list as not built landed with
  W3 (2026-08-12, tui's push arm) and moved into the runtime itself on
  2026-09-25 (`account-data-plane.md` § Implementation status today, the
  2026-09-25 entry). (The scope-set derivation this list used to name landed
  2026-08-12, member half included — *Built — W3 the own-actor scope-set
  derivation* below.)

- **Built — per-record coordinates on the two bulk purge paths; `mail` joins
  the feed (2026-08-11, row 4):** before this, `records_db::
  tombstone_mail_up_to_seq` and `tombstone_conv_up_to_seq` (mail's relay-ack
  purge and conv's twin) each ran one bulk UPDATE that flipped `tombstoned`
  without touching `changed_seq`, so a purged record's coordinate never
  moved and a replica already past it would never learn of the delete — the
  reason `mail` sat off `FEED_SERVED_KINDS` above. Both paths now share
  `records_db::tombstone_up_to_seq_with_coordinates`: every newly-tombstoned
  row gets its own fresh coordinate, materialized into a temp table first
  (same technique as `migrations::backfill_segment_records_changed_seq`) so
  SQLite's no-promise on what an in-flight correlated subquery sees of the
  table it is updating never bites, and `ROW_NUMBER() OVER (PARTITION BY
  scope_id ORDER BY rowid)` keeps each scope's counter independent when one
  call spans many scopes (conv's multi-channel ack). `mail`'s scope id is
  the owning actor — the same own-actor equality every other served kind
  already reduces to — so nothing else stood between it and the door.
  Proven by `bins/fauna-nest/tests/conformance_account_bootstrap.rs`
  (tier_3): a mail record purged by the relay-ack path reaches a replica
  that had already walked past it, as a tombstone, and the replica's index
  converges. **Not built:** nothing — this closes the gap the row above
  named. The remaining feed-walk gaps are tracked on that entry; the
  scope-set derivation this line used to name landed 2026-08-12, member half
  included on tui (*Built — W3 the own-actor scope-set derivation*), and the
  nudge trigger wiring is the runtime's own since 2026-09-25, so what remains
  is the six-app `MembershipSource` trickle-down that entry states.

- **Built — W2.3 the generalized feed, nest side (2026-08-10):**
  `fauna.account.state.put` + `fauna.sync.changes.list`'s `item_class` /
  `frontier` arms. `sync_changes` grew the four additive nullable columns
  (`item_class`, `origin_writer`, `origin_seq`, `entry_sealed`) through
  `reconcile_added_columns`, so every shipped row keeps its meaning and NULL
  *is* the pre-W2.3 file-row semantics. The account-state scope is realized as
  the per-actor reserved folder `__state`; a sealed entry is one feed row with
  the blinded item key in the `path_hash` slot and the T14 envelope inline, and
  the write fires the scope-tagged nudge. Three rulings a reader should not
  re-derive: **(a)** `since` remains the nest-writer slot and `frontier` carries
  the device writers — the doc's *"an omitted frontier is `{nest: since}`"* read
  as the two coexisting, which needs no nest-key discovery client-side;
  **(b)** a put **collapses only its own writer's** predecessors for that item
  (§ Store logical schema's *"a writer may compact its own log's superseded
  class-2 rows"*), because collapsing per item would delete the other replicas'
  entries and silently make the multi-master plane last-writer-wins; **(c)** the
  plane is bounded by a **live-entry count cap × a per-entry byte cap**, since
  blinding makes the key space unenumerable by name and closing an enumeration
  is therefore unavailable — metering is refused (no `device_id` on the wire, and
  charging settings against the file quota is a product change). **Not built:**
  folder-scope serving through this route (folders keep their shipped
  feeds) and durable idempotency (W4). *Superseded since:* the reader-side
  merge seam and walk landed at W2.4, and content-scope serving on 2026-08-11
  (the feed-walk entry below) — this route now carries two of the three scope
  families.

- **Built — W2.4 the client walk + merge seam (2026-08-10):**
  `fauna_protocol::merge_policy` + `fauna_sync_engine::account_state_plane`.
  The seam is the closed five-policy set with a per-kind lookup returning
  `Option` (an unknown kind is *not on the plane here*, never a defaulted
  policy) and `apply_class2`, whose CRDT-per-field arm delegates to the shipped
  `merge_user_configs` rather than re-implementing it; the first registered kind
  was `fauna.state.user-config` (whose registration E0 of the dissolution
  schedule retired 2026-08-12 — nothing ever sealed under it; the seen-set is
  the CRDT arm's exemplar now). The plane publishes (local write assigns the
  writer seq → T14 seal → `fauna.account.state.put` → own frontier slot
  advances), walks the scope's feed on the frontier, and reconciles from a zero
  frontier as backstop 2. Six first-build rulings a reader should not re-derive:
  **(a)** a **kind string is frozen** the moment anything seals under it — it is
  the `keyed_hash` input of both `entry_key(kind)` and `item_blind(kind)`, so
  the policy table grows rows and never edits one; **(b)** a reader recovers a
  row's kind by **trial-opening** under each registered kind (the blind is
  one-way and the AEAD tag arbitrates) — the shipped epoch-opener's trial-chain
  shape; **(c)** a row that opens under **no** registered kind is skipped and
  left *unaccounted*, which is the compat answer for a newer writer's kind and a
  per-kind grant alike (the reconcile re-presents it after an upgrade), while
  the walk's own paging cursor — in-memory, never persisted — is what keeps the
  durable frontier honest and the page loop advancing; **(d)** an ingested row's
  `entry_version` is the **origin's `writer_seq`**, derived from the wire so a
  replay is byte-identical and therefore idempotent rather than a spurious
  equivocation refusal (the origin's own counter is not on the wire and the feed
  keeps one live row per `(item, writer)`, so its seq *is* the item's version on
  its log); **(e)** a merge that reproduces the current value reports
  *keep*, not *merged* — otherwise two converged replicas re-publish at each
  other forever; **(f)** a **tombstone on a CRDT kind is refused**, because
  degrading it to a stamp comparison lets a value concurrent-with-and-newer-than
  the tombstone win wholesale on one replica while the other keeps its merge,
  and the two never converge — per-field deletion is a design slice, not an
  implementation choice. The local entry rests **plaintext** per § Local at-rest
  posture; sealing happens only at publish, which is also why it happens after
  the local write (the AAD binds the seq the store assigns). **Not built:** the
  three-way resolver (no kind declares it), and the offline outbox — W2.4 ships
  only `publish_pending`, which replays this replica's own un-published class-2
  rows and holds no intents. (The W2.5 exemplar kinds all registered since:
  the preference cluster with item 1, the seen-set with item 2, the
  device-endpoints with item 3.)

- **Built — the feed compacts itself behind a retention gate (2026-09-16):**
  `fauna.account.state.retire` marks one live class-2 row superseded by its
  cleartext coordinates, inserting nothing — the one write that needs no
  live-entry headroom — and the nest refuses it `not_yet_stable` while any
  walker marked on the scope (the additive `walker_id` a walk sends beside
  the `held_through_seq` it banks; `state_walk_marks`, the max per (scope,
  walker)) whose key holds a live, non-tombstoned grant has not walked past
  the row — and, since 2026-09-22, tells the walker where that gate stands:
  the additive reply field `retirable_through_seq`, the lowest counted mark,
  which the reclamation pass reads beside each relay row's own serve
  coordinate to withhold a retire the gate would refuse (owner:
  [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation
  machinery → *Fleet-scope reclamation*, clause (1) → *the gate's
  watermark*) — and `generation_in_use` while a live form-v2 row still names the
  generation a caller asserts dataless (`no_rows_sealed_under`; the feed's
  additive `sealed_under` filter is the matching read); with the additive
  `delete_escrow_wraps` beside that belt, the retire that lands also deletes
  the holder's escrow wraps of the belted generation in the same transaction
  (the escrow sweep, ruled in
  [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation
  machinery → *Fleet-scope reclamation* clause (3e)). Every field and
  kind is additive; the retired row keeps its bytes and coordinate, so the
  seq-reuse refusal is unchanged. **Who decides what is redundant is the
  client's ruling, not this plane's:**
  [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation
  machinery → *Fleet-scope reclamation* owns it, with the
  `fauna.state.device-reach` kind that carries the evidence.
- **Built — the writer door refuses an entry no nest will accept
  (2026-09-15):** ruling (c) of the W2.3 entry bounds a sealed entry at
  `MAX_STATE_ENTRY_BYTES` (64 KiB, `fauna_protocol::account_state`), and every
  nest refuses a larger one. `put` and `tombstone` now compute the entry's
  sealed length in its kind's registered form
  (`fauna_core::account_entry_crypto::sealed_envelope_len` — encodings only, no
  key) and refuse an over-cap entry **before the local write**, naming the
  kind, the length and the cap. The reason is `publish_pending`: it exists to
  re-send rows a *network* failure left local, but an over-cap row is refused
  on every pass, and because the pass stops at its first failure, every later
  row the same writer put in that scope stayed local-only behind it. That was
  measured, not reasoned: on a whole-suite sweep's session actor a heal-mint
  entry outgrew the cap as the fleet grew
  ([`account-data-taxonomy.md`](account-data-taxonomy.md) § Implementation
  status today) and held the fleet scope for the rest of the run. The heal
  `publish_pending` once ran for such a row (built 2026-09-15) was removed at
  the compat-remnant sweep: every row the plane journals passes the door, so
  only a pre-sweep store could hold one
  ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation
  machinery → *The bounded mint*, clause (f)).

- **Built — the merge door beside the writer door, and the seen-set's
  per-scope budget (2026-09-15):** the walk's `Merged`
  arm was the journal's third writer and the only unsized one. A union CRDT's
  join can outgrow both inputs, and two under-cap `fauna.state.seen-set`
  entries (the finding's probe: about 1 300 itemized references each, under
  a narrow-encoding writer id) merged to a 130 KB value the
  walk journaled, published, and was refused on — the stall the writer door
  exists to prevent, reached from a *reading* replica. Two pieces. **(1)** The
  kind's join is bounded by construction: `fauna_core::seen_set` carries a
  per-scope element budget folded into the join as a closure (owner:
  [`account-replica-posture.md`](account-replica-posture.md) § The replica
  boundary → T2 transition (4), *The budget*), so the seen-set's merged value
  — and the intake's put, which met the same cap on one device — never
  reaches the cap. **(2)** The merge door sizes every merged value with the
  same `refuse_if_over_entry_cap` the writer door uses, before the local
  write, and treats a failure as a row-content refusal: skipped, counted in
  `WalkReport::unmergeable`, not accounted, re-presented by reconcile — never
  a local row. Proven at the door by
  `libs/fauna-sync-engine/tests/peer_leg_convergence.rs` (two replicas whose
  under-cap seen-sets' union outgrows the cap converge under it; a writer
  population past the budget is skipped at the door) and at the arm by
  `fauna_protocol::merge_policy`'s seen-set tests, which also pin the budget
  to the cap. Each half is pinned independently: deleting the door reds only
  the door test (the walk then merges and journals the over-cap value),
  deleting the budget fold reds the three fold laws, the arm test and the
  convergence test — which then fails at the *door*, the backstop catching
  what the budget no longer bounds. **Built 2026-09-16 — the door is a
  ticket, not a step:** the walk's `Replace` +
  `Take::Carry` arm — which re-journals the fleet's value as this replica's
  own row — carried a *third* hand-wired copy of the same check, and no test
  reached it: removing that copy red nothing across 775 engine tests and the
  peer-leg suite, while the identical edit on the neighbouring `Merged` arm
  red exactly one. A guard four arms must remember, with a fifth write
  exempted "bounded by construction", is a guard that can be unwired
  ([`security/review-method.md`](security/review-method.md) ⭐ *A guard whose
  failure is silent lives inside the consumer, not the callers*) — so the
  sizing **moved** rather than gaining a third pin. `refuse_if_over_entry_cap`
  is private now and reachable only through a `SizedEntry` ticket, and the
  plane's one write onto its own log (`put_own_row`, the module's only
  `StateStore::put_state` caller) takes that ticket and nothing else: an arm
  journaling a value this replica authored cannot reach the store without
  passing the door, and a new one cannot forget it — rustc refuses the write.
  The verdict is *carried* rather than checked at the head of the write
  because `put`/`tombstone` must pass the door **before**
  `admit_origination`, which can mint a generation an entry that can never
  publish must not spend. Each arm still decides what a refusal means (the
  write returns it, the walk's arms count it in `WalkReport::unmergeable`
  and re-present the row),
  which is why the ticket hands the refusal back instead of handling it.
  Pinned twice, both mutation-measured 2026-09-16, each redding exactly one
  of 776: deleting the check inside `SizedEntry::size` reds
  `an_entry_no_nest_accepts_is_refused_before_the_local_write` — the pin that
  already covered `put` now covers every arm — and pointing the carry arm back
  at `put_state` directly reds `the_plane_has_one_sized_write_onto_our_own_log`,
  a mechanism pin over the module's own source because "no arm goes around the
  seam" is not a behaviour any test can witness, which is exactly how the
  unwitnessed copy arose. The over-cap local
  row a replica already holds stays with the writer-door entry above.

- **Built — the CRDT delegate is a join in BYTES (2026-08-10):** the follow-on
  W2.4 owed itself before W2.5 puts the real `UserConfig` on the plane.
  `merge_user_configs` was commutative in *content* but built several
  collections in arrival order and broke every whole-record tie toward "ours",
  so two replicas agreeing on every element still produced different bytes —
  which each reads as new state, re-publishing forever. Two rules close it, and
  they are the pattern any later CRDT kind copies: **no local preference** (a
  tie resolves on the values' own canonical bytes, per field, so each field is a
  max over a total order and the fold is associative as well as commutative) and
  **order derived from content** (every collection sorts on a key computed from
  its elements; a *capped* collection applies the cap after that ordering, which
  is what makes even `mail.prior_mseks`' top-2 a join rather than a content
  divergence). Asserted on bytes over a pair differing in every tiebreaking
  field, both laws, each arm red-verified by reverting its own fix
  (`format_user_config.rs`, § A join, is the authority; `reserved-folders.md`
  § UserConfig Sync carries the rail-side consequence). The plane's own
  canonical-argument-order rule (`merge_user_config_entry`) is now
  belt-and-braces rather than the thing making a pair agree. Guarantee scope is
  **one owner chain**, which is every production merge; a by-product of the same
  pass is that the merged config now carries the **successor's** `actor_id`
  (`succession-aftermath.md` § Re-key scope). (The whole-record merge and
  `format_user_config.rs` retired with the rail on 2026-10-02 —
  [`config-dissolution.md`](config-dissolution.md) § Implementation status
  today.)

- **Ratified + built — the content-scope string (2026-08-11):**
  § Feeds and cursors → *The scope string*, with the constructor/parser in
  `fauna_protocol::scope` (`Scope` / `ContentScope`: canonical `Display`,
  strict shape-only parse, both fixture mis-spellings regression-pinned) and
  every test fixture moved to canonical spellings. No production replica ever
  wrote a content-scope row before the ruling (test fixtures only, and the
  account-state scope was already ratified), so the freeze starts clean — no
  migration. **The string is now production-load-bearing** (2026-08-11): the
  feed walk parses it at the nest door and files every journal, frontier and
  index row under it client-side, so ruling 3's strictness is what a real
  request meets — and the `ScopeBinding` seam this entry left open is closed by
  `content_scope_plane::binding_for`, which derives the binding *from* a
  `ContentScope` so the bulk half and the walk half cannot name a scope
  differently. **Not built:** nothing derives a replica's scope *set* from
  memberships (W3+), and the folder family tag stays reserved-unruled.

- **Built — W2.6 the peer leg (2026-08-12): `libs/fauna-peer-sync` + the
  store's relay plane + the plane's pull-only mode.** Store↔store proven
  with no nest anywhere: two real replicas converge divergent LWW and
  union-CRDT class-2 state and transfer blocks by want-list pull, over the
  transport seam (`fauna-sync-engine/tests/peer_leg_convergence.rs`) and
  over a **real iroh QUIC connection**
  (`fauna-iroh/tests/peer_sync_over_quic.rs`, `--features quic`). The
  pieces, each owned where its mechanics live: the **admission seam** as
  ruled — the core consumes a per-connection verdict
  (`fauna_peer_sync::admission`), the `DeviceAuthorization` witness verifier
  sits beside the cert's own mechanics
  (`fauna_core::encoding::verify_device_admission_witness` — proven-key
  binding, same-account, expiry; capabilities deliberately not consulted),
  the exchange kinds are `fauna_protocol::peer_sync`
  (`fauna.peer.sync.admit`, strict dag-cbor, inline carriage, mutual =
  independently admitted); the **relay plane** — the store retains one live
  verbatim wire row per `(scope, writer, item)` (the nest's own collapse;
  `fauna-account-store` `relay_rows`, additive at-rest), fed at publish and
  at ingest, because the merged `StateEntry` cannot reconstruct another
  writer's row and R6's verbatim relay (a future key-less custodian
  included) needs the sealed envelope itself — and pruned at the nest's
  final refusal of a coordinate, so the plane serves what the nest holds
  or will hold and never a row it refused for good (the rule, its
  replay/burn split and its one accepted trade are owned by
  [`account-replica-posture.md`](account-replica-posture.md) § The store
  device principal, refinement 11 → *a refused row's relay residue*); the
  **serve side**
  (`fauna_peer_sync::server`) answering the SAME `fauna.sync.changes.list`
  wire from that plane plus `fauna.peer.sync.blocks.pull` (per-block scope
  enforcement against the verdict; frame-budgeted pages/`deferred`); and
  the **walk** — `AccountStatePlane::new_pull_only` over a
  `PeerRequester`, the identical W2.4 walk with two peer-leg rules: a
  merge's output lands locally + relay-sealed but never RPC-publishes
  (pull-both-ways; the nest leg's `publish_pending` carries it onward), and
  a self-echo from a peer never advances the own-writer frontier slot (that
  slot is the *published-to-the-nest* watermark — a peer echo proving
  otherwise would silently starve the nest of the row). Wormability
  obligations landed: **(3)** the dispatcher is the four-kind allowlist
  (`server::allowlisted_kinds`; everything else `unknown_kind`, pinned);
  **(5)** the listener exists iff `start_peer_sync_node`'s node is alive
  (no unconditional bind) and the kind family is `fauna.peer.sync.*` under
  no `p2p-share` gate — **the store-safe witness column binds at the first
  artifact that compiles the leg** (none does at W2.6; W3's placement adds
  it, the separability that makes it provable is landed); **(7)** the
  `peer-sync` capability token is the fleet brake — advertised always-on
  nest-side (`discovery_core`), refused client-side at the one bind door;
  **(8)** per-peer fixed-window connection + request quotas on an injected
  clock, admission-refused attempts included, the ledger itself
  size-bounded — a flood of fresh pre-admission keys evicts fully-elapsed
  windows first, then the oldest, so the map stays O(cap), never
  O(every-key-ever-seen) (`fauna_peer_sync::quota`; the quota
  fix, landed 2026-08-12 ahead of W3's listener binding, its twin —
  the blocks.pull preflight's account-match without a scope check, so a
  future Named-scope verdict reaches the per-block enforcement — landed
  beside it). Discovery (T5) is code:
  `sibling_dial_targets` reads the store's device-endpoint entries,
  cross-checks each value's node id against its logical key, applies PT-4
  hygiene + the shipped LAN arithmetic, and carries the relay URL to the
  endpoint builder's slot (`IrohTransport::bound_addrs` is the publisher's
  address source). **Dial candidates are verified fleet members only
  (built 2026-10-02):** `peer_leg::dial_pass`
  hands `sibling_dial_targets` the fleet view's `is_verified_member`
  (`fleet_removal::fleet_view`), so a removed, unenrolled or predecessor
  device's merged entry is never dialed, while the custodian targets
  merged in after it stay unfiltered
  (`peer_leg::tests::the_dial_pass_dials_verified_members_and_custodians_only`,
  red without the filter). It subsumes the 2026-09-30 skip of this
  machine's own retired writers: after an in-process
  succession the predecessor's endpoints row names this machine's
  retired writer, whose enrollment is the retired root's and never
  verifies, so the separate skip list and `PrincipalBundle::predecessor_writer_ids`
  are removed and `a_successor_never_dials_its_own_predecessor_writer`
  now proves the case through the filter.
  **The assembly seam this entry used to defer is BUILT
  (2026-08-15)** — both of its gates landed first exactly as the
  recon sequenced them (the W5.4 ceremony mints the witness; R14 + the
  device-endpoints writer feed live discovery), and the *Built — W5.7*
  entry below owns the mechanism. Still deliberately absent from the serve
  side's world: the production **dial pass** (the W5.7 entry
  states it). The *meaningful* store-safe witness **third column** is
  **BUILT (2026-08-17/18)** — W8's share surface reached a root, and
  `just tui-store-safe-check` now proves `fauna.peer.share.` absent +
  `fauna.peer.sync.` present in the store-safe flavor, share present in
  default (rule 5 DISCHARGED; recipe owned by `dynamic-features.md` § The
  feature-matrix test story). PQ-2 fuzz coverage is
  **BUILT 2026-08-12** (`peer-channel-hardening-check`, the thirteenth merge
  gate). web remains the declared structural absence (the crate never
  enters `fauna-wasm`'s graph). The content-coordinate gap this entry used to
  state is CLOSED — the *Built — W3 the peer content-coordinate relay* entry
  below.

- **Built — removal severs peer-plane admission (2026-09-20).** § The
  admission seam → *Validity and severance* is in code, closing the gap the
  entry above recorded (the built verifier checks proven-key binding,
  same-account and expiry — and the production fleet cert has no expiry, so
  nothing withdrew it). The `DeviceAuthorization` arm of `evaluate_witness`
  now returns the admitted device key beside the verdict
  (`fauna_peer_sync::admission`, the twin of the custody arm's grant id), the
  serve side carries a per-served-account removed-device view
  (`PeerSyncServerConfig::device_removed`) consulted at the admit door
  **and** on every request through the connection slot
  (`AdmittedConnection`, beside the served-account check), and the dial side
  carries the same view on `admit_over_as`
  (`fauna_peer_sync::AdmissionViews`). The snapshot behind it is
  pump-refreshed per pass from merged plane state
  (`fauna_sync_engine::fleet_removal::removed_device_ids` →
  `peer_leg::refresh_removed_devices`), beside the custody-revocation one;
  its bound is the bullet's own (*When it learns is the bound*). Pinned
  store↔store with no nest anywhere in
  `fauna-sync-engine/tests/peer_leg_convergence.rs`
  (`a_removed_device_is_refused_at_the_next_handshake_in_both_directions`,
  `removal_severs_a_live_connection_at_its_next_request`). The custodied-serve
  residual the bullet states is open by design, not by omission — narrowed
  since by the grant's own list (next entry).
- **Built — the custody grant's removed-device exclusion list (2026-09-28).** `CustodyGrant::removed_devices`
  (`fauna_core::custody_grant`, additive, pinned byte-identical to the
  pre-field encoding when empty and decoded empty from old bytes) is filled
  at the one witness-signing site of the ceremony driver
  (`fauna_client_capabilities::custody_ceremony::drive_ceremonies`, the
  deliver arm) from `CustodyRegistryWriter::removed_device_ids` — on the
  account runtime, `fleet_removal::removed_device_ids` over its store and
  trust. The custodian's map is `fauna_peer_sync::admission::CustodiedExclusions`,
  held on `PeerLegState` beside `removed_devices`, re-derived in
  `custody_leg::CustodyLegState::serve_refresh` before the serve registry is
  fed, and read by the composed serve/dial view
  (`WithdrawalSnapshot::device_removed_view` — own account from the
  snapshot, custodied accounts from the map) and by the custodian's own
  custody dial. Pinned in `fauna-peer-sync` `server::tests` (admit door, live
  connection, signer binding), `custody_leg::tests` (derivation + union) and
  store↔store in `fauna-sync-engine/tests/custody_convergence.rs`
  (`a_removed_owner_device_the_held_grant_lists_is_refused_by_the_custodian_both_ways`,
  an unlisted owner device the in-test control).
- **Built — both withdrawal snapshots fail closed until derived, and custody
  revocation severs per request (2026-09-22).** The bullet's bound is in
  code on both arms. Both snapshots are
  `fauna_sync_engine::peer_leg::WithdrawalSnapshot`s: refreshed side by side
  right after the fleet walk and before the ensure step (the
  custody-revocation derivation moved out of the later serve refresh into
  `custody_leg::CustodyLegState::refresh_revoked`), starting underived, and
  every view over an underived snapshot refuses its arm. Before this, a first
  pass whose device-set read failed bound a server that answered from an
  empty set — admitting every removed device, at the door and per request —
  and the custody-revocation snapshot derived only AFTER the bind on every
  first pass, so a freshly bound server admitted revoked custody grants until
  the serve refresh ran. The serve side's per-request check now covers the
  custody arm beside the device arm (`AdmittedConnection::custody_grant_id`,
  re-checked in `PeerSyncServer::admitted_connection`); a revoked grant used
  to keep a live connection until its own `expires_at`. Pinned:
  `peer_leg::tests::a_first_bind_over_a_failed_refresh_refuses_until_one_succeeds`
  (the production ensure step over a real SQLite fault that fails the
  device-set read alone — binding succeeds, a sibling's admission is refused,
  and the next successful refresh heals the same server with no rebind),
  `a_failed_refresh_after_a_success_keeps_the_last_derived_set` and
  `a_withdrawal_view_refuses_until_derived_then_only_what_it_lists` beside it,
  `custody_leg::tests::an_unreadable_replica_leaves_the_revocation_view_refusing_until_one_reads`,
  and `fauna_peer_sync::server::tests::a_custody_grant_revoked_mid_connection_severs_at_the_next_request`.
- **Built — frontier compaction as a serve-order watermark, nest leg
  (2026-09-13).** § Feeds and cursors → *Compaction is a serve-order
  watermark, never a forgotten entry* (which replaced the unhonourable
  "forget a retired writer" remedy) is in code on the nest leg: the two
  additive wire fields (`SyncChangesListRequest.held_through_seq`,
  `SyncChangesListReply.complete_through_seq`); the nest's third gate and
  the echo, stamped from the page actually cut
  (`bins/fauna-nest/src/db/account_state.rs` `get_account_state_changes`,
  `sync_handlers.rs` `serve_account_state_feed`); the store's per-scope
  watermark and per-writer relay high-waters (`AccountStore::nest_watermark`
  / `raise_nest_watermark` / `relay_high_waters`, the watermark cleared with
  its scope by `drop_scope`, or on its own by `clear_nest_watermark`); the
  page loop's echo absorption (`page_walk.rs` — a cursor takes each reply's
  echo after the page's applies and before the refusals, and an empty page
  whose echo moved the cursor is checkpointed); the class-2 plane's nest leg
  (`AccountStatePlane::walk` / `reconcile` project and bank per page, under
  the policy the *Requester* bullet records); and the custodian's nest arm
  (`custody_leg::custody_pull` with `of_owner`). **Un-banking a rolled-back
  nest — built 2026-09-14:** a cursor opened against a bank it has
  not itself reconfirmed, whose first reply carries no echo, falls back to
  the whole frontier for the rest of that walk and clears the persisted
  watermark, per the *Requester* bullet's addendum — one full re-serve, then
  every walk after reopens exactly as an older nest was always walked with.
  Proofs: `libs/fauna-sync-engine/tests/serve_order_watermark.rs` — a scope
  past `MAX_FRONTIER_WRITERS` converges on an honouring nest with every
  request under the ceiling and the stored frontier complete, an older nest
  keeps refuse-re-grow-refuse, an unopened row keeps its writer named, the
  peer leg never projects, a nest rolled back after banking a watermark is
  un-banked after one full re-serve and the next walk takes none — and
  `bins/fauna-nest/tests/conformance_account_state.rs` (the gate and the
  echo through the wire). **Not built, by the ruling:** the store-served
  legs (the peer leg, the custodian's owner-device pull) and the group
  plane, whose feed is a share serve set's relay pull; there, and against a
  nest predating the watermark, an honest frontier at the ceiling is still
  the refuse-re-grow-refuse state — reachable only after a lifetime of
  successions on one scope.
