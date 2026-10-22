# Cross-nest shared folders + channel append — target state

Owns: cross-nest-shared-folders
Status: ratified — split verbatim out of [`federation.md`](federation.md) on 2026-10-17. The section was ratified 2026-07-18 (nest plane and client read side BUILT 2026-07-19, write plane BUILT 2026-07-20); every later ruling and build below carries its own date.
Authority: the **kinds, gates, and wire rules** of the cross-nest shared-folder and channel-append plane — the substrate-vs-policy model shared by the conversation and folder rails, the structural foreign-member gate and the foreign-writer gate, the Welcome relay's set name and owner label, the folder-owner marker and its succession arm, recipient-side access discovery, the cross-nest writer roster read, relay serving across nests, the public folder read plane, and the build record of all of it. Defers: the nest↔nest carrier, the peer-auth model, the trust model, key packages, the anonymous-surface security model, the federation kind inventory (§ Federation residue surface) and the nest-writer backup plane to [`federation.md`](federation.md); folder behavior and UI to [`../ui/folders.md`](../ui/folders.md) § Sharing; custody and crypto to [`mls-group-key-material.md`](mls-group-key-material.md) § M2; the client leg's status to [`../behavior/file-sync.md`](../behavior/file-sync.md) § Multi-writer shared sets. On conflict in those domains, raise it.

Last verified: 2026-10-17 (the split; every ruling date, pin and residual below is its entry's own, carried across unedited) | Sources: `bins/fauna-nest/src/{federation_handlers.rs,folder_handlers.rs,conversations_handlers.rs,chunk_routes.rs,db/channels.rs}`, `libs/fauna-conversations/src/backends/fauna_mls.rs`, `libs/fauna-client-folders/src/{custody.rs,mls_adapter.rs}`, `libs/fauna-client-mls-sync/src/orchestration.rs`

> **Audience:** nest and shared-Rust contributors touching a folder shared with a member on another nest, or a channel whose home is another nest.
> **Purpose:** the authority on what one Fauna nest serves to a peer nest's member of a channel it homes — reads, appends, leaves, writes, rosters and bytes — and on the gate each of those passes.

*Split verbatim out of [`federation.md`](federation.md) on 2026-10-17. The section and its build record were 98K of a doc thirteen days from the whole-file read ceiling; they form one concept family, while the rest of that doc specifies the nest↔nest carrier and its auth. A stub remains at each original location; prior history: `git log --follow docs/goal/architecture/federation.md`. A bare `§` reference below to a section this doc does not have — § Peer-auth model, § Trust model, § Key packages, § Security, § Transport, § Federation residue surface (the residue table), § Nest-writer backup plane — names that section of [`federation.md`](federation.md).*

## Cross-nest shared folders + channel append (ratified 2026-07-18; nest plane + client read-side BUILT 2026-07-19, write plane BUILT 2026-07-20)

Design + rationale: the 2026-07-18 read-write/cross-nest design (tracked internally);
behavior/UI owner: `../ui/folders.md` § Sharing; custody/crypto owner:
`mls-group-key-material.md` § M2. This section owns the **kinds, gates, and wire rules**.

**Substrate vs. policy (rationale of record, 2026-07-19 — the one-sentence model for the
two rails).** Conversations and shared folders share one substrate — one MLS group =
one channel, one roster (`actor_channels` + `channel_foreign_members`), one Welcome
relay, one fetch/append/leave kind family, bulk bytes on the by-hash HTTP plane for
both — and the **channel claim is the single discriminator**: an unclaimed channel is
egalitarian (any member posts and commits — a conversation), a claimed channel is
owner-managed (roster-membership Commit admission, owner-only share/evict/rotate —
a folder).
Everything above that bit is deliberately per-rail **policy**, not accidental drift:
reader/writer access, byte caps, and metering exist only on the claimed side because a
filesystem has an owner and a storage-exhaustion abuse model, while a chat has neither
(its abuse model is spam → behavioral checks). Do not force symmetry at the policy
layer: a future conversations access feature (e.g. read-only announcement members)
copies the channel-keyed *pattern* of `folder_member_access`, never generalizes the
table or its claimant-gated writer (a conversation has no claimant to gate on). The
known place substrate symmetry bites is the conv-reuse latent (a folder bound to a
live conversation's group would flip that conversation to owner-only share/evict/
rotate — Commit admission is unaffected, since `fauna.federation.channel.append`
below already makes it roster-membership on both rails) — the claim-time fix belongs
to whatever flow first builds real conversation-binding; its full obligation list is
owned by `mls-group-key-material.md` § M2 / Rotate-on-removal, not restated here.

- **One structural gate for the whole family** — the `channel.fetch` gate reused
  verbatim: serve iff `foreign_member_home_nest(channel_id, requester) ==` the
  connection's verified `origin_nest_id` (the row the home nest itself wrote at
  Welcome-relay time). Write kinds (`changes.record`, `write_token.mint`,
  `channel.append` on folder channels for content) additionally require the
  owner-granted `access == 'writer'` in `folder_member_access`. Consistent with
  § Trust model: the peer's signature is attribution; the authorization is state
  this nest wrote.
- **`fauna.federation.channel.actors` — REGISTERED 2026-07-29 (cross-nest chat
  add slice 2); its contract lives on its kind-table row above** (§ the
  cross-nest calls table, *Channel roster read*): the `channel.fetch` gate
  verbatim, the union answer, the ids-only disclosure rule, the
  no-auto-register-on-any-hop rule, and the distinct-client-kind
  (`fauna.conversations.channel.actors_remote`) skew rationale. Heal
  mechanics stay owned by `mls-group-key-material.md` § M2, chat bullet.
- **The nest→nest id→handle question — RATIFIED 2026-09-10: no nest ever
  asks another what handle an actor wears; a member's own home nest TELLS the
  room's home, on the member's own drain.** The question this answers: a room
  member homed on another nest has no `users` row on the room's home, so the
  floor roster read (`../behavior/conversation-rooms.md` § Implementation
  status today, the roster bullet) could not name it and the member rendered
  as its elided actor id. Two shapes were weighed and one refused:
  - **REFUSED — a reverse lookup, `"what handle does actor X wear?"`, as a
    kind, under any gate.** The nest that would answer (the actor's home) can
    verify nothing about the asker's standing: a foreign room's floor roster
    lives on that room's home nest, and the actor's own nest records nothing
    about the rooms its member joined elsewhere. So the only gate available
    is the nest-signature — attribution and throttling, never authorization
    (§ Trust model) — which makes the kind an *open* id→handle oracle bounded
    by rate alone: the first surface in any plane to serve that direction, on
    the disclosure-minimal roster surface whose one relay answers "hex actor
    ids only" (the `channel.actors` row). Handles are public in the forward
    direction (§ Key packages — `by_handle` is world-readable), but the
    reverse turns an opaque 32-byte id — observable only by those an actor
    has actually reached: co-members, recipients — into a name for whoever
    holds the id. Not built, and not to be built.
  - **RATIFIED — the announce.** The member's own home nest is the authority
    for handles at its own domain, and it already originates every federated
    call for that member. So it volunteers the member's `handle@domain` —
    joined nest-side from its own `users` row, never taken from the client —
    as two additive fields on the `fauna.federation.channel.fetch` it relays
    (`FedChannelFetchRequest::requesting_handle` / `requesting_domain`), and
    the room's home records the pair on the member's `channel_foreign_members`
    binding **after** `require_foreign_member` admits the drain
    (`federation_handlers::record_announced_handle`). `room.list_roster` then
    joins it exactly as it joins a local member's `users.handle`; nothing on
    the client changes shape. The name travels *with* the member into the
    rooms it is in — the audience every chat product shows a member's name
    to, and the audience the local join already served — never *out* to
    whoever asks.
  - **The domain is bound to the announcing key from the domain end, never
    taken on the announcer's word.** Any nest can advertise any domain string
    (its `nest.info.domain` is a self-description), so a hostile nest could
    otherwise seat its member as `alice@bank.example` in every room it joins.
    The room's home therefore runs the ordinary discovery chain of § Peer-auth
    model *from the asserted domain* — `https://{domain}` → the shared
    SRV/port resolver → `fauna.nest.info` over authenticated TLS
    (`FederationChannelPool::resolve_domain_nest_id`, cached per domain) — and
    stores the pair only when that chain lands on the authenticated
    `origin_nest_id`. The same TLS floor § Security rests on; a nest naming
    its member at a domain it does not serve is refused and logged as the
    signal it is. The verification runs off the fetch path — a spawned task
    the reply never awaits — so a slow or hostile domain never delays the
    page. **Validated and rate-limited before anything is recorded.** The
    asserted domain must first be a syntactically valid `host[:port]`
    authority (`fauna_core::web::is_domain_authority_syntax`: a bare hostname
    or IP literal, an optional numeric port, at most
    `fauna_core::web::MAX_HOSTNAME_BYTES` octets) or the announce is ignored
    like a malformed handle, before it ever reaches the dedup/verification
    state. Identical announces are a no-op before any lookup, and a failed
    resolution is cached too, both for a bounded window: a hostile peer buys
    at most one discovery **per distinct domain** per window, never one per
    fetch (alternating between two non-resolving domains does not defeat
    this, since the cache keys on the domain, not the binding), and past the
    window the same assertion is re-verified, so a domain that was only
    transiently unreachable is not stuck on its first failure forever
    (`FederationChannelPool::resolve_domain_nest_id`'s negative cache,
    ). **The window alone bounds
    the cost, not the resident memory** — a peer sending fresh
    syntactically-valid domains fast enough would otherwise grow the negative
    cache without limit inside one window, so it is additionally capped at a
    fixed size (`FederationChannelPool::MAX_DOMAIN_FAILURE_ENTRIES`). **Past
    the cap a newly-failing domain is simply not cached — nothing is evicted
    to make room for it**, so the one-discovery-per-distinct-domain-per-window
    bound above holds only while a peer's distinct-domain footprint inside one
    window stays at or under the cap; a peer that floods past it pays a fresh
    discovery on every single assertion of whichever domains the cap turned
    away, while the resident negative-cache set itself still never exceeds
    the cap either way (). A
    domain already being resolved by a concurrent binding short-circuits to a
    distinct "in flight" outcome rather than starting a second network
    attempt or being mistaken for a cached failure (singleflight per domain);
    total concurrently in-flight verifications across every peer are capped
    too (`FederationChannelPool::MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS`),
    checked before the assertion is recorded so a drop at either cap is never
    mistaken for a completed check and is free to retry on the member's next
    drain ().
  - **Why an additive field and not a distinct kind** — the `channel.actors`
    row's own dividing line, answered rather than assumed away: an old home
    nest ignoring the pair leaves the member elided, an old member-nest
    omitting it leaves the member elided — both the shipped fallback, benign
    in both skew directions — where `actors`' partial-roster-as-clean-success
    would have corrupted a membership-mutating heal. A benign degrade rides
    an additive field; a corrupting one needs a distinct kind.
  - **Staleness.** The announce rides *every* drain, so a rename lands on the
    member's next poll; the stored pair is display-only, refreshed by the
    member's own reads and gone with the binding row on `channel.leave` — no
    cache, no TTL, no invalidation protocol. Client-side the id-keyed read
    re-asks a listed-but-nameless member at a widening gap of polls
    (`FaunaMlsBackend::NAMELESS_REASK_CAP`), because "nameless" now means
    "not announced yet", which the member's first drain changes.
  - **Never an identity input.** What lands is what the roster *shows*;
    every membership decision keys on the actor id, an announce for an actor
    with no binding row writes nothing, and the member-reported mirror still
    carries no handle (`../behavior/conversation-rooms.md` § The floor
    roster). Witness: `conformance_cross_nest_conversations_client.rs`
    (`a_foreign_members_handle_rides_its_own_drain_and_the_room_home_names_it`
    — the honest drain names the member on the room's home and through the
    client seam, the spoof at another nest's domain is refused, a rename at
    the member's own domain lands, a malformed handle is ignored).
- **The foreign-member binding is inviter-asserted (TOFU) — a named, accepted
  premise (analysis 2026-07-18).** The `channel_foreign_members` row's
  `home_nest_id` is resolved from the *inviter-client-supplied* `nest_url` at
  Welcome-relay time — nothing binds it cryptographically to the recipient. A
  hostile inviter (the owner, for folders; any member, for conversations) can
  therefore misbind a recipient to an attacker-controlled nest. Bounded and
  accepted for v1: MLS still carries confidentiality + message authenticity (the
  misbound nest gains only ciphertext/metadata reads), misattribution stays inside
  the declared nest-asserted-provenance envelope (`key-material-hierarchy.md`
  § M2), and the residual effects are read-DoS of the genuine member plus
  last-writer-wins re-binding on re-invite — all within the inviter's existing
  power (they chose to add the member). Consequence for every kind in this family:
  never derive quota, billing, or identity decisions from `home_nest_id` alone.
  **That bound is enforced in two layers (2026-08-30).** The claimant gate
  settled who may *write* a grant; it left the *conflict* arm open, and on the
  conversation rail there is no claimant to gate on — so the power to move an
  existing binding rested on knowledge of the 32-byte channel id alone. The
  sole writer now splits insert-if-absent from update-existing: a first grant
  stays open to any permitted caller (cross-nest DM initiation depends on it —
  the caller is on no roster yet), while **rebinding an existing grant
  requires standing on the channel** (the claimant, or a rostered actor on the
  unclaimed conversation rail) — and **first authenticated use pins the
  binding**: the first federated call the bound home nest is served for the
  member (`require_foreign_member`, the one seam every exercising call flows
  through — an outbound Welcome-relay ack is contact with the
  *inviter-asserted* nest and deliberately confirms nothing) stamps the grant
  confirmed, after which the standing-based arm refuses to move it. Only the
  claimant — a claimed folder channel's owner, whose rebind power is the
  accepted premise above — still moves a confirmed grant, and the move clears
  the stamp so the incoming nest must earn its own confirmation. The pin is
  what excludes the **MLS-removed ex-member**: removal is an encrypted commit
  the nest never observes, and nothing deletes conversation-rail roster rows
  (the only roster delete is folder-rail eviction/self-leave; the rail has no
  same-nest leave kind at all), so standing there can only ever attest "was
  ever rostered" — a predicate a former member passes forever. Rather than
  asking the roster for an event it cannot see, the gate stops trusting the
  roster once the binding has been exercised.
  **The narrowed accepted residual is the establishment window, for a newly
  created binding**: between the Welcome relay and the bound nest's first
  served call, a rostered actor (an ex-member included) can still move the
  unconfirmed grant — the healing window. Accepted because the window is
  short by construction (the recipient's first drain, `direct-messages.md`
  § Technical Flow — Cross-Nest step 3, is the very next thing that happens)
  and re-binding inside it stays within the premise above. Two consequences
  are deliberate, not oversights: a grant misbound at relay time and then
  exercised by the misbound nest is pinned too — the healing rebind died with
  the attack it enabled, and the remedy on the conversation rail is a fresh
  channel; and a confirmed member whose home nest dies without an identity
  succession is re-established the same way, since the cooperative re-homing
  path — the bound nest's own `channel.leave` (deleting the grant), then
  re-invite, or the claimant's evict + re-invite on the folder rail — needs
  the old nest alive. Read "the inviter" in the premise above as: the
  claimant, or — while the grant is unconfirmed — a rostered actor. That a
  confirmed binding on an unclaimed channel thereby has no mover but the
  incumbent is ruled a **per-object remedy** satisfying the client-state
  recoverability invariant — conditions and rationale at
  [`nest/common.md`](nest/common.md) § Client-state recoverability →
  *Per-object remedies*, with this case as the worked example.

  **Identity succession re-points the member, never the binding (2026-08-31).**
  A verified peer succession `A → A'` renames the actor on every
  `channel_foreign_members` row it owns; it does **not** move `home_nest_id`.
  The statement carries no home-nest field at all (`../behavior/identity-succession.md`
  § The succession statement (wire)), so there is nothing for it to assert, and
  the powers enumerated above are the complete list — a succession is not the
  claimant and not a rostered actor. Where the successor already holds a row on
  the same channel (separately welcomed, or a second statement re-pointing onto
  an existing edge), the primary key admits one row and the **predecessor's**
  binding is the one kept, stamp and all; the successor's is dropped and, when
  the two disagreed about the home nest, logged. Resolving that collision the
  other way is what let a caller who merely knew the 32-byte channel id take a
  pinned grant: plant a first grant for the (public) successor id through the
  permissive `Unclaimed` arm, let the succession delete the genuine row in its
  favour, then earn the pin on the first served call — after which, on an
  unclaimed conversation channel, nothing could move it back. Re-homing through
  a succession is therefore the same act as any other re-home, subject to the
  same powers: a rostered actor's re-invite while the kept grant is unconfirmed,
  the claimant's once it is confirmed, and otherwise the fresh channel the
  previous paragraph names — the paragraph's aside about a nest that dies
  "without an identity succession" is about the *statement* keeping the old nest
  reachable for a cooperative `channel.leave`, never about the succession
  rebinding anything by itself.
  **The member's grant rides the re-point (built 2026-10-03), and a second
  predecessor does not (ruled 2026-10-02, not built):** the same transaction carries the member's
  `folder_member_access` rows on the channels whose roster row it moved (a
  grant the successor already holds stands), and a statement naming a
  successor this nest already holds a link into is refused — owner
  [`writer-signed-change-records.md`](writer-signed-change-records.md)
  § Writer-signed change records, ruling (8)(j).

  **A second, distinct residual — the migration window, for every binding
  that existed before `confirmed_at` did (2026-08-30).** The column is
  additive and nullable with no backfill — correctly, since no backfill
  could honestly claim contact the nest never witnessed — so at upgrade
  every pre-existing binding starts unconfirmed, exactly as exposed as
  before the confirm-on-first-use fix landed. For these rows the window is
  *migration → that binding's own next served federated call*, not *Welcome
  relay → first drain*: there is no Welcome relay inside it, so the
  short-by-construction reasoning above does not bound it. The bound instead
  is a client-driven poll — nothing nest-side drains a channel on its own,
  only a member's own client calling `fauna.conversations.channel.fetch`
  with `nest_url` set originates the federated fetch that pins the grant —
  so a **dormant** conversation (no member currently polling that channel)
  leaves its legacy binding movable indefinitely. **Accepted for v1, on the
  same terms as the establishment window above**: the alpha population is
  small, the window self-heals the moment any member's client next drains
  the channel rather than only on redeploy, and the exposure stays inside
  the inviter's already-accepted power — read-DoS of the genuine member plus
  last-writer-wins re-binding, MLS confidentiality and message authenticity
  untouched. Not a request to backfill a witness the nest never observed;
  revisit only if a mechanism arrives that can derive one honestly (e.g. a
  boot-time reconcile keyed on a served-call record that predates the
  migration).
- **`fauna.federation.channel.append`** — the mutating twin of `channel.fetch`,
  shared by conversations (cross-nest send — `direct-messages.md` § Technical Flow
  — Cross-Nest owns that flow) and any channel-riding surface. **Folder-channel
  Commit admission (re-ratified 2026-08-24, superseding the 2026-07-18
  claimant-only refusal; both planes — `channel.append` reuses
  `channel_send_core` verbatim):** on a claimed folder channel a
  `ChannelEnvelope::Commit` is admitted from the claimant **or any actor on the
  channel roster**; off-roster actors are refused (the `ingest_channel_envelope`
  roster check, side-effect free). The 2026-07-18 rule — only the claimant may
  append a Commit — was found mutually unsatisfiable with two other ratified
  rules: members MUST send application traffic
  on the set's channel (`p2p.md` § Cross-user shared-set transfer — the
  advertisement/custody carriage), and the device-owned-epoch invariant
  (`devices.md` § Cross-device MLS group-state sync) makes every member device
  post a bare self-`Update` takeover Commit before its first application send —
  so under claimant-only, a member seat could structurally never advertise or
  receipt custody. The resolution moves owner-only **roster management** to the
  only layer that can see commit content: MLS commits are PrivateMessage
  ciphertext to the nest (it classifies *that* an envelope is a Commit, never
  *what* it commits), while every member decrypts the staged commit. So:

  - **Nest**: roster-membership is the Commit admission rule (evicted members
    and outsiders stay out — the roster-**add** paths all stay claimant-gated,
    **both halves of the roster**: `actor_channels` and the cross-nest
    `channel_foreign_members`, whose relay-side write is a roster-add like any
    other and gates the same way, so eviction durability is
    unchanged; removed-member *exclusion* never rested on this gate —
    `mls-group-key-material.md` § Rotate-on-removal owns the claim guard's
    families).
  - **Members** (shared Rust, all apps): `MlsEngine::process_commit`'s **folder
    commit policy** — on a channel stamped with a durable folder owner
    (owner-side at group mint; member-side at `join_folder_welcome` from the
    Welcome's MLS-authenticated sender), a proposal-carrying commit
    (Add/Remove/PSK/extensions — any roster or group-state change) merges only
    when its leaf-authenticated committer is the owner; a bare self-`Update`
    (the takeover) merges from any member. Deterministic at every honest
    member; refused commits are skipped like intrinsically invalid records,
    with a durable pre-decrypt memo so the every-launch folder-rail re-walk
    repeats the verdict instead of stalling on the consumed ratchet
    generation.

  **The marker follows the owner's verified succession (ratified
  2026-09-22).** The durable folder owner is a **trust
  anchor with two readers**: the commit policy above, and the owner-attested
  declassification verdict, for which a member's seat supplies it as the one
  identity the nest cannot forge
  ([`encryption-at-rest.md`](encryption-at-rest.md) § Readable classes →
  *The declassification is owner-ATTESTED*). A succession retires an identity
  key, typically because it was stolen, so a marker that keeps naming the
  predecessor keeps handing the thief exactly the two powers the succession
  exists to remove: roster authority on the channel, and the power to write a
  member's copy of the folder unsealed. The marker therefore **re-points to
  the successor, from an anchor the nest cannot forge**, and the same rule
  answers both readers at once — they never diverge. **Member side:** the
  in-group succession statement the sweep posts between its two commits
  (`GroupMetaMessage::Succession`,
  [`../behavior/succession-propagation.md`](../behavior/succession-propagation.md)
  § Propagation → *MLS groups*) reaches the folder channel like any other
  application envelope — the folder rail already decrypts them to route the
  share-endpoints body — and is put through the **same session-wired
  witness and the same roster-pair door** the conversations rail uses
  (`SuccessionWitness`, `settle_verified_succession`): a statement that
  verifies, whose `old_actor_id` **is this channel's recorded owner**, and
  whose successor **is seated here** re-stamps the marker to `new_actor_id`
  **at once — at the ceremony's midpoint, before the remove-old commit** —
  and that timing is load-bearing in both directions. Not on the roster
  pair's completion: the remove-old is authored by the successor's leaf, so
  it is the re-stamp that admits it, and a marker that waited for it would
  wait for ever. Not on verification alone either: a true statement is
  public and any member may carry one ahead of the sweep, and a marker moved
  before the add-successor landed would refuse that add — the old leaf's
  one constructive act — and strand the channel. A statement whose successor
  holds no leaf here is dropped, exactly as the conversations arm drops it,
  and when the sweep reaches this channel it posts the statement again beside
  its own add. A statement the witness cannot settle yet parks and re-drives
  under the conversations arm's rules, keyed by channel (a folder channel has
  no thread), because a consumed application message is the only copy the
  seat will ever hold. **Owner side:** the sweep stamps its own join
  (`fauna_client_recovery::group_sweep`, `stamp_successor_folder_owner`): the
  successor's fresh seat takes the marker the predecessor's engine held —
  re-pointed to the successor where the predecessor was the owner, copied as
  is where it had merely joined someone else's folder channel — on the fresh
  and the resumed path alike, and that stamp reaches the successor's other
  devices through the MLS state-replica plane like every other provider value
  (`devices.md` § Cross-device MLS group-state sync). The anchor is again one
  the nest cannot forge: the predecessor's own engine, on the device running
  the ceremony. The registry-anchored re-stamp — `MlsEngine::restamp_folder_owner_markers`
  over the predecessors this device attested
  (`attested_predecessor_actor_ids`), never a served field — is the
  belt-and-braces primitive for a marker the replica plane left naming a
  predecessor; the launch backfill that was to host it was removed 2026-09-25
  (below), so it has no launch caller today, and the sweep's stamp is the
  owner-side mechanism. **No self-authored record confers this authority:** a governed
  room's step-0 succession record is what its *policy* admits the two commits
  under, but the old leaf's holder can author one naming any successor, so on
  a folder channel the chain-verified statement is the only source. **What
  this leaves, stated plainly:** a member seat the statement never reaches
  keeps the retired key as its anchor — the same residual
  `succession-propagation.md` names for a group the sweep never reached (the
  nest's transport half, the claim re-point of
  [`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md)
  § Re-key scope → the MLS-groups row, has been built since 2026-08-03, so the
  sweep does reach a succeeded owner's claimed channels); a legacy unstamped
  member seat stays on open commit processing as before; and a statement the
  witness refuses *for now* on a seat whose walk then meets the remove-old is
  answered by the hold below, inside the bound it states. Build ledger:
  § Implementation status today.

  **The folder commit walk inherits the harvest wait (ratified 2026-09-28).**
  The fork the arm left open: a statement the witness refuses *for now* (no
  anchor held yet for the retired identity) parks, but the successor's
  remove-old that follows it in the same walk meets a marker still naming the
  predecessor, is refused by the commit policy and **memoized** — the decrypt
  consumed the committer's ratchet generation, so the memo is what keeps the
  every-launch re-walk from stalling on it — and the re-drive that later
  re-stamps the marker can never re-admit that commit; the seat has forked
  from the group's epoch. **The rule: while a witness-refused statement
  naming this channel's recorded owner is parked on it, the member seat's
  folder commit walk stops BEFORE every commit record on that channel** —
  cursor left behind the record, nothing decrypted, nothing memoized, the
  next pass retries — after re-asking the witness once (an anchor may have
  landed since the park; verified, the marker re-stamps and the commit folds
  in the same pass). The consumption is the irreversible act, so the only
  sound hold is ahead of the decrypt: a peek at the staged commit's committer
  ahead of the memo was refused on that ground (staging is the decrypt), and
  holding a staged commit un-merged across passes would leave a crash with a
  consumed generation and no memo — the wedge the memo exists to close. **The
  bound is the witness's own harvest wait, one level up**
  ([`../behavior/identity-succession.md`](../behavior/identity-succession.md)
  § The succession statement → *the harvest wait*), because a parked
  statement is any member's to post — the transport sender is unbound by
  design, and to a witness holding no anchor a forgery naming the owner is
  indistinguishable from the honest statement — so an unconditional hold
  would let one hostile member wedge every other member's commit rail for the
  session. So the walk holds **only in a session that runs a peer-anchor
  sweep** (`arm_succession_harvest_wait` — the same announcement that arms the
  witness; never armed, never held: the wait fails open exactly as the
  witness's does), **only until the sweep has settled the recorded owner by
  any arm** — a seed, a standing outcome, the spent retry budget, which the
  backend records as *spoken for* — and **never past it**: after the settle a
  still-refused statement holds nothing (it stays parked for a read-path seed,
  as before), the walk resumes, and the commit meets the marker as it stands.
  **To make that settle a guaranteed event, every folder channel's recorded
  owner joins the harvest's walk** (`RailBackend::harvest_anchor_wants`, the
  FaunaMls rail's folder-channel owners, this account's own identity excluded
  → `ConversationsManager::harvest_walk_actors`): the sweep walked thread
  rosters and room-policy names only, and a folder channel has no thread, so
  an owner the member shares no conversation with was never harvested — the
  folder park already broke the parking-bound premise "nothing parked waits on
  a harvest that will not come", and the hold would have made that an
  indefinite wedge. Same door, same grade, same once-per-session guard and
  retry ladder as a roster peer; bounded by the seats this device holds. **The
  bound, stated as the attacker sees it:** a forged statement naming the owner
  holds the channel's commit walk from its decode until the sweep settles the
  owner — one profile fetch online, the ladder's ~100 s budget offline — once
  per session; a second statement after the settle holds nothing. A
  poll-count lifetime was refused as a clock in disguise (it releases before an
  offline sweep's budget and holds after an online settle), and dropping the
  park at the settle was refused because a read-path seed may still re-stamp
  the marker later in the session. **The park rests with the engine (built
  2026-09-28)**, so the hold survives the session: a launch re-walks the log
  from 0 but cannot re-read a statement an earlier launch consumed, and
  without the rested copy its first walk would meet the successor's
  remove-old with no statement and no hold — refused, memoized, forked. Every
  park is mirrored to the engine's provider KV, one slot per channel
  (`fauna:parked_folder_succession:<channel_id>`, latest wins; the
  owner-bound admission is unchanged and is what bounds it); each launch's
  first walk over the channel drains it back into the park before any
  record; the harvest speaking for the owner — verified or still refused —
  forgets it (after draining it, should the sweep speak first), so a forged
  statement costs at most one hold window per launch; a walk that drops the
  statement forgets it, and so does leaving the channel
  (`MlsEngine::forget_group`). Derived, recreatable state — the statement
  rides the channel log whether or not a seat consumed it. **The folder rail
  diverges from the conversations rail here on purpose:** that park stays
  session-lifetime ([`../behavior/identity-succession.md`](../behavior/identity-succession.md)
  § The succession statement → *the harvest wait* (e), "Parking keeps its
  roster bound and session lifetime"; the at-rest variant it rejected under
  *What a statement may cost the member who receives it* was forger-minted
  re-drive work every later session pays, plus an expiry policy, to close a
  seconds-wide race). Here the harm is a forked seat, and neither cost
  carries over: the owner's settle is the expiry, and the slot is one per
  channel. **Residuals, stated plainly:** (1) retired — the session ending
  before the sweep reaches the owner no longer ends the hold; (2) an owner the sweep cannot read — homed on
  another nest (`fauna.profile.get` answers for same-nest identities only),
  resting delegated, or never having mirrored a head — settles at once with
  no anchor, and the walk resumes into the pre-hold residual; (3) a session
  with no sweep never holds. The remedy for all three is unchanged: the owner
  re-shares the set to the forked member. Pinned by `fauna-conversations`
  `a_witness_refused_statement_holds_the_remove_old_until_the_harvest_settles_it`,
  `a_forged_statement_holds_the_walk_only_until_the_harvest_settles_the_owner`,
  `without_an_armed_sweep_the_folder_walk_never_holds` and
  `the_folder_owner_joins_the_harvest_walk`.

  **Named residuals, accepted with the re-ratification:** (a) a hostile member
  can churn epochs with takeover commits — the same set-DoS class as
  Application-envelope spam (which was always roster-open); a nest-side
  per-(actor, channel) commit rate cap is BUILT (`conversations_handlers::
  channel_send_core`, claimant exempt, `bridge_rate_limit::
  CHANNEL_COMMIT_LIMITER_CONFIG` — 60/hour, refusal is the retryable
  `fauna.conversations.rate_limited`, `RpcError::action() => Transient`; the
  federation `channel.append` relay inherits it for free, same chokepoint);
  (b) two
  distinct exposures share this name and only the first is a mere transition
  window — **(b-i)** an old client **build** (code predating the member-side
  policy) merges hostile roster commits until the user upgrades: a
  transition-window exposure bounded by the alpha population, no wire shape
  changed (old member clients' takeover commits simply start landing against
  a new nest; a new client against an old nest keeps today's
  refused-and-retrying behavior); **(b-ii)** an upgraded client's **legacy
  seat** — a channel this engine minted or joined *before* the marker existed
  — is not fixed merely by upgrading the build, since the marker is stamped
  once, at mint/join time, never retroactively by a code update. **No
  legacy seat exists** since the 2026-09-24 baseline reset — every seat was
  minted or joined by code that stamps — so the owner-side launch backfill
  that once swept for them (`FoldersAuthor::backfill_owner_markers`) was
  removed by the compat-remnant sweep (`compat-remnant-sweep.md` § Program
  4), and (b-ii) is closed on both sides by construction.
  Pinned by
  `channel_send_admits_rostered_member_commit_on_claimed_folder_channel`
  (nest), `fauna-mls`'s folder-commit-policy tests (member side, both halves
  red-verified), and `stamping_the_owner_marker_arms_the_policy_on_an_unstamped_channel`
  (`fauna-mls`, the stamp alone arms the policy). The claim table keys on
  `folder_channel_claimed_by`,
  which marks only **pure** (fresh-group) folder channels; the
  conversation-reuse caveat is unchanged — its full obligation list is owned by
  `mls-group-key-material.md` § M2 / Rotate-on-removal, not restated here.
  Conversation channels are unchanged: any member may commit.
  Redelivery-safe via the L3 `idempotency_key`; duplicate MLS payloads quiet-skip
  as past-epoch — note the L3 cache is per-connection and TTL'd, so append
  consumers rely on MLS-layer duplicate tolerance, never on nest-side
  exactly-once.
- **`fauna.federation.channel.leave`** — self-scoped, idempotent delete of the
  requester's own `channel_foreign_members` row. The first mutating member-gated
  kind; deliberately generic (a cross-nest conversation participant's leave is the
  same operation). Revokes future discovery only — never key material already held
  (parity with same-nest voluntary leave).
- **`fauna.federation.folder.{changes.fetch, content_key.fetch}`** — the
  read plane: change-log rows (manifest refs, sizes, `content_key_version`,
  `author_actor_id`) and the sealed content-key envelope (opaque to both nests).
  The home nest resolves `channel_id → folders` row via `mls_group_id`.
- **`fauna.federation.folder.changes.record` + `write_token.mint`** — the write
  plane: the record relay runs the same owner-pays metering + per-member-cap +
  version-floor transaction as a local record and nest-stamps
  `author_actor_id = requester`; the mint returns a **short-lived, write-only
  bulk-byte token** (the `BulkWriteAuth` token arm's shape) for direct HTTPS
  chunk/manifest POSTs. **Bulk bytes never ride the federation channel** in either
  direction (`transport.md` § carve-out): reads GET by ciphertext hash on the open
  bulk plane; writes POST with the token. Two contract points (Phase-3 pass,
  2026-07-19): **(i) the record relay is exactly-once by CONTENT, never by the
  L3 key** — the per-connection idempotency cache cannot span reconnects, so the
  handler treats a record matching the path's latest entry on
  `(acting actor, path_hash, manifest_hash, change_type, content_key_version)`
  as already-applied: it returns the existing seq and charges the owner's meter
  nothing (the same dedupe applies to the same-nest record path — uniform
  shape); **(ii) the token is TTL-bound, not revocation-checked per POST** — it
  carries `Write` scope only, a hard-coded short TTL, and a `(channel, actor)`
  binding that routes metering to the owner's quota under that member's cap;
  eviction/demotion refuses the next *mint*, and an outstanding token's residual
  window is one TTL constant — accepted (parity with "generations already held
  are not revoked"), no per-POST grant re-check without a measured abuse signal.
- **`fauna.federation.conversation.write_token.mint` — the conversation rail's
  byte-plane write leg (ratified + BUILT 2026-09-09).** The same short-lived,
  write-only bulk-token shape as `folder.write_token.mint` (contract point (ii)
  verbatim: TTL-bound, not revocation-checked per POST — a leave refuses the
  next *mint*), behind the structural foreign-member gate alone. Its purpose in
  the fleet: `../behavior/conversation-rooms.md` § The home nest → *Attachment
  bytes* rules that a room's attachment bytes rest on the room's home nest, and
  this is how a foreign member's bytes get there without ever riding the
  channel. Contract on its kind-table row above (*Conversation attachment
  write token*).
- **Relay serving across nests (ruled 2026-10-01; refutable; the read token
  and the read door BUILT 2026-10-03, the `residency` stamp and both serving
  kinds BUILT 2026-10-04).** The flow, and why it has this shape, is
  [`../behavior/relay-serving.md`](../behavior/relay-serving.md) § Relay serving → *A
  member on another nest*; this bullet owns the kinds, gates and stamps. All of
  it is additive, and bulk bytes still never ride this channel.
  - **`fauna.federation.folder.serve.announce`** (member's nest → home nest).
    Names the member, one of its devices, and whether that device serves the
    folder. Gate: the write gate — foreign-member-bound-to-home-`nest_id` plus
    `access == 'writer'`. The home nest keeps the result in memory only, bounded
    in count, on a lease of a hard-coded length that the member's nest renews;
    an announce that says *no longer serving*, a lapsed lease, and the member's
    removal or demotion each drop it.
  - **`fauna.federation.folder.chunk.wanted`** (home nest → member's nest — the
    one folder kind besides the Welcome that the HOME nest originates). Carries
    the member, the device, a request id and a store key. The member's nest
    serves it only when the calling nest's verified `nest_id` is the one it
    forwarded that device's announce to, so a nest can ask only about a folder
    the seat itself announced with that nest as its home. It pushes the ask on
    that one connection and replies whether a connection was there to push to.
    It learns a store key; it never sees the answer, which goes from the seat to
    the home nest's bulk plane.
  - **`fauna.federation.folder.read_token.mint`** (member's nest → home nest).
    The read-scoped twin of `write_token.mint`, behind the structural member
    gate alone — a reader holds no write grant to mint under. Same TTL rule
    (contract point (ii) above). The token is `Read`-scoped, so every bulk
    write route refuses it; its purpose is `ForeignFolderRead`, which no
    bridge can mint. The member's client asks through its own nest with
    `fauna.folders.read_token.get`, the twin of `write_token.get`.
  - **Where a federated byte-plane token is taken for a read.** One door: the
    store-miss arm of the home nest's chunk route. It takes a read token or a
    write token, reads the token's actor, and resolves the hinted folder against
    `channel_foreign_members` at every use — so unlike a write, a read is
    re-checked per request and ends with the membership row. The answer route of
    relay serving is a bulk write route and takes the write token as every such
    route does. The door reads the token's purpose, and only to pick the
    roster: a token of the foreign-folder pair is resolved through
    `channel_foreign_members` and nothing else (never the owner arm, never
    the same-nest roster), among folders whose channel the folder's owner
    claimed; a token of any other purpose does not open the arm. The hint is
    the one every reader sends — the folder's name, or its name hash, which
    wins — and a miss of any kind is the plain `404`.
  - **The `residency` stamp.** The folder's content residency rides beside
    `caller_access` on every federated folder read reply and beside `access` on
    the Welcome relay, nest-authoritative on both as `access` is. Absent means
    *not stated* — never *full*.
- **Recipient-side access discovery (ratified 2026-07-20; seed + refresh wire
  BUILT 2026-07-22).**
  Two wire-additive deltas, no new kind:
  `fauna.federation.welcome.deliver`'s request gains `#[serde(default)] access:
  Option<String>` (resolved nest-authoritatively from `folder_member_access` at
  `welcome_deliver_core`, exactly as `set_name` is — never sender-asserted), and
  **every federated folder READ reply** gains `caller_access: Option<String>`
  (the home nest stamps the requester's current role — it resolves membership
  for the gate anyway), threaded through the relay reply so the recipient's
  client can refresh its stored copy. *Scope widened at build time (2026-07-22),
  extending rather than revising the ratified design:* the 2026-07-20 statement
  named `changes.fetch` as the carrier, but the `nest_url`-routed
  `fauna.sync.changes.list` relay has **no production client caller** (only nest
  conformance tests) until the sync agent gains foreign read routing — whereas
  `content_key.fetch` is the federated read a foreign member's client runs on
  every commit poll. Both kinds now stamp it, so the refresh is live today and
  the rule a cold reader carries is one line: *every federated folder read
  reply stamps the caller's current access.*
  **The value is advisory-for-UI only,
  never an authz input** — enforcement remains this section's write-kind gate
  (`require_foreign_writer`), and a bind is verified by an eager
  `write_token.get` at the bind gesture (a typed refusal fails the bind loudly).
  An **absent** stamp asserts nothing (a pre-Phase-4 home nest, or a set with no
  role row = the implicit reader default): the client keeps the value it holds,
  and it is never read as a revocation — demotion is enforced at the next
  mint/record, never inferred from a missing field.
  No push/notification kind for demotion — poll-parity with same-nest access
  changes (which have no push either); mid-life revocation is enforced at the
  next mint/record and handled fail-closed + loud client-side (mechanics owner:
  `../behavior/file-sync.md` § Multi-writer shared sets). The asserted value
  rides inside the accepted TOFU inviter-binding envelope above: a lying or
  misbound home nest asserting `writer` gains only what it already has
  (ciphertext + the duty to accept writes it cannot read).
  **A companion field rides the SAME carriers** (the Welcome relay + the
  `content_key.fetch` reply), additive, no new kind, same absent-asserts-nothing
  semantics — the home nest's own deployment
  `nest_actor_id`, the identity root the member's agent needs to graduate an SPKI
  pin for the **direct byte-plane dial** to a self-signed home (owner:
  `transport-trust.md` § Transport trust, the federation-granted Axis-2 row;
  `../behavior/file-sync.md` § Multi-writer shared sets). Both flow into the
  member's `ForeignFolder` row (the `fauna.state.folder-keys` kind's `foreign/`
  rows since the `__config` rail retired 2026-10-02 —
  [`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution
  schedule → *The kinds*) and the pushed engine-key blob. Like
  `access`, both are home-nest-resolved and peer-asserted-safe: the cadence is a
  backstop interval; the identity is only ever *checked* against the channel
  binding the byte plane proves, so a wrong value fails the pin, never weakens it.
  **A third field rides the `content_key.fetch` reply alone (built 2026-09-28):**
  the set's owner-stamped `content_key_floor`, the value `fauna.folders.list`
  projects on the row a cross-nest member does not have — refreshed into
  `ForeignFolder.content_key_floor` so the member's engine arms its pre-seal hold
  from it (owner: `../behavior/on-demand-files-shared-sets.md` § Shared sets on a capability
  host → *One mechanism*, question 2). Same absent-asserts-nothing semantics; an
  input to the member's own HOLD only — the enforcement stays this nest's
  `stale_content_key` refusal, so a wrong value costs a held or refused write,
  never a seal under a rotated-past generation.
- **The cross-nest owner label (ratified 2026-10-03; BUILT 2026-10-05).** A set shared across nests names its owner on
  two member surfaces — the pending-share knock ("Shared by ‹…›",
  `../behavior/value-formatting.md` § Account display label) and the shared
  set's display name in the member's file browser (`docs (alice)`,
  `../behavior/on-demand-files-shared-sets.md` § Shared sets on a capability host,
  decision 3) — and a cross-nest member's own nest holds no `users` row for
  that owner, so without a label both fall back (an unknown sharer; the
  home nest's host). The id→handle bullet above forecloses the obvious fix: no nest asks
  the owner's nest what handle an actor wears. The ratified shape is the
  announce's, applied to the folder plane:
  - **Carrier — the home nest volunteers, on the two stamps it already
    originates.** The set's home nest is the authority for its owner's handle
    and already stamps the Welcome relay and every federated folder read
    reply nest-authoritatively (`set_name`, `access`, `home_nest_actor_id` —
    the *Recipient-side access discovery* bullet; on the Welcome the
    recipient's own nest delivers `home_nest_actor_id` as the origin it
    verified on the channel, not the home's word,
    `transport-trust.md` § The post arm). It adds the owner's handle
    and domain, joined from its own `users` row and `handle_domain_if_set()`
    — never client-asserted — as two additive fields on each: the
    `fauna.federation.welcome.deliver` request (`owner_handle` /
    `owner_domain`; a folder share is owner-gated — `share_core` verifies the
    caller owns the set — so the sharer the Welcome names IS the owner, one
    meaning on both carriers) and `fauna.federation.folder.content_key.fetch`'s
    reply (the read a foreign member's client runs on every commit poll, so a
    renamed handle refreshes exactly as `caller_access` does). The pair is
    split on every wire and at rest, as the announce
    (`requesting_handle`/`requesting_domain`) and the roster read
    (`RoomMemberRow.handle`/`domain`) carry it; it is joined only at display.
  - **Trust rule — the member's own nest binds the domain to the origin's key
    before anything reaches the client; a bare handle never crosses a nest
    boundary.** A handle rendered bare is, everywhere in the apps, one of the
    viewer's own nest's users, so a foreign nest stamping `alice` would seat a
    stranger as a local contact on the one surface — the knock — whose whole
    job is to let the recipient weigh who is asking. So the receiving nest
    (the Welcome relay) and the relaying nest (the read reply) forward the
    pair only when the ordinary discovery chain *from the asserted domain*
    (`FederationChannelPool::resolve_domain_nest_id` — the id→handle bullet's
    verification, same syntax validation, caches and TLS floor) lands on the
    authenticated origin: the handshake-verified `origin_nest_id` on the
    relay, the pool's verified identity behind `peer_url`
    (`resolve_peer_nest_id`) on the read. A mismatch forwards nothing and is
    logged as the signal it is. The `shared_by` actor id stays unstamped
    cross-nest, so the contact gate still reads every cross-nest arrival as a
    knock (`../ui/folders.md` § Sharing) — the label names, never admits. The
    client's trust rule is one line: a handle with a domain beside it is a
    foreign user its own nest bound to that domain's key; a handle with none
    is a local `users` row.
  - **When the binding runs differs by path, under one cost budget.** The
    Welcome relay is a one-shot, per-share delivery, and the knock is where
    the name matters most, so the receiving nest AWAITS the resolve inline —
    cached after the first contact per domain, singleflighted, under the
    resolver's own dial timeout and the announce's concurrency cap
    (`MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS`; past the cap, or on any
    failure, the Welcome delivers unstamped — a slow or unreachable domain
    never refuses a share, it only leaves the fallback standing). The read
    reply is the polled path the announce was designed around: it forwards
    the pair on a cache hit only, and a miss spawns the same verification so
    the next poll carries it — the reply is never delayed.
  - **Join rule on the member.** `WelcomeInbox`/`WelcomePayload` carry the
    pair as `shared_by_handle` + an additive `shared_by_domain`
    (`shared_by_handle` is no longer same-nest-only: bare for a local sharer,
    paired with its domain for a verified foreign one); `ContentKeyGetReply`
    carries `owner_handle`/`owner_domain`; the member's `ForeignFolder` record
    gains both as a fifth latest-wins advisory pair under `updated_at`
    (written at accept from the Welcome, refreshed by
    `custody::refresh_foreign_set_from_reply` from every read reply; an older
    device's stamp-less newer write clears it and the next refresh restores
    it — never a wrong name). The display string is the canonical
    `handle@domain` from one shared formatter
    (`fauna_core::format::qualified_handle`, lifted from
    `RoomRosterKnownMember::qualified_handle` so the roster and the folder
    surfaces cannot drift — `../behavior/value-formatting.md` § Account
    display label owns the label), fed to `account_display_label` as the
    handle: `shared_by_display` on the knock, `docs (alice@example.com)` from
    `on_demand_presence::held_sets`, the home nest's host standing in until
    the pair lands. Display-only on every hop, as `set_name` is: never a
    lookup key, never an authorization input.
  - **Refused:** qualifying the handle with the verified origin's *host*
    (`alice@nest.example:8443`) — honest without a resolve, but a host is not
    a handle domain, so it would name the owner by a string no picker
    resolves and no roster shows, diverging from the one identity form every
    other cross-nest surface uses; and a distinct cross-nest-only field
    carrying an unverified handle — the receiving nest can verify, so an
    unverified name reaching the client would regress the announce's own
    rule.
- **Client-kind wire rule (ratified):** a cross-nest client kind must **fail loud
  on a relay-unaware home nest**. An additive `nest_url` field is permitted only
  where the old-nest path fails visibly anyway (`changes.list`/`content_key.get`/
  `changes.record`/`folders.leave` → local `not_found`); where the additive path
  would **silently misdirect a write** — `channel.send`, whose local append
  blackholes the message — a **distinct kind** is required
  (`fauna.conversations.channel.send_remote`); so is a read whose answer is an
  **authorization input** and could come back partial or from the wrong set as
  a clean success (`fauna.conversations.channel.actors_remote`, the *Channel
  roster read* row; `fauna.folders.members.list_actors_remote`, *The cross-nest
  writer roster read* below).
- **Old-peer error shapes differ by plane; both are loud (verified 2026-07-18).**
  A new *client* kind on an old home nest fails typed `unknown_kind` (per-actor
  plane). A new *federation* kind sent to an old peer hits the allowlist first and
  fails `fauna.protocol.unauthenticated` (connection stays open). The relaying
  nest maps the peer-side `unauthenticated` to a client-visible "the home nest
  needs an update" — never a spurious auth failure.
- **The cross-nest writer roster read (ratified 2026-09-29; BUILT 2026-09-29).** The one read the writer-signed change-record
  reader runs to learn which actors hold `writer` on a set
  (`writer-signed-change-records.md` § Writer-signed change records, ruling (3)
  — same-nest `fauna.folders.members.list_actors`) gains its cross-nest arm as
  **one new federation kind, `fauna.federation.folder.actors.fetch`
  `{requesting_actor_id, channel_id}`**, in the folder content plane's family:
  the family's one structural gate (`require_foreign_member`), the S7
  claim-resolve (`claimed_folder_for_channel`), the **same projection the
  same-nest read serves** (`actor_channels` ∪ `channel_foreign_members`, each
  with its `folder_member_access` grant, the owner as the `role == "owner"`
  row — one projection function behind both doors), and the `caller_access`
  stamp every federated folder read reply carries. **Disclosure follows the
  *Channel roster read* row: hex actor ids, roles and grants — `handle` rides
  EMPTY across nests** (the reader needs ids; a member already holds the
  membership off the MLS tree; and id→handle is the announce's direction, never
  a nest's answer to another — the 2026-09-10 ruling above). The owner row and
  each `writer` row also carry the landed succession statements that end at
  that member (ruled and BUILT 2026-10-01 — `mls-group-key-material.md`
  ruling (8)(b) owns why a reader needs them and how it verifies them; a
  reader-access row carries none). A statement is a public record — the
  pre-identity `fauna.recovery.succession.lookup` serves it to anyone holding
  the retired id, and the sweep posts it in the set's own group — so the read
  discloses a writer's retired id to the set's other members and nothing more.
  Read-only on every hop: registers nothing, mints nothing. **The client leg is the distinct kind
  `fauna.folders.members.list_actors_remote { channel_id, nest_url }`** (reply
  shape = `ActorMembersListReply`), never an additive `nest_url` on
  `members.list_actors`, for exactly the *Channel roster read* row's sharper
  reason: a roster is an **authorization input** to the reader, and an old
  member-nest ignoring an additive field on a *name*-addressed request would
  answer the member's own same-named set's roster as a clean success —
  installing another set's writers on this one — whereas an unknown kind fails
  typed `unknown_kind`, the read fails, and the reader's fail-closed posture
  holds (the last roster stands; never read ⇒ a member's rows are held). An old
  HOME nest refuses the federation kind `unauthenticated` → the relaying nest's
  `peer_nest_outdated` → the same hold. Birth-shape under the 2026-09-24
  baseline, like the signature it serves. **Rejected:** carrying the roster on
  the `ForeignFolder` record (the reader-side reasons are ruling (3)'s: a
  nest-projected authorization fact inside `BackupKey`-sealed custody, merged
  per device by a CRDT, where the same-nest reader keeps no on-disk roster
  either); an `access` field on `fauna.federation.channel.actors` (access is
  claimed-side *policy* and that kind's union answer is the substrate's — the
  § *Substrate vs. policy* rationale at the top of this section); a plain
  additive `nest_url` on `members.list_actors` (above). Consequence, not ruled
  here: the p2p share leg's cross-nest carve-out
  (`SyncEngine::refresh_share_writer_roster`) reads the same roster and may lift
  on this relay when p2p cross-nest is scoped.
- **Throttling:** all kinds inherit the channel's per-IP + per-nest throttles —
  the write plane's record kind at the writer-plane budget, every other kind at
  the channel default (§ Nest-writer backup plane → *Throttling* owns the budget
  and its reasoning); nothing new to configure (no-operator invariant — cadences
  and budgets are Rust constants).

### The public folder read plane (design ratified 2026-08-18; BUILT 2026-08-18 — folders re-model phase 4, slice 4f-i; see the § Status bullet below)

Behavior owner: `../behavior/folders.md` § Publicly-synced follow (address/floor/strip/flip-back
semantics live there). This subsection owns the **kinds, gates, and wire rules** — what the
deliberately-closed kind inventory admits, and why the member-gated family above is untouched.

- **One new federation kind: `fauna.federation.folder.public.fetch`** (read-only), plus its
  client twin `fauna.folders.public.fetch` (the caller's own nest relays, exactly as the
  member reads relay — same carrier, same loud-failure shapes on an old peer/home). The kind
  inventory grows by **one read-only kind**, a deliberate decision of this design pass: the
  v1-closed inventory (decision of record 2026-07-20) closed the *write/conflict/lease* surface;
  the publicly-synced read path is ratified product surface (`principles.md` public exception +
  the re-model's audience bullet), and a public folder is the one folder shape the existing
  family structurally **cannot** address — every kind above is `channel_id`-keyed and
  membership-gated, while a public folder is typically unbound (no MLS group, no channel, no
  roster row to gate on).
- **Why a distinct kind, not an audience arm on `folder.changes.fetch`:** the family's one
  structural gate (`foreign_member_home_nest == origin_nest_id`) stays exactly as strong as it
  is. An OR-ed audience arm would put a world-readability branch inside the member gate, where
  one bug opens member data; the distinct kind's gate is the inverse shape — **serve iff the
  addressed row's current `audience == 'public'`, else `not_found`** — and its handler can never
  serve a sealed row's data because nothing else is reachable through it. Same reasoning
  nest-locally: `folder_authz::can_read_folder` keeps its membership-only behavior (the
  `FolderReadGrant` enum gains a `Public` variant for surfaces that need to name the grant, but
  no blanket arm — a blanket arm would flow into `authorize_snapshot`, folder listing, and every
  future call site; the public read core authorizes independently, fail-closed by construction).
- **Request carries NO `requesting_actor_id`** — deliberate, not an omission: there is no
  membership to check, so the follower's identity never crosses the wire. The home nest sees the
  requesting *nest* + source IP (the throttle keys) and nothing about which user follows.
  Addressing: `(owner_actor_id, folder_name)` or the pinned `folder_id`, `since` cursor, `limit`
  — reply: folder meta (`folder_id`, plaintext name, `home_nest_actor_id` stamp for the
  byte-plane SPKI pin) + the floor-filtered, stripped `SyncChange` rows (projection contract:
  `../behavior/folders.md` § Publicly-synced follow).
- **Zero state, zero metering:** the handler writes nothing (no follower rows — not enumerable,
  not floodable into disk) and charges nothing (reads are unmetered everywhere; the abuse bound
  is the existing per-source-IP `/64` + per-nest throttles every `fauna.federation.*` kind
  already rides — `serve_request` step 2.5). Bulk bytes stay off this channel per the standing
  carve-out: manifests/chunks GET by hash on the open bulk plane (plaintext for a public folder;
  world-readable by ratified design).
- **Version compat:** all additive. The client twin on a relay-unaware home nest fails typed
  (`unknown_kind` / the peer-side `unauthenticated` mapped to "the home nest needs an update") —
  loud per the client-kind wire rule; nothing silently misdirects (reads only).
- **Status: BUILT 2026-08-18** (phase 4 slice 4f-i, `git log --grep 'phase 4 slice 4f-i'`).
  Both kinds are registered and the § Federation residue surface table carries this plane's
  row. What landed: the floor column `folders.public_floor_seq` (schema **v44**, additive —
  `MIN_READER` stays 42 — stamped in the ONE audience writer, in the same UPDATE as the flip
  and guarded on the pre-update value so a re-assert never re-stamps); the independent read
  core `bins/fauna-nest/src/folder_public.rs` (address resolve → audience-only gate → floor
  filter → strip → frame budget), which both kinds call; `FolderReadGrant::Public`, named but
  never returned by `can_read_folder`; and `bins/fauna-nest/tests/
  conformance_folder_public_fetch.rs` pinning all four design oracles (the gate's
  indistinguishability, the floor + its re-stamp, the strip, flip-back-as-revoke) plus
  "a public read writes nothing".
- **One wire detail the design's prose did not anticipate.** The stripped projection means
  **no value**, not **no key**: `author_actor_id`, `path_sealed` and `content_key_version`
  carry `skip_serializing_if` and so lose their keys, but `SyncChange.device_id` is one of the
  struct's original fields with neither `skip_serializing_if` nor `#[serde(default)]`, so it
  rides as an explicit `null`. Adding the attribute would make a new nest emit rows that fail
  to decode on every peer and client lacking the matching `#[serde(default)]` — a compat break
  inside a major ([`version-compatibility.md`](version-compatibility.md)). A `null` discloses
  nothing about the owner's device fleet, so the confidentiality property is identical.
- **The client half is BUILT (status refreshed 2026-10-02 — this entry read "not yet built"
  from 2026-08-18 on, long after the pieces landed).** 4f-ii: `FollowedFolder`
  (`libs/fauna-core/src/data.rs`) rests on the `fauna.state.follows` kind — one row per
  followed folder, an unfollow a tombstone (`libs/fauna-account-plane/src/follows_rows.rs`;
  planned for `__config` first, on the kind since 2026-10-02 —
  [`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule →
  *The kinds*); the follow/unfollow write recipes are shared Rust
  (`libs/fauna-client-folders/src/follow_ops.rs`, lifted 2026-08-20), the followed-folders
  list is `fauna_client_config::load_followed_folders`, and the reads (availability, the
  listing from the relayed changes) are `public_follow.rs` beside it. 4f-iii: the follow UX
  ships on the apps (web's `wasm-folders.ts` wrappers landed 2026-08-19; the e2e markers below
  name the rest). 4f-iv: `tests/e2e-unified/tests/api/test_public_folder_follow_cross_nest.py`
  (2026-08-18) drives two real nests over the API, and
  `tests/e2e-unified/tests/test_folder_follow_outcomes.py` (2026-09-22) drives the cross-nest
  follow through the app UI on tui, web, linux, macos, ios and windows, beside its one-nest
  unfollow, revoke, bad-address and network-fault cases.


## Implementation status today

The entries below moved verbatim from [`federation.md`](federation.md) § Implementation status today on 2026-10-17; each carries its own landing date. The carrier, auth and serving entries they build on stay there.

- **Relay serving across nests (ruled 2026-10-01) — the read direction BUILT
  2026-10-03, the stamp and the serving kinds BUILT 2026-10-04, the seat
  BUILT 2026-10-09** (§ Cross-nest shared folders + channel
  append → *Relay serving across nests*). **Built:**
  `fauna.federation.folder.read_token.mint`
  (`federation_handlers::folder_read_token_mint_handler` — the structural
  member gate, then a `Read` token of purpose `ForeignFolderRead` on the
  write token's TTL constant), its relay `fauna.folders.read_token.get` on
  the member's nest, and the client call
  `fauna_client_folders::FoldersClient::read_token_get`; and the read door —
  `chunk_routes::relay_chunk_for_folder` takes a bulk token of the
  foreign-folder pair and resolves the hint through
  `folder_authz::resolve_foreign_readable_folder`, one roster read per
  request. Pinned in `bins/fauna-nest/tests/sync_relay_serving.rs` (a
  cross-nest member reads a metadata-only folder's chunk by relay under its
  write token and under a read token, nothing rests; no other roster or
  purpose opens the arm; a removal ends the reads inside the token's life; a
  read token opens no write route) and in
  `conformance_federation_channel.rs` (the mint's gate). **A reader's engine
  takes the read token (2026-10-05):** `engine_lifecycle::assemble_engine`
  gives a cross-nest binding marked `read_only` the read-token bearer
  (`write_token_bearer::folder_read_token_bearer`) and every other one the
  write-token bearer, so a cross-nest READER's on-demand host hydrates —
  driven across two nests by `cross_nest_agent_capstone.rs`;
  `../behavior/on-demand-files-shared-sets.md` § Shared sets on a capability host,
  *Implementation status* → *Decision 3*. **Built (2026-10-04): the `residency` stamp** —
  `federation_handlers::residency_stamp`, always stated, on the three
  federated folder read replies and on the Welcome relay, threaded through
  the member nest's relays to the member's custody record; pinned in
  `conformance_federation_channel.rs` and, across two real nests, in
  `conformance_cross_nest_conversations_client.rs`. **Built (2026-10-04):
  the two serving kinds** — `fauna.federation.folder.serve.announce`
  (`federation_handlers::folder_serve_announce_handler`: the write gate,
  then a lease in `chunk_relay::ForeignSeats`, asked back at the roster
  row's `nest_url`, the reply stating the lease length; *no longer serving*
  drops only the calling nest's seat) and
  `fauna.federation.folder.chunk.wanted`
  (`federation_handlers::folder_chunk_wanted_handler`: push only on the
  connection that announced that device and folder with the caller as its
  home, reply `pushed`), originated by the member's nest from
  `fauna.sync.serve.announce`'s additive `foreign` list and by the home
  nest's relay walk to the leasing `nest_id` alone; pinned in
  `bins/fauna-nest/tests/conformance_relay_serving_cross_nest.rs`.
  **Built (2026-10-09): the seat** — the per-user agent's engine of a
  folder homed on another nest is announced in that `foreign` list by its
  home nest and answers on its own byte plane, and the interim refusal of
  the metadata-only ⊕ cross-nest-member pair is gone; the witness across two
  real nests and the real agent is `cross_nest_agent_capstone.rs`.
  Mechanism and pins: `../behavior/relay-serving.md` § Relay serving, its
  status paragraph.
- **BUILT 2026-10-05 — the cross-nest owner label**
  (§ Cross-nest shared folders + channel append → *The cross-nest owner
  label*): the home nest stamps `owner_handle`/`owner_domain`
  (`federation_handlers::owner_label_stamp`) on the folder Welcome relay
  (`welcome_deliver_core`) and on the content-key fetch reply; the receiving
  nest awaits the domain binding inline (`verified_owner_label`) and the
  relaying nest forwards on a warm cache only (`folder_handlers::
  relayed_owner_label`, over `FederationChannelPool::cached_domain_nest_id`),
  both through the announce's one binding (`well_formed_asserted_name`,
  `start_domain_verification`, `bind_domain_to_origin` — one cap); the pair
  lands on `WelcomeInbox`/`WelcomePayload` as `shared_by_handle` +
  `shared_by_domain`, on `ContentKeyGetReply`, and on `ForeignFolder` as the
  fifth latest-wins advisory pair (accept: `join_folder_welcome`; refresh:
  `custody::refresh_foreign_set_from_reply`); `fauna_core::format::
  qualified_handle` joins it for `FolderPendingShare.shared_by_display` and
  `on_demand_presence::held_sets`. Pinned at tier 1 (the formatter, the merge,
  the custody refresh, the pending-share label, `held_sets`, the relay's
  warm/cold/mismatch cases in `folder_handlers`' tests) and against two real
  nests in `conformance_cross_nest_conversations_client.rs` (an honest pair
  arrives, a domain resolving to another nest's key and a non-folder relay
  carry none; a production share records the pair and the relayed read
  forwards it).
- **BUILT 2026-09-29 — the cross-nest writer roster read**
  (§ Cross-nest shared folders + channel append → *The cross-nest writer roster
  read*): `fauna.federation.folder.actors.fetch`
  (`federation_handlers::folder_actors_fetch_handler`, the same
  `folder_handlers::actor_roster_for_channel` projection the same-nest
  `members.list_actors` serves, handles blanked, `caller_access` stamped;
  `conformance_federation_channel.rs` pins the gate, the ids-only disclosure and
  the S7 refusal) and its distinct client kind
  `fauna.folders.members.list_actors_remote` (reply `ActorMembersListReply`,
  which gained the additive `caller_access`); the sync engine's reader leg
  (`SyncEngine::refresh_reader_roster`) relays a foreign-routed set's read there,
  and a cross-nest member's signed row is held until the first read, as
  same-nest. **The succession-statement
  carriage on the owner and `writer` rows is BUILT 2026-10-01** — the
  projection fills it, so both doors serve it
  (`conformance_federation_channel.rs::folder_actors_fetch_carries_the_same_succession_statements_as_the_same_nest_read`);
  what the carriage does not do is `mls-group-key-material.md`
  § Implementation status today's to say.
- **Built 2026-09-28 — the folder-owner marker's succession arm (ratified
  2026-09-22, § Cross-nest shared folders + channel append → *The marker
  follows the owner's verified succession*)**, landed with the owner-attested declassification's reader
  half, which is the first reader of the marker. **Member side:**
  `poll_inbound_folder` routes a `GroupMetaMessage::Succession` body through
  `route_folder_succession` — the session-wired `SuccessionWitness`, then
  `settle_folder_owner_succession`, the folder twin of the conversations
  rail's roster-pair door: verified, `old_actor_id == folder_channel_owner`,
  successor seated (`AwaitingRemoveOld` or `Complete`) re-stamps at once;
  `NotHere` drops; a witness-refused statement parks **by channel**
  (`parked_folder_successions`, admitted only where its `old_actor_id` is the
  recorded owner) and re-drives on the channel's next folded commit and on
  the harvest settle (`redrive_after_harvest`). **Owner side:** the sweep
  stamps its own join (`stamp_successor_folder_owner`, fresh and resumed
  paths); `MlsEngine::restamp_folder_owner_markers` is the registry-anchored
  primitive (no launch caller — the ruling, above). **Nest:** the claim
  re-point (`folder_channel_claims.claimed_by`, `Succession::Move`) has been
  built since 2026-08-03
  ([`../behavior/succession-propagation.md`](../behavior/succession-propagation.md)
  § Implementation status today); this ledger wrongly called it unbuilt until
  2026-09-28. Pins: `fauna-conversations`
  `the_folder_rail_re_stamps_the_owner_marker_at_the_midpoint_and_admits_remove_old`,
  `without_the_statement_the_successors_remove_old_is_refused_on_a_member_seat`,
  `a_statement_carried_ahead_of_the_add_re_stamps_nothing`,
  `a_witness_refused_statement_parks_by_channel_and_re_stamps_on_the_harvest_re_drive`;
  `fauna-client-recovery`
  `a_sweep_stamps_the_successor_as_folder_owner_where_the_predecessor_owned_the_channel`,
  `a_resumed_sweep_stamps_the_marker_the_interrupted_run_never_reached`;
  `fauna-mls` `restamping_moves_only_markers_that_name_an_attested_predecessor`;
  `fauna-sync-engine`
  `binding_member_seat_follows_the_folder_owner_marker_through_a_succession`
  (the anchor moving with the marker). **Built 2026-09-28 — the hold behind
  a parked statement:** the refused-then-memoized
  fork the arm declared is closed inside the ruled bound (*The folder commit
  walk inherits the harvest wait*, above): `poll_inbound_folder` re-asks the
  witness ahead of every commit while a statement naming the recorded owner
  is parked (`redrive_parked_folder_in_channel`, now run before the commit as
  well as after a fold) and returns `stalled: true` ahead of the record while
  the sweep has yet to speak for that owner (`FaunaMlsBackend::
  folder_walk_waits_on` — `harvest_armed`, set by
  `arm_succession_harvest_wait` natively and `note_harvest_sweep_armed` on
  web, and `harvest_spoken_for`, written by both harvest re-drive arms);
  `SuccessionStatementCounts::held_commits` counts the holds. The owner joins
  the sweep through `RailBackend::harvest_anchor_wants` →
  `ConversationsManager::harvest_walk_actors`. Pins: the four tests the
  ruling names. Residuals stand as the ruling states them. **The at-rest
  half (built 2026-09-28):** `FaunaMlsBackend::
  park_folder_succession` mirrors each park to `MlsEngine::
  park_folder_succession` (`PARKED_FOLDER_SUCCESSION_PREFIX`);
  `poll_inbound_folder` drains it once per channel per session
  (`load_rested_folder_park`, the `folder_park_loaded` memo);
  `redrive_after_harvest`'s folder arm drains every rested park, then forgets
  those naming the owner (`forget_rested_folder_parks_naming`);
  `redrive_parked_folder_in_channel` forgets the copy of a statement it drops;
  `MlsEngine::forget_group` drops the key. Pins:
  `fauna_mls_backend_tests.rs::{the_hold_behind_a_parked_folder_statement_survives_a_relaunch,
  a_forged_statements_hold_does_not_recur_on_the_launch_after_the_settle}`
  and `engine.rs::the_folder_park_rests_per_channel_and_goes_with_the_group`.
  **The slot is a bounded set that displaces nothing (ruled 2026-10-12):**
  the rule, its one exception and its residual are `identity-succession.md`'s
  (§ The succession statement (wire) → *The slot's replacement policy*); this
  rail's share is that the at-rest mirror rests the **whole set** of the
  channel's slot, and a rested value that fails to decode as one is forgotten
  (a hold lost once, inside the 0.1.x compat-free window). Pinned by
  `fauna_mls_backend_tests.rs::the_rested_folder_park_holds_the_whole_set_across_a_relaunch`.
- **Cross-nest shared folders + channel append — NEST PLANE BUILT
  (2026-07-19, Phase 2 first slice; § Cross-nest shared folders + channel
  append above is no longer fully "unbuilt").** The four kinds serve with the
  one structural gate (`federation_handlers::require_foreign_member`):
  `channel.append` (reuses `channel_send_core` verbatim — the S2 commit gate
  covers the federated plane by construction), `channel.leave` (the first
  `channel_foreign_members` delete; wrong-home-nest refusal, idempotent
  absent-row success), `folder.{changes.fetch, content_key.fetch}` (S7
  claim-resolve via `claimed_folder_for_channel`; same `SyncChange` wire rows
  as same-nest `changes.list`; frame-budgeted). Client-kind relays live:
  `fauna.conversations.channel.send_remote` (distinct kind, routed client-side
  by `FaunaMlsBackend::send_on_channel` over the recorded `channel_home_url` —
  **every** producer, gated commits included: the commit gate reaches that door
  through `fauna_client_mls_sync::BackendChannelSend` and holds no conversations
  rpc of its own, so a gated commit cannot address a nest the channel does not
  live on. Until 2026-08-29 it did: `send_commit_gated` called `channel_send`
  unconditionally, so a cross-nest member's device-owned-epoch takeover — and
  every gated add/remove — landed on the *member's own* nest while the
  advertisement it was taken for routed to the home nest. Co-members read the
  home log, so they never saw the takeover and could not decrypt what followed
  it; the roster-membership Commit admission ratified above, and the commit rate
  cap that throttles it, had no client producer at all, since the local nest
  reports the channel `Unclaimed`). **The recorded
  home is durable as of 2026-08-30, and the sentence above depends on it being
  so.** `FaunaMlsBackend::channel_home` is a RAM map written only by the three
  Welcome-**join** paths, so until then it was empty after every relaunch, and
  an established foreign-homed channel read as same-nest at all *five*
  consumers of `channel_home_url` — the send door and, identically, the three
  inbound drains and `channel_actors`, so the member stopped *receiving* the
  channel as well as failing to send on it. The routing datum now rides the
  channel's own at-rest record (`ChannelHistorySlice.home_nest_url`, additive
  and `serde(default)`), stamped on persist and re-seeded into the map by the
  launch restore beside its existing `bind_channel` — one write and one read,
  because all five consumers resolve through the same map. **Stated, not closed:** a slice written before that
  field existed decodes as `None`, which is indistinguishable from a genuine
  same-nest channel, so a cross-nest channel joined by a pre-2026-08-30 binary
  keeps the old behaviour. Refusing every `None` is not the safe reading of that
  ambiguity — it would refuse the same-nest common case, which is the
  overwhelming majority of channels — so the fail direction is bought by the
  datum being present, not by the send door second-guessing it. **What
  re-delivery does and does not buy** (corrected 2026-08-30; the sentence here
  previously named a re-delivered Welcome as the recovery, which no path
  performed): each of the three
  Welcome paths now records the home **above** its idempotency guard, so a
  Welcome that *is* re-delivered re-teaches the home at no init-key cost —
  before, all three learned it only below the guard, and none of them ever
  reached that line on a re-delivery (`ingest_welcome` returns at
  `thread_for_channel`, which the launch restore repopulates; scheduling returns
  at `is_scheduling_channel`, which ORs in a durable engine marker; the folder
  path's RAM-only marker misses instead, and the re-join then fails on the spent
  init key). That makes the mechanism real, **not** the population whole: an
  inbox Welcome is acked once drained, so a channel established in an earlier
  session is normally never offered one again. The pre-field cross-nest
  population is therefore recovered only where its Welcome is still undrained (a
  crash between ingest and ack); otherwise it stays mis-routed, and closing it
  needs a datum that is durable independently of the slice. **The FOLDER half of
  that population is closed as of 2026-08-30**: a member's cross-nest folder
  join writes a `ForeignFolder` row into their own folder-key custody (in
  `__config` until the rail retired 2026-10-02; since then a `foreign/` row of
  the `fauna.state.folder-keys` kind — [`config-dissolution.md`](config-dissolution.md)
  § The `__config` dissolution schedule → *The kinds*), carrying the
  set's `channel_id` and `home_nest_url`, and that row outlives both the slice
  and the drained Welcome. The launch restore now spends it — one custody
  read for the whole foreign population (`FolderCustodySink::foreign_homes` →
  `FaunaMlsBackend::seed_channel_homes_from_custody`), run **after** the slice
  loop and writing only into holes, so a channel's own at-rest home — the more
  specific of the two data — still wins, and no init key is spent and no
  Welcome re-delivered. Per channel this would have been one fetch each over a
  list whose same-nest majority holds no record at all, which is why the seam is
  a population read and not a loop over the single-channel
  `foreign_home_url`. The record rests in the
  folder-key custody, which is readable only once the seat's account runtime
  has assembled, and the restore can finish first — so the launch folder pass
  runs the seed again once custody is readable (in line, or detached until it
  is), where a restore-only seed used to fill nothing on a launch the runtime
  lost; the rule is
  [`mls-group-key-material.md`](mls-group-key-material.md)'s (§ M2 →
  *Rotate-on-removal*, the launch-time resume). **The conversations half is
  CLOSED BY DECISION (2026-08-30): the population does not justify the
  machinery**. Folder-key custody holds a
  foreign-set row for a shared *set* only, never for a conversations channel,
  so a pre-field cross-nest conversations channel has no durable datum outside
  its slice and stays mis-routed — and **all three routes to resolving it were
  examined on 2026-08-30 and are closed**: the two bullets at the end of this
  entry, then the decision paragraph that follows them.
  Both writers of `history/<ch>` stamp the home: the
  Rule-3 flush and the debounced replica autosave. The autosave did not, and it
  is the only writer a Welcome recipient who has merely *received* ever runs, so
  until 2026-08-30 that population was written away as home-less on current
  binaries too — the residual was never purely historical. The two closed
  routes for the conversations remainder: **(a) a total encoding does
  not buy the fail direction here** — making the map total, so that a *missing*
  entry means "pre-field" rather than "same-nest", works for slices written
  from now on and for those alone; every legacy slice lacks the marker and the
  overwhelming majority of legacy channels are same-nest, so refusing on "no
  recorded home" refuses precisely the same-nest common case this entry already
  ruled out. The rule stands as written — the fail direction is bought by the
  datum being present, and for this population it is absent by construction;
  the encoding would still *name* the ambiguous set, but that set stopped
  growing when the autosave began stamping the home, so nothing is lost while
  it stays unnamed. **(b) Resolving the home by asking this nest is
  contaminated by auto-register** — the natural probe, "does my own nest home
  this channel?", would be decisive if the answer were untouched, but
  `channel.send`'s core auto-registers the poster on an **unclaimed** channel's
  `actor_channels` roster (every DM / group / conversations channel is
  unclaimed), and that is the very call each mis-routed send has been making
  since the join, so this nest holds a roster row for a channel it does not
  home and the probe answers "yes, mine" for exactly the population it was
  meant to detect. **(c) The remaining route — the MLS roster — was examined
  by the design slice and deliberately NOT built (decided 2026-08-30)**. It is the one route whose datum neither the
  client's own state nor its own nest's roster can have forged, and it is
  real in sketch: the local engine's leaf-authenticated roster
  (`MlsEngine::group_members`) names the members' actor ids; each candidate
  member's nest can then be probed through the structural fetch gate, whose
  answer is decisive — the true home serves (it wrote the
  `channel_foreign_members` binding at Welcome-relay time, and every
  cross-nest channel postdates that plane), a non-home refuses loud, and a
  hostile co-member's nest answering falsely sits inside the same envelope as
  the TOFU inviter premise (a co-member already holds plaintext; the capture
  is read-DoS + metadata, remedied by a fresh channel). What sinks it is the
  step between: **an actor id resolves to a home nest NOWHERE on the client.**
  Discovery is handle-keyed end to end (`resolve_handle_domain` →
  `fauna.actor.by_handle`, the recipient-picker path) and the MLS leaf
  credential carries exactly the 32-byte `ActorId` — no handle, no nest — so
  the route requires new actor-id-keyed resolution machinery (a new directory
  read or federation kind) plus the probe loop plus tri-state send-door
  semantics: permanent wire and client complexity serving a population that
  is **frozen** (stopped growing 2026-08-30 — every current join and autosave
  stamps the home at birth, so the ambiguous set cannot regrow), **bounded**
  (a member enters it only via a cross-nest conversations channel, which
  needs two federated production nests with real users during the closed
  alpha), and **self-remediable in-product** (a fresh conversation with the
  same peer routes correctly under current code; the mis-routed channel's
  local history stays readable). So the send door keeps datum-present
  routing: the pre-field conversations population keeps the old local-arm
  behavior, and the folder at-rest rider stays on its accepted residual
  above. **Re-opening this requires evidence of an actual affected
  population, and starts from route (c)'s recorded sketch — not from a
  fourth hypothesis, and never from (a) or (b).**
  **The pre-guard write itself was a plant vector, and is CLOSED as of
  2026-08-30**: recording the
  home *above* the idempotency guard (the re-delivery recovery)
  means it runs **before any MLS authentication** of the re-delivery, on a value the peer
  declared — and `welcome.deliver` is open-federation with a Dm/Group Welcome
  ungated on the client, so an absent entry was fillable by any peer. A same-nest
  channel was merely *absent*, so every ordinary local DM and group was such a
  hole. Two constraints close it, on two planes. **Nest-side:** the relayed
  Welcome's home `nest_url` is now bound to the connection's handshake-**verified**
  `origin_nest_id`, not the peer-declared `origin_nest_url` — the nest forwards a
  **dial-proven** `nest_addresses` address of the verified origin, honouring the
  declaration only when it is itself one of the origin's proven addresses (a
  migration between two proven addresses — its own current self-claim wins) or
  when the origin has no proven address yet (first contact, where the declaration
  is also recorded as an *unproven* sighting for a later handshake to promote),
  the same verified-origin discipline the folder-relay set name already uses. So
  a plant is bounded to the sender's **own** verified nest, never an arbitrary
  URL. **Client-side:** `FaunaMlsBackend::channel_home` is now a **total
  encoding** — `Foreign` / `SameNest` / absent — and a channel created or joined
  under current code carries an explicit `SameNest` marker, persisted via the
  additive `ChannelHistorySlice.home_same_nest` and re-established on restore; the
  pre-guard fills only an **absent** entry, so it can never re-home a
  known-same-nest channel. This closes the plant entirely for the current-binary
  population. **This is the total encoding used for the FAIL direction, not the
  recovery direction route (a) ruled out** — the marker never *recovers* a legacy
  channel's home; it only stops the pre-guard from inventing one. A channel
  persisted by a **pre-marker** binary restores as absent and stays fillable —
  the declared, bounded residual, now capped by the nest-side constraint to the
  sender's own verified nest. **That cap is a cap on a URL, and a peer may
  decline to name one** — omitting or blanking `origin_nest_url` on the
  federation door takes its no-declared-origin arm, which the client blanks and
  hands to the pre-guard. The pre-guard briefly wrote `SameNest` for a blank,
  which — the marker being deliberately immovable — let one unauthenticated
  Welcome pin a foreign-homed channel permanently, closing both recovery routes
  (the genuine re-delivery finds no hole; the durable folder seed fills `Vacant`
  only). **A blank now writes nothing** (2026-08-31), restoring the general property: *no unauthenticated
  write may produce a routing state a later authenticated datum cannot correct.*
  **A channel pinned during the blank-writes-`SameNest` window stays pinned
  today.** That arm existed only between 2026-08-30 15:37 UTC+2
  and 2026-08-31 01:25 UTC+2 — about ten hours, in closed alpha
   — and the marker's deliberate
  immovability means neither recovery route reaches a channel pinned there: no
  migration is planned, since the population is bounded by that ten-hour
  window and the loss is recoverability of a routing datum rather than
  confidentiality (`SameNest` and absent both route sends locally), which
  [`nest/common.md`](nest/common.md) § Client-state recoverability →
  *Per-object remedies* accepts as the standing frame for exactly this shape
  of unrepairable per-object state.
  The stated cost is that a pre-marker **local** channel is no longer upgraded
  absent → `SameNest` by a genuine same-nest re-delivery; it stays in this same
  declared residual, which the URL cap above does bound.
  **The same value also rests in the member's own `ForeignFolder` records, and
  a record written before the nest-side constraint landed rests UNCONSTRAINED —
  a declared, accepted residual, decided rather than gated (2026-08-30).** The launch seed
  above replays those records into the routing map as written: a seed is
  client-side restoration with no handshake to verify against, and no marker
  tells the two populations apart at rest. Accepted on three grounds. **(1)
  The lineage is the inviter-asserted TOFU premise this doc already accepts
  for v1** (§ Cross-nest shared folders + channel append, the foreign-member
  binding bullet): the record exists only where this user **explicitly
  accepted** the share — every pushed folder path fails closed to Knock, a
  cross-nest relay always stamps `shared_by: None` — and its URL is what the
  accepted sender's relay declared at share time. A hostile inviter misrouting
  this member's reads gains ciphertext/metadata plus a read-DoS of this
  member, inside the premise's envelope, and the premise's remedy heals it:
  leave + re-accept, or a re-share, either of which mints a fresh record
  carrying the now-verified value. **(2) A written-under-new-code marker on
  the record would be structurally empty**: a channel joined under current
  code rests with its slice-carried home, so the seed's load-bearing
  population is exactly the pre-fix records (plus the crash window before a
  current channel's first autosave, whose records already carry the
  constrained value) — a marker the seed refuses on deletes the recovery the
  seed exists to perform, and one it admits enforces nothing. **(3) The
  population stopped growing when the nest-side constraint landed** and is
  bounded by the closed-alpha federated population. The launch-seam pin
  (`a_pre_field_folder_channels_home_is_seeded_from_its_foreign_set_record`)
  witnesses the decided semantics — the record routes as written. The
  roster-derived home resolution that could one day re-derive these records
  from an unforgeable datum was examined and deliberately not built — route
  (c) in the conversations-half decision above; if that decision is ever
  re-opened, the folder legacy population rides the same design (a folder
  channel's roster leaf-authenticates its owner where `welcome_sender`
  survives, subject to the same legacy-seat caveat as residual (b-ii) in
  § Cross-nest shared folders + channel append).
  and additive `nest_url` (+ channel addressing) on
  `changes.list`/`content_key.get`/`folders.leave`; S5 old-peer mapping is
  the shared typed `fauna.federation.peer_nest_outdated` (NeedsUpdate class).
  Owner-side: `members.list_actors` unions foreign rows (`remote: true`);
  `members.evict` purges the foreign row (S8). S6: the two byte GET routes
  carry route-scoped `Any`-origin no-credentials CORS outside the blanket
  credentialed layer, admitting the `Authorization` request header a
  folder-hinted read's bearer rides (`relay-serving.md` § Relay serving, step
  (4) — a script-held bearer is no ambient authority). **Client read-side BUILT (2026-07-19, Phase 2 second
  slice):** the Welcome relay wire carries an additive `set_name`
  (home-nest-resolved from the claimed row, never sender-asserted —
  `WelcomeInbox`/`WelcomePayload`/`FedWelcomeDeliverRequest`); the recipient's
  accept writes a durable `ForeignFolder` record (`FoldersConfig.foreign_sets`,
  union-by-channel CRDT merge) via the extended `FolderCustodySink`; the
  custody-ingest and Media key-resolver route foreign sets through the
  `nest_url`+`channel_id` relays; the byte plane fetches direct from the home
  nest per resolved `home_nest_url` (`ForeignBlobFetcherFactory` — native
  plain-WebPKI GET, wasm fetch under the S6 CORS); the member-visible list
  unions foreign records (`DevicesMachine::ForeignSetsSource` under the same
  MLS join-filter); and leave relays via `FoldersClient::leave_with_home`
  (home URL durably resolved from the record after relaunch). Two-nest
  client-level conformance:
  `conformance_cross_nest_conversations_client.rs::cross_nest_shared_folder_reads_evict_and_leave_through_client_stack`
  (+ the send_remote round-trip in `bob_receives_alices_cross_nest_message…`).
  **Write plane BUILT (2026-07-20, Phase 3):** the two federation write
  kinds serve behind `require_foreign_writer` (the structural
  foreign-member gate **then** owner-granted `access == 'writer'`):
  `folder.changes.record` runs the same owner-pays metering + per-member
  cap + version floor as a same-nest record via the shared
  `sync_handlers::record_change_core`, nest-stamping `author_actor_id =
  requester` and **content-idempotent by construction** (a re-relayed
  record returns the original seq and charges nothing — the durable
  exactly-once guarantee in `record_sync_change_metered`, keyed on
  `(actor, path_hash, manifest_hash, change_type, content_key_version)`
  vs the path's head; the same-nest path shares it); `folder.write_token.mint`
  mints a short-lived `Write`-only bulk token (TTL the Rust constant
  `FOREIGN_WRITE_TOKEN_TTL_SECS = 600`, purpose `ForeignFolderWrite`,
  bridge-mint-refused) for direct chunk/manifest POSTs. Client kinds:
  additive `nest_url`+`channel_id` on `fauna.sync.changes.record` (relay
  branch fails loud on an old own nest — local `not_found`) and the
  net-new per-actor `fauna.folders.write_token.get` (unknown-kind-loud
  on an old own nest, S5-mapped to `peer_nest_outdated`). Engine:
  `SyncEngine::set_foreign_routing` threads the control-plane relay,
  and `fauna_client::write_token_bearer::WriteTokenBearer` is the
  byte-plane bearer (lazy mint + proactive refresh; lifted from
  `fauna-sync-engine` 2026-09-09 when the conversation rail's
  `fauna.federation.conversation.write_token.mint` +
  `fauna.conversations.blob.write_token.get` became its third consumer —
  `../behavior/conversation-rooms.md` § The home nest → *Attachment
  bytes*, proven by
  `cross_nest_attachment_bytes_rest_on_the_home_nest_and_survive_gc_through_client_stack`). Two-nest client conformance:
  `cross_nest_writer_records_and_uploads_owner_reads_back_through_client_stack`
  (Success a+e); gate/idempotence/cap pinned at the federation-channel +
  DB levels (`folder_write_plane_gates_on_writer_and_is_content_idempotent`,
  `writer_record_charges_owner_and_enforces_member_cap`). **This serving
  plane is real and reachable by any correctly-driven caller** — the caveat
  was upstream of it, and closed on 2026-07-22: the discovery deltas
  (§ Cross-nest → *Recipient-side access discovery*) are **BUILT**, so a
  recipient learns its own grant, and the client leg that drives this plane
  (foreign engine routing + the eager bind-time mint verify) is built for
  **linux**.
  **PROVEN end-to-end 2026-07-22** by the task-6 capstone
  (`bins/fauna-sync-agent/tests/cross_nest_agent_capstone.rs`: two real nests +
  the real sync-agent process — share, bind, bytes both ways, demotion parks);
  the client-side revocation surface — a typed mint/record refusal parks the set
  fail-closed and loudly — landed 2026-07-22 (task 4). Owner for client-leg status:
  `../behavior/file-sync.md` § Multi-writer shared sets →
  *Implementation status today*.
  **v1 scope (decision of record 2026-07-20):** the write plane is exactly
  these two federation kinds — the kind inventory in [`federation.md`](federation.md) § Federation residue surface lists no
  `lease`/`conflicts` federation twin, and the read-write/cross-nest
  design makes cross-nest conflict-report relay a **named v1 gap** (the
  losing version is still structurally retained; nothing is lost) and does
  not route the advisory upload lease cross-nest (concurrent cross-nest
  writes auto-resolve as conflicts). A cross-nest writer's holder-scoped
  same-nest lease-release fix landed alongside.
