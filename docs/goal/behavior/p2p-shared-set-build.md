# Cross-user shared-set transfer — the build (target state + build ledger)

Owns: p2p-shared-set-build, phone-peer
Status: ratified — split verbatim out of [`p2p.md`](p2p.md) on 2026-09-27; the share leg's serve/pull core, byte plane, pump, discovery carriage, row half and shared driver are built, with the tui and linux legs landed; the phone-peer shared arm is built (2026-10-01), the android and iOS legs are unbuilt
Authority: **building the cross-user shared-set transfer plane** — the share leg's build contract (its first design pass), the row-half build design (B2), the build ledger (every *Built — …* record of what a build measured, including the per-app status paragraphs), and the **phone-peer design** (android and iOS as foreground partial peers: the replica access, the body source, the wanted-body landing policy, the lifecycle, the iOS door). **NOT owned here** — the plane's contract (the scenario and its contract points), § Offline share initiation, § Peer-served change-row provenance, the wormability walk and posture, the transport seam and everything else about P2P → [`p2p.md`](p2p.md); the replicas themselves and the keys a capability host holds → [`on-demand-files.md`](on-demand-files.md). On conflict with the contract, the contract in [`p2p.md`](p2p.md) wins.

---

Split verbatim out of [`p2p.md`](p2p.md) § Cross-user shared-set transfer on 2026-09-27, when that doc reached 240,363 B — two days from the 262,144 B whole-file read ceiling, with five build rows about to append their *Built* records to the same section. Nothing was reworded. **Pointers in the text below keep their original meaning:** a bare `§` pointer, or an "above"/"below", naming a section this doc does not hold — "the contract above", "the provenance ruling above", § Implementation status today, § Inbound authorization, § Offline share initiation, § *Wormability walk* — resolves in [`p2p.md`](p2p.md); one naming a section of this doc resolves here.

## Cross-user shared-set transfer — the build

### Build contract — the share leg's first design pass (2026-08-17; refutable at build time)

The share-twin build opened with this
pass; these refinements bind the slices unless a build refutes one, in which
case the refutation is recorded here exactly as the peer leg's were.

- **Kind family: `fauna.peer.share.*`, a disjoint sibling of
  `fauna.peer.sync.*`** — never an extension of it. The separability the
  same-account walk landed (distinct kind families, rule 5) is what makes the
  store-safe absence witness provable; `fauna_protocol::peer_sync`'s own
  module doc reserves the split. The family is compile-gated behind the
  `p2p-share` cargo feature on the excision spine
  ([`../architecture/dynamic-features.md`](../architecture/dynamic-features.md)
  § Compile-time excision: default-on at flavor roots, forwarded features,
  `store-safe` complement; `fauna-protocol`'s existing `p2p` feature is the
  absent-⇒-decode-`Unknown` precedent).
- **The M2 witness is a CLAIM, not a certificate.** `WITNESS_M2_MEMBERSHIP`
  (the string `fauna_protocol::peer_sync` reserves) carries the claimed set
  id(s) (`ChannelId`) and nothing else; verification is the serving side's
  **local group-state consult** — the proven actor key (PT-1b: the channel's
  NodeId *is* the actor key) checked against the set's MLS roster
  (`FolderGroupCrypto::contains_member` is the landed predicate, actor-key
  leaf credentials). This satisfies the admission seam's "admits a proven
  key, never a bearer" *directly* — the proven key is the roster entry — and
  its "never a registry lookup" rule: the roster is the evaluator's own
  store, offline-available, not a third party. The verdict is
  `AdmittedScopes::Named` over the admitted sets' scopes (that variant's doc
  already anticipates this producer); each side admits independently, exactly
  as the seam rules.
- **A shared set gains a scope string: the reserved `folder` family is ruled
  by this build** (`fauna_protocol::scope` parks it today). Shape:
  `folder:<64-hex ChannelId>` — the set's derived channel id is already the
  group/custody/wire key everywhere else. Additive; the frozen `content:`
  grammar is untouched.
- **The listener is the contact-plane node, in the app process.** Share
  kinds register on the existing `PeerNode`/`PeerHandlers` (Y.1) actor-key
  node — not the same-account peer-sync listener (device-key NodeId, disjoint
  kind family, its own gates). The app process is where the MLS engine,
  content-key custody, and sync state live, so serving requires the app up;
  the nest remains the always-on source. Consequence, accepted for v1: an
  actor-key NodeId conflates one user's devices, so the leg holds **one live
  counterpart connection per remote actor** — multi-source concurrency across
  *one* user's several devices is out of v1 (multi-source across {counterpart
  device, own fleet, nest} stands).
- **Admission reconciliation (resolves a latent divergence, this section
  wins per its own header).** § Inbound authorization (2026-08-02) named the
  `PeerDb` `p2p_enabled` contact rows as the admission set for future
  `fauna.peer.*` kinds; this contract (2026-08-10) rules **same-group
  membership** as the share leg's admission. They compose as: the share
  kinds' admission set IS the set's M2 roster — membership is only reachable
  through the owner's quota-gated share (§ below, rule 8) and the
  recipient's contact-gated Welcome accept (`NestFolderGate` over
  `fauna.contacts.status`), so every edge was chosen by both endpoints; the
  WG-era `PeerDb` rows stay non-authoritative (their dormant status
  unchanged). The trigger § Inbound authorization arms **fires at this leg's
  first content kind**, discharged by the M2 admission plus working
  severance affordances (member evict + leave + contact removal) — the
  build verifies the lead app renders evict/leave before the leg serves.
- **Content keys never ride the share plane.** Key distribution stays the
  M2 group's existing rail (`fauna.folders.content_key.{put,get}` + the
  group-sealed envelope); the share plane moves sealed bytes and set
  metadata only.
- **Discovery carriage is its own slice.** Candidates travel only over
  authenticated channels; the default direction is the T5 symmetry — the
  set's own conversation channel carries members' endpoint advertisements,
  cached for offline dial (LAN arithmetic over cached candidates; relay
  rendezvous when advertised). No DHT, no broadcast, no scanning — restated
  from the contract, binding here too.

### Built — the serve/pull core (2026-08-17, row 59 slice B): what the build measured

The contract above bound the slice and **nothing in it was refuted**. Five
refinements the build discovered are recorded here, in the contract's own
"refutable at build time" spirit — each is a question the design pass could not
have answered without code:

- **An own-pending row needs a LOCAL-ORIGIN signal, not just its contents.** The
  provenance ruling admits "own-pending and own-sequenced alike", and a sequenced
  row is checkable (its nest-stamped `author_actor_id` must name the serving
  side). An **un-sequenced** row has no stamp at all — so on the wire it is
  indistinguishable from a *provisional row this replica ingested from a third
  peer*. Serving one would defeat the no-relay rule through a row that looks
  perfectly legitimate. The serve filter therefore reads a second fact the store
  answers from local origin (its own outbox / writer id), never from the row:
  `fauna_peer_share::provenance::LocalShareChange::locally_authored`. Both signals
  must agree wherever both exist, and a stamp naming someone else outranks the
  local claim.
- **A SEQUENCED row with no author stamp is refused on both sides.** The ruling
  did not name this shape; it exists (pre-multi-writer rows predate the nest's
  stamping). Such a row's recorder cannot be proven to be the serving peer, so it
  is neither served nor ingested — unattributable is not own.
- **⚠ The ROW half has no local source yet — a client replica does not retain change
  rows at all.** Measured 2026-08-17, after the serve core landed: the file-sync
  plane's client store keeps **per-path current state** (`sync_entries` in
  `fauna-account-store`'s `SyncDb`) plus a cursor, and nothing else. It applies
  nest-served `SyncChange` rows and discards them — `author_actor_id` appears
  nowhere in client production code, and there is no client-side change log.
  (`fauna-account-store`'s `journal` table is the **account** plane's, keyed
  `(scope, writer_id, writer_seq)` with `op`/`item_ref` — not file rows.) So
  `ShareStore::changes_since` is a seam no replica can honestly fill today, and
  the trap is specific: a naive impl would **synthesize** rows from
  `sync_entries` (path, manifest hash, size are all there) with no `seq`, no
  author stamp and a guessed `sequenced` — which silently defeats the whole
  provenance ruling, since the author check would have nothing to check. Do not
  do that. The row half needs real retention of at least this replica's
  **own-authored** rows, which is now part of the overlay slice below.
  **The byte half is unaffected and complete**: manifests and chunks are
  content-addressed and re-derivable from local plaintext, which is the heavy
  part of any transfer. *(CLOSED 2026-08-17: B2.1's funnel write-through and
  B2.5's own-pending mint are exactly that local source — § Build design
  below.)*
- **The ingest leg splits in two, and only the first half is built.** The wire
  marking + the refusal policy (`accept_peer_row`: fail-closed cached-writer
  consult, then the author check against the channel-proven actor) are code. The
  **engine-side provisional overlay** — the at-rest cached writer roster, the
  read-side latest-per-path overlay, materialization from provisional rows, and
  the reconnect confirm/supersede reconcile — is a `fauna-sync-engine` mechanism
  and remains unbuilt. Until it lands, an accepted row has no consumer: the
  refusal boundary is real, the overlay it guards is not yet there. *(CLOSED
  2026-08-17: B2.2–B2.4 landed the roster, overlay, materialization and
  reconcile — § Build design below.)*
- **Chunk pulls are RANGED, and that is not an optimisation — it is what makes
  the leg work at all.** A chunk body is 512 KiB–8 MiB (`fauna_core::chunker`:
  anything under the 8 MiB single-chunk threshold is ONE chunk), while a
  peer-channel frame caps at 1 MiB. So the whole-body-per-reply shape the
  same-account leg uses for its small CBOR blocks **cannot move an ordinary
  file** — every holiday video, photo and document is exactly the case that
  fails. `chunks.pull` therefore carries a want list of `(store_key, offset)` and
  each reply a bounded slice with the body's true `total_len`; the puller
  assembles contiguously (a gap, rewind, overlap or length change mid-transfer is
  refused, never patched; so is a declared `total_len` above
  `fauna_core::chunker::MAX_STORED_CHUNK_BODY`, a sealed maximum-size chunk, on
  the slice that declares it — no honest body is longer, and a puller that
  trusted the claim would buffer rounds of bytes a stall then discards before
  the completion check ever runs; the bytes a failed pull did receive are still
  charged to `p2p-share.transfer`) and hash-verifies **on completion**, which is the first
  point a slice-assembled body has an address to check. Manifests stay whole-body
  — a manifest is ~64 bytes per chunk, so the budget only bites past ~11k chunks
  (**≈ 87 GB in one file**), where the fetcher fails with that limit named rather
  than reporting the manifest absent; making manifest fetches ranged too is the
  named forward fix, owed only when a file that large needs to move. Consequence for rule 4: the
  per-body check moves from per-reply to per-completed-body, still strictly before
  any bytes reach the walk. Measured 2026-08-17 by
  `an_ordinary_multi_megabyte_file_transfers`, whose 300 KB predecessor passed —
  a sub-budget fixture is how a whole-body design survives its own test suite.
- **A sealed chunk's store key is its body's hash; an unsealed one's is not.** On
  the legacy plaintext path (`stored_hashes` absent) the store key is the
  *plaintext* hash while the stored body is compression-framed, so a fetcher
  cannot verify a body against the address it asked for. That path is legacy only
  — owner content has rested sealed unconditionally since 2026-07-13 and a
  cross-user shared set is M2-sealed by definition — so every chunk this leg will
  ever serve satisfies `store key == hash(stored body)`, which is what makes the
  transfer-boundary check above meaningful. A share-leg fixture must therefore be
  sealed; a plaintext one tests a corpus the leg never sees.
- **Peer-served bytes ride the EXISTING download walk.**
  `fauna_peer_share::PeerShareBlobFetcher` implements
  `fauna_core::file_download::BlobFetcher`, so `download_file_bytes_by_manifest`
  opens a peer-served file with no new verification or key-handling code — one
  walk, one integrity policy, and the M2 content key applied in exactly one
  place. Rule 4's "inert at the transfer layer" is enforced on both legs: the
  walk itself **re-hashes the manifest against the address it was fetched by**,
  for every fetcher, and the peer fetcher additionally **re-hashes every chunk
  body against the address it asked for before returning it**, so substituted
  bytes die at the transfer boundary with a precise error instead of surviving to
  a confusing end-of-download mismatch. The peer fetcher's own manifest check is
  now redundant with the walk's and is kept only for the earlier, peer-naming
  error.
- **No bind door exists yet, deliberately (rules 5 + 7).** The crate exposes only
  `ShareServer::handler_factory`, never a `start_share_node`. The plane must light
  behind the `p2p-share` nest capability token, which lands with slice D; adding
  a listener door first would be precisely the brake-less listener rule 7
  forbids. Consequence for rule 5's excision claim: because **no flavor root
  links the plane**, unreferenced kind strings are dead-code-eliminated, so the
  store-safe witness's third column stays **vacuous** until a root actually
  compiles the serve/pull in (slice A measured this; the column rides the first
  root-linking slice, and rider (c) stays open
  until then). **DISCHARGED 2026-08-17, same day:** tui's Cargo.toml made
  `p2p-share` a default feature (§ *Wormability walk* rule 5), so tui became
  that first root-linking artifact — `dynamic-features.md` § The app-shell
  column measured `fauna.peer.share.` 3/0 default vs store-safe, non-vacuous.

### Built — the discovery carriage (2026-08-17, row 59 slice F): what the build measured

The Build contract's discovery bullet named the T5 symmetry as its "default
direction" — *the set's own conversation channel carries members' endpoint
advertisements, cached for offline dial*. The build **confirmed that direction
against code and nothing in the bullet was refuted**; what follows is what
could only be answered with code in hand.

- **The set's own channel is already a live, authenticated carriage — no new
  rail was needed.** A shared file set is registered on the conversations
  roster by the same nest-side write that serves conversations
  (`register_actor_channel_gated`), so the set's derived `ChannelId` addresses
  a real MLS group whose application messages every member receives. The
  advertisement therefore rides as an opaque-bytes `ChannelMessageBody`
  variant — the `Custody` discipline verbatim (never a chat bubble,
  sink-routed, additive-legal within the major: an older decoder fails the
  record's dag-cbor decode and the shared poll loop skips it). Two properties
  come free and are why the contract picked this direction: only members can
  **read** an advertisement (it is group-sealed) and only a member can **make**
  one anybody believes (MLS authenticates the sending leaf).
- **The advertisement's identity is self-asserted, and binding it is the whole
  security property.** The payload's `member_actor` is attacker-controllable in
  exactly the way `ChannelMessage.sender` is before `MlsEngine::decrypt`
  overwrites it with the leaf credential. So an admitted member can advertise a
  **third** member's location over a set it genuinely belongs to, and the
  channel will authenticate the liar correctly — nothing below the binding can
  catch it. `fauna_peer_share::bind_share_advertisement` ties the claim to the
  MLS-authenticated sender *and* to the carrying channel before any dial row
  exists, and **refuses rather than repairs**: filing a hostile peer's
  candidates under an honest member's name is precisely the durable harm.
  The same cross-check runs again on the way *out* of the store
  (`share_dial_targets`), so neither a lying advertiser nor a tampered store
  can point a dial at a node its named member does not control.
- **The cache is the third member of the location-data family, not a new
  shape.** `fauna.state.share-endpoints` (LatestWins + FleetOnly +
  GenerationTip) sits beside `device-endpoints` and `custodian-endpoints` for
  their reasons exactly: fleet-only because the row holds a *counterparty's*
  location data and must never travel onward, tip-sealed because removing a
  device from this fleet must sever its future knowledge of where the account's
  counterparties can be reached. Reading it composes through the **shared**
  `dial_target_from`, so an advertised candidate passes exactly the hygiene an
  own sibling's row does (PT-4 `is_safe_candidate`, the shipped v4 LAN-probe
  arithmetic, first-safe-WAN, relay availability) — a hostile advertisement
  buys no reach a hostile plane row would not.
- **The at-rest kind is deliberately NOT cfg-gated behind `p2p-share`,**
  although the plane it serves is. A flavor-dependent at-rest kind table is the
  worse failure: a store-safe replica must carry forward a row it cannot read
  rather than drop it (no-data-loss). What rule 5's excision claim pins is the
  `fauna.peer.share.` **wire** family, which this at-rest kind does not widen —
  and because no flavor root links the plane, the store-safe third column stays
  vacuous exactly as slice B left it.
- **`node_id` means the member's ACTOR key here**, not a device principal — the
  share leg dials the contact plane (PT-1b) where the same-account leg dials
  device principals. The 32 bytes flow through the shared composition
  untouched, so the reuse is exact and only the meaning differs.
- **Superseding is per `(set, member)`, last-writer-wins — a consequence of the
  contract's own one-connection-per-remote-actor bullet.** Because an actor-key
  NodeId conflates one user's devices, a counterpart's second device
  advertising different LAN candidates supersedes the first. Harmless by
  construction: iroh dials the *actor* NodeId and every candidate is only a
  path hint, so the worst case is one stale hint with WAN and relay standing.
- **Still unbuilt here:** the publish *pump* (when a device advertises, and how
  often) and the sink implementation that performs the account-plane write —
  both belong with the app-side share pump (slice E), which is
  gated on the `p2p-share` capability brake. This slice ships the carriage, the
  binding, the cache and the reader; nothing advertises itself yet.

### Built — the serve-side byte half (2026-08-18, slice E leg 1): what the build measured

The serve/pull core's row-half finding left the byte half to "the app-side
node assembly"; this leg built it engine-side (it needs the seal keys, the
state DB and the local tree — all the engine's), leaving the node assembly a
pure composition. Four refinements, in the build-record spirit:

- **Manifests are RETAINED at the seal sites, not re-derived at serve time.**
  A manifest's canonical bytes are in hand at exactly the two upload seal
  sites (in-memory + streaming), are self-verifying (key == blake3(bytes)),
  and cost ~64 B/chunk — so `own_manifests` retains them best-effort and
  unconditionally, the row retention's posture (B2.1), and
  `SyncEngine::share_manifest_bytes` answers a manifest fetch as a lookup.
  Pre-retention heads take a fallback: re-derive in memory from the entry's
  recorded head under the RECORDED generation, verify, backfill the
  retention — bounded by a named cap (`SHARE_RESEAL_IN_MEMORY_MAX`, 256 MiB;
  past it the file is unservable offline until it re-records, stated not
  hidden).
- **Chunk bodies re-derive from plaintext RANGES through the ONE per-chunk
  seal pipeline.** `seal::seal_chunk_body` (frame → encrypt keyed by the
  plaintext hash → re-key by ciphertext hash) was factored out of
  `seal_blob` and now backs the batch seal, the streaming upload's per-chunk
  loop, and `SyncEngine::share_chunk_body` — three call sites, one
  implementation, because a serve-side fork that drifted would read as
  "peer cannot serve this chunk" rather than as an error (the seal module's
  own two-implementations hazard, third instance). Since 2026-09-03 that
  function IS `fauna_core::chunk_seal::seal_chunk_body` (re-exported by
  `fauna_sync_engine::seal`), the crate-private AEAD's only door — a fork
  turned out to cost confidentiality, not just serveability
  (`mls-group-key-material.md` § Per-chunk file-sync key). Serving never buffers a
  whole file: the chunk's offset is the manifest's `chunk_sizes` prefix sum,
  the range read is checked against the manifest's plaintext anchor before
  sealing, and the re-derived body must hash to the requested store key or
  the answer is `None`.
- **Chunk serving is manifest-anchored.** The serve memo indexes a
  manifest's chunk coordinates when the manifest itself is served (bounded
  FIFO); a store key outside it answers `None`. Honest because the shared
  download walk always fetches a file's manifest before its chunks — an
  evicted manifest re-indexes on its next fetch, and `None` is the ordinary
  multi-source degrade.
- **A cross-build seal difference degrades to refusal, never to wrong
  bytes.** The re-derived body reproduces the original only when this build's
  framing/compression matches the sealing build's; a mismatch fails the
  store-key check and answers `None` — stated so a future framing change
  knows its cost (peers stop serving pre-change chunks until content
  re-records).
- **A verified re-derivation is memoized, bounded by bytes not count**
  (`ShareServeMemo`, 16 MiB, FIFO — row 451). Safe unconditionally: the cache
  key is the store key, the ciphertext's own content hash, so a hit is never
  staler than a fresh re-derivation. This exists because a puller re-asks for
  every still-incomplete chunk on every pull round once a chunk's sealed size
  exceeds the peer-share reply budget (`MAX_BODY_BYTES_PER_REPLY`, 700 KiB) —
  without it, every one of those rounds re-paid the full read + reseal for
  every chunk not yet served that round, not just the one round could afford.
  `ShareStore::chunk_exists` is the other half: a cheap existence-only check
  (default: fall back to `chunk_body`) the server consults instead once its
  reply budget is already spent, so a want that will only be `deferred` never
  pays for a derivation whose bytes get discarded unused.

### Built — the pump, the boundary, and the discovery halves: what the build measured

The serve-side byte half's record above opened this leg; what follows landed
the same day and completes the shared-Rust half of slice E — every app
affordance now composes over `fauna_sync_engine::share_pump` +
`fauna_client_capabilities::group_ceremony_node` rather than building its
own. Findings, one per refinement the design passes could not have made:

- **The Build contract's colocation premise is REFINED for
  out-of-process-agent apps.** "The listener is the app process, because
  that is where the MLS engine, content-key custody and sync state live" is
  HALF-true on every app that drives the per-user sync agent, which since the
  A3 cutover is all of them: their file-sync state lives in that agent, not in
  the app. (This bullet named linux as the in-process counter-example until
  2026-08-21; linux retired its in-process driver with A3, and the later leg
  found the claim still being copied forward.) The refinement, ruled by
  building it: the listener stays the app's single actor-keyed node
  (`CeremonyNode::bind_with_share_plane` — the ceremony's bind door grown
  the real M2 consult + a hot-swappable per-set router, one bind ever);
  **serving stays app-side** as a cross-process WAL read of the agent's
  state (the sanctioned second-connection pattern) — so serving requires
  the app up, exactly the contract's consequence, and keeps working when
  the agent is down; **ingest stays with the engine** (single-writer
  invariant) behind the `ShareIngestDoor` seam — one additive IPC verb pair
  (`GetShareServeInfo` + `ShareIngest`, bodies via a verified spool) on
  out-of-process deployments — the adapter pair is shared, not per-app
  (`fauna_sync_engine::share_glue::agent_share_access`) — and
  `always_resident::ingest_share_page` called directly by an app that hosts
  the engine, which none does today. The pump cannot tell and must not.
- **The pull cursor lives beside the overlay, forward-only.** Per-(set,
  peer), advanced only by the ingest door to the page's highest sequenced
  seq (own-pending rows carry no coordinate and never advance it), read by
  the pump as its next `since`. A page that claims `more` without advancing
  the cursor stops the pump rather than spinning it.
- **The publish cadence is RULED** (the discovery-carriage record left it
  "genuinely undesigned"): change-triggered immediate + a 1-hour floor
  (`share_pump::ADVERTISE_FLOOR_SECS`). The floor is not politeness — an
  MLS application message reaches only the members present at send time, so
  a later joiner has no row for us until the next publish; the floor bounds
  that joiner's peer-discovery latency at one tiny group-sealed message per
  set per hour per device. Reactive re-advertise on a new member's first
  advertisement is the named forward refinement.
- **Rule 7's cached-brake half is BUILT and single-owner.** The share
  plane's bind reads the peer leg's own `META_NEST_FACTS` cache
  (`AccountStoreHandle::cached_nest_capabilities`) when the live
  `fauna.nest.info` fetch fails — one cache, one writer (the peer leg's
  pump), corrupt-decodes-to-no-evidence, so the two legs cannot disagree
  about what the nest last advertised. And the **limits screen now hides on
  the same token**: `fauna_client_features::capability_token` answers
  `p2p-share` (it had answered `None` — a stale spot from before the token
  existed), so an excised nest's row hides exactly as payments' does, from
  the same string the bind door consults.
- **Rule 1's severance precondition is VERIFIED on the lead app** (the
  trigger's discharge duty): tui renders `folder-member-remove-button`
  (owner rows; the backend rotates the content key —
  `apps/fauna-tui/src/settings/folders.rs`) and `folder-leave-button`
  (member rows) today, so the evict/leave affordances the serve leg's
  admission rests on exist before anything serves.
- **The two-seat mechanism is PROVEN one layer below the app**: a tier_1
  test (`fauna-sync-engine/tests/share_pump_two_seats.rs`) moves a 20 MiB
  multi-chunk file between two different users' seats over a real channel —
  engine-less serve stack on the author's side, loop-owned engine behind
  the real `EngineCommand` channel on the puller's — byte-identical on
  arrival, cursor-quiet on the second pass, and a stranger dies at
  admission with the ingest door never opening.

Still unbuilt after this leg: the app glue that binds at attach and runs the
pump (tui first) and the app-level two-seat journey. (Leg 7, landed later the
same day, built the client-side `p2p-share.transfer` composition itself —
§ *Wormability walk* rule 8; what it left open is the transfer-verdict
SURFACE, not the composition. The glue and that surface landed 2026-08-19
on tui — § *Built — the tui app leg* below, whose closing paragraph reports
the app-level two-actor journey GREEN the same day — so nothing from this
leg's "still unbuilt" list remains open.)

### Built — the tui app leg (2026-08-19): what the build measured

The lead app's glue over the shared halves above — `apps/fauna-tui/src/
share_glue.rs`, composition only. Three findings, each a refinement a
later app leg would otherwise re-derive:

- **"Bind at attach" is really "bind at store-ready".** Every seam the
  plane pumps through — rule 7's cached brake evidence, the discovery
  cache's dial targets, the sink's durable write, the transfer ledger —
  lives behind the account-store runtime's handle, which arrives
  asynchronously after the post-auth hook. The glue therefore starts at
  the `AccountStoreReady` edge (the earliest honest start), and a failed
  store assembly leaves the plane down for the session — the same
  degraded posture every other store consumer takes. The pump loop exits
  when the handle dies (sign-out's deterministic shutdown), and sign-out
  also drops the seat state: rule 5 ties the listener to participation,
  and a signed-out session participates in nothing.
- **The spec join is `FolderRef`-first, and any residual ambiguity serves
  NOBODY** (fixed 2026-08-19 — this bullet's earlier name-keyed
  shape was the finding: the serve side is the *duplicating* side, since
  the agent reports every location with a DB, unbound included, and a
  name-ambiguous join could pair one set's keys with another folder's
  bytes). The agent's serve info carries every binding's `folder_id`
  (required), a serve row matches only the binding wearing the same ref
  — never one wearing the same name (the name-only pairing a
  pre-identity binding took was retired 2026-09-24) — and two
  rows resolving to one `set_id` serve neither — the fail-closed
  direction at every arm. The ingest write path routes by the same ref
  (`SharedSetSpec.folder_id` → `ShareIngest.folder_id`, required → the
  agent's ref-keyed engine registry), because the
  engine's own provenance re-check judges writer/author facts, not set
  identity — routing carries the burden.
- **The share advertisement carries no relay, matching its transport.**
  The seat is bound relay-less today, so advertising the nest's relay URL
  would name a rendezvous nothing answers; the advertisement's `relay_url`
  stays `None` until the seat is bound with the relay, which
  [`p2p.md`](p2p.md) § The relay → *The cross-user seat and the relay*
  (ruled 2026-10-05) orders for the build
  that follows address discovery: then the
  advertisement carries the seat's own relay URL, and a reader attaches a
  relay only when that URL is its own — a relay serves two members on one
  nest only ([`p2p.md`](p2p.md) § The relay → *Across nests*, ruled
  2026-10-01). The ceremony's rule (no dial path the user has not chosen)
  is kept by its dials' own `relay_available: false`, not by the bind.

**The app-level two-actor journey is GREEN (2026-08-19, same day):**
`tests/e2e-unified/tests/test_share_pump_two_actor.py` — two real tui
seats (distinct device ids), a real MLS share, nest-mediated hydration as
the anchor, then the nest STOPS, the owner authors a file offline (the
B2.5 pending mint), and the file arrives on the member's disk peer-to-peer
with the transfer surface reporting it. The journey flushed out two real
defects on its first two runs — each is why an app-level run exists:

- **The folder rail skipped every Application envelope, so advertisements
  never reached the sink.** They post to the set's OWN channel — a FOLDER
  channel — and the `ShareEndpointsSink` arm lived only in the
  conversation rail's poll (the tier_1 carriage test drove that poll
  directly on the channel, masking it). Fixed: one shared routing arm
  (`route_share_endpoints`) called by BOTH polls;
  `poll_inbound_folder` decrypts Application envelopes just far enough to
  route ShareEndpoints (every other body drops — a folder channel carries
  no chat). Pinned by the carriage test's leg (4), which drives the
  folder poll and was red-verified against the reverted skip.
- **The pump must not need the nest to pump.** The spec read's
  key-bindings half loaded `__config` from the nest (the custody it read is
  the account-state plane kind `fauna.state.folder-keys` since the rail
  retired 2026-10-02 —
  [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md)
  § The `__config` dissolution schedule → *The kinds*), so the whole pump sat
  out the nest-down window — the exact scenario the plane exists for. The
  glue now holds the LAST-KNOWN specs and pumps them when the fresh read
  fails; safe because admission still consults the live M2 roster per
  request, the transfer gate prices every page, and a stale content-key
  generation fails closed at the seal/open.
- **An advertisement is marked sent only when the send SUCCEEDED** (found
  by the journey's flake hunt — one barrier-red in a four-run loop with
  every link silent). `AdvertiseState::due` had marked at build time, so
  one send lost in flight cost every member an hour of no dial row (the
  floor is the only re-send — best-effort by design). `mark_sent` is now
  the caller's act after a successful `send_share_endpoints`, and the
  floor is overridable for e2e (`share_pump::advertise_floor_secs`,
  `FAUNA_SHARE_ADVERTISE_FLOOR_SECS` — production keeps the hour), which
  turns any residual single-shot loss into a latency under the journey's
  deadline poll, never a verdict. Verified by a 6/6-green loop.

> ⚠ **SUPERSEDED IN PART — 2026-08-22. The journey is RED on `origin/main` under `--app linux`,
> and a MEMBER cannot advertise at all.** The GREEN above was a `--app tui` reading and still
> stands as one; what it did not exercise is the member leg against the nest's folder-channel
> claimant gate. Measured + tier_1-pinned cause: a member's advertisement is an *application*
> send, and every application send funnels through
> `fauna_conversations::backends::fauna_mls::post_app_message`, whose first act (with a
> `CommitGate` injected) is the device-owned-epoch takeover — a self-`Update` **Commit**. A member
> joined by Welcome never authored the set's epoch and never can, so it commits on every pass; the
> nest refuses a non-claimant's Commit on a claimed folder channel, and the advertisement never
> leaves. This puts three ratified rules in direct conflict — § Cross-user shared-set transfer's
> *"only a member can make one anybody believes"* (above), [`devices.md`](devices.md)
> § Cross-device MLS group-state sync's device-owned-epoch invariant, and the claimant gate
> (5d-SEC first-binder-wins) — so resolving it is a **nest security-boundary decision**, not a
> client fix. **Do not repair this by making the send doors skip the takeover**: that repeals the
> invariant whose purpose is *"fresh sender chains, so a shared single leaf never forks a ratchet
> generation"*. The same door carries custody payloads and receipts, so the member leg of custody
> is presumed broken on a claimed folder channel too (unmeasured). The mechanism is pinned by
> three tier_1 witnesses in `libs/fauna-client-mls-sync/src/gate_impl.rs`
> (`a_members_share_endpoint_advertisement_posts_a_commit`,
> `a_member_cannot_advertise_when_the_nest_refuses_its_takeover`, and the replica-hygiene twin);
> the remaining decision is tracked as row 87 of the tui backlog.
>
> **✅ RESOLVED 2026-08-24 — § *Built — the member advertisement unblocked* below.** The
> boundary decision landed: roster-membership commit admission at the nest + the member-side
> folder commit policy ([`../architecture/federation.md`](../architecture/federation.md)
> § Cross-nest shared folders + channel append owns the rule). The three-rule conflict above
> is history; the takeover warning ("do not skip it") stands unchanged.

> ⚠ **SUPERSEDED IN PART — 2026-10-01. The journey's nest-down half is RED on the built code since 2026-09-27.** The GREEN above stands as a 2026-08-19 reading. On 2026-09-27 the sync agent began holding every write whose set's row cannot be read ([`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability host, decision 2), and with the nest stopped that is every write: the owner's engine refuses the seal, so the B2.5 pending mint this journey rests on never runs and the member reads an empty page. Measured on tui 2026-10-01: `test_member_pulls_offline_authored_file_from_owner_peer` and `test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived` fail with the file never arriving; the stranger journey passes. Nothing in the plane this ledger records is at fault, and nothing here is to be repaired by relaxing the hold in place: the rule for that write is decision 2′ of the same section (ruled 2026-10-01 — the seal stands on the last floor read, and the hold moves to what is published to a nest), which also owns the code gap.

> ✅ **RESTORED — 2026-10-02.** Decision 2′ is built ([`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability host owns the shape): a failed row read keeps the last floor read, so the owner's write seals under it and the B2.5 pending mint runs, while a publication hold keeps everything it sealed off the nest until a read checks the floor. All three journeys of `test_share_pump_two_actor.py` pass again on tui (2026-10-02, `--app tui --strict-app`, 3 passed) and on linux (`--app linux --strict-app`, 3 passed; a first `--app linux` run lost one journey to an un-waited read of the transfer surface after the file had already arrived); the stranger journey never regressed.

### Built — the shared driver, and the linux leg: what the build measured

The trickle-down's first act was **not** a port. tui's `share_glue.rs` was
composition — but composition that included [`compose_specs`], the
`FolderRef`-first join, whose arms decide which folder's bytes are served
under which peer group's keys. Copying it into a second app would have been
the first of six re-derivations of that fix, which is the same argument that
had already moved the duplicate-`set_id` refusal down into
`refresh_serve_sources` ("defence in depth, not the primary guard"). So the
app-agnostic half was lifted to **`fauna_sync_engine::share_glue`** and both
apps now consume it.

**What is shared, and therefore never re-derived by a leg:** the pump loop and
its cadence (`FAUNA_SHARE_PUMP_SECS`), the `FolderRef`-first spec join with
all four of its fail-closed arms, the last-known-spec hold across a nest-down
window, rule 7's live-else-cached evidence composition, the serve refresh, the
advertisement decision (including `mark_sent` only on a send that reached the
channel), the pull pass's pricing through the transfer gate, the durable
sink's decode→bind→put body, and the six ids' readings as `LocalizedText`.
Two agent adapters (`GetShareServeInfo` / `ShareIngest`) are shared too — an
app that drives the external agent composes them with one call
(`agent_share_access`).

**What a leg still writes** — the `SharePlaneHost` trait's methods. ⚠ This
list said "and why each is genuinely per-app" until 2026-08-23, when it was
checked method-by-method against the two legs that implement it and **most of
it did not hold**: six of the nine methods, plus the durable-sink impl beside
them, were *byte-identical* in `fauna-tui` and `fauna-linux`. The stated
reasons were the stale part — both legs hold the same `Arc<NestClient>`, and
`CeremonySeat` is `fauna_sync_engine::offline_share`'s one shared type, not
"its own seat type" per app. What follows is that list re-derived from the
code, with the *real* reason each entry is still a leg's:

- **Its device label** — the two bind doors themselves are shared
  (`fauna_sync_engine::offline_share::{bind_seat, bind_share_plane_seat}`,
  lifted 2026-08-23; ~80 duplicated lines that differed by one string
  literal), and both bind through the session's one `SessionSeat`
  (§ Offline share initiation → *One seat per session*). A leg supplies only the string it calls itself by now: the
  `CeremonyTransportFactory` closure assembling the actor-keyed `fauna_iroh`
  endpoint was itself byte-identical between tui and linux and is **no
  longer a leg's either** (lifted 2026-08-28 to `fauna_iroh::
  ceremony_transport()`, alongside the same crate's existing
  `peer_leg_transport` — the "app side of the seam" for every native
  non-mobile app, both consumers already depending on it). The
  iroh-cleanliness bargain that keeps `fauna_sync_engine` naming no concrete
  transport substrate (`peer_leg`'s own escape, `PeerTransportFactory`) still
  holds — the closure moved to the transport crate, not into the
  substrate-agnostic driver.
- **The two UI nudges** (seat bound, state changed) — genuinely per-app: they
  post onto the app's own event loop, in the app's own message type.
- ~~**`membership`, `key_bindings`, `send_share_endpoints`**, and the durable
  advertisement sink beside them~~ — **no longer a leg's (lifted 2026-08-27 to
  the new leaf crate `fauna-client-share-host`).** They
  were NOT per-app by nature — each was one identical expression in both legs
  — but stayed app-side on a **crate-graph** constraint: they need
  `fauna-client-folders/mls` (`MlsSetMembership`; the engine-key producer they
  also call, now `resolve_engine_keys`, left the `mls` gate 2026-09-27) and
  `fauna-conversations`, and
  `fauna-sync-engine` takes `fauna-client-folders` deliberately *without*
  `mls` so the bearer-only `fauna-sync-agent` and the
  `--no-default-features` mail bridge do not pay for the conversations/MLS
  graph. **The ruling, and its cost:** the home is a native-only leaf ABOVE
  the driver with both edges — `fauna-client-custody`'s exact shape and
  reason ([`../ui/devices.md`](../ui/devices.md) § Custody facet) — rather
  than a widened `p2p-share` feature on the driver's crate (which would spend
  the leanness those three deployments bought) or a widened
  `fauna-client-custody` (whose unconditional consumers, `fauna-ffi` and
  `fauna-client-account-runtime`, would then need the new
  `fauna-client-folders/mls` + `fauna-peer-share` edges threaded behind a
  forwarded feature). A purpose-named leaf is itself the gate: nothing that is
  not a share-plane host links it, and an app takes it behind its own
  `p2p-share` feature exactly as it takes `fauna-peer-share`. The price is one
  more small workspace crate; what it buys is that the five legs still owed
  (and the one FFI host the three native ones will likely share) inherit these
  bodies instead of copying them a third, fourth and fifth time. The leg's
  shape: hold a `ShareHostSeams { nest, secret_hex, conversations }`, delegate
  the five app-agnostic answers to it (`membership`, `key_bindings`,
  `send_share_endpoints`, `live_capabilities`, `transfer_policy` — the last
  two re-served there so every app-agnostic answer reads off one struct), and
  register the sink with `install_durable_sink` at the account-store-ready
  edge.
- **`seat_node`** — one field access, kept only because `Seat` is an
  associated type; it costs a leg nothing and buys the driver its shape
  freedom.
- ~~`live_capabilities` / `transfer_policy`~~ — **no longer a leg's** (lifted
  2026-08-23 to `fauna_sync_engine::share_glue::{live_capabilities,
  transfer_policy}`): both read through the plain `NestClient` the crate
  already depends on, so nothing ever blocked them but the sentence above.

A leg is now roughly 80 lines against the ~700 the reference leg wrote, and
the honest remaining duplication between any two legs is nine one-line
delegations plus the genuinely per-app three (the seat type and its field
access, the two nudges, and the bind door's device label).

**Three findings from the second leg:**

- **"linux is in-process" was stale by two months, and it changed the leg.**
  Row 338 and the pump's own module docs said linux hosts the engine in
  process, so its ingest door would be a direct
  `always_resident::ingest_share_page` call. The A3 cutover retired
  `SyncDriver`: linux drives the external per-user `fauna-sync-agent` exactly
  as tui does, so the leg is the *same* agent-verb shape, not a second one.
  A leg that had trusted the note would have written a door for an engine its
  app does not host.
- **The paint cell's absence is a distinct fact from `NoSets`, and only one of
  them is a status line.** The surface renders nothing at all before a plane
  starts this session (signed out, a seam absent, the feature off), because
  "this device is not running the plane" and "this device has no shared
  folders to serve" are different statements and rule 5's transparency line
  owes the user the second one only when it is true. linux therefore posts one
  repaint nudge at start, where tui's per-frame paint reads the fresh cell for
  free.
- **A `LocalizedText` whose argument is itself a key needs `resolve_nested`.**
  The transfer gate's honest refusal renders "Limited by {source}" where
  `{source}` is `features.tier_admin` — a key, not data. Both legs' state
  reading resolves nested; a plain `resolve` would paint the raw key at the
  user, and each app's own unit test asserts against exactly that mutation.

**linux's leg**: the seat binds through `offline_share::bind_share_plane_seat`
(the `bind_with_share_plane` twin of the ceremony's own bind, relay-less for
the same reason), started at the account-store-ready edge —
`account_runtime::install`'s `Installed` arm — with its seams gathered on the
GTK main thread beforehand, because the agent provisioner lives in a
GTK-thread `thread_local` a tokio task cannot reach. The six ids render
page-level on the Folders page directly under the ceremony panel, the
placement tui uses.

**The remaining apps are gated, and none of them on the account runtime.**
The driver's every durable seam (dial targets, the sink's write, the
transfer ledger) lives behind the account-store handle, and every native app
hosts it
([`../architecture/account-runtime.md`](../architecture/account-runtime.md)
§ Implementation status today owns the three records):
**android since 2026-08-22** (the seat's first consumer), **macOS and iOS since 2026-08-25** (`FaunaClient.startAccountRuntime()`, called from the
shared `FaunaClient.start()` post-auth funnel both apple shells run; this
section said until 2026-09-26 that iOS hosted none, which was stale from that
day), **and windows since 2026-08-27** (closing the seat's last app call site — the
`NestRpcClient.StartAccountRuntimeAsync` wiring). Their shared host — macOS's
and windows' today, the phones' once each holds a replica the plane can
stand on (the next paragraph and § *Phone peers — design*) — is
`fauna-ffi`'s `p2p-share` feature (2026-09-24): `FfiNestClient::start_share_plane`
runs `share_glue::run` over `ShareHostSeams`, binding through
`bind_share_plane_seat` into the SAME process-wide, actor-keyed
`SessionSeat` the FFI panel door (`offline_share_bind_seat`) binds through,
so a session still holds one listener; the app passes its own sync-agent
provisioner (the plane's two agent verbs), a spool directory and a
two-method `FfiSharePlaneListener` for the repaint nudges, and paints
`share_plane_view()`'s readings (`LocalizedText`, the state reading
resolved nested). The feature rides `default`, not `store-safe`: the App-Store
flavor excises the plane as tui's does. **The host waits for its own inputs**
(2026-09-25): `start_account_runtime` returns once its assembly is *spawned* and the
conversations session is stashed when the app's own build finishes, so an app has no
account-store-ready edge to observe — tui and linux start on that edge natively, an
FFI app cannot. `start_share_plane` therefore polls for both (bounded, 180 s each)
and refuses only when one never lands, which keeps the ordering in shared Rust
instead of one retry loop per app, and **starting it again replaces the running plane** (2026-09-26): the previous driver is stopped before the new one starts, so a session holds one driver and an app that re-fires its ready edge never pumps one account twice. **macOS calls it** (2026-09-25):
`FaunaClient.startSharePlane`, from where its sync-agent provisioner starts — the one
construction point both the production launch and the e2e session patch share, the
latter skipping `FaunaClient.start()` — into a shared FaunaKit `SharePlaneModel` that
the six ids paint from (`SharePlaneSectionView`, directly under the offline-share panel
on the Folders page; nothing renders while no plane runs; every reading resolves
nested, pinned by `SharePlaneReadingsTests`). Its live witness is the member seat of a
tui owner — apple's owner-side access-grant surface is declared absent
(`folder-member-role-select`; `../ui/folders.md`), so macOS cannot own the journey's
set and its own serving half is not yet witnessed live. windows, iOS and android do not call it yet, and the
shared FaunaKit surface is already compiled into iOS but never started there: iOS
runs no sync agent, so it hits the same provisioner gap as android (next paragraph)
— that, and not its account runtime, which it hosts. web stays the declared structural
absence (no QUIC seam in the wasm graph — walk rule 5).

⚠ **The "the app passes its own sync-agent provisioner" clause two sentences
up does not hold for android — and android's leg on this plane is OWED,
GATED like iOS's: the user's ruling of 2026-09-26, refuting the 2026-09-25
declared-absence ruling.** windows
(`SyncAgentSessionHost.cs`) and macOS (`FaunaClient.swift`) both drive a real
external `fauna-sync-agent`, so `FfiSyncAgentProvisioner` names something
real there; android runs none —
`libs/fauna-ffi/src/sync_agent_provisioning.rs`'s own module doc calls the
type dead code on android (and iOS), matching
[`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md)
§ Scope per platform. The row that scoped the leg assumed the other arm:
`SharePlaneHost`'s two seams (`ShareServeInfoSource`/`ShareIngestDoor`)
implemented in-process over android's own `FfiSyncEngineHost`. **That arm
has nothing to stand on today, because both seams need a local replica and
android keeps none yet.** The serve half reads every body from the
set's bound tree (`ShareServeSource::chunk_body` →
`chunk_body_from_hit(&watch_dir, …)`, `peer_share_store.rs`) and the ingest
half materializes every pulled body into it
(`SyncEngine::ingest_peer_share_rows` resolves each path under
`self.watch_dir`, `engine.rs`) — while android's `FfiSyncEngineHost` is a
construct-run-drop *ingress* host with no resident engine and no watch
directory: both android ingresses (MediaStore, SAF) stage bytes to a temp
file, `ingest_file`, and delete the temp, and "a location binding is **not**
android's shape and no VFS seam was added"
([`sync-engine-deployments.md`](sync-engine-deployments.md) § Implementation
status today (android byte-sync); ui.yaml's `folders` android note says the
same — no local-folder binding). An adapter written over today's ingress host would report no serveable set
(the honest `NoSets` line, for ever) and refuse every ingest (no tree to land
in): a listener bound for nothing, which rule 5 forbids. So android does not
call `start_share_plane` yet; the six ids stay unrendered there (the
surface's "plane not running" reading — the linux leg's finding below), and
outcomes 3/7/8 of the offline-sharing catalog page read short on android —
**unbuilt, not absent** (the user's ruling, below the re-examination): no
per-outcome `absences` entry (the grammar exists —
[`../architecture/feature-catalog.md`](../architecture/feature-catalog.md)
§ Coverage contract, *A column a goal doc excuses from some outcomes* —
and is deliberately not applied to either phone), no ui.yaml android note,
no e2e `declared_absence`. **What unblocks the leg** is the local replica
android is missing — not a location binding over a user's tree (the
deployments doc still rules that out, rightly) but an **on-demand replica
over an app-owned root**, the shape the re-examination below reached and the
user adopted: the SAF `DocumentsProvider` binding over `provider_face`
([`on-demand-files.md`](on-demand-files.md) § Android SAF DocumentsProvider
binding) plus one joint phone-peer design
for both phones as foreground partial peers.
**iOS is not android's shape either, nor macOS's:** it drives no agent, and
its replicas live in the File Provider extension's process as on-demand
placeholders whose bodies are local only once hydrated
([`on-demand-files.md`](on-demand-files.md) § Apple File Provider binding),
so an iOS leg is an app↔extension seam over a partial tree — settled by that
same joint design pass (§ *Phone peers — design*, decision 3), never a lift
of macOS's agent arm.

**Re-examined 2026-09-26 — the ruling's rationale, questioned against iOS
(a design pass, user-directed; advisory, and
RE-PUT to the user at the end — nothing here is ratified or built until they
answer).** The user asked that every asserted phone constraint be attacked
before they rule. The first finding is about provenance: **no goal doc ever
gave scoped storage, battery, background-execution limits or "phones are not
meant to hold full copies" as the reason** — that sentence was a session's
paraphrase. The recorded rationale is narrower and purely architectural,
three links: (i) a SAF binding is a `content://` tree with no path, so both
android ingresses take the library-ingress shape and "a location binding is
not android's shape" ([`sync-engine-deployments.md`](sync-engine-deployments.md)
§ Implementation status today (android byte-sync), *The shape landed*; the
same rule as [`file-sync.md`](file-sync.md) § Implementing Sync on a New
Platform, step 3); (ii) since 2026-09-25 no app binds a location in-process —
every resident engine is the desktop agent's
([`sync-engine-deployments.md`](sync-engine-deployments.md) § Apple apps —
convergence design, *the resident half … was retired*), and android runs no
agent ([`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md)
§ Scope per platform names three desktops and iOS, android nowhere); (iii)
hence android keeps no replica, and both seams read and write one. Each link,
and each folk constraint, examined:

1. **"A SAF tree has no path" — TRUE, and narrower than the ruling drew it.**
   It holds for a *user-picked external* tree: a `content://` document URI
   exposes no filesystem path, so neither `chunk_body_from_hit(&watch_dir, …)`
   nor `ingest_peer_share_rows`' `atomic_write_file` under `self.watch_dir`
   can run over one, and `WatchedDirectoryManager.kt` rightly reads bytes
   through the content resolver and stages them. It does **not** hold for an
   **app-owned** tree: android's `filesDir` is an ordinary path-addressable
   directory — already the account store's root (`AccountStores.kt`) — and
   android already ships the OS surface that exposes such a tree to the
   system file picker and every other app: `FaunaDocumentsProvider.kt`, a
   manifest-registered SAF `DocumentsProvider` (read-only, then enumerating
   the Room `sync_files` rows and serving a placeholder body; since
   2026-09-28 its read path serves the owned-tree host instead —
   [`on-demand-files.md`](on-demand-files.md) § Android SAF DocumentsProvider
   binding).
   That is the platform twin of apple's File Provider domain: an OS-driven,
   control-inverted, on-demand surface over a Fauna-owned root under the
   system's file browser, no user path chosen. The 2026-07-16 sentence ruled
   out binding a *user's* SAF tree; the 2026-09-25 ruling generalized it to
   "android keeps no replica" without weighing the app-owned root. **iOS:**
   the identical fact — an FP domain "cannot take over an arbitrary user
   directory" — produced the opposite conclusion: a set-level on-demand
   replica under the system-managed location
   ([`on-demand-files.md`](on-demand-files.md) § Apple File Provider binding),
   the engine's staging root a real path per set. Android has no on-demand
   binding at all — that doc ratifies none for it — which is the actual gap,
   and a uniformity one (priorities #1/#3/#4): the `provider_face` cores are
   written to be consumed by every platform's on-demand host (apple's appex,
   windows' planned cfapi migration, linux's FUSE root); a SAF
   `DocumentsProvider` over them is the fourth binding, and the only one that
   runs **in the app's own process** (a `ContentProvider` with no
   `android:process` attribute is in-process), so its seam to the share plane
   is a direct call — simpler than the iOS app↔extension seam below.

2. **Battery and background execution (Doze, App Standby, foreground
   services, WorkManager; iOS `BGProcessingTask`) — a REAL constraint on
   *when* a phone serves, not on *whether*, and not android-specific.** The
   plane's driver is a resident loop over a bound iroh listener; a phone
   cannot keep that alive in the background on either platform — android
   needs a foreground service (the app already declares
   `FOREGROUND_SERVICE_DATA_SYNC` and a one-shot `SyncService` of that type),
   iOS has no daemons and no foreground-service equivalent at all, its
   `BGProcessingTask`s are opportunistic and short, and its FP extension is
   launched for Files callbacks, never for a peer's dial. But the contract
   does not ask for background serving: the scenario is two people **sitting
   together** moving a set, the plane is multi-source and resumes from any
   peer or the nest with nothing re-sent, and rule 5's participation switch
   is the user's per-device brake. So a phone's natural shape is a
   **foreground peer** — the plane runs while the app is open (android may
   extend it with an explicit "keep transferring" foreground service, the
   download-manager pattern; iOS cannot) — exactly as both phones already
   bind the ceremony's own seat in the foreground today (`offlineShareBindSeat`
   in `ApiClient.kt` and in the shared FaunaKit `APIClient.swift`). Nothing in
   this constraint distinguishes android from iOS; iOS is the stricter of the
   two.

3. **Storage volume — "phones don't hold full copies" — TRUE of a full
   replica and IRRELEVANT to a partial one, which the plane already models.**
   Both seams tolerate absence honestly: `chunk_body_from_hit` answers
   `Ok(None)` for a file not on disk (the puller turns to another source),
   and `judge_materialization` skips `Placeholder` rows ("storage policy")
   while every accepted row still lands in the overlay unmaterialized. So a
   placeholder replica **serves what it has hydrated and ingests rows without
   materializing bodies** — a partial seed, which the multi-source contract
   was written to accept. What a phone shape adds in shared Rust is a
   *materialization policy* for pulled bodies on an on-demand root (record
   the row as a placeholder; hydrate on open or pin — the storage-not-direction
   split [`on-demand-files.md`](on-demand-files.md) § Sync direction
   ratified) and a serve seam that reads a body through the provider host
   rather than a bare `watch_dir` path. **iOS today:** the FP replica is
   exactly such a partial tree, but its hydrated bytes live in the OS-owned
   tree of an app-dead extension process, so an iOS serve leg is the
   app↔extension seam over a partial tree named above — a design question
   android, in-process, does not have. **One thing the path-based serve
   forecloses on both phones:** the set a phone most plausibly *originates* —
   the holiday videos of the founding scenario — is the photo library, whose
   originals have no path either (PhotoKit assets; MediaStore descriptors),
   so a phone serving its own photos needs the serve half to read plaintext
   through an opened descriptor, not a path. `chunk_body_from_hit` is a range
   read plus a re-seal and does not care where the range comes from.

4. **The 2026-09-25 retirement of the in-process resident engine — TRUE, and
   the strongest link, but it retired *location-bound watcher* engines, not
   resident engines as such.** `FfiFileProviderHost` keeps a folder-scoped
   engine resident outside the agent today (no watcher, control-inverted,
   request/response) and is the sanctioned out-of-agent host for on-demand
   surfaces; an android `DocumentsProvider` host is that host with a
   different OS binding, and `SharePlaneHost`'s two seams over it are the
   in-process arm the 2026-09-25 design pass assumed — over an on-demand
   replica instead of a bound tree. New work, not a reversal.

5. **Uniformity (priorities #1/#4).** iOS and android are in the same
   situation today — no share plane started on iOS, no replica
   on android, the six ids unrendered on both, outcomes 3/7/8 short on both —
   yet iOS's leg is recorded as *owed, gated* while
   android's was ruled a *declared absence*. (This item named iOS's gate as
   "its hosting story" when written; iOS hosted the account runtime already,
   and its real gates are § *Phone peers — design*'s.) Two phones with the same
   fundamentals must take one classification; the only structural absence on
   this plane is web's (no QUIC seam in the wasm graph — rule 5).

**RECOMMENDATION (this pass's; advisory): REFUTE the absence as a permanent
ruling and re-classify android's leg exactly as iOS's — OWED, GATED — behind
one phone-peer design, not two.** The shape: **(a)** android gains the
on-demand replica it is missing — a SAF `DocumentsProvider` binding over the
shared `provider_face` cores, an app-owned root under `filesDir`, set-level
like apple's (`folder-on-demand-toggle`, "Show in Files"), the fourth binding
in [`on-demand-files.md`](on-demand-files.md) (a new section and registry
row; Rule A for the toggle's android scope); **(b)** one design pass for
*both* phones as **foreground partial peers**: the plane runs while the app
is open, serves hydrated bodies (and, later, library originals through a
descriptor-reading serve seam), ingests overlay rows under an on-demand
materialization policy, and reads `share-serve-status` honestly when
suspended; iOS's leg additionally settles the app↔extension byte seam (the
recommendation also named its account-runtime hosting, which was already
built). Until (a) and (b) land both phones stay
**unbuilt, not absent**: the six ids unrendered, the e2e keeps android (and
iOS) out of its supported-app set with no `declared_absence`, and the catalog
cells read short — the honest state. The alternative, RATIFY, is coherent
only if iOS is ruled the same way — phones never take part in
device-to-device transfer, the photo-library origin case included; it is
cheaper by two design rows and one android feature, and it forgoes the one
scenario where a phone is the natural source.

**RE-PUT TO THE USER:** **(1) REFUTE** — phones are peers: mint the android
on-demand-replica design row and the joint phone-peer design row,
close the ratify-or-refute row pointing at them, no catalog or ui.yaml
absence; or **(2) RATIFY** — declare BOTH phones absent from outcomes 3/7/8
by design: per-outcome `absences` for android **and ios** once the
per-outcome grammar lands, the folders-page android and ios notes under Rule
A, the e2e `declared_absence` for both, and iOS's owed leg withdrawn. A
ratification of android alone is the one answer this pass advises against.

**User ruling 2026-09-26: (1) REFUTE — phones are peers.** android's leg of
outcomes 3/7/8 is OWED, not absent, gated like iOS's behind an android
on-demand replica (a SAF `DocumentsProvider` over `provider_face` on an
app-owned root) and one joint phone-peer design pass (both phones as
foreground partial peers); until those land both phones read *unbuilt, not
absent*, and no catalog absence, ui.yaml android note or e2e
`declared_absence` is added for either. The two design rows were minted at
the close of the ratify-or-refute row.

### Phone peers — design (2026-09-26; the direction is user-ruled, the decisions below are refutable at build time; the shared arm BUILT 2026-10-01, the app legs UNBUILT)

**What this is.** The joint design the ruling above called for: android and iOS take part in this plane as **foreground partial peers** — one classification, one shared-Rust arm, one policy, and per-app code only in the two shells. The shared arm is built (2026-10-01 — *Built — the phone-peer shared arm* below, which also records what the build corrected in the decisions here); no app leg is, and both phones still read *unbuilt, not absent*. The working (alternatives weighed, the platform facts still to be measured) is in the design record; the claims are here.

**Three gates, and iOS's account runtime is not one of them.** iOS has hosted the runtime since 2026-08-25 ([`../architecture/account-runtime.md`](../architecture/account-runtime.md) § Implementation status today). What both phones wait on: **(1) a replica of a *shared* set.** This plane moves M2-bound sets only (`share_glue::compose_specs` skips every unbound one), and an on-demand host is built through the capability constructor, which since 2026-09-27 loads a bound set's keys from custody itself and follows its rotations ([`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability host, decisions 1 and 2); a member's replica needs shared-with-me sets in the presence plan — in the shared plan and on android since 2026-10-01, apple's leg owed — and what is still missing outright is an owner's pre-bind files, which need the pre-bind re-seal enabled. Both gaps are declared by their owner — that section's implementation status. **(2) The platform's replica itself** — android's binding ([`on-demand-files.md`](on-demand-files.md) § Android SAF DocumentsProvider binding, unbuilt); iOS's is built, but iOS refuses the domain identifier of every real set, so it has never registered one (measured 2026-09-27, decision 3). **(3) The shared arm below**, and on iOS the carriage to its extension.

**1. One arm over an on-demand replica, beside the agent arm.** The driver's two seams stay what they are (`ShareServeInfoSource`, `ShareIngestDoor`); what they stand on stops being agent-shaped. `AgentShareAccess` becomes the **replica access** — the one pair of seams, whoever hosts the replica — with two constructions: the agent's (today's; nothing changes on the app↔agent seam) and the **on-demand host's**, in `fauna-sync-engine` beside it, because it needs the engine, the set's state DB and the owned-tree operations of `provider_face`, and none of the MLS or conversations edges that put `fauna-client-share-host` above the driver. `FfiNestClient::start_share_plane` takes that access in place of a sync-agent provisioner, so the FFI host has two arms and no phone-only fork. A process's on-demand hosts live in one shared-Rust registry, process-wide and keyed by account and set, which the platform's provider and the plane both reach: one host per set, one writer per set. The registry holds a host only as long as its provider does (sign-out, switch and toggle-off drop the hosts, and a dropped host leaves the plane with it), and a provider asks it for a set's live host before it builds one.

**The serve half reads a body through a source, never a bare path.** `ServeFolderInfo` and `SharedSetSpec` carry a **body source** where they carry `watch_dir` today, and `chunk_body_from_hit` and `rederive_manifest_from_disk` take it: the bound tree (the desktops, behaviour unchanged), the on-demand replica's lookup (kept root, then cache root, else none), and later an opened descriptor (decision 4). A placeholder answers `Ok(None)` and the puller turns to another source — the honest partial seed. The on-demand source reads the row it needs (is this cache-root body still the file's?) through its own connection to the set's state DB, never through the replica's writer: the serve half must not queue behind a hydration, and on iOS the writer is another process. **The serve half never hydrates on a peer's behalf:** it serves what is on the device, so a peer's request can spend neither the phone's data allowance nor its storage.

**The ingest half lands rows always, and bodies by policy.** On an on-demand replica the door records **every accepted row** — the overlay row, and a path that has no row yet as a placeholder (metadata only; a path that already has a row is the nest fold's to re-point) — and lands a body only when it is **wanted**: *(a)* the row is un-sequenced, so the nest cannot serve its bytes and peers are their only source (the no-nest-in-between promise, and the first build); *(b)* the user asked for the file — an open the nest could not answer — which the state writer records as a want in the set's state DB and the pump reads as it reads the cursor (a later slice). The pump plans its fetches by the same policy, so an unwanted body is never pulled, priced or metered. A landed body the nest has not confirmed goes to the **kept root** and stays there until the reconcile's *confirm* stamps the dehydration proof, which is what demotes it to the cache root — the row half's "never earns the dehydration proof", in root terms; a body for a sequenced row lands in the cache root (no build lands one yet: the first want is the un-sequenced row alone). The reconcile therefore runs on the on-demand replica's populate fold as it does on the resident apply, and it has two outcomes that touch the tree: a **confirm** demotes the body, through the one dehydration gate; a **supersede** — the nest recorded some other head for the path — **drops** the landed body, so the nest's row stands. **A peer-landed body is kept, but it is not a write intent.** The kept root's own rule is that a body found there is an un-recorded edit and is uploaded; a peer's body rests there for the same reason an edit does (the nest cannot serve it again) and for no other, so the replica's host never ingests it — recording it would publish another member's file as this device's own — and it stops being a peer's body the moment the user writes over it ([`on-demand-files.md`](on-demand-files.md) § Android SAF DocumentsProvider binding owns the kept root's rule and carries this exception). The materialization judge keeps every conservative arm and changes one: on an on-demand replica a placeholder holds no bytes, so landing a wanted body over it destroys nothing — while a body in the kept root is a local write intent and is never overwritten, unless it is the body an earlier peer row landed there, which that peer's next write replaces. **Storage floor:** a wanted body is not landed when that would take the device's free space under a floor (a shared-Rust constant, never a setting); its row stays a placeholder and a later pass retries. **The transfer row says so with the reading it already has** (the user's answer, 2026-09-30): `share-transfer-state` reads *Limited by {source}* with the source *free space on this device* — no new element and no new id.

**2. A foreground peer: the plane runs while the app is in front, and it stops — it does not pause.** Both shells start the plane where macOS starts it (`start_share_plane` already waits for the account runtime and the conversations session, and starting again replaces the running plane) and stop it when the app leaves the foreground: `stop_share_plane` ends the driver and unbinds the session's seat — the `unbind` the participation switch uses — so a backgrounded phone holds **no socket**. A listener a suspended process cannot answer is a listener bound for nothing (walk rule 5). Coming back to the front starts it again; the pull cursor and the overlay are at rest, so a transfer the background cut resumes with nothing re-sent. **The suspended reading is one the surface already has:** a stopped plane is "this device is not running the plane", rendered as nothing at all — no new reading and no new element. **android's "keep transferring"** is designed here and is not in the first build: while a transfer is in flight the app holds a foreground service of its existing `dataSync` type, started while the app is still in front, which keeps the plane running after the app leaves it and ends when the plane goes idle. It is a product default with a stop action on its notification, never a setting, and the device's participation switch stays the brake; its notification is a new user-visible surface, so it is asked of the user before it is built. **iOS gets none, by platform fact:** it has no foreground-service equivalent, and a `BGProcessingTask` is opportunistic and cannot hold a listener open for a peer's dial.

**3. iOS: the extension is the state writer, so the door crosses to it; the bytes wait in the shared container.** iOS's replica host is the File Provider extension — another process, and the set's one writer. The plane runs app-side (the runtime, the seat, the conversations rail and the keys are the app's) and reaches the replica as a desktop app reaches its agent. **Reads** are cross-process reads of the set's state DB in the app-group container (the Media-badge precedent). **The ingest door is a request to the state writer with the spool handed over by path** — the platform's own app→provider service channel to the extension, the spool in the app-group container, answered by the same host operation android calls in-process. **Measured 2026-09-27 (iOS 26.5 simulator, the ad-hoc signing a local build carries): the channel is there, and opening it launches the extension.** With the extension not running, `FileManager.getFileProviderServicesForItem` on the domain's root offered the extension's `NSFileProviderServicing` source and started the extension process (0.3–1.9 s cold, ~60 ms warm); `getFileProviderConnection` opened in at most 42 ms and a trivial request round-tripped in 1–12 ms. The door rests on it. The durable inbox in the container (the extension drains it, the cursor read back cross-process) stays only as the fallback should a device refute the simulator. **Bodies:** the extension's staging root is iOS's kept root — a landed body waits there, `fetchContents` answers from it before asking the nest, and the app-side serve half reads it by path. **Nothing demotes a body from it today (measured 2026-09-27):** a body the user created through Files is still in the staging root, byte for byte, after the nest records it — the extension stages it for `ingest` and neither side removes it (pinned in `bins/fauna-nest/tests/conformance_file_provider_client.rs`). So the kept-root rule (a body stays only until the nest confirms it) needs its demotion built with this leg; until then the staging root also keeps a second copy of every file created through Files, beside the OS's own. The OS-managed tree is iOS's cache root and the OS owns it: serving a body hydrated only there is a **deferred slice**, and no longer gated on a measurement. Measured 2026-09-27: the app lists and reads its own domain's tree at the location `NSFileProviderManager.getUserVisibleURL` names; a read of a hydrated item returns its bytes with no provider callback (0.3 ms), while **any** read of a dataless item, plain or coordinated, hydrates it through `fetchContents`. So that slice's serve half tells the two apart by `stat`'s `SF_DATALESS` flag, which enters no provider, and answers none for a dataless file; the *downloading status* resource key cannot, because it still read *not downloaded* after the hydration. A second, app-hosted engine over the same set was rejected — two writers. **The gate, named:** iOS's leg waits on gate (1), on the shared arm and on its own carriage; its live witness additionally waits on the extension running under test at all. A simulator can host it (measured 2026-09-27): File Provider domains and the app-group Keychain belong to the simulator device, which the harness already mints per session, and the app→extension credential rendezvous completes under ad-hoc signing — the `-34018` refusal is macOS's keychain ([`on-demand-files.md`](on-demand-files.md) § Apple File Provider binding, *headless-first testing*). Two things still stand between: the harness installs a bundle it assembles itself, with no extension and no entitlements, so the run needs the `.xcodeproj`-built bundle; and iOS refuses the domain identifier of every real set ([`on-demand-files.md`](on-demand-files.md) § Apple File Provider binding, *the actor-scoped device identity*). It does not wait on the account runtime.

**4. Library originals are a deferred slice with rows of their own — shaped now, built after.** A set a phone feeds through an ingress (the photo library above all) has no on-demand replica by the presence plan, so a phone serving its own photos is a different serve arm: the ingress host's state DB (retention is unconditional at the recording funnel, so its rows are already there) plus a **descriptor body source** — a range read through a descriptor the shell opens for the path, then the same re-seal. It is out of the first build because its two platforms differ in a way that wanted measuring, not deciding: android's `MediaStore` hands back a seekable descriptor, while PhotoKit's resource API streams a resource from its start and an original may not be on the device at all. **Measured 2026-09-27 (iOS 26.5 simulator, an 82 MB video original):** `PHAssetResourceManager.requestData` does stream from byte 0 only (1 MiB chunks, 79 of them), but `PHImageManager.requestAVAsset` for the `.original` version with network access off hands back an `AVURLAsset` whose URL is the original file itself, which the app opens, seeks and reads — a 64 KiB range at 81.3 MB in 1.1 ms, byte-identical to the same range of the stream. So for an original on the device, iOS's opener has android's shape, a seekable descriptor, and no platform bound on size is owed. An original that is **not** on the device is unmeasured: the simulator has no iCloud Photos, so the gate is a device with iCloud Photos' *Optimize Storage* holding an evicted original; the serve half's rule (never fetch on a peer's behalf, answer none) needs no measurement to hold. The simulator's sandbox is not a device's, so the file-URL read is re-confirmed on a device before the opener rests on it. The body source of decision 1 is cut so the descriptor form is a third implementation, never a redesign.

**5. Witnesses.** Below the apps, the policy and the judge's changed arm are pure functions pinned at tier 1; the arm is proven over the in-memory transport in the two-seat vehicle the plane already has (`libs/fauna-sync-engine/tests/share_pump_two_seats.rs`) with one seat over a bound tree and one over an on-demand replica — the un-sequenced body lands in the kept root and is served back, a placeholder answers none, an interrupted pull resumes — and the stop is asserted on the socket, never on a flag. At the apps, `test_share_pump_two_actor.py` grows an android and an ios seat for outcomes 3 and 8 of the catalog page, each first as the **member** of a desktop owner — the shape macOS's leg took, and a phone dials out more easily than it is dialled; outcome 7 needs the phone to be the serving seat and follows. No catalog absence, ui.yaml note or e2e `declared_absence` is added for either phone at any point, and the six ids render page-level under the ceremony panel, the placement tui, linux and macOS use — no new id.

**6. Uniformity.** One classification, one arm, one policy, one lifecycle. The divergences, each with its platform reason: the door's **carriage** (android calls the host in-process; iOS crosses to its extension, another process by platform fact); the **cache root's owner** (android's provider owns both roots; on iOS the OS owns the hydrated tree, which defers that one serve slice); and **keep transferring** (android only). Whatever else a build leg finds is named with its reason or escalated.

**Build order** (rust-first; every row re-verifies its slice against the code): the shared-set replica gate and the shared arm (the shared arm BUILT 2026-10-01) → android's leg behind its binding's read path and iOS's leg behind a domain identifier iOS accepts (its carriage measured 2026-09-27, decision 3) → the wanted-body slice → library originals and keep transferring.

### Built — the phone-peer shared arm (2026-10-01): what the build measured

**What is built.** The shared-Rust half of *Phone peers — design* — decision 1 whole, the stop of decision 2, and decision 5's tier-1 witnesses — in `fauna-sync-engine` and `fauna-ffi`; no app code. **The body source** (`share_body`): one trait with two reads (a body's length, a range of it), the bound tree (`TreeBodySource` — what the serve half did before, byte for byte) and the on-demand replica (`OnDemandBodySource` — kept root, then the cache root while the row is hydrated, else none), threaded through `ServeFolderInfo`, `SharedSetSpec`, `ShareServeSource`, `chunk_body_from_hit` and `rederive_manifest_from_disk`; the agent's `GetShareServeInfo` reply is unchanged and its `watch_dir` maps to the tree source. A descriptor-backed source (decision 4) is a third implementation: both reads are answerable from an open descriptor. **The replica access** (`share_glue::ReplicaAccess`, the renamed `AgentShareAccess`) with its two constructions, `agent_share_access` and `on_demand_share_access` over an `OnDemandHosts` registry seam; tui, linux and the FFI host changed mechanically. **The landing policy** (`share_landing`), pure: the want (`body_is_wanted` — the un-sequenced row), the root (`landing_root`), the floor (`STORAGE_FLOOR_BYTES`, `landing_fits`, `free_space`) and the on-demand judge (`judge_on_demand_landing`). **The door**: `OwnedTree::share_ingest`, the twin of the agent's `ShareIngest` arm — the same decode, the same `judge_peer_row` provenance, relay retention and cursor rule as the resident ingest (one engine function, `ingest_peer_share_rows_landing`, with the landing as its one parameter), landing through the owned tree's own `land_body` and `record_placeholder`. **The reconcile on the populate fold**: `record_placeholders_from_changes` runs the same retire core as the resident apply (`retire_share_overlay`) and reports what it confirmed and superseded; `OwnedTree::settle_peer_bodies` demotes and drops by that report. **The FFI**: `FfiFileProviderHost::share_ingest` (exported, so iOS's extension can answer it), the process-wide registry (`on_demand_host`; every `app_dead_owned_tree` host registers itself), `FfiReplicaAccess` with `agent_replica_access` and `on_demand_replica_access`, `FfiNestClient::start_share_plane` over the access, and `FfiNestClient::stop_share_plane`. **The reading**: `SetPullOutcome::storage_limited` renders *Limited by free space on this device* through the existing `folders.share_transfer_state_limited` key.

**What the build found, and corrected in the design above.**

1. **A kept-root body is uploaded by the replica's own sweep — so a peer's body landed there would have been published as this device's write.** The owned tree's start sweep ingests every kept-root body, and the engine re-drives an upload for a `Synced` row with no recorded-head proof, which is exactly the row a peer-landed body has. On a desktop the same row is inert because the scan uploads only what changed; an owned tree has no scan, only the sweep. The engine now answers *is this the body a peer landed* (`holds_provisional_peer_body`: the path's overlay row is materialized and the disk still hashes to what it landed) and the tree's ingest refuses such a body. Writing over it moves the hash, and it is an ordinary edit again.
2. **A supersede has to drop the body.** The design named only the confirm. A landed body the nest superseded would sit in the kept root over a row the fold re-points to a placeholder — the shape the sweep reads as *an edit the nest's head moved under* and uploads through the conflict arm. `settle_peer_bodies` drops it, and only while it still hashes to what the peer landed.
3. **A placeholder at the row's own head still owes its body.** A wanted body that cannot land this pass (the floor, a cut transfer) leaves a placeholder row at the peer's manifest, and the resident rule *the tracked head already is this manifest — fetch nothing* would then skip it for ever. Both the pump's plan and the judge treat an on-demand placeholder at that head as owing the body.
4. **The floor is 1 GiB and is checked twice.** Twice android's fixed low-storage threshold, so a transfer stops well before the OS reclaims caches. The pump checks before it plans a fetch — counting each body twice, once spooled and once landed — so a body that cannot land is not pulled; the door checks again on the bytes that arrived. Where free space cannot be measured the floor does not refuse.
5. **The serve half reads the replica's rows itself.** The three-operation seam's lookup is the tree's rule (`owned_tree::body_path`), but the serve half runs it over its own state-DB connection rather than through the host's worker (decision 1 above).
6. **The registry holds hosts weakly, and registration is the constructor's.** A strong registry would keep a provider's dropped hosts alive across a sign-out. android's existing provider code is therefore already registered, unchanged; what its leg adds is asking `on_demand_host` before it builds.

**Witnesses.** Tier 1, pure: `share_landing::tests` (the want, the root, the floor, every arm of the judge), `share_body::tests` (both sources), `share_pump`'s plan tests (only the un-sequenced body is planned; the floor at plan time; a placeholder at the row's head plans again), `share_glue`'s label and on-demand-access tests. Tier 1 over a real engine and state DB (`share_replica_test`): the un-sequenced body lands in the kept root with no proof and a sequenced row lands none; a wanted body lands over a placeholder; a kept-root write intent is never overwritten and a peer's own body is; the floor leaves a placeholder and a later pass lands the body; the populate fold confirms and the body is demoted through `is_dehydration_safe`; the populate fold supersedes and the body is dropped with nothing recorded as this replica's change; the door resumes a cut pull and keeps the cursor rule. `owned_tree::tests`: a peer-landed body is never ingested; a confirmed one is demoted only on the engine's proof; a superseded one is dropped unless the user wrote over it. **The vehicle** (`tests/share_pump_two_seats.rs`, `an_on_demand_seat_lands_the_unsequenced_body_and_serves_it_to_a_third_pull`): a bound-tree seat serves an on-demand seat over the in-memory transport — a pull cut before the hand-off leaves the cursor at rest, the next lands the page (the body in the kept root, the sequenced row a placeholder), the third moves no byte — and the on-demand seat then serves the kept-root body to a third seat while answering none for the placeholder without failing her pull. **The stop** (`offline_share::tests::stopping_the_plane_frees_the_listener_and_a_restart_binds_again`) is asserted on the transport's accept side with a driver holding its own seat clone, as `share_glue::run` does. Five guards were confirmed by removing each and watching its witnesses fail: the sweep's refusal of a peer-landed body, the reconcile on the populate fold, the un-sequenced want, the stop's unbind, and the cache root counting only under a hydrated row.

**What is still owed, by whom.** *(a) The app legs* — android's and iOS's (*Build order* above): a shell that builds the access with `on_demand_replica_access`, starts the plane at the foreground edge and stops it at the background one, and the six-id paint. *(b) iOS's landing.* `share_ingest` refuses on a host that owns no tree; iOS's extension keeps its kept root in its staging root and the OS keeps the rest (decision 3), so its leg gives the door those roots. *(c) The confirm's proof is marked as fetched (built 2026-10-04) — nothing owed.* [`file-sync.md`](file-sync.md) § Relay serving → *A holder keeps what it wrote* rules that a row which does not say how its proof was earned counts as the seat's own write; `retire_share_overlay`'s confirm stamps the proof *fetched*, so a confirmed peer body in a metadata-only folder is freed like any fetched body (the owned tree demotes it to the cache root) while the seat's own records stay kept — pinned by `share_replica_test`, whose replica has no residency reading and so frees the confirmed body only because it is marked fetched. *(d) A landed body is assembled in memory*, as the resident landing's is; a bounded-memory landing is owed before a phone takes large bodies. *(e) The cache-root landing* (a sequenced body the user asked for) and the want that drives it are the wanted-body slice's; the door answers that arm *not built*. *(f) A seal on the last floor read with the nest unreachable* ([`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability host, decision 2′(b)) is not built here; a control-inverted host still holds such a write, and the phone leg inherits the rule when it lands. *(g) The host's first populate does not apply a stale hydrated row*, so a superseded path reads its old head until the first refresh re-points it. *(h) macOS's call site* (`APIClient.startSharePlane` now wraps its provisioner in `agentReplicaAccess`) is changed and not yet compiled on macOS. *(i) The guides* gain the *free space on this device* reading with the first phone leg that can show it.

### Built — the member advertisement unblocked: what the build measured

The linux leg's step-3 red was structural, not a share-plane bug: every
advertisement (and custody payload/receipt) send runs the device-owned-epoch
takeover first — a self-`Update` **Commit** — and the nest's then-ratified
claimant-only commit gate refused it from a member, forever. Three ratified
rules were mutually unsatisfiable: this section's member-advertisement
carriage, `devices.md`'s device-owned-epoch invariant, and the claimant-only
gate. **The resolution is owned by [`../architecture/federation.md`](../architecture/federation.md)
§ Cross-nest shared folders + channel append (re-ratified 2026-08-24)**: the
nest admits any *rostered* member's Commit (commit content is ciphertext to
it), and owner-only roster management is enforced member-side by
`MlsEngine::process_commit`'s folder commit policy — a bare self-`Update`
merges from any member; a proposal-carrying commit merges only from the
folder owner. Consequence here: a member seat's first advertisement pass
posts its takeover, the nest logs it, co-members (owner included) merge it,
and the advertisement rides out on the very next application send — the
member leg of this plane (and of the custody ceremony) works without any
change to the share driver. Against a nest predating the re-ratification the
takeover is still refused and the leg degrades to the old
retrying-forever-noisily state — correct across version skew.

### Build design — the row half (B2 scoping pass, 2026-08-17; refutable at build time)

> **Superseded in part 2026-09-29 — the relayed-row lift.** The author-direct rules this pass and the ledger above record (serve own rows only, refuse relayed rows, a pending row served stampless) are the pre-lift posture; the current one is [`p2p.md`](p2p.md) § Peer-served change-row provenance's. In code: the serve filter is `fauna_peer_share::provenance::serves_held_row` (own rows, plus every writer-signed row this replica's reader verified, kept in `relayed_change_log`), the pump's binding-free screen `screen_peer_row`, the engine door's authoritative judge `judge_peer_row` over the shared `fauna_protocol::sync_row_verify::RowReader`; a retained own row — pending included — carries its writer and the signature made over it as served. The overlay, the cached roster (now consulted for the SIGNED actor), the retention and the delete posture below stand.

The provenance ruling above and the serve/pull core's ROW-half finding bound
this pass; it rules the at-rest shapes and the four invariant-preserving
mechanics the build legs implement. Two invariants no leg may lose, restated
from the ruling: **a provisional row never enters the nest-sequenced log
store**, and **a provisional delete never destroys local bytes**.

- **Producer — retention rides the ONE recording funnel.**
  `SyncEngine::record_change` is the single fn every change-recording caller
  passes through (uploads, streaming, deletes, re-seals, thumbnail backfills),
  and its `Ok(seq)` is the moment the row provably exists in the nest's log
  with a known seq. Retention is a write-through at that point into a new
  `SyncDb` table (`own_change_log`): the row as recorded — seq, set, path (+
  sealed sibling), path_hash, manifest_hash, size, change_type, created_at,
  content-key generation, thumbnail, causal stamp — with `sequenced = 1` and
  `locally_authored = 1` **by construction**: only this replica's own records
  pass the funnel, which is exactly the local-origin signal
  `LocalShareChange::locally_authored` demands. The author column is this
  replica's own actor id, self-recorded; a nest echo later carrying a
  different stamp is a store bug the serve filter already refuses
  (`serves_own_authored`: the stamp outranks the local claim).
  **Build refinement (B2.1, 2026-08-17):** retention is UNCONDITIONAL in the
  per-set state DB, not gated on the set being shared — a shared-ness
  predicate at the funnel would couple the hot record path to MLS state, a
  set can become shared after years of history, and the rows are tiny and
  derived-recoverable (the nest's log is the durable copy; losing the table
  costs offline serveability until rows re-record, never data). What stays
  scoped is *serving*: only a set the M2 verdict admits ever reads the
  table. The table still dies with the set on leave/evict — the severance
  duty (walk rule 1) covers it explicitly.
- **The OFFLINE-authored (own-pending) leg is a separate slice, and its
  local-origin rule is fixed here.** An un-sequenced retention row may be
  minted **only** at the engine's own local-change detection — a path whose
  pending state the engine itself derived from local disk (local hash differs
  from the recorded base), with the manifest computed from local plaintext at
  mint time. Never from applied state: a `synced` row in `sync_entries`
  reflects *someone's* change, not necessarily ours, and minting from it is
  the synthesis trap by another door. `record_change`'s `Ok(seq)` upgrades
  the pending row to its sequenced form in place. Sequenced-only retention
  already carries the ratified two-member scenario (author uploads online at
  home, serves offline later); the pending leg is what adds *authored in the
  cabin* on top.
  **Build refinements (B2.5, 2026-08-17):** the mint sits between the upload
  paths' own local seal and their first network call
  (`SyncEngine::seal_for_upload` on the in-memory path; the streaming twin's
  locally-final manifest) — the manifest the pending row names IS the one the
  eventual record carries, because `seal_blob` is deterministic over the same
  bytes and root; and the resident local-write loop reaches `upload_file`
  with no connectivity gate, so a disconnected edit does hit the mint. The
  mint is folder-gated like the record (a folder-less engine's pending row
  could never upgrade) and best-effort like the retention epilogue. At most
  ONE pending row per path — a re-mint replaces, since only the newest local
  state is worth serving. The `Ok(seq)` upgrade retires the path's pending
  row WHATEVER its manifest: a record this replica just landed is its newest
  truth for the path, so this widening over the scoping pass's "same path +
  manifest" also clears a stale pending row that a superseding record (a
  later edit's, or a delete's) would otherwise strand serveable forever. On
  the wire a pending row is `seq: 0`, `sequenced: false` and STAMPLESS — an
  author stamp is a nest fact, and the receiver attributes an un-sequenced
  row to the channel-proven serving peer (`accept_peer_row`) — and it serves
  only on the TAIL page (sequenced results short of `max_rows`), capped at
  `max_rows` total, so a truncated pending set simply reappears at the next
  tail. Deliberately out of scope: an offline DELETE mints no pending row —
  the mint point is the upload paths' manifest computation, a delete has no
  manifest to compute, and the delete's own record retires the path's stale
  pending row at reconnect through the retire-by-path widening.
- **Consumer — the cached writer roster is its own small table, fail-closed
  by absence.** `share_writer_roster` (actor, writer?, refreshed-at; per-set
  by the store's own scoping), read by ingest as the
  `peer_is_cached_writer` bool `accept_peer_row` takes. No row ⇒ `false` ⇒
  rows refused, bytes still served — the ruling's fail-closed arm.
  **Build refinements (B2.2, 2026-08-17):** the WRITER fact is the NEST's
  access model, not MLS membership — `role == "owner"` or the multi-writer
  Phase 1 `access == "writer"` grant from
  `fauna.folders.members.list_actors` (MLS membership answers *admission*;
  writer-ness is what the provenance ruling consults). The refresh rides the
  sync-mode refresh cadence (lifecycle build + resident tick) **plus an
  off-cadence trigger on the control plane's transition into `Connected`
  (2026-08-22 fix)** — build typically runs before the plane is up, so the two
  cadence triggers alone left a hole between build and the first tick that
  happened to catch `Connected`, in which the cache stayed empty; if the nest
  went down inside that hole the empty cache was PERMANENT, since offline the
  refresh short-circuits on the same connected-check. The transition trigger
  closes the hole in milliseconds instead of a tick. The refresh replaces the
  cache wholesale only on a successful read, treats `not_shared` as an
  authoritative empty roster (cleared — an unshared set has no peers), and
  keeps the stale roster on any other failure. Cross-nest foreign sets take
  the same v1 carve-out `resolve_sync_mode` has (skip; stale stands).
- **Materialization commits like a download, but never earns the dehydration
  proof.** An accepted provisional create/modify fetches bytes over the
  existing `BlobFetcher` walk and commits the head the way a nest download
  does — local and recorded hashes equal, so the scan is inert and the
  engine never re-uploads peer content as its own authorship (the
  misattribution door). Two deliberate omissions: **no
  `recorded_content_hash` stamp** — that proof licenses dehydration, and the
  nest does *not* hold these bytes, so freeing local bytes on their account
  is data loss until the nest confirms; and **no log accounting** — anchor,
  path frontiers and edit-frontier are nest-log bookkeeping
  (`conflicts.md` clause 5), and a provisional row has no seq there. The
  overlay row (below) is what remembers the head is provisional.
  **Build refinements (B2.3, 2026-08-17):** the write gate is a pure judge
  (`judge_materialization`) deliberately FAR more conservative than the nest
  apply's licensed folds — it writes only onto a locally-absent path or one
  clean at its tracked state, and skips (overlay row still landing,
  unmaterialized) every ambiguous shape: untracked-but-present bytes,
  disk drifted from tracked state, tracked-but-missing, any non-Synced
  state, and Placeholder rows — materializing a dehydrated row would
  override the storage-policy choice that dehydrated it. The engine's
  ingest door **re-checks provenance itself** against its own cached
  roster (it never trusts that the pump ran `accept_peer_row`), and a
  per-row fetch failure skips that row, never the page.
- **The overlay is a provenance ledger over `sync_entries`, not a second
  fold.** `share_overlay` keeps latest-per-path provisional rows (set, path,
  the peer row verbatim, the channel-proven author, ingested-at). Reads
  annotate from it (provisional badge; a provisional delete hides the path
  from listings while disk and `sync_entries` stay untouched). The fold in
  `apply_remote_changes` never consults it — carrier/covering licensing is
  measured against nest rows alone, which is what "the overlay never changes
  what the converged log says" means in code.
- **Reconcile is retire-on-arrival.** When a nest-sequenced row for an
  overlaid path applies: same manifest ⇒ **confirm** — retire the overlay
  row and stamp the dehydration proof the materialization withheld;
  different manifest ⇒ **supersede** — retire the overlay row and let the
  nest row apply by the existing rules, and the materialized copy is neither
  uploaded nor retained as a conflict loser (it was never this replica's
  authorship — the author's own record reaches the nest from the author).
  Either way the overlay entry is gone and the path is ordinary again;
  reconcile cannot fork because it only ever *removes* the provisional
  annotation.
  **Build refinements (B2.4, 2026-08-17):** the sweep runs at the END of
  `apply_remote_changes`, after the fold and consulting none of its
  licensing, and it is deliberately **ungated** — overlay rows are written
  only by `p2p-share` builds, but any build of the engine (the headless
  daemon included) must retire them when the nest speaks, or a mixed
  deployment strands provisional annotations and blocked dehydration
  forever. A CONFIRM keys on the path's **batch-latest content row below
  the apply caps** (deferred rows have not applied and must not retire
  early; retention rows are invisible to content and never name the head)
  matching the materialized manifest, with the entry still tracking those
  bytes. And materialization saves the **merge base** exactly as the
  nest-download apply does — without it the nest's later row reads the
  materialized state as unpublished local work and mints a bogus conflict
  instead of the clean supersede.

Build legs, in order (each re-verifies its slice against code first): retention producer
→ writer-roster cache → overlay ingest + materialization → reconcile +
provisional delete + the success-criterion tier_1 trio → the own-pending leg.
**All five landed 2026-08-17 (B2.1–B2.5)** — the row half is built end to
end, offline authorship included.
