# The account-data taxonomy — target state

Owns: account-data-taxonomy
Status: ratified — the audience ladder (R13, ruled 2026-08-11) and the generation machinery (R14, ratified 2026-08-13 and built the same day) are the taxonomy's settled pieces; split verbatim out of `account-data-plane.md` on 2026-09-06; the recipient-set scheme (T20), its third, split verbatim out to [`recipient-set-scheme.md`](recipient-set-scheme.md) on 2026-09-28; the delegable-scope reclamation split verbatim out to [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md) on 2026-10-01
Authority: **what account data IS, who may see it, and how it is keyed** — the classes of account data and their total classification, the audience ladder (R13) and its rungs, the generation machinery (R14: generation tips, escrow, the storage-group keying seam), and the export-confidentiality axis. **NOT owned here** — the recipient-set scheme (T20: how a storage group scope is keyed, its roster, severance and witness) → [`recipient-set-scheme.md`](recipient-set-scheme.md); what holds the delegable scope at one live row per item and what a full one is owed → [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md); the plane those classes travel on → [`account-sync-plane.md`](account-sync-plane.md); what a replica may hold of them → [`account-replica-posture.md`](account-replica-posture.md); the account store that persists them → [`account-data-plane.md`](account-data-plane.md) § The account store (W1); the charter, the ratified decisions, the nest-side requirements and the cross-cutting status → [`account-data-plane.md`](account-data-plane.md). On conflict in those domains, raise it.

Last verified: 2026-09-06 (split verbatim; each ruling carries its own ratification date below)

Split verbatim out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06 — that doc had reached **686,434 B**, 2.62× the 262,144 B whole-file read ceiling, and no single seam could clear it (moving its status ledger alone left both halves breached, re-verified at three successive sizes). Its own `Authority:` line already enumerated the five concepts it owned; this is that list made structural, each concept taking its rule sections **and** its status-ledger entries together. A routing stub remains at each original location; prior history: `git log --follow docs/goal/architecture/account-data-plane.md`. The `W<n>` workstream labels and `R<n>` decision labels used throughout are defined in [`account-data-plane.md`](account-data-plane.md) § Workstreams and § The ratified decisions.

> **Reading this doc.** Its text was carried **verbatim** out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06, so an unqualified `§ <name>` citation inside it may name a section that is no longer a sibling on the page. Resolve any such name against the rest of the family first: [`account-data-plane.md`](account-data-plane.md) (the ratified decisions, the account store, the nest-side requirements and the cross-cutting status), then [`account-sync-plane.md`](account-sync-plane.md), [`account-offline-mutation.md`](account-offline-mutation.md), [`account-runtime.md`](account-runtime.md), [`account-replica-posture.md`](account-replica-posture.md). Positional words (“above”, “below”) inside a carried block point within this doc: every section moved whole, so an intra-section deictic could not break, and the boundary-crossing ones were scanned before the split.

## Section map

- **[The account-data taxonomy](#the-account-data-taxonomy)** — the classes, the audience ladder (R13) and the generation machinery (R14); the recipient-set scheme (T20) keeps a stub here and lives in [`recipient-set-scheme.md`](recipient-set-scheme.md). The delegable-scope reclamation keeps a stub in the generation machinery and lives in [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md). **The heading is unchanged from `account-data-plane.md`**, so a `§ The account-data taxonomy` citation resolves by swapping the filename.
- **[Implementation status today](#implementation-status-today)** — the export-confidentiality axis build-out, carried with the classes it grades.

## The account-data taxonomy

Every piece of account data belongs to exactly one class; the class decides
its sync unit, merge policy, hydration behavior, and offline mutability.

1. **Content records** — immutable, content-addressed records: posts, mail,
   conversation messages, calendar events, cards. Wire bytes = at-rest block
   bytes = replica block bytes; identity is the dag-cbor CID
   ([`data-flow.md`](data-flow.md) § At-rest storage). Replication is block
   transfer; there is nothing to merge — a record exists or it doesn't.
2. **Mutable state** — settings-class and relationship-class entries:
   the account-state kinds that replaced the retired `UserConfig` blob's
   fields ([`config-dissolution.md`](config-dissolution.md) § The `__config`
   dissolution schedule → *The kinds*), profile, memberships, grants, read/ack markers, the
   seen-set itself. Merge-policy-governed (§ The sync plane → merge-policy
   seam), and **audience-rung-governed orthogonally** (§ The audience
   ladder, R13): the merge policy says how replicas converge; the rung says
   who can open. Operational secrets are class-2 kinds at the fleet-only
   rung — the class never decides the audience.
3. **Payload blobs** — large bytes: attachments, media, synced file contents.
   Content-addressed, hydration-policy-governed per device
   ([`../behavior/on-demand-files.md`](../behavior/on-demand-files.md)
   generalized: every item's metadata everywhere, payloads where policy says).
4. **Derived / recomputable** — search indexes, previews, feed projections,
   caches. Never truth, never a sync unit; rebuilt locally from classes 1–3
   (an existing nest-replicated derived store like the sealed `__index` rail
   may persist as an optimization, owner
   [`../behavior/reserved-folders.md`](../behavior/reserved-folders.md)).
5. **Crypto that genuinely cannot ride the plane** (narrowed 2026-08-11,
   R13) — the **roots** (the identity seed and anything recovery must
   reconstruct *before* any store opens: a root can never rest in a store
   sealed under itself, so root recovery and inheritance are always
   ceremonies — paper, hardware token, Shamir shares), **MLS state** (rides
   its own ratified replica —
   [`../behavior/devices.md`](../behavior/devices.md) § Cross-device MLS
   group-state sync; device-owned-epoch invariant), and **device-local
   keys** (the device-only rung — never sync). Still excluded from the
   general plane by construction. **Operational secrets are NOT class 5**:
   MSEK, provider credentials, rotation keys, period keys, content keys,
   and deployment seeds are class-2 kinds registered at the fleet-only rung
   (§ The audience ladder) — they rest on every replica as ciphertext no
   grant can open. The pre-R13 phrasing "key material rides the credential
   store and the provisioning ceremonies" is retired: a separate
   credential-store substrate is rejected (R13's rationale), and only the
   *roots* keep the ceremony path.
6. **Out-of-plane local state** — install-scoped and OS-rendezvous data per
   [`apps/account-scoping.md`](apps/account-scoping.md)'s taxonomy (window
   geometry, install id, caches). Not account data; never syncs.

### The audience ladder (R13 — ruled 2026-08-11)

The class decides a datum's sync unit and merge mechanics; the **audience
rung** decides who can open it. They are orthogonal, both declared per kind
at registration, both frozen — two independent registry columns. The proof
they must never be conflated: DNS provider credentials are *recreatable*
(merge axis: whole-record LWW) **and** *secret* (audience axis: fleet-only)
— reusing a merge table as an audience split ships credentials to grantees.

The rungs, widest to narrowest, each enforced by **which key branch seals
the kind's entries** — never by client-side classification code. That
structural placement is what makes the unknown-field hazard unrepresentable:
an old binary never decides a new kind's audience; it relays ciphertext it
cannot open.

1. **Delegable** — sealed under per-kind keys inside the grant-mintable
   universe: a user-minted, revocable, audited grant hands the kind's
   `{entry_key, item_blind}` pair
   ([`encryption-at-rest.md`](encryption-at-rest.md) § Capability tiering).
   For preferences, relationship state, anything a third party could
   legitimately be granted.
2. **Fleet-only** — sealed under a sibling derivation branch the grant
   machinery **structurally cannot reach**: grant bundles are minted from a
   schedule type that does not contain this branch
   ([`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2
   owns the derivation split), and a standing conformance check asserts no
   `Secret*`-typed field appears in any delegable-rung kind's payload type.
   Operational secrets live here: every replica in the world may carry the
   ciphertext — that is the durability story — and no grant can ever open
   it.
3. **Device-only** — never enters any sync plane (a device's own private
   keys). A rung, not a store.
4. **Ceremony-only** — the roots. Outside every store by construction
   (nothing can rest in a store sealed under itself); recovery and
   inheritance are ceremonies (paper, hardware token, Shamir shares).

**The default rung is fleet-only; delegable is the deliberate exception.**
The asymmetry forces this: a kind can be *re-registered* one rung wider
later (a new kind string, a re-seal), but a kind admitted too wide has had
its plaintext sealed under a key that grants can reach — there is no quiet
withdrawal. Registration starts narrow and widens only on an argued need.

**The total-loss story this buys** (amended by R14): every device gone →
recover the root by ceremony → re-derive `BackupKey` → pull ciphertext from
any custodian — a family member's tablet suffices, with one reachability
caveat made explicit 2026-08-13: *finding and dialing* that custodian with
zero nests is not free — peer-only cold bootstrap is the declared non-goal
(§ The peer leg → Discovery) — so the recovery flow reaches it via any
reachable nest or an out-of-band dial candidate in the M2-invite pattern;
the ciphertext story needs no nest, and the rendezvous story names its
path rather than assuming one. For generation-0-sealed
data (the delegable preference cluster and the seen-set) that alone
suffices; for
generation-keyed data (fleet-only kinds, content scopes — R14) recovery
additionally needs **any one surviving escrow holder** to unwrap the
generation keys (nest by default; redundancy user-tunable). Durability
comes from promiscuous ciphertext replication; secrecy comes from the root
ceremony; *revocability* — real deletion, real removal-severance — comes
from the generation axis, and the escrow requirement is its stated price.
Inheritance is the same flow with a sadder trigger.

**Scopes are the other dimension.** The ladder is per-scope: an account
scope carries these four rungs rooted in the account's root and re-keyed by
the account's succession. A shared-audience datum (family, group) is
**published into a sibling group-rooted scope** (membership lifecycle —
join, leave, revoke), never expressed by widening an account rung: accounts
have *succession*, groups have *membership*, and the two lifecycles never
share a key ladder. **What roots a group scope's keys is a seam (R15):**
messaging groups root in MLS; storage groups key on the recipient-set
scheme (random scope key HPKE-wrapped per member on membership change —
R15 owns the rationale; build design: § The recipient-set scheme, T20
resolved 2026-08-17). The account plane stays
single-principal (§ Substrate settlements). Family custody needs no rung at
all — key-less ciphertext replication is free at every rung (R7 custodian
replicas); family *sharing* is a group scope; family *recovery* is the
ceremony rung.

**Per-rung sub-scopes are the granting shape** (ruled 2026-08-11,
greenfield finding A5 — direction, binding before any per-kind grant
ships). Kind-blinding makes a scope feed unfilterable by kind — every
subscriber receives every row's shape — so a single account-state scope
would let a delegable-kind grantee pull (and observe the existence/timing
of) all the fleet-only churn it can never open. Before grants ship, the
account family partitions per rung (the scope grammar is family-first and
extensible), so a delegable-cluster subscription is narrow and fleet-only
churn is invisible to grantees. **Sequencing sharpened (2026-08-13, the
rows-8/9/20 coherence pass): the partition decision is owed with or
before the R14 generation-schedule build, not merely "before grants
ship."** The R14 sealing gate is what keeps the partition cheap: while it
holds, production rows under the frozen unified `state` scope are
delegable-rung only (the writer door refuses fleet-only origination —
`fauna_sync_engine::account_state_plane`), so the partition can land as a
pure grandfathering — the frozen `state` string becomes the delegable
sub-scope, and fleet-only kinds mint a sibling scope at their first
production sealing, with no row ever moving. The moment the generation
schedule lands and fleet-only kinds begin sealing into the *unified*
scope, the partition becomes the migration this ruling exists to avoid.
**Landed with the R14 build design (2026-08-13, refutable until built):**
the sibling is the `state-fleet` family (§ The scope string owns the
ruling; § The generation machinery the landing) — the window is banked.

**The `UserConfig` disposition (the split line, decided over the
real struct — `libs/fauna-core/src/data.rs`, field-by-field verification
2026-08-11).** The `__config` blob dissolved into per-kind entries — the
rail retired 2026-10-02 ([`config-dissolution.md`](config-dissolution.md)
§ The `__config` dissolution schedule → *The closure order*, step (6); each
field's kind: the same § → *The kinds*). This table is the rung each former
field's kind took; per the default-narrow rule, every field started
fleet-only except the argued delegable set:

| Rung | Fields |
|---|---|
| Delegable | `moderation`, `sync_prefs`, `personalization`, `delegation` — the preference cluster: secret-free, recreatable, each merging independently (no cross-record invariant spans them), the legitimate grant material |
| Fleet-only | `mail` (MSEK + prior MSEKs + credential secrets), `dns` (provider credentials — recreatable ≠ non-secret), `atproto_identity` (renamed from `bluesky` 2026-09-27 — senior rotation keys, client-only-resident), `atproto` (app-credential secrets — `AtprotoAppCredential.secret`), `subscriptions` (period keys), `folders` (content keys), `deployment_seeds` (box-recovery seeds; the single-seed scalar it once sat beside is retired), `backup` (no secrets today, but the reserved `"s3"` destination kind will carry credentials — the rung is chosen for where the row is going) **plus `unattested_destination_marks`** (added 2026-08-12 — landed the day of this ruling and predated by its table; its `Removed` prune reaches into `backup.destinations` at merge time, so it rides in `backup`'s kind — the dissolution schedule owns the grouping), `sections` (default-narrow; vestigial — no production writer, so it gets no plane kind unless a consumer appears), and the succession/witness cluster — `grant_events`, `prior_actor_ids`, `unattested_grant_marks`, `unattested_member_items`, `peer_chain_heads`, `peer_anchor_domains` — whose integrity anchors the theft defenses: signed entries make forgery evident, but these stay fleet-only until a concrete delegation need argues otherwise — **plus the fields the struct grew after this ruling, placed 2026-09-28 by the same default-narrow test at the dissolution table's refresh** ([`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule owns each one's kind): `unattested_filter_marks` (the fourth adjudication plane, the witness cluster's), `follows` (the followed-public-folders list — a recreatable preference, but relationship state a grant has no argued need for, so narrow-first like every kind that is not the preference cluster; re-registrable one rung wider under a new string if a materialized-view consumer ever argues it), `custody` and `group_shares` (ceremony records: the only durable copies of consumed MLS / peer-channel payloads and, for a group share, the held machinery root — key-bearing), `blessed_nests` (a per-nest trust choice that drives grant auto-renewal — authority state, not preference), `nostr_npub_confirmed_at` (the succession aftermath's adjudication stamp — witness state) and `refused_scheduling_changes` (inbound-rail refusals naming authenticated senders and event hashes — nothing a third party is owed) |
| Dissolves | `schema_version`, `min_reader_version`, `updated_at`, `actor_id` — blob-level bookkeeping that becomes per-entry form/merge metadata (with one refinement: the succession-ledger kind carries the account chain — `actor_id` + `prior_actor_ids` — as *value content*, the one place account identity is the data itself); `extra`, the unknown-field catch-all, which has no per-kind analogue: an unknown *kind* is relayed unopened by construction, which is the same guarantee with no client-side list; and `plane_mirrors` (added 2026-08-12 — the CAS-blob bridge's own blob-side anchor, bridge bookkeeping that died with the bridge at closure step (5), 2026-10-01; the rest died with the blob at step (6), 2026-10-02) |

Migration sequencing from the CAS blob to this disposition was
user-deferred at the ruling, **scoped 2026-08-12**, and completed with the
rail's retirement 2026-10-02:
[`config-dissolution.md`](config-dissolution.md) § The `__config`
dissolution schedule owns the phases, the gates, the kind groupings, and
the build record (its § Implementation status today).

**The seen-set rung: delegable (W2.5 item 2 ruling, 2026-08-12; the
production-writer condition is DISCHARGED the same day — the auto-in-set
producer ships, § Implementation status → *Built — W3 the auto-in-set
seen-set producer*; T1's own body-rendered ruling stays refutable until the
browse trigger builds).** `fauna.state.seen-set` was never a
`UserConfig` field, so the table above does not carry it; the same
default-narrow test does, and its argued need is concrete and already
ratified: **materialization (R9) is a grant**, and only delegable-rung
kinds are grant-mintable at any tier
([`encryption-at-rest.md`](encryption-at-rest.md) § Capability tiering) —
a fleet-only seen-set would foreclose materialized read-state (unread
views, cross-device resume, attention-derived digests) permanently, since
a rung widens only by re-registration under a new kind string. The payload
is references — scope-feed coordinates and per-writer watermarks, never
content, never credentials — held checkable by its Secret-free pin
(`fauna_protocol::secret_free`). Costs accepted with the ruling: it seals
under **generation 0** like the preference cluster (no crypto-shred of
seen history until the R14 schedule lands — lower stakes than content:
losing or leaking it exposes attention *references*, and "loss" merely
re-presents items as unseen); and it is a high-churn kind, so once per-rung
sub-scopes exist (A5), delegable-rung grantees would observe its churn
*timing* — the A5 build should weigh giving it its own sub-scope (noted
here so the scope grammar is chosen knowing it; binds nothing today). The
rung is enforced like every other: by which key branch seals it, never by
the manifest or client code.

**The device-endpoints rung: fleet-only (W2.5 item 3 ruling, 2026-08-12).**
`fauna.state.device-endpoints` (§ The peer leg → *Discovery*) takes the
default-narrow answer: no grant-shaped consumer of dial candidates exists —
grantees never dial your devices, and materialization wants derived views,
never endpoints — and the entries are **location data**. The consequence is
deliberate and recorded as such: the R14 gate refuses production
origination of this kind until the generation schedule lands, and for
location data that gate is a *feature* — generation keying is what severs a
removed (possibly stolen) device from reading the fleet's future
addresses — so the explicitly-recreatable R14 exception was considered and
**rejected on the merits**, not just for gate hygiene, even though
endpoints are trivially recreatable. Until the schedule exists, W2.6's
store↔store proofs stage endpoint rows door-lessly (the sanctioned test
pattern); **production peer discovery sequences behind the generation
schedule**, which joins it to the queue that already holds every fleet-only
kind's production sealing.

**The contact-overlay rung: fleet-only, tip-sealed (ruled 2026-09-19;
registered 2026-09-26 — a kind string freezes on first seal, so this one
is now frozen).** `fauna.state.contact-overlay` — one item per person, the owner's
own nickname, notes and labels on them
([`../ui/contacts.md`](../ui/contacts.md) § The private overlay owns the
record, the per-field merge and the succession fold) — takes the
default-narrow answer, and on the merits rather than by default alone. *No
grant-shaped consumer exists*: nothing a third party or a materialized view
could legitimately be handed is served by reading what the owner privately
wrote about other people, and a note about a person is among the most
sensitive things an account holds — about someone who never consented to
its existence. The one imaginable consumer, a nest-side "search by my
nickname", is refused by the same reasoning, and the roster filter that
needs the nickname is local. Should a real consumer ever appear, the ladder's
own rule applies: re-register one rung wider under a new kind string.
**Sealing epoch: `GenerationTip`, for the severance property** — a removed
(possibly stolen) device keeps `BackupKey` forever, so a `Gen0` overlay
would keep opening every future note for it; tip sealing is what ends its
reach at the next fleet mint, the same argument that settled the location
kinds above. The price is the one every tip-sealed kind pays and is
accepted: a write is refused while no admissible, escrow-acked tip resolves
for the device (`fauna_sync_engine::account_state_plane` — the refusal keeps
no local row), which the edit surface reports as an ordinary save error; a
fresh nest + fresh app mints and escrows by default, so out-of-the-box is
unaffected. The payload is user-authored text and **not recreatable**, which
is why its merge policy is CRDT per-field rather than the whole-record
latest-wins the location kinds use — the two columns stay independent, as
everywhere on this ladder. It was **never** a `UserConfig` field and had no
blob-rail twin, so the `__config` dissolution schedule never carried it;
the reserved `__contacts` folder it
replaces was never minted
([`../behavior/reserved-folders.md`](../behavior/reserved-folders.md)
§ Contacts Sync). Registered 2026-09-26 in `fauna_protocol::merge_policy`
(CRDT per-field, fleet-only, tip-sealed; an account-scoped row on departure).

**The read-marker rung: delegable, generation 0 (ruled 2026-09-21 with the kind's registration).** `fauna.state.read-marker` — one entry per fauna-native conversation channel, holding the channel `seq` the user has read through ([`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md) § The read-marker record owns the key, the value, the join and the retention) — takes the deliberate exception, on the argued need the seen-set's ruling above already ratified in so many words: **materialization is a grant, and only delegable-rung kinds are grant-mintable**, so a fleet-only read marker would foreclose *materialized read-state (unread views, cross-device resume, attention-derived digests)* permanently — and unread is precisely the read state those views are made of; the seen-set records observation for custody and was never going to paint a badge (`account-replica-posture.md` § The replica boundary draws that line). The concrete consumer is the one a phone needs: an unread count the user's own nest can compute while every device is closed. A rung widens only by re-registration under a new kind string, and the string freezes on first seal, so the choice is made now rather than narrow-first. The payload passes the same test as the seen-set's — a channel reference and a counter, never content, never credentials, the key blinded before it reaches a nest — held checkable by its Secret-free pin. Costs accepted with the ruling, the seen-set's pair again: it seals under **generation 0**, so a removed device that kept `BackupKey` can go on reading how far the user has read in which channel (references to attention, not content; the tip-sealing that contact-overlay needed for user-authored notes about third parties is not bought here, and cannot be — every delegable kind seals under generation 0); and it is a **churning** kind — one write per thread read — so once per-rung sub-scopes exist (A5) a delegable-rung grantee observes its timing, and the A5 build's note about giving the seen-set a sub-scope of its own applies to this kind with it.

### The generation machinery (R14 build design — ratified 2026-08-13; BUILT the same day, all seven steps — § Implementation status today)

The plane-side design of the R14 generation schedule: what turns "random
generation keys, device-set distributed, escrowed" from ruling into
buildable mechanics. The **key-material half** — stratification rationale,
per-generation derivations, X-Wing wrap format, escrow-target derivation,
key↔id commitment — is owned by
[`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 →
*The schedule build design*, never restated here. This section owns the
machinery **kinds**, the **device-set merge**, the **mint protocol**, the
**escrow doors**, and the **A5 partition landing**.

- **The sealing-epoch axis, and the gate's real shape.** The kind registry
  (`fauna_protocol::merge_policy`) gains a third typed column beside merge
  policy and audience rung: the **sealing epoch** — `Gen0` or
  `GenerationTip`. Delegable kinds are `Gen0` by construction (their branch
  never gains a generation axis); fleet-only *data* kinds (device-endpoints
  and every E3 fleet kind) are `GenerationTip`; the machinery kinds below
  are fleet-only rung + `Gen0` — audience says *who can open* (fleet;
  outside the grant universe, unchanged R13), the epoch says *which key
  material seals*, and conflating them is what made the machinery look
  paradoxically self-gated. The writer-door gate
  (`fauna_sync_engine::account_state_plane::refuse_if_r14_gated`) becomes:
  **resolve the current admissible, escrow-acked generation tip; refuse a
  `GenerationTip` origination if none exists.** That one check *is*
  escrow-before-first-seal (a minted-but-unacked generation resolves to
  the prior tip; no prior tip → refuse), *is* the R14 gate while no
  generation exists, and lifts automatically — per replica, honestly — the
  moment a mint's escrow receipt lands in merged state. Removing the
  boolean refusal is thereby still "the last step of landing the
  schedule", exactly as the code comment demands, and no second gate ever
  exists to drift from the first. An old binary's posture is unchanged:
  it never originates kinds it doesn't know, and relays what it cannot
  open. **Build rulings (step-6 resolver view, 2026-08-13 — the pure view
  is code, `fauna_core::generation::resolve_admissible_tip`, and the
  writer door consumes it since the step-6 landing the same day:
  `account_state_plane`'s sealing-epoch dispatch admits `Gen0`, resolves
  for `GenerationTip`, seals form v2 under the resolved tip, and the
  walk's open path routes v2 rows by their cleartext generation id):**
  three details the ratified text left to the build, **re-armed 2026-08-13
  by the ST-007 security grade** (the R14 escrow-chain adversarial second
  pass: the first cut's admissibility was vacuous over an empty member set and
  satisfiable by any verified subset excluding the observer, and the put
  door acks any caller-named generation — so a crafted wedge mint slipped
  ruling 2's own defense; **the resolver, not `build_mint`, is the trust
  boundary**, and whatever the builder refuses at authoring the resolver
  re-enforces at reading): (1) a **candidate at observer O** is a `Minted`
  row that passes, in order: the **Key↔id binding** (logical key equals the
  recomputed content-derived id of its own core — a row squatting a
  foreign key verifies as nothing, neither candidate nor DAG edge);
  **authenticated authorship** — a non-empty member set, `minter ∈
  member_ids`, and a verifying **minter signature** (Ed25519 over the
  domain-separated content-derived id; the verification key IS
  `core.minter`, since a device id is its Ed25519 public key — so a mint
  is an authenticated statement by the device it names, forgery-refused
  even from a `BackupKey` holder, and a removed minter's mints go
  inadmissible with it via the member check, structural failures counted
  `invalid`); **wholly view-verified** members; an ack by a **trusted
  holder's verifying receipt** (integrity + holder trust + generation
  binding; the exact wrap-hash binding was the depositor's deposit-time
  check — the ciphertext is not merged state); and **O-keyability** — an
  inline member wrap or merged top-up row that opens under O's device KEM
  secret and matches the mint's key commitment, **or, since 2026-09-16, a
  key in O's own retained bundle that matches the commitment** (the bundle
  only ever records a key that opened against its mint's commitment, and
  the fleet-scope reclamation ruling below retires a consumed top-up cell
  from the feed once O's reach says it holds the key — so the bundle is the
  one source left for a spilled member's candidacy; the earlier
  plane-native-only reading was the safer one while cells were permanent,
  and is not once they are reclaimed) (tracked `unkeyable`, not
  `invalid`: a legitimate mint that raced O's enrollment fails it too
  until a top-up lands — the documented offline-window posture, so
  sealing-tip choice is observer-relative by design while **reads stay
  integrity-only**, walking every retained generation); (2) **only a
  candidate descendant retires an ancestor** — the ancestor walk crosses
  shredded and inadmissible intermediates, but a non-candidate descendant
  retires nothing: with (1)'s full predicate this now really does close
  the wedge class it was written against — a mint the observer cannot key
  neither wins candidacy nor retires the tip the observer can key (a
  client-causable unrecoverable state otherwise, so the invariant
  chooses; rulings mutation-red-verified, the wedge shapes pinned in
  `conformance_account_state_walk.rs` § ST-007); (3) the winner among
  surviving candidates is the **byte-order max of the content-derived
  generation id** — order from content, never arrival, the plane's
  standing no-local-preference discipline; (4) **ruled and built
  2026-10-01: a generation a `fauna.state.generation-closed` row names is no
  candidate**, whatever else it passes — it neither wins nor retires an
  ancestor, like every other non-candidate (the kind's bullet, and *The
  mint protocol, trigger (b)*, below). **Accepted residuals,
  re-stated 2026-08-15 (the original bound "partitions last
  until the top-up machinery covers them" was falsified: the accepted
  vandalism could forge the healer's own coverage evidence and defeat the
  cure permanently; the hardening restores the bound):** any `BackupKey`
  holder — a live enrolled member, and equally a **removed** device whose
  `BackupKey` is never rotated — can still author subset mints and forged
  mint variants (vandalism-grade under the accepted posture; sharpened
  2026-08-15 — the earlier "live enrolled member" phrasing
  named a narrower set than the `BackupKey` gate it sat beside), which
  partition *reads* of the affected rows **transiently — until the top-up
  pass, trigger (b), or the unkeyable signal covers them, a bound that
  now holds against forged evidence**: inline coverage is believed only
  for the **id-bound** `member_ids` set (the content-derived generation
  id is recomputed at the coverage read and authenticates the member
  list — checked, never assumed from "inside the signature";
  2026-08-15 — a forged whole `Minted` value squatting an honest key
  wins the mint join, and without the binding check its member list
  suppressed the heal of a victim the honest mint never listed, even at
  healers still holding the key in their retained bundles), top-up
  coverage only from a healer-signed per-healer row by a
  currently-verified non-removed member (the wrap kind's bullet owns the
  mechanism). Same-key `Minted`-variant
  suppression and crafted `Shredded` absorption remain the pre-existing
  availability-vandalism class (lattice-accepted, `BackupKey`-gated,
  root-ceremony-backstopped) — neither confidentiality nor the sealing
  invariant is reachable through them — and since the landing
  (2026-08-15) **no durable case remains**: in-place corruption of a
  *member's own inline wrap* (the member is in the signed set and a wrap
  is present, so healers cannot distinguish it from health — only the
  target can falsify the coverage) self-heals through the target-authored,
  target-signed "cannot key generation G" signal, whose kind bullet below
  owns the mechanism and its anti-churn bound. Every partition in this
  class is now transient, healed by the top-up pass, trigger (b), or the
  signal. **Both bounds were falsified once more on 2026-09-16 and
  restored the same day:** the `Enrolled` record was not
  self-authenticating (the cert covers only the id; the X-Wing wrap target
  beside it was unsigned), so a `BackupKey` holder — a removed device
  included — could file a member's valid cert beside its OWN key, win the
  byte-order join, and have every later heal and mint seal that member's
  generation keys to itself under the member's name: a **confidentiality
  escalation**, and a **permanent partition** (the member cannot open a
  wrap sealed to the forged key, and every healer re-heals to the same
  forged target). The device-set bullet's *The self-signed enrollment*
  owns the fix; with it the two bounds above hold again (the pre-ruling
  binaries' residual exposure it once named went with their unsigned shape,
  retired 2026-09-24 by the compat-remnant sweep ([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program 4)).
- **The machinery kinds** — ordinary class-2 entries (T14 form, in-seal
  writer signature), fleet-only rung, `Gen0` epoch, all in the `state-fleet`
  scope (partition bullet below):
  - **`fauna.state.device-set`** — one entry per device id (= the device
    principal's Ed25519 public key = its NodeId). Value: the device's
    X-Wing device-KEM public key (the wrap target), its root-signed
    `DeviceAuthorization` (a predecessor root's signature counted too until
    the 2026-10-01 re-ruling — the *source of `prior`* bullet below),
    enrollment stamp, and `state: Enrolled | Removed`.
    **Merge is a per-id monotone lattice — `Removed` is absorbing**: any
    merge of `Enrolled` with `Removed` is `Removed`, regardless of stamps
    or arrival order; re-enrolling the same physical machine mints a fresh
    device keypair and therefore a *new* id, so "add after remove" is
    unrepresentable on any id and add-wins resurrection — a failure
    guarded against by review — is
    structurally absent. This merged state is what the Rotation bullet's
    admissible-tip argument consumes: removal-monotone by construction.
    **Authority:** an enrollment entry must carry a valid
    root-signed `DeviceAuthorization` for its id; a removal entry
    is written by any enrolled, non-removed device (the user acts from
    whichever device they hold) or by a root-holding surface.
    **The self-signed enrollment (ruled 2026-09-16; owner of the binding
    between a member's id and its wrap target).** The cert authenticates
    the *id*; nothing in the record authenticated the *X-Wing public key*
    beside it, and the join within the `Enrolled` phase was the byte-order
    max — so any `BackupKey` holder (a live member or a removed device
    whose `BackupKey` is never rotated) could publish, under its own
    writer, a second `Enrolled` row at a member's id carrying that member's
    cert verbatim and the holder's own KEM key, and win: the *Accepted
    residuals* paragraph above records the breach. The fix, additive and
    in four parts: **(1)** an `Enrolled` row carries `device_sig` — Ed25519
    by the device id (the cell key IS the verification key, the
    reach/unkeyable pattern) over a fixed-width domain-tagged preimage of
    the **device id**, the stamp, and the length-prefixed KEM key and cert
    (`fauna_core::generation::enrollment_signing_bytes`; the id is inside
    the preimage so the same bytes re-filed under another cell never
    verify), built only by `sign_device_enrollment`, which derives the KEM
    half from the very secret that signs; **(2)** the join is
    **key-aware** — the merge arm parses the cell key and ranks
    (`Removed` absorbs, self-verifies at this cell, bytes), so a device's
    own signed row is displaced by nothing but a removal, while two
    non-verifying rows resolve by byte order; **(3)** the view admits only
    a row that self-verifies at its cell — an unsigned row, or a forgery
    under a carried signature, is flagged, never a member — while **first
    contact stays permissive**: the device-set arm is deliberately the one
    machinery kind whose adoption checks no signature, since the join and
    the view are where a non-verifying row loses; **(4)**
    `fleet_bootstrap`'s write-if-absent now means *absent this device's own
    verifying row*: a merged row that is unsigned or forged is re-published
    over, signed, once (count-neutral — a writer's put replaces its own row
    at the item), a merged `Removed` is honoured as a stop, and a verifying
    own row is left alone. **Retired 2026-09-24:** the unsigned shape every
    pre-ruling binary wrote — until then still cert-verified at the view,
    with a stated compat posture toward those binaries — by
    the compat-remnant sweep ([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program 4). **Compat posture, as it stood until then:** the field is
    `#[serde(default)]` and skipped when empty, the join returns the
    winning input verbatim (never re-encodes), and no record here decodes
    with `deny_unknown_fields` — so a pre-ruling binary reads, carries and
    re-serves a signed row unchanged; ranking by bytes alone, it even
    prefers the signed row over any *unsigned* forgery (the fourth map key
    raises the record's header byte), and remains open only to a forgery
    carrying junk signature bytes — its pre-ruling exposure, unchanged, and
    closed at every binary that ranks verification first. Old and new
    binaries can therefore pick different winners only where a signed row
    and a junk-signed forgery coexist at one cell, which honest operation
    never produces (each device writes only its own enrollment). Proofs:
    the join pin (a self-signed row beats an unsigned forgery and a
    junk-signed forgery with a fabricated stamp, both orders; red-verified
    against the byte-order join), the tampered-field and foreign-cell
    refusals, the view's refusal of an unsigned row
    (`generation.rs::an_unsigned_enrollment_is_flagged_not_a_member`), the
    dispatcher pin, the bootstrap's three re-publish cases,
    and the real-plane pin
    (`conformance_account_state_walk.rs::a_backup_key_holder_cannot_redirect_a_members_wrap_target`).
    **The group-roster twin is deferred, deliberately:** the recipient-set
    scheme's `Enrolled` roster record carries `reception_pubkey` outside
    `authority_sig` (whose preimage is the entry id alone), the same shape
    — but its signer is the *authority device*, not the member, and the
    member's reception key is already member-signed elsewhere in the
    ceremony (`sign_group_reception_published`), so the binding there is a
    T20 design choice (authority co-signs the observed key, or the row
    carries the member's own signed publication) with a legacy re-sign
    story of its own; it is captured as its own security-review row rather
    than bolted on here.
    **Reader-side realization (step-3 build ruling, 2026-08-13 —
    `fauna_core::generation::FleetView`, built the same day):** authority
    is a pure, deterministic **view over merged rows** — never a merge
    precondition. *Membership is cert-verified* — an id is a member (and
    wrap target) only if its `Enrolled` row's embedded cert verifies:
    signed by the account root — and by nothing else: the arm that also
    admitted a prior actor id's signature left with the 2026-10-01 re-ruling
    (the *source of `prior`* bullet below) — covering exactly that row's
    id, expiry anchored to the enrollment's asserted instant (never a
    clock — a time-dependent membership view would diverge across
    observers), capabilities deliberately not consulted (membership is
    bundle-level: sealing under the fleet-only branch is the possession
    proof — the admission seam's carrier-level precedent). *Exclusion is
    unconditional* — a `Removed` row excludes its id whatever its
    attribution, for two provable reasons: a writer-authority
    *precondition* on removals reads non-monotone merged state, so two
    replicas meeting a mutual-removal race in opposite orders would refuse
    opposite rows and diverge permanently (breaking exactly the
    removal-monotonicity the admissible-tip argument rests on); and
    refusing a "stop trusting" signal on verification grounds would keep a
    stolen device in the wrap-target set — honoring removals eagerly is
    the fail-safe direction, with the blast radius exactly the accepted
    posture below. *Attribution is advisory* — `removed_by` is classified
    (root / member / unverified) for audit surfaces only; the write-side
    rule above stays the honest-writer contract, and the stricter removal
    authority noted below lands additively with this view as its extension
    point. The view is likewise the admissibility primitive: a tip is
    admissible only if **every** id in its mint's member set is a
    verified, non-removed member of the observer's view — an id the
    observer cannot verify (unknown, cert-invalid, or removed) makes the
    tip inadmissible there; view-verification is necessary, not
    sufficient — the resolver rulings (the gate bullet above) own the
    full sealing-candidacy predicate, whose authenticated-authorship and
    observer-keyability clauses closed ST-007's vacuous-subset hole. **The
    accepted posture, stated:** a stolen-but-not-yet-removed device can
    race removals against the fleet — vandalism, not compromise-escalation
    (a live enrolled device already holds `BackupKey` and authoring); what
    it may **not** do is keep *itself* enrolled by mis-stating its nest row
    (*Fleet-scope reclamation* clause (4), *A disagreement is the user's to
    settle*); the
    backstop is the root ceremony (re-enroll fresh devices; data recovers
    via retained keys + escrow), and stricter removal authority (quorum,
    root-only) is a post-W5 opt-in narrowing in the T8 pattern, not a
    default. **Relation to nest device rows:** the nest's device/session
    rows (`devices.md`) remain the connection-auth and bearer truth; this
    kind is the *fleet-membership truth* for generation admissibility and
    wrap targeting. The app's remove-device action writes both; a nest-side
    deletion alone does not sever generations — the plane removal record is
    what mints the exclusion. **Membership is per-account and role-defined,
    never machine-typed** (resharpened 2026-08-13 — the first pass's "the
    nest itself is never a member" invited the wrong machine-typed
    reading): this kind lists **the account's reading fleet** — exactly
    the enrollments whose bundle carries `BackupKey` (R7's reading
    posture). Whatever serves the account keylessly — a docker nest, a
    friend's app hosting a custodian replica (which is simultaneously a
    reading fleet member *of its own owner's account* — R7's per-account
    per-replica axis), an own relay-only kiosk enrollment — never appears
    here, so mint wraps never target a serving replica and the
    identity-targeted escrow wrap remains the only generation key material
    that ever rests with one. "Nest" names roles a store holds *toward an
    account* (R8's decomposition), not a species of machine. The guard is
    structural, not procedural: machinery entries seal under the
    fleet-only gen-0 branch (`BackupKey`-derived), so a keyless replica
    **cannot author** a device-set entry even while holding a valid
    root-signed `DeviceAuthorization` — the kiosk case: authorized to
    sync, structurally unable to join the wrap-target set. A reading
    bundle is minted for user devices only; inventing one for a box would
    re-create exactly the standing-key exposure R14's escrow indirection
    exists to avoid.
    - **The source of `prior` — ruled 2026-09-13: ATTESTED by possession, never asserted by a replica. Re-ruled 2026-10-01: it signs nothing in this view.** `prior` is the device's attested predecessor set — **the actor ids whose seeds this device's account registry holds** (the ids of `AccountRegistry::predecessor_backup_keys_by_actor`: a registry row exists only because this device ran the ceremony or restored the seed out of the successor's own escrow container, and a seed derives its id, so possession *is* the attestation). Until the 2026-09-13 ruling the runtime read the set off `UserConfig::prior_actor_ids` in the device-local `__config` replica: a **writer-asserted** list (`seal_user_config` signs nothing, the re-key records the blob's own claimed `actor_id`, and the predecessor-replica fold unions the predecessor's copy verbatim), carried across the ceremony by the very pass the aftermath ruling marks every other plane for ([`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md) § Adjudicating what the aftermath carries across), with **no mark plane** of its own — nothing on the struct can say an id was carried across, so "mark it" is not a fix. The replica's list is **not consulted at all**, not even intersected: intersecting an attested set with an unattested one narrows nothing and adds an offline dependency. A **seedless host** (the sync agent) holds no registry; its attested set is what the identity-holding app hands it beside the retired keys — the additive `SyncCapability.predecessor_actor_ids` ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model) — and an app that has not wired it leaves that host's set **empty**, the fail-safe direction.
      **What the set is for.** It names whose retired material this device may *open with*: the predecessor slots whose retained generation keys it carries into the successor's slot, and the retired keys its walk trial-opens under — the delegable schedules ([`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md) § Re-key scope) and the mint kind's keys ([`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 → *Rotation*, the succession rider, which owns how the machinery crosses). It also names whose writer ids on this machine the peer leg counts as its own.
      **It is no signer in this view (the 2026-10-01 re-ruling).** Until then the view admitted an `Enrolled` row whose cert a `prior` id had signed, on the reading that a pre-succession enrollment crosses the succession and stays predecessor-signed for the device's life. It does not cross. A device's keypair is per (machine, account), a successor's generation-0 schedule derives from its own seed, and the walk opens none of the predecessor's fleet-only rows, so every device enrolls afresh under the successor root and no predecessor-signed row ever opens on a successor's replica. The arm verified nothing in production. What it did was stand ready to admit every device the retired root ever enrolled — a seed thief's self-enrolled one among them — the day anything opened those rows, with removal as the only control. So the view verifies a cert against the account root alone, and "excluded exactly like a removed device" needs no removal. The view stays clock-free and deterministic over merged rows plus the identity line, and a verdict may flip only toward acceptance. The cost paragraph and the argument against a succession-time bound that stood here described the admitting view and went with it. A group scope's authority view keeps its own use of the attested set; that is the recipient-set scheme's and is not ruled on here.
  - **`fauna.state.generation-mint`** — one immutable entry per generation,
    logical key = the content-derived generation id. Value: parent tip
    id(s), the member set (device ids, from merged device-set state at
    mint), minter device id, the key commitment
    (`owner-key-material.md` owns it), the **minter signature** (ST-007:
    the minter's Ed25519 signature over the domain-separated
    content-derived id, sibling of the core so the id derivation is
    unchanged; required, not optional — the compat set was provably empty
    when it landed 2026-08-13: the only production mint writer is the
    first-need trigger, reachable only from a `GenerationTip`
    origination, and no production surface originated any `GenerationTip`
    kind before the device-endpoints writer), the per-member **X-Wing
    wraps** of the generation key, and the mint stamp. Concurrent mints fork the tip
    exactly as ratified (Rotation bullet); the entry never changes after
    write — and the shred marker for a deleted generation is the **in-value
    absorbing `Shredded` state** of the same record, not a T14 tombstone
    (build ruling, 2026-08-13, step 1: CrdtPerField structurally refuses
    tombstones — the E0 convergence law — so deletion is a lattice phase,
    exactly like the device-set's `Removed`; `Shredded` retains the mint's
    DAG core, which is gen-0-readable metadata regardless, and drops the
    wrap ciphertexts so honest replicas stop carrying unwrappable key
    material for the dead generation).
  - **`fauna.state.generation-wrap`** — the top-up and re-escrow vehicle.
    Written by any device holding the key, targeting any *enrolled,
    non-removed* device lacking one — the self-healing path for
    enrollment/mint races ("the mint that raced my enrollment"), for
    handing older retained generations to a newer device, and for the
    succession re-escrow. Never targets a removed id (writers check merged
    device-set state **at each wrap's write**, never through a view read before one of the pass's yields — the devices page's removal is a local command served at exactly those yields; a receiving device verifies the key commitment
    before trusting any wrap). **Cell shape (hardening ruled per the
    security-review's finding *the healer that believes
    the vandal* — full verdict in the internal review archive under that
    title): logical key = (generation id, target device id, healer device id) —
    one cell per healer** — value = a healer-signed record (`at_ms` and
    the wrap bytes' hash inside the signature; the healer id is the
    Ed25519 verification key, the `minter_sig` pattern). The kind merges
    `CrdtPerField`: within a per-healer cell a **verifying record beats
    any non-verifying bytes** (then signed stamp, then bytes) — so a
    `BackupKey` holder without that healer's device secret can neither
    displace nor pre-empt an honest heal, which is what makes the top-up
    pass's skip decision forgery-proof: it counts as coverage only a
    verifying row by a currently-verified, non-removed member. The
    pre-hardening two-segment cells (unattributed value, outer-stamp LWW),
    readable and dual-written as transition courtesy until then, were
    retired 2026-09-24 by the compat-remnant sweep ([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program 4): a two-segment key is not a cell
    of this kind, refused at adoption and merge alike. No burn at
    succession is owed (it was, until the rider's 2026-10-01 re-ruling, as
    an authenticated absorbing in-value variant): the predecessor's cells
    are sealed under its generation-0 schedule and target its devices'
    ids, so they are no part of the successor's merged state
    ([`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 →
    *Rotation*, the succession rider).
  - **`fauna.state.escrow-target`** — an identity's published X-Wing
    escrow public key (+ future additional holder targets, additively),
    written once per identity by a seed-holding surface at the
    **per-identity key `identity/<actor-id-hex>`**
    (`fauna_core::generation::escrow_target_identity_key`) — one Immutable
    row per identity the account has had, so a successor publishes its own
    beside the predecessor's (the constant key `identity` it replaced
    could only ever hold the first identity's). The same string is the
    escrow wrap's AAD target key and the receipt's signed target key.
    Without the current identity's row no mint can escrow and fleet
    sealing stays refused with a precise error.
  - **`fauna.state.escrow-receipt`** — one entry per (generation id,
    holder, identity), key `<generation-hex>/<holder-hex>/<actor-id-hex>`
    (`escrow_receipt_cell_key`): the holder-signed durable-receipt payload
    (holder id, wrap hash, **target key**, stamp — the door signs the
    target key the deposit named), written by the depositing device after
    the door replies. **This row is what the writer door's tip resolution
    checks**, counting only receipts whose target key is the observer's
    own identity's — a predecessor's receipt vouches for a wrap the
    succession burned, so it acks nothing for the successor, who re-escrows
    (owner-key-material.md § Rotation, the succession rider). Escrow
    status is merged plane state, never a live nest query, so the check
    works offline and in the no-nest profile identically.
  - **`fauna.state.generation-unkeyable`** — the target-authored "cannot
    key generation G" signal (built 2026-08-15 — the cure for the
    corrupted-inline-wrap case the accepted-residuals bullet retired). One
    cell per (generation, target), two-segment key; rows verify ONLY under
    the **target's own device key** (segment 2 IS the Ed25519 verification
    key — the `minter_sig`/`healer_sig` pattern), ranked
    (verifies-at-cell, signed stamp, bytes) — unforgeable and
    un-squattable by construction, adoption-strict like the wrap kind,
    its join decode-or-fail (`transport.md` § Schema and forward-compat
    discipline → *Rule 3 in full*, the `consensus` ground: a side that
    does not decode fails the merge, never ranks lowest), no tombstones. `Asserted` names the **tried-and-failed
    evidence**: BLAKE3 hashes of the wrap ciphertexts the target attempted
    (its own inline entries on the winning mint row + verifying
    per-healer rows by currently-verified non-removed members; sorted,
    deduped, capped by byte order — legacy/unverified wraps are still
    tried on read but never listed, so unauthenticated junk cannot evict
    the hashes that gate). `Satisfied` retracts at a later signed stamp,
    same cell. **The anti-churn contract:** the target asserts only where
    it cannot key AND the pair is apparently covered (a plainly-missing
    wrap is the ordinary top-up pass's job), re-asserts only when the
    evidence set changes, and skips any mint row that fails the
    resolver's own authorship gate or whose minter is not a
    currently-verified non-removed member (key↔id binding, ST-007
    authorship via the shared `verify_mint_authorship`, verified minter —
    the binding alone stops a squatter but not an invented core, whose id
    is its own hash; so neither a squatted nor an invented mint, a removed
    device's self-signed one included, extracts testimony); a healer treats a
    verifying assertion as clearing BOTH suppression grounds, bounded to
    **one fresh wrap per healer per assertion** (publish iff its own cell
    holds no verifying row or one whose hash the assertion names);
    publications into an own cell stamp `max(now, cell stamp + 1)`, so a
    clock regression cannot freeze a superseded record. Net bound: a
    crashed target extracts at most one wrap per healer, ever; a
    malicious target's amplification is ≤ fleet size per self-signed row
    (self-inflicted, vandalism-grade); a converged pair is byte-quiet on
    both sides.
  - **`fauna.state.device-reach`** — the target-authored "I hold these
    generations" statement (ruled + built 2026-09-16 — the *Fleet-scope
    reclamation* bullet below owns why it exists). One cell per device id,
    logical key = the device id hex; rows verify ONLY under the device's own
    key (the key IS the Ed25519 verification key — the unkeyable kind's
    pattern), ranked (verifies-at-cell, signed stamp, bytes),
    adoption-strict/join-total, no tombstones. Value: the sorted ids of the
    live `Minted` generations the device can key, and the signed stamp. It is
    **coverage evidence** for the top-up pass — a verifying reach by a
    currently-verified, non-removed member that lists G covers (G, that
    member) ahead of any per-healer cell — and it is what lets a healer
    retire its own cells: a cell is redundant the moment its target says it
    holds the key. A device refreshes its row only when the set changes
    (count-neutral: same item, same writer), never on a cadence, and the set
    shrinks as generations shred, which is what keeps the row under the
    per-entry byte cap over a long life. A device with no reach row has
    nothing of its retired by anyone but itself.
  - **`fauna.state.generation-closed`** — the remover's "seal nothing more under G" statement (ruled and built 2026-10-01; *The mint protocol, trigger (b)* below owns why it exists and who writes it). One cell per generation, logical key = the generation id hex. Value: the closing device's id, the removed device's id it answers, and a stamp — audit only. **Nothing in the value is consulted; the row's presence is the statement.** So the kind merges by byte-order max and carries no signature: two rows at one cell say the same thing, and no forgery can make a closed generation open. Adoption checks only that the key parses as a generation id and the value decodes. The reader rule is resolver ruling (4), in the gate bullet above; reads are untouched and walk every retained generation. A row is live until merged state reads its generation `Shredded` — never merely until the mint row is absent, which a row arriving ahead of its mint over another leg would be. It is retired with the generation's other rows when the generation shreds (*Fleet-scope reclamation*, clause (3)(e)), and its relay copies are then forgotten under every writer as a dead gen-0 item's are (clause (3)(h)). Additive: a binary that does not know the kind carries the row unopened and resolves as before.
- **The mint protocol.** Triggers: (a) first need — a `GenerationTip`
  origination finds **no candidate tip for this observer** (the resolver
  rulings' full predicate: none admissible, acked, and keyable here) and
  the engine mints instead of refusing forever (provided an escrow target
  exists and a holder is reachable; otherwise the refusal stands and says
  why). "No candidate resolves" IS first-need, whatever rows exist: the
  build's earlier narrowing — refuse whenever *any* mint row exists — was
  the wedge behind ST-007 and is retired (2026-08-13). The
  first-need mint names **the current mint-DAG leaves as its parents**
  (every id-bound row no other id-bound row supersedes, capped at a
  hard-coded constant by byte order under flood), so a heal across
  attacker or orphan rows keeps the supersession edge and retires them
  where they are candidates rather than forking a parentless second root
  — an empty DAG yields the empty parent list, "an empty list is the
  first generation", unchanged. (b) a device-set removal — never a mint of
  its own: a removal leaves no generation the removed device may key as a
  sealing candidate, and (a) then mints at the next origination (the
  *trigger (b)* bullet below owns the rule; ruled and built 2026-10-01 —
  § Implementation status today); (c) cadence — a hard-coded Rust constant
  (no knob: no human would choose a rotation cadence, the
  DKIM/TLS-automation precedent); (d) succession — never a mint of its own
  either: no generation the predecessor's devices minted is a candidate
  under the successor, so (a) mints at the successor's first tip-sealed
  origination, over the carried generations as its parents (the Rotation
  bullet's rider owns the rule; re-ruled and built 2026-10-01 —
  § Implementation status today). Any enrolled device may mint —
  convergence handles forks — but the engine-singleton role (W5) is the
  *preferred* minter per machine, keeping forks rare-and-short-lived as an
  operational property (R6's star-topology argument, restated for mints).
  Mint sequence: read merged device-set → mint random key → build wraps
  (every enrolled member) + escrow wrap → **deposit escrow first**, obtain
  the receipt → **re-check every listed member against merged device-set state** (a member removed while the deposit was in flight — the removal is served at that await — refuses the mint rather than filtering it, since the member set is inside the content-derived id; the orphaned holder wrap is the harmless state below, and the next origination re-mints over the fresh view) → write mint entry + escrow-receipt entry (+ the door's
  durability already held). **The deposit is the sequence's one online step; its rows are local-first like every other write (stated 2026-10-01):** they are written to this replica's journal — the mint row, the spill top-ups, the receipt row last — and then sent in journal order, and a send that fails does not undo the mint: the holder's receipt is durable, the tip resolves on this replica, and the next pass's publish step sends the rows ahead of anything sealed under them. A mint whose entry would not seal within the
  plane's per-entry byte cap ([`account-sync-plane.md`](account-sync-plane.md)
  § Implementation status today → *the writer door refuses an entry no nest
  will accept*) is refused **before the deposit**, and says so: that entry
  could never publish, and depositing anyway orphans a holder-side wrap on
  every attempt. A crash between deposit and publish leaves an
  acked-but-unpublished generation: harmless (never sealed under, re-mint
  supersedes; the holder's orphan wrap is garbage-collectable by wrap
  hash). The **loser re-seal pass** is engine work as ratified: each
  writer re-originates its own loser-sealed entries under the winning tip;
  an emptied loser is marked `Shredded` and its wraps deleted. Its shape is
  ruled as the own arm of *Fleet-scope reclamation* clause (3)(g), which
  owns it.
- **The mint protocol, trigger (b) — a removal closes every generation the removed device may key (ruled and built 2026-10-01).** What a removal owes is that nothing more is sealed under a generation the removed device may key. Trigger (a) already mints whenever no candidate resolves, so the whole of (b) is what stops such a generation being a candidate. **The member rule alone does not.** A tip whose mint names the removed device is inadmissible wherever the `Removed` row merges (the device-set bullet), and that was read as the rule. It misses a device that enrolled after the tip was minted: it keys the tip by top-up, is no member of its mint, and its removal unseats nothing — no origination mints, and it holds the key of every row sealed from then on. Measured 2026-10-01, and the ordinary shape of a fleet, whose first device mints at its first write and whose every later device is a top-up target. It misses the other side too (read from the resolver, not measured): where the tip does name the removed device, resolver ruling (2) retires an ancestor only under a *candidate* descendant, so a live ancestor that does not name it resolves in the tip's place, and a healer may have handed the removed device that key as well. **The closure is the rest.** The device that writes another device's `Removed` row closes, in the same act, every generation that could still be a sealing candidate: each `Minted` row of its merged state that is id-bound, passes the authorship gate, and whose members its view wholly verifies with the removed device already excluded. It writes one `fauna.state.generation-closed` row per such generation (the kind's bullet above), journalled ahead of the `Removed` row, so a removal on record is never without its closures; a crash between the two costs one mint, and the staged intent still writes the removal. A closed generation is no candidate at any observer that merged its row. The next tip-sealed write there — the remover's or a sibling's — mints by (a): the mint names the DAG leaves as its parents, lists no removed device, and no row closes it, so every observer that merges it resolves it and none mints again. The fleet mints once per removal, with (a)'s own bound that two originations racing each other's mint fork. No cap is needed: an admissible generation takes a verified member's device secret to author, so a flood of invented mints draws no closure rows. **Why the statement sits on the generation.** The open question was what makes a later mint *covering*. A mint's member set cannot say: it leaves out a device that enrolled after it exactly as it leaves out one removed before it. Three carriers were weighed and refused. *A list of covered removals in the mint* changes the preimage of the content-derived id, grows with every removal still in merged state, and makes a mint's standing depend on when each observer forgets the removal evidence ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 7 owns when that is). *A list of unseated generations on the `Removed` row* is displaced by any later `Removed` row at the same cell — the join keeps one input — and leaves with the evidence. *Evidence that the removed device keyed the tip*, its top-up cell or its reach, is what the reclamation pass retires, and a stolen device publishes no reach. A row per generation needs none of it: "is this mint covering" becomes "is this generation closed", which every observer answers from one row that lives exactly as long as the generation does. **Who closes.** The remover: the Devices page's row gesture, the member-addressed door, and the reconcile that finishes either, all through the one writer (`fauna_account_plane::fleet_removal::write_removed`). *The remover mints* was weighed as the whole rule and refused as one: it needs an observer rule beside it for a removal the minter did not write, which is the covering question again. **A sign-out closes nothing.** The leaving device erases its keys in the act that writes its row, so no holder is left to mint past; the member rule still unseats a tip that names it, as before, and the resolver's fall-back to a live ancestor stands there — every generation live at an earlier remover's removal was closed then. **Who the mint severs, stated because it is narrower than it reads.** A removed seed holder recovers any generation from its escrow wrap (*Escrow recovery*, (4)), and every app is seed-holding today, so today a mint past a removal takes nothing from the removed machine that its seed does not give back; succession is the remedy there. What it severs is a seedless reading replica: a machine approved from an existing device ([`account-replica-posture.md`](account-replica-posture.md) § *Seed residency*), and under the two narrower postures every device but the primary, or every device. Removed, such a machine keeps `BackupKey` and each generation key it held, and no session at any nest. The generation key is then the one layer between it and ciphertext it obtains some other way, and that layer is what R14 exists to add; minting past only the devices a tip happens to name made it a property of enrollment order. The rule is one rule for every removal because the plane cannot know which removed device held the seed, and guessing wrong in the cheap direction is the unsafe one. **What it costs.** One generation per removal a remover writes — the mint, its receipt, the spill top-ups, and one re-seal of the scope's account-level rows, all reclaimed (*Fleet-scope reclamation*) — and one closed row per generation live at the removal, retired with it. Removals that land before the next origination share one mint. **Minting ahead of the next origination is not adopted.** It was (b)'s remaining scope while (b) was read as "sooner". With the closure it buys no confidentiality: a writer that has merged the closure mints before it seals, and one that has not would not hold the early mint either — both ride the remover's log, in order. What is left is a mint made while the holder is known reachable and fewer racing first-need mints; nothing schedules it. **Bounds, stated.** (i) Severance is as real as propagation, as everywhere in this section: a device that has not merged the closure seals under the closed generation until it does. (ii) A generation the remover could not see as a candidate — one its merged state did not hold, or one naming a member it could not yet verify — is not closed; the removed device keys it only if it is no member of that mint and a healer that had not merged the removal topped it up. (iii) A removal written by a binary older than this rule closes nothing, which is today's gap for that removal; a binary older than this rule that merges a closure does not read it, and seals under the old tip until a closing sibling's mint reaches it, which it adopts like any candidate descendant. (iv) A closure is honoured whatever wrote it, as the removal it follows is: a `BackupKey` holder that can still reach the plane can close a tip and force a mint — vandalism of the grade a forged `Removed` row already is, and cheaper to undo.
- **The bounded mint (ruled + built 2026-09-15).** One inline wrap is about 1.3 KB as encoded (an X-Wing
  encapsulation plus the AEAD'd key, carried as a CBOR byte string since
  2026-09-28 — it was about 2.4 KB as the integer array serde writes for a
  bare `Vec<u8>` when the defect was found) and the plane caps a sealed entry
  at 64 KiB
  ([`account-sync-plane.md`](account-sync-plane.md) § Implementation status
  today → *the writer door refuses an entry no nest will accept*), so a mint
  wrapping every member stopped fitting past about 27 devices, and such a
  fleet could never mint again — every `GenerationTip` kind refused at the
  writer door for good. The shape that closes it, all in shared Rust:
  **(a) The inline set.** The mint still LISTS every verified member in its
  signed `member_ids` — admissibility, severance and coverage arm A range
  over the whole fleet exactly as before — but wraps inline only the
  minter (always: candidacy at the minter is decided by what the plane
  distributes, never by its retained bundle, and a self-top-up is noise the
  pass never writes) plus the first `MAX_INLINE_MEMBER_WRAPS − 1` other ids
  in byte order (`fauna_core::generation::inline_wrap_members` — pure, so
  every reader of a mint row names the same set its builder did). Every
  other member is the **spill**, reached by the minter's own top-up rows in
  the ordinary healer shape (the per-healer v2 cell plus the legacy courtesy
  row, `generation_topup::put_heal`): the minter is simply the first healer,
  its rows are coverage under the wrap kind's own rule (a verifying row by a
  verified, non-removed member), and the ST-007 admissibility predicates are
  untouched — O-keyability already accepted a merged top-up. **(b) Log
  order is the contract.** The sequence writes the mint row, then one
  top-up per spilled member, then the escrow receipt LAST, so an escrow-acked
  mint is one whose every wrap already precedes it on the minter's log, and a
  reader holding the receipt holds the wraps (one writer's rows are served in
  log order on both legs). A spilled member that originates before its top-up
  lands sees the mint as `unkeyable` — the enrollment-race posture already
  documented, healed by the ordinary pass (arm A finds it listed-but-
  unwrapped) and never an unkeyable-signal storm (a plainly-missing wrap is
  the pass's job under the anti-churn contract). **(c) The ceilings,
  measured.** `MAX_INLINE_MEMBER_WRAPS` (8) and `MAX_MINT_MEMBERS` (512) are
  hard-coded constants (invariant bucket (1)): a listed id costs 34 B, an
  inline wrap about 1.3 KB and a full `MAX_MINT_PARENTS` list about 1.1 KB
  as encoded, and a mint at the member ceiling with a full parent list seals
  to about 29 KB, under the cap with wide headroom — pinned by measurement in
  `fauna_account_plane::generation_mint`'s tests, never by the two numbers
  alone; `build_mint` refuses a larger fleet before any key material exists,
  and the sequence's pre-deposit size check stays as the backstop. The inline
  cap is small on purpose: the spill costs the same rows whether the cap is 8
  or 16, while every inline wrap comes out of the member list's byte budget.
  **The count cap binds earlier for a large fleet:** with the legacy courtesy
  row a generation over N members costs about 2(N − 8) + 2 live entries on
  top of one device-set row per device *ever enrolled*, against the scope's
  4096-entry cap — a 64-device fleet affords about thirty-five generations, a
  512-device fleet three — and until 2026-09-16 nothing reclaimed a superseded
  generation's rows or a removed device's. (The figures this sentence carried
  before that date — "about twenty" and "two" — did not follow from its own
  formula; the *Fleet-scope reclamation* bullet below owns the remedy and the
  conformance test that now measures the count instead of deriving it.)
  **(d) Encoding, re-ruled 2026-09-29: byte strings, for the wrap bytes and
  the ids alike.** The 2026-09-15 ruling kept the integer-array carriage of
  `member_ids`, `parents` and the wrap bytes because a bytes carriage changes
  the preimage of the content-derived id and the at-rest bytes of an
  immutable kind for every pre-change reader — a wire break inside the major
  version for a 2× gain the bounded shape made unnecessary. The 2026-09-24
  baseline reset ([`version-compatibility.md`](version-compatibility.md)
  § Dimension 2, the fourth ratified exception) voids the pre-change reader,
  so the wrap bytes moved to a CBOR byte string on 2026-09-28, and the fixed-width ids — `member_ids`,
  `parents`, and every other `[u8; 32]` on the mint and its siblings —
  followed on 2026-09-29, before the 2026-10 baseline, under the tree-wide
  rule [`serialization.md`](serialization.md) § Canonical IPLD dag-cbor →
  *Fixed-size byte arrays* owns (34 B per id instead of up to 66; every
  generation id re-derived, which is why it landed before the 2026-10 baseline and
  never after). The pin (`fauna_account_plane::generation_mint`'s ceiling
  test) measures exactly **34 B per listed member and 34 B per parent**,
  about 1.24 KB per inline wrap, and about 28.9 KB for a mint at the member
  ceiling with a full parent list; the integer-array baselines were never
  pinned. A second record variant emitted only past the v1 ceiling remains
  the escape hatch if that ceiling is ever reached in practice; nothing
  schedules it. **(e)
  Device-set growth, ruled: NO automatic leave — membership ends by removal,
  never by silence.** The view is clock-free and deterministic, a verdict may
  only move toward acceptance (the `prior` bullet above), an idle device is
  indistinguishable from an offline one, and removal is the ruled control; so
  every enrollment stays a wrap target until removed. The removal a sign-out
  owes the plane — the machine's standing ends everywhere
  ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md)
  § Implementation status today, the RULED + BUILT 2026-09-14 entry) — was
  deliberately NOT ruled with the bounded mint: each removal unseats the tip
  and mints, and under the count-cap arithmetic above a sign-out per test
  would have exhausted the fleet scope within a few dozen cycles. It landed
  with reclamation (the *Fleet-scope reclamation* bullet, clause (4)). **(f)
  No over-cap row to heal.** Every row the plane journals, a mint row
  included, passes the per-entry cap door (`SizedEntry`) before it lands
  locally, so no replica holds a mint row over the cap. The re-bounding heal
  `publish_pending` once ran on a pre-bounding mint row an older build left
  local was removed at the compat-remnant sweep
  ([`version-compatibility.md`](version-compatibility.md) § Dimension 2, the
  fourth exception): no such store exists. **Compatibility, additive:** a pre-change
  binary mints all-inline as before (and refuses past ~27, as before), keys a
  bounded mint inline or through the minter's top-up exactly as it keys a heal
  today (v2 cells since 2026-08-15, the legacy row before that), and its own
  top-up pass finds every spilled member already covered by the minter's
  verifying row; the only skew-window cost is a redundant per-healer cell from
  a healer that walked the mint row but not yet the top-ups behind it.
- **Fleet-scope reclamation (ruled + built 2026-09-16).** The fleet scope's live-entry cap counts one row per
  `(item, writer)`, superseded only by the same writer re-putting the same
  item; every machinery row of a superseded generation, every row a removed
  device ever wrote, and every top-up cell a device has already consumed was
  therefore permanent, so a fleet spent the cap at the rate of one generation
  per removal and one device-set row per enrollment. The remedy is three
  mechanisms and one product ruling, each stated so it can be re-argued:
  **(1) The nest reclaims its own feed, behind a retention gate.**
  `fauna.account.state.retire` names a live row by its cleartext coordinates
  (scope, blinded item key, writer, writer seq) and the nest marks it
  superseded **inserting nothing** — the one plane write that needs no cap
  headroom, which is what makes a full scope recoverable from an app with no
  user act (principles.md § Client-recoverable nest state; question (d) of
  the row). The row's bytes and coordinate stay (the seq-reuse memory is
  intact, so a replay of the retired row is refused as before). The gate:
  **a row is retired only once every walker of the scope has walked past
  it** — a walker being a principal that has sent the feed its `walker_id`
  beside the serve-order watermark it banks (`held_through_seq`, the claim
  the walk already makes; the nest keeps the max per (scope, walker) in
  `state_walk_marks`), and a mark *counts* while its key holds a live,
  non-tombstoned grant on the account (a signed-out or deleted device's
  mark drops with its grant; a dead machine blocks reclamation exactly until
  the user removes it — removal is the control, as everywhere in this
  section). The gate is what makes a retire safe against every stale
  replica, the peer leg included: a replica that could still hold, or serve
  a peer, the pre-retirement state has by construction walked the row that
  supersedes it. Refused `not_yet_stable`; the caller retries next pass —
  **and asks only what the gate can accept (*the gate's watermark*, ruled
  2026-09-22):** the class-2 feed reply carries
  `retirable_through_seq`, the lowest counted mark of the scope (stamped
  after the requesting walker's own mark lands, so a converged walk never
  reads its own lag as the fleet's); the walk keeps each relay row's own
  serve coordinate (`RelayRow::feed_seq`, nullable and additive at rest,
  stamped from the put reply for an own row and from the page for a walked
  one); and the reclamation pass **withholds** the retire of any row above
  the watermark without asking, counting it `withheld` and keeping it exactly
  as a deferred row is kept. The rule is exact rather than a backoff — it
  predicts the nest's own verdict from state the pass's walk just merged —
  so it is computable on the first pass of a fresh runtime, never delays a
  retire the gate would accept, and moves no safety argument (the nest gates
  every retire it receives regardless). Not withheld: a row whose coordinate
  is unknown (a store-served leg's, one recorded before the column, an own
  row no reply stamped), every retire against an older nest that serves no
  watermark, and the sign-out's `sever_self` leg, which runs once with the
  nest's answer as its only witness. What this buys: a fleet with a dead
  walker costs no request per removed-device row per pass — some 240 refused
  retires per pass by the middle of a whole-suite sweep — and removal stays
  the control: the moment the user removes the dead machine its mark stops
  counting, the next walk reads a higher watermark, and the withheld retires
  go out and land. A
  retire may also carry `no_rows_sealed_under: G`, refused
  `generation_in_use` while any live form-v2 row in the scope names G in
  its cleartext header — the nest-side belt for clause (3e). Kinds and
  fields are additive (I4): an older nest fails the retire typed and the
  scope grows as before; the `walker_id` and `sealed_under` fields ride
  `fauna.sync.changes.list` unchanged for every old caller.
  **(2) Reach is the possession evidence.** The `fauna.state.device-reach`
  kind (its bullet above): once a target's own signed row lists G, every
  cell wrapping G to it is redundant, and the top-up pass counts the reach
  as coverage before it looks at cells, so retirement never re-triggers a
  heal from a reach-reading binary. **The courtesy row's sunset (question
  (b))** — a healer wrote the legacy two-segment row only for a target with
  no reach row, so the dual-write stopped per target on the target's own
  signature — is history: the courtesy row itself was retired 2026-09-24 by
  the compat-remnant sweep ([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program 4).
  **(3) The reclamation pass** (`fauna_sync_engine::generation_reclaim`,
  once per full pump pass, best-effort, after the top-up and unkeyable
  passes) retires only what is redundant, and only rows whose author is
  itself or is no longer a verified member — never a live sibling's, whose own pass retires
  them. **(a)** Refresh this device's reach when the set
  changed. **(b)** Wrap cells whose target's reach covers
  the generation, whose target is no longer a verified member, or whose
  generation is `Shredded`. **(c)** This device's own unkeyable cells once
  `Satisfied` is merged or the generation is shredded, and any cell whose
  target is removed. **(d)** A removed device's rows: its `Enrolled` row
  FIRST, then its reach and device-endpoints rows. The `Removed` evidence
  rows go after the enrollment — so no reader sees an enrollment without
  its removal — and behind the gate, which guarantees every walker bound
  to that nest saw the removal first. **The evidence rows are not this pass's
  to retire (ruled and built 2026-10-01):** a nest's gate counts only the
  walkers bound to that nest, so on an account with a linked replica a
  retire in the pass let the evidence go before a device bound to the other
  nest had read it — measured. When removal evidence leaves a nest, and
  what retires it, is [`account-sync-plane.md`](account-sync-plane.md)
  § The bind leg, ruling 7's to say; the enrollment, reach and
  device-endpoints rows stay this pass's, in this order, and a process that
  runs no secondary leg retires no evidence. **Only the device's own rows go
  (2026-09-19):** of the `GenerationTip` kinds, device-endpoints alone is
  device-scoped (keyed by the device's own id). The reception keypair, held
  group machinery roots, both custody registry kinds and share endpoints
  are the ACCOUNT's — written by whichever device ran the flow, never
  re-written by another — and no departure retires them, by removal or by
  sign-out: retiring one with its writer loses it for the whole account
  (§ No user-data loss). The classification is an exhaustive match the
  compiler owns (`fauna_protocol::merge_policy::TipSealedKind::scope`), so a
  new tip-sealed kind chooses a side in the commit that registers it; a row
  the pass cannot open is kept, never guessed at. The price, stated: a kept
  row stays sealed under the generation it was written in, which keeps that
  generation in use — it never reads dataless, so it never shreds and its
  escrow wrap stays (bound (v) below). The cure is the hand-over arm of
  clause (g): a surviving member re-seals the item under the tip, after
  which the departed author's row is redundant and retired — by that
  member, on the strength of its own published row, and by nobody else.
  **(e)** A dataless superseded
  generation is shredded and retired: G is *superseded* when every verified
  member's reach holds this observer's resolved tip and G is a proper
  ancestor of it (so no member still seals under G — every one resolves at
  or past the tip it holds); *dataless* when the nest serves no live row
  sealed under G (the feed's `sealed_under` filter, asked of the nest, never
  inferred from this replica's relay plane, which is never told about other
  writers' retirements); then the minter writes `Shredded` into its own row
  (count-neutral), or exactly one member does if the minter is removed (one
  transient row) — the byte-order-min verified member whose verifying reach
  lists G, or, when no member's does, the byte-order-min verified member —
  and once the gate admits it the mint row(s), the receipt
  and any remaining cells — its `fauna.state.generation-closed` row among
  them, by the pass's dead-cell step — are retired, the retire carrying
  `no_rows_sealed_under: G`. Reach lists live `Minted` generations only, so
  it shrinks as generations shred. **The shredder's veto (ruled 2026-09-19,
  built 2026-09-23): the nest's answer is necessary, never sufficient.** A nest that
  answers "none" while live rows are still sealed under G induces an
  absorbing `Shredded`; every device then drops G's key on merge, and every
  copy of those rows — the ones replicas would serve a new device over the
  peer leg included — is unopenable for good. That is more than the
  availability a nest already holds, which ends at its own leg, so it is
  not accepted. Before writing `Shredded` the shredder reads its own relay
  plane's rows whose cleartext header names G and refuses this pass while
  any row it can open is **uncovered**. A row is covered when it is
  device-scoped and its writer is no longer a verified member (it goes with
  the device: clause (d) on removal evidence, clause (g)'s let-go arm without it), or when its item's merged entry is carried by a
  verified member's row on this plane at the tip's item key (clause (g)'s
  product, or the writer's own later re-publish) that is published as the
  pass saw it ([`account-sync-plane.md`](account-sync-plane.md) § The bind
  leg, ruling 1(d)). The evidence is the
  covering row, which every walker receives and no nest can forge — never
  the old row's liveness, which the relay plane is never told — so the veto
  clears by itself as clause (g) runs and another writer's unseen
  retirement cannot wedge it; a live sibling that has not re-sealed yet
  holds the shred exactly as its row holds the nest's belt today. A row the
  shredder cannot open vetoes nothing, which is why the stand-in shredder
  is drawn from the members that key G whenever one does. The residue,
  accepted: when no member keys G the shred rests on the nest alone, and
  there the only path to G's rows is the escrow wrap that same nest holds —
  it could delete that with no lie at all. A second escrow holder class
  (the *escrow doors* bullet's holder-management surface) re-opens this
  residue and must rule on it. **Ruled for the first such class, the
  user's own linked nests (2026-09-30, built 2026-09-30;
  [`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 4,
  owns the class):** a shred must reach every holder, so the belted retire
  below is issued against each linked nest as well as the bound one, each
  nest's belt checking its own rows; and because `Shredded` is absorbing —
  every device drops the key on merging it — ANY member's secondary leg
  retires the rows a linked nest still lists under a shredded generation,
  whoever wrote them, and sweeps that nest's wraps for it. The residue moves
  and is stated: a linked nest no device can reach keeps a shredded
  generation's wrap until one does, and a nest the account has left keeps
  it until the account is deleted there. The holder-management surface
  for holders that are not the user's own nests still owes its own ruling.
  **The escrow sweep (2026-09-16):** the
  shredded generation's escrow wrap goes with the receipt row that named
  it — the receipt retire carries `delete_escrow_wraps` beside
  `no_rows_sealed_under: G`, and the nest deletes every wrap it holds for G
  in the same transaction as the retire that lands, after the belt has found
  G dataless once more. This is the one non-user caller of the
  per-generation crypto-shred (`fauna.generation.escrow.delete` stays
  user-gated: it can destroy recoverability), and it is safe for the reason
  the shred is: a dataless generation has nothing to recover, so its wrap
  protects nothing, and the client act that vouches for it is the one this
  ruling already trusts to shred. Three shapes keep it so: the flag names no
  generation of its own, so the deletion can never outrun the belt (a flag
  without a belt is refused); a retire that lands nothing deletes nothing;
  and a retained generation's wrap is never touched — an age-based sweep is
  rejected, the nest cannot tell retained from shredded by age and a
  retained wrap IS the recovery path. The belt scans only the retire's own
  scope and is complete for the account solely because every generation-
  sealed kind is fleet-only (the `GenerationTip` set in
  `fauna_protocol::merge_policy`, all routed to `state-fleet`); a delegable
  generation-sealed kind would need the scan widened in the same change. The retire kind enforces the fleet-only premise by construction: `no_rows_sealed_under` (and `delete_escrow_wraps` with it) is refused `invalid_request` unless the request names `state-fleet`.
  **(f) A writer retires its own
  superseded generation-sealed rows.** A re-seal of a `GenerationTip` row
  under a newer tip — `device-endpoints` on every tip change, any such kind
  on its own re-publish — is a NEW wire row: the form-v2 item key derives
  from the per-generation schedule, so the nest cannot collapse the old row
  under the new one, and only the writer knows the two are one logical item
  (its journal holds the `(kind, key)` at the writer seq its relay row
  carries). The pass keeps the newest row per item and retires every older
  generation-sealed row of its own once that newest row is published as
  the pass saw it ([`account-sync-plane.md`](account-sync-plane.md) § The
  bind leg, ruling 1(d) — a row the bound nest does not hold carries the
  item to nobody there), which is what lets a superseded
  generation ever read as dataless at the nest; the 12-seat sweep measured
  the count growing about two rows per cycle without it and flat with it.
  **(g) The re-seal pass (ruled 2026-09-19, built 2026-09-23) — one pass,
  two arms.** A live `GenerationTip` row sealed under a superseded generation
  keeps it in use whoever wrote it, and only device-endpoints re-seals on a
  tip change; so a fleet holding any account-level row never sheds a
  generation. One pass closes that for a live writer's rows and a departed
  writer's alike — the ratified loser re-seal and the hand-over are the
  same act, differing only in who performs it. It runs ahead of clause (e)
  in the same reclamation pass, over exactly the generations (e) would
  shred but for being in use — G *superseded* in (e)'s own sense, so every
  verified member already holds the tip a re-sealed row lands under, and a
  fork's loser waits, as it does for (e), until a later mint names it a
  parent — and only over rows this device can open. For such a row R of
  logical item X = (kind, key): **what is re-sealed is this replica's
  merged entry for X**, value, `merge_meta` and tombstone marker verbatim,
  through the ordinary writer door (which seals under the resolved tip) —
  never R's own bytes. The merged entry dominates every row of X under the
  kind's join, so whatever row carries it makes R redundant; a `LatestWins`
  stamp travels unchanged, so a re-seal can never outrank another write; an
  `Immutable` value is the same value; and a tombstone is re-put as a
  tombstone, because retiring one uncovered could resurrect what it
  deleted. **Own arm:** R's writer is this device and it holds no row of
  its own for X at the tip's item key → put; clause (f) then retires R.
  **Hand-over arm:** R's writer is no longer a verified member, R's kind is
  account-level (`TipRowScope::Account`), and this device is G's **hander**
  — the byte-order-min verified member whose verifying reach lists G, the
  same single deterministic writer clause (e) names → put, unless its own
  row at the tip's item key already carries the merged entry. A live
  sibling's row is its own pass's business, as everywhere in clause (3).
  **The retire of a departed writer's account-level row** belongs to a
  member whose OWN row carrying X's merged entry is published as the pass
  saw it ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg,
  ruling 1(d)), and to nobody else: another member's
  covering row may have reached this replica over the peer leg before the
  nest holds it, and a retire licensed by it could leave the nest serving
  the item to no one. "Holds a row of its own at the tip's item key" is the
  device-endpoints writer's check, exact and stateless. **Races, stated:**
  reach skew, or a change of hander while R still stands, can produce a
  second member's copy — one extra live entry for that item, both copies
  correct, each retiring R idempotently; a copy is only ever retired by its
  own writer's clause (f), or handed over in turn when that writer departs.
  Nothing on this path can lose a row: the put precedes the retire, the
  retire waits for the put's publication, and a refused put (`scope_full`,
  no tip) leaves R where it was. **No member keys G** — the single-device
  sign-out → sign-in, where the new device holds no wrap and nobody is left
  to top it up: there is no hander, so nothing is handed over, nothing is
  retired, and G stays `Minted` with its escrow wrap, the one thing that
  can re-open it. A device that later keys G by any path lists it in its
  reach and is thereby the hander; the pass needs no recovery arm of its
  own. What *drives* that recovery is the *Escrow recovery* bullet below:
  the next seed-holding pump pass keys G from its escrow wrap, so the
  no-hander state lasts one pass, not for ever. **Compatibility:** every row
  the pass writes is an ordinary put and every retire an existing one — no
  wire or at-rest change; a pre-change binary never re-seals, so its
  account-level rows hold their generation until it upgrades or is removed
  (then the hand-over arm takes them), which is today's cost and no more.
  **(g), the let-go arm — a departed writer's device-level row is retired without removal evidence (ruled 2026-10-01; built the same day — § Implementation status today).** The hand-over arm skips a device-scoped row: it describes a device, so the account has nothing to keep by it and nothing is handed over. Clause (d) retires such a row on removal evidence, and until this ruling nothing else retired one. A device-level row whose writer is no member and was never removed therefore stayed live under its generation for good, and that generation never read dataless. A succession makes this the ordinary case. Under the successor a predecessor's device is no member and no removal names it ([`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 → *Rotation*, *What is re-made, never carried*), so its device-endpoints row pinned every carried generation. Measured 2026-10-01: that row is the whole obstacle. Retired by hand at the nest, the carried generation shreds in the next pass and its escrow wrap is swept. **The rule.** Over the generations the re-seal pass runs on — G superseded in (e)'s sense — G's hander retires each row sealed under G that it can open, whose kind is device-scoped (`TipRowScope::Device`) and whose writer is not a verified member. No put precedes the retire, and the hander forgets its relay copy when the nest confirms the row gone. **Why non-membership is evidence enough here, when clause (d) asks for a removal.** Clause (d) retires a removed device's rows under any generation, the tip included, so it needs a statement nothing can withdraw. This arm reaches a superseded generation only. Every verified member's reach holds the tip, a live writer re-publishes its device-level row under the tip on every tip change, and its own clause (f) retires the row it left behind. So where the writer is in fact a member, the row is one it has replaced or is about to. One case has a cost: a member this replica has not verified yet, its enrollment row not merged here, that has not re-published either. A replica that first walks in that window misses that member's dial candidates until the member's next pass re-publishes under the tip. No data is lost, because a device-level row is re-derived by its writer on every pass. The shredder's veto already rests on the same reading: it counts such a row covered and lets the shred go over it (clause (e)), after which no reader opens the row. This arm retires only what the veto had already let go. **Who asks, and what the nest checks.** The hander, as for the hand-over: one deterministic requester, and one that keys G, which it must to learn the row's kind. The retire door needs no change. It authorizes the account's own session and never the row's writer (clause (1)), and its gate applies as to any retire; a predecessor's device holds no grant on the successor's account, so its walk mark counts for nothing there. **What else it closes:** a removed device's device-level row still standing when its removal evidence left, and a row whose writer's enrollment never verifies. **What stays.** A row no member can open is kept, as everywhere in clause (3). A predecessor's device-level row under a generation no successor device carried is one such, and that generation is already dead to the account (`owner-key-material.md`, the rider's residual (i)). With no member keying G there is no hander, so nothing is retired until one does — the hand-over arm's own no-hander case. **Compatibility:** an existing retire, sent from an existing pass; no wire or at-rest change. A binary older than this rule retires nothing here, which is today's residue. **Refused.** *Dropping the predecessor devices' device-level rows in the succession ceremony's own transaction:* the kind is sealed, so the nest cannot tell a device-level row from an account-level one, and could only drop every tip-sealed row a predecessor device wrote, the account's reception key among them. *The ceremony's client retiring them:* it reaches only the rows that one client can open at that moment, and a retire the gate defers needs a pass that asks again, which is this one. *Accepting the residue as a bound:* each succession would leave a generation that never shreds, its key in every bundle and its wrap at every holder, for the sake of a row nothing reads, when the pass already knows how to send the retire.
  **(h) A replica forgets a dead row's relay copy, whoever wrote it (ruled + built 2026-09-27).** Nobody tells a replica's relay plane about another writer's retirements (clause (e)). Without this clause a long-lived replica kept for good every row a live sibling superseded under clause (f), every row a departed writer retired itself, and every gen-0 cell or receipt a sibling wrote. On the 12-seat sweep that grew one replica's plane from 180 to 480 rows over cycles 10 to 30, against a flat 43 live entries at the nest. The clause has two arms. Both are local — no journal row, nothing published — and both run in the reclamation pass. **A shredded generation's residue:** every relay row whose cleartext header names a generation that this replica's merged state reads `Shredded` is forgotten, whoever wrote it. The rows are found through an index over the header's generation (`relay_rows.generation_id`, an additive column back-filled once at the open that adds it). The index is probed only for the generations the plane still holds rows under, so the cost follows the live plane, never the fleet's history. **A dead gen-0 item's copies:** where the pass forgets a wrap cell, an unkeyable cell, a receipt or a removed device's reach as dead everywhere, it forgets the item's relay row under every writer, not only the rows it retired itself. Otherwise a live sibling's copy outlives the merged entry, and no later pass would ever name it again. **Why the peer leg ([`account-sync-plane.md`](account-sync-plane.md) § The peer leg) never needs either arm:** `Shredded` is absorbing, and every device drops the key on merging it (clause (e)), so no reader anywhere opens such a row. The shred also waited until every row under G that the shredder could open was covered by a row at the tip's item key (the shredder's veto), and that covering row carries the item a peer might want. A dead gen-0 item is dead by this ruling's own rule, the same rule that already forgets its merged state. **The record point stays unconditional** ([`account-replica-posture.md`](account-replica-posture.md) states the relay plane's record rule and points here for this exit). When a reconcile or a lagging peer serves such a row again, the walk records it again and the next pass forgets it again. The forget is an idempotent per-pass sweep, never a once-only memo that a re-serve would undo. **The bind leg's publish diff declines to push what this clause forgets** ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 1(b) owns that rule; ruled and built 2026-09-30): a row added to either arm, or taken out of one, changes what that diff sends, and a `Shredded` generation's own mint row is in neither arm, so a sibling's copy of it leaves by the diff's refused put (ruling 1(c)).
  **(i) The predecessor arm — a retired identity's generation-0 machinery rows are opened to be retired (measured and ruled 2026-10-01; built 2026-10-02 — § Implementation status today).** A succession moves the fleet scope's feed to the successor with every live row the predecessor's devices wrote ([`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md) § Re-key scope owns the move, and the ceremony touches no class-2 row). The successor's schedule opens none of the generation-0 ones, its walk carries one kind of them (the mint record: [`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 → *Rotation*, the succession rider), and every clause above retires only a row the pass can open. So they stayed live for good. Measured: a predecessor fleet of N devices on one generation leaves 3N + 3 live rows — per device its enrollment, its reach and its device-endpoints row, and per fleet the escrow target, the generation's mint row and its receipt. That is 6, 12 and 21 rows for 1, 3 and 6 devices, the same at the third successor pass and the fifteenth. The device-endpoints rows are the let-go arm's (clause (g)). The other 2N + 3 are this arm's. **The keys.** A runtime is handed, for each attested predecessor, that identity's generation-0 keys for every fleet-only machinery kind, open-only; `owner-key-material.md` (the rider → *The retired machinery keys open only to retire*) owns what is handed and that the walk still gets the mint kind's pair alone. This pass tries them on a form-v1 relay row that the plane's own schedule does not open and whose writer is not a verified member. **The rule.** A row that opens under them is that retired identity's machinery, of the kind the open names. (1) *Any kind but the mint record* is retired: a device-set row, enrollment and removal alike, a wrap cell, an unkeyable cell, a closed row, a reach, a receipt, the escrow target. Nothing precedes the retire. The succession re-makes these rows and the successor's merged state holds none of them (the rider → *What is re-made, never carried*). (2) *A mint record* is retired on either of two licences. The first is the hand-over arm's: this device's own row for that generation carries the merged record and is published as the pass saw it ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 1(d)) — the row the walk's carry wrote. The second: the record it opens to, or this replica's merged state, reads the generation `Shredded`; that retire carries the belt, as clause (e)'s do. Otherwise the row is kept, because it is the one row a device that brings that generation's key later still needs (the rider's residual (iii)). The device forgets its relay copy when the nest confirms a row gone. **Why this is no guess.** Clause (3) keeps a row the pass cannot open, and this arm opens every row it retires. The open names the kind and proves the row was sealed under the retired schedule, whatever writer id it claims; and the retire names the blinded item key that schedule derives, which no row sealed under the successor's schedule sits at. So a row forged under the retired keys can do no more than be retired itself. **The retired keys open here to retire and for nothing else:** no row opened under them is merged, re-authored, vouched to the publish diff or pushed. **Who asks, and what the nest checks.** Every device that holds the material. A retire is idempotent, and a second asker's answer is its licence to forget its own copy. The retire door needs no change (clause (1)) and its gate applies as to any retire; the predecessor's devices hold no grant on the successor's account, so their marks count for nothing; and the retire record carries each retire to the linked nests (`account-sync-plane.md` § The bind leg, ruling 5). **What stays, stated.** (α) Where no successor device holds a predecessor's seed, nothing of that predecessor's is opened and all of it stays: 2N + 3 rows per succession beside what clause (g) leaves, measured at 6,737 B sealed for one device and 2,773 B more per further device with clause (g)'s rows counted in. A 64-device fleet is 131 rows against the 4096-entry cap. (β) A mint record of a generation no successor device keys and no holder wraps, and every row sealed under it. Nothing opens them, and a device that arrives later with the key is their one cure (the rider's residual (i)). Zero in the measured flow, whose one generation is carried; **11 rows for one predecessor device on one generation** in the kept-wrap measurement (the rider → *The kept wrap*): the account's reception key, senior rotation key, period keys, folder keys and mail custody, and the device's own endpoints row. A succession no longer puts a generation here — the ceremony keeps the holder's copy (ruled 2026-10-01) — so (β) is reached by the three roads clause (j) names, and clause (j) is what gives the room back. (γ) A replica that holds no predecessor material, and walked these rows before they were retired, keeps its relay copies: nothing tells a relay plane about another writer's retirement (clause (h)), and it cannot open the rows to learn they are dead. A replica that joins afterwards is served none. **Ruled 2026-10-01:** the ceremony may not burn a generation's last escrow copy — the rider → *The kept wrap* owns the rule — and the room (β) holds is given back by the user, clause (j). **The delegable scope's counterpart** is ruled where the carry is: `succession-aftermath.md` § Re-key scope → *The predecessor's own row is retired behind the carry*. There the walk already opens every predecessor row, so that rule reads the walk's list and needs no key handed to the pass. **Compatibility:** an existing retire, sent from an existing pass; no wire or at-rest change; the keys derive from a `BackupKey` every attesting host already holds. A binary older than this rule retires nothing here, which is today's residue. **Refused.** *Stating the residue as a bound:* each succession would leave a fleet's machinery on the feed for good, served to every fresh walker and kept in every relay plane, and a scope filled that way could be emptied by no client (principles.md § Client-recoverable nest state). *The nest dropping the rows in the ceremony:* they are sealed, so the nest cannot tell the machinery from a mint record or from the account's own tip-sealed rows. *Retiring every form-v1 row of a non-member writer that this device cannot open:* that is the guess clause (3) refuses. A kind this build predates and a predecessor this device does not attest both fail to open, and the second may be a mint record. *Handing the walk the wider keys:* the rider refuses it, and nothing here needs it.
  **(j) The let-go — a dead generation's rows are retired by the user, and by nothing else (ruled 2026-10-01; built 2026-10-02, tui first — § Implementation status today).** A generation is **dead** when no device keys it and no holder wraps it. Three roads lead there, and a succession is no longer one of them (the rider → *The kept wrap*): the minting device is lost between its mint and the deposit its re-escrow pass owes, a one-pass window; the user shreds the generation at every holder through `fauna.generation.escrow.delete`, their own gated act; or every device is lost and the holder no longer has the wrap — a nest restored from a store older than the deposit (a receipt proves a deposit, not a holding: *A holder change re-receipts and never mints*, clause (3)), or a nest the account has left (the linked-nest residue, clause (e)). The rows sealed under a dead generation are bound (β): the account's tip-sealed rows of the moment plus one device-endpoints row per device, 11 for one device, and they count against the cap for as long as they rest. **Why no pass retires them.** A client can observe three things — it does not key G, no verified member's reach lists G, and the holder's filtered get answered empty (the durable answered-empty bit of *Escrow recovery*) — and not the fourth: a device of the user's, asleep since before the loss, whose bundle still keys G and which reads every row under it the day it is signed in again. Nothing on the nest or in a replica can tell that device apart from one that never existed, so a timer, a cap-pressure drop, or a retire on the holder's empty answer would destroy data a later device could read (`principles.md` § No user-data loss). The decision is the user's, and it is the user's data. **The act.** From the Settings recovery-kit section ([`../ui/settings.md`](../ui/settings.md) § Recovery kit owns the placement and the view state; its element ids need the user's approval under rule A before the build), the app lists each dead generation as the data sealed under it — its mint stamp (`MintCore::minted_at_ms`) and its live-row count — and the let-go confirm says the one thing the client cannot know: a device not signed in since then may still read these, sign in there first. On confirm the client retires, through the existing retire door, every live row whose cleartext form-v2 header names G — the walk is served that header on every row, opened or not, which is what the nest's belt reads too, so the rows are named by their coordinates without being opened; the gate applies as to any retire, and the retire record carries each to the linked nests ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 5) — and then deletes G's wraps at every holder, an idempotent no-op where none rest. G's mint record goes with the rest where this device can open it (clause (i)'s keys, or its own schedule); where it cannot, it stays as bound (α) keeps it. A replica forgets its relay copies of the retired rows as clause (h) forgets a shredded generation's. **Why this is no guess.** Clause (3) keeps a row the pass cannot open because the pass cannot know what it holds; the user is told exactly that and decides. The header is cleartext and unforgeable as to generation only in the sense the belt already relies on: a row whose header lies about its generation is retired with the generation it claims, which it had made itself a row of. **Compatibility:** an existing retire and an existing delete, from a new caller; no wire or at-rest change. **Refused.** *A nest-side drop of every row under G once the scope is full:* the nest reads G from the header but cannot know whether a device keys it. *A client-side retire when the holder answers empty and no member's reach lists G:* the dormant device. *Stating (β) as a bound with no exit:* the cap is reachable in principle, and a scope a client cannot empty is what `principles.md` § Client-recoverable nest state calls a bug, however far the bound.
  **(4) Sign-out severs on the plane (question (c)).**
  `AccountStoreHandle::shutdown_for_sign_out` writes this device's own
  `Removed` row (superseding its `Enrolled` row at the nest — same item, same
  writer, count-neutral; an enrolled non-removed device may remove itself
  under the kind's authority rule), retires its own reach and
  device-endpoints rows — never an account-level row it wrote (clause
  (3)(d)'s classification) — and only then retires its enrollment nest-side (the
  grant tombstone is what drops its walk mark). One shared-Rust seam, so every
  app whose sign-out stops its runtime through it severs with no per-app change
  (all seven apps do; web since 2026-10-01 —
  [`account-client-lifecycle.md`](account-client-lifecycle.md) § Implementation
  status today, the 2026-10-01 entry); best-effort like the grant
  retirement — an offline sign-out leaves the enrollment, removable from the
  devices page. **The `Removed` row is the writer's last word, never queued
  behind a refused row (ruled 2026-09-24):**
  it drains this writer's unsent own rows first like any put, but a drain
  that fails does not hold it back — it is sent at its own coordinate
  regardless (`AccountStatePlane::put_last_word`), the one write outside
  refinement 11's ordered own publish ([`account-replica-posture.md`](account-replica-posture.md)).
  The contiguous-prefix law protects a slot a later pass or heal reads, and
  a signing-out writer has neither: its journal and key are erased right
  after. The case it closes is the sign-out's own cut
  ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) §
  Implementation status today, the third placeholder source): a pass cut
  inside a put the nest recorded loses the reply, the drain's re-send is
  refused `stale_writer_seq` as the replay it is — no value lost, the nest
  holds the row — and a `Removed` row wedged behind it left a named device
  un-severed on the fleet plane, its rows live for every later prologue to
  carry. Pinned by
  `account_runtime::tests::a_sign_out_cutting_a_fleet_put_mid_flight_still_severs_the_device`
  (tier_1; red-verified). **The devices page's remove-device action owes — and now
  writes — the same row from the *removing* device, built 2026-09-16**: `DevicesMachine::remove_device`
  (`libs/fauna-devices-machine`) drives the account runtime through an
  app-supplied `FleetRemoval` port — resolve, stage, delete at the nest,
  settle (the two rules below) — and a failure reaches `error-message` rather
  than dropping silently, which would reproduce this same leak.
  **The removal target is resolved from client-held truth, before anything
  is deleted (ruled + built 2026-09-19).**
  Every field of a `fauna.sync.devices.list` row is the nest's to write —
  its `principal` included — and a `Removed` row is absorbing and excludes
  unconditionally (the device-set bullet), so a removal that trusted the
  row's principal let a hostile nest choose which device the removal
  permanently excludes (a live sibling, the caller itself) **and which it
  spares**: the device the user meant stays a verified member and a wrap
  target of every later generation, which is exactly what severance exists
  to forbid. The nest cannot seal a fleet-plane row, so the binding is one:
  **each device states the nest `sync_devices` row it enrolled on** (the
  registration latch's row half) **on its own `fauna.state.device-endpoints`
  entry** — generation-sealed, one whole-record LWW row per device, so an
  adopt that moves a machine onto a co-located agent's row simply restates
  it (the enrollment's join could not carry a mutable fact), additive (an
  entry stating no row is byte-identical to the earlier shape), plane-only
  (the carried copy the admit exchange hands a peer outside the fleet never
  has it), and retired with the device like the rest of clause (3)(d). The
  rule (`fauna_core::fleet_removal::resolve_removal_targets`, pure; the
  runtime's `AccountStoreHandle::resolve_fleet_removal` gathers the facts):
  (a) the row is this device's own enrolled row → refused — leaving is this
  clause's first half, sign-out, never a roster gesture; (b) some verified
  member states the row → **those members are the target, whatever principal
  the nest claims** (several only when one machine re-minted its principal),
  refused if this device is among them; (c) nobody states it → the nest's
  claim is *checked*, never trusted: itself → refused; already removed →
  nothing to write; not a verified member of this replica's fleet view, or
  not 32 bytes → refused; a member that states a *different* row → refused
  (a re-pairing nest or a mis-stating member — *A disagreement is the user's
  to settle*, below); a member that has stated no row yet (a pre-binding
  build) → accepted; no claim at all → the row names no fleet member and the
  nest deletion proceeds alone. **A refusal deletes nothing and tells the
  user the device was not removed** (`devices.error_remove_own_device` /
  `devices.error_remove_unverified_device` /
  `devices.error_remove_row_mismatch` on `error-message`), so the row is
  still there to retry from — resolution runs *before*
  `fauna.sync.devices.delete`, and a runtime that cannot answer — up or not
  (*The completion rule*) — refuses rather than skipping the fleet leg. The writer door keeps the same
  honest-writer check behind whatever was resolved:
  `fleet_removal::write_removed` — reached through `settle` (`Gone`),
  `complete_pending` and `remove_fleet_member` — refuses this device's own id
  and any id its fleet view does not verify, and is a no-op on an id already
  removed. This narrows the *writer* only — readers still exclude
  unconditionally (the device-set bullet's ruling is untouched), and an
  enrolled insider can still write any `Removed` row directly, the accepted
  vandalism-not-escalation posture (what that write costs a guardian-enrolled
  device on a supervised account, and what answers it:
  [`../behavior/family-safety.md`](../behavior/family-safety.md) § Full
  visibility for young children → *The device marker*). **Bounds, stated:** a member that never
  runs a binding build (a stolen laptop on an old version) states no row,
  so for it arm (c) is all there is: the nest can no longer aim its removal
  at a *stating* device or at the caller, but it can still strip the claim
  (the laptop is spared, silently) or aim it at another non-stating member
  — closed by the member-addressed door below, whose page surface is built on
  tui, macOS and iOS (2026-09-25) and owed by linux, windows and android (web
  a declared absence);
  and the label a user picks a row by is the nest's too unless it rendered
  from its seal (`path-sealing.md` owns the device label), so a nest that
  swaps whole rows' labels still misleads the *choice* — the this-device
  marker (`../behavior/devices.md` § This-device marker) covers the caller's
  own row, nothing covers a sibling's.
  **A disagreement is the user's to settle, against client-held identity
  (ruled 2026-09-19; built 2026-09-25 — the rule, the runtime's read and
  removal doors, and the page surface on tui, macOS and iOS; linux, windows
  and android owe the render, web a declared absence).**
  The binding trusts each member's own statement, so by itself it lets an
  enrolled device decide whether it can be removed: a stolen device that
  states a row other than its real one — a fabricated row, a sibling's, the
  caller's own — turns every removal of its real row into the *different
  row* refusal, with an honest nest, for ever. One row-to-member binding
  cannot serve both adversaries: when a member's statement and the nest's
  claim disagree, the replica holds **no fact that says which one lies** (a
  re-pairing nest and a mis-stating member present identical facts), so the
  row gesture keeps refusing — trusting the claim would hand the nest back
  its aim at a stating sibling — and the disagreement goes to the one party
  who can settle it, the user, through a **second, member-addressed door**
  (`fauna_core::fleet_removal::resolve_member_removal`): the user picks a
  fleet member by its fleet id and that id is the target. It takes no nest
  input, so the nest cannot aim it, and reads no row statement, so the
  member cannot veto it; this device's own id is refused, an id the fleet
  view does not verify is refused, an id already removed writes nothing. It
  is one leg — no nest row is chosen, so there is no nest deletion to
  bracket and nothing to stage; a failed write says so and the member stays
  listed to retry from. The member's nest row, if it still has one, then
  comes off by the ordinary row gesture, whose claim now names a removed id
  — or none, once its grant is revoked (*The nest half follows merged state*,
  below) — and needs no fleet write. **Who is offered the door: every verified member
  other than this device that no roster row accounts for**
  (`fauna_core::fleet_removal::unaccounted_members`) — and a row accounts
  for a member only when removing that row resolves, by the rule above, to
  **that member alone**. The derivation is the rule itself run over the
  roster, so nothing a member can state takes it off the list while keeping
  it irremovable: a fabricated row or the caller's own resolves nowhere; a
  sibling's row makes that row resolve to two members, so *both* are listed
  and the user removes one by its key rather than both by the row; a
  non-stating member whose claim the nest stripped or re-aimed is listed
  (the bound above, closed by the same list); a member whose nest row was
  deleted alone — every removal made from web — is listed; and an honest
  device left stating a stale row (an adopt restates on the device's next
  pass; a device lost before it keeps the old statement) is either still
  removable through the stale row while that row exists or listed once it
  does not — the same ruling, no separate arm. **What the user confirms
  against, stated plainly because it is thin:** a fleet member's only
  client-held identity is its **fleet-id fingerprint**; its enrollment time
  is carried too but is the member's own self-signed word, a hint and never
  proof; its device *label* is the nest's and is never shown for it. The
  user settles by elimination — every device still in hand shows its own
  fingerprint on its own devices page, and the member to remove is the one
  that matches none of them. **Bounds, stated:** a user who confirms without
  comparing can still be steered — by a nest's labels, as before — into
  removing a live sibling; that costs a re-enrollment, not confidentiality,
  and the member they meant is still listed afterwards. And a member-door
  removal leaves the device's nest row until the row gesture follows — with
  a hostile nest that row was never the client's to delete anyway — while
  its connection auth and bearers end with its grant, at the next full pass
  (*The nest half follows merged state*, below). The refusal's copy is its own
  (`devices.error_remove_row_mismatch`): unlike the unverified-member
  refusal, which a sync can clear, no retry clears this one, so it never
  says to try again.
  **The completion rule: the removal is crash-safe across its two legs
  (ruled + built 2026-09-19).**
  Two writes on two systems, and until this rule the second was best-effort
  with no memory: a failed write, a runtime that was down, or a client killed
  between the legs left the nest row deleted — nothing to retry from — and
  the device a verified member and wrap target for ever, the user having been
  told it was removed. The nest deletion is the **single decision point**,
  and a **durable intent** brackets it
  (`fauna_core::fleet_removal::PendingFleetRemoval` — the row plus the
  *resolved* fleet ids, never a principal the nest hands back later). It
  lives in the per-actor credential slot beside the registration latch, not
  in the replicated store: it is this machine's own unfinished business, it
  must be writable before any plane write is, and every process sharing the
  slot sees it — on a desktop the co-located agent usually holds the pump.
  Staged (verified by read-back, decoded) **before** `fauna.sync.devices.delete`; an
  intent that cannot be staged deletes nothing. Every read-modify-write of the
  staged intents runs under the slot's own write section, so a co-located
  process's stage, sighting or clear is never overwritten by a stale copy.
  Settled on the deletion's
  outcome (`NestDeletion`): **gone** — deleted, or `not_found`, the
  deletion's own postcondition however it came about — re-stages the intent,
  journals every `Removed` row and only then clears it; **kept** — the nest
  definitively refused, by a refusal the page's error mapping names
  (`conflict`, a malformed request, and the deletion's own
  `fauna.sync.guardian_marked`, mapped to `Conflict`; the former
  `fauna.sync.sole_source` retired with the role contraction) — clears
  it unwritten, because `Removed` is absorbing and the user was told the
  device stays (which is also why writing `Removed` *first* was rejected);
  **unknown** — a transport failure, a reply lost after the nest acted, a
  refusal the page's error mapping does not name, a crash before the reply —
  leaves it staged. **The reconcile**
  (`fauna_sync_engine::fleet_removal::complete_pending`, the pump's step
  after the reclamation pass, so at every assembly's first full pass and
  every pass after) finishes whatever is staged, with no user gesture: it
  reads the roster — only while something is staged — and a row that is
  absent means the deletion happened (complete), and an unreadable roster
  means wait. **A row still present is not yet evidence (ruled + built
  2026-09-21)****:** a pass
  cannot tell a deletion that never happened from one still in flight — the
  runtime passes between the page's commands, and on a desktop the
  co-located agent's pump passes on its own clock — so the first pass to find
  the row records that sighting in the intent and waits, and only a later
  pass still finding it `fauna_core::fleet_removal::DELETION_IN_FLIGHT_BOUND_MS`
  (one hour, hundreds of times the deletion's own 5 s deadline) after the
  sighting drops the intent unwritten — the user's row is there to retry
  from. The bound also caps what a never-happened intent costs: a roster read
  per full pass, ending within the hour. A re-stage is a new flight and
  clears the sighting; a sighting is recorded, and a drop carried out, only
  on the intent the pass judged; the rule itself is pure
  (`fauna_core::fleet_removal::reconcile_verdict`). The sighting is a slot
  group of its own (`row@ms`), which a reader from before it drops alone, so
  the spelling stays additive in both directions: an older process reads
  every intent unchanged, and a slot it rewrites merely loses sightings,
  which the next pass re-takes (a later drop, never an earlier one). Trusting the roster here is sound: the
  targets are client-verified and the user asked for the removal; the nest
  only answers whether its own half happened, and lying either way gains it
  nothing it could not do by refusing the deletion. The verification question
  (`nest/common.md` § Client-state recoverability) holds at every write: die
  after the stage → reconcile completes it once the roster shows the
  deletion landed, or drops it once the row has outlasted the in-flight
  bound; die after
  the nest deletion → reconcile completes it; die between two `Removed`
  writes → the intent is still staged and `write_removed` (via `complete_pending`) is idempotent;
  and a pass that lands inside the deletion's flight only waits, so the page
  failing to settle afterwards strands nothing.
  **An absent runtime refuses too (ruled + built 2026-09-19)****:** every runtime-hosting seat starts its runtime fire-and-forget at login and best-effort, so on all six the handle is absent while the assembly runs and for a whole session when it failed — and resolving *nothing* then let the nest deletion proceed alone, this rule's own leak. The door is now one shared adapter (`fauna_account_seams::fleet_removal::RuntimeFleetRemoval`, re-exported at `fauna_client_account_runtime::fleet_removal`; wired by tui, linux and the `fauna-ffi` seam, and served to web's Devices page by its core chunk through the account port) that answers `FleetRemovalRefusal::Unavailable` at resolution, staging and settling alike: nothing is deleted, the page says so, the row is there to retry from. tui wires it when the machine is built rather than at its account-store-ready edge, which had left the machine door-less — web's shape — until then. **Bound, stated:** a session whose assembly failed cannot remove a device from that app until a relaunch assembles it — on web, a tab that hosts no runtime (a second tab of the same account) refuses the removal the same way; the gesture works in the hosting tab. Web's Devices machine sits in another wasm chunk than the runtime and reaches this same adapter through the account port (built 2026-09-30, [`account-client-lifecycle.md`](account-client-lifecycle.md) § The client-side lifecycle → *The account port*), so its removal stages and settles its fleet leg exactly as on the six native apps.
  Pinned: `fauna-sync-engine`'s
  `a_staged_removal_the_page_never_settled_completes_on_the_next_pass` (the
  review probe's scenario, green — and it waits rather than guesses while the
  roster is unreadable), `a_refused_nest_deletion_clears_the_intent_and_writes_nothing`
  (the reconcile's half waits out the bound first),
  `a_settled_removal_lands_at_once_and_leaves_nothing_staged`,
  `a_pass_during_the_deletions_flight_keeps_the_removal_it_then_completes`,
  `an_unknown_deletion_outcome_leaves_the_removal_for_the_reconcile` and
  `a_gone_settle_re_stages_a_removal_a_racing_pass_dropped`;
  `fauna_core::fleet_removal`'s
  `a_row_still_held_is_waited_out_before_the_intent_is_dropped`,
  `a_re_staged_removal_is_neither_stamped_nor_dropped_on_an_older_sighting`
  and `the_sighting_spelling_is_additive_in_both_directions`;
  `fauna-devices-machine`'s
  `remove_device_stages_before_the_nest_deletion_and_settles_on_its_outcome`,
  `a_removal_whose_intent_cannot_be_staged_deletes_nothing` and
  `a_kept_device_deletion_maps_to_conflict`;
  `fauna-client-account-runtime`'s
  `an_absent_runtime_refuses_the_removal_rather_than_resolving_nothing`.
  **Each seat's OWN build-time wiring is pinned too (ruled + built
  2026-09-25)****,** against a `DevicesMachine::fleet_removal()`
  read-back rather than the shared adapter alone — so a seat that stops
  wiring the door reds only its own pin: tui's
  `the_removal_door_refuses_until_the_account_runtime_lands` (now built over a
  real `DevicesState::build` machine, not a hand-built door), `fauna-ffi`'s
  `the_builder_hands_the_machine_the_fleet_removal_door` and linux's
  `the_page_machine_carries_the_fleet_removal_door`. Two gaps remain open.
  Linux's pin builds its own machine and calls `wire_fleet_removal` itself,
  so it pins the wiring fn but not the page builder's call to it. The
  `fauna-ffi` pin can race sibling tests that install a store into the
  process-global host. **And end to
  end (2026-09-20):** the tier_3 kill-between-the-legs journey
  `test_crash_recovery_journeys.py::test_kill_client_between_the_removal_legs_reconcile_finishes_it`
  — two really enrolled seats on one account, the remover SIGKILL'd while the
  nest holds `fauna.sync.devices.delete` parked ahead of its handler (the
  `rpc_hold` seam, so the window is arranged rather than raced), the deletion
  then committed with no client alive to settle it, and a relaunched remover
  journaling the sibling's `Removed` row off its carried intent with no user
  gesture. Its two ground truths are what keep it from passing vacuously: the
  nest's roster read over the side channel (the deletion provably committed —
  on a roster that still held the row the reconcile correctly writes nothing:
  it waits, and drops the intent once the in-flight bound has passed) and the staged intent read out of the credential
  slot *after* the kill (the client provably died between the legs). Wired for
  tui and
  linux directly and for windows/macOS/iOS/android through the one
  `fauna-ffi::devices::build_devices_machine` seam, and for web through the
  account port (its leg crosses a wasm chunk boundary; the forwarder →
  loopback → `serve` wiring reruns the removal-order pins, and
  `fauna-wasm`'s `account_runtime` tests prove the core half over the real
  browser runtime). Pinned by `fauna_core::fleet_removal`'s rule
  tests, tier_1 tests on the `SettleFleetRemoval` and `ResolveFleetRemoval`
  Cmds (`the_honest_writer_refuses_itself_and_strangers_through_the_settle_door` pins the writer's arms through `settle`), `the_pump_publishes_the_latch_row_as_the_device_endpoints_statement` (the pump's own feed of the row statement; the tier_3 journey `test_device_member_removal.py` witnesses the same feed end to end), `fauna-devices-machine`'s resolve-before-delete and refusal pins, and
  the statement's own journey through a real nest
  (`conformance_account_state_walk`'s
  `a_members_row_statement_reaches_a_siblings_removal_facts`). **The
  member-addressed door is BUILT (2026-09-25; page surface on tui, the lead
  app, and on macOS and iOS — linux, windows, android and web owe the render
  leg, web's read reaching its runtime through the account port; the page's confirm still addresses a card by list
  position,
  `../ui/devices.md` § Implementation status today):**
  the pure rule (`resolve_member_removal` and `unaccounted_members`, pinned in
  `fauna_core::fleet_removal` — `a_member_that_misstates_its_row_is_listed_and_still_removable`
  and its siblings: the two-member row, the re-pairing nest, the honest stale
  statement); the runtime's read door `AccountStoreHandle::unaccounted_fleet_members`
  (this device's fleet id plus every unaccounted member with its asserted
  enrollment instant, `fauna_sync_engine::fleet_removal::unaccounted_members`)
  and its one-leg writer `remove_fleet_member` (resolve by key, then the same
  honest-writer door `settle` and `complete_pending` share
  (`write_removed`); nothing staged — there is no
  nest deletion to bracket), both served locally like the quartet and pinned
  by `the_member_door_lists_the_unaccounted_and_removes_by_key` over two real
  enrolled runtimes; the `FleetRemoval` port's `fleet_members` / `remove_member`
  on the shared `RuntimeFleetRemoval` adapter (an absent runtime lists nobody
  and refuses, `an_absent_runtime_neither_lists_nor_removes_members`);
  `DevicesMachine`'s member list and `remove_member`, with both fingerprints
  rendered in the snapshot through the one `fauna_core::format::fleet_fingerprint`
  (`the_member_card_and_the_own_row_render_one_fingerprint`); the tui surface
  (`ui/devices.md` § Members without a matching entry); and end to end
  `test_device_member_removal.py` — a sibling seat mis-stating its row under
  the e2e-only `FAUNA_E2E_STATED_ROW` pump override (compiled out of release
  builds; the only way to produce the statement, which the harness cannot
  forge) refused by its row, listed, removed by key, its nest row then finished
  by the row gesture; and the row-deleted-elsewhere sibling listed by
  fingerprint and removed. The refusal's copy now ends with the pointer to the
  group.
  The live e2e convergence assertion this once called "still owed" is the
  kill-between-the-legs journey documented just above — closed once
  `fauna_client_account_runtime::device_set_state_json` (a debug/e2e-agent
  gated reader over the same `fauna.state.device-set` plane, convention 15
  rule (a)) gave it an observation path, with tui,
  linux, android, macOS, iOS and windows wired (macOS and iOS share one
  `FaunaKit` handler over the gated `FfiNestClient` export; windows wraps the
  same export in a Debug/e2e-agent-only `NestRpcClient` method, because the
  production `windows-ffi` bindings do not carry it). Windows has no unclean
  kill, so the journey itself never runs there; the reader's presence on every
  app that hosts the account runtime is pinned instead by
  `test_account_runtime_pump.py`'s reader test. Trigger
  (b)'s closure is built (2026-10-01): severance lands at the next
  origination after the remover's closure merges, whether or not the tip
  names the removed device (*The mint protocol, trigger (b)*).
  **The nest half follows merged state — a removed member's grant is revoked by key, at every nest (measured red + ruled 2026-10-01; built 2026-10-01 — § Implementation status today).** Without this rule the removal's nest half reaches one nest. `fauna.sync.devices.delete` goes to the nest the removing app is bound to, and the member-addressed door sends nothing nest-side at all. A device registers only at the nest it is bound to ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 4: a secondary leg needs no device principal), so a device bound to another of the account's nests is, at the remover's nest, a member no roster row accounts for, and its removal is its fleet row alone. **Measured 2026-10-01, before the build,** on two nests each linked at the other: the one device bound to B is lost, a device bound to A removes it by its key, and after six rounds — each with a leg run that completed B — B still mints bearers for the removed key, still lists its row, and its gate's watermark stands at the lost device's mark while B's log moves past it. So removal was not the control clause (1) says it is: B's gate holds every retire above that mark for good, the removal evidence of [`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 7 included; a stolen device goes on authenticating at its own nest after the user removed it; and the writer-key tombstone a removed device's ending rests on ([`account-replica-posture.md`](account-replica-posture.md) § The store device principal → *Principal succession after a device delete*) never lands there. **The rule: at every nest a runtime completes — its bound nest in each full pass, each linked replica in each secondary-leg run — it reads that nest's roster and revokes, by key, the grant of every row whose principal is a fleet id its merged state reads `Removed`.** The request is `fauna.sync.device_grant.revoke` on the account's own session ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model owns the kind and its two arms): the nest clears the grant, tombstones the key, ends every session and socket it minted, and — by the gate's own predicate, clause (1) — stops counting its walk mark. The roster is read only once merged state reads some device removed; the leg revokes after that nest's reconcile and ahead of its own retires, so the run that revokes is the run whose retires land; and whichever process pumps runs the bound nest's half, the seedless agent included, since the arm needs the account's session and no seed. No new kind, and nothing new at rest. **Why by key.** The key is client-held truth — the fleet id, read from a `Removed` row this replica verified — and `Removed` is absorbing, as the tombstone is: revoking that key is right wherever it rests, and can never reach a device that is still a member. The roster decides only whether to ask. A nest that hides a principal spares itself a request it could as well have refused, and one that claims a removed key on another row gets that key revoked, which harms no one. The arm never names this device's own key: a removed device's own ending is the posture doc's. **What the nest keeps: the row.** Its label and its places stay, keyless — what a sign-out leaves ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model, *never the whole device row*) — and the row comes off by the ordinary row gesture from an app bound to that nest. **A guardian-marked row is left alone:** the arm skips a row the roster flags, and the nest refuses the session arm for a marked row's key as it refuses that row's deletion ([`../behavior/family-safety.md`](../behavior/family-safety.md) § Full visibility for young children → *The device marker* owns the refusal). **Bounds, stated.** The roster read costs one request per nest per full pass or leg run for as long as merged state reads any device removed, which after the first removal or sign-out is for good. A nest no runtime of the account completes — no live device bound to it, and linked from no nest a live device is bound to — keeps the grant, as it keeps everything else it was given. A removal reaches a linked nest's grants when a seed-holding device's leg next reaches that nest, and no sooner (ruling 4's bound). **Refused: deleting the row** — `fauna.sync.devices.delete` at each nest, for the row that claims a removed key. A deletion is addressed by row, and which key a row carries can change between the roster read and the request: a machine that signs out and in again enrolls a fresh principal on the same named row. A row-addressed request can therefore end a live successor; a key-addressed one cannot. **Refused: listing every nest's roster on the Devices page**, so that the row gesture exists for a device bound elsewhere: a surface on seven apps for a duty the user discharged when they removed the device, with the gate held until they find it.
  **The steady state, measured:** a 64-device fleet driven through 200
  sign-out → sign-in cycles against the real nest holds its live-entry count
  flat (the count after cycle 200 within O(members) of the count after
  cycle 20, and never a `scope_full`) —
  `bins/fauna-nest/tests/conformance_account_state_walk.rs`, the reclamation
  section. **Bounds, stated:** (i) the reach gate for shredding makes "no
  member still seals under G" hold (the succession corner this bound once
  excepted, where admissibility diverged by `prior`, went with that arm —
  the *source of `prior`* bullet); a row
  published under a shredded generation is re-sealed by its writer —
  `device-endpoints` re-seals on any tip change already, and the general
  re-seal of one's own rows under a dead generation is the ratified loser
  re-seal pass — clause (3)(g)'s own arm; (ii) an enrolled device that died without
  signing out stays a wrap target and costs the fleet one cell per healer
  per generation until it is removed — removal is the control; (iii)
  concurrent first-need mints still fork and each loser costs a generation's
  rows once; losers go dataless and reclaim like any superseded generation;
  (iv) a fleet whose every member runs a pre-reach binary reclaims nothing —
  exactly today's growth, until one member upgrades; (v) an account-level
  row a departed device wrote (clause (3)(d)) keeps its generation from
  shredding — one pinned generation (and its escrow wrap) per such
  generation — only while no verified member can key it (the
  single-device sign-out → sign-in): clause (3)(g)'s hand-over arm
  re-seals it the moment a member does, which the *Escrow recovery*
  bullet's pass makes the next seed-holding sign-in; a removed device that
  held that generation's key keeps the reach it already had to that
  generation's rows, and every newer generation still excludes it; (vi)
  the re-seal pass costs one put per live account-level row per superseded
  generation that held it, count-neutral once clause (f) or the hand-over
  retire lands, plus at most one extra live entry per item per change of
  hander while a departed writer's row of it still stood.
- **Delegable-scope reclamation — moved 2026-10-01 to [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md) § Delegable-scope reclamation, verbatim and whole.** What holds the delegable scope (`state`) at one live row per item, and what a scope full of live items is owed: the cover, the put that names the rows it replaces, the hand-over, the retire below the cover (parts (1) to (4)), the parked row, a departed scope's retire and the hand-over's membership test (parts (5) to (7)), with the bound each leaves. The cap it works under and the retire kind it uses are the bullet above's.
- **Live distribution and the offline window, honestly.** A live device
  learns a new generation from the mint entry itself (its wrap is inline
  when it was in the mint's member set) or from a top-up wrap — plane rows
  over any leg (nest-mediated or peer), **no nest handshake door**: a
  distribution door would re-couple generation propagation to nest
  liveness, which R11 and the peer leg exist to avoid. A device offline
  across a mint keeps sealing under its last admissible tip — admissible
  until the removal/supersession propagates, exactly the "severance is
  only as real as propagation" window the Rotation bullet already states —
  and adopts the winner on next sync; readers walk every retained
  generation, so nothing written in the window is lost. A device enrolled
  moments before a mint that missed its enrollment record seals under its
  bundle tip and is topped up on first contact.
- **The escrow doors (nest-side requirement 4).** v1 holder is the user's
  nest: `fauna.generation.escrow.put` (idempotent per (generation id, wrap
  hash); persists durably, then returns the **holder-signed receipt** —
  deployment-identity-signed, the key clients already pin),
  `fauna.generation.escrow.get` (serves the account's enrolled devices and
  a recovery-ceremony session), `fauna.generation.escrow.delete` (the
  per-generation crypto-shred half; user-gated at the calling surface — the
  one non-user path is the belted receipt retire of a shredded generation,
  *Fleet-scope reclamation* clause (3e)'s escrow sweep).
  **Two step-4 build rulings (2026-08-13):** idempotency extends **to the
  receipt bytes** — a byte-identical re-deposit returns a byte-identical
  receipt, stamped with the *first* deposit's instant, so a crash-retrying
  minter can never fork the immutable `fauna.state.escrow-receipt` row
  into two verifying variants; and the doors' admission is **the
  authenticated account, full stop** — "enrolled devices and a
  recovery-ceremony session" both authenticate as the account, and fleet
  enrollment is sealed plane state the holder deliberately cannot read
  (the receipt's signed encoding + holder-generic verification are shared:
  `fauna_core::generation::{sign,verify}_escrow_receipt`).
  Multiple holders are redundancy of the *same* identity-targeted wrap;
  non-nest holders (another device, a friend's nest — exemplars, not an
  enumeration: T16's holder-management surface owns the holder classes, and
  a friend's *device* is a candidate class it must explicitly rule on) ride
  the same put/receipt contract when the T16-era custody surfaces build the
  management UI — the *protocol* lands now, the holder-management UI
  deliberately waits for the co-design note. **No-nest honesty:** an
  account with no reachable escrow holder cannot complete a first mint —
  fleet-only sealing stays refused until it has one; stated, not hidden
  (the PQ-1-era nest-less messaging question is a separate one, not this).
- **Escrow recovery — a seed-holding device keys what it cannot key from
  the holder, unasked (ruled 2026-09-19).** The gap this closes: a single-device account that signs out and
  back in enrolls a fresh device key, no member is left to top it up, and
  every row sealed under the old generations — the account's own
  group-reception keypair among them, and through it every storage group's
  content — is unreadable from every app though nothing was destroyed. The
  out-of-the-box invariant does not allow a state only a terminal could
  heal, so the engine heals it.
  **(1) Who can open an escrow wrap: every seed-holding runtime, and
  nothing else — no ceremony.** The escrow secret is derived from the
  identity seed (`fauna_core::generation::derive_escrow_xwing_keypair`;
  [`owner-key-material.md`](owner-key-material.md) § The schedule build
  design → *The escrow target* owns why it is never `BackupKey`-derived),
  and every one of the 7 apps assembles its runtime as a seed-holding
  surface (`RuntimePrincipal::SeedHolding`). Under seed residency
  ([`account-replica-posture.md`](account-replica-posture.md) § *Seed
  residency*) the only machine without the seed is one approved from an
  existing device — which by construction has a sibling to top it up; a
  single-device account's next sign-in has no sibling to approve it, so it
  necessarily imports the seed. Signing in with the seed IS the possession a
  recovery ceremony would establish, so no separate ceremony, prompt or
  setting exists. The seedless host (the sync agent, `Seedless`) never
  recovers: recovery is one more seed-only leg, skipped there and healed by
  a signed-in app on the same machine — which process runs it, and when,
  is the seed-leg role's
  ([`account-runtime.md`](account-runtime.md) § Multi-instance concurrency;
  ruled and built 2026-10-01: the app beside a pumping agent recovers in
  its seed pass) — and
  the recovered key rides
  the retained bundle, which is the machine's shared principal slot, so the
  host reads it from there. **One opener besides a runtime's pass (ruled
  2026-09-30, not built):** the box-recovery **cold read** — a reader that
  holds the identity seed and an owner-authenticated connection before any
  runtime exists for the account, opens wraps exactly as this pass does (the
  seed-derived secret, then the mint's key commitment) over a throwaway
  in-memory replica, and writes nothing, so it keeps no key it opens
  ([`nest/box-recovery.md`](nest/box-recovery.md) § The plane-era recovery
  floor, (b), owns it). Possession of the seed is the admission there too, so
  "nothing else" stands: no seedless reader opens a wrap.
  **(2) The trigger: every pump pass, for every generation this device
  cannot key — no waiting on a healer.** The pass runs after the fleet walk
  and **before** the top-up pass. Its domain is each live, canonical,
  id-bound `Minted` row that (a) `generation_key_for` answers "no key" for
  — retained bundle, inline wrap and every merged top-up already tried —
  and (b) carries a verifying escrow receipt from a trusted holder in
  merged state (the writer door's own ack predicate). For each, one
  `fauna.generation.escrow.get` **filtered to that generation**; every wrap
  in the reply is opened with the seed-derived escrow secret and checked
  against the mint's key commitment, exactly like a device wrap, so a
  substituted or foreign wrap keys nothing. Not waiting is deliberate: the
  alternative trigger — "no verified member's reach lists G" — is defeated
  by an enrolled device that died without signing out (bound (ii)), whose
  reach lists G for ever and which never heals anyone; and a new device
  whose only sibling is asleep would stay blind until it woke. The top-up
  path is untouched and remains the only path for a seedless replica and
  for the no-nest profile — this is a fallback a seed holder takes, not a
  distribution door the plane depends on. **Bounds:** clause (b) means a
  forged-mint flood extracts no request at all (a receipt is holder-signed
  and binds the generation id); an answered request — a key, or no wrap
  that opens — is remembered for the life of the runtime and not repeated,
  so the steady cost is zero and the worst case is one request per live
  unkeyable generation per runtime start; a transport failure is not an
  answer and retries at pass cadence like every other step. An answer with
  no wrap that opens is also what a waiting read is told: it ends the
  holder's half of the unkeyed hold, which
  [`account-client-lifecycle.md`](account-client-lifecycle.md) § The
  client-side lifecycle → *The first listing*, clause (5), owns (ruled
  and built 2026-10-01 — that doc's § Implementation status today; the
  answer is kept as the generation's durable answered-empty bit). The filtered
  form is ruled over the unfiltered "everything" form because the latter's
  reply is unbounded in an old account; what the filter shows the holder —
  that this session lacked G — is nothing it could not infer from the
  sign-in it just served.
  **(3) What the recovered key becomes: a retained-bundle entry, and
  nothing else.** `RetainedKeyCustody::record_generation_key`, the
  ratified "record after every obtain" duty — no self-addressed wrap, no
  new row, no second distribution path. From there the existing machinery
  does the rest in the same pass: `generation_key_for` answers from the
  bundle, the top-up pass (which now runs with the key in hand) heals every
  later or seedless device, the reclamation pass publishes a reach that
  lists G, and the device is thereby clause (3)(g)'s hander. A runtime
  assembled with no custody attached has nowhere to hold the key and skips
  the pass.
  **The pass that keys a generation is the pass that reads it (ruled and
  built 2026-10-01).** The fleet walk ran before recovery and left every row
  sealed under G unopened, so a pass that recovers at least one key
  re-presents the fleet scope (one more full-state reconcile) right behind
  recovery, before any writer. Without it the first pass of a single-device
  sign-out → sign-in ends with G keyed and its rows still unmerged, and the
  runtime's readiness edge
  ([`account-client-lifecycle.md`](account-client-lifecycle.md) § The
  client-side lifecycle — the prologue) answers a consumer that asks once
  "nothing rests here": the senior ATProto rotation keys read as an empty
  ring, the custody check is skipped as nothing-to-check, and the settings
  page says no key is held, until some later pass happens to run. Nothing
  was lost in that window — the rows and the wrap rest at the nest — but a
  device that believes it holds no rotation key cannot contest a recovery
  fork, so "eventually" is not good enough for a row this device can
  already open. The cost is one extra walk on the one pass per runtime life
  that recovers anything; every other pass recovers nothing and walks once.
  **(4) Security: nothing new is reachable, by anyone.** The door's
  admission is unchanged (the authenticated account) and the wrap is
  useless without the seed, so the two cases are the ones
  [`account-data-plane.md`](account-data-plane.md) § Implementation status
  today (the ST-007 consequence-B trace) already rules: a removed
  *seedless* device is refused a session once its grant is tombstoned and
  could not derive the escrow secret if it were not; a removed *seed*
  holder keeps full account access by design — R14 severs only seedless
  replicas, and succession is the remedy for a copied seed. The holder
  learns nothing it did not hold: it serves ciphertext it already stores,
  sees no key, and the recovered key never leaves the device except as
  ordinary device-targeted top-up wraps. **Succession (re-ruled 2026-10-01; built 2026-10-02):** the pass opens
  wraps with the *current* identity's escrow secret and, behind it, with
  each attested predecessor's — the identities whose seeds this device
  holds — for the generations whose predecessor mint record the walk
  opened and could not carry for want of the key; the ceremony keeps the
  predecessor-targeted wraps for exactly this, the rider's *The kept wrap*
  owns the rule, and the re-escrow pass that runs right after this one
  (`fauna_account_plane::generation_reescrow`) deposits what it keyed under
  the successor's target, which is what sweeps the kept wrap at the holder.
  **Compatibility:** one existing User-class door, called by a new caller;
  no wire or at-rest change. A pre-change binary never recovers and stays
  exactly as blind as today until it upgrades.
- **A holder change re-receipts and never mints (ruled 2026-09-30; built 2026-09-30, the linked-nest clause included).** The holder half of the bind leg ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg owns the leg, its replica id and the settled replica). The trusted holder stays what it is: the one identity pinned for the bound nest — and since 2026-10-05 every native TLS login writes that pin, a public-CA nest's included, so it is empty only on a plaintext nest or before the machine's first login ([`security.md`](security.md) § Transport trust → *The login's pin* owns which graduation records what). Three things change around it. **(1) Trust follows the pin, every pass.** The set is read from the pin store at the start of each pass, never frozen at assembly, so a rotation the app has accepted ([`nest/box-recovery.md`](nest/box-recovery.md) § Client acceptance) moves it without a restart; frozen, a running runtime refuses every successor-signed receipt and can neither mint nor re-escrow until it is reassembled. **(2) A receipt from a holder that is no longer the trusted one is answered by a deposit, not a mint.** The re-escrow pass already deposits every generation the device keys that no trusted holder has receipted, and writes the new holder's receipt in its own cell beside the old one. It never mints afterwards: the deposit restores the tip, and a mint there would re-seal the whole scope at every change of nest for nothing. Until the succession rider's 2026-10-01 re-ruling the pass did mint across a succession, told from a holder change by a test on the receipts (merged state held no receipt for the tip naming this identity's own escrow target, from any holder). A succession now leaves the predecessor's tip no candidate under the successor, so there is nothing for the pass to mint past ([`owner-key-material.md`](owner-key-material.md) § Path A-sibling-2 → *Rotation*, the succession rider; built 2026-10-01). **(3) A receipt proves a deposit, not a holding.** A rebuilt box presents the identity that signed the receipts in merged state and holds none of the wraps. So in the first pass after every assembly the device asks the holder for the account's wraps (one unfiltered `fauna.generation.escrow.get`) and deposits a fresh wrap for every generation it keys that the reply lacks — at every assembly, not only when the replica id changed, because a box restored from a snapshot keeps its id and loses what was deposited since. The receipt row already in merged state stands: it is immutable, it names the first wrap's hash, and the ack predicate never compared that hash with anything the holder holds. **Recovery, and only recovery, admits a verified ancestor.** For the escrow-recovery pass a receipt whose holder is a superseded ancestor of the pinned identity counts, the ancestry proven by the nest's rotation chain verified to the pinned head; the sealing predicate keeps requiring the current identity. Without this a generation receipted only under the predecessor and keyed by no surviving device is unrecoverable though its wrap rests at the nest — the recovery pass would never ask for it — and with it the seed-holding device that recovers the key re-escrows under the successor in the same pass, so the state heals. The clause bounds requests, as clause (b) of *Escrow recovery* always did, and trusts nothing new: an opened wrap is still checked against its mint's key commitment. **The box-recovery cold read needs no receipt at all:** it is one reader making one unfiltered get, so the request bound the receipt provides has nothing to bound; it opens every wrap the nest serves against the mints it walked. **A linked nest is a holder too (ruled and built 2026-09-30):** a seed-holding runtime deposits each generation it keys at every linked nest it completes ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 4), verifies the receipt against the pairing row's nest id, and writes it in that holder's own cell. Such a receipt acks no tip — sealing needs the bound nest's — and becomes the trusted one only for a runtime bound to that nest, which is exactly the fresh device that reaches it after the other nest is lost.
- **The A5 partition lands with this design** (the ladder's sequencing
  rule: with-or-before the schedule build, so the grandfathering window is
  kept). **`state-fleet`** joins the scope grammar as a whole-string
  family (§ The scope string): the frozen `state` string *is* the
  delegable sub-scope — every production row ever sealed there is
  delegable-rung, so nothing moves — and every fleet-only kind, machinery
  and data alike, seals into `state-fleet` from its first row. A delegable
  grantee's subscription is `state` and never sees fleet churn; custody
  grants and admission verdicts enumerate both strings as ordinary scopes.
  The seen-set **stays in `state`** (grandfathered): its churn timing is
  visible to delegable-scope subscribers, accepted and recorded at its
  rung ruling — the grammar keeps per-kind sub-scopes expressible for
  future high-churn kinds at birth, which is all the ruling's "weigh its
  own sub-scope" note requires.
- **First production consumer + what un-queues.** The build's proof kind is
  `fauna.state.device-endpoints`: first `GenerationTip` production sealing,
  un-gating production peer discovery — then every E3
  dissolution slice, fleet-only production sealing generally, D6's
  device-keyed kinds, and the A5 window is banked instead of forfeited.
  **Build order** (each step green before the next): registry column +
  machinery kind registrations & merge arms → envelope form v2 → device-set
  kind + its merge/authority tests (contingency check red-verified
  by an attempted add-wins resurrection) → escrow target/doors/receipt →
  mint + wraps + commitment verification → tip resolution replacing the
  boolean gate → device-endpoints seals in tier_3 conformance.

### The recipient-set scheme (T20 build design — ratified 2026-08-17, refutable until the first group-scope build)

**Moved 2026-09-28 to [`recipient-set-scheme.md`](recipient-set-scheme.md) § The recipient-set scheme, verbatim and whole** — the T20 build design: *Scope birth + the authority seam*, *The roster kind* (with *The per-writer roster cell*), *Generations ride the R14 machinery, re-targeted*, *Mint triggers — removes mint, adds never do*, *Severance, per axis* (member removal, a member's fleet severance, *An authority device's removal*), *The membership witness*, *The concurrent-membership lattice*, and *Constraint (b) discharged explicitly*. Any `§ The recipient-set scheme → …` citation resolves there by swapping the filename. The generation machinery it re-targets stays in this doc (§ The generation machinery, above); the key-material half stays [`key-material-hierarchy.md`](key-material-hierarchy.md) § Audience: a storage group.


## Implementation status today

*The entries below were carried verbatim out of [`account-data-plane.md`](account-data-plane.md) § Implementation status today, which stays the home of the cross-cutting entries no single plane owns.*

- **Moved 2026-10-01 — the delegable-scope reclamation's two entries** (parts (1) to (4), and parts (5) to (7)), verbatim with their measurements, to [`delegable-scope-reclamation.md`](delegable-scope-reclamation.md) § Implementation status today.
- **Ruled 2026-10-01 — § The generation machinery → *Fleet-scope reclamation*, clause (3)(j), the let-go (built 2026-10-02, tui first), and the succession clause of *Escrow recovery* (built 2026-10-02).** The kept-wrap half's code facts, tests and measurement are [`owner-key-material.md`](owner-key-material.md) § Implementation status today's (the kept-wrap entry). **The let-go:** `libs/fauna-account-plane/src/generation_let_go.rs` holds the read (`dead_generations`) and the act (`let_go`). The read takes the generations the fleet plane's relay rows are sealed under (`AccountStore::relay_rows_sealed_under`, the `relay_rows.generation_id` index) that are not `Shredded`, are keyed by no device here and are listed in no other verified member's reach. The bound holder must also not wrap them: the answered-empty bit, else one filtered `fauna.generation.escrow.get`. The act re-reads that, retires each row by its coordinates through `AccountStatePlane::retire`, retires the mint rows and receipts behind the dataless belt, deletes the wraps at the bound holder (`linked_leg::sweep_wraps`) and records the generation in the store's let-go set, which the secondary leg's step 7 sweeps at every linked holder. A replica that did not act forgets its copies at its next read, once the nest's listing no longer shows them. The runtime serves both as pass-bound commands (`AccountStoreHandle::dead_generations`, `::let_go`). Pinned by `a_dead_generation_is_let_go_by_the_users_act_and_a_live_siblings_rows_stay` (`bins/fauna-nest/tests/conformance_account_state_walk.rs`; measured: 8 rows under the dead generation, 19 live fleet entries before the act and 9 after, wrap gone, the siblings' rows unchanged). Which apps render the surface is `ui/settings.md` § Implementation status today's. **Residual:** the read asks the bound holder alone; a wrap that only a linked holder still keeps does not stop a generation reading dead.
- **Measured and ruled 2026-10-01, built 2026-10-02 — § The generation machinery → *Fleet-scope reclamation*, clause (3)(i), the predecessor arm.** **What is code.** `AttestedPredecessors::retired_machinery_keys` (`libs/fauna-account-plane/src/attested_predecessors.rs`) derives, in the one derivation beside the delegable schedule and the mint kind's pair, each attested predecessor's generation-0 pair for every kind the registry seals fleet-only at `Gen0` — the eight machinery kinds today, taken from the registry. The driver hands them to the bound nest's fleet plane alone (`AccountStatePlane::with_predecessor_machinery_keys`); the peer leg's and the linked leg's source guards refuse the call, as they refuse the carries' keys. `AccountStatePlane::open_predecessor_machinery_row` is the one open that tries them — form v1 only, the hit pair first, a row no pair opens remembered and not tried again — and `open_relay_row`, the walk and the publish diff keep the plane's own keys. `generation_reclaim::ensure_reclaimed` runs the arm as its step 9, skipped outright on a plane handed no keys: over the relay rows of writers that are no verified member, a row the retired keys open and the plane's own do not is retired under its own item key and its relay copy forgotten on `Retired`/`Gone`; a mint row only on licence 1 (this device's own row at that generation's item key carries the merged record and is listed) or licence 2 (the record or merged state reads `Shredded`, retired with the `Dataless` belt). **The proof** (`bins/fauna-nest/tests/conformance_account_state_walk.rs`, red-verified by not handing the plane the keys): the succession flow that keeps S and D running ends with no live row a predecessor device wrote — 5 left without the arm, all form v1, clause (g)'s let-go having taken the endpoints row — and so does a predecessor fleet of three devices (`a_three_device_predecessor_fleet_leaves_no_row_on_the_successors_feed`; 9 without). `a_predecessors_mint_row_no_successor_device_carries_is_kept`: a successor device that attests the predecessor and keys nothing retires the predecessor's enrollment, reach, escrow-target and receipt rows and keeps the mint row (5 form-v1 rows without the arm, 1 with). `a_late_carrier_reads_the_mint_record_after_the_predecessors_row_is_retired`: S's first pass retires the predecessor's mint row on licence 1 before the generation shreds, and a successor device arriving afterwards with the generation's key merges the record from S's row and reads every pre-succession row. Unit pins: `attested_predecessors`' `the_retired_machinery_keys_open_every_fleet_only_generation_0_kind_and_no_delegable_one`; `generation_reclaim`'s `the_predecessor_arm_retires_only_a_non_members_row_the_retired_keys_open` (a member's row and a row no key opens are left); and `conformance_succession_carry.rs`'s `a_predecessors_fleet_only_generation_0_row_stays_unopened` holds with the new keys attached — the walk still opens nothing. **Bound (γ), measured 2026-10-02:** D, the seedless successor seat, walked the feed before the arm ran and ends holding 5 relay copies of the predecessor's rows for one predecessor device and 9 for three — exactly the 2N + 3 form-v1 rows the arm retired at the nest (the device-endpoints rows go with the carried generation's shred residue, clause (h)). A walk does not prune another writer's retired row, so they stay for that replica's life. **The measurement that ruled it (2026-10-01).** Before the build nothing opened a retired identity's generation-0 machinery row: `AttestedPredecessors` derived the mint kind's pair alone and only the walk read it, and every retire in `generation_reclaim` followed `AccountStatePlane::open_relay_row`, which tries the plane's own keys. The nest's side is read from code: `folders` and `sync_changes` are both `Succession::Move(MoveShape::Plain)` in `bins/fauna-nest/src/db/actor_tables.rs`, a state scope is a reserved folder of the actor (`CacheDb::find_state_scope`), and `successions.rs` touches no class-2 row. **Measured 2026-10-01** on the flow of `the_succession()` in `bins/fauna-nest/tests/conformance_account_state_walk.rs`, instrumented for the measurement and restored. Predecessor fleets of 1, 3 and 6 devices on one generation; the predecessor devices' grants revoked at the ceremony, which is the production shape (`sync_devices` stays with the retired identity); then two successor seats, one carrying the generation's key and attesting the predecessor and one seedless, run for 3, 7 and 15 full passes. Before the ceremony the feed held 16, 22 and 31 live rows. Afterwards the predecessor devices' rows still live were 6, 12 and 21, the same at every sample: per device a device-set row and a reach row (form v1) and a device-endpoints row (form v2, under the carried generation), and per fleet an escrow-target row, a mint row and an escrow-receipt row (form v1). The ten account-level rows under the carried generation were handed over and retired by the third pass (clause (3)(g)'s hand-over arm). The successor's own live rows numbered 21. The residue was 6,737 B sealed for one device and 2,773 B more per further device. A fresh seed-only successor device was served every one of those rows on its first walk, opened none of them, and kept them in its relay plane. The flow's one generation is carried, so bound (β) measured zero. With the predecessor seat's grant left live, as the committed fixture has it, the predecessor's count is the same and one successor top-up cell more stays live.
- **Ruled and built 2026-10-01 — § The generation machinery → *Fleet-scope reclamation*, clause (3)(g), the let-go arm.** `generation_reclaim::reseal_superseded`, over a generation in `handing` (this device its hander), retires a departed writer's device-scoped row it can open with no put before it, through the ordinary `Retirer::retire`, and forgets the relay copy on `Retired` or `Gone`; a deferred or withheld retire is asked again next pass. Nothing changed at the nest. The veto's half was already code: `uncovered_row_under` counts such a row covered. **Proven:** `bins/fauna-nest/tests/conformance_account_state_walk.rs::a_successors_first_mint_supersedes_the_carried_generation_and_a_late_seed_only_device_reads_every_pre_succession_tip_sealed_row` — after the hand-over, no live row rests under the carried generation, both successor seats read it `Shredded` and the holder keeps no wrap of it, red-verified without the arm (one live row, the predecessor device's, left under it); its fixture `the_succession` now also revokes the predecessor device's grant beside the wrap burn, the production fact the gate depends on, and the test reads the seedless sibling's top-up of the carried key after every round, since the generation can now shred, and its key be dropped, within them. The `generation_reclaim` unit pins: `the_hander_lets_go_a_departed_writers_device_level_row`, `a_verified_members_device_level_row_is_not_let_go_by_a_sibling`, `a_member_that_is_not_the_hander_lets_go_of_nothing`. **Measured 2026-10-01, before the build,** on `bins/fauna-nest/tests/conformance_account_state_walk.rs::a_successors_first_mint_supersedes_the_carried_generation_and_a_late_seed_only_device_reads_every_pre_succession_tip_sealed_row`, instrumented for the measurement and restored. After seven full passes of the two successor seats, the predecessor device's device-endpoints row is the only live row under the carried generation, `AccountStatePlane::any_row_sealed_under` answers true at both seats, and neither pass retires or shreds anything. With that one row retired at the nest by hand, the next pass shreds the generation and both seats read it `Shredded`. The retires behind the shred — the mint row, the receipt and the escrow sweep that rides it — were then withheld: the fixture's predecessor seat keeps a live grant on the suite's one actor, and its walk mark held the gate's watermark at 16 against rows up to 41. With that grant revoked the same pass sweeps the wrap. In production the predecessor's device holds no grant on the successor's account (the nest's `sync_devices` table stays with the retired identity at a succession), so the fixture is what differs; the build's proof has to model that. **Observed:** each replica's merged entry for a departed device's endpoints outlives the row (the successor seat still held the predecessor device's entry after the shred). **Ruled and built 2026-10-02:** the entry stays in merged state, and the peer leg dials an entry only while its device is a verified member of the replica's fleet view — owner [`account-sync-plane.md`](account-sync-plane.md) § The peer leg → *Discovery*.
- **Ruled 2026-10-01 — § The generation machinery, the device-set bullet's *source of `prior`* and *The mint protocol*, trigger (d): the fleet view takes no `prior` signer, and the re-escrow pass never mints — both built 2026-10-01.** `fauna_core::generation::FleetView::build(root, rows)` takes no predecessor set: an `Enrolled` row whose cert any identity but the account root signed is flagged and is no member, pinned by `generation::tests::a_predecessor_signed_enrollment_is_flagged_and_no_member`. `GenerationTrust::prior` (filled by `account_driver::enrollment::r14_trust` with the attested predecessor set) now feeds only the group authority view (`group_authority_revocation::severance_work`). `generation_reescrow::ensure_reescrowed` is the deposit sweep alone (`AccountStatePlane::mint_successor_generation` and `ReescrowPass::Reescrowed::minted` are deleted), and the successor's first tip-sealed origination mints by trigger (a) over the carried generation. The crossing itself — the mint record carried by the fleet walk — is [`owner-key-material.md`](owner-key-material.md) § Implementation status today's entry, which owns the measurement.
- **Fixed 2026-10-01 — § The generation machinery → *The mint protocol*, the mint sequence: a first-need mint whose holder answered stands when its rows cannot be sent yet.** `AccountStatePlane::mint_first_need` sent each row as it wrote it, so a state put that failed after the deposit landed aborted the sequence between the mint row and the receipt row: the write that tripped the mint was refused, an unacked mint row stayed in the journal, and every retry deposited another generation. Until 2026-09-30 the pass hid this — its re-escrow step ran after the first writer and receipted the stranded row in the same pass; the bind leg's holder half moved re-escrow ahead of every writer and the refusal became visible (`fauna-wasm`'s two reception-key runtime tests, whose requester answers the escrow door and no state put). The sequence now writes its rows with `put_local` (the spill through `generation_topup::put_heal_local`) and makes one ordered send whose failure is logged and left to the next pass's publish step. Proof: `account_state_plane::door_tests::a_first_need_mint_stands_when_its_rows_cannot_be_sent_yet` (one deposit across two tip-sealed writes, the tip resolving locally, the rows leaving in journal order once the put answers), red before the change with the browser tests' own error chain.
- **RULED + BUILT 2026-09-30 — § The generation machinery → *A holder change re-receipts and never mints*, and the cold-read clause of *Escrow recovery*.** (1) The trusted set follows the pin: the host hands the runtime a source, not a set (`account_driver::TrustedHolderSource`), re-read at the start of every pass into the one `generation_tip::TrustedHolders` cell every plane of the assembly borrows. (2) `generation_reescrow::ensure_reescrowed` mints only when the re-escrowed tip had no receipt for this identity's target from any holder before the pass (`fauna_core::generation::escrow_receipted_generations`); it runs right after the fleet walk, ahead of the host legs and the device-endpoints writer, so a moved pin is re-receipted before anything this pass seals. (3) The holdings check: the first pass after every assembly and after every bind verification asks the holder once, unfiltered (`bind_leg::holder_holdings`), and the re-escrow deposits every acked generation the reply lacks without rewriting its receipt row (`ReescrowPass::Reescrowed::restored`). Recovery admits the pinned identity's verified ancestors (`bind_leg::fetch_verified_ancestors` over `fauna_protocol::nest_rotation::verified_ancestors`; `ensure_recovered`'s `ancestors`); the sealing predicate is unchanged. The cold read is `deployment_seed_recovery::cold_read_deployment_seeds`. (4) The writer door answers a moved pin the same way: a `GenerationTip` origination that finds no tip while merged state holds a receipt for this identity's target from a holder no longer trusted — the pin moved and no re-escrow pass has re-receipted it yet (a pass cut, or ended early, after its pin re-read) — makes the re-escrow's deposits itself (`generation_reescrow::reescrow_owed_at_the_door`) and re-resolves, and refuses when a deposit fails; it never first-need mints there. Inside a pass such a write still parks behind it (`AccountStatePlane::origination_mints`), since the deposits are network. Proofs: `conformance_account_plane_bind` (a second nest, a rebuilt nest, a rotated nest — each red-verified by reverting its arm), the `generation_reescrow` and `generation_escrow_recover` unit pins (the door's `a_tip_sealed_write_after_a_pin_move_re_receipts_and_never_mints`, red-verified without the door's re-escrow, and `a_pin_move_whose_deposit_fails_refuses_the_write_instead_of_minting`), `conformance_account_state_walk::a_seed_holder_that_never_enrolls_reads_the_deployment_seeds_with_the_cold_read`. **Built 2026-09-30 — a linked nest as a holder:** the secondary leg deposits every generation this device keys at each linked `account_replica` nest, holdings-checked, the receipt verified against the pairing row's nest id and written in that holder's own cell, never minting (`generation_reescrow::ensure_deposited_at_linked`, sharing `ensure_reescrowed`'s one deposit core); a shred reaches every linked holder through the mirrored and widened retires and a sweep of the holder's wraps. Its mechanism and proofs are [`account-sync-plane.md`](account-sync-plane.md) § Implementation status today's ruling-4 entry.
- **Measured red, ruled and built 2026-10-01 — § The generation machinery → *Fleet-scope reclamation*, clause (4) → *The nest half follows merged state*.** One function, `fauna_account_plane::removed_grants::revoke_removed_grants`, generic over the requester and outside the `account-driver` feature — it sends `fauna.sync.devices.list` and `fauna.sync.device_grant.revoke` with `fauna-protocol` types — called from the two places a runtime completes a nest. `account_driver::pass::pump` runs it against the bound nest on the account's session in every full pass, whichever process pumps, sharing the staged-removal reconcile's roster read when that ran (`PumpReport::removed_grants`). `linked_leg::complete_linked_nest` runs it against each linked replica as the leg's step 5, after that nest's reconcile and diff and ahead of its retires (`LinkedCompletion::removed_grants`). It asks the nest nothing until merged state reads a device other than this one removed, never names this device's key, skips a row the roster flags `guardian_marked`, counts a `fauna.sync.guardian_marked` refusal without failing, and never sends `fauna.sync.devices.delete`. All 7 apps and the sync agent carry it with no per-app code. The member-door bound above (*connection auth and bearers end with its grant, at the next full pass*) is code with it. Proofs: `conformance_account_plane_bind::a_lost_device_removed_at_the_other_nest_stops_counting_at_its_own_nests_gate` (the measured case: the lost device's nest stops minting for its key, its gate counts no mark, and it keeps the row) and `a_lost_device_removed_by_key_stops_minting_at_the_nest_it_shares_with_its_remover` (the bound nest's half; the remover's own grant stands), each red-verified by reverting its call; the `removed_grants` unit pins (nothing removed sends no roster request; this device's key is never named; only rows claiming a removed principal are revoked; a marked row is left alone; a roster already read is not read again). The nest's refusal for a marked row: [`../behavior/family-safety.md`](../behavior/family-safety.md) § Implementation status today.
- **Measured red, ruled and built 2026-10-01 — § The generation machinery → *The mint protocol, trigger (b)*: a removal the tip does not name is minted past.** Before the build the only removal-driven mint was first-need's behind the member rule: a tip stopped being a candidate only when a member its mint names was removed, so removing a device that enrolled after the tip (keyed by top-up, in no member set) left the tip admissible and the remover's next tip-sealed write sealed under the generation the removed device keys. **The kind:** `fauna_protocol::merge_policy::KIND_GENERATION_CLOSED` — fleet-only, `Gen0`, merged by byte-order max (`fauna_core::generation::join_generation_closed`), adopted on a key that parses as a generation id and a value that decodes, no signature. **The record:** `fauna_core::generation::GenerationClosedRecord` (closing device, removed device, stamp — audit only). **The closure set:** `fauna_core::generation::closure_set` — every `Minted` row that is id-bound, passes `verify_mint_authorship`, and whose members the view wholly verifies with the removed device excluded; no ack, keyability or cap. **The reader:** `fauna_core::generation::resolve_admissible_tip` takes the closed ids (`closed_generations` over the live rows, fed by `generation_tip::resolve_tip`) and skips a closed generation before it can become a candidate; its edges stay in the DAG, so the first-need mint names it as a parent. **The writer:** `fauna_account_plane::fleet_removal::write_removed` journals the closed rows with `put_local`, then the `Removed` row, skipping a generation merged state already holds a closed row for; the early return for an id already removed writes nothing. A sign-out (`generation_reclaim::sever_self`) writes none. **Reclamation:** `generation_reclaim::ReclaimState::dead_item` calls a closed row dead once merged state reads its generation `Shredded` — every writer's — so the pass's dead-cell step retires this device's own (or a departed writer's) row, forgets the item locally under every writer, and the publish diff declines to push it. **Proofs, each red with its clause taken out:** `conformance_account_plane_bind::a_removal_the_tip_does_not_name_is_minted_past` (un-ignored; red again without the closure write: one generation before the remover's next write and the same one after); `fauna_core::generation::tests` — `a_closed_generation_is_no_candidate_and_retires_no_ancestor`, `the_closure_set_names_a_tip_the_removed_device_is_no_member_of`, `the_closure_set_names_a_live_ancestor_the_removed_tip_would_fall_back_to`, `the_closure_set_leaves_out_every_row_that_is_no_candidate_anywhere`; `fleet_removal::tests` — `a_removal_journals_its_closures_ahead_of_the_removed_row`, `an_invented_mint_draws_no_closed_row`; `generation_reclaim::tests` — `a_closed_row_is_retired_when_its_generation_shreds_and_not_before`, `the_publish_diff_skips_a_closed_row_of_a_shredded_generation`, `a_sign_out_closes_nothing`; `merge_policy::tests::the_closed_kind_registers_and_merges_by_byte_order_max`. **The second hole has no case through real passes:** a tip that names the removed device over a live `Minted` ancestor that does not is a shape no pass produces today — a superseded ancestor is shredded once every member's reach holds the tip — so it is pinned over staged rows, at the resolver (`the_closure_set_names_a_live_ancestor_the_removed_tip_would_fall_back_to`) and through the writer and the store (`generation_reclaim::tests::a_removal_closes_the_live_ancestor_its_tip_would_fall_back_to`), each resolving the ancestor without the closure and nothing with it.
- **Ruled + built 2026-10-01 — § The generation machinery → *Fleet-scope reclamation*, clause (3)(d): the removal evidence is no longer the reclamation pass's to retire.** [`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 7 owns the rule, and its status entry the build and its proofs. Here: `generation_reclaim::Retirer::retire_removed_device` retires a removed device's `Enrolled` rows, its reach and its device-scoped rows, and no `Removed` row; `generation_reclaim::removal_evidence` lists the evidence for the secondary leg's removed-device arm, which retires it. Proof: `generation_reclaim::tests::a_removed_devices_enrollment_is_retired_before_its_removal_evidence` — two passes retire the enrollment and never the evidence, and the arm retires it behind them.
- **Built 2026-09-16 — the self-signed enrollment.** § The generation machinery →
  the device-set bullet's *The self-signed enrollment* owns the ruling; what
  is code: the additive `device_sig` on `DeviceSetRecord::Enrolled`, the
  preimage and builder (`fauna_core::generation::{enrollment_signing_bytes,
  sign_device_enrollment}`), `DeviceSetRecord::self_verifies_at`, the
  key-aware `join_device_set` behind `fauna_protocol::merge_policy`'s
  device-set arm (adoption permissive by design), `FleetView`'s flagging of
  an unsigned or failed binding, and `fleet_bootstrap`'s re-publish over a
  non-own row in `fauna_sync_engine::account_runtime`. Every production and
  fixture writer builds through `sign_device_enrollment`; the unsigned shape
  survives in tests only as the refused case.

- **Moved 2026-09-28 — the recipient-set scheme's build-out** — seven entries, verbatim, to [`recipient-set-scheme.md`](recipient-set-scheme.md) § Implementation status today: the bound roster entry, the unbound `Enrolled` entry refused, the legacy roster re-bind (retired), the per-writer roster cell, the authority-device severance publisher, the peer witness door wired, and the reader half of authority-device revocation with the authored shred.

- **Built 2026-09-15 — the bounded mint.** The mint entry used to carry one
  inline wrap per enrolled member (then about 2.4 KB each as encoded) under the
  plane's 64 KiB per-entry cap, so past roughly 27 members no mint could
  publish, no tip resolved, and every `GenerationTip` kind stayed refused at
  the writer door — an account got there by accumulating devices it never
  removed. § The generation machinery → *The bounded mint* owns the shape:
  every member listed, `MAX_INLINE_MEMBER_WRAPS` wrapped inline, the spill
  topped up by the minter ahead of the receipt, the ceilings measured, the
  encoding and device-set-growth questions ruled. Proofs: `fauna_core`'s
  inline-set tests; `fauna_mls`'s builder tests (the cap, the spill, the
  member ceiling, a bad spilled target still refuses); `fauna_account_plane::
  generation_mint`'s 64-member mint, the ceiling refusal before any deposit,
  and the measured arithmetic at the ceiling; and end to end against the real
  nest (`bins/fauna-nest/tests/conformance_account_state_walk.rs`):
  `a_first_need_write_over_a_64_member_fleet_mints_a_bounded_generation_every_member_keys`.
  The fleet scope's count-cap reclamation and the sign-out's plane-side
  removal followed the next day (the entry below).

- **Built 2026-09-22 — the gate's watermark, and the pass's step timings.** § The generation machinery →
  *Fleet-scope reclamation*, clause (1) → *the gate's watermark* owns the
  ruling. What is code: `CacheDb::retirable_through_seq` (the lowest counted
  mark, the gate's own predicate) served on every class-2 page as the
  additive `SyncChangesListReply::retirable_through_seq`, after the
  request's own mark is recorded (`bins/fauna-nest/src/sync_handlers.rs`);
  `RelayRow::feed_seq` (the nullable, additive `relay_rows.feed_seq` column,
  added to an existing store by `migrate()`), stamped from the put reply
  (`AccountStore::stamp_relay_feed_seq`) and from every walked page;
  `AccountStatePlane::retirable_through_seq`, banked per walk; and the
  reclamation pass's `Retirer::withhold_above` — every retire carries the
  row's coordinate, a row above the watermark is withheld as a local
  `NotYetStable` (`ReclaimPass::withheld`), `sever_self` never withholds.
  Beside it, the instrumentation the measured prologue lacked:
  `PumpReport::timings` (`PassTimings` — the two walks and the four
  generation-machinery steps, and the whole pass) with `log_pump` naming
  the slowest step at info on every pass of a second or longer, so a sweep's
  app log attributes a long prologue without a hand count. **The measured
  floor, stated** (the 2026-09-22 `--app linux` sweep's captured logs, the
  session account at 33 devices): the reclamation's refused retires cost
  10–20 ms each — 222 refusals in about 4.5 s, the part this rule removes;
  the escrow recovery's 89 generations ended 24 s after the store mounted,
  walk included, and its per-generation work is already the ruled minimum
  (one filtered `escrow.get`, a KEM decap and a bundle write per generation
  a fresh sign-in cannot key — bound (ii) above, the skips for a shredded
  or sibling-keyed generation already built); the top-up's per-pass cost is
  a local walk of the mint rows with one up-front `wrap_coverage` read, two
  rows published while walking 110. The captured logs end before that
  prologue's pass completes, so the rest of the five minutes is not
  attributable from them — which is what the timings are for: the next
  whole-suite sweep's log names the step. Proofs: the nest's `the_retirable_watermark_is_the_lowest_counted_walk_mark`;
  the store's `a_relay_row_remembers_its_feed_seq_once_told`;
  `generation_reclaim`'s `a_row_above_the_gates_watermark_is_withheld_without_a_request`
  and `sever_self_never_withholds_against_the_watermark` (tier_1, the fake
  nest counting retires); and end to end against the nest's own handler
  table, `conformance_account_state_walk.rs::a_stranded_walker_costs_the_fleet_no_retire_request_per_pass`
  (a round across the live fleet under a stranded walker sends no retire and
  is refused none; removing the dead machine sends and lands them).
- **Built 2026-09-16 — fleet-scope reclamation.** § The generation machinery → *Fleet-scope reclamation* owns
  the ruling; what is code: the nest's `fauna.account.state.retire` kind
  behind its retention gate (`state_walk_marks`, fed by the additive
  `walker_id` beside `held_through_seq` on the feed; a mark counts while its
  key holds a live, non-tombstoned grant) and its generation belt
  (`no_rows_sealed_under`, over the additive `sealed_under` feed filter) —
  `bins/fauna-nest/src/db/account_state.rs`; the `fauna.state.device-reach`
  kind (`fauna_core::generation::DeviceReachRecord`, registered in
  `fauna_protocol::merge_policy`); the reclamation pass
  (`fauna_sync_engine::generation_reclaim::ensure_reclaimed`, the pump's
  step after the unkeyable pass) and the sign-out's plane leg
  (`generation_reclaim::sever_self`, the first act of
  `AccountStoreHandle::shutdown_for_sign_out`); the top-up pass reading
  reach as coverage and writing the courtesy row only for a target without
  one; the resolver consulting the retained bundle for candidacy (the
  *sealing-epoch axis* bullet's O-keyability clause, amended the same day);
  and `AccountStoreHandle::settle_fleet_removal` / `remove_fleet_member`, the devices page's seams into `write_removed`
  (with `resolve_fleet_removal`, its client-truth target resolution, since
  2026-09-19 — clause (4)).
  Proofs: `fauna_core`'s reach laws; `fauna_protocol`'s registry pin; the
  nest's four DB tests (retire without insert + coordinate memory, the
  gate, the belt, the filter); `generation_reclaim`'s tier_1 scenarios (a
  healer's own cell retired on the target's reach and not before, the
  courtesy row written only without a reach, the reach published on change
  only, a removed device's enrollment retired before its removal evidence);
  and the steady state end to end against the real nest — the suite's fast
  twin `conformance_account_state_walk.rs::a_fleet_survives_sign_out_cycles_at_a_steady_live_entry_count`
  (12 seats, 30 cycles) and the ratified measurement
  `…::a_64_device_fleet_survives_200_sign_out_cycles_at_a_steady_live_entry_count`
  (explicit, `--ignored`: about an hour in a debug build, every cost real
  crypto over a real feed). Three costs that measurement exposed are fixed
  in the same landing, all pure and production-relevant: the resolver, the
  reach and the read path consult the retained bundle (a commitment compare)
  before any KEM open; every pass's signature checks — enrollment certs,
  healer cells, receipts, mints — go through a bounded memo of checks that
  passed (`fauna_core::generation`, the *verified-signature memo*); and the
  walk's trial-open tries the kind that opened the previous row first
  (rows arrive in runs of one kind). A row the pass knows is dead everywhere
  is also **forgotten locally** (`AccountStore::forget_state`, no journal
  row, nothing published), so a long-lived replica's merged state stops
  growing with every cell ever walked. **Built 2026-09-27 — clause (3)(h), the relay plane's twin.** The pass forgets a dead gen-0 item's relay row under every writer (`generation_reclaim`'s `forget_dead_item`), and step 8 sweeps every relay row sealed under a `Shredded` generation (`AccountStore::relay_generations` / `relay_forget_sealed_under` over the new `relay_rows.generation_id` index). Pins: the steady-state sweep now also asserts that a replica living through every cycle keeps its relay plane flat. It was red at 180 → 480 rows over cycles 10 → 30 before the build, with 280 of those rows sealed under shredded generations and 144 being gen-0 cells and receipts whose merged entries were already forgotten. After the build it holds 56 → 56 rows over the same cycles (43 live entries at the nest), at an unchanged wall clock. `sqlite::tests::relay_rows_are_swept_by_the_generation_their_header_names` pins the index, the one-time back-fill and survival across payload eviction. `the_hand_over_keeps_a_row_it_cannot_open` now pins the residue arm on a row the hander cannot open.
  **Fixed 2026-09-19 — a departure retires only device-scoped rows.** As first built,
  the removed-device arm and `sever_self` retired every generation-sealed
  row the departing device wrote, so a sign-out and sign-in on a
  single-device account, or removing the device that first seated a
  community room, lost the account's reception keypair for good (and any
  held group root or custody registry row it wrote). Both arms now retire
  only the kinds `fauna_protocol::merge_policy::retired_with_its_writer`
  names (clause (3)(d)). No release-pipeline nest image carries the retire
  kind (the last `build-nest-image.yml` dispatch and the pinned production
  release both predate it), and an older nest answers every retire
  unsupported, so no production row was retired. Proofs: the
  registry pins `the_tip_sealed_kinds_are_exactly_the_registry_set` and
  `only_device_endpoints_is_retired_with_its_writer`; against the real nest,
  `conformance_account_state_walk.rs`'s control
  (`the_reception_key_reaches_a_later_device_while_its_writer_stays`), its
  sign-out and removal arms (`a_sign_out_…` / `a_removal_retires_its_endpoints_and_keeps_the_accounts_reception_key`
  — red with the catch-all restored) and the single-device arm
  (`a_single_device_sign_out_and_back_in_keeps_the_reception_key_and_its_escrow_wrap`).
  **Ruled 2026-09-19, built 2026-09-23 — the re-seal pass and the
  shredder's veto.** Clause
  (3)(g) is step 5 of `fauna_sync_engine::generation_reclaim::ensure_reclaimed`,
  between the removed-device step and clause (f): one tip resolution now
  feeds both it and the generations step, and it scans this replica's relay
  plane only while some generation is reclaimable. "A row of its own at the
  tip's item key" is `generation_reclaim::own_row_at`, the
  device-endpoints writer's own check lifted to serve both (homed in the
  ungated module, which the writer imports). Clause (3)(e)'s
  veto and its reach-first stand-in election run in the generations step,
  after the nest's `sealed_under` answer. A local-only covering row of the
  shredder's own counts: the ordered own publish cannot send the `Shredded`
  marker ahead of it. Tier-1 pins, in the module:
  `the_hand_over_never_retires_before_its_own_row_is_published`,
  `a_siblings_covering_row_never_licenses_the_hand_over_retire`,
  `a_stale_own_row_never_licenses_the_hand_over_retire`,
  `the_hand_over_keeps_a_row_it_cannot_open` (the last three added at the
  build's verify-back, each red-verified by its own mutation),
  `a_member_that_is_not_the_hander_writes_nothing`,
  `a_tombstone_is_handed_over_as_a_tombstone`,
  `the_veto_holds_the_shred_until_the_covering_row_is_walked`,
  `a_shredder_that_cannot_key_the_generation_is_not_vetoed`. Against the
  real nest, in `conformance_account_state_walk.rs`: the sign-out and
  removal arms above now assert the whole chain (C holds the key, B serves
  its own re-sealed row of it, A's generation-1 row is retired, generation 1
  is `Shredded`, its escrow wrap is swept), and
  `a_single_device_sign_in_hands_the_recovered_generation_over_then_shreds_it`
  runs the same chain after escrow recovery. The seedless single-device pin
  and the 12-seat steady-state sweep pass unedited, the sweep within noise
  of its pre-build wall clock. The ignored 64 × 200 sweep's verdict on this
  build is not yet recorded. Each tier-1 pin was
  red-verified by reverting its own arm.
  **Ruled + built 2026-09-19 — escrow recovery.** The *Escrow recovery* bullet's pass
  is `fauna_sync_engine::generation_escrow_recover::ensure_recovered`, the
  one production caller of `fauna.generation.escrow.get`: the pump runs it
  after the fleet walk and before the top-up pass on every seed-holding
  runtime (`FleetWriter::escrow_recovery`; `None`, and skipped, on the
  seedless host), with one answered-set per runtime worker. Shared-Rust, so
  all 7 apps heal with no per-app change. Its ack gate is
  `fauna_core::generation::escrow_acked_generations`, lifted out of the tip
  resolver so the two share one predicate. Proof, against the real nest:
  `conformance_account_state_walk.rs`'s
  `a_single_device_sign_in_recovers_its_generation_from_escrow_and_holds_the_reception_key`
  (A′ keys generation 1 from the holder's wrap, reads the reception key, and
  tops up a later seedless device; recovery deletes no wrap — only generation 1's
  later shred sweeps it — red with A′ built seedless); the bounds are the module's own unit pins
  (`an_acked_unkeyable_generation_is_recovered_and_asked_for_once`,
  `a_generation_without_a_trusted_receipt_is_never_asked_for`,
  `an_unreachable_holder_is_asked_again_next_pass`,
  `a_wrap_that_does_not_open_is_an_answer_and_is_not_asked_for_again`). Item (2)'s commitment check is pinned at the door (`fauna_mls` `an_escrow_wrap_substituted_under_the_binding_fails_the_commitment`) and through the pass (`a_substituted_wrap_under_the_generations_own_binding_keys_nothing`; the Key↔id gate: `a_squatted_mint_row_draws_no_escrow_request`), and the read half re-checks behind it: `generation_tip::generation_key_for` never serves a retained key that fails a live, id-bound mint's commitment, exactly as `key_for_tip` refuses it.
  The far half of the chain — A′ handing the departed writer's rows over,
  generation 1 then shredding and its wrap being swept — is clause (3)(g)'s,
  pinned by `a_single_device_sign_in_hands_the_recovered_generation_over_then_shreds_it`
  (the re-seal pass entry above).
  **Built 2026-10-01 — the re-presentation behind a recovery.** Item (3)'s last rule: `pump`
  (`fauna_account_plane::account_driver::pass`) runs the fleet plane's
  `reconcile` again when `ensure_recovered` answers `Recovered`, reported in
  its own slot (`PumpReport::fleet_rewalk`). Measured before it, on tui: a
  same-device sign-out and sign-in settled its prologue with the rotation
  ring empty, and the critical-alert sweep's custody feeder skipped as
  `no-held-rotation-keys` while the nest still named the minted DID. Web did
  not show it because its sign-out keeps the origin's replica. Proof, through
  the production pump over the real nest doors:
  `conformance_account_runtime.rs`'s
  `v17_a_sign_in_after_a_sign_out_reads_the_rotation_ring_once_its_prologue_settles`
  (red without the re-presentation: the ring reads empty at `settled`); the
  journey is `test_alert_sweep_directory_feeders_e2e.py`'s
  `test_custody_alarm_reaches_a_fresh_sign_in_before_the_runtime_assembles`,
  which signs out and back in on every app.

- **Built 2026-09-16 — the escrow sweep.**
  Clause (3e)'s wrap deletion rides the receipt retire: the additive
  `delete_escrow_wraps` flag on `fauna.account.state.retire`, honoured only
  beside `no_rows_sealed_under` and only by a retire that lands, deletes the
  generation's `generation_escrow_wraps` rows in the retire's own
  transaction (`bins/fauna-nest/src/db/account_state.rs`); the reclamation
  pass sets it on exactly the receipt retire of a shredded generation
  (`generation_reclaim`'s `Belt::Sweep`). Proofs: the nest's DB test
  `a_belted_retire_that_lands_sweeps_the_generations_escrow_wraps_and_nothing_else`
  (refused belt, no flag, no landing, another generation, flag without belt
  — each leaves the wraps); and the steady-state sweep above now also
  asserts the holder's escrow table holds wraps only for generations a
  current seat still holds a mint row for, the tip's among them. **Fixed 2026-09-21:** the belt's fleet-only premise is now enforced by construction — `no_rows_sealed_under` off `state-fleet` is refused `invalid_request` before the sweep ever runs; regression: `a_belted_sweep_is_refused_off_the_fleet_scope_even_though_the_belt_would_pass`.

- **Built — the export confidentiality axis: skeleton + emission:** the fourth `ACTOR_TABLES` axis
  (§ Nest-side requirements item 1) exists as the required
  `export: Export` field with the ratified vocabulary, the down-only
  ratchet (`tests::the_unreviewed_export_backlog_only_shrinks`), and a
  first evidence-backed tranche of verdicts (the secret-bearing planes the
  row named plus the two full-coverage shaped domains); the rest of the
  registry is `Export::Unreviewed`. **The emission now executes those
  verdicts:** `CacheDb::gather_actor_export` walks `export_emit_legs()` and
  `export_routes.rs` emits `export/tables/<name>.ndjson` per
  `Verbatim`/`Redacted` entry — read `WHERE <column> = ?1` bound in the
  entry's own `ActorKey` spelling, minus a `Redacted` verdict's named
  columns, BLOB values as lowercase hex — while `manifest.json` carries
  rule 4's declaration as an **additive** `coverage` object (format stays
  1): the partiality flag, the `Unreviewed` table names, and the withheld
  tables by name + reason class, both lists scoped to tables this build's
  schema actually has. The emission's *mechanism* is graded on a synthetic
  registry slice (`gather_export_set` takes the entry list), because the
  skeleton left zero `Verbatim`/`Redacted` verdicts and every test against
  the real registry would otherwise pass vacuously; the runtime belt behind
  the declaration is `export_never_carries_a_secret_bearing_table`, whose
  roster of credential/key/escrow tables is kept deliberately **apart from
  the registry's own verdicts** — mutation found the first version asking
  `ACTOR_TABLES` which tables to check, so demoting one to `Verbatim`
  removed it from the check and the leak passed green. **The judgment
  passes draining the `Unreviewed` tail are
  underway: 151 → 140 → 132 → 123 → 107 → 93 → 81 → 71 → 65 → 56 → 51 → 45 →
  41 → 39 → 33 → 27 → 19 → 12.** Cluster 1 (2026-08-15) took the
  key/credential/escrow plane, all `WithheldSecret`; cluster 2 (2026-08-15)
  took the bearer/verification-secret plane — the tables whose secret is a
  redeemable code, a shared verification secret, or a key sealed inside an
  otherwise user-meaningful ledger row — and with it **the emission stopped
  being quiet**: the archive now carries `export/tables/<name>.ndjson` for
  real verdicts, so from here a wrong `Verbatim`/`Redacted` is a live leak
  rather than an inert declaration, and the ruling's *when unsure, leave
  `Unreviewed`* bias is load-bearing. Cluster 2 also landed the two guards
  `Redacted` needed before its first use, both failure modes being silent:
  a walk asserting every `omit` name is a real column of its table (the
  omission is matched by name against the live schema, so a typo drops
  nothing and emits what the reason string says it withheld), and the
  excised-kind prose walk widened to the export axis, which it had never
  covered. Cluster 3 (2026-08-15) took the recovery/public-material plane —
  contents that are published, world-served, or a public-key tombstone —
  and with it the axis's first `WithheldDerived` and `WithheldOperational`
  verdicts, so rule 4's reason-class declaration now has a live example of
  each of its three classes rather than only `secret`. Cluster 4 (2026-08-15)
  took the **MUA-facing bridge plane** — the sixteen `bridge_*` tables the
  DAV/IMAP collection stack rests on — and is the first cluster whose bulk is
  the owner's own *content* rather than material to withhold: the IMAP
  placement index and mailbox list, the CalDAV/CardDAV collections, the two
  owner-expressed preferences, the two auth-history logs and the
  restore-divergence notice all `Verbatim`, against four
  `WithheldOperational` (the three RFC 7162 / RFC 6578 tombstone logs, whose
  only function is one MUA's incremental catch-up against this box's own
  modseq clock, and the daily submission meter). With it the **sealed-columns
  clause has its first live use**: the sealed calendar/vCard bodies ride as
  ciphertext, pinned end-to-end by
  `a_sealed_collection_rides_while_its_tombstone_log_does_not`. ⚠ That test
  also pins a **deliberate divergence between axes on the same three-table
  groups** — succession rules the IMAP three and the CalDAV three inseparable
  because a MUA compares one sync clock across them, while the export
  withholds their tombstone logs, an archive being a snapshot with no clock
  to keep consistent; the two axes ask different questions and correctly get
  different answers. Cluster 4 also landed
  `an_exporting_verdict_never_carries_an_uncleared_secret_shaped_column`,
  which closes the direction the per-*table* secret roster cannot see: a
  later migration ALTERing a key, bearer token or wrapped secret onto a table
  that already exports. It walks every exporting table's real columns, flags
  credential/key/ciphertext-shaped names, and requires each to be dropped by
  a `Redacted` verdict or individually cleared with the reason it is safe —
  so a new *sealed* column must state which side of cluster 1's discriminator
  it falls on (sealed content rides, sealed key material never does).
  Cluster 5 (2026-08-16) took the mail **control** plane beside those
  collections — the account's addresses and aliases, the user-authored filter
  rules, the mail settings and serving toggle, the mailing lists and their send
  history, and the per-message scan verdicts, all `Verbatim`; the list rate
  counter, the auto-reply dedup memory and the two mail **spools**
  `WithheldOperational`. Two rules generalize from it. **(a) A superseded table
  inherits its successor's verdict** — the two pre-Phase-C tables (`imap_mailboxes`,
  `expunged_uids`, both dropped at schema 99) took what their live replacements got, so
  the archive cannot treat the same fact differently by era. **(b) A transient
  spool is withheld even though it carries the owner's own mail**: a spool row's
  `raw_message` is a full RFC822 message in *plaintext*, while the durable copy
  of that message rests sealed in the mailbox, so exporting the spool would mint
  an unsealed copy of already-held mail into an archive an eviction token can
  fetch. `the_mail_spools_never_reach_the_archive` pins that on the *bytes*
  rather than the verdict, so a later promotion to `Verbatim` reds whatever the
  declaration says. Clusters 4 and 5 together also establish where this axis
  and the succession axis **deliberately diverge**: succession asks *what will
  this row do next* (so it clears `forward_all_to`, deletes outward-emitting
  filters and moves tombstone logs), the export asks *whose data is it* (so an
  archived rule, which emits nothing, rides to the person who wrote it).
  Cluster 6 (2026-08-16) took the **family** plane — the eleven `guardian_*`
  tables plus `guardianships` — and its finding is structural rather than
  per-table: **every entry in the family keys on `supervised_actor_id`, so the
  whole plane reaches the ward's archive and never the guardian's**, the
  guardian-side columns being `SUCCESSION_REFERENCES` entries the export never
  walks. That is what makes a two-party plane rulable at all, and
  `the_guardian_family_exports_only_on_the_wards_own_column` keeps it from
  decaying into a second disclosure channel beside the guardian-scoped RPCs
  ([`../behavior/family-safety.md`](../behavior/family-safety.md) § Don't do
  these bounds that surface at envelope metadata and category counts). Ten
  `Verbatim` — the link, the policy the ward is entitled to see by that doc's
  transparency invariant, their own asks, verdicts, usage and notice counts —
  against two `WithheldOperational`: the mail gate's own correlation memory,
  one of whose columns is **spendable** against a guardian budget, so
  exporting it would hand an eviction-token holder a key past a guardian
  control.
  Cluster 7 (2026-08-16) took the **scoring/reporting** plane: the owner's
  spam settings, trained model, training history, sealed personalization
  models, feed factor weights and own engagement all `Verbatim`, against the
  trending cache, the anti-spam behavioural profile and both reporting tables
  `WithheldOperational`. It is the first cluster where a wrong verdict would
  disclose a third party **to** the exporting actor — `sender_reputation` was
  keyed on the person reported, so an actor-keyed read returned other people's
  testimony about the exporter, reporter identities included; the plane stays
  out under [`../behavior/report-sharing.md`](../behavior/report-sharing.md)
  § Report capture (*a reporter's identity never leaves her own nest at any
  count*), and `the_report_plane_never_reaches_an_archive` asserts on the
  archive's bytes rather than trusting the verdict. (`sender_reputation` left
  the registry with the federation reputation leg at schema 109, 2026-10-02 —
  [`federation.md`](federation.md) § Federation residue surface; the test now
  pins `content_reports`, the plane's remaining table.) Three
  vocabulary rulings come with it, each narrowing a variant that was drifting
  wider: **`WithheldDerived` means re-derivable from what the ARCHIVE carries**
  — a claim about the reader, not about the nest's ability to rebuild, which is
  why a trending cache computed from every actor's engagement is `Operational`;
  **a migration calling a table "derived/re-creatable" is answering a RETENTION
  question**, a different one with a different answer (age-out-safe and
  irreplaceable to its owner are compatible); and **where another owner doc
  rules a plane's disclosure, this axis defers** rather than re-deriving from
  the columns — `content_reports` would read as a clean `Verbatim` from its
  schema alone and is withheld because that doc allows no second read surface
  onto the table.
  Cluster 8 (2026-08-16) took the **web-publishing** plane — the uploaded
  source, the custom domains, the subdomain opt-in and the apex designation
  `Verbatim`; the two render caches `WithheldDerived`, which are that variant's
  second and third uses and the ones that make its test precise: **is the input
  in the archive?** A render is a projection of `web_files`, which rides beside
  it with bodies under `include_blobs`, so the owner lacks no data; a trend
  score (cluster 7) is computed from every other actor's engagement and can
  never be re-derived from this owner's archive, which is why that one is
  `Operational`. "The system can rebuild it" decides neither.
  Cluster 9 (2026-08-16) took the **shaped-domain** plane — the nine tables
  the 11 hand-written domains of `gather_export_data` read — and its finding
  reverses the expectation the earlier clusters carried: **not one of the 11
  domains is total.** Every one drops columns, row-filters, or both, so
  `Shaped` was the wrong verdict for all nine and each is `Verbatim`
  (`knocks`, `folders`, `sync_devices`, `groups`, `group_members`,
  `key_packages`, `feeds`, `content`, `content_links`). The shaped half of
  the export has therefore been quietly partial since it was written:
  `posts.json` row-filters `schema LIKE 'post/%'`, so every non-post row the
  actor authored was absent from their archive; `knocks.json` reads
  `poll_knocks`, so every knock already *delivered* was absent; `folders.json`
  carried 6 of 18 columns, dropping eleven knobs the user set in their own app
  (conflict policy, WebDAV serving, paywall tier, retention and snapshot
  policy, and the sealed name/include/exclude labels). **`Shaped` is now a
  checkable claim rather than a prose one:** the variant carries a `covers`
  column list, and `every_shaped_verdict_covers_every_column` walks the real
  schema and requires `covers` ∪ {the actor column} to be exactly the table's
  columns — so a partial shaped domain is no longer expressible, and an
  `ALTER TABLE` reds the guard instead of silently falsifying a verdict. That
  failure was live, not hypothetical: `feeds` grew `scope`,
  `contributor_seeds` and `composition` by `ALTER` after `feeds.json` was
  written, so verifying its coverage the obvious way — against the
  `CREATE TABLE` block — *confirms* a claim that is false. `Shaped` keeps
  exactly its two proven-total entries (`contacts`, `inbox_modes`).
  Cluster 10 (2026-08-16) took the **backup/custody** plane — `backup_custody`,
  `backup_custody_generations`, `backup_destinations`,
  `backup_custodian_checkins` and `restore_history`, all `Verbatim`, four of
  them decided by a sentence the table's own author had already written
  ("None of it is secret — the user's own chosen backup targets") and the fifth
  by the product (`ui/backups.md` § Restore history renders those rows to the
  owner). ⚠ Its lasting finding is about the guard set, not the plane: the
  succession axis calls two of these "a PROJECTION", which reads like
  `WithheldDerived` here and is wrong by cluster 8's test — but demoting them
  **reds nothing**, because the backlog ratchet counts only `Unreviewed` and no
  guard checks *which* withholding class a table gets. The asymmetry is
  deliberate (rule 2's "a wrong Withheld is the status quo" biases every belt
  toward the leak direction), and its cost is the other direction: an archive
  that silently comes up **short** has no automatic witness. That is the same
  blind spot that let the shaped domains stay partial (cluster 9), so each
  plane whose verdict a later reader might talk themselves out of now carries a
  hand-written end-to-end pin (`the_backup_topology_rides_rather_than_reading_
  as_derived`).
  Cluster 11 (2026-08-16) took the **subscription** plane — `subscribers`,
  `subscription_tiers` and `subscribe_requests` `Verbatim` (with two tier-MLS
  tables since retired at schema 91), `feed_contributors` `WithheldOperational`.
  It is where the two-party question resolves in the **permissive** direction
  for the first time: `subscribers` and `subscribe_requests` each name a second
  person — the shape that withheld `sender_reputation` (since removed) in
  cluster 7 — but
  [`monetization.md`](../behavior/monetization.md) § Pillar 1 renders both the
  roster and the request queue to the creator in their own Tiers tab, so
  withholding would take the commercial relationship the creator *owns* out of
  their own records. Two-party is a question, never an answer. ⚠ The plane's
  catch is a table whose name and actor column both point the wrong way:
  `feed_contributors.author_id` is not the feed's author but a contributor a
  discovery feed *found* (the feed's owner is `feeds.owner`), so an
  actor-scoped read returns rows about the exporter sitting inside **other
  users' feeds** — cluster 3's `foreign_recovery_heads` shape, except this one
  matches real rows and would really emit.
  **The guard-set asymmetry is now stated in its strong form** (cluster 10 had
  the weaker one): demoting a `Verbatim` to any `Withheld*` reds nothing, and
  flipping a `Withheld*` to `Verbatim` reds nothing either — the registry belts
  detect **named-secret** leaks through column-name fragments and cannot see a
  wrong-**owner** decision in either direction. So each verdict a later reader
  might plausibly reverse carries a hand-written end-to-end pin, in whichever
  direction the reversal would go (`the_creators_subscriber_roster_rides`,
  `a_foreign_feeds_contributor_rows_never_reach_the_archive`).
  Cluster 12 (2026-08-16) took the **account-record** plane — `actor_last_ip`
  `Verbatim`; `actor_message_dedup`, `feature_usage`, `rpc_idempotency`
  `WithheldOperational` — on the split that a record of what the *owner did*
  rides while the machinery the nest runs *about* them does not.
  `actor_last_ip` is personal data in the strict sense, which cuts **toward**
  exporting: the archive goes to the person it describes, and *what does my
  nest know about me?* is the question this export exists to answer.
  `rpc_idempotency` carries a second, independent reason — `reply` mirrors
  whatever a served reply held, so exporting it would route around every other
  table's verdict through a cache nobody would think to audit. ⚠ `audit_log`
  and `pending_actions` were deliberately left `Unreviewed` by cluster 12 —
  both are nest-wide hash-chained records naming third parties in a `target`
  column and reachable under the export's weakest credential, so ruling them
  is a disclosure judgment about a transparency log rather than a table read —
  and the **escalation discharged them the same day** (2026-08-16), the
  first time the row's escalate-when-ambiguous clause fired in twelve
  clusters. Both are `Redacted`, omitting only the chain columns
  (`audit_log`'s `prev_hash`/`entry_hash`, `pending_actions`' `chain_hash`).
  What decides it is an **authorship invariant every writer holds**: each
  production audit site keys its row to the party who *performed* the act, so
  the `WHERE actor_id = me` slice is by construction a record of the
  exporter's own conduct with content they supplied — safe under the eviction
  token because acting already required knowing it, while withholding would
  deny an evicted or succeeded actor the record of their own acts. **The
  credential carve-out (2026-10-06):** that argument covers *knowledge*, never
  a *bearer credential* the actor once handled — knowing an invite code at mint
  is not holding a live one after removal — so no audit writer stores a bearer
  credential in `target`/`detail`, and `invite.create`/`invite.delete` store a
  keyed fingerprint of the code (subkeyed from the deployment seed, so it is no
  offline guessing oracle) that keeps the two matchable; otherwise the row
  would route around `invite_codes`' own omission of `code`. Pinned by
  `export_api.rs::an_admins_minted_invite_codes_never_ride_their_audit_rows`. The chain
  columns are the one part of the row that is about the *rest of the log*: they
  verify nothing inside a slice and their only marginal information is a
  confirmation oracle on third parties' adjacent entries. Pinned end-to-end by
  `export_api.rs::the_actors_own_conduct_rides_while_the_hash_chain_stays_home`.
  ⚠ The scope caveat a
  reader will trip on is cluster 12's correction restated — this is the
  actions-I-*performed* view, never the actions-about-me view; rows where the
  exporter is the `target` are keyed to the acting admin and do not ride.
  The cluster's durable output is a
  **correction**: `audit_log`'s policy and succession reasons both said
  `actor_id` records "who an entry is about", and every call site passes the
  **acting** admin/guardian with the subject in `target` — an export verdict
  read off that wording would have been ruled on the wrong party, the same
  mistake cluster 11 caught on `feed_contributors`. Both reasons are fixed in
  place; the succession conclusion is unaffected.
  Cluster 13 (2026-08-16) took the **sync** plane — `sync_changes`,
  `actor_channels`, `folder_member_access`, `folder_channel_claims` and
  `content_uid_map` `Verbatim`, `channel_foreign_members`
  `WithheldOperational`. ⚠ It is cluster 11's wrong-party catch in a second and
  sharper form: `feed_contributors.author_id` pointed at the wrong party, while
  `channel_foreign_members.actor_id` points at a party a local export can never
  match at all — the sole producer is the federation-relay branch of welcome
  delivery, whose recipient is homed on the peer nest by construction. Both
  halves are ruled deliberately: the **vacuity** (why nothing leaks today) and
  the **class** (why the verdict must not flip if a local row ever appears —
  the row is the *channel's* relay-authorization record naming which peer nest
  may pull its messages, not the member's data). Its membership twin
  `folder_member_access` runs the same check and comes out the other way, since
  `actor_id` there is the grantee: **the check is per table, never per family.**
  The plane's near-miss is `sync_changes.actor_id`, which is the *recording*
  writer rather than always the set owner (multi-writer shared sets) — it reads
  the right way for a per-actor cut, but a reader who takes it for "the set
  owner" rules the whole journal on a party the column does not name. The
  secret-shaped-column belt fired on three `sync_changes` columns and earned its
  friction on the first: `content_key_version` is a **generation number**
  selecting a key the reader already holds, not a key; `path_sealed` and
  `entry_sealed` are sealed *content* under the exporting actor's own root and
  ride in at-rest form. The split is graded end to end by
  `the_sync_plane_rides_except_the_foreign_membership_row`, whose foreign-member
  seed is **synthetic and says so** — no producer can write a local actor there,
  and the succession ruling forbids such a pin on *its* axis; the
  difference is the direction of the claim, since this one asserts a
  withholding, so passing means "the walk honours the verdict", never
  "production can reach this state".
  Cluster 14 (2026-08-16) took the **identity/lifecycle** plane —
  `actor_successions`, `handle_cooldowns`, `admin_actor_ids`,
  `actor_mls_pubkeys`, `actor_index_pubkeys` and `access_grants`, all
  `Verbatim`; the first plane in four clusters where the wrong-party check
  comes out clean on every table. ⚠ Its own trap replaces that one, and three
  of the six carry it: **a succession `Stay` reasoned from *compromise* says
  nothing about disclosure.** The two pubkey tables stay behind at a ceremony
  because their keys seal *future inbound* mail and a seed thief can derive the
  private halves — a question about who keeps *receiving*. The stored bytes are
  the published halves, which the MTA bridge fetches by RPC, so reading
  "compromise" as "withhold" would have withheld the public key the nest hands
  to anyone who asks. Two facts about `actor_successions` are recorded because
  a later reader will re-derive them wrongly: its slice keys on `old_actor_id`,
  the **retired** identity, and that *is* reachable —
  `auth_core::refuse_if_superseded` guards token *minting* while the export
  endpoint accepts an already-minted bearer or an eviction token and re-checks
  neither — so it is a real disclosure decision under the weakest credential,
  coming out permissive only because a succession statement is public by
  construction; and consequently the **successor's** archive carries no
  succession history at all, their side being `new_actor_id` in
  `SUCCESSION_REFERENCES`, which the walk never touches. That must not be
  "fixed" with a second registry entry on the same table — the walk iterates
  entries, so one table with two would emit its `.ndjson` twice into one zip;
  the successor's view belongs in a shaped domain. The plane also produced the
  best argument yet for keeping the secret-shaped-column belt deliberately
  dumb: it fired on an `actor_successions` rotation-sentinel **timestamp**
  that matched on the word `keys`, and that was cleared by name rather than by
  narrowing the fragment (the column itself left the schema at schema 91).
  Cluster 15 (2026-08-16) took the **residual set**, and it splits three ways:
  `notifications`, `labeler_subscriptions`, `nest_pairings`, `import_sessions`
  and `obligation_action_records` `Verbatim`; `push_subscriptions` `Redacted`;
  `invite_requests` and `labelers` `WithheldOperational`. ⚠ The redaction is the
  sharp one — `(endpoint, key_p256dh, key_auth)` together **are** a sendable Web
  Push credential, the dispatcher authenticates nobody, and no kind this nest
  has can list or revoke a rogue endpoint, so an archive copy is an
  *unrevokable* delivery capability under the eviction token; the registration
  itself (which device, what transport, since when) rides. That is also the one
  place a succession reason transferred wholesale, which sharpens rather than
  breaks cluster 14's rule: it transferred because it was never an argument
  about the ceremony but about what the bytes *are*. `labelers` is the
  wrong-column check in a **third** distinct form — `publisher_actor` is a
  self-signed artifact keypair, a different *kind* of identity rather than a
  different party — and `invite_requests` is vacuous by a mechanism no other
  entry uses: the **export endpoint's own precondition** excludes the population
  the table can hold (`handle_export` requires `get_user` to resolve; every row
  here names an actor with no `users` row, since approve deletes the row).
  `obligation_action_records` is the first time the defer-to-the-owner-doc rule
  runs **permissive**: it withheld `content_reports` because
  [`report-sharing.md`](../behavior/report-sharing.md) routes every read through
  one gate, while [`moderation.md`](../behavior/moderation.md) § Queue defines
  `fauna.moderation.actions` as serving these rows to the connection actor's own
  content and the legal-takedown design is a visible tombstone plus appeal,
  never a silent removal — transparency to the author is the mechanism's point.
  Cluster 17 (2026-08-17) took the **ATProto PDS** plane — `atproto_identities`,
  `atproto_account_settings`, `atproto_native_records`, `atproto_preferences`,
  `atproto_blobs` and `atproto_retired_identities` `Verbatim`, against
  `atproto_consent_requests` `WithheldOperational`. ⚠ The withholding is the
  first time this axis has ruled on a **ceremony** rather than on a thing: what
  the owner durably agreed to is an `atproto_oauth_grants` row, which cluster 12
  left `Verbatim` precisely so the connected-apps audit
  [`principles.md`](../principles.md) § The user always controls their data
  requires reaches the archive, while a consent request is only the minutes-long
  question that minted it — swept by expiry however it was answered, so an export
  can catch one only mid-flight. That sweep is also why a **denial** leaves no
  durable trace anywhere: a property of the sweep, not of this verdict, and
  `Verbatim` could not have recovered it. The plane's `Verbatim` half is the
  identity/lifecycle rule (cluster 14) applied to a second directory-published
  key set — `user_rotation_pub` / `signing_pub` / `bridge_rotation_pub` are what
  the PLC directory serves to anyone who resolves the DID, while the secret
  halves are `atproto_identity_key_blobs` and the client-held senior rotation
  key — plus the repo itself, whose export a landed neighbour had already
  *presumed*: `atproto_identity_key_blobs`' `WithheldSecret` reason draws its
  line as "the repo it signs for exports under `atproto_native_records`". Pinned
  end-to-end on the bytes by
  `the_atproto_repo_rides_while_the_consent_ceremony_stays_home`, which asserts
  both directions because cluster 10's asymmetry means no generated check can
  falsify a `Withheld*` class — mutation grade 4/4 exact, each mutant caught by
  the assertion written for it, including the one that grades the *clearance*:
  deleting `rkey`'s entry reds the column guard, so it is load-bearing rather
  than decoration. ⚠ And it **corrects a ranking this backlog carried
  for two clusters**: these seven are absent from `FEATURE_GATED_ELSEWHERE` (which
  names fifteen tables, all `ap_*` / `bluesky_*` / `nostr_*`), so the plane was
  visible to a bare `cargo test` all along and was the *cheapest* remaining work,
  not part of the expensive gated tail.
  Cluster 18 (2026-08-17) took the **eleven feature-gated bridge tables** — the
  set the ranking above had left as the expensive tail — 12 → 1: `ap_follows`,
  `ap_post_map`, `bluesky_dm_map`, `bluesky_interactions`,
  `bluesky_saved_feeds`, `nostr_dms` and `nostr_follows` `Verbatim`;
  `ap_accounts` and `bluesky_accounts` `Redacted`; `bluesky_convos` and
  `nostr_federation_cursors` `WithheldOperational`. ⚠ **The plane's lesson is
  that three of its tables lie in their column names, each in a different
  direction, so a verdict ruled off `CREATE TABLE` would have gone wrong on all
  three.** `bluesky_accounts`' `access_token` / `refresh_token` / `dpop_key` are
  `BLOB NOT NULL` and written **empty** by their only writer (the live OAuth
  session is keyed by DID in `atproto_sessions`, `WithheldSecret`) — they are
  redacted anyway, because `Verbatim` there would be a verdict correct only
  until a writer changed and the change would be silent. `bluesky_convos`'
  `fauna_convo` mapping is written as the **empty string** and its `last_poll` /
  `poll_interval` by nothing at all, which answers cluster 17's
  ceremony-or-thing question with a third answer — *neither*: a live row is the
  DM poller's `(actor, opaque remote convo id, last message seen)` watermark,
  delivery bookkeeping whose conversations' actual messages ride the `content`
  plane. And `bluesky_dm_map` has **no production writer at all**, so its
  `Verbatim` is ruled on the columns' meaning and emits nothing until one
  arrives. ⚠ `ap_accounts` is the cluster-14 trap inside a single table:
  succession carries `encrypted_privkey` forward untroubled because it is sealed
  under the nest KEK and no seed thief read it — a **compromise** argument —
  while this axis must drop it because it signs HTTP Signatures *as* the actor,
  a **disclosure** one. Two axes, one column, opposite answers, both right.
  `ap_post_map` is the wrong-party check passing on a table that genuinely holds
  other authors' rows: inbound Notes key on `synthetic_actor_id(actor_uri)`, a
  BLAKE3 derivation nobody holds a key for, so a real exporter matches exactly
  the rows they authored and the synthetic rows belong to actors that can never
  authenticate. ⚠ **All eleven verdicts are invisible to a bare `cargo test`** —
  all three bridges are opt-in, so the grading command is
  `cargo test -p fauna-nest --lib --features nostr,bluesky,activitypub
  actor_tables`; with anything less the guards report green having looked at
  nothing. The same cluster closed a gap: the guards'
  `apply_available_bridge_schemas` applied each bridge's `CREATE_TABLES_SQL` but
  not the post-`CREATE` `ALTER`s that `bluesky::init_db` and
  `activitypub::init_db` also run, so `bluesky_accounts.write_through` and
  `ap_post_map.remote_actor_uri` were unobserved *by construction*. The remedy
  is structural rather than the two missing calls: each bridge now exposes one
  `apply_schema(&Connection)` and `init_db` and the guards' seeding are its only
  callers. It sits beside `init_db` in the nest rather than in the bridge
  crates, which export SQL constants and do not depend on `rusqlite`. Since the
  bridge genesis collapse there is no post-`CREATE` `ALTER` left to miss: every
  column is in the bridge's `CREATE_TABLES_SQL`, and `apply_schema` is that
  block plus the shared additive column reconciler
  (`bridge_schema::apply_genesis`). Pinned end-to-end on the bytes
  by `the_bridge_links_ride_while_their_credentials_and_cursors_stay_home`,
  which seeds through each bridge's real `init_db` and fills the credential
  columns NON-empty on purpose, so the redaction is a property of the verdict
  rather than of the current writer — **mutation grade 5/5 exact, three
  exclusive**, and the exclusive ones are the load-bearing statement: demoting
  `bluesky_convos` out of `WithheldOperational` reds that pin and *nothing
  else*, which is cluster 10/11's asymmetry confirmed rather than assumed.
  **Two of the eleven are gone (2026-10-03, schema 121):** `bluesky_convos` and
  `bluesky_dm_map` were dropped with the dark DM poller they belonged to, and
  left `ACTOR_TABLES` with them — the Bluesky DM leg stores its rows in the
  bridged-conversation family (`bridge_conversation_rooms` /
  `bridge_conversation_messages`, both `Verbatim`) and keeps no cursor at
  rest, so the watermark verdict above has no table left to rule and the pin
  drops its two seeds (`conversations.md` § Where logic lives → *The `Bridged`
  adapter*, ruling 3). Neither held a row a user owns: one was a re-derivable
  cursor whose only writer never ran, the other had no writer.
  Cluster 19 (2026-08-17) ruled the last one, `segment_records` `Verbatim`, and
  **the backlog reached ZERO: every table in `ACTOR_TABLES` now carries an
  individually-reasoned export verdict**, 151 of them across nineteen clusters.
  Two facts make that verdict safe and are worth carrying. The table is the
  segment store's **mirror, never its bodies** — a row is a placement
  (`kind`, `segment_id`, `record_cid`, `bucket`) plus the mail-kind sparse
  floor, and `record_cid` is a content ADDRESS of the `content.blob_hash` class,
  so naming a record does not open one. And `scope_id` holds **two kinds of
  identity** since the Plan 6 T4 `actor_id` → `scope_id` rename: `mail` scopes
  to the recipient actor, `post` to the AUTHOR actor, `calendar`/`card` to the
  owner, but `conv` scopes to the **channel** — so a `scope_id = ?actor` walk
  returns the owner's own records and can never return a conversation scope.
  That is cluster 15's different-KIND-of-identity check meeting the one table
  holding both kinds in one column. `legal_takedown_ref` rides for a positive
  reason: [`moderation.md`](../behavior/moderation.md) § Categories &
  enforcement makes a legal takedown a visible tombstone plus appeal, so
  transparency to the author is the mechanism's point.
  **The manifest must not be read as saying more than it does.**
  `ActorExportSet::partial()` is defined as *any table still `Unreviewed`*, so
  it now reports **false** on every export: that means no open judgments remain,
  **not** that the archive is complete — `withheld_tables` goes on declaring by
  reason class what the nest holds and does not export. Of the two gaps that
  stood behind that flag, **(a) is BUILT 2026-08-17: the export
  reaches the segment store behind `include_blobs`** — the four actor-scoped
  planes (`mail`/`post`/`calendar`/`card`) are finalized and enumerated in the
  async gather, and the blocking zip writer streams each `.dat`+`.meta` pair
  from disk verbatim into `export/segments/<kind>/`; the eviction-export-token
  ruling and the `segment_store` manifest declaration are per the *Payload
  stores* ruling above (item 1). Pinned by
  `include_blobs_carries_the_segment_pair_at_rest_form_verbatim` and
  `the_eviction_token_pulls_the_same_archive_the_owner_does`
  (`tests/export_api.rs`). Gap **(b) is BUILT the same day**: the
  owner's `conv` records reach the export through the membership-resolved
  `conversations` shaped domain (the Universe paragraph's second instance,
  item 1 above — never a second registry entry on a channel column). The
  remainder both closes left — the **conv BODIES** — is **BUILT 2026-08-17**: the owner's channels' live conv bodies ride behind
  `include_blobs` at `export/conversations/bodies/<channel>/rec-<seq>`,
  read through the serving door's own primitive
  (`segments::conv::read_after_seq`), so tombstoned records stay excluded
  and legally-withheld bodies stay withheld by the same single gate; the
  channel-scoped pairs themselves stay out deliberately
  (compaction-undropped records), still declared by
  `segment_store.channel_scoped_not_included`, and the manifest's
  `conversations.bodies_included` declares the door. Pinned by
  `the_owners_conv_bodies_ride_the_membership_resolved_door`
  (`tests/export_api.rs`), which drives the HTTP endpoint and asserts all
  four dispositions: live rides verbatim, tombstoned absent, taken-down
  absent-but-declared, non-membership unreachable.
  The mirror halves are pinned by `the_segment_mirror_rides_while_the_channel_scope_stays_out_of_reach`,
  which seeds through the production writers (`records_db::insert_mail` /
  `insert_conv`) and asserts both directions — the mirror rides, the channel
  scope is unreachable from a per-actor walk — **mutation grade 2/2 exact**,
  and M1 is exclusive: demoting the verdict to `WithheldOperational` reds that
  pin and nothing else, which is the guard-set asymmetry holding to the last
  verdict in the registry.
