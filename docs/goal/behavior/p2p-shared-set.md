# Cross-user shared-set transfer — target state

Owns: p2p-shared-set, offline-share-initiation, delivery-seat
Status: ratified 2026-08-10 (user-directed) and partly built — the plane's contract and its rulings are target state, refutable at build time; § Offline share initiation is designed (PQ-1, resolved 2026-08-17) and refutable until its first build; what is built is recorded in the build ledger of [`p2p-shared-set-build.md`](p2p-shared-set-build.md) and in [`p2p.md`](p2p.md) § Implementation status today. Split verbatim out of [`p2p.md`](p2p.md) on 2026-11-04.
Authority: **the cross-user shared-set transfer plane** — the offline, BitTorrent-like scenario and its contract points, the transfer row of a pull that failed, the stale-endpoint ruling for a departed member's cached endpoint, **offline share initiation** (the co-present ceremony, the storage half born as recipient-set group scopes, the messaging half on the **delivery seat** — the per-MLS-group sequencing seat as a named, re-pointable role), peer-served change-row provenance, and the wormability walk of the share leg against the eight rules. **NOT owned here** — the plane's build contract, build ledger, row-half build design and phone-peer design → [`p2p-shared-set-build.md`](p2p-shared-set-build.md); the group set → [`group-set.md`](group-set.md); the transport seam, the relay, inbound authorization, LAN detection, the wormability posture, per-device participation, the `p2p` page and everything else about P2P → [`p2p.md`](p2p.md); the scheme detail → [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md). On conflict with [`p2p.md`](p2p.md) § Wormability posture, the posture wins.

## Implementation status today

Not restated here: the plane's build state lives in [`p2p.md`](p2p.md) § Implementation status today (its cross-user paragraphs) and in the build ledger of [`p2p-shared-set-build.md`](p2p-shared-set-build.md).

---

Split verbatim out of [`p2p.md`](p2p.md) § Cross-user shared-set transfer on 2026-11-04, when that doc reached 241,572 B (92% of the 262,144 B whole-file read ceiling). Nothing was reworded. **Pointers in the text below keep their original meaning:** a bare `§` pointer, or an "above"/"below", naming a section this doc does not hold — § Implementation status today, § Transport seam, § The relay, § Inbound authorization, § LAN detection, § Element IDs, § Wormability posture, § Per-device participation — resolves in [`p2p.md`](p2p.md); one naming a section of this doc resolves here; the *Built* records, § Build contract, § Build design and § Phone peers are one-line stubs pointing on to [`p2p-shared-set-build.md`](p2p-shared-set-build.md).

## Cross-user shared-set transfer — offline, "BitTorrent-like" (target state, ratified 2026-08-10)

**Scenario (user directive 2026-08-10):** two users — e.g. spouses, each with
their own account — move a large set of files (holiday videos, tens–hundreds
of GB) between their devices **offline over p2p**: chunked, resumable,
integrity-checked, multi-source. Analysis + the full internal BitTorrent
comparison: the 2026-08-10 p2p content-sharing considerations doc
(`2026-08-10-p2p-content-sharing-considerations.md`, internal plans tree —
tracked internally, not shipped; frozen). This section is the contract and
wins on divergence.

This is a **byte-path optimization under unchanged custody** — the
cross-user twin of the account plane's same-account peer leg
([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
§ The peer leg). The sharing mechanism stays exactly M2 shared folders
([`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
§ Audience: an MLS group): membership, keys, rotation, and the chunk/manifest
pipeline ([`file-sync.md`](file-sync.md)) are unchanged; only where the
sealed bytes travel changes. (A set born in person is the one exception, by
design: it is keyed by the recipient-set scheme and its path state is plane
data — § Offline share initiation, built as
[`group-set.md`](group-set.md) § Build design — the
group set; the byte path below is shared by both families.) Contract points,
refutable at build time:

- **Precondition: the set is already shared** (Welcome delivered, envelope
  ingested) while connectivity existed. Offline share *initiation* is out of
  v1 scope for this M2-keyed path — **permanently, by design**: the
  offline-initiation path is a plane group scope, never a retrofit onto MLS
  folder keying (§ Offline share initiation below — PQ-1, resolved
  2026-08-17).
- **Identity + admission.** Contact-plane pairs dial by **actor key**
  (PT-1b, § Transport seam); admission above the seam is same-group
  membership. The content keys are themselves the confidentiality boundary:
  every transferable byte is M2-sealed ciphertext, so a wrongly-admitted
  peer can fetch nothing it can read (the membership check is the front
  door, the seal is the wall). The wall is as current as the serving host's
  last read of the set's content-key floor: a file authored while that host
  cannot reach a nest is sealed under the generation it holds, so a person
  removed since that read, and still admitted by a roster that has not
  heard either, can open it until the host reconnects — the rule, its
  nest-side guarantee and why no time limit is set are owned by
  [`on-demand-files-shared-sets.md`](on-demand-files-shared-sets.md) § Shared sets on a capability
  host, decision 2′ (ruled 2026-10-01).
- **Data plane.** The puller reads the set's change-log delta + manifests
  over the Y.1 `PeerChannel`, computes a want-list of chunk store-keys, and
  pulls chunks — the same latest-per-path fold and the same
  ciphertext-hash-addressed chunks the nest plane serves. Every chunk
  self-verifies (store key = hash of body; AEAD tag under the set's
  generation key) — the F9 anti-poisoning twin, peer-side.
- **Multi-source by construction.** The convergent M2 seal means owner
  devices, member devices, and the nest hold **byte-identical** chunks: any
  subset serves any part of the want-list concurrently; an interrupted
  transfer resumes from any other source (nest included, when connectivity
  returns) with held chunks never re-sent. No rarest-first/tit-for-tat
  machinery — those solve open-swarm stranger problems this closed,
  cryptographically-admitted set does not have (full table: the plans doc).
- **Discovery.** LAN candidacy by the shipped arithmetic (§ LAN detection);
  non-LAN via the relay (rendezvous, and the encrypted packets themselves
  when no direct path exists — never a party to the data) for two members
  whose accounts live on one nest — once the relay serves address
  discovery, which is when the seat takes it; until then the seat binds
  relay-less and such a pair pulls from the nest (§ The relay → *The
  cross-user seat and the relay*); two whose accounts live on different
  nests have no relay and pull from the set's home nest (§ The relay →
  *Across nests*); peer
  endpoints learned only through authenticated channels. **No global DHT,
  no scanning, no stranger-servable content** — deliberately: sharing
  reaches exactly the people the owner picked, which is both the privacy
  posture and the structural answer to open-swarm abuse (the policy layer
  on top: `p2p-share` is a charter member of the controversial-class
  feature registry — quota-shaped gating + excision, owner
  [`../architecture/dynamic-features.md`](../architecture/dynamic-features.md)).

### The transfer row of a pull that failed (ruled 2026-10-24; readings user-approved and built 2026-11-01)

**The question.** The pull pump reports one `share-transfer-item` row per `(set, peer)` pull that *returned* (`share_pump::SetPullOutcome`), and the paint cell holds the latest pass's rows only (`share_glue::SharePlaneState::outcomes`, replaced wholesale by `update_cell`). As built on 2026-10-24 a pull that fails — the dial, the admission handshake, or a page fetch or the door after admission — is logged at debug and returns no outcome, so one pass that cannot reach a member wipes that member's row, straight after a pass that delivered, while `share-serve-status` keeps reading "Serving N shared folder(s)". Seen once on macOS: the file had arrived, then 240 s with no row. Rule 5's transparency line and Dim-3's honesty (§ Wormability walk, rule 8: a bound names what set it, never a silent stall) rule on the refusal and the cap readings and on the status line, and were silent on this.

**Ruling: every dial target a pass attempts yields exactly one row, success or failure; a failed pull keeps its row and reads the failure honestly.** The row is the unit the user reasons about — this folder, with that member's device — and a device that could not be reached is a fact the user can act on (it is off, on another network, behind a path with no relay), where a row that comes and goes between passes is noise that reads as a defect. So `SetPullOutcome` records that the pass's pull ended in an error, and the row stays with the counts this pass actually moved (zero for a dial that failed; what landed before the cut for a pull that broke after admission — the progress reading already says "this pass"). The failure takes one of two readings, derived from whether admission had completed when the pull failed: **unreachable** — the dial or the admission handshake failed, nothing was exchanged — and **interrupted** — the peer admitted this device and the pull failed after that (a page fetch, the door, the set's state DB). The two are kept apart because they tell the user different things: the first that their device is not reachable from here, the second that it is and the transfer did not finish this pass; both retry next pass, as today. The text, user-approved 2026-11-01 (the element's enumerated readings are user-approved — ui.yaml `share-transfer-state`): "Could not reach their device" (`folders.share_transfer_state_unreachable`) and "Connection lost; retrying" (`folders.share_transfer_state_interrupted`).

**Precedence on the state reading.** The transfer gate's "limited by …" (a refusal, or a capped file) stays first — a bound the user set, or a cap, is the most actionable fact and is already rendered honestly — then free space, then the failure, then admission pending, pulling, up to date (`share_glue::transfer_state_label` is the one reading, consumed by every app through `fauna-ffi`'s share-plane view and the tui and linux glue).

**What this does not rule.** Which targets a pass attempts is the dial-target read's rule (`AccountStoreHandle::share_dial_targets`, the cached `fauna.state.share-endpoints` rows), and what becomes of a cached row whose member has left the set is § *A departed member's cached endpoint* below (ruled 2026-11-01, built the same day): a departed member's row is not dialed from the pass after this replica's roster drops them, so it leaves the surface. Two alternatives were weighed and refused. Keeping the previous pass's outcome per (set, peer) and marking it stale: its counts would read "this pass" about a pass that moved nothing, and a row that must age out needs a clock this surface does not have. Recording nothing — today's behavior, written down: an unreachable member is exactly the fact rule 5's line exists to tell.

**Implementation status today: BUILT 2026-11-01.** `share_pump::pull_target` (the pass's per-target leg) keeps the outcome of a failed pull with `SetPullOutcome::failed` set and the counts moved before the error; `share_glue::transfer_state_label` reads it unreachable or interrupted in the ruled precedence; witnesses `share_glue::tests::a_failed_pull_reads_unreachable_or_interrupted_in_the_ruled_precedence` and `share_pump_two_seats::an_unreachable_member_keeps_their_row_after_a_delivering_pass`. With the row kept, the e2e barrier that waits for a `share-transfer-item` after a delivering pull (`test_share_pump_two_actor.py`) no longer loses the race to the next pass. A departed member's endpoint row is § *A departed member's cached endpoint*'s (built 2026-11-01).

### A departed member's cached endpoint — the stale-endpoint ruling (ruled 2026-11-01; refutable at build time)

**The question.** A `fauna.state.share-endpoints` row is this fleet's cache of where one member of one set was last reachable — one row per (set, member actor), written by the sink on the member's advertisement over the set's own channel (§ *Discovery*; `fauna_core::share_endpoints`), superseded by their next advertisement from whichever of their devices sends it, and read back as the pass's dial targets (`fauna_peer_sync::discovery::share_dial_targets_from_rows`). Nothing retires one: the sink only puts, and no membership change touches the kind (checked 2026-11-01), so a member removed from the set, or who left it, stays a dial target on every replica of this fleet for ever, and since § *The transfer row of a pull that failed* every pass mints a row for them reading "Could not reach their device". The same-account leg met the same question first — a removed device's `device-endpoints` entry outlives its device, because nothing forgets a merged entry — and answered it at the read: an entry is a dial candidate only while its device is a verified member of the replica's fleet view ([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg → *Discovery*; `sibling_dial_targets`' `verified_member`). This ruling is that answer applied across users, by priority #3.

**Ruling 1 — the set's roster, as this replica holds it, decides what is dialed, and the decision is taken at the dial-target read, not at an event.** A cached row is a dial target only while the set's roster lists its actor, per the SAME consult admission runs on (an M2 set's MLS roster through `SetMembership::is_member`, which the pump already holds; a group set's roster through the group roster evaluator the glue already lends): `share_dial_targets_from_rows` takes the consult exactly as `sibling_dial_targets` takes `verified_member`, and a row the roster does not vouch for is skipped before the dial, for the sibling rule's reason — admission would refuse that peer after the dial; this refuses it before, spending no dial and showing this device's address to nothing that answers at that node id. Three consequences. **(a)** The transfer surface drops the row at the first pass after the roster change, by construction: a target not dialed yields no outcome, and the surface is the pass's outcomes (§ *The transfer row of a pull that failed*) — nothing on the surface is marked, aged or filtered. **(b)** Roster lag is handled by the same read: a Remove commit this replica has not yet merged keeps the row dialable, and the moment the commit merges — on a device that was offline through the removal too — the row is skipped, with no event to have missed; the reading is exactly as current as this replica's roster, which is what decision 2′'s wall is for the serve side ([`on-demand-files-shared-sets.md`](on-demand-files-shared-sets.md) § Shared sets on a capability host). **(c)** A consult that cannot read its roster answers "not a member" (fail closed), the pass dials nobody for that set, and the next pass retries — a transient, never a deletion (ruling 2).

**Ruling 2 — the row is then tombstoned, as hygiene, on a POSITIVE reading only.** The row is a counterparty's location data (the kind's fleet-only rung exists for that), and a departed member's last addresses kept on every replica for ever is retention nobody chose, re-sealed at every generation reclaim. So the kind's registration stands as written — "a leave or evict removes the row by stamped tombstone" (`fauna_protocol::merge_policy`, `KIND_SHARE_ENDPOINTS`): the pass that skips a row of a set it HOLDS, whose roster it could READ, and which does not list the actor, writes a stamped tombstone at the row's `(kind, key)` (`tombstone_lww_row_local`, the follows and atproto rows' deletion), the account runtime's publish step carries it to the fleet, and the generation-reclaim walk already hands a tombstone of this kind over as a tombstone. The reading must be tri-state: `SetMembership::is_member`'s boolean is right for ruling 1 (a pass that cannot read dials nobody and tries again) and wrong for a deletion (an engine error would erase every row), so the sweep asks "held, readable, and not listed", and an unreadable roster tombstones nothing — a widened consult or a second seam, the build's choice. A row of a set this replica no longer holds as a shared set at all — it left, or was removed, and the folder's engine binding is gone — is outside every pass's read, and is tombstoned by the positive local event that drops the binding, never by a pass that merely does not hold the set.

**Re-admission.** A member removed and admitted again is dialed from their next advertisement (their advertise state is per process: at the floor, on an endpoint change, or on a restart — `share_pump::ADVERTISE_FLOOR_SECS`), whose put this fleet's sink stamps later than the tombstone this fleet wrote; until then the set has no row for them — absent, not "unreachable", because there is nothing to dial until they say where they are — and the nest path carries the set.

**Refused.** (1) An expiry of the advertisement: the surface has no clock, the same refusal § *The transfer row of a pull that failed* made; a member whose every device is off for a month is still a member, "Could not reach their device" is the honest reading of that, and the floor re-advertisement keeps the candidates current. (2) A tombstone written from a roster-change EVENT (a hook in the conversations rail): a replica offline through the removal never sees the event as an event, only as merged state, and the consult IS that state — an event-driven prune would need the consult anyway, as its heal. (3) The removal of one of the member's devices from THEIR account as a trigger: the row is keyed by actor, not device — one row per (set, member) — their remaining devices' next advertisement supersedes the candidates by LWW, and a member with no device left reads unreachable, honestly.

**Implementation status today: BUILT 2026-11-01.** Ruling 1: `fauna_peer_sync::discovery::share_dial_targets_from_rows` takes the roster consult (`is_member`, over the member actor) and returns a `ShareDialRead` — the targets, and the well-formed rows the consult did not vouch for; the pump's per-set targeting `share_pump::set_dial_targets` passes `SetMembership::is_member` for the set, so `pull_pass` neither dials nor mints an outcome for an unlisted actor. Ruling 2: the seam gained a tri-state `SetMembership::roster_reading` (`Listed` / `NotListed` / `Unreadable`, defaulting to `Unreadable` so a fake or a forgetful wrapper deletes nothing); the production `MlsSetMembership` answers `NotListed` only for a group the engine holds and still sits in (`is_group_active` — an evicted device's frozen roster vouches for no absence), `RosterMembership` only when `holds_group` and `contains_member` both answer; the pump's sweep tombstones an unvouched row on `NotListed` alone, through `AccountStoreHandle::tombstone_share_endpoints` (`custody_rows::tombstone_share_endpoints`, live rows only). Witnesses: `discovery::tests::a_row_the_roster_does_not_list_is_not_dialed`, `account_runtime::tests::a_departed_members_share_endpoint_is_dropped_then_tombstoned_on_a_positive_reading` (dropped, unreadable-keeps, tombstoned, re-admitted by a fresh advertisement — over a real account runtime, since the two-seat pump file holds none), `mls_adapter::tests::roster_reading_is_not_listed_only_on_a_held_active_group`. The group family needs no consult here: `share_glue` partitions group sets out before `pull_pass` (the group leg dials from its own endpoints cell, never a `share-endpoints` row). Ruling 2's last sentence, the LEAVE arm: `FaunaMlsBackend::leave_folder`, after a forget that succeeded, calls `FolderCustodySink::drop_share_endpoints` (`NestFolderCustodySink` → `FolderKeyStore::drop_set_share_endpoints` → `PlaneFolderKeys` → `AccountStoreHandle::tombstone_share_endpoints_of_set`, `custody_rows::tombstone_share_endpoints_of_set`: every live row whose key starts with the set's channel hex, answering the count; web crosses the account port as `folder_keys.drop_set_share_endpoints`), so a voluntary leave tombstones the set's cached rows on every app, and a failed leave issues nothing. Witnesses: `account_runtime::tests::a_left_sets_share_endpoints_are_tombstoned_and_no_other_sets`, `fauna_mls_backend_tests::a_completed_leave_drops_the_sets_share_endpoints_once_and_a_failed_one_none`, `port::tests::the_set_share_endpoints_drop_crosses`. The REMOVAL arm: `poll_inbound_folder`, after every walk (a clean drain, a stall, a held commit — a retired engine excepted), calls `FaunaMlsBackend::drop_share_endpoints_if_evicted`, which issues the same `FolderCustodySink::drop_share_endpoints` for a set the engine still HOLDS but whose group is no longer active (`is_group_active` false — the merged commit removing this device), once per session per channel (a RAM-only set, so a relaunch over the evicted group's restored snapshot drops once more on its first walk, healing a crash between the merge and the drop; the store door answers 0 on a repeat); a set the engine does not hold drops nothing, and a remaining member merging the same commit drops nothing. Witness: `fauna_mls_backend_tests::a_processed_removal_drops_the_sets_share_endpoints_once_and_a_never_held_set_none`.

### Offline share initiation (PQ-1 — resolved 2026-08-17; design, refutable until first build)

**The question:** a *first* share with no nest reachable needs identity
exchange, Welcome + envelope delivery, and roster/claim writes that are
nest-arbitrated today — plus reconcile-on-reconnect that cannot fork the
group. **The resolution splits along the R15 (account-data-plane.md § The ratified decisions) seam**
([`../architecture/account-data-plane.md`](../architecture/account-data-plane.md)
§ The ratified decisions R15 — storage keying vs messaging crypto): offline
*storage* shares stop being an MLS problem entirely, and the messaging half
gets a delivery-*seat* design instead of a delivery-service substitute.
Build state: § Implementation status today (the ceremony's
carrier-agnostic payloads, admission-bundle doors, state machine, and the
bind door are code as of 2026-08-17; the affordance's shared paint decision is
code too and every app with the ceremony consumes it, § Implementation status
today; the whole messaging half is not built).

**1. Storage shares are born as group scopes, never as offline M2 folders.**
An offline-initiated shared file set is created as an account-plane **group
scope** under the T20 recipient-set scheme (random scope generation key
HPKE-wrapped to each member; no MLS epochs, no delivery service, membership
arbiter-free by T20 constraint (a)) — so initiation needs no nest **by
construction**. The shipped M2 path keeps its online-initiation precondition
permanently: offline initiation is never retrofitted onto MLS-keyed folders;
instead the D4 convergence direction (file-sync onto scope families, charter
§ Substrate settlements) gains its first named consumer. Contract points:

- **The initiation ceremony is two-party and co-present**, over the
  contact-plane peer channel (PT-1b actor-key dial), with an in-person
  key-verification affordance (QR/short-code compare — build detail, ships
  with the ceremony; **element IDs minted + rule-A user-approved 2026-08-17**:
  ui.yaml `folders` page `offline-share-*` / `offline-receive-*`, the
  consent card reusing the knock trio `folder-pending-share` +
  accept/decline; QR is a later per-app rendering of the same code
  elements, and the born set is unnamed in v1 — naming kinds arrive with
  group content-kind sealing). One exchange
  delivers: mutual actor-key verification, the scope offer + accept, the
  recipient's wrap (T20's scheme), and the initial manifest/frontier
  handoff. Precedent for the offer/accept shape: the custody ceremony.
  - **The compare code carries the addressing, not just the identity
    (RESOLVED 2026-08-19 — user decision; supersedes this point's original
    "LAN candidacy by the shipped arithmetic, § LAN detection").** The first
    build measured the consequence of a key-only code: nothing could dial
    (*Measured — the co-present dial has no discovery*, below). The
    resolution is **(b) of the three that bullet named — widen the
    co-present payload** to carry this device's bound LAN endpoints, so
    **the code itself is the advertisement channel** a nest-free pair
    otherwise lacks. Consequences, each load-bearing:
    - **"Compare the code" now means a *transcribed* code, not a spoken
      one** — this is the ratified-contract change the widening costs. The
      code is the 64-hex actor key, then `-`-separated 12-hex candidates
      (4 octets + 2-byte port). **Not QR-first:** tui is the lead app and a
      terminal has no camera, so the typed/pasted form is the one that must
      work; QR stays exactly what this point already called it, a later
      per-app rendering of the same elements.
    - **Additive** — a bare 64-hex key still parses, carrying no addressing.
    - **At most 4 candidates, IPv4 only** (`MAX_CODE_CANDIDATES`): the code
      is transcribed by a human, so its length is a UX budget; IPv6 would
      cost 32 hex characters per candidate and § LAN detection's arithmetic
      is IPv4-only anyway. The dial needs only one candidate to be reachable.
    - **A garbled candidate refuses the whole code** rather than being
      dropped — the module's repair-vs-refuse posture, and it surfaces the
      typo while the other person is still standing there.
    - **No new dependency, and no new *admission* surface**: the candidates
      are peer-supplied and ride the *existing* PT-4 `is_safe_candidate`
      filter inside the transport's `dial`, so a poisoned candidate is at
      worst a no-op — and *who* may start a ceremony is still the receive-act
      expectation, untouched by this. Options (a) a local-discovery crate and
      (c) the nest's relay were both refused — (a) costs a third-party tree
      in a shipped artifact across 7 apps (rule 1), (c) contradicts the
      nest-free claim.
    - **What it does newly disclose, stated rather than glossed:** the code
      now reveals this device's private LAN addresses to whoever holds it.
      Judged acceptable because the code is exchanged in person and never
      sent (the affordance's own help string says so), the addresses are
      RFC-1918 and meaningless off that network, and holding them grants
      nothing — admission is the receive act, not reachability. It is
      nonetheless a reason the "never send it in a message" instruction is
      now *more* load-bearing than when the code was a bare key, and a
      reason not to render this code anywhere it could be screenshotted into
      a support thread.
    - **Where the addresses come from:** the *assembler*, never the seam.
      `fauna_iroh::peer_leg_transport` already returns
      `(Arc<dyn PeerTransport>, Vec<SocketAddr>)`, and the seat records it
      via `CeremonyNode::with_bound_addrs`; the `PeerTransport` trait keeps
      its deliberate silence about bound addresses (`peer_leg` module docs).
      The bound-sockets × interface-addresses cross is the shared
      `fauna_core::device_endpoints::lan_socket_addr_candidates`, the same
      one the nest-published peer-leg facts use. **Only an IPv4 socket's
      port pairs with an IPv4 address:** the endpoint binds a `[::]` socket
      beside the `0.0.0.0` one, on its own port, and where that socket is
      v6-only (the Windows default) an IPv4 address carrying its port has no
      listener — every send to it draws a port-unreachable back.
    - **Deliberately still open:** the code has **no checksum**, so a
      mistyped key fails as a failed dial rather than an immediate refusal.
      That is pre-existing (a 64-hex key was always transcribed), and one
      contract change per pass; revisit if transcription errors show up in
      practice.
    - In code: `fauna_client_capabilities::group_ceremony_view::{PeerCode,
      format_peer_code, parse_peer_code}` and
      `group_ceremony_node::{with_bound_addrs, local_endpoints, dial_code}`.
      Proof: `group_ceremony_node.rs`'s
      `a_ceremony_dials_on_the_addressing_its_compare_code_carried`, which
      dials through a transport that refuses any dial whose candidates do
      not name the listener — with the key-only dial asserted to fail, so
      the test cannot pass for the wrong reason.
- **Roster and claims are plane data in the scope, not nest writes.**
  Membership entries ride per-writer logs and frontier-merge like any
  class-2 kind (charter § Ordering model); a member's nest first learns of
  the scope on next contact and ingests as an ordinary custodian replica —
  claim-time arbitration does not exist on this path.
- **Fork-free reconcile is by construction, with the lattice left to T20:**
  there are no epochs to fork — per-writer logs converge under frontier
  merge; concurrent *adds* must converge as wrap-set union within a
  generation, and any *remove* mints a new generation (R14 composition).
  The precise wrap/roster lattice and the per-member severance rule are
  designed (T20 resolved 2026-08-17 — charter § The audience ladder → The
  recipient-set scheme; key material:
  [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
  § Audience: a storage group).
- **Admission for the transfer plane is a fourth witness kind** —
  group-scope membership, evaluated against the channel-proven actor key
  and yielding exactly the scope, through the same verdict-shaped core
  (charter § The admission seam, which registers the witness kind). The
  witness *form* is designed (T20 resolved 2026-08-17): the member's
  `Enrolled` roster entry plus its authoring chain to the authority actor
  root — charter § The recipient-set scheme, membership-witness bullet.
- **Built — the ceremony carriage (2026-08-17; refutable
  by the security review).** Three `fauna.peer.share.ceremony.*` kinds —
  offer push, accept poll, deliver push — ALL initiator-originated over
  one dialed channel: the accept side of a peer node serves and never
  originates, so the recipient stays purely reactive and the consent gap
  is the poll's pending (`None`) answer, never a hang; a declined
  invitation answers refused, terminally. **First-contact admission is the
  receive-act expectation** (the build decision rule 1 demanded): the
  recipient's own "receive a share" act, after the in-person compare,
  mints a device-local, single-actor, 15-minute-TTL expectation
  (`fauna_client_capabilities::group_ceremony_peer`) that admits exactly
  the scanned initiator's ceremony frames **before any payload parsing**;
  from offer-recorded on, the durable ceremony record itself admits its
  initiator, so a dropped co-present connection redials without
  re-scanning — **scoped to that record's own ceremony (2026-09-27):** the record takes frames only on its own
  `scope_id` and only while the ceremony is live (not declined; not yet
  delivered, or delivered within
  `GROUP_CEREMONY_DELIVER_RESEND_GRACE_SECS`, so the initiator's
  byte-identical re-send of a deliver whose ack a drop lost is acked and
  records nothing new); an offer for a scope with no record needs a live
  expectation, and at most
  `GROUP_CEREMONY_MAX_PENDING_INVITATIONS_PER_INITIATOR` un-consented
  invitations per initiator are held. The pre-decode gate names only the
  actor; the per-scope check runs after decode in
  `GroupCeremonyPeer::ingest_frame`. Only an *invited* record admits:
  every leg is initiator-originated, so an actor this account shared TO
  pushes nothing to its listener, and an accept frame pushed there is
  refused. This is
  § Inbound authorization's stronger-per-kind-admission carve-out applied
  to the ceremony family — an edge both endpoints chose. The eight-rule
  walk for the new listener surface: (1) the expectation/record gate runs
  before decode, pinned by a test that a refused actor's bytes never reach
  the seam, and fail-closed when no ceremony state is wired; (2) the
  wrapper structs, the frame enum, and the inner signed payloads all sit
  in `KIND_PAYLOAD_COVERAGE_P2P_SHARE` with real-signed corpus entries,
  both ties pinned; (3) the kinds joined `allowlisted_kinds()`, asserted
  as the whole set — the deliver's admission wrap is key material only as
  sealed end-to-end ciphertext to the recipient's reception key (the T20
  rail, the group twin of the M2 rail), opaque to every parser on the
  path; (4) every frame is signature-verified against the channel-proven
  sender at ingest, and a forged snapshot row faces ordinary first-contact
  adoption strictness; (5) the kinds ride the same `p2p-share` excision
  and the `fauna.peer.share.` grep family; (6) expectations expire and
  cancel, and a record admits only its own live ceremony — a decline, or
  the re-send grace running out after the deliver, withdraws it; (7) they serve only
  through the same brake-gated listener as the rest of the share set — no
  new bind door; (8) ceremony requests are metered by the same per-peer
  ledger, refused attempts included. The transport half is proven end to
  end over a real in-memory channel (the capstone re-run over the wire,
  `group_ceremony_over_wire.rs`).

- **Built — the affordance, both roles (tui, 2026-08-17/18; tui leads per
  the ordering rule; linux 2026-08-20, row 334).** The initiator half and the
  eight approved element ids landed 2026-08-17; the recipient half landed
  2026-08-18, once the group plane's store existed for its machinery to land
  in. Linux's leg landed as a shared-logic commit followed by the panel /
  consent-card / group-scope-row UI wiring, reusing the same eight ids and
  the existing knock-trio/folder-row ids — no new element ids. What the surface is:
  - **One seat per session, bound by whichever door asks first — never at
    login.** An actor-keyed endpoint for a feature most people never touch
    would be waste at login. The seat comes up when a panel opens (the
    co-present user's explicit "I am doing this now") or when the share
    plane's driver — started at the account-store-ready edge, § Cross-user
    shared-set transfer — first has a set to serve under an admitting brake,
    whichever is first; the other door is handed that seat back, so a session
    never holds two listeners on one NodeId, in either order or in a race.
    The door that binds reads the brake at that moment, so the *ceremony
    itself* then runs with no nest involved at all, which is the substantive
    offline claim. Every seat binds share-plane-capable over the session's own
    roster, which admits nobody until the plane's driver lends it the M2
    consult and nobody again once that driver ends: a seat the panel bound
    serves no set before the plane would have bound its own, and a ceremony in
    flight on it is never displaced. Built as
    `fauna_sync_engine::offline_share::SessionSeat`, the slot both doors bind
    through on tui and linux (2026-09-15) — it replaced two opposite per-app
    folds (tui replaced the panel's seat, linux kept it) that both ran after
    the driver's own listener was already up. The FFI apps' panel door binds
    through the same slot since 2026-09-24 (`fauna-ffi`'s process-wide,
    actor-keyed `SessionSeat`, emptied at the account runtime's teardown);
    before that it bound its own node outside any slot. (A cold start with no nest ever
    reachable still cannot bind through the panel's door — it has no cached
    capability read.)
  - **The seat's record is lent late — the bind never waits for the account
    runtime (ruled 2026-09-28; the lead E3
    slice's e2e refuted the opposite).** The ceremony record rests on the
    account plane (`fauna.state.group-share-ceremony`, one row per ceremony —
    [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md)
    § The `__config` dissolution schedule, the kinds table), whose store
    exists only once the account runtime is assembled, seconds after
    sign-in; the panel opens whenever the co-present user opens it, and the
    two-seat journeys open it before that edge. A seat that required the
    store's handle at bind refused the panel with "this device has no account
    runtime yet" and the compare code never appeared. So the seat binds over
    an **empty in-memory replica** and the record is **lent to the session
    seat at the account-store-ready edge**, exactly as the M2 roster consult
    is (*One seat per session* above): one shared call on the slot
    (`SessionSeat::lend_record`), the lent handle held `Weak` — the account
    driver's command channel closes when its last handle drops, so a strong
    hold in a panel's seat would keep the runtime from closing — and the
    lend runs one flush at once, both directions of the record's own join
    (the replica into the store, the store's rows back into the replica).
    Who lends is the runtime's host, never the share-plane driver: tui and
    linux at the edge that also starts the driver, `fauna-ffi` at its
    account runtime's start — iOS hosts no share plane and still
    runs the ceremony (windows calls the plane's start from its sync-agent session install since 2026-10-10). Until the lend, the compare code, the listener, the receive
    act, frame ingest and a decline run on the replica, and the persist
    task's flushes are no-ops the lend replays; the **two acts that need
    account material keep taking the handle and refuse plainly without
    it** — `initiate` (the authority carriage) and `consent` (the reception
    key, then the held-root and machinery rows) — which is the shape both
    have had since 2026-09-24. The page read (`load_group_shares`) keeps its
    seat fallback with a new reason: lent, it answers the durable fold
    joined with the replica (the durable copy wins, so a seat's view never
    masks what another device recorded); unlent, the replica. **The honest
    bound:** a frame ingested before the lend rests in memory only until it;
    a crash in that window (the seconds between sign-in and the store's
    assembly) loses the ingested offer, and the initiator's redial then
    needs a fresh receive act. Nothing a user made is at stake before
    consent, and consent itself needs the runtime.
  - **The consent card mints NO ids**: it is the knock trio
    (`folder-pending-share` + accept/decline) with a second source, so the two
    lists are ONE indexed family and a user sees one list of things awaiting an
    answer. Both gestures address the **scope id**, never the row position — an
    offer can land while the user's finger is moving, since the listener
    ingests it without asking the page.
  - **Consent is one act**, because it is one decision: the reception keypair is
    minted and rested, the accept recorded, the deliver awaited under a named
    generous budget, the admission run, and the machinery written through. Three
    orderings inside are load-bearing: the reception key rests *before* the
    accept (its public half rides the accept and the bundle seals to it — a
    crash between the two must leave an unused key, never an unopenable
    delivery); the held-root row lands *before* the machinery rows (which seal
    under it); every monotone marker is set *after* its write returned.
  - **A landed scope lists as an ordinary set row** — no new ids, since a
    ceremony-born set is a set — badged with who shared it, and read from the
    **store's** group rows rather than the ceremony record: a recorded-but-never-
    adopted scope is silently absent, never painted as a set the device cannot
    read. A group row carries the same location form as a folder row
    ([`group-set.md`](group-set.md) § Build design — the group set, decision 7),
    no name until one is recorded (a recorded `fauna.group.name` labels the row,
    and no app writes one yet — the row shows the short scope id meanwhile) and
    no leave button (severance is the authority's mint —
    [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md)
    § The recipient-set scheme).
  - **The driver seam is shared, not per-app**: `AccountStoreHandle`'s four
    group doors (`put_group_held_root`, `put_group_reception_key`,
    `adopt_group_rows`, `group_scope_states`), the joiner's walk
    (`group_ceremony_node::{consent_to_group_share, decline_group_share,
    await_delivery}`), the consent-card projection
    (`group_ceremony_view::pending_group_invitations`) and the listing
    projection (`fauna_sync_engine::group_scope_view`). The four-app trickle-down
    (windows, macos, ios, android — web is a declared absence, § *Wormability
    walk* rule 5) consumes these rather than re-deriving them: windows/macos/ios
    through the UniFFI boundary (`libs/fauna-ffi/src/offline_share.rs`, landed
    2026-08-21), android through the same boundary — its own account-runtime
    prerequisite was met 2026-08-22.
  - **Proof:** `tests/e2e-unified/tests/test_offline_share_two_seat.py` — two
    seats, two accounts, one machine, the whole ceremony through two UIs, ending
    on *the recipient's folders page lists the shared set*; plus the decline
    leg's two independent halves (the card stops knocking AND nothing is
    adopted) and the uninvited-initiator refusal. The `ceremony_seats` fixture
    is parametrized over tui/linux (2026-08-20, row 334) rather than
    hardcoded to tui, so linux's leg is proven the same way.
  - **v1 stops at ADMISSION, not at bytes.** The machinery is adopted and the
    scope lists; content transfer waits on the group serve door, which still
    checks the folder family only (§ Cross-user shared-set transfer; the group
    content-kind sealing slice). **That slice is designed (2026-10-29):**
    [`group-set.md`](group-set.md) § Build design —
    the group set owns it — path state as `GenerationTip`-sealed entries of
    the scope, bytes over the share doors under the scope id, the feed on
    the share serve set, a location bound under the scope through the
    agent; its plane half is built, the feed and the seat are not — its
    status line there.
  - ⛔ **And it does not run yet at all** — the dial has no addressing
    information. See the next bullet.

- **Measured — the co-present dial has no discovery (2026-08-18, RED).** The
  two-seat journey drove the whole ceremony through two real apps and the
  initiator's own panel reported
  `could not reach them: transport i/o error: iroh connect: No addressing
  information available`. **The affordance, the walk, the refusal path and the
  paint are all correct — nothing can reach the counterpart.** Three facts,
  each verified rather than assumed:
  - The compare code carries **only the actor key** (it is read aloud, so it
    is 64 hex and nothing more), so a dial starts with an `EndpointId` and an
    empty `PathCandidates`.
  - `iroh` 1.0 as pinned in this tree exposes **no local/mDNS discovery
    feature at all** (`default`, `tls-ring`, `portmapper`, `metrics`,
    `platform-verifier`, `qlog`, `test-utils`, `fast-apple-datapath`,
    `unstable-*` — that is the whole list), and the ceremony deliberately
    attaches **no relay URL**.
  - **§ LAN detection does not close it**, which is the part the PQ-1 design
    got wrong by implication: `should_attempt_lan_probe()` keys on a
    *peer-advertised* LAN address, and advertisement is exactly the
    nest-mediated path a nest-free ceremony does not have. "LAN candidacy by
    the shipped arithmetic" therefore describes the transfer plane, not this
    one.

  **This is a design gap in contract point 1, not an implementation bug**, and
  the three ways out are not equivalent: a **local-discovery dependency** (new
  third-party crates — a supply-chain decision, not a code decision); **widening
  the co-present payload to carry LAN candidates**, which the QR rendering
  could carry for free but a *spoken* code cannot, so it changes what "compare
  the code" means; or the **nest's relay**, which contradicts the nest-free
  claim outright and fails cross-nest anyway. Owner: the next design pass on
  this section. Captured internally.

  ✅ **RESOLVED 2026-08-19 by (b)** — contract point 1's *The compare code
  carries the addressing* bullet above owns the resolution and its
  consequences. One correction to the framing recorded here: (b) is **not**
  "QR-first". The reasoning that reached for QR assumed the widened code needs
  a camera; tui is the lead app and a terminal has no camera, so the
  **transcribed** form is the one that had to work, and it does. QR remains a
  later per-app rendering of the same elements, unchanged.

**2. Messaging groups: the delivery seat is a named, re-pointable role a
member device can hold** — A3's arbiter-epoch machinery (charter § The nest
decomposed, item 2) instantiated per MLS group. Each group has exactly one
**sequencing seat**; today it is implicitly the channel-home nest
([`direct-messages.md`](direct-messages.md) § Welcome Delivery). Target
state: a signed **seat-epoch entry** in the group's channel state names the
seat — a nest *or a member device* — and the epoch at which it assumes;
handover is sequenced by the outgoing seat, or ceremony-forced on its death
with A3's fencing. A nest-less initiation makes the initiating member's
device the group's first seat, and the Welcome travels directly over the
peer channel — the co-present ceremony *is* the delivery. When a nest
becomes reachable, the seat re-points to it by ordinary handover; the
group's history is continuous. **Rejected: provisional-group +
rebase-on-reconnect** — rebasing MLS epochs rewrites transcript history,
which is precisely the fork the obligations below forbid, and it opens a
reconcile window where two members hold irreconcilable transcripts.

**Fork-free reconcile proof obligations** (each must hold in the build, and
each gets a test):

1. **Epoch linearity.** At most one commit is accepted per epoch: the seat
   sequences commits, and a member refuses any commit not attested by the
   group's current seat.
2. **Seat-chain linearity.** Seat-epoch entries form a single chain, each
   sequenced by its predecessor; a ceremony-forced replacement fences the
   dead seat (retired-seat memory — the rotation-fence shape) so a
   partitioned old seat cannot mint competing epochs.
3. **Retrospective verifiability.** Commits carry the seat's order
   attestation, so a catching-up member verifies epoch order from any
   relaying peer without trusting it.
4. **Partition is a liveness cost, never a fork.** Seat unreachable ⇒ no
   membership changes; application messages within the current epoch still
   flow member↔member. A stated availability harm, no inconsistency.

**Trust posture — stated per the § Ordering model revisit trigger, never
inherited silently:** a device-held seat holds exactly the powers the nest
delivery service holds today over the same group — ordering among honest
commits, and withholding. MLS content authenticity and transcript agreement
are unchanged; members detect withholding/equivocation by comparing epoch
authenticators over the peer leg (detection, not proof — the same bound the
account plane accepts for frontiers). The generalization changes *who may
hold the seat*, never what the seat can do.

**The multi-device residue, named honestly:** a nest-less *member's own
fleet* still lacks its class-5 MLS-state arbitration (charter § The peer
leg → Scope). Single-device members are fully served by this design;
multi-device nest-less members wait on the *account* arbiter seat itself
becoming device-holdable — the same A3 generalization applied to the
account plane, owner charter § The nest decomposed item 2. Deliberately not
designed here.

**Build order (the tui chain):** T20's scheme detail (**resolved
2026-08-17**, same pass — charter § The recipient-set scheme) → the first
group scope + the initiation ceremony (shared Rust → nest ingest → **tui
leads the affordance**, per the ordering rule) → linux followed
(2026-08-20, row 334) → the UniFFI ceremony boundary (2026-08-21) → the
remaining four-app trickle-down (windows, macos, ios, android; web is a
declared absence, § *Wormability walk* rule 5). The messaging-seat half
builds separately, behind A3's build and the conversations plane; the
storage half never waits for it.

**Open questions:**

- **PQ-1 — offline share initiation. RESOLVED 2026-08-17** (design pass,
  refutable until first build) — § Offline share initiation above: storage
  shares are born as T20 recipient-set group scopes (no MLS, no seat, no
  arbiter — initiation nest-free by construction); messaging groups get the
  re-pointable delivery seat (A3's arbiter-epoch machinery per group,
  holdable by a member device). The R15 narrowing (2026-08-11) is what made
  the split available; the v1 M2-keyed precondition above stands
  permanently.
- **PQ-2 — hardening track.** Fuzzing the peer-channel framing + p2p kinds
  before the data plane ships (§ Wormability posture, mitigation "memory-safe
  stack" is necessary, not sufficient). **Scoped 2026-08-10** into a concrete
  cargo-fuzz track (framing decode, DAG-CBOR frame decode, kind payload
  decode; gate: green before any p2p data-plane slice ships). **BUILT
  2026-08-12:** the three check bodies live in
  `fauna_peer_channel::hardening`, shared by the workspace-excluded
  cargo-fuzz targets (`libs/fauna-peer-channel/fuzz`) and the
  `peer-channel-hardening-check` merge gate — a bounded smoke replaying the
  checked-in real-exchange corpus + a fixed deterministic sweep on every
  relevant push ([`../architecture/merge-gate-catalog.md`](../architecture/merge-gate-catalog.md)
  § The heavy gate catalog, thirteenth gate). The gate ran green at introduction,
  discharging the "green before any p2p data-plane slice ships" condition
  for slices landing while it stays green. **Pinned to the serve surface
  2026-08-12:** the gate's coverage was a hand-written snapshot of the kinds
  `fauna_peer_sync::server::allowlisted_kinds()` serves, with nothing tying
  the two together — so a fifth allowlisted kind would have re-fired the gate
  and passed **green** over its own un-smoked pre-auth parser, the gate's
  green reading as "the peer parser surface is hardened". The surface is now
  *declared* (`fauna_peer_channel::hardening::KIND_PAYLOAD_COVERAGE`) and the
  smoke checks it against the allowlist in both directions plus against the
  corpus on disk, so growing the allowlist without growing the coverage is a
  RED. Residual: coverage-guided long
  runs need `cargo-fuzz` + `libfuzzer-sys` (outside the vetted workspace
  lockfile) — install-gated on explicit user approval, per the fuzz crate's
  README; the gate never builds the fuzz crate.

> **The share leg's build contract, its build ledger, the row-half build design and the phone-peer design moved to [`p2p-shared-set-build.md`](p2p-shared-set-build.md) on 2026-09-27**, verbatim, when this doc reached 240,363 B (91.7% of the whole-file read ceiling, two days from it). The contract above, § Offline share initiation, § Peer-served change-row provenance and § Wormability walk stay here. Each moved heading below is a routing stub, so every citation still resolves in one hop.

### Build contract — the share leg's first design pass (2026-08-17; refutable at build time)

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract — the share leg's first design pass** (2026-09-27 split).

### Peer-served change-row provenance (ruled, lifted; refutable by the security review)

The walk's rule-4 sequenced obligation. The problem: nest-served change rows
carry a nest-stamped `author_actor_id` (the authenticated writer), while a
peer-served row's only provenance is "an admitted member relayed it" — so a
read-only member could fabricate rows misattributed to writers, and no
receiving replica could tell. The 2026-08-17 ruling split the plane by
**what the channel can verify** and refused relay outright; writer-signed
change records
([`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md)
§ Writer-signed change records, ruling (3), which owns
the signature, the reader and the switch) gave rows a proof of their own, so
the refusal lifts:

- **Bytes are multi-source; a row travels as far as its proof does.** Chunks
  (store-keyed by ciphertext hash, AEAD-tagged) and manifests (fetched by
  manifest hash, verified against it) are **self-verifying regardless of who
  serves them** — any admitted member serves any of them, and the
  multi-source contract stands untouched for the heavy half of the transfer.
  A change **row** carries one of two proofs. **A writer-signed row verifies
  self-contained** — the row, its `signature`/`signer_key` pair and its
  delegated signer's cert carried inline on the page (`PeerShareChange.
  signer_cert`) — through the one shared reader every client reader uses,
  under the receiver's own set nonce; so **the serve side sends every row it
  holds**, its own and every writer-signed row it relays (own-pending and
  sequenced alike, each marked pending-vs-sequenced), and an N-member set
  offline-relays every member's metadata, not only the relayer's. **Every
  writer signs, so an unsigned row is refused** outside the signature check's
  class exemptions — the same refusal the nest pull makes, never a second
  policy. An exempt-class row's only proof is the channel's (PT-1b): it is
  admitted solely as the serving peer's own; a relayed unsigned row is
  refused, and the serve side never sends one. A fabricated, altered or foreign-set row
  fails the signature; a read-only member's row signed under its own key
  fails the writer check below. What a replica relays is what its reader
  verified — off the nest pull or a peer's page — kept byte-exact beside the
  cert it chained through.
- **The writer check is a cached-roster consult on the SIGNED actor,
  fail-closed.** A verified row is admitted only if its signed actor is the
  set's owner or a `writer` — per the reader's own roster read (the engine's
  ungated roster leg) and, where the reader has not read one, per the share
  leg's cached roster (refreshed at each online roster read — a small additive
  at-rest cache, the last roster that stands offline, each writer's proven
  predecessors kept beside the writer so the one judge places a retired
  identity's row as on the nest pull —
  [`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md)
  rulings (8)(e) and (11)(i)); the serving peer's
  role vouches for no one else's row. An exempt-class own row needs the
  serving peer to be a cached writer. No cached role ⇒ refuse rows, serve/pull bytes
  only. A stale cache denies fresh writers until reconnect (heals) and honors
  recently-demoted ones briefly (the provisional overlay's reconcile cleans
  up — next bullet).
- **Peer-ingested rows are a provisional READ-SIDE overlay; the nest stays
  the log's arbiter.** Accepted rows never enter the nest-sequenced log
  store: they overlay the latest-per-path fold for reads (and drive
  materialization), and on reconnect the nest's sequenced rows confirm or
  supersede them — the overlay never changes what the converged log says,
  so reconcile cannot fork (the same posture as the account plane's
  "nest-arbitrated kinds travel as intents at most").
- **A provisional DELETE never destroys local bytes.** Provisional
  creates/modifies materialize; a provisional delete affects the overlay
  *view* only — local file deletion follows nest-confirmed rows alone
  (deletion is the irreversible direction; the no-data-loss bias picks the
  recoverable failure).
- **Residual, stated — a p2p-relayed row never touches the home nest,** so
  device revocation is not enforced on it at ingest: a row signed by a device
  revoked since is admitted on its (still chain-valid) cert. It is the first
  cross-boundary flow that does not route through the distinguished replica
  ([`devices.md`](devices.md) § Revocation
  authority names the re-visit); bounded by the provisional overlay, which
  the nest confirms or supersedes on reconnect, though bytes may have been
  applied (ruling (5)(v) of the owner section).

### Built — the serve/pull core (2026-08-17, row 59 slice B): what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the serve/pull core** (2026-09-27 split).

### Built — the discovery carriage (2026-08-17, row 59 slice F): what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the discovery carriage** (2026-09-27 split).

### Built — the serve-side byte half (2026-08-18, slice E leg 1): what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the serve-side byte half** (2026-09-27 split).

### Built — the pump, the boundary, and the discovery halves: what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the pump, the boundary, and the discovery halves** (2026-09-27 split).

### Built — the tui app leg (2026-08-19): what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the tui app leg** (2026-09-27 split).

### Built — the shared driver, and the linux leg: what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the shared driver, and the linux leg** (2026-09-27 split).

### Phone peers — design (2026-09-26; the direction is user-ruled, the decisions below are refutable at build time; the shared arm BUILT 2026-10-01, the app legs UNBUILT)

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Phone peers — design** (2026-09-27 split).

### Built — the member advertisement unblocked: what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the member advertisement unblocked** (2026-09-27 split).

### Build design — the row half (B2 scoping pass, 2026-08-17; refutable at build time)

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build design — the row half** (2026-09-27 split).

### Wormability walk — the share leg against the eight rules (first design pass; refutable by the security review)

The rules are § Wormability posture above; this is the share leg's per-rule
record, the cross-user twin of the peer leg's walk
([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
§ The peer leg → Wormability walk). **By construction** = the specified shape
satisfies the rule; **obligation** = the build must land the named piece for
the verdict to hold.

1. **Admission before parsing — by construction.** Nothing beyond
   `fauna.peer.node_info` and the admit exchange parses before the M2
   admission ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract); endpoints are learned only from
   authenticated channels' cached advertisements — no global address space,
   no broadcast, no scanning. Propagation is bounded to set co-membership
   edges, each created by an owner's gated share + a contact-gated accept.
2. **Memory-safe pre-auth stack — by construction plus one obligation.**
   Same seam and stack (Rust + quinn + rustls); the new pre-auth surface is
   the `fauna.peer.share.*` envelopes, strict dag-cbor. Obligation: every
   new kind, payload struct, and witness inner parser joins
   `fauna_peer_channel::hardening::KIND_PAYLOAD_COVERAGE` with corpus
   entries, and the smoke pins the share dispatcher's allowlist exactly as
   it pins `fauna_peer_sync`'s — growing either allowlist without coverage
   is a RED (the PQ-2 pinned-to-the-serve-surface rule, extended to the
   second allowlist). **DISCHARGED 2026-08-17 (slice B):** all eleven share
   wire structs sit in `KIND_PAYLOAD_COVERAGE_P2P_SHARE` with corpus entries,
   and both ties are pinned — table ↔ corpus, and table ↔ the real
   `allowlisted_kinds()`. **Extended same day (slice 3):** the
   ceremony carriage's wrappers, frame enum, and inner signed payloads
   joined the table + corpus in the same change that allowlisted the kinds
   — § Offline share initiation's carriage record owns their walk. **And
   again (slice 2's slot):** the admit exchange's
   `group_witnesses` carriage + the carried `GroupRosterRecord`
   certificate's own decode joined the admit row's coverage — the fourth
   witness kind's inner parser is smoked where the wire reaches it.
3. **Least-kind dispatcher — obligation. DISCHARGED 2026-08-17 (slice B):**
   `fauna_peer_share::server::allowlisted_kinds()` is exactly the probe, the
   admit exchange, the set change-log read, manifest fetch, and the want-list
   chunk pull — asserted as the whole set, never a sample. No config,
   capability-mint, admin, or key-material kinds — content keys ride only the
   M2 rail ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract). **Extended same day (slice 3):** the
   three offline-ceremony kinds joined the set behind their own pre-parse
   admission; the deliver's admission wrap is sealed end-to-end to the
   recipient's reception key (the T20 rail) — § Offline share initiation's
   carriage record owns that walk.
4. **Received content inert at the transfer layer — by construction, with
   one SEQUENCED ruling.** Chunks are convergent AEAD ciphertext,
   store-keyed by ciphertext hash, self-verifying (the F9 twin) — a
   poisoned chunk fails its hash/tag check and is refused; the worst a
   compromised member can feed the byte path is storage waste, bounded by
   rule 8. The recorded interaction is the **change plane**: nest-served
   change rows carry a nest-stamped `author_actor_id`, while a peer-served
   row's provenance is only "an admitted member relayed it" — a read-only
   member could fabricate rows misattributed to writers. **RULED 2026-08-17
   (sequenced exactly as the peer leg's rule-4 hostile-row posture was):**
   § *Peer-served change-row provenance* above owns the posture (lifted
   2026-09-29 by writer-signed change records: a row relays as far as its
   signature proves it). The chunk-pull leg never waited on this ruling; the
   ingest leg builds against it.
5. **No listener when off; compiled away when excised — the excision half
   holds; the participation half DOES NOT (corrected 2026-08-24).** This
   entry used to read "Share kinds exist on the listener only while p2p
   participation is on (the contact-plane node's lifecycle — no
   unconditional bind)". **Both halves of that sentence are false in code**,
   and the correction is recorded here rather than quietly rewritten because
   a walk entry is a compliance claim: the share seat does **not** ride the
   contact-plane node's lifecycle (it binds its own seat, at the
   account-store-ready edge), and its bind **is** unconditional with respect
   to any human — see § Element IDs → *RATIFIED 2026-08-24* and
   § Implementation status today → *The participation half of rule 5 is
   BUILT* for what was true until then. **HOLDS since 2026-09-25 —
   § Per-device participation, enforced at both bind doors in shared code:
   off drops the seat and the leg's node, and "no socket" is asserted on
   the socket (§ Implementation status today names the tests).** What
   else holds
   unchanged: web is structurally absent (no QUIC seam in the wasm graph). The whole plane sits behind the `p2p-share` cargo feature
   ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract), so a store-safe flavor ships **no share data plane at
   all** — rule 5's own excision claim. The **store-safe witness third
   column** ("same-account `fauna.peer.sync.` PRESENT + `fauna.peer.share.`
   ABSENT") lands WITH the first artifact that compiles a real share
   surface — the column-meaningfulness sequencing both walk records own —
   refreshing the four two-column sibling check rows in the same landing.
   **DISCHARGED 2026-08-17/18:** the share halves landed with tui's first
   root-linking artifact, the sync-PRESENT half and the sibling-row refresh
   the next day (`just tui-store-safe-check`; recipe shape owned by
   `dynamic-features.md` § The feature-matrix test story).
6. **Revocable per-device authority — by construction, and the STRONG
   severance class, with one honest limit.** Content severance here is
   **cryptographic**: eviction/leave rotates the set's content key (M2
   rotate-on-removal — the (a) class of the peer leg's corrected rule 6,
   with none of its generation-0 residue). Admission severs at the next
   evaluation (the roster consult re-runs; an evicted actor's next
   handshake finds no membership). The honest limit: the leg admits
   **actors**, so a counterparty's lost/compromised *device* still holds
   that counterparty's actor key — per-device severance on the contact
   plane is the counterparty's own device-removal + succession story,
   identical to every other actor-key surface; stated, not hidden. A second
   limit, stated the same way (2026-10-01): both severances reach a device
   through the nest, so a host that cannot reach one keeps admitting, and
   keeps sealing under the generation it holds, until it reconnects — what
   it authors in that window is open to a person removed since its last
   read, over this plane only and never from a nest
   ([`on-demand-files-shared-sets.md`](on-demand-files-shared-sets.md) § Shared sets on a capability
   host, decision 2′ owns the rule).
7. **Version brake — obligation; DISCHARGED 2026-08-17, both halves
   (nest slice D leg 1; client slice E).** The share plane lights behind a
   `p2p-share` nest capability token (cfg-gated advertisement, the
   `subscriptions` shape); the client binds and serves share kinds only
   under the token, cached last-known for offline starts exactly as the
   peer leg's brake evidence is — no evidence refuses by default. An
   excised nest omits the token (excision criterion 4). Built:
   `capability::P2P_SHARE` in `fauna-protocol::discovery`, advertised by
   `discovery_core::capabilities_for` under `cfg!(feature = "p2p-share")`
   (a default-ON `bins/fauna-nest` flavor-root feature forwarding
   `fauna-protocol/p2p-share`). It follows `SUBSCRIPTIONS`, not
   `PEER_SYNC`, deliberately: `peer-sync` is always-on because the nest
   carries none of that leg's traffic, but this plane has a nest-side
   half (rule 8's chokepoint), so a nest that compiled the plane away
   must not invite a client to bind a listener whose counterparty bound
   this box is not running. **The client-side halves (bind-refusal without
   cached evidence, the listener door) are now BUILT too**
   (`group_ceremony_node::{ceremony_bind_verdict, CeremonyNode::bind}` — no
   evidence at all refuses, verbatim the peer leg's posture), **and so is
   the cached-last-known read (2026-08-18)**: the bind's offline evidence is
   the peer leg's own `META_NEST_FACTS` cache re-served through
   `AccountStoreHandle::cached_nest_capabilities` — one cache, one writer,
   and the limits screen hides on the same token
   (`fauna_client_features::capability_token`). **The tui lead's app-side
   wiring is BUILT too (2026-08-17/18):** `offline_share.rs` reads the live
   brake (`ceremony_bind_verdict`) and calls `CeremonyNode::bind` with the
   resulting verdict — § Offline share initiation → *Built — the affordance,
   both roles*. **The transfer half's own bind is BUILT too (2026-08-19):**
   `share_glue::run` calls `bind_share_plane_seat`, which composes
   `bind_with_share_plane`/`set_shared_sets` — [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the tui app leg*.
   Since 2026-09-15 both doors bind `bind_with_share_plane` through
   the session's one `SessionSeat` (§ Offline share initiation → *One seat
   per session*); the verdict typing is the same, so this rule's evidence
   posture is unchanged.
8. **Bounded fan-out — obligation, on BOTH planes.** Listener-level:
   per-peer + per-window connection/request quotas on the share serve set,
   admission-refused attempts included, size-bounded ledger — **DISCHARGED
   2026-08-17 (slice B)**, by *reusing* `fauna_peer_sync::quota`'s ledger rather
   than growing a second one (it is peer-keyed and clock-injected, so it is
   transport-level, not account-plane-specific); a witness-refused admit spends
   the same budget as an admitted one, pinned. Feature-plane:
   `p2p-share.member.admit` composed **nest-side at the member-admit
   door** — **DISCHARGED 2026-08-17 (slice D leg 2), with the seam
   refined by building it.** The first design pass said "at the share
   roster write"; the build ruled that the gate composes one level up, in
   `welcome_deliver_core`'s first-reach arm on the **claim-row
   discriminator** (`folder_channel_claims` — nest state, never the
   sender's `req.kind`, which real folder shares don't reliably carry and
   a patched client could relabel). Three measured facts made that the
   ruling: (a) the raw roster write (`register_actor_channel_gated`)
   serves the conversation plane too and swallows its failures by design,
   so a gate composed there would either charge group-chat membership to
   the share plane or report the surface wired while binding nothing —
   the born-gated bar: a gate must live at a door that can *refuse*;
   (b) `welcome_deliver_core` is the ONE production door through which a
   new member reaches a claimed set's roster — the same-nest register and
   the cross-nest federation forward both fork inside it — and it already
   refuses first-reach Welcomes today (the reach floor), so a refused
   admit is established, recoverable behavior (nest state untouched; the
   owner's committed-but-un-Welcomed MLS add is cleared by the ordinary
   evict/rotate-on-removal path — no roster strands behind the crypto);
   (c) established recipients — roster or foreign-member rows — are
   in-band re-Welcomes: no gate, no spend, per the newness-delta rule, so
   idempotent retries cannot drain the unrefundable counterparty quota.
   `fauna.folders.share` (`share_core`) deliberately carries **no** gate
   call: it is per-set rather than per-member, skippable by a patched
   client for members 2..N, and a second spend point would double-count.
   The gate call is **unconditional** (no `cfg`): excising the
   `p2p-share` cargo feature removes the P2P transfer plane, not
   nest-mediated folder sharing, and the admission fan-out is the same
   bound in every flavor. Pinned through the real handler in
   `bins/fauna-nest/tests/conformance_feature_gate_p2p_share.rs`.
   And `p2p-share.transfer` composed
   **client-side at the transfer surface** (tier-1 constants ship in the
   artifact; the app is the enforcement point for a plane the nest never
   sees — [`../architecture/dynamic-features.md`](../architecture/dynamic-features.md)
   § Evaluation points) — **the MECHANISM is DISCHARGED 2026-08-18**: the pump prices every page through the ONE shared
   `feature_verdict` before any byte moves
   (`share_pump::{transfer_verdict, TransferUsageLedger}` — nest-served
   effective policy when reachable, the artifact's tier-1 constants
   offline), over a device-local day-bucketed ledger (meta-table, one
   owner, newness-delta counterparties, pruned past the largest window;
   device-local because the nest cannot arbitrate what it cannot see — the
   fleet-summed refinement is named forward work). One operation = one
   file transfer, ruled here. **Both metered dimensions are counted by the
   PULLING side, never taken from the counterparty (ruled 2026-08-20):**
   ops are bodies that actually arrived and volume is the bytes that
   actually arrived — `share_pump::SpoolSpend`, counted inside
   `spool_planned` as each body lands, discarded bodies included.
   The change row's own `size_bytes` may *price* the pre-authorisation
   (the plan is put to the gate before any byte moves, which is the point
   of pricing it) and may do nothing else: it is the serving peer's
   assertion about its own transfer, and a counterparty's assertion is
   never what the meter spends. For the same reason the record is
   unconditional on **arrival**, not on
   materialization — whether a row materializes is equally the peer's
   choice, and a page of rows the ingest's path guard refuses moved every
   byte. Consequence worth stating: volume is WIRE spend, so a compressed
   or encrypted body meters below its plaintext size, and a body fetched
   for a row that never materializes still meters. Over-counting against
   the counterparty is the sanctioned direction; under-counting on the
   counterparty's say-so is not a direction at all. **The per-operation
   cap is put to each FILE, never to the page (ruled 2026-10-12):** one
   operation is one file, so the page's windows are priced on its summed
   size while the cap is priced on each planned file
   (`share_pump::price_plan`), and it is enforced again on **arrival** —
   with a cap set the spool pulls a file chunk by chunk and stops it at the
   first body that takes the bytes seen past the cap, metering every byte
   that moved and landing nothing (`share_pump::spool_planned`), because a
   claimed size may price and do nothing else. **A capped file is a spool
   miss, never a page refusal, and a permanent one:** the pump marks the
   file capped in the page's spool and the ingest reads that miss as a
   permanent skip (`peer_share_store::SpoolFetcher`), so its row lands
   without the body — the overlay row stands unmaterialized, and an
   on-demand replica also records a placeholder for a path it had no row
   for — and the cursor advances, unlike a body the peer did not produce
   this pass, which holds the cursor below its row for a retry; so one
   oversized file never holds the files behind it; the file reaches this
   device by the nest path, and the surface reads "limited by" the cap's
   tier (`SetPullOutcome::capped`). A refusal stops the
   page and rides `SetPullOutcome::refusal` for the surface to render
   (Dim-3). The
   serve side stays bounded by the listener-level quotas above;
   feature-plane accounting of served bytes is named forward work. The
   surface that renders the verdict is built — `share_glue::transfer_state_label`
   reads a refusal or a capped file first ("limited by …"), consumed by every
   app that binds the plane (§ *The transfer row of a pull that failed*).
