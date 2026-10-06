# Federation — nest ↔ nest wire layer — target state

Owns: federation, rate-limiting
Status: ratified
Authority: owns the nest↔nest peer-auth model, federation carrier + kind inventory, open-federation trust model, KP privacy/exhaustion, anonymous-surface security model, and request-level rate-limiting (anonymous + federation throttles); what a client concludes from a foreign domain's `by_handle` answer or non-answer (discovery-failure semantics, the dial rule) → foreign-handle-resolution.md; connection-level caps → transport-connection.md § Abuse posture; mail per-IP cap → behavior/mail-policy-config.md; client↔nest WS-RPC → transport.md; HTTP classification → api-layers.md.

> **Audience:** nest and shared-Rust contributors touching any cross-nest path
> (conversations, knock/contacts, nest-sync, report and trend exchange,
> discovery feed); app
> contributors wiring a typed foreign handle (`bob@other-nest.test`).
> **Purpose:** the authority on how one Fauna nest authenticates and talks to a
> **peer** Fauna nest. This is the **Spec Y2** surface
> (design ratified 2026-05-30; tracked internally).
> **Scope:** Fauna↔Fauna(/fork) federation only. Federation with **non-Fauna**
> nests stays HTTP+JSON-LD per ActivityPub (`transport.md` § HTTP residue) and is
> out of this doc.
>
> Sources: `bins/fauna-nest/src/{federation_sig.rs,federation_channel.rs,
> federation_handlers.rs,federation_router.rs,federation_pool.rs,discovery_core.rs,
> discovery_handlers.rs,pre_identity_allowlist.rs,anonymous_rate_limit.rs}`,
> `libs/fauna-protocol/src/discovery.rs`, `bins/fauna-nest/src/db/channels.rs`.

## Goal

A Fauna nest authenticates a federation request **on behalf of its client** by
signing the request with its long-lived nest Ed25519 key; the peer verifies the
signature against the originating nest's `nest_id` (which **is** that public key).
This **mutual nest-key** model replaces the client-bearer ceremony of the
client↔nest plane for every cross-nest call — a foreign client never needs (and
cannot hold) a bearer for a peer nest. The carrier is the **long-lived nest↔nest
WS-RPC channel** (`GET /api/v1/federation/ws`, reusing the Y.1 L3 envelopes,
peer-symmetric) — the **sole** Fauna↔Fauna carrier since Spec Y2 slice 5
(2026-06-03), when the request-signed HTTP interim that carried the migration
was retired. The **auth model is the settled, load-bearing part**; the carrier
migration is complete (§ Transport, § Implementation status today).

## Peer-auth model

- **Identity = key.** A nest's `nest_id` is its 32-byte Ed25519 public key
  (`federation_sig.rs:4`). There is no separate key registry: to verify a peer's
  signature you build a `VerifyingKey` straight from the `nest_id` carried in (or
  discovered for) the request.
- **Signing.** Federation requests are signed with `federation_sig::sign_payload`
  (canonical dag-cbor sign-over-CID) and verified with `verify_payload` — the same
  primitive the retired `/api/v1/nest-sync/*` HTTP twins used (Spec Y2 slice 5
  moved that sync surface onto this channel's handshake-auth model). **Do not
  invent a new envelope.**
- **Nest-key discovery** is the existing **anonymous** discovery chain — no new
  anonymous surface:
  1. `fauna.nest.resolve(domain)` → canonical node URL (SRV).
  2. `fauna.nest.info` on that URL → `nest_id` (the peer's Ed25519 pubkey).
  3. `fauna.actor.by_handle(handle@domain)` → target `actor_id`.
  Discovery **MUST run over authenticated TLS** to the resolved canonical domain so
  the TLS certificate binds the domain to the `nest_id` it serves (see § Security).
  Plain-HTTP / loopback discovery is for in-process tests only.

  **Discovery trust rule (ruled and ENFORCED 2026-09-25).** "Authenticated TLS" is judged by the nest at the
  moment it would believe a `nest.info` answer, from the cert the capturing
  verifier saw on that dial (the anonymous dial is encrypt-only until then —
  `security.md` § Transport trust, Axis 1). The answer is believed only when the
  served cert is **WebPKI-valid for the dialed authority** (a domain, or a public
  IP under `nest/tls-certificates.md` § B-IP), with exactly two carve-outs: **(1) a
  loopback literal** — the in-process fixture, the same test-only carve-out the
  peer-URL guard documents; **(2) the deployment's own pull target at a private
  address** — the `nest_url` of a pairing row whose actor is an admin of the
  private nest (`nest/private-mode.md` § Pairing Flow): a home box reaching its
  public relay over the LAN, a VPN or
  the docker bridge, where the relay's public-domain cert cannot match the private
  name it is dialed by and where that private network is the deployment's declared
  trust boundary (`nest/deployment-home-with-public-relay.md`). Anything else — a
  request-named URL, a handle domain, a non-admin's pairing row, a pull target
  on the public internet —
  serving a cert the WebPKI does not trust for that authority is **refused before
  any request is sent**, and nothing is cached. **Deliberately no trust-on-first-use
  rung**: the client-side TOFU rung exists for a home nest with no DNS authority
  (LAN, `.local`); a federation peer has one by construction, so a peer the WebPKI
  cannot vouch for is misconfigured or impersonated, never benign — and a peer
  whose ACME issuance has not completed is refused until its cert lands (every
  originator retries). Carve-out (2) is **pinned**: the private nest's pairing
  rows store the paired nest's `nest_id` beside its `nest_url`, and every
  discovery of a URL a row records — fresh or cached, from any worker that dials
  it, the carve-out's or not — is refused unless the answering nest is the id
  the rows record there, the way a registered backup destination's pin is spent.
  For a URL a live admin row names, only the live admin rows choose the pin — a
  non-admin's row at that URL is request-named and cannot unpin it; rows (of
  the set that chooses) recording several ids at one URL leave it unpinned with a
  warning.
  The class that decides carve-out (2) is judged from **one** resolution of the
  target's name, and the dial connects to an address from that same answer: the
  set is private only when every address is, so a mixed or rebinding answer is
  judged by the public rule. The succession pull's anonymous dial is anchored separately, by proving the
  pinned identity, and does not ride this rule. Code:
  `federation_pool::discovery_dial_admits` (the pure table) beneath
  `resolve_peer_nest_id`; pinned on the wire by `conformance_discovery_tls_root.rs`.
- *Moved 2026-09-28 to [`foreign-handle-resolution.md`](foreign-handle-resolution.md) § Peer-auth model, verbatim:* **Discovery-failure semantics (ratified 2026-08-29)** — what the client's anonymous `by_handle` hop against a foreign domain concludes: **a nest answered** (the closed disowning set; **The dial names the peer**; **What a peer's answer may and may not claim**), **no nest answered** (the known-Fauna-domain evidence; **"Unknown" is a verdict the client must EARN**), and the **non-signals, rejected on the record**, with the same-nest sibling's agreement. The nest-side half — which `nest.info` answer a nest believes — stays above, in *Discovery trust rule*.
- **Replay protection.** Replay is bounded per session by the channel: the
  `fauna.federation.hello` handshake binds a fresh `channel_nonce` to the live TLS
  SPKI, and within a session the L3 `idempotency_key` + per-connection idempotency
  cache make redelivery safe (destructive kinds, e.g. key-package fetch, set
  `forbid_replay`). The HTTP interim's bespoke per-request `requested_at_ms` +
  `nonce` skew-window / dedup were retired with it in slice 5.

## Trust model — open Fauna federation

Cross-nest **conversations** and **knock-send** are **open federation**: any Fauna
nest may present a valid signature, and any user on any Fauna nest may start a
conversation with / knock `bob@other-nest.test`, subject to the recipient's own
gates (key-package availability, `InboxMode`). This matches the existing anonymous
discovery posture and the public-data federation routes. **Pairing (`is_paired`)
stays the gate only for the private nest-sync surface that already uses it** —
conversations/knock are not allowlist-gated, because federated messaging is not.

**A nest-signature is attribution + rate-limiting, NOT authorization.** Anyone can
run a nest and mint a key, so a signature proves *which* nest is calling (so it can
be throttled/banned), never *that the call is allowed*. Every structural defense
must hold against a hostile signer.

## Federation residue surface

Cross-nest calls and their auth target. Every row below rides the nest↔nest
WS-RPC channel (`fauna.federation.*`) as its **sole** carrier; the HTTP interim
that carried them during the migration was **retired in Spec Y2 slice 5**
(2026-06-03). The "Former HTTP route" column records the now-deleted twin for
provenance. The table is code-verified against the registered `FederationRouter`
kinds: **10 rows carrying 17 served kinds** as of the 2026-07-12
re-verification, plus the `fauna.federation.hello` handshake — a missed row
would make the "uniform migration" claim false. Since then the 2026-07-18
cross-nest rows landed and are BUILT: 6 more served kinds — `channel.append`,
`channel.leave` and `folder.{changes.fetch, content_key.fetch}` (Phase 2,
2026-07-19) + `folder.{changes.record, write_token.mint}` (Phase 3,
2026-07-20) — across 3 more rows. **Two more corrections found stale during
the 2026-07-20 sweep (never folded into this running count):** the trends
row is **also BUILT, not target** — `fauna.federation.trends.{exchange,export}`
was registered 2026-07-14 (`trending.md` § Implementation status today, Phase
2 slice 3 of the distributed-moderation frame — a separate, earlier lineage
than this folder Phase 2/3 work) and has originated on the exchange-originator
plane since the same day (slice 4); and the post-forward row gained a second
kind, `fauna.federation.post.delete` (2026-07-15, the delete twin of
`post.forward` — see that row below), riding the same row rather than a new
one. **A further row was found undocumented entirely during the 2026-07-23
sweep:** the Nostr Phase-2 proxy-delegation relay —
`fauna.federation.sync.{nostr_push,nostr_pull}` — was registered 2026-07-22
(Nostr Phase-2 wave 2, `federation_handlers.rs:1444-1451`; `ui/nostr.md`
§ The bridging gate → Phase 2 owns the mechanism), riding a new row rather
than an existing one. **The nest-writer backup plane's two kinds landed
2026-07-23** (nest-side segment backup slice 3, auth-plane pass) —
`fauna.federation.backup.{changes.record,write_token.mint}`
(`register_backup_federation_handlers`), gated on the nest-writer grant (§
Nest-writer backup plane below owns them). **Identity-succession propagation
landed 2026-07-29** (`identity-succession.md` slice 4) —
`fauna.federation.succession.push` (`register_succession_federation_handlers`),
a new row and the **only mutating kind on this surface with no authorization
gate**: since the 2026-07-29 anchor-rule hardening the push is a *hint* whose
payload is never trusted — the receiver re-verifies from its own recorded
anchor (see the row's Notes). **The conversation roster-read relay landed
2026-07-29** (cross-nest chat add slice 2) — `fauna.federation.channel.actors`
(`register_conversations_federation_handlers`), the roster-read twin of
`channel.fetch` (see the row). **The public folder read plane landed 2026-08-18** (folders re-model phase 4
slice 4f-i) — `fauna.federation.folder.public.fetch`
(`register_folder_federation_handlers`), on a **new row** rather than folded
into the folder content-plane row: same subject, but a different auth target
entirely (the addressed folder's own audience, never the caller), and the auth
target is the column this table is keyed on. It is also the family's first kind
whose request carries no requesting actor at all (§ The public folder read plane
owns the reasoning). **The room plane's two conversation kinds landed
2026-09-09 and 2026-09-10** — `fauna.federation.conversation.write_token.mint`
(a room's attachment write relay) and
`fauna.federation.conversation.generations.fetch` (a foreign member's own
generation wraps), each on a **new row**: both are conversation-plane kinds
whose auth target is the `channel.fetch` gate, but the table is keyed on the
call, and neither is a variant of an existing one. **The room roster report
relay landed 2026-09-10** (`fauna.federation.conversation.roster.report`, the
write twin of the roster read), on a new row for the same reason. **The
room-post verdict read's relay landed 2026-09-11**
(`fauna.federation.conversation.room_labels.fetch`), likewise on a new row —
and it is the last community-room read that lacked one: a room post's verdicts
leave by a post-scoped door of their own rather than on any envelope read
([`../behavior/restricted-posts.md`](../behavior/restricted-posts.md) § Encryption at rest → *Room-restricted — the
ruling* → *Built* detail (v)), so nothing already relaying carried them. **The
room LEAVE relay landed 2026-09-11** (`fauna.federation.conversation.room.leave`,
the self-scoped twin of the roster report), on a new row for the same reason,
and it is the room plane's first *mutating self-scoped* relay: a foreign member
had no departure at all until it, because `room.leave` is a same-nest door and
the generic `channel.leave` it could otherwise reach drops the relay binding
without touching the floor. **The cross-nest community INVITE landed
2026-09-26** on two new rows — `fauna.federation.conversation.room.invite`
(the room home's delivery of a knock to the invitee's nest) and
`fauna.federation.conversation.room.accept` (the invitee's nest's relayed
acceptance, the seating twin of the leave) — the one pair of room relays
whose gate is not the foreign-member binding, because the accept is what
writes it ([`../behavior/room-invitations.md`](../behavior/room-invitations.md)
§ Join rules and invites → *A cross-nest invitation* owns the ruling) — and,
later the same day, **a third row for the leg the pair left named**,
`fauna.federation.conversation.room.invite_issue` (a foreign member's relayed
*issue* of an invitation, gated on the binding like the leave and run through
the same-nest invite body). **The reputation exchange/export row left
2026-10-02** with the federation reputation leg (the ruling paragraph below
the table). Current total: **28 rows carrying 42 served kinds** (re-counted
against the registered `FederationRouter` kinds 2026-10-02 — 41 distinct
`fauna.federation.*` handler registrations, `grep -c 'b\.add('` on
`federation_handlers.rs`, plus the abuse-report forwarding kind registered from
`abuse_report_federation.rs` — plus the `hello` handshake.)

| Call | Former HTTP route (retired, slice 5) | Auth target | Channel kind | Notes |
|---|---|---|---|---|
| Key-package **fetch** | `GET /api/v1/keypackage/{actor}` | **nest-signature** | `fauna.federation.keypackage.fetch` | destructive (`take_key_package`); last-resort fallback + throttle (§ Key packages). `forbid_replay` on the channel. Bearer was the slice-2 residual bug, now fixed. |
| MLS **Welcome** deliver | `POST /api/v1/welcome/{actor}?nest_url=…` | **nest-signature** | `fauna.federation.welcome.deliver` | `push_inbox` + `Welcome` push. Records the foreign recipient as a channel member (home `nest_id`) on the channel-home nest, so the next row can authorize the relayed fetch. |
| Conversation **channel-message fetch** | **(none — channel-only, net-new)** | **nest-signature** + channel-membership-bound-to-home-`nest_id` | `fauna.federation.channel.fetch` | net-new, NOT residue: the **open-federation** cross-nest message pull for **unpaired** nests (`direct-messages.md` § Technical Flow — Cross-Nest, step 3). Read-only; returns the channel's ciphertext entries `(seq, envelope)` the relay cannot decrypt (MLS E2E, § Security). A member's drain on its home nest originates it to the channel-home nest, which serves it iff the requesting actor is a recorded member of the channel **and** the verified `origin_nest_id` is that member's recorded home nest (the structural defense against a hostile signer harvesting an un-hosted channel). Per-originating-nest throttled. Distinct from the `is_paired`-gated `mls_pull` below (that is the paired-nest buffer-pull optimization). **Carries the member's announced `handle@domain` (additive `requesting_handle`/`requesting_domain`, 2026-09-10)** — the id→handle ruling's one mechanism: the originating nest volunteers what it joins from its own `users` row, and the home nest records it beside the binding only after resolving the domain to the originator's key (§ Cross-nest shared folders + channel append, the id→handle bullet, owns the rule). **Carries a community room's verdicts beside each entry (additive `labels`/`scores`, 2026-09-10)** — what the room's named labelers derived (`../behavior/community-rooms.md` § The three classes → *What the home nest does with its read* owns what they are and who may read them), filled exactly when the requesting actor is a live floor member of the room: the gate the same-nest read keys on its caller, applied to the actor the binding admitted, never to the peer — and never beside a withheld envelope. The relaying nest forwards them onto its member's `ChannelFetchEntry` and stores none. The skew is benign both ways, which is why an additive field suffices here: an older home serves none and an older relaying nest ignores them, and either way the member reads the envelopes alone — the pre-field behaviour, never a wrong verdict. |
| Namespace + MLS **sync** | `/api/v1/nest-sync/{pull,push,mls-pull,mls-ack}` | **nest-signature** (`is_paired`) | `fauna.federation.sync.{pull,push,mls_pull,mls_ack}` | already nest-signed today; the private paired-nest surface. |
| Mail **relay** (public→private) | **(none — channel-only)** | **nest-signature** + `mail_pull` capability (`pairing_has_capability`) | `fauna.federation.sync.{mail_pull,mail_ack}` | net-new, NOT residue: the public→private `__mail` relay (`deployment-home-with-public-relay.md` § Inbound mail). Built channel-only (no HTTP twin) — slice 5 has nothing to retire here. Moves verbatim sealed `MailRecordEnvelope` bytes; ack→tombstone→compaction purge. |
| Post **forward** (relay) | `POST /api/v1/forward` | **nest-signature** + `post_forward` capability (`pairing_has_capability`), under the `paired_only` submission policy | `fauna.federation.post.{forward,delete}` | private paired-nest post relay (`federation_handlers.rs` `register_post_federation_handlers`). The twin's extra per-forward nest-signature over the JSON body is **gone** — the channel handshake authenticates the relay, so only the post's own author envelope rides. Origination (the private-side outbox worker) is owned by `nest/private-mode.md` § Post Forwarding. **`post.delete`** (added 2026-07-15) is the delete twin: the author-signed `Tombstone` rides verbatim (re-verified on receipt) so a forwarded copy on the public nest does not outlive the original (`feed.md` § State & data shape → Post deletion → Propagation), gated by the same `post_forward` capability. |
| Discovery-feed query / post fetch | `POST /api/v1/feeds/query`, `GET /api/v1/posts/{id}` | unauthenticated public-data | `fauna.federation.feed.query`, `fauna.federation.post.get` | scoring/post bytes; **unauthenticated today** — gains attribution + per-nest throttle on the channel. |
| Social inbox **deliver** | ~~`POST /api/v1/inbox/{actor}`~~ *(twin DELETED 2026-06-09 — channel-only)* | **nest-signature** | `fauna.federation.inbox.deliver` | cross-nest Fauna-native signed `(ContactRequest, Post)` delivery → recipient `InboxMode` routing + inbox row + push (`routes::deliver_inbox_payload_core`, kept — the channel rides it). |
| Report exchange/export | **(none — channel-only, net-new)** | **nest-signature** | `fauna.federation.reports.{exchange,export}` | the k-anonymized per-item report aggregates (`behavior/report-sharing.md` § Federation exchange — that doc owns the mechanism): only `(content-hash, factor, count)` crosses, importer re-validates the k-gate, hard 1024-entry cap, per-peer row cap. **All peer aggregates for a hash collapse into one non-scaling peer bucket** — a peer's `nest_id` is self-minted and free, so neither a peer's claimed count nor the number of peers may scale the local distinct-reporter consensus (the ruling paragraph below the table, *What outlives the leg*, keeps the reason). Open-federation, per-origin throttled. Registered 2026-07-07 (`register_reports_federation_handlers`). |
| Abuse-report **forwarding** *(BUILT 2026-09-26)* | **(none — channel-only, net-new)** | **nest-signature** | `fauna.federation.abuse_report.{deliver,withdraw,outcome}` | user-initiated reporting's cross-nest leg (`behavior/moderation.md` § Routing — that doc owns the mechanism): a report on a foreign author is forwarded reporter-anonymously to the author's home nest, which accepts it only for a subject it hosts; the withdrawal and the outcome follow, each pinned to the nest the delivery was verified as. Every leg is idempotent on the receiver (a delivery is keyed on the verified origin plus its `report_ref`), so each is retry-safe; the sender drains them from a durable queue with backoff. Open-federation, per-origin throttled (`abuse_report_federation::register_abuse_report_federation_handlers`). |
| Channel **append** *(BUILT 2026-07-19; ratified 2026-07-18)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.channel.append` | the **mutating twin of `channel.fetch`** — a foreign member's send relayed by their home nest into the channel-home nest's log (same structural gate as the fetch). Commit admission on claimed folder channels is **roster-membership** (re-ratified 2026-08-24 — a foreign member's device-owned-epoch takeover must land; owner-only roster management is enforced member-side, since commit content is ciphertext to the nest); off-roster actors refused; conversation channels open. Rides the L3 `idempotency_key`. § Cross-nest shared folders + channel append. |
| Channel **leave** *(BUILT 2026-07-19; ratified 2026-07-18; room-hardened 2026-09-11)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.channel.leave` | the **first mutating member-gated self-scoped kind**: deletes the requester's own `channel_foreign_members` row (idempotent; absent = success), killing all future federated fetches. Generic — conversations and folder channels alike. ⚠ **On a floor-authoritative room it runs the room's own leave body instead** (`conversations_handlers::room_leave_apply`, shared with the *room leave* row below), because on a room the binding delete is only half a departure: the seat stays, and a seat nobody occupies is not inert — the roster-coverage gate *obliges* every later generation mint to wrap to its reception key, and `room.invite` refuses to re-admit a principal that is already a member, so the ghost both keeps drawing key material and blocks the re-admission that would heal it. Nothing of ours calls this kind with a room channel id (the conversations plane has its own kind, and no app carries a conversation self-leave gesture at all), but a peer nest can, and a door that converges only when the caller picks the right kind is not converged. The `removed` flag keeps naming the **binding**, which is this kind's own contract. The absent-row arm still unseats nothing, deliberately: with no binding this nest can prove nothing about the caller's home, and unseating on an unproven claim would hand any peer a removal door for any principal on any room's floor. |
| Channel **roster read** *(BUILT 2026-07-29; ratified 2026-07-29)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.channel.actors` | the **roster-read twin of `channel.fetch`** — the add-participant heal's discriminator for a member whose channel is foreign-homed (heal mechanics: `mls-group-key-material.md` § M2 *Admitting a member*, chat bullet). The home nest answers the union `actor_channels ∪ channel_foreign_members` — **hex actor ids only, never nest URLs** (members already read the full membership off the MLS ratchet tree, so the ids disclose nothing new; URLs would) — and the read is **strictly read-only on every hop**: no auto-register anywhere (a roster read that wrote a row would make every phantom look healthy to the next caller). The **client leg is the distinct kind** `fauna.conversations.channel.actors_remote { channel_id, nest_url }` — never an additive `nest_url` on `channel.actors`, for a sharper reason than `send_remote`'s (`../behavior/direct-messages.md` § step 3b): an old member-nest ignoring an additive field would answer its **partial** roster as a clean success, and the heal consuming it would evict a healthy member under supported version skew — while an unknown kind fails loud and the seam's error→"roster unreadable" mapping lands in the heal's refuse arm, the correct degradation on every skew pairing. The fetch-vs-actors dividing line: fetch's additive field is safe because its old-nest degrade is a benign stale/empty read; the actors read feeds a **membership-mutating** heal, so a silently wrong answer corrupts. |
| Conversation **attachment write token** *(BUILT 2026-09-09; ratified 2026-09-09)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.write_token.mint` | the **blob twin of `channel.append`**: a room's attachment bytes rest on its home nest beside the record that pins them (`../behavior/conversation-rooms.md` § The home nest → *Attachment bytes* owns the residency), so a foreign member's own nest relays this mint and the member POSTs the sealed attachment DIRECT to the home nest's `POST /api/v1/blob` under the returned short-lived, write-only bulk token (purpose `ForeignConversationWrite`, TTL the same `FOREIGN_WRITE_TOKEN_TTL_SECS` Rust constant, bridge-mint-refused) — **bytes never ride this channel**. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) — no `access == 'writer'` arm, because a conversation has no claimant and every member posts; no metering, because a room carries no owner-pays byte policy (§ Cross-nest → *Substrate vs. policy*). Client leg: the distinct kind `fauna.conversations.blob.write_token.get { channel_id, nest_url }` (unknown-kind-loud on an old own nest, S5-mapped to `peer_nest_outdated`); the read leg needs no kind — a member fetches by content address from the home nest's public `GET /api/v1/blob/{hash}`, direct. |
| Conversation **room generation read** *(BUILT 2026-09-10; ratified 2026-09-08)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.generations.fetch` | the **key-read twin of `channel.fetch`**, and the piece that makes a community room reachable from a foreign member at all: the log `channel.fetch` relays is sealed under a room generation key (`../behavior/community-rooms.md` § The three classes → *Community*), so a member homed elsewhere needs its own wraps as well as the ciphertext, and "a member on a foreign nest reaches the room only through their own home nest" (§ The home nest). Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) — a room id **is** its channel id. ⚠ **The request carries `{ requesting_actor_id, room_id }` and deliberately NO `entry_id`, and never may:** the room home resolves which wraps to serve from the requesting actor's own live floor entry (the same resolution the same-nest `room.generations` uses), because the home nest of a community room is *itself* a seated floor principal with a room-read wrap — an entry-named request would let a relay ask for that one and read the room, when a relay is specified to carry ciphertext and hold no wrap (§ The home nest). Bound as it is, the worst a hostile relay can request is a wrap belonging to one of **its own** members, sealed to that member's reception key and opaque to it. Read-only on every hop: serving a wrap registers nothing and mints nothing. Client leg: the distinct kind `fauna.conversations.room.generations_remote { room_id, nest_url }` — distinct for the `channel.actors_remote` reason, not `send_remote`'s: an old own-nest ignoring an additive `nest_url` would answer from its own empty room plane as a clean success, and the member would read "this room has no generations", a silently wrong answer **about key material**; an unknown kind fails loud. The relaying nest forwards the reply and stores none of it. |
| Conversation **room roster read** *(BUILT 2026-09-10; ratified 2026-09-10)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.roster.fetch` | the **name-read twin of `channel.fetch`**, and the piece that carries the id→handle announce to the one seat it could not reach: the **foreign member's own device**. A room's floor roster lives on its home nest alone (`../behavior/conversation-rooms.md` § The home nest), so a member homed elsewhere has no roster read at all — its own nest holds no room record and answers `permission_denied`, indistinguishable from "you are not a member" — and every co-member it has not met renders as an elided actor id, including the members the announce names for everyone on the room's home. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) — a room id **is** its channel id. ⚠ **The gate is that and deliberately NOT `is_room_member` on top:** the home nest's `channel_foreign_members` row is its own record of the Welcome it relayed and the binding it pinned — admission itself — whereas an end-to-end room's floor roster is a member-*reported* mirror (§ The floor roster) that routinely lags a membership commit, so stacking it on would deny the read to the newest-seated member, precisely the one whose co-members are still nameless. Nothing is disclosed by that choice: an admitted channel member already reads every member's actor id off the MLS ratchet tree — and **admitted** is a live fact, not a historical one, because a removal ends the admission along with the seat. So this gate closes with the membership, and the doors beside it that read the same row alone — the write-token mint and `channel.actors` — close with it. ⚠ **Which ceremony closes them differs by class, and both are needed for that sentence to hold** (2026-09-11): a **community** room's `room.remove` *purges* the binding before it unseats, exactly as `members.evict` does on the folder plane (S8, the *Channel append* row) — the ceremony is [`../behavior/community-rooms.md`](../behavior/community-rooms.md) § Implementation status today → *A removal severs*; an **end-to-end** room has no such door (its membership authority is its MLS group, so `room.remove` refuses the class outright) and removes a member by membership commit plus the committing device's roster report, whose absorb stamps `removed_at` and purges nothing — so on that class these three doors *read the floor's verdict* and refuse a **positively removed** requester (`federation_handlers::refuse_removed_room_member`). **The two ceremonies are complementary, never cumulative:** the floor read is skipped on a floor-authoritative room, exactly the class `room.remove` serves, so every room has one severance mechanism and none has two — and the skip is required, because on the purge's class the binding *is* admission and the seat is a separate act, so a re-invite's relayed Welcome re-inserts the purged binding and must be served while the old `removed_at` row the purge superseded still stands. The ceremony is [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Implementation status today → *A removal severs on the end-to-end class too*. A requester with **no** floor row stays admitted under either ceremony — that is still the newest-seated member this row's gate is ratified to protect. **The resulting per-class asymmetry, declared:** the purge shuts all four doors on a community room including `channel.fetch`'s ciphertext drain, where the floor read shuts only these three plaintext doors and leaves an end-to-end room's `channel.fetch` open — ratified, because that removal re-keys the MLS group, so what a removed member may still fetch it cannot open, and the residue is the one its own retained keys already are. The floor read was chosen over purging on absorb *because* the report ratchet deliberately admits a departing member's final report, so a purge there would let one live member's false report sever every co-member's binding past recovery (the Welcome relay's insert arm holds `InsertOnly` power) — `principles.md` § No client-causable unrecoverable nest state. **Handles ride the reply, and that is the read.** This is emphatically NOT the refused id→handle oracle (§ Cross-nest shared folders + channel append, the id→handle bullet): that shape answered "what handle does actor X wear?" to any nest holding an id, under a gate that could verify nothing about the asker; here the asker must prove, through the same binding that lets it fetch the room's ciphertext, that one of its own members sits on this room's floor — exactly the audience the ratified announce names ("the rooms it is in"). The neighbouring `channel.actors` row serves hex ids only because *its* consumer is a membership-mutating heal that needs identity and never names; here naming is the whole point. The home serves it from `conversations_handlers::room_roster_reply`, the **same body** the same-nest door serves, so a foreign member sees the same names a home member sees and the two replies cannot drift. Read-only on every hop: serving a roster registers nothing and mints nothing. Client leg: the distinct kind `fauna.conversations.room.list_roster_remote { room_id, nest_url }` — distinct for the `channel.actors_remote` reason, since an old own-nest ignoring an additive `nest_url` would answer its own room plane's clean `permission_denied`, a refusal the seam cannot tell from "you were removed", leaving the members silently elided; an unknown kind fails loud. The relaying nest forwards the reply and stores none of it. |
| Conversation **room roster report** *(BUILT 2026-09-10; ratified 2026-09-10)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.roster.report` | the **write twin of the roster read above**, and the piece that lets a member homed elsewhere keep the room's floor roster true: § The floor roster has the committing device report the resulting roster to the room's **home** nest after every membership or policy commit, and a foreign member's commit rides its own nest's relay to the home's log — so its report must ride the relay too, or the home's floor (routing fan-out, the custody serve door, the cross-nest relay gate, succession targets) goes stale until a same-nest member happens to commit (`../behavior/conversation-rooms.md` § The home nest). The request carries `{ requesting_actor_id, room_id, members, policy_version, commit_seq }` — the same-nest `room.roster_report` body plus the requester the home binds; `commit_seq` is additive, and it is the home's own log position, since the member's commit rode the same relay to the home's log. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) **in place of** the same-nest door's routing-roster gate 1, and strictly stronger than it: the `channel_foreign_members` row is the home's own record of the Welcome it relayed and the binding it pinned, where a routing row is `channel.send`-self-registered and proves only knowledge of the channel id. Behind that gate the home runs **the same body its same-nest door runs** (`conversations_handlers::room_roster_report_apply`: roster validation, gate 0's provenance refusal of a floor-authoritative room, the gate 2 ratchet against the STORED roster, the commit-order guard of `../behavior/conversation-rooms.md` § The floor roster — whose authorship half reads the `requesting_actor_id` the home bound here as the reporter, and compares it against the sender the home recorded for the commit at that position, which for a foreign member's commit is the same bound id its `federation.channel.append` carried — the wholesale replace) — one body, so the two doors cannot drift, and the bootstrap bound (`conversation-rooms.md` § Implementation status today) does not widen: a relayed first report is admissible only from a Welcome-bound member naming itself, never from any routing-roster actor. Client leg: the distinct kind `fauna.conversations.room.roster_report_remote { room_id, nest_url, members, policy_version, commit_seq }` — distinct for the `channel.actors_remote` reason: an old own-nest ignoring an additive `nest_url` would run its same-nest door on a room it does not home and, the reporter being on its routing roster, bootstrap a **stray floor** as a clean success while the real home stayed stale — a silently wrong answer about membership; an unknown kind fails loud and the seam tallies the report undelivered. The glue picks the kind where the read does, from the channel's recorded home (`RoomRosterReport.home_nest_url`). The relaying nest forwards the position and the ack and stores none of it: it holds no room record for a room homed elsewhere, and a roster it cached would be a membership question it has no authority over. Replay-safe on both hops (a wholesale replace is idempotent). |
| Conversation **room-post verdict read** *(BUILT 2026-09-11; ratified 2026-09-11)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.room_labels.fetch` | the **verdict-read twin of `channel.fetch`** for the one room read that had none, and the piece that puts a room post's badge on a foreign member's card: a room *message*'s verdicts ride `channel.fetch`'s page, which already relays, but a room **post** is its author's ordinary post — it reaches every follower through `fauna.posts.get`, the feed pages and the deep-link door, none of them floor-gated because the bytes are sealed — so its verdicts take a post-scoped door of their own that no envelope read carries ([`../behavior/restricted-posts.md`](../behavior/restricted-posts.md) § Encryption at rest → *Room-restricted — the ruling* → *Built* detail (v) owns that door). That door resolves each post's room from the serving nest's **own** reception-pass map, which only the room's home nest writes (`room_post_view::index_room_post` runs on the nest that *stores* the post), so a member homed elsewhere resolved nothing and was answered empty — and empty is deliberately indistinguishable from “nobody labelled this post”, so the card lost its badge silently rather than loudly. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) — a room id **is** its channel id — **and the floor on top of it**, unlike the *room roster read* row above. That is not a fresh decision but the verdict plane's standing one: `conversations_handlers::page_verdicts` is already the one home for both rules, shared by the same-nest `channel.fetch` and its federated twin, so the **message** verdict read across this very relay stacks `is_live_floor_member` too, and the two verdict reads may not drift on who a verdict reaches. The roster read's ratified refusal to stack a seat check cannot apply here: it feared denying the newest-seated member off a member-*reported* mirror, and `is_live_floor_member` refuses any room that is not floor-authoritative outright (`db::rooms::RoomRecord::is_floor_authoritative` — “the nest's own ceremonies write its roster”), so the only rooms it passes are ones whose floor this nest writes itself. ⚠ **The request carries `{ requesting_actor_id, room_id, post_ids }` — `room_id` where the same-nest request deliberately has none**, and that difference is the gate's: the same-nest read is post-scoped precisely so a caller need not know which room indexed a post, while the structural gate needs the channel id before anything is resolved. The requester always holds it (a room post names its room in its own `KeyAccess::Room` arm, which is what it opened the post by), and naming it also **narrows** the read — the home answers only for posts its own map assigns to *that* room, so one request cannot fish across rooms. Behind the gate the home runs **the same body its same-nest door runs** (`posts_handlers::room_labels_for`, narrowed to the named room), so a foreign member reads exactly what a member homed here reads. Read-only on every hop: serving a verdict registers nothing and mints nothing. Client leg: the distinct kind `fauna.posts.room_labels_remote { room_id, post_ids, nest_url }` — distinct for the `channel.actors_remote` reason, since an old own-nest ignoring an additive `nest_url` would answer from its own reception-pass map, which indexes no post of a room it does not home, as a clean **empty success** the reader cannot tell from “nobody labelled this post”; an unknown kind fails loud, and the feed's best-effort read then keeps the card's own labels. The glue picks the kind where the conversations plane picks it, from the channel's recorded home (the `RoomPostKeys::room_home_nest_url` seam over `ChannelHome`). The relaying nest forwards the reply and stores none of it: it holds no room record, no map row and no verdict row for a room homed elsewhere, and a verdict it cached would be a floor question it has no authority over. |
| Conversation **room leave** *(BUILT 2026-09-11; ratified 2026-09-11)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.room.leave` | the **self-scoped twin of the roster report above**, the room plane's first *mutating self-scoped* relay, and the piece that gives a member homed elsewhere a departure at all: [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization grants *leave (remove self)* to every role but owner with no homing carve-out, yet `room.leave` is a same-nest door and a leaver's own nest holds no room record for a room homed elsewhere, so it answers "no such room" (§ The home nest — every other nest relays). What a departing foreign member could actually reach was the generic `channel.leave` row above, which drops the relay binding and never touches the floor — so the seat outlived the departure. That residue is not inert: the roster-coverage gate **obliges** every later generation mint to wrap to a live floor entry carrying a reception key, so the room could not mint afterwards without handing the generation to a member who had left; and `room.invite` refuses a principal that is already a member, so the same ghost blocked the re-admission that would have healed it, while the departed member — binding gone — could not even read the floor it was still on. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`), and **self-scoped past it** — the body retires the *requesting* actor's seat and nothing else, so a peer nest can end only its own members' membership, never somebody else's. Behind the gate the home runs **the same body its same-nest door runs** (`conversations_handlers::room_leave_apply`: the owner's refusal — a room is never owner-less, unreachable here because a room is born on its owner's home and a transfer re-homes it — then — on a floor-authoritative room only — the binding purge, then the unseat; an end-to-end room's departure moves the seat alone, since that class severs by reading the floor, [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization → *Leaving — the mechanism*), so the two doors cannot drift, and the generic `channel.leave` runs it too on a room. **Two acts in the ratified fail-closed order**, `room.remove`'s verbatim ([`../behavior/community-rooms.md`](../behavior/community-rooms.md) § Implementation status today → *A removal severs*): purge the binding **before** the unseat, because the purge reads no floor, so a failed purge changes nothing and a purge over a failed unseat leaves a seat with no relayed reach — tighter than it was, never leakier. The one way this door's residue differs from the removal's is that a *self*-leave's own authorization is the binding it just purged, so its own retry is refused at the gate rather than converging; `room.remove` from any remaining owner or admin — a door that reads a rank, not a binding — is what converges it. **No rotation**, and that is a reason rather than an omission (same §, the leave paragraph): a leaver holds no mint authority, and a mint it could build would still wrap to itself. **Idempotent**, as the same-nest door is too since 2026-09-23: a caller already off the floor is answered with the live count, because this is the door a §4.D re-send arrives at and a refusal here would report a departure that landed as a failure that never happened; the gate still fails loud for a member this nest never bound. Client leg: the distinct kind `fauna.conversations.room.leave_remote { room_id, nest_url }` — distinct for the `channel.actors_remote` reason, since an old own-nest ignoring an additive `nest_url` would run its same-nest door on a room it does not home and answer "no such room", an `invalid_params` the seam cannot tell from a mistyped room id, leaving the leaver believing a departure failed for a reason it could fix while the real home kept them seated; an unknown kind fails loud. It is also the one relayed twin whose `forbid_replay` deliberately differs from its door's (`false` where `room.leave` is `true`), for the idempotence above. The glue picks the kind where the roster relays pick it, from the channel's recorded home (`ChannelHome`). The relaying nest forwards the ack and stores none of it: it holds no room record for a room homed elsewhere, and a seat it mirrored would be a membership question it has no authority over. |
| Conversation **room invite delivery** *(BUILT 2026-09-26; ratified 2026-09-26)* | **(none — channel-only, net-new)** | **nest-signature**; the recipient's own reach policy against the SIGNED inviter | `fauna.federation.conversation.room.invite` | the **room home's push of a community-room knock** to an invitee homed on the receiving nest ([`../behavior/room-invitations.md`](../behavior/room-invitations.md) § Join rules and invites → *A cross-nest invitation*, ruling 1–2). The request carries `{ recipient_actor_id, signed_invite, origin_nest_url }` — the inviter's signed `SignedRoomInvite` verbatim (the record names the room, the invitee, the role and the policy version, and is what the receiver verifies before delivering a word of it) plus the home's self-declared address, honoured only against the connection's verified identity (`federation_handlers::resolve_origin_home_url`, one helper shared with the Welcome relay's `nest_url`). The receiver refuses an unregistered recipient (F10), a record that does not verify or names another invitee, and — **unlike a Welcome, whose wire carries no signed sender** — runs the recipient's reach policy in full over the inviter (`conversations_handlers::conversation_initiation_reach_gate` at `Federation` origin: the floor's `federation_contact` pillar, the inbox mode against the contact edge), then delivers one `InboxKind::RoomInvite` envelope under the recipient's quota with `room_node` bound to the verified home, and stores no room record and no invitation row. Why a kind of its own: `inbox.deliver`'s wire is a signed contact-request pair with its own verification, and `welcome.deliver` fails the reach floor closed for want of a signed sender. **`forbid_replay=true`** on `inbox.deliver`'s grounds (a delivery appends a row and charges quota per call); the home records its `room_invites` row BEFORE originating and consumes it on a failed delivery, so a retry re-delivers exactly one knock. Originated by `room.invite` itself when `invitee_node` is non-empty (`conversations_handlers::room_invite_deliver_abroad`); the home's own reach gate is deliberately not run for a foreign invitee. |
| Conversation **room accept** *(BUILT 2026-09-26; ratified 2026-09-26)* | **(none — channel-only, net-new)** | **nest-signature** + **the delivered invitation names the origin as the invitee's home** (NOT the foreign-member binding) | `fauna.federation.conversation.room.accept` | the **seating twin of the room leave above**, and the one relayed room door whose gate is not `require_foreign_member` — because the `channel_foreign_members` row is what this door WRITES, and writing it at delivery time would open the binding-only doors (the relayed roster read above all) to an invitee who never accepted. The request carries `{ requesting_actor_id, room_id, reception_pubkey }` (the same-nest `room.accept_invite` body plus the requester the home binds). Gate: the home's `room_invites` row for `(room, requester)` must carry the verified `nest_id` the invitation was DELIVERED to — resolved from the delivery dial's handshake, never inviter-declared (`db/rooms.rs::room_invite_home_binding`) — so only the nest an invitation went to can seat that invitee, and self-scoped past it. Behind the gate the home runs **the same body its same-nest door runs** (`conversations_handlers::room_accept_apply`: the standing-offer judgement, the policy-version compare-and-swap seating, the lapse) and THEN inserts the binding with `InsertOnly` power — **seat first, bind second**, the removal's purge-then-unseat inverted for the inverted reason (a seat without reach is the tighter half-state; a binding without a seat would serve the floor to a non-member). **Idempotent**, `room.leave`'s posture: a requester whose invitation from this nest is already accepted and who holds a live seat is answered with its role and its binding re-asserted, which also converges the half-landed "seated, not yet bound" state; a consumed invitation, or an accepted one whose seat was since removed (`unseat_room_member` clears the row), is refused as a stale same-nest accept is. No routing-roster row for a foreign member (pull-only, `../behavior/community-rooms.md` § Implementation status today). Client leg: the distinct kind `fauna.conversations.room.accept_invite_remote { room_id, nest_url, reception_pubkey }` — distinct for the `channel.actors_remote` reason: an old own-nest ignoring an additive `nest_url` would run its same-nest door on a room it does not home and answer "no invitation pending", indistinguishable from a genuine lapse; an unknown kind fails loud. `forbid_replay=false` on the client leg as on `leave_remote`, for the idempotence above. The glue picks the kind off the knock's `room_node` (`RoomCeremonyRpc::room_accept_invite`'s `home_nest_url`), and the shared manager records that URL as the channel's home so every later relay of the room (fetch, send, roster, generations, leave) is routed by the one `ChannelHome` signal. The relaying nest forwards the ack and stores none of it. Proof: `conformance_cross_nest_conversations_client.rs::a_cross_nest_invitation_seats_the_foreign_member_through_its_own_nest`. |
| Conversation **room invite issue** *(BUILT 2026-09-26; ratified 2026-09-26)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id`, and the signed act's signer IS the requesting actor | `fauna.federation.conversation.room.invite_issue` | the **issuing twin of the room accept above**, and the leg that gives a member homed elsewhere the *invite* that `member-invite` grants every member ([`../behavior/room-invitations.md`](../behavior/room-invitations.md) § Join rules and invites → *A cross-nest invitation*, the closing paragraph): `room.invite` is a same-nest door, and an inviter's own nest holds no room record for a room homed elsewhere, so it answers "no such room". The request carries `{ requesting_actor_id, signed_invite, invitee_node, origin_nest_url }` — the same-nest `room.invite` body plus the requester the home binds and the relaying nest's self-declared address. Gate: the signed act's signer must be the requesting actor (`conversations_handlers::verify_room_invite_act`, the same-nest door's own first act, run on the relaying nest too so it never forwards an act under its member's name), then `require_foreign_member` verbatim — that actor holds a live binding on the room from the verified origin; a removal purges the binding with the seat on this class, and the body's seat read refuses whoever a purge missed. Behind the gate the home runs **the same body its same-nest door runs** (`conversations_handlers::room_invite_apply`: the floor-authoritative check, the join-rule judgement, the already-a-member refusal, the delivery), so the two doors cannot drift and a foreign member is granted and refused exactly as a member homed there. **Three deliveries, one body:** `invitee_node` keeps its inviter-side meaning (the invitee's home as the inviter knows it, empty for the inviter's own nest) and picks the arm — empty is resolved by the home to the relaying nest's **verified** identity (`resolve_origin_home_url`, the helper the Welcome and invite-delivery relays share; `origin_nest_url` is honoured only against that identity) and served by the *room invite delivery* push above, aimed back at the relaying nest; a node naming the home itself is a same-nest delivery into the invitee's own inbox under the invitee's reach policy at `Federation` origin (a nest never dials itself); any other node is the same push to that third nest. The delivery's dial records the invitee's home as before, so each invitee's later accept is gated exactly as the *room accept* row says. **`forbid_replay=true`** on both hops, `room.invite`'s own posture and unlike the leave and accept twins: a delivery per call, so the inviter re-issues and the pending invitation is refreshed rather than duplicated. Client leg: the distinct kind `fauna.conversations.room.invite_remote { invite, nest_url, invitee_node }` — distinct for the `channel.actors_remote` reason: an old own-nest ignoring an additive `nest_url` would run its same-nest door on a room it does not home and answer "no such room", an `invalid_params` the seam cannot tell from a mistyped room id; an unknown kind fails loud. The glue picks the kind off the channel's recorded home (`ChannelHome`), as `leave_remote` does. The relaying nest forwards the ack and stores none of it. Proof: `conformance_cross_nest_conversations_client.rs::a_foreign_member_invites_through_its_own_nest_under_member_invite`. |
| Folder **content plane** *(BUILT — read kinds 2026-07-19, write kinds 2026-07-20; ratified 2026-07-18)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` (+ `access == 'writer'` on the write kinds) | `fauna.federation.folder.{changes.fetch,content_key.fetch,actors.fetch,changes.record,write_token.mint}` | cross-nest shared-set discovery + write relay (`actors.fetch` — the writer roster read, BUILT 2026-09-29; § Cross-nest shared folders + channel append → *The cross-nest writer roster read*); chunk/manifest **bytes never ride this channel** (open by-hash HTTPS bulk plane, `transport.md` § carve-out; the write side gets a short-lived write-only bulk token from `write_token.mint`). **With `folder.read_token.mint` (BUILT 2026-10-03 — the read-scoped twin of `write_token.mint`, member gate alone) this is the whole BUILT folder federation kind set — no `lease`/`conflicts` twin (v1 scope; conflicts a named gap); two more kinds, `folder.serve.announce` and `folder.chunk.wanted`, carry relay serving across nests (ruled 2026-10-01, BUILT 2026-10-04; *Relay serving across nests* in the section).** § Cross-nest shared folders + channel append. |
| Folder **public read plane** *(BUILT 2026-08-18; ratified 2026-08-18)* | **(none — channel-only, net-new)** | **nest-signature** for attribution + throttle only — the authorization is the ADDRESSED FOLDER's own `audience == 'public'`, never the caller | `fauna.federation.folder.public.fetch` | the publicly-synced follow's read leg (§ The public folder read plane owns the gate reasoning; behavior: [`../behavior/folders.md`](../behavior/folders.md) § Publicly-synced follow). **The one kind here whose request carries NO requesting actor** — deliberate, not an omission: there is no membership to check, so a follower's identity never crosses the wire and the home nest sees only the requesting nest + source IP (its throttle keys). **Deliberately NOT an audience arm on `folder.changes.fetch`:** an OR-ed world-readability branch inside the member gate is one bug away from opening member data, so this kind's gate is the inverse shape — *serve iff the addressed row's current `audience == 'public'`, else `not_found`* (`folder_public::resolve_public_folder`, the one core both this kind and its client twin `fauna.folders.public.fetch` authorize through). Same reasoning nest-locally: `folder_authz::can_read_folder` keeps its membership-only behavior; `FolderReadGrant` gained a `Public` variant that names the grant but is never *returned* by that resolver. **Zero state, zero metering:** the handler writes nothing (no follower rows — not enumerable, not floodable into disk) and charges nothing. Rows are floor-filtered (`folders.public_floor_seq`, schema v44 — nothing recorded before the latest flip-to-public is served) and **stripped** of `device_id`/`author_actor_id`/`path_sealed`/`content_key_version`. Bulk bytes stay off this channel per the standing carve-out: manifests/chunks GET by hash on the open bulk plane, plaintext for a public folder. A flip-back makes the next read answer `not_found` — that IS the revoke. |
| Trend exchange/export *(BUILT 2026-07-14; ratified 2026-07-12)* | **(none — channel-only, net-new)** | **nest-signature** | `fauna.federation.trends.{exchange,export}` | k-gated trend-velocity aggregates (`behavior/trending.md` owns the mechanism — landed there through 2026-07-14, including the exchange-originator legs + the import-triggered `post.get` fetch). **The one ratified, scoped exception to the non-scaling rule (frame D7, 2026-07-12):** distinct chosen peers contribute presence-only buckets that sum on a **capped log ramp** (ceiling 300‰) — permitted because trending is promote-only into a user-chosen feed; claimed magnitudes still buy nothing, and reports keep the flat bucket verbatim. Open-federation, per-origin throttled, small entry cap + peer-row TTL. |
| Nostr proxy-delegation relay *(BUILT 2026-07-22; ratified 2026-07-22)* | **(none — channel-only, net-new)** | **nest-signature** + `nostr_push` pairing capability (`pairing_has_capability`) | `fauna.federation.sync.{nostr_push,nostr_pull}` | Phase-2 head↔public-box proxy relay (`ui/nostr.md` § The bridging gate → Phase 2 owns the mechanism): the paired head **pushes** its `origin='ingest'` rows public-ward (the RPC reply is the ack; auto-provisions the public box's `signing_mode='proxied'` account row, refusing to clobber a deposited `nsec` or a different pubkey) and **pulls** externally-deposited rows head-ward, cursor-parameterized on the head-persisted compound `(stored_at, id)`, non-destructive (the public box keeps serving what it relays — no purge, hence no ack kind). Wire carries only signed public Nostr wire JSON + opaque kind-1059 wraps, never key material. Feature-gated (`nostr`; on in the shipping build — `Dockerfile`'s `--features bluesky,nostr,activitypub`). Registered 2026-07-22 (`register_sync_federation_handlers`). |
| Nest-writer backup plane *(BUILT 2026-07-23; ratified 2026-07-23)* | **(none — channel-only, net-new)** | **nest-signature** + a user-minted **nest-writer grant** (`origin_nest_id` == the owner's granted writer pubkey, unrevoked; gate `require_backup_writer`) | `fauna.federation.backup.{changes.record,write_token.mint}` | the cross-location segment-backup writer plane (§ Nest-writer backup plane owns it): a source nest's in-process coordinator relays an owner's segment-backup custody into the owner's reserved custody-copy set here (resolved-or-created lazily behind the gate, same `record_change_core` as every write plane, owner-charged) and mints a short-lived write-only bulk token (`NestBackupWrite` purpose, 600 s Rust constant) for direct HTTPS chunk POSTs — **bytes never ride this channel**. **NOT pairing-gated:** `is_paired` stays scoped to the private nest-sync surface; the user-minted grant is the allowlist entry, so revocation at the destination freezes the source's writes with the source fully hostile. Registered 2026-07-23 (`register_backup_federation_handlers`). |
| Identity-succession **propagation push** *(BUILT 2026-07-29; anchor-rule hardening 2026-07-29; ratified 2026-07-23)* | **(none — channel-only, net-new)** | **nest-signature** (attribution + throttle only — **no authorization gate**) | `fauna.federation.succession.push` | a home nest tells a peer that an identity the peer holds residue about was superseded (`succession-propagation.md` § Propagation owns the mechanism **and the anchor rule**). **The one mutating kind here with no gate, deliberately — because the push is a HINT whose payload is never trusted:** the receiver takes only *which identity to check* from the request, then verifies against the chain it fetches from its own recorded anchor for that identity (oldest addressable `channel_foreign_members` binding + its persisted `foreign_recovery_heads` head), refusing identities it holds no anchor for; hint-triggered verifies are throttled. A gate would add no security — the trust decision never rests on the sender. (The request's former `chain` field, kept for pre-hardening receivers that verified from the payload, left the wire with the 2026-09-24 compat-remnant sweep.) On acceptance the peer writes its own `actor_successions` row and re-points `channel_foreign_members` + contact edges; a statement naming a **locally-homed** identity is refused (only the client-facing `fauna.recovery.succession.submit` may supersede a local account, because only it re-points the account). Pushed best-effort to peers resolved from the residue rows' `home_nest_id` mapped through the nest-wide `nest_addresses` id→URL directory — each target identity's known addresses, proven-first and per-identity-fair capped, never the membership row's own overwritable address column (`succession-propagation.md` § Propagation owns the rule); the **pull** direction has no federation kind — a peer reads the pre-identity `fauna.recovery.{succession.lookup,registration.chain}` from its anchor over an anonymous client connection, the same way `federation_pool::resolve_peer_nest_id` reads `fauna.nest.info`. Registered 2026-07-29 (`register_succession_federation_handlers`). |

**Ratified target-state, not yet built, deliberately NOT a table row until registered (2026-07-23): identity-succession propagation.** A push kind carrying a user's RecoveryKey-signed succession statement to every peer holding residue for that actor, plus a pre-identity pull of the succession chain from the old identity's home nest; a verifying peer re-points its remote-identity residue (`channel_foreign_members`, contact edges, cached key packages) and refuses *new* content signed by the superseded key. Statement shape, verification rule, and consumer obligations: [`../behavior/succession-propagation.md`](../behavior/succession-propagation.md) § Propagation — that doc owns the ceremony; this table gains the row when the kinds register.

**The exchange originator plane is BUILT (2026-07-13; the 2026-07-12 distributed-moderation plan, Phase 1).** The `exchange_originator` worker (`bins/fauna-nest/src/exchange_originator.rs`, spawned beside the discovery poller) originates the reports and trends pairs: per cycle it **pushes** this nest's local exportable aggregates (`*.exchange`) to each peer and **pulls** each peer's (`*.export`), importing through the *same* validation + peer-bucket functions the serving exchange handlers use (`federation_handlers::import_report_entries`, and `import_trend_entries` for trends — one import path, both directions). Peer set, re-assembled every cycle (deduped, self-excluded): on a private nest, the distinct `nest_url`s of the live pairing rows whose actor is an admin of the nest (the deployment's own topology — `nest/private-mode.md` § Pairing Flow), distinct `feed_contributors` nests, and prior exchange partners (`exchange_peers`, recorded at origination time after a successful cycle — the channel handshake carries no origin URL, so inbound peers are unrecordable; the push+pull cycle keeps data flow bidirectional anyway). Triggers: startup, an ~hourly tick, debounced local-aggregate transitions (report capture/withdrawal, report-share opt-out), and peering events (a contributor grant/seed adding a new peer URL); every cadence constant is hard-coded Rust. Throttled both directions: a per-peer min-exchange-interval on origination (throttled pushes are delayed, never dropped) plus the serving side's per-nest `federation_rate_limit`. Proven by the two-nest cycle conformance test (push + pull + partner memory + no-laundering + throttle) and the two-binary tier_3 e2e (`test_federation_exchange_originator.py`: k=3 reports on nest A move to nest B's `peer_content_reports` + flat 100‰ bus row with no manual federation RPC). The trends pair lands on this same plane.

**The reputation leg is ruled removed, whole, before the 2026-10 baseline (2026-10-01; a reading of the dead-shape ratification the user accepted with the 2026-10-01 survey — [`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Still queued, and what stays; BUILT 2026-10-02).** `fauna.federation.reputation.{exchange,export}`, the exchange originator's reputation step, the `sender_reputation` table and its aggregate go together. The reports pair and the trends pair stay: each has a local source. The grounds: **(1) Nothing feeds the leg and nothing reads it.** With the algorithm-service mesh gone ([`core-client-kind-catalog.md`](core-client-kind-catalog.md) § Algorithm & Reputation) the table's one writer is the peer import and its one reader the peer export, so on a fleet of unmodified nests every table is empty and the pair carries nothing — the only party that can put a row on this wire is a nest that is not running this code. No ranking, mail-scoring, reach or moderation path consults the aggregate. **(2) No source can be named for this shape under the ratified frame.** A human report may not feed it ([`../behavior/moderation.md`](../behavior/moderation.md) § User-initiated reporting: a per-reporter attributed reputation signal would turn reporting into a reputation weapon); the nest classifies nothing ([`content-scoring.md`](content-scoring.md), the placement rule); and the one scorer at a capability position that sees a sender, the mail perimeter, sees a mail address and scores a message onto the scores bus, while this table is keyed on an actor id. The leg's own export rule predates [`content-moderation-and-ranking.md`](content-moderation-and-ranking.md) § Distributed report sharing and meets none of its invariants: one local reporter at full confidence clears the 0.3 export floor (no k-gate), there is no opt-in, and what crosses names a sender, from a table that keeps each reporter's id against that sender. The cross-user signal that frame ratified — many people flagged this same item — is the per-item report aggregate, and it crosses on the reports pair. **(3) Published, it binds the major to a door with a cost and no benefit.** Two open-federation kinds any signer may call; an import that writes one row per sender the peer names, with no per-peer row cap (the reports import carries one, `MAX_PEER_REPORT_ROWS`; this one never got it); and, once the mesh is gone, no production caller of the prune — unbounded growth at a hostile peer's choosing, in a table nothing reads. **What outlives the leg** is the invariant it was first written down for, which this section keeps owning: a peer's `nest_id` is self-minted and free, so nothing a peer claims may scale a local distinct-reporter count — a peer's whole contribution to an aggregate is one non-scaling bucket (the *Report exchange/export* row; trending's capped ramp is the one ratified exception) — and every peer-supplied value is validated at the import boundary, and dated by the receiving nest's own clock, before it can enter an aggregate. **What goes:** the two kinds with their handlers, wire types and boundary tests (`federation_handlers.rs`); `originate_reputation_{exchange,export}` (`federation_pool.rs`) and the originator's reputation push and pull (`exchange_originator.rs`); `db/sender_reputation.rs` and `db/reputation_exchange.rs` with `SenderReputationSummary`; and the `sender_reputation` table, dropped in one schema step under the dead-schema ruling, with its succession legs (the table's own move and the `reporter_actor` counterparty column — [`../behavior/succession-repoint-axis.md`](../behavior/succession-repoint-axis.md)), its export-registry entry ([`account-data-taxonomy.md`](account-data-taxonomy.md)) and its tests. **What stays:** `sender_behavior`, the DM-initiation profile — a different table, with a live writer and a live reader. A future cross-nest per-sender signal is a new design, made then under the invariants of [`content-moderation-and-ranking.md`](content-moderation-and-ranking.md) § Distributed report sharing; it does not revive these names. **Landed 2026-10-02**: the two kinds, the originator's push and pull, the store and `SenderReputationSummary` are gone, and schema 109 drops the table from an existing database.

**Two former rows are retired, not residue:** calendar invite deliver
(`fauna.federation.calendar.invite_deliver`, ex `POST /api/calendar-invite-deliver/{actor}`)
and remote RSVP deliver (`fauna.federation.event.rsvp_deliver`, ex
`POST /api/events/remote-rsvp-deliver`) were **deleted with the Decision-B § 4c
legacy-calendar cleanup** (2026-06-14) along with the whole legacy
`fauna.{events,calendars}.*` plaintext plane (`federation_handlers.rs:21-23`
records the retirement). Cross-nest scheduling now rides the encrypted CalDAV
store plus the conversations relay (the `channel.fetch` row above); that surface
is owned by `behavior/caldav-server.md` / `ui/events.md`.

✅ The inbox row's HTTP twin was **deleted 2026-06-09** — the **last** residue twin
(the others went in slice 5); the federation channel + `fauna.inbox.send` are now the
sole carriers. **Both legs of the migration:**
- *nest→nest:* the group-invite fan-out (`conversations_handlers`) *originates*
  `fauna.federation.inbox.deliver` (`federation_pool::originate_inbox_deliver`)
  instead of `reqwest`-POSTing the twin.
- *client→home-nest:* the authed bearer kind **`fauna.inbox.send`**
  (`inbox_handlers::send_handler`, gated `User`; landed 2026-06-06) — the client
  hands its **home** nest the signed tuple + recipient (+ the recipient's nest URL
  when cross-nest); the handler binds `cr.sender == caller` (a property the
  unauthenticated twin can't enforce), then local-delivers
  (`recipient_nest_url` None) or originates `fauna.federation.inbox.deliver` (Some).
  Mirrors the `fauna.events.remote_rsvp` → home-nest-originates precedent (row above).

**Client-side peer discovery for a cross-nest knock** (built 2026-09-20; the piece `fauna.inbox.send` waited on since 2026-06-06). The kind always accepted a `recipient_nest_url`, and the nest always branched on it — what no app could do was *produce* one. A client now does, without any new discovery mechanism and without a new kind: the knock rides `fauna.federation.inbox.deliver` as its transport exactly as the group-invite fan-out does. Two shared-Rust helpers own the decision and the derivation, so no app re-derives either (priorities #1/#2): `fauna_core::resolve::is_foreign_handle_domain(typed_domain, home_domain)` rules a typed `@domain` foreign by comparing it ASCII-case-insensitively against the domain the caller's *own* nest echoed on a same-nest `fauna.actor.by_handle` (an absent home domain reads as foreign, so a failed local probe never silently resolves `bob@other.test` to a local `bob`); `fauna_provisioning::probe::peer_nest_url` then derives the peer's base URL by the same `resolve_handle_domain` rule the conversations recipient picker's relay `nest_url` uses. The client opens an **anonymous connection straight to that authority** and runs `fauna.actor.by_handle` there — the pre-identity discovery hop, not a home-nest relay. It is deliberately **not** the nest-proxied `fauna.nest.resolve` kind: that one refuses loopback, IP-literal and `.local` authorities by design (`discovery_core.rs`), so it can reach neither a peer on a LAN address nor any two-nest test topology. The resolved URL is then carried on the find result and handed to `fauna.inbox.send`, whose `Some` arm originates the deliver; the receiving nest runs the identical `deliver_inbox_payload_core` with `ArrivalOrigin::Federation`, so a stranger's cross-nest knock takes the recipient's own `InboxMode` route (`allow_knock` → `store_knock`) exactly as a same-nest one would — there is no federation-specific knock policy, which is why this needed no new kind.

tui ships it as the lead app; the other six already carry `recipient_nest_url` on their transports and owe only the page wiring. Witnesses: `bins/fauna-nest/tests/conformance_federation_channel.rs` `inbox_send_cross_nest_stores_a_knock_for_a_stranger` (the `allow_knock` + `ArrivalOrigin::Federation` arm, headless) and `tests/e2e-unified/tests/test_contacts_cross_nest_knock.py` (the app journey, two real nests; exclusion class (8) — one nest dialing another is refused in docker as non-global, so the catalog cell comes from standalone and live runs).

The unauthenticated twin could reach **same-nest** recipients only — cross-nest social
sends were never routed through the home nest (wrong under Spec Y2 — clients reach remote
actors *through* their home nest). `fauna.inbox.send` is the full Spec-Y2 capability; each
app that had a direct POST (linux / apple / windows) moved onto it; web's direct sender
was dead-code-removed, and android's was P2P-signal-only and removed (never a `(CR,Post)`
send). With all migrants resolved, the twin — `routes::post_inbox` + its route, the
HTTP-only `inbox_outcome_to_response` mapping, and the bearer-exemption test
`post_inbox_still_works_without_bearer` — was **deleted 2026-06-09**; the shared
`deliver_inbox_payload_core` / `verify_inbox_payload` / `inbox_payload_sender` are kept
(the channel rides them).

**Per-app fan-out status:**
- **linux — DONE** (2026-06-07). `apps/fauna-linux/src/client.rs` `build_and_send`
  composes the signed `(ContactRequest, Post)` tuple with the **shared writer**
  `fauna_client_core::email::build_signed_email` (the single canonical composer — same
  one web uses via wasm and apple/android/windows via the UniFFI `build_signed_email`
  export) and hands it to the home nest via `fauna_client_inbox::InboxClient::send` over
  the bearer WS-RPC connection (`recipient_nest_url=None`, faithful same-nest; cross-nest
  awaits client-side peer discovery). No direct `POST /api/v1/inbox/{actor}` and no inline
  composition remain in the linux app.
- **apple — DONE** (2026-06-08). `apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/APIClient.swift`
  `sendToInbox` composes the signed `(ContactRequest, Post)` tuple with the same shared writer (the
  UniFFI `buildSignedEmail` export) and hands it to the home nest via the `FfiInboxClient.send` seam
  over the bearer WS-RPC connection (`recipientNestUrl=nil`, faithful same-nest). The sole caller is
  the watchOS reply path (`Fauna-watchOS/Views/MessageDetailView.swift`); no direct
  `POST /api/v1/inbox/{actor}` and no inline composition remain. (The migration also retired a dangling
  `build_reply` call that never had a backing FFI export.)
- **windows — DONE** (2026-06-09). `apps/fauna-windows/FaunaApp/FaunaApp.Core/Services/NestRpcClient.cs`
  `SendKnockAsync` composes the signed `(ContactRequest, Post)` tuple with the same shared writer (the
  UniFFI `BuildSignedEmail` export, via the `BuildKnockPayload` helper — `"Knock"` / `"Contact request"`)
  and hands it to the home nest via the `FfiInboxClient.Send` seam over the bearer WS-RPC connection
  (`recipientNestUrl=null`, faithful same-nest). The three add-contact call sites
  (`ContactsViewModel.AddFoundContactAsync` + `ContactsPage`'s two handlers) now route through the
  `INestRpcClient` seam; no direct `POST /api/v1/inbox/{actor}` and no inline composition remain.
  (The pre-migration windows knock was *already* broken against `verify_inbox_payload` — it POSTed an
  off-spec JSON `{from,handle,type:"knock"}`, never the signed tuple — so this also fixed a latent bug.)
- **android — off-gate (no migration).** Android's only inbox POST was a P2P WireGuard
  **signal-JSON** send (never a `(CR, Post)` social send); it was **removed** (
  2026-06-07), so android holds no add-contact twin to migrate and does **not** gate the twin
  deletion. (A future android add-contact-by-id UI would use the same shared
  `build_signed_email` → `FfiInboxClient::send` seam — a feature gap, not residue.) This
  corrects the prior "android — the lone remaining client" claim, which contradicted both the
  code and the tracked-internally item 3 ("linux ✅, android off-gate, apple ✅; gated on
  windows only").

**Out of the Fauna-channel scope** (do **not** migrate): cross-nest delivery to a
**third-party ActivityPub** actor rides `/ap/users/{username}/inbox`
(`activitypub/inbox_routes.rs`), JSON-LD over HTTP Signatures — ActivityPub stays
HTTP+JSON-LD (§ Scope) because the far end is not a Fauna binary. This is **distinct**
from the Fauna-native `/api/v1/inbox/{actor}` (`routes::post_inbox`), which *is* a
Fauna↔Fauna federation surface and migrates to `fauna.federation.inbox.deliver` (the
row above) — an earlier revision wrongly conflated the two and excluded the inbox.
Cross-nest **knock policy** (which `InboxMode` admits a knock, the app send-knock
UX) is tracked internally (coordinate-don't-merge); that
track rides `fauna.federation.inbox.deliver` as its **transport** (no separate kind).
(The former cross-nest invitation read `fauna.events.inbox_invitations` was a
**local client read** of the caller's own inbox, never federation; it was retired
with the Decision-B § 4c legacy-calendar cleanup — § residue-table note above.)

**Reachability of a handle** is **not** a separate cross-nest call: it is folded
into the anonymous `by_handle` reply as a boolean (§ Key packages).

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
  [`../behavior/file-sync.md`](../behavior/file-sync.md) § Relay serving → *A
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
  `security.md` § Transport trust, the federation-granted Axis-2 row;
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
  from it (owner: `../behavior/on-demand-files.md` § Shared sets on a capability
  host → *One mechanism*, question 2). Same absent-asserts-nothing semantics; an
  input to the member's own HOLD only — the enforcement stays this nest's
  `stale_content_key` refusal, so a wrong value costs a held or refused write,
  never a seal under a rotated-past generation.
- **The cross-nest owner label (ratified 2026-10-03; BUILT 2026-10-05).** A set shared across nests names its owner on
  two member surfaces — the pending-share knock ("Shared by ‹…›",
  `../behavior/value-formatting.md` § Account display label) and the shared
  set's display name in the member's file browser (`docs (alice)`,
  `../behavior/on-demand-files.md` § Shared sets on a capability host,
  decision 3) — and a cross-nest member's own nest holds no `users` row for
  that owner, so without a label both fall back (an unknown sharer; the
  home nest's host). The id→handle bullet above forecloses the obvious fix: no nest asks
  the owner's nest what handle an actor wears. The ratified shape is the
  announce's, applied to the folder plane:
  - **Carrier — the home nest volunteers, on the two stamps it already
    originates.** The set's home nest is the authority for its owner's handle
    and already stamps the Welcome relay and every federated folder read
    reply nest-authoritatively (`set_name`, `access`, `home_nest_actor_id` —
    the *Recipient-side access discovery* bullet). It adds the owner's handle
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
- **Throttling:** all kinds inherit the channel's per-IP + per-nest throttles;
  nothing new to configure (no-operator invariant — cadences are Rust constants).

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

## Nest-writer backup plane (ratified 2026-07-23; auth plane BUILT 2026-07-23)

Target state for the cross-location segment-backup writer. `segment-backup-protocol.md`
§ Cross-location backup protocol owns writer identity, custody mechanics, and the
grace-window/audit model; this section owns the kinds and gates. The source nest's
in-process backup coordinator authenticates to each destination over **this channel's
ordinary handshake** — no pairing (`is_paired` stays scoped to the private nest-sync
surface), no new envelope. Gate: a user-minted **nest-writer grant** row the
destination wrote when the owner's client registered it — serve iff the verified
`origin_nest_id` equals the grant's writer nest pubkey for the owner scope and the
grant is unrevoked; the same "signature is attribution, authorization is state this
nest wrote" shape as the `channel.fetch` gate. Both kinds refuse with the typed
reason `fauna.backup.writer_not_seated` (ruled and built 2026-10-01; an untyped
`forbidden` before), so the calling coordinator can report a refusal as a refusal — an
owner has one such row per destination, and `segment-backup-protocol.md` § Cross-location
backup protocol → *The writer seat* owns that rule and how the row moves. Kinds:

- `fauna.federation.backup.changes.record` — custody record relay into the owner's
  reserved backup set (the `backup_custody` projection); exactly-once **by content**
  on the same tuple contract as `folder.changes.record`; owner-charged metering.
- `fauna.federation.backup.write_token.mint` — short-lived, write-only bulk-byte
  token for direct HTTPS chunk/manifest POSTs (the `BulkWriteAuth` token arm; TTL a
  Rust constant, `NEST_BACKUP_WRITE_TOKEN_TTL_SECS = 600`). Bulk bytes never ride this
  channel; grant revocation refuses the next *mint*, and an outstanding token's
  residual window is one TTL — the accepted contract restated from the folder
  write plane.

The owner's client registers/revokes/lists the grant and lists/restores custody
generations over its **own** authed client↔destination connection (USER-class
kinds, the `fauna.backup.writer_grant.*` and `fauna.backup.generation.*` families)
— never over this channel — so the freeze-the-backup affordance works with the
source nest fully hostile.

**Build status (2026-07-23).** The **auth plane is BUILT** (nest-side segment
backup slice 3, first pass): both federation kinds above
(`bins/fauna-nest/src/federation_handlers.rs::register_backup_federation_handlers`,
gate `require_backup_writer`), the destination-side grant store
(`db/backup_writer_grants.rs`, `MIGRATIONS_BACKUP_WRITER_GRANTS`), and the
client↔destination `fauna.backup.writer_grant.{register,revoke,list}` USER-class
family (`backup_handlers.rs`). The record relay resolves-or-creates the owner's
reserved custody-copy set lazily behind the gate and runs the same
`record_change_core` every write plane runs. Proven tier_3 end-to-end over a real
`fauna.federation.hello` handshake — `bins/fauna-nest/tests/conformance_backup_writer_grant.rs`
(the capstone dials with no pairing, is refused before the grant, served after).
The remaining pieces landed after this block was written: the **source-side
in-process coordinator** that originates these calls (2026-07-24), and the
**destination-side custody grace window `T = 30 d` + the
`fauna.backup.generation.{list,restore}` USER-class kinds** (2026-07-24) — both
owned by `segment-backup-protocol.md` § Cross-location backup protocol (the window's
mechanism: its § *Custody grace window (T)*).

## Key packages — privacy & exhaustion

- **Reachability probe = a boolean on `by_handle`, never a cross-nest count.** The
  anonymous `fauna.actor.by_handle` reply carries `addressable: bool` ("target has
  ≥1 usable key package, one-time **or** last-resort"). This leaks yes/no, not a
  number, and adds no new anonymous surface. The authed same-nest
  `fauna.conversations.keypackage.count` kind is unchanged (local top-up UX).
- **Last-resort key package (mandatory).** Every actor publishes one reusable
  last-resort key package at onboarding (standard MLS). `take_key_package` returns a
  one-time KP if the pool is non-empty, else the last-resort KP **without deleting
  it**. This (a) keeps a target reachable after its one-time pool is drained and
  (b) makes `addressable` ~equivalent to "exists", so the probe leaks nothing beyond
  what a successful `by_handle` already revealed.
- **Fetch exhaustion is the real hazard** (one-time KPs are consumed). Mitigated by
  the last-resort fallback, **per-originating-nest throttling**, and the
  **nest-signature gate** (fetch is never anonymous). Together these bound the rate
  and keep the target reachable.
- **`by_handle` discovery (handle→actor_id) is world-readable by design** — handles
  are public identifiers (like email / fediverse `@handle`). The anonymous discovery
  surface is per-source rate-limited to bound directory harvesting.

## Security — the anonymous & federation attack surface

Full analysis in the Spec Y2 doc § 8. Load-bearing rules:

- **Domain↔key binding rides TLS.** Because `nest_id` *is* the verifying key,
  nest-key discovery (`nest.info`) **MUST** run over authenticated TLS to the
  resolved domain; the cert binds domain→key. Without it a DNS-hijack/MITM could
  substitute a hostile `nest_id`. (DNSSEC / signed `.well-known` may harden later;
  TLS is the floor.) What "authenticated" means at the dial, and its two
  carve-outs, are § Peer-auth model → *Discovery trust rule* — enforced
  2026-09-25; before that the dial accepted any cert and this bullet was
  aspiration.
- **MLS, not the transport, carries end-to-end security.** Fetching a target's KP
  on a client's behalf is *supposed* to be possible (public material); MLS
  authenticates group membership end-to-end, so a relaying/MITM nest can neither
  read nor forge messages. The federation layer never holds the security guarantee.
- **Rate limiting is the primary DoS defense** (since signatures don't authorize):
  per-source on the anonymous discovery surface (`by_handle`, `handle.available`,
  `nest.resolve`), per-originating-nest **and per-source-IP** on the federation
  data plane (KP fetch, welcome/knock delivery, and every other
  `fauna.federation.*` kind). **The per-nest bucket can never be the only
  dimension** — a peer's `nest_id` is self-minted and free (§ Trust model), so it
  is not a scarce identity and cannot bound anything on its own (2026-06-23
  firewall-exposure security review). The invariant since:
  every federation request is checked against the PROXY-v2-resolved, /64-masked
  **source IP** first, before the
  per-nest bucket is even touched — `federation_channel.rs`'s `serve_request`
  step 2.5, a distinct key slot from the per-nest bucket so the two budgets never
  collide. The same review closed the companion `welcome.deliver` flood
  (reject delivery to a non-registered local actor before any write). All
  federation/anonymous limiter `DashMap`s are periodically swept (`main.rs`,
  `lib.rs`) so a rotating-source flood can't grow them unboundedly. Reuse
  `bins/fauna-nest/src/bridge_rate_limit.rs`. The auth-bootstrap kinds ride none
  of these plane throttles; the one bound on `fauna.auth.device_handshake` is the
  refusal-counting failed-credential throttle, owned with the upgrade half it
  shares by `transport-connection.md` § Abuse posture → *The failed-credential
  throttle*.
- **No server-side dereference of `nest_url`** — it is stored in the inbox envelope
  and used by the *client* to address its next hop; the server never fetches it (no
  SSRF). `nest.resolve` already rejects bare IPs / `localhost` / `.local` /
  `.internal` (`discovery_core.rs:34-35`). The cross-nest Welcome's inbox envelope is the
  canonical DAG-CBOR `InboxEnvelope { kind: Welcome, payload: WelcomeInbox }` (one `kind`
  tag across *all* `push_inbox` writes), per `core-client-kind-catalog.md` § Inbox & Messaging —
  "Durable inbox-apply consumer" layer 1 (built 2026-06-14; the old `serde_json`
  `{type:"welcome", …}` shape is gone). `nest_url` stays a field on that envelope (it is
  `WelcomeInbox.nest_url`, used by the client only); only the encoding + the cross-write
  uniformity changed.
- **Handle/version enumeration is an accepted, rate-limited property.** Handles are
  public, so handle/actor-id resolution stays open but is **per-source
  rate-limited** (`anonymous_rate_limit`, keyed on the real client IP) to bound
  bulk harvesting. Anonymous `nest.info`/`setup.status` version is **coarsened to
  `major.minor`** (anti-fingerprinting). The anonymous **secret-guess** surfaces —
  the one-time `claim_admin` and the in-band `invite_code.verify` — are
  rate-limited too: `claim_admin` per-source **and** with a global
  (source-independent) cap bounding a distributed brute-force, `invite_code.verify`
  per-source. The **claim-code entropy** was raised on 2026-05-31 and then
  **deliberately reduced on 2026-07-24** (a short code over an ambiguity-free
  alphabet, `fauna_core::claim_code`) — a **deliberate user
  decision** trading entropy for transcription comfort, since a claim code is read
  off a terminal and typed into an app by hand, once. **This makes the
  `claim_admin` throttles the primary bound again, not defense-in-depth** (they
  had been demoted to defense-in-depth by the longer code it replaced). Under the
  global attempt cap the keyspace is unexhaustible on any human timescale, and
  more so under the tighter per-source cap, so it remains unreachable —
  *because of* the throttles, not independently of them. Two
  consequences bind: **neither throttle may be loosened or removed without first
  restoring the code length**, and the global cap's documented availability
  tradeoff (an attacker saturating the bucket to *delay*, never take over, a
  legitimate claim) can no longer be bought off by raising the cap — the accepted
  mitigation is operational, firewalling the box to the admin's own address for
  the minutes between deploy and claim (`docs/guides/nest-internet-setup.md`).
  The shorter code that predated all of this *was* brute-forceable at the root; the
  floor that matters is that the keyspace stay unexhaustible under the throttle
  ceiling, which the current keyspace clears by orders of magnitude and its
  predecessor did not (spec § 8.5).
- **Anonymous account-creating writes are per-source rate-limited.** The signed
  anonymous *write* surfaces — `fauna.account.register` and
  `fauna.account.invite_request.submit` — each carry a per-source sliding-window
  throttle (`anonymous_rate_limit::{register_config,invite_request_config}`, keyed
  on the real client IP) so one source can't flood account/handle creation or
  pending invite-request rows; `invite_request.submit` adds a **global**
  pending-row cap (`MAX_PENDING_INVITE_REQUESTS`) as the distributed-flood disk
  backstop. These restore the per-IP limits the retired HTTP twins carried, now
  that the SNI router conveys the real client IP (security review
  § D6/D10).
- **Anonymous signature-bound ceremonies are per-source rate-limited *because*
  they are signature-bound, not despite it.** The seed-escrow restore pair, the
  replacement veto pair, `recovery.succession.submit` and the emergency
  `fauna.account.lockout` all ride the generic `anonymous_rate_limit` gate. An
  Ed25519 forgery is not the threat on any of them, so the signature check is
  precisely the unmetered asymmetric work an anonymous flood conscripts once it
  clears the cheap pre-checks — which makes "it is signature-gated" an argument
  **for** the throttle. None can lock a legitimate holder out: each ceremony is
  a handful of calls, once, and the bucket keys on the caller's own source (the
  unspoofable TCP peer for an anonymous caller — see the next bullet for the
  authenticated one), so a flood throttles only its own source.
  `account.lockout` was the last kind still carrying the inverted pre-D10
  rationale in the boundary file; closed 2026-08-10.
- **Every throttle above binds the KIND, on every connection class — not the
  anonymous class (fixed 2026-08-24).** Each kind these five gates guard is a
  *pre-identity* kind, and the dispatcher deliberately admits an
  **authenticated** connection to pre-identity kinds (`transport-connection.md` § Pre-identity (anonymous) connection → *Routing* —
  a fixed pre-identity allowlist; the class allowlist exempts them by design).
  So while the gates were *additionally* scoped to anonymous connections, a
  single account of any tier converted all of them — the unmetered Ed25519
  verifies, the unbounded in-memory nonce mints, the directory enumeration, the
  account-creating writes, and `claim_admin` itself — from their stated budget
  to **unbounded on one connection**. That is not a loosening of the caps but
  their absence for a whole class, which for `claim_admin` is exactly what this
  section's keyspace arithmetic forbids. Two consequences bind, and both are
  structural rather than a matter of remembering: a throttle here is written
  against the kind and never against `conn.anonymous`; and because an
  authenticated connection carries **no** peer address (its upgrade captures no
  `ConnectInfo`, which is also what makes the loopback-gated bridge enrollment
  fail closed for it), the authenticated arm keys on the **actor**, in a
  namespace disjoint from the IP keys. Widening a gate's condition without
  giving that class a key of its own is a silent no-op — the IP-keyed check
  fails open on an absent peer — so the key, not the predicate, is the fix.
  **Both classes share each gate's configured budget deliberately:** a budget is
  justified by what the *kind* costs, and none of that gets cheaper because the
  caller authenticated. The account is the better bound in any case — it is the
  scarce thing an authenticated caller spends, an IP key would collapse every
  user behind one NAT into a single bucket, and unlike an address an abusive
  account is independently revocable.
- **A trip is operator-visible.** Each of these five gates reports through
  `anonymous_rate_limit::report_shed` (`fauna_conn_limit::ShedCounter`, the
  same reporter `transport-connection.md` § Abuse posture's connection caps
  and the per-IP request-rate governor use): at most one `warn!` per minute
  per gate, carrying the batch count, so a sustained trip is visible in a
  shipped nest's default log — never silent, never one line per refusal.

## Transport: the nest↔nest WS-RPC channel

Two orthogonal axes (`transport.md` § HTTP residue):
- **Auth (this doc):** mutual nest-key sign-over-CID. **Settled.**
- **Carrier:** a long-lived nest↔nest WS-RPC channel that reuses the Y.1 L3
  envelopes (`Frame`/`Request`/`Reply`/`Push`/`Cancel` + `RpcDispatcher`,
  peer-symmetric). It absorbs **all** federation residue rows uniformly. The
  request-signed HTTP interim that carried the feature during the migration (the
  same shape `/api/v1/nest-sync/*` used) was **retired in slice 5** — the channel
  is now the sole carrier.

The carrier is **built** (slices 4–5, 2026-06-02/03; design ratified 2026-06-01,
tracked internally, Spec Y2 slice 4). Load-bearing shape (further mechanics tracked internally):

- **Connection class.** A third nest WS connection class —
  **`GET /api/v1/federation/ws`**, subprotocol `fauna.federation.v1`, keyed on the
  **peer `nest_id`** — distinct from the per-actor `GET /api/v1/ws/{actor_id}`
  (bearer) and the anonymous `GET /api/v1/ws` (pre-identity). One long-lived,
  peer-symmetric channel per peer nest, dialed on demand, pooled, kept warm.
- **Channel-level auth (not per-request).** The first frame is a mutual nest-key
  handshake `fauna.federation.hello` that **reuses `federation_sig`** (settled — not
  redesigned), binding both `nest_id`s + a fresh `channel_nonce` + the served-cert
  `spki_sha256` (reusing the client↔nest channel-binding mechanism, `transport.md`
  § Channel binding). After it verifies, every federation Request is attributed to
  the connection's verified peer `nest_id` and carries **no** signature. The
  bespoke per-request `requested_at_ms` skew window + `nonce` dedup
  (`federation_auth`) **retire**: within a session the L3 `idempotency_key` +
  per-connection idempotency cache make redelivery safe, non-idempotent kinds
  set `forbid_replay` (the criterion is `transport.md` § Idempotency and
  reconnect-with-resume — destructive consumes *and* per-call appends/charges
  alike; the cache never spans a reconnect, so it is a bonus, not the ground),
  and a fresh handshake (fresh nonce bound to live TLS SPKI)
  defeats cross-session replay.
- **Peer-symmetric dispatch.** Each end both originates (reusing `RpcDispatcher`)
  and serves. Serving uses a new **`FederationRouter`** (mirrors the per-actor
  `RpcRouter`; handler receives the **originating `nest_id`** where the actor router
  passes an actor) gated by a **federation kind allowlist** — a peer may invoke
  only `fauna.federation.*` kinds, never a client-actor kind (the structural
  guard against actor impersonation), with per-originating-nest throttling. This
  needs a contained L3 extension surfacing inbound `Request`/`Cancel` like
  `push_subscriber()` — **landed** as slice 4's first step
  (`RpcDispatcher::{inbound_requests,inbound_cancels,send_reply}`): the channel
  driver subscribes to serve, clients never do (so their behavior is unchanged).
- **Keepalive / reconnect / TLS.** Reuse the native 30s Ping / 60s dead-link
  detection and the backoff supervisor; reconnect re-runs the
  handshake. Peer URL is **TLS-only** (loopback only for tests), as
  `federation_channel::validate_peer_url` enforces.
- **Uniform migration + mixed-version safety.** The channel absorbs every
  Fauna↔Fauna federation call (§ Federation residue surface carries the
  authoritative, current row/kind count — kept there only, not restated here
  where it would go stale) onto `fauna.federation.*` kinds. During the migration
  the channel and the HTTP interim coexisted with capability negotiation (a nest dialed the
  channel, falling back to the HTTP interim when a peer offered no
  `/api/v1/federation/ws`). **Slice 5 (2026-06-03) retired the HTTP interim**
  once the channel was the production version floor: Fauna deploys as a single
  nest (example.com) today, so the floor was met trivially on deploy. Capability
  detection survives (`dial` → `ChannelUnsupported`), but with no HTTP to fall
  back to it now surfaces as a hard `PoolError::Unsupported` — a channel-only
  build cannot federate with a pre-slice-4 peer. This is a deliberate
  pre-production simplification; per `version-compatibility.md` I2 / § Dim 5, a
  future multi-party deployment **must re-introduce a negotiated fallback
  before admitting older peers** (a requirement at multi-party time, not an
  option). **that fallback must also cover the
  *signature* layer, not only capability negotiation:** the federation hello
  signature is domain-tagged `FEDERATION_HELLO_V1` with **no legacy fallback**
  (`key-material-hierarchy.md` § Architectural rules #8;
  `fauna_protocol::sig_domain`), so a tagged nest cannot complete the hello with
  an older *untagged* peer — the signature fails before any capability
  negotiation runs. So the version floor includes the hello-sig tag version, and
  admitting an older peer needs a transition at the signature layer too (accept
  untagged hello sigs during a window, or floor at the tag version) — see
  `version-compatibility.md` § Dim 5.

## Implementation status today

**Current state (peer-auth + discovery-guard + foreign-member-gate claims
re-verified 2026-08-14; individual bullets below carry their own landing
dates).** The nest↔nest WS-RPC channel is the sole Fauna↔Fauna carrier, live
end-to-end:

- **Relay serving across nests (ruled 2026-10-01) — the read direction BUILT
  2026-10-03, the stamp and the serving kinds BUILT 2026-10-04, the seat
  UNBUILT** (§ Cross-nest shared folders + channel
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
  `../behavior/on-demand-files.md` § Shared sets on a capability host,
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
  **Unbuilt:** the seat — no engine announces a folder homed on another
  nest yet. What that leaves broken, and the refusal that stands in until
  it is built: `../behavior/file-sync.md` § Relay serving, its status
  paragraph.
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
- **BUILT 2026-09-25 — the discovery trust rule** (§ Peer-auth model →
  *Discovery trust rule*): `resolve_peer_nest_id` refuses a `nest.info` peer
  whose served cert is not WebPKI-valid outside the loopback and
  configured-private carve-outs, before any request is sent;
  `conformance_discovery_tls_root.rs` pins the refusal on the wire. **Built
  2026-09-28:** the configured-private carve-out is pinned on the pairing rows'
  stored public `nest_id` (since 2026-10-02 every row URL is pinned and only an
  admin's is the carve-out — `FederationChannelPool::set_pairing_targets`, built
  by `nest_sync_worker::refresh_pairing_targets` before every sync, outbox and
  exchange pass, after every pairing write and after every admin-roster change;
  a mismatch is `PoolError::PeerMismatch`; since 2026-10-06 an exempt URL's
  pin is chosen from the live admin rows alone, so a non-admin's row at it
  cannot unpin it, and the identity succession and the first-admin claim
  rebuild the table too),
  and the host class
  is judged from the one resolution the dial connects to
  (`classify_peer_host_resolving`); `conformance_discovery_tls_root.rs` pins the refusal of a
  mismatched answerer, including over an answer cached before the pin.
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
- **Channel + auth:** `GET /api/v1/federation/ws` (subprotocol
  `fauna.federation.v1`), the mutual `fauna.federation.hello` nest-key handshake
  (reuses `federation_sig`; binds both `nest_id`s + a fresh `channel_nonce` + the
  served-cert SPKI), `FederationConnection` over the shared `fauna-ws-substrate`
  adapter (30 s Ping / 60 s dead-link detection, backoff supervisor), and the
  `FederationChannelPool` in `AppState` (nest_id-keyed dial-on-demand + reuse;
  `dial` → `ChannelUnsupported` surfaces as a hard `PoolError::Unsupported`, no
  HTTP fallback). (`federation_channel.rs`, `federation_pool.rs`.)
- **Serving:** the `FederationRouter` (prefix-enforced `fauna.federation.`
  allowlist — its registered kinds ARE the allowlist; per-originating-nest **and
  per-source-IP** throttle via the `federation_rate_limit` module, the IP check
  running first and Sybil-resistant since a fresh `nest_id` per connection can't
  evade it (§ Security); the `dispatch_core` shared with the per-actor path)
  serves **every kind in the § residue table above** (that section's intro
  carries the authoritative, current row/kind count — kept there only, never
  restated here, precisely because a restatement here already went stale
  twice: the original "15 kinds of 9 rows" was an unrefreshed 2026-07-07
  snapshot predating even the reports pair, found stale 2026-07-20; a
  since-added "14 rows / 26 kinds as of 2026-07-20" replacement was itself
  found stale by the 2026-07-23 sweep when the Nostr Phase-2 row landed).
  (`federation_router.rs`, `federation_handlers.rs`.)
  The former calendar/event legs are retired (§ residue-table note). Paired
  surfaces gate `is_paired` on the connection's **verified** subject nest_id;
  the feed query, formerly unauthenticated on HTTP, inherits the channel's
  mutual auth.
- **Originators channel-only:** the KP-fetch + Welcome relay
  (`conversations_handlers`), the nest-sync worker, the discovery-feed poller,
  the channel-fetch drain, and the group-invite inbox fan-out all originate via
  `federation_pool::originate_*`; `post.get` has no production originator (the
  poller stores a `fetch_url` the *client* reads — its serving handler stays
  ready).
- **One table drives serving, the wire hint, and the §4.D retry policy
  (2026-08-01).** Both channel ends attach a metadata-only registry derived
  from the serving `FederationRouter` (`hint_registry`), so an originated
  Request carries the `replay_forbidden` hint (before this, the peer's
  "caller missing replay_forbidden hint" warning fired on every cross-nest
  KP fetch); and `originate`'s per-call-site `retry_safe: bool` is replaced by
  `FederationRouter::retry_safe` (= `!forbid_replay`), each kind's idempotence
  rationale living at its declaration site in `federation_handlers.rs`. The
  same audit flipped two wrong values — `inbox.deliver` and `welcome.deliver`
  are `forbid_replay: true` (both land in the append-log
  `push_inbox_with_quota`, so a §4.D re-send double-delivered and
  double-charged the recipient's inbox quota; the old rationale leaned on the
  per-connection idempotency cache, which never survives the redial). Set
  pinned by
  `federation_router.rs::the_federation_replay_forbidden_set_is_exactly_these_kinds`;
  flag semantics owner: `transport.md` § Idempotency and reconnect-with-resume.
- **The id→handle announce — BUILT 2026-09-10 (§ Cross-nest shared folders +
  channel append, the id→handle bullet).** `conversations_handlers`'
  relaying `channel.fetch` joins the caller's `users.handle` +
  `handle_domain_if_set()` and passes them to
  `federation_pool::originate_channel_fetch`; the home nest's
  `federation_handlers::channel_fetch_handler` calls
  `record_announced_handle` after the gate, which binds the domain by
  `FederationChannelPool::resolve_domain_nest_id` and stores through
  `CacheDb::record_foreign_member_handle` (two additive nullable columns on
  `channel_foreign_members`); `list_floor_roster` `LEFT JOIN`s the binding
  row so `room.list_roster` names the foreign member. No reverse-lookup kind
  exists, by ruling. Client: `FaunaMlsBackend::resolve_nameless_members`
  re-asks listed-but-nameless members at a doubling gap of polls
  (`NAMELESS_REASK_CAP`). **The foreign member's OWN device is reached too,
  since 2026-09-10** (the `conversation.roster.fetch` row above): its roster
  read is relayed by its own nest rather than refused locally, so it sees the
  same names a home member sees — `resolve_nameless_members` passes the
  channel's recorded home (`channel_home_url`) through the `RoomRosterReader`
  seam, the glue picks `room.list_roster_remote` on `Some(url)`, and the room
  home answers from `conversations_handlers::room_roster_reply`, the same body
  its same-nest door serves. Proofs:
  `conformance_cross_nest_conversations_client.rs` (the drain names, the
  spoof is refused, a rename lands;
  `a_foreign_member_reads_the_rooms_floor_through_its_own_nest` — the foreign
  member's roster equals the home member's, and the same-nest read finds
  nothing) + `fauna_mls_backend_tests.rs`
  (`the_handle_read_reasks_after_a_roster_that_omitted_the_member`,
  `a_listed_nameless_member_is_reasked_at_a_widening_gap_of_polls`,
  `the_roster_read_is_routed_by_the_channels_recorded_home`).
  **And the foreign member's own commits reach the home's floor, since
  2026-09-10** (the `conversation.roster.report` row above): the report seam
  carries the same recorded home (`RoomRosterReport.home_nest_url`), the glue
  picks `room.roster_report_remote` on `Some(url)`, the member's nest
  originates the relay, and the room home applies the report through
  `conversations_handlers::room_roster_report_apply` — the body its same-nest
  door runs — behind `require_foreign_member`. Proof:
  `conformance_cross_nest_conversations_client.rs::a_foreign_members_membership_commit_reaches_the_rooms_home_floor`
  (a foreign admin's Remove lands on the home floor; the relaying nest holds
  no room record).
- **A community room's verdicts on the relayed read — BUILT 2026-09-10** (the
  `channel.fetch` row above). The home nest's
  `federation_handlers::channel_fetch_handler` fills
  `FedChannelFetchMessage.labels`/`.scores` through
  `conversations_handlers::page_verdicts`, the body its same-nest
  `channel.fetch` calls, for the requester `require_foreign_member` bound;
  `federation_pool::originate_channel_fetch` returns the peer's entries whole,
  and the relaying `channel.fetch` maps them onto `ChannelFetchEntry` — no
  row written on the relaying side. (The home nest's per-record `author`
  attestation rides the same two structs the same way, additive and forwarded
  unchanged; its meaning and its one consumer are owned by
  `../behavior/inbound-scheduling-authority.md` § Who may mutate an existing event over the
  inbound rail.) Proofs:
  `conformance_cross_nest_conversations_client.rs::a_foreign_members_relayed_read_carries_the_community_rooms_verdicts`
  (two real nests; the verdict reaches the member's client seam, an unseated
  bound actor reads none, the relaying nest keeps no row) and the three
  relayed-read cases in `conformance_conversation_rooms.rs`.
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
  credentialed layer. **Client read-side BUILT (2026-07-19, Phase 2 second
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
  these two federation kinds — the kind inventory above lists no
  `lease`/`conflicts` federation twin, and the read-write/cross-nest
  design makes cross-nest conflict-report relay a **named v1 gap** (the
  losing version is still structurally retained; nothing is lost) and does
  not route the advisory upload lease cross-nest (concurrent cross-nest
  writes auto-resolve as conflicts). A cross-nest writer's holder-scoped
  same-nest lease-release fix landed alongside.
- **Client cross-nest path (shared Rust, end-to-end):** the `ConversationsRpc`
  seam carries `actor_by_handle_remote` (anonymous discovery against the peer)
  + `peer_domain` relay routing (`libs/fauna-conversations`,
  `libs/fauna-client-conversations`); mandatory last-resort KP publication
  (`MlsEngine::generate_last_resort_key_package_bytes` →
  `ensure_last_resort_keypackage`, idempotent single-row upsert); the
  `addressable` boolean on `by_handle`. The receiver-side cross-nest drain rides
  the shared receive loop natively (`WelcomeNudge.home_nest_url` →
  `fauna.federation.channel.fetch`, authorized by `channel_foreign_members`)
  and the wasm `drainInbox` path on web
  (`libs/fauna-wasm/src/conversations.rs:1719-1725` threads `home_nest_url`).
- *Moved 2026-09-28 to [`foreign-handle-resolution.md`](foreign-handle-resolution.md) § Implementation status today, verbatim:* **Discovery-failure semantics** — BUILT 2026-08-29 in shared Rust, hardened 2026-08-29 (the closed disowning set) and 2026-08-30 (the cold-start window, three passes), with **the price of closing it** (2026-09-22); **The dial names the peer** — BUILT 2026-09-22; and **What a spoofed label could actually reach** — closed 2026-09-27 by the overlay's paint gate.
- **Anonymous-surface hardening:** per-source throttles on discovery, the
  signature-bound recovery ceremonies (escrow / veto / succession /
  `account.lockout` — the conscripted-verify bound, § Security),
  `claim_admin` (per-source + a global distributed-brute-force cap),
  `invite_code.verify`, `account.register`, and `invite_request.submit`
  (`anonymous_rate_limit`, keyed on the real client IP for an anonymous
  connection — conveyed by the fauna-sni-router via PROXY-protocol-v2 — and on
  the **actor** for an authenticated one, in a disjoint bucket namespace via the
  single door `check_conn`, since 2026-08-24: the caps bind the kind on every
  connection class, § Security above; mechanism owned by
  `transport-connection.md` § Abuse posture); the account age band's nonce
  mint `fauna.account.age_nonce` (2026-08-24, `behavior/family-safety.md`
  § The account age band) rides the
  same gate — an unauthenticated in-memory nonce-store write, the same
  memory-growth profile as the escrow/replacement challenge pair; anonymous
  version coarsened to `major.minor`; short
  hand-transcribable claim codes (`fauna_core::claim_code`) — throttle-bound,
  see § Security.
- **Proofs (tier_3):** `conformance_federation_channel.rs` (handshake,
  allowlist, throttle, idempotency replay, per-kind round-trips, the cross-nest
  MLS capstone + per-originator tests),
  `conformance_cross_nest_conversations{,_client}.rs` (engine-direct and
  client-driven two-nest MLS groups; channel-fetch DM receipt + cross-nest
  scheduling iMIP), the GUI `test_fauna_mls_cross_nest_roundtrip.py` (linux;
  **web green only since 2026-08-29** — see Provenance, the entry there corrects
  a three-month-stale claim that it already was), the two-driven-client `test_fauna_mls_two_client_inbox_drain.py`
  (linux leg) + `test_fauna_mls_web_receives_from_linux_sender.py` (web leg of
  the identical same-nest two-real-GUI shape), and the Python `FederationChannelClient`
  API harness (`tests/e2e-unified/clients/ws_rpc_federation_client.py` driving
  `test_{namespace_sync,post_forwarding,cross_nest_api}.py`).

**Gaps (the honest remainder):**

- ~~**apple + android login glue**~~ — ✅ **already covered, both legs** (found stale
  2026-07-20 during a TODO-verification pass — this entry pre-dated the fix on
  both platforms). Neither needs its own explicit
  `ensure_last_resort_keypackage()` call: both build the shared
  `ConversationsSession` and call its `startReceiveLoop()` at real login
  (android `ConversationsManagerHost.kt:187`, wired from `ApiClient.kt:390`
  since 2026-07-02; apple `ConversationsVM.swift:117` `activate()`,
  called from both `Fauna-iOS/App/FaunaApp.swift` and
  `Fauna-macOS/App/FaunaMacApp.swift` since 2026-07-14), and
  `ConversationsSession::start_receive_loop` (`libs/fauna-conversations/src/
  session.rs:989-993`) calls `ensure_keypackages` **and**
  `ensure_last_resort_keypackage` itself, session-owned. Web is the one app
  that does NOT run this loop, so it correctly calls
  `ensureLastResortKeypackage()` explicitly (`apps/fauna-web/src/lib/
  conversations.ts:212`, not line 601 as this entry previously cited).
- ~~**web cross-nest receive proof**~~ — no longer an open item here, and this
  entry states no status of its own: the owner is
  [`../behavior/direct-messages.md`](../behavior/direct-messages.md) §
  Implementation status today. **Correction (2026-09-20):** it read "✅ already
  built" on the strength of `test_fauna_mls_web_receives_from_linux_sender.py`,
  which is a **same-nest** proof by its own docstring — alice and bob share one
  nest there, so it never exercised a cross-nest hop and could not close this
  gap. That miscitation made this the third doc to hold a cross-nest-receive
  status while two others deferred theirs to each other; the status now lives in
  exactly one place, witnessed by `test_fauna_mls_cross_nest_receive.py`.
- **multi-party negotiated fallback**: required before admitting older peers at
  multi-party time — capability negotiation AND a hello-sig-layer transition.
  Owner: this doc; trigger: a multi-party deployment.
  (The cross-nest **knock** that used to sit here as an open remainder is
  built — § Client-side peer discovery for a cross-nest knock, below.)

**Provenance (dated landings — tombstones; detail lives in the named specs/TODOs):**

- 2026-05-30 — slice 2: nest peer-auth on the (then-HTTP) federation routes;
  last-resort KP column + non-deleting fallback; `addressable`;
  per-originating-nest throttle; originating relay; tier_3 two-nest proof.
- 2026-05-31 — anonymous-surface hardening (per-source discovery / claim /
  invite throttles, global claim cap, version coarsening, longer claim codes;
  Spec Y2 § 8).
- 2026-08-29 — the **browser** cross-nest GUI proof actually goes green, closing
  the gap the 2026-06-01 entry above had claimed shut. The web arm of
  `test_fauna_mls_cross_nest_roundtrip.py` had been failing deterministically,
  and failing *silently*: the harness's foreign-peer fixture (`cross_nest_foreign`)
  serves the always-live self-signed floor cert, which every native client accepts
  through the channel-binding trust model but a browser structurally cannot — so
  the anonymous `actor_by_handle_remote` hop died on
  `net::ERR_CERT_AUTHORITY_INVALID`, `resolve_foreign` mapped that transport error
  to `NotFound` like any non-Fauna domain, the recipient degraded to a plain
  `Email` chip, and the DM went out over SMTP with no error raised anywhere. A
  harness fix only — the Chromium contexts now set `ignore_https_errors`, the
  fixture-cert analogue of the CA-signed name a real browser-reachable peer has;
  no nest-side or wasm-transport code was implicated. The test now asserts the
  thread's **rail** before the nest-side counts, so this failure mode can never
  again present as a bare key-package-count mismatch.
- 2026-08-29 — **discovery-failure semantics ratified and built** (§ Peer-auth
  model): the product behaviour the entry above exposed — an unreachable peer
  read as "just an email address" and the message left over SMTP with no error
  anywhere — is closed at all four collapse points (seam outcome structural;
  backend known-domain rule; manager's terminal `Error`; picker commits only a
  probed address). First contact with a never-seen unreachable domain still
  resolves as email, by ruling, with the chip showing the email rail.
- 2026-07-24 — claim-code entropy **deliberately reduced** (user decision, for
  hand-transcription); the claim throttles become the primary bound again and
  are now un-loosenable without restoring the length (§ Security).
- 2026-06-01 — slice 3: shared-Rust client re-consumption (seam cross-nest
  discovery + relay routing; web wasm seam + binding glue; cross-nest
  GUI proof green on `--client linux`). ⚠ **Corrected 2026-08-29:** this entry
  read "browser cross-nest GUI proof green on `--client web` + linux" for three
  months, and the `--client web` half was never true: that test's web arm had no
  recorded green in `docs/features/ledger/web.json` at any point, only a failure. See the 2026-08-29 entry for what was actually
  wrong and when the browser leg first went green.
- 2026-06-02 — slice 4: L3 peer-symmetry
  (`RpcDispatcher::{inbound_requests,inbound_cancels,send_reply}`), the
  `fauna-ws-substrate` lift, the channel core + hello handshake, the
  `FederationRouter` + all then-13 §4.E handlers, the `FederationChannelPool` +
  originator rewire, the tier_3 channel capstone.
  (design ratified 2026-06-01; tracked internally.)
- 2026-06-03 — slice 5: the HTTP interim retired — twins + `federation_relay` +
  `federation_auth`/`FederationReplayGuard` + the per-request signing payloads
  deleted; originators channel-only. The channel became the sole carrier.
- 2026-06-06 — `fauna.federation.inbox.deliver` + the client bearer leg
  `fauna.inbox.send` (both legs); the Python `FederationChannelClient` api
  harness (tracked internally, closed).
- 2026-06-07/09 — per-app inbox fan-out: linux 2026-06-07, apple 2026-06-08,
  windows 2026-06-09 (also fixed windows' latent off-spec knock POST); android
  off-gate; the `POST /api/v1/inbox/{actor}` twin **deleted
  2026-06-09** (§ residue surface carries the per-app detail).
- 2026-06-14 — the cross-nest `channel.fetch` relay + `channel_foreign_members`
  authorization + `WelcomeNudge.home_nest_url`; the native two-driven-client
  receive proof. Same day: the Decision-B § 4c legacy-calendar cleanup retired
  the calendar/event federation legs (§ residue-table note).
- 2026-06-15 — the web FaunaMls receive loop (`pollFaunaMlsOnce`).

**Out of scope:** non-Fauna federation (ActivityPub); the `libs/fauna-peer`
P2P-transport migration (`transport.md` § Future directions).
