# Federation — nest ↔ nest wire layer — target state

Owns: federation, rate-limiting
Status: ratified
Authority: owns the nest↔nest peer-auth model, federation carrier + kind inventory, open-federation trust model, KP privacy/exhaustion, anonymous-surface security model, and request-level rate-limiting (anonymous + federation throttles); what a client concludes from a foreign domain's `by_handle` answer or non-answer (discovery-failure semantics, the dial rule) → foreign-handle-resolution.md; the kinds, gates and wire rules of cross-nest shared folders + channel append (the public folder read plane included) → federation-shared-folders.md; connection-level caps → transport-connection.md § Abuse posture; mail per-IP cap → behavior/mail-policy-config.md; client↔nest WS-RPC → transport.md; HTTP classification → api-layers.md.

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
  `transport-trust.md` § Transport trust, Axis 1). The answer is believed only when the
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
whose request carries no requesting actor at all ([`federation-shared-folders.md`](federation-shared-folders.md) § The public folder read plane
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
| Conversation **channel-message fetch** | **(none — channel-only, net-new)** | **nest-signature** + channel-membership-bound-to-home-`nest_id` | `fauna.federation.channel.fetch` | net-new, NOT residue: the **open-federation** cross-nest message pull for **unpaired** nests (`direct-messages.md` § Technical Flow — Cross-Nest, step 3). Read-only; returns the channel's ciphertext entries `(seq, envelope)` the relay cannot decrypt (MLS E2E, § Security). A member's drain on its home nest originates it to the channel-home nest, which serves it iff the requesting actor is a recorded member of the channel **and** the verified `origin_nest_id` is that member's recorded home nest (the structural defense against a hostile signer harvesting an un-hosted channel). Per-originating-nest throttled. Distinct from the `is_paired`-gated `mls_pull` below (that is the paired-nest buffer-pull optimization). **Carries the member's announced `handle@domain` (additive `requesting_handle`/`requesting_domain`, 2026-09-10)** — the id→handle ruling's one mechanism: the originating nest volunteers what it joins from its own `users` row, and the home nest records it beside the binding only after resolving the domain to the originator's key ([`federation-shared-folders.md`](federation-shared-folders.md) § Cross-nest shared folders + channel append, the id→handle bullet, owns the rule). **Carries a community room's verdicts beside each entry (additive `labels`/`scores`, 2026-09-10)** — what the room's named labelers derived (`../behavior/community-rooms.md` § The three classes → *What the home nest does with its read* owns what they are and who may read them), filled exactly when the requesting actor is a live floor member of the room: the gate the same-nest read keys on its caller, applied to the actor the binding admitted, never to the peer — and never beside a withheld envelope. The relaying nest forwards them onto its member's `ChannelFetchEntry` and stores none. The skew is benign both ways, which is why an additive field suffices here: an older home serves none and an older relaying nest ignores them, and either way the member reads the envelopes alone — the pre-field behaviour, never a wrong verdict. |
| Namespace + MLS **sync** | `/api/v1/nest-sync/{pull,push,mls-pull,mls-ack}` | **nest-signature** (`is_paired`) | `fauna.federation.sync.{pull,push,mls_pull,mls_ack}` | already nest-signed today; the private paired-nest surface. |
| Mail **relay** (public→private) | **(none — channel-only)** | **nest-signature** + `mail_pull` capability (`pairing_has_capability`) | `fauna.federation.sync.{mail_pull,mail_ack}` | net-new, NOT residue: the public→private `__mail` relay (`deployment-home-with-public-relay.md` § Inbound mail). Built channel-only (no HTTP twin) — slice 5 has nothing to retire here. Moves verbatim sealed `MailRecordEnvelope` bytes; ack→tombstone→compaction purge. |
| Post **forward** (relay) | `POST /api/v1/forward` | **nest-signature** + `post_forward` capability (`pairing_has_capability`), under the `paired_only` submission policy | `fauna.federation.post.{forward,delete}` | private paired-nest post relay (`federation_handlers.rs` `register_post_federation_handlers`). The twin's extra per-forward nest-signature over the JSON body is **gone** — the channel handshake authenticates the relay, so only the post's own author envelope rides. Origination (the private-side outbox worker) is owned by `nest/private-mode.md` § Post Forwarding. **`post.delete`** (added 2026-07-15) is the delete twin: the author-signed `Tombstone` rides verbatim (re-verified on receipt) so a forwarded copy on the public nest does not outlive the original (`feed.md` § State & data shape → Post deletion → Propagation), gated by the same `post_forward` capability. |
| Discovery-feed query / post fetch | `POST /api/v1/feeds/query`, `GET /api/v1/posts/{id}` | unauthenticated public-data | `fauna.federation.feed.query`, `fauna.federation.post.get` | scoring/post bytes; **unauthenticated today** — gains attribution + per-nest throttle on the channel. **`post.get` gains a client relay leg (ruled 2026-10-16; built 2026-10-16, `posts_handlers::posts_get_remote_handler` — the home's `fauna.federation.not_found` comes back as `fauna.posts.not_found`, so the relayed door answers a missing post as the local one does):** the distinct kind `fauna.posts.get_remote { post_id, nest_url }` — the deep-link door's read for a post this nest does not hold ([`feed-read-model.md`](feed-read-model.md) § The read model → *Opening a post the timeline never loaded* → *A post homed on another nest* owns the door) — which the reader's own nest originates here and forwards unstored, `legal_takedown` marker included — stamping `home_nest_actor_id`, the `peer_nest_id` it dialed the channel as (`federation_pool::resolve_peer_nest_id`), the root the client's byte plane graduates its pin against ([`transport-trust.md`](transport-trust.md) § Transport trust, the federation-granted row's *post arm*; ruled 2026-10-19, unbuilt) — under the same `caller = None` quarantine posture as the trend import's fetch; the reply is the **envelope only** — the sealed body and every attachment are fetched by the client by content address from the home's public `GET /api/v1/blob/{hash}`, direct (a room post opens under the room's key; a broadcast-gated post's `KeyBlob` is a roster and is read through the follower's own nest — [`../behavior/federated-follow.md`](../behavior/federated-follow.md) § Decision 3): **bulk bytes never ride this channel** ([`federation-shared-folders.md`](federation-shared-folders.md) § Cross-nest shared folders + channel append). Distinct, not an additive field on `fauna.posts.get`, for two reasons: the `_remote` family's one shape (`posts.room_labels_remote`, `room.list_roster_remote`, `generations_remote`), and because a kind that makes this nest dial a caller-named peer is a different trust shape from a local read — it takes its own allowlist, offline-class and deadline rows (`OnlineOnly`, the 30 s peer round-trip deadline, never pre-identity), not `posts.get`'s. |
| Social inbox **deliver** | ~~`POST /api/v1/inbox/{actor}`~~ *(twin DELETED 2026-06-09 — channel-only)* | **nest-signature** | `fauna.federation.inbox.deliver` | cross-nest Fauna-native signed `(ContactRequest, Post)` delivery → recipient `InboxMode` routing + inbox row + push (`routes::deliver_inbox_payload_core`, kept — the channel rides it). |
| Report exchange/export | **(none — channel-only, net-new)** | **nest-signature** | `fauna.federation.reports.{exchange,export}` | the k-anonymized per-item report aggregates (`behavior/report-sharing.md` § Federation exchange — that doc owns the mechanism): only `(content-hash, factor, count)` crosses, importer re-validates the k-gate, hard 1024-entry cap, per-peer row cap, table-wide row cap (§ Security → *Rate limiting*). **All peer aggregates for a hash collapse into one non-scaling peer bucket** — a peer's `nest_id` is self-minted and free, so neither a peer's claimed count nor the number of peers may scale the local distinct-reporter consensus (the ruling paragraph below the table, *What outlives the leg*, keeps the reason). Open-federation, per-origin throttled. Registered 2026-07-07 (`register_reports_federation_handlers`). |
| Abuse-report **forwarding** *(BUILT 2026-09-26)* | **(none — channel-only, net-new)** | **nest-signature** | `fauna.federation.abuse_report.{deliver,withdraw,outcome}` | user-initiated reporting's cross-nest leg (`behavior/moderation.md` § Routing — that doc owns the mechanism): a report on a foreign author is forwarded reporter-anonymously to the author's home nest, which accepts it only for a subject it hosts; the withdrawal and the outcome follow, each pinned to the nest the delivery was verified as. Every leg is idempotent on the receiver (a delivery is keyed on the verified origin plus its `report_ref`), so each is retry-safe; the sender drains them from a durable queue with backoff. Open-federation, per-origin throttled and per-target budgeted (§ Security → *Rate limiting*) (`abuse_report_federation::register_abuse_report_federation_handlers`). |
| Profile read across nests *(RULED 2026-10-19 — [`../ui/profile.md`](../ui/profile.md) § Where logic lives → *The remote read* owns the opt-in, the client leg and the fallback; BUILT 2026-10-19 in the nest)* | **(none — channel-only, net-new)** | **nest-signature** + the actor's own consent: served only for an actor hosted here whose latest signed profile decodes with `audience = Everyone` | `fauna.federation.profile.get` | the profile twin of the post-fetch row: `{ actor_id }` → the actor's latest signed profile bytes (`profile_handlers::latest_profile_bytes`, verbatim — the client verifies the Ed25519 signature as the same-nest read does); not hosted, no profile, `audience = ThisNest` or an unknown audience all answer `fauna.federation.not_found` — one refusal, so a peer learns nothing about whether an actor publishes a profile. Client leg: the distinct kind `fauna.profile.get_remote { actor_id, nest_url }` (`User`, `OnlineOnly`, 30 s, never pre-identity — the `_remote` family's shape, for the post-fetch row's two reasons), the reader's nest forwarding unstored and re-speaking the home's `not_found` as `fauna.profile.not_found`. `avatar` / `banner` bytes are fetched by the client from the home's public blob route, direct, the relay reply stamping the home's dial-proved identity (`home_nest_actor_id`, the post arm's stamp) for the plane's pin ([`transport-trust.md`](transport-trust.md) § Mechanism for the federation-granted row → *The post arm* → *A relayed profile's pictures*). **Why a kind and not a public route:** it is the owner's 2026-10-19 re-open of the 2026-08-17 decline of any anonymous profile read ([`../behavior/identity-succession.md`](../behavior/identity-succession.md) § Don't do these) — the home gets attribution and the per-nest throttle, the pre-identity surface stays closed, and the consent is the actor's signed word, which the home cannot forge. |
| Moderation **obligation delivery + relayed appeal** *(RULED 2026-10-18 — [`../behavior/takedown-and-block.md`](../behavior/takedown-and-block.md) § Legal takedown → *The author homed on another nest* owns the mechanism; built 2026-10-19)* | **(none — channel-only, net-new)** | **nest-signature** — `obligation.deliver`: the named author is hosted by the receiver, and a `post` is that author's by the receiver's own store (a conversation record cannot be checked; the budgets bound it); `moderation.appeal`: the verified origin is the nest the row was delivered to and `author` the row's `author_hex` | `fauna.federation.moderation.obligation.deliver`, `fauna.federation.moderation.appeal` | the transparency triple's appeal leg across a nest boundary: a takedown or admin block whose author the deciding nest does not host (a foreign member's conversation record on the channel home; the local projection of a foreign post on the reporter's nest) **pushes** the author's `obligation_action_records` row to the author's home nest, which serves it on the author's own queue, and the author's appeal rides back through their home nest to the issuer's gate and audit chain. The delivery is durable on the moderation plane's queue (the abuse-report triad's), idempotent on `(verified origin, issuer ref)`, per-origin throttled and per-target budgeted (§ Security → *Rate limiting*); the appeal is an inline relay (the `posts.get_remote` posture), the home recording its own `moderation:appeal-relayed` row only on the issuer's acknowledgement. |
| Channel **append** *(BUILT 2026-07-19; ratified 2026-07-18)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.channel.append` | the **mutating twin of `channel.fetch`** — a foreign member's send relayed by their home nest into the channel-home nest's log (same structural gate as the fetch). Commit admission on claimed folder channels is **roster-membership** (re-ratified 2026-08-24 — a foreign member's device-owned-epoch takeover must land; owner-only roster management is enforced member-side, since commit content is ciphertext to the nest); off-roster actors refused; conversation channels open. Rides the L3 `idempotency_key`. [`federation-shared-folders.md`](federation-shared-folders.md) § Cross-nest shared folders + channel append. |
| Channel **leave** *(BUILT 2026-07-19; ratified 2026-07-18; room-hardened 2026-09-11)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.channel.leave` | the **first mutating member-gated self-scoped kind**: deletes the requester's own `channel_foreign_members` row (idempotent; absent = success), killing all future federated fetches. Generic — conversations and folder channels alike. ⚠ **On a floor-authoritative room it runs the room's own leave body instead** (`conversations_handlers::room_leave_apply`, shared with the *room leave* row below), because on a room the binding delete is only half a departure: the seat stays, and a seat nobody occupies is not inert — the roster-coverage gate *obliges* every later generation mint to wrap to its reception key, and `room.invite` refuses to re-admit a principal that is already a member, so the ghost both keeps drawing key material and blocks the re-admission that would heal it. Nothing of ours calls this kind with a room channel id (the conversations plane has its own kind, and no app carries a conversation self-leave gesture at all), but a peer nest can, and a door that converges only when the caller picks the right kind is not converged. The `removed` flag keeps naming the **binding**, which is this kind's own contract. The absent-row arm still unseats nothing, deliberately: with no binding this nest can prove nothing about the caller's home, and unseating on an unproven claim would hand any peer a removal door for any principal on any room's floor. |
| Channel **roster read** *(BUILT 2026-07-29; ratified 2026-07-29)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.channel.actors` | the **roster-read twin of `channel.fetch`** — the add-participant heal's discriminator for a member whose channel is foreign-homed (heal mechanics: `mls-group-key-material.md` § M2 *Admitting a member*, chat bullet). The home nest answers the union `actor_channels ∪ channel_foreign_members` — **hex actor ids only, never nest URLs** (members already read the full membership off the MLS ratchet tree, so the ids disclose nothing new; URLs would) — and the read is **strictly read-only on every hop**: no auto-register anywhere (a roster read that wrote a row would make every phantom look healthy to the next caller). The **client leg is the distinct kind** `fauna.conversations.channel.actors_remote { channel_id, nest_url }` — never an additive `nest_url` on `channel.actors`, for a sharper reason than `send_remote`'s (`../behavior/direct-messages.md` § step 3b): an old member-nest ignoring an additive field would answer its **partial** roster as a clean success, and the heal consuming it would evict a healthy member under supported version skew — while an unknown kind fails loud and the seam's error→"roster unreadable" mapping lands in the heal's refuse arm, the correct degradation on every skew pairing. The fetch-vs-actors dividing line: fetch's additive field is safe because its old-nest degrade is a benign stale/empty read; the actors read feeds a **membership-mutating** heal, so a silently wrong answer corrupts. |
| Conversation **attachment write token** *(BUILT 2026-09-09; ratified 2026-09-09)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.write_token.mint` | the **blob twin of `channel.append`**: a room's attachment bytes rest on its home nest beside the record that pins them (`../behavior/conversation-rooms.md` § The home nest → *Attachment bytes* owns the residency), so a foreign member's own nest relays this mint and the member POSTs the sealed attachment DIRECT to the home nest's `POST /api/v1/blob` under the returned short-lived, write-only bulk token (purpose `ForeignConversationWrite`, TTL the same `FOREIGN_WRITE_TOKEN_TTL_SECS` Rust constant, bridge-mint-refused) — **bytes never ride this channel**. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) — no `access == 'writer'` arm, because a conversation has no claimant and every member posts; no metering, because a room carries no owner-pays byte policy ([`federation-shared-folders.md`](federation-shared-folders.md) § Cross-nest → *Substrate vs. policy*). Client leg: the distinct kind `fauna.conversations.blob.write_token.get { channel_id, nest_url }` (unknown-kind-loud on an old own nest, S5-mapped to `peer_nest_outdated`); the read leg needs no kind — a member fetches by content address from the home nest's public `GET /api/v1/blob/{hash}`, direct. |
| Conversation **room generation read** *(BUILT 2026-09-10; ratified 2026-09-08)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.generations.fetch` | the **key-read twin of `channel.fetch`**, and the piece that makes a community room reachable from a foreign member at all: the log `channel.fetch` relays is sealed under a room generation key (`../behavior/community-rooms.md` § The three classes → *Community*), so a member homed elsewhere needs its own wraps as well as the ciphertext, and "a member on a foreign nest reaches the room only through their own home nest" (§ The home nest). Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) — a room id **is** its channel id. ⚠ **The request carries `{ requesting_actor_id, room_id }` and deliberately NO `entry_id`, and never may:** the room home resolves which wraps to serve from the requesting actor's own live floor entry (the same resolution the same-nest `room.generations` uses), because the home nest of a community room is *itself* a seated floor principal with a room-read wrap — an entry-named request would let a relay ask for that one and read the room, when a relay is specified to carry ciphertext and hold no wrap (§ The home nest). Bound as it is, the worst a hostile relay can request is a wrap belonging to one of **its own** members, sealed to that member's reception key and opaque to it. Read-only on every hop: serving a wrap registers nothing and mints nothing. Client leg: the distinct kind `fauna.conversations.room.generations_remote { room_id, nest_url }` — distinct for the `channel.actors_remote` reason, not `send_remote`'s: an old own-nest ignoring an additive `nest_url` would answer from its own empty room plane as a clean success, and the member would read "this room has no generations", a silently wrong answer **about key material**; an unknown kind fails loud. The relaying nest forwards the reply and stores none of it. |
| Conversation **room roster read** *(BUILT 2026-09-10; ratified 2026-09-10)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.roster.fetch` | the **name-read twin of `channel.fetch`**, and the piece that carries the id→handle announce to the one seat it could not reach: the **foreign member's own device**. A room's floor roster lives on its home nest alone (`../behavior/conversation-rooms.md` § The home nest), so a member homed elsewhere has no roster read at all — its own nest holds no room record and answers `permission_denied`, indistinguishable from "you are not a member" — and every co-member it has not met renders as an elided actor id, including the members the announce names for everyone on the room's home. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) — a room id **is** its channel id. ⚠ **The gate is that and deliberately NOT `is_room_member` on top:** the home nest's `channel_foreign_members` row is its own record of the Welcome it relayed and the binding it pinned — admission itself — whereas an end-to-end room's floor roster is a member-*reported* mirror (§ The floor roster) that routinely lags a membership commit, so stacking it on would deny the read to the newest-seated member, precisely the one whose co-members are still nameless. Nothing is disclosed by that choice: an admitted channel member already reads every member's actor id off the MLS ratchet tree — and **admitted** is a live fact, not a historical one, because a removal ends the admission along with the seat. So this gate closes with the membership, and the doors beside it that read the same row alone — the write-token mint and `channel.actors` — close with it. ⚠ **Which ceremony closes them differs by class, and both are needed for that sentence to hold** (2026-09-11): a **community** room's `room.remove` *purges* the binding before it unseats, exactly as `members.evict` does on the folder plane (S8, the *Channel append* row) — the ceremony is [`../behavior/community-rooms.md`](../behavior/community-rooms.md) § Implementation status today → *A removal severs*; an **end-to-end** room has no such door (its membership authority is its MLS group, so `room.remove` refuses the class outright) and removes a member by membership commit plus the committing device's roster report, whose absorb stamps `removed_at` and purges nothing — so on that class these three doors *read the floor's verdict* and refuse a **positively removed** requester (`federation_handlers::refuse_removed_room_member`). **The two ceremonies are complementary, never cumulative:** the floor read is skipped on a floor-authoritative room, exactly the class `room.remove` serves, so every room has one severance mechanism and none has two — and the skip is required, because on the purge's class the binding *is* admission and the seat is a separate act, so a re-invite's relayed Welcome re-inserts the purged binding and must be served while the old `removed_at` row the purge superseded still stands. The ceremony is [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Implementation status today → *A removal severs on the end-to-end class too*. A requester with **no** floor row stays admitted under either ceremony — that is still the newest-seated member this row's gate is ratified to protect. **The resulting per-class asymmetry, declared:** the purge shuts all four doors on a community room including `channel.fetch`'s ciphertext drain, where the floor read shuts only these three plaintext doors and leaves an end-to-end room's `channel.fetch` open — ratified, because that removal re-keys the MLS group, so what a removed member may still fetch it cannot open, and the residue is the one its own retained keys already are. The floor read was chosen over purging on absorb *because* the report ratchet deliberately admits a departing member's final report, so a purge there would let one live member's false report sever every co-member's binding past recovery (the Welcome relay's insert arm holds `InsertOnly` power) — `principles.md` § No client-causable unrecoverable nest state. **Handles ride the reply, and that is the read.** This is emphatically NOT the refused id→handle oracle ([`federation-shared-folders.md`](federation-shared-folders.md) § Cross-nest shared folders + channel append, the id→handle bullet): that shape answered "what handle does actor X wear?" to any nest holding an id, under a gate that could verify nothing about the asker; here the asker must prove, through the same binding that lets it fetch the room's ciphertext, that one of its own members sits on this room's floor — exactly the audience the ratified announce names ("the rooms it is in"). The neighbouring `channel.actors` row serves hex ids only because *its* consumer is a membership-mutating heal that needs identity and never names; here naming is the whole point. The home serves it from `conversations_handlers::room_roster_reply`, the **same body** the same-nest door serves, so a foreign member sees the same names a home member sees and the two replies cannot drift. Read-only on every hop: serving a roster registers nothing and mints nothing. Client leg: the distinct kind `fauna.conversations.room.list_roster_remote { room_id, nest_url }` — distinct for the `channel.actors_remote` reason, since an old own-nest ignoring an additive `nest_url` would answer its own room plane's clean `permission_denied`, a refusal the seam cannot tell from "you were removed", leaving the members silently elided; an unknown kind fails loud. The relaying nest forwards the reply and stores none of it. |
| Conversation **room roster report** *(BUILT 2026-09-10; ratified 2026-09-10)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.roster.report` | the **write twin of the roster read above**, and the piece that lets a member homed elsewhere keep the room's floor roster true: § The floor roster has the committing device report the resulting roster to the room's **home** nest after every membership or policy commit, and a foreign member's commit rides its own nest's relay to the home's log — so its report must ride the relay too, or the home's floor (routing fan-out, the custody serve door, the cross-nest relay gate, succession targets) goes stale until a same-nest member happens to commit (`../behavior/conversation-rooms.md` § The home nest). The request carries `{ requesting_actor_id, room_id, members, policy_version, commit_seq }` — the same-nest `room.roster_report` body plus the requester the home binds; `commit_seq` is additive, and it is the home's own log position, since the member's commit rode the same relay to the home's log. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) **in place of** the same-nest door's routing-roster gate 1, and strictly stronger than it: the `channel_foreign_members` row is the home's own record of the Welcome it relayed and the binding it pinned, where a routing row is `channel.send`-self-registered and proves only knowledge of the channel id. Behind that gate the home runs **the same body its same-nest door runs** (`conversations_handlers::room_roster_report_apply`: roster validation, gate 0's provenance refusal of a floor-authoritative room, the gate 2 ratchet against the STORED roster, the commit-order guard of `../behavior/conversation-rooms.md` § The floor roster — whose authorship half reads the `requesting_actor_id` the home bound here as the reporter, and compares it against the sender the home recorded for the commit at that position, which for a foreign member's commit is the same bound id its `federation.channel.append` carried — the wholesale replace) — one body, so the two doors cannot drift, and the bootstrap bound (`conversation-rooms.md` § Implementation status today) does not widen: a relayed first report is admissible only from a Welcome-bound member naming itself, never from any routing-roster actor. Client leg: the distinct kind `fauna.conversations.room.roster_report_remote { room_id, nest_url, members, policy_version, commit_seq }` — distinct for the `channel.actors_remote` reason: an old own-nest ignoring an additive `nest_url` would run its same-nest door on a room it does not home and, the reporter being on its routing roster, bootstrap a **stray floor** as a clean success while the real home stayed stale — a silently wrong answer about membership; an unknown kind fails loud and the seam tallies the report undelivered. The glue picks the kind where the read does, from the channel's recorded home (`RoomRosterReport.home_nest_url`). The relaying nest forwards the position and the ack and stores none of it: it holds no room record for a room homed elsewhere, and a roster it cached would be a membership question it has no authority over. Replay-safe on both hops (a wholesale replace is idempotent). |
| Conversation **room-post verdict read** *(BUILT 2026-09-11; ratified 2026-09-11)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.room_labels.fetch` | the **verdict-read twin of `channel.fetch`** for the one room read that had none, and the piece that puts a room post's badge on a foreign member's card: a room *message*'s verdicts ride `channel.fetch`'s page, which already relays, but a room **post** is its author's ordinary post — it reaches every follower through `fauna.posts.get`, the feed pages and the deep-link door, none of them floor-gated because the bytes are sealed — so its verdicts take a post-scoped door of their own that no envelope read carries ([`../behavior/restricted-posts.md`](../behavior/restricted-posts.md) § Encryption at rest → *Room-restricted — the ruling* → *Built* detail (v) owns that door). That door resolves each post's room from the serving nest's **own** reception-pass map, which only the room's home nest writes (`room_post_view::index_room_post` runs on the nest that *stores* the post), so a member homed elsewhere resolved nothing and was answered empty — and empty is deliberately indistinguishable from “nobody labelled this post”, so the card lost its badge silently rather than loudly. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`) — a room id **is** its channel id — **and the floor on top of it**, unlike the *room roster read* row above. That is not a fresh decision but the verdict plane's standing one: `conversations_handlers::page_verdicts` is already the one home for both rules, shared by the same-nest `channel.fetch` and its federated twin, so the **message** verdict read across this very relay stacks `is_live_floor_member` too, and the two verdict reads may not drift on who a verdict reaches. The roster read's ratified refusal to stack a seat check cannot apply here: it feared denying the newest-seated member off a member-*reported* mirror, and `is_live_floor_member` refuses any room that is not floor-authoritative outright (`db::rooms::RoomRecord::is_floor_authoritative` — “the nest's own ceremonies write its roster”), so the only rooms it passes are ones whose floor this nest writes itself. ⚠ **The request carries `{ requesting_actor_id, room_id, post_ids }` — `room_id` where the same-nest request deliberately has none**, and that difference is the gate's: the same-nest read is post-scoped precisely so a caller need not know which room indexed a post, while the structural gate needs the channel id before anything is resolved. The requester always holds it (a room post names its room in its own `KeyAccess::Room` arm, which is what it opened the post by), and naming it also **narrows** the read — the home answers only for posts its own map assigns to *that* room, so one request cannot fish across rooms. Behind the gate the home runs **the same body its same-nest door runs** (`posts_handlers::room_labels_for`, narrowed to the named room), so a foreign member reads exactly what a member homed here reads. Read-only on every hop: serving a verdict registers nothing and mints nothing. Client leg: the distinct kind `fauna.posts.room_labels_remote { room_id, post_ids, nest_url }` — distinct for the `channel.actors_remote` reason, since an old own-nest ignoring an additive `nest_url` would answer from its own reception-pass map, which indexes no post of a room it does not home, as a clean **empty success** the reader cannot tell from “nobody labelled this post”; an unknown kind fails loud, and the feed's best-effort read then keeps the card's own labels. The glue picks the kind where the conversations plane picks it, from the channel's recorded home (the `RoomPostKeys::room_home_nest_url` seam over `ChannelHome`). The relaying nest forwards the reply and stores none of it: it holds no room record, no map row and no verdict row for a room homed elsewhere, and a verdict it cached would be a floor question it has no authority over. |
| Conversation **room search** *(BUILT 2026-10-17; ruled 2026-10-16)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` **+ the floor on top** (the verdict-read row's gate, for the verdict-read row's reason: `is_live_floor_member` is the same-nest door's own gate, and the two doors may not drift on who a hit reaches) | `fauna.federation.conversation.room.search.fetch` | the search twin of `channel.fetch` for a member homed elsewhere — the one community-room read of a live floor member that still had no relay (`../behavior/community-rooms.md` § Implementation status today owns the door and its rule: hits name positions, ids and ranks, never text; `include_posts` names the room posts the home stores). `{ requesting_actor_id, room_id, query, limit?, include_posts }` → the same-nest `RoomSearchReply`, the home resolving the requester's standing itself and running the body its own `room.search` door runs (`conversations_handlers::room_search_for` — whose floor-seat gate is the floor this row stacks, so it is stacked by construction, never restated); read-only on every hop, the relaying nest stores nothing. Client leg: the distinct kind `fauna.conversations.room.search_remote { room_id, query, limit?, include_posts, nest_url }`, the glue picking it by the channel's recorded home as every `_remote` relay is picked. **Why it exists here:** it is the discovery door a room post takes to a foreign member until the federated follow lands — a post hit, with the room's home, opens through the `posts.get_remote` relay (the post-fetch row above). |
| Conversation **room leave** *(BUILT 2026-09-11; ratified 2026-09-11)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` | `fauna.federation.conversation.room.leave` | the **self-scoped twin of the roster report above**, the room plane's first *mutating self-scoped* relay, and the piece that gives a member homed elsewhere a departure at all: [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization grants *leave (remove self)* to every role but owner with no homing carve-out, yet `room.leave` is a same-nest door and a leaver's own nest holds no room record for a room homed elsewhere, so it answers "no such room" (§ The home nest — every other nest relays). What a departing foreign member could actually reach was the generic `channel.leave` row above, which drops the relay binding and never touches the floor — so the seat outlived the departure. That residue is not inert: the roster-coverage gate **obliges** every later generation mint to wrap to a live floor entry carrying a reception key, so the room could not mint afterwards without handing the generation to a member who had left; and `room.invite` refuses a principal that is already a member, so the same ghost blocked the re-admission that would have healed it, while the departed member — binding gone — could not even read the floor it was still on. Gate: the `channel.fetch` gate verbatim (`require_foreign_member`), and **self-scoped past it** — the body retires the *requesting* actor's seat and nothing else, so a peer nest can end only its own members' membership, never somebody else's. Behind the gate the home runs **the same body its same-nest door runs** (`conversations_handlers::room_leave_apply`: the owner's refusal — a room is never owner-less, unreachable here because a room is born on its owner's home and a transfer re-homes it — then — on a floor-authoritative room only — the binding purge, then the unseat; an end-to-end room's departure moves the seat alone, since that class severs by reading the floor, [`../behavior/conversation-rooms.md`](../behavior/conversation-rooms.md) § Roles and authorization → *Leaving — the mechanism*), so the two doors cannot drift, and the generic `channel.leave` runs it too on a room. **Two acts in the ratified fail-closed order**, `room.remove`'s verbatim ([`../behavior/community-rooms.md`](../behavior/community-rooms.md) § Implementation status today → *A removal severs*): purge the binding **before** the unseat, because the purge reads no floor, so a failed purge changes nothing and a purge over a failed unseat leaves a seat with no relayed reach — tighter than it was, never leakier. The one way this door's residue differs from the removal's is that a *self*-leave's own authorization is the binding it just purged, so its own retry is refused at the gate rather than converging; `room.remove` from any remaining owner or admin — a door that reads a rank, not a binding — is what converges it. **No rotation**, and that is a reason rather than an omission (same §, the leave paragraph): a leaver holds no mint authority, and a mint it could build would still wrap to itself. **Idempotent**, as the same-nest door is too since 2026-09-23: a caller already off the floor is answered with the live count, because this is the door a §4.D re-send arrives at and a refusal here would report a departure that landed as a failure that never happened; the gate still fails loud for a member this nest never bound. Client leg: the distinct kind `fauna.conversations.room.leave_remote { room_id, nest_url }` — distinct for the `channel.actors_remote` reason, since an old own-nest ignoring an additive `nest_url` would run its same-nest door on a room it does not home and answer "no such room", an `invalid_params` the seam cannot tell from a mistyped room id, leaving the leaver believing a departure failed for a reason it could fix while the real home kept them seated; an unknown kind fails loud. It is also the one relayed twin whose `forbid_replay` deliberately differs from its door's (`false` where `room.leave` is `true`), for the idempotence above. The glue picks the kind where the roster relays pick it, from the channel's recorded home (`ChannelHome`). The relaying nest forwards the ack and stores none of it: it holds no room record for a room homed elsewhere, and a seat it mirrored would be a membership question it has no authority over. |
| Conversation **room invite delivery** *(BUILT 2026-09-26; ratified 2026-09-26)* | **(none — channel-only, net-new)** | **nest-signature**; the recipient's own reach policy against the SIGNED inviter | `fauna.federation.conversation.room.invite` | the **room home's push of a community-room knock** to an invitee homed on the receiving nest ([`../behavior/room-invitations.md`](../behavior/room-invitations.md) § Join rules and invites → *A cross-nest invitation*, ruling 1–2). The request carries `{ recipient_actor_id, signed_invite, origin_nest_url }` — the inviter's signed `SignedRoomInvite` verbatim (the record names the room, the invitee, the role and the policy version, and is what the receiver verifies before delivering a word of it) plus the home's self-declared address, honoured only against the connection's verified identity (`federation_handlers::resolve_origin_home_url`, one helper shared with the Welcome relay's `nest_url`). The receiver refuses an unregistered recipient (F10), a record that does not verify or names another invitee, and — **unlike a Welcome, whose wire carries no signed sender** — runs the recipient's reach policy in full over the inviter (`conversations_handlers::conversation_initiation_reach_gate` at `Federation` origin: the floor's `federation_contact` pillar, the inbox mode against the contact edge), then delivers one `InboxKind::RoomInvite` envelope under the recipient's quota with `room_node` bound to the verified home, and stores no room record and no invitation row. Why a kind of its own: `inbox.deliver`'s wire is a signed contact-request pair with its own verification, and `welcome.deliver` fails the reach floor closed for want of a signed sender. **`forbid_replay=true`** on `inbox.deliver`'s grounds (a delivery appends a row and charges quota per call); the home records its `room_invites` row BEFORE originating and consumes it on a failed delivery, so a retry re-delivers exactly one knock. Originated by `room.invite` itself when `invitee_node` is non-empty (`conversations_handlers::room_invite_deliver_abroad`); the home's own reach gate is deliberately not run for a foreign invitee. |
| Conversation **room accept** *(BUILT 2026-09-26; ratified 2026-09-26)* | **(none — channel-only, net-new)** | **nest-signature** + **the delivered invitation names the origin as the invitee's home** (NOT the foreign-member binding) | `fauna.federation.conversation.room.accept` | the **seating twin of the room leave above**, and the one relayed room door whose gate is not `require_foreign_member` — because the `channel_foreign_members` row is what this door WRITES, and writing it at delivery time would open the binding-only doors (the relayed roster read above all) to an invitee who never accepted. The request carries `{ requesting_actor_id, room_id, reception_pubkey }` (the same-nest `room.accept_invite` body plus the requester the home binds). Gate: the home's `room_invites` row for `(room, requester)` must carry the verified `nest_id` the invitation was DELIVERED to — resolved from the delivery dial's handshake, never inviter-declared (`db/rooms.rs::room_invite_home_binding`) — so only the nest an invitation went to can seat that invitee, and self-scoped past it. Behind the gate the home runs **the same body its same-nest door runs** (`conversations_handlers::room_accept_apply`: the standing-offer judgement, the policy-version compare-and-swap seating, the lapse) and THEN inserts the binding with `InsertOnly` power — **seat first, bind second**, the removal's purge-then-unseat inverted for the inverted reason (a seat without reach is the tighter half-state; a binding without a seat would serve the floor to a non-member). **Idempotent**, `room.leave`'s posture: a requester whose invitation from this nest is already accepted and who holds a live seat is answered with its role and its binding re-asserted, which also converges the half-landed "seated, not yet bound" state; a consumed invitation, or an accepted one whose seat was since removed (`unseat_room_member` clears the row), is refused as a stale same-nest accept is. No routing-roster row for a foreign member (pull-only, `../behavior/community-rooms.md` § Implementation status today). Client leg: the distinct kind `fauna.conversations.room.accept_invite_remote { room_id, nest_url, reception_pubkey }` — distinct for the `channel.actors_remote` reason: an old own-nest ignoring an additive `nest_url` would run its same-nest door on a room it does not home and answer "no invitation pending", indistinguishable from a genuine lapse; an unknown kind fails loud. `forbid_replay=false` on the client leg as on `leave_remote`, for the idempotence above. The glue picks the kind off the knock's `room_node` (`RoomCeremonyRpc::room_accept_invite`'s `home_nest_url`), and the shared manager records that URL as the channel's home so every later relay of the room (fetch, send, roster, generations, leave) is routed by the one `ChannelHome` signal. The relaying nest forwards the ack and stores none of it. Proof: `conformance_cross_nest_conversations_client.rs::a_cross_nest_invitation_seats_the_foreign_member_through_its_own_nest`. |
| Conversation **room invite issue** *(BUILT 2026-09-26; ratified 2026-09-26)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id`, and the signed act's signer IS the requesting actor | `fauna.federation.conversation.room.invite_issue` | the **issuing twin of the room accept above**, and the leg that gives a member homed elsewhere the *invite* that `member-invite` grants every member ([`../behavior/room-invitations.md`](../behavior/room-invitations.md) § Join rules and invites → *A cross-nest invitation*, the closing paragraph): `room.invite` is a same-nest door, and an inviter's own nest holds no room record for a room homed elsewhere, so it answers "no such room". The request carries `{ requesting_actor_id, signed_invite, invitee_node, origin_nest_url }` — the same-nest `room.invite` body plus the requester the home binds and the relaying nest's self-declared address. Gate: the signed act's signer must be the requesting actor (`conversations_handlers::verify_room_invite_act`, the same-nest door's own first act, run on the relaying nest too so it never forwards an act under its member's name), then `require_foreign_member` verbatim — that actor holds a live binding on the room from the verified origin; a removal purges the binding with the seat on this class, and the body's seat read refuses whoever a purge missed. Behind the gate the home runs **the same body its same-nest door runs** (`conversations_handlers::room_invite_apply`: the floor-authoritative check, the join-rule judgement, the already-a-member refusal, the delivery), so the two doors cannot drift and a foreign member is granted and refused exactly as a member homed there. **Three deliveries, one body:** `invitee_node` keeps its inviter-side meaning (the invitee's home as the inviter knows it, empty for the inviter's own nest) and picks the arm — empty is resolved by the home to the relaying nest's **verified** identity (`resolve_origin_home_url`, the helper the Welcome and invite-delivery relays share; `origin_nest_url` is honoured only against that identity) and served by the *room invite delivery* push above, aimed back at the relaying nest; a node naming the home itself is a same-nest delivery into the invitee's own inbox under the invitee's reach policy at `Federation` origin (a nest never dials itself); any other node is the same push to that third nest. The delivery's dial records the invitee's home as before, so each invitee's later accept is gated exactly as the *room accept* row says. **`forbid_replay=true`** on both hops, `room.invite`'s own posture and unlike the leave and accept twins: a delivery per call, so the inviter re-issues and the pending invitation is refreshed rather than duplicated. Client leg: the distinct kind `fauna.conversations.room.invite_remote { invite, nest_url, invitee_node }` — distinct for the `channel.actors_remote` reason: an old own-nest ignoring an additive `nest_url` would run its same-nest door on a room it does not home and answer "no such room", an `invalid_params` the seam cannot tell from a mistyped room id; an unknown kind fails loud. The glue picks the kind off the channel's recorded home (`ChannelHome`), as `leave_remote` does. The relaying nest forwards the ack and stores none of it. Proof: `conformance_cross_nest_conversations_client.rs::a_foreign_member_invites_through_its_own_nest_under_member_invite`. |
| Folder **content plane** *(BUILT — read kinds 2026-07-19, write kinds 2026-07-20; ratified 2026-07-18)* | **(none — channel-only, net-new)** | **nest-signature** + foreign-member-bound-to-home-`nest_id` (+ `access == 'writer'` on the write kinds) | `fauna.federation.folder.{changes.fetch,content_key.fetch,actors.fetch,changes.record,write_token.mint}` | cross-nest shared-set discovery + write relay (`actors.fetch` — the writer roster read, BUILT 2026-09-29; [`federation-shared-folders.md`](federation-shared-folders.md) § Cross-nest shared folders + channel append → *The cross-nest writer roster read*); chunk/manifest **bytes never ride this channel** (open by-hash HTTPS bulk plane, `transport.md` § carve-out; the write side gets a short-lived write-only bulk token from `write_token.mint`). **With `folder.read_token.mint` (BUILT 2026-10-03 — the read-scoped twin of `write_token.mint`, member gate alone) this is the whole BUILT folder federation kind set — no `lease`/`conflicts` twin (v1 scope; conflicts a named gap); two more kinds, `folder.serve.announce` and `folder.chunk.wanted`, carry relay serving across nests (ruled 2026-10-01, BUILT 2026-10-04; *Relay serving across nests* in the section).** [`federation-shared-folders.md`](federation-shared-folders.md) § Cross-nest shared folders + channel append. |
| Folder **public read plane** *(BUILT 2026-08-18; ratified 2026-08-18)* | **(none — channel-only, net-new)** | **nest-signature** for attribution + throttle only — the authorization is the ADDRESSED FOLDER's own `audience == 'public'`, never the caller | `fauna.federation.folder.public.fetch` | the publicly-synced follow's read leg ([`federation-shared-folders.md`](federation-shared-folders.md) § The public folder read plane owns the gate reasoning; behavior: [`../behavior/folders.md`](../behavior/folders.md) § Publicly-synced follow). **The one kind here whose request carries NO requesting actor** — deliberate, not an omission: there is no membership to check, so a follower's identity never crosses the wire and the home nest sees only the requesting nest + source IP (its throttle keys). **Deliberately NOT an audience arm on `folder.changes.fetch`:** an OR-ed world-readability branch inside the member gate is one bug away from opening member data, so this kind's gate is the inverse shape — *serve iff the addressed row's current `audience == 'public'`, else `not_found`* (`folder_public::resolve_public_folder`, the one core both this kind and its client twin `fauna.folders.public.fetch` authorize through). Same reasoning nest-locally: `folder_authz::can_read_folder` keeps its membership-only behavior; `FolderReadGrant` gained a `Public` variant that names the grant but is never *returned* by that resolver. **Zero state, zero metering:** the handler writes nothing (no follower rows — not enumerable, not floodable into disk) and charges nothing. Rows are floor-filtered (`folders.public_floor_seq`, schema v44 — nothing recorded before the latest flip-to-public is served) and **stripped** of `device_id`/`author_actor_id`/`path_sealed`/`content_key_version`. Bulk bytes stay off this channel per the standing carve-out: manifests/chunks GET by hash on the open bulk plane, plaintext for a public folder. A flip-back makes the next read answer `not_found` — that IS the revoke. |
| Trend exchange/export *(BUILT 2026-07-14; ratified 2026-07-12)* | **(none — channel-only, net-new)** | **nest-signature** | `fauna.federation.trends.{exchange,export}` | k-gated trend-velocity aggregates (`behavior/trending.md` owns the mechanism — landed there through 2026-07-14, including the exchange-originator legs + the import-triggered `post.get` fetch). **The one ratified, scoped exception to the non-scaling rule (frame D7, 2026-07-12):** distinct chosen peers contribute presence-only buckets that sum on a **capped log ramp** (ceiling 300‰) — permitted because trending is promote-only into a user-chosen feed; claimed magnitudes still buy nothing, and reports keep the flat bucket verbatim. Open-federation, per-origin throttled, small entry cap + peer-row TTL. |
| **Federated follow** *(ruled 2026-10-16 as TARGET STATE; designed the same day — [`../behavior/federated-follow.md`](../behavior/federated-follow.md) owns the design; the subscribe twin, the state leg, the key-blob relay and delivery BUILT 2026-10-17 — § Implementation status today)* | **(none — channel-only, net-new)** | **nest-signature** + the subscriber's own signed `FollowIntent` (under `Capability::Follow`, naming its home nest's id) carried verbatim by their nest, which the home binds to the verified `origin_nest_id` — the `write_token.mint` posture | five kinds, all nest-signature, net-new: `fauna.federation.follow.subscribe` and `follow.unsubscribe` (the signed twins of `fauna.subscriptions.{subscribe,unsubscribe}`, originated inline by the follower's nest and answered with the home's `SubscribeReply`), `follow.deliver` (**push** — the author's nest's outbox sends each stored post's envelope, the `post.forward` payload shape, once per distinct follower home; the receiver admits it only for an author one of its users follows from that origin), `follow.state` (the home's push of a request's fate — approved / rejected / removed — to the subscriber's home) and `follow.key_blob.fetch` (the follower's nest relaying its subscriber's `key_blob.get` to the home, served iff on the roster from that origin); the delete leg widens `fauna.federation.post.delete` with a follow arm rather than adding a kind | a follower on nest A subscribes to an author on nest B and A receives B's author's post **envelopes** for its own index — delivery semantics [`feed-read-model.md`](feed-read-model.md) § The read model → *Posts of authors you follow on other nests*; the subscription and key leg [`../behavior/monetization.md`](../behavior/monetization.md) § Pillar 1 → *A follow across nests*. Envelopes only: the sealed body, `KeyBlob` and attachments stay on the author's nest — the body and attachments fetched by content address from its public blob route, the `KeyBlob` (a roster) read through the follower's own nest over `follow.key_blob.fetch` — and bulk bytes never ride this channel. |
| Nostr proxy-delegation relay *(BUILT 2026-07-22; ratified 2026-07-22)* | **(none — channel-only, net-new)** | **nest-signature** + `nostr_push` pairing capability (`pairing_has_capability`) | `fauna.federation.sync.{nostr_push,nostr_pull}` | Phase-2 head↔public-box proxy relay (`ui/nostr.md` § The bridging gate → Phase 2 owns the mechanism): the paired head **pushes** its `origin='ingest'` rows public-ward (the RPC reply is the ack; auto-provisions the public box's `signing_mode='proxied'` account row, refusing to clobber a deposited `nsec` or a different pubkey) and **pulls** externally-deposited rows head-ward, cursor-parameterized on the head-persisted compound `(stored_at, id)`, non-destructive (the public box keeps serving what it relays — no purge, hence no ack kind). Wire carries only signed public Nostr wire JSON + opaque kind-1059 wraps, never key material. Feature-gated (`nostr`; on in the shipping build — `Dockerfile`'s `--features bluesky,nostr,activitypub`). Registered 2026-07-22 (`register_sync_federation_handlers`). |
| Nest-writer backup plane *(BUILT 2026-07-23; ratified 2026-07-23)* | **(none — channel-only, net-new)** | **nest-signature** + a user-minted **nest-writer grant** (`origin_nest_id` == the owner's granted writer pubkey, unrevoked; gate `require_backup_writer`) | `fauna.federation.backup.{changes.record,write_token.mint}` | the cross-location segment-backup writer plane (§ Nest-writer backup plane owns it): a source nest's in-process coordinator relays an owner's segment-backup custody into the owner's reserved custody-copy set here (resolved-or-created lazily behind the gate, same `record_change_core` as every write plane, owner-charged) and mints a short-lived write-only bulk token (`NestBackupWrite` purpose, 600 s Rust constant) for direct HTTPS chunk POSTs — **bytes never ride this channel**. **NOT pairing-gated:** `is_paired` stays scoped to the private nest-sync surface; the user-minted grant is the allowlist entry, so revocation at the destination freezes the source's writes with the source fully hostile. Registered 2026-07-23 (`register_backup_federation_handlers`). |
| Identity-succession **propagation push** *(BUILT 2026-07-29; anchor-rule hardening 2026-07-29; ratified 2026-07-23)* | **(none — channel-only, net-new)** | **nest-signature** (attribution + throttle only — **no authorization gate**) | `fauna.federation.succession.push` | a home nest tells a peer that an identity the peer holds residue about was superseded (`succession-propagation.md` § Propagation owns the mechanism **and the anchor rule**). **The one mutating kind here with no gate, deliberately — because the push is a HINT whose payload is never trusted:** the receiver takes only *which identity to check* from the request, then verifies against the chain it fetches from its own recorded anchor for that identity (oldest addressable `channel_foreign_members` binding + its persisted `foreign_recovery_heads` head), refusing identities it holds no anchor for; hint-triggered verifies are throttled. A gate would add no security — the trust decision never rests on the sender. (The request's former `chain` field, kept for pre-hardening receivers that verified from the payload, left the wire with the 2026-09-24 compat-remnant sweep.) On acceptance the peer writes its own `actor_successions` row and re-points `channel_foreign_members` + contact edges; a statement naming a **locally-homed** identity is refused (only the client-facing `fauna.recovery.succession.submit` may supersede a local account, because only it re-points the account). Pushed best-effort to peers resolved from the residue rows' `home_nest_id` mapped through the nest-wide `nest_addresses` id→URL directory — each target identity's known addresses, proven-first and per-identity-fair capped, never the membership row's own overwritable address column (`succession-propagation.md` § Propagation owns the rule); the **pull** direction has no federation kind — a peer reads the pre-identity `fauna.recovery.{succession.lookup,registration.chain}` from its anchor over an anonymous client connection, the same way `federation_pool::resolve_peer_nest_id` reads `fauna.nest.info`. Registered 2026-07-29 (`register_succession_federation_handlers`, `federation_handlers.rs:4405`); the push originates in `originate_succession_push` (`federation_pool.rs:2804`) and is fired by `push_succession_detached` (`recovery_handlers.rs:518`). |

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

**Client-side peer discovery for a cross-nest knock** (built 2026-09-20; the piece `fauna.inbox.send` waited on since 2026-06-06). The kind always accepted a `recipient_nest_url`, and the nest always branched on it — what no app could do was *produce* one. A client now does, without any new discovery mechanism and without a new kind: the knock rides `fauna.federation.inbox.deliver` as its transport exactly as the group-invite fan-out does. Two shared-Rust helpers own the decision and the derivation, composed in `find_user_across_nests` so no app re-derives either (priorities #1/#2): `fauna_core::resolve::is_foreign_handle_domain(typed_domain, home_domain)` rules a typed `@domain` foreign by comparing it ASCII-case-insensitively against the domain the caller's *own* nest echoed on a same-nest `fauna.actor.by_handle` (an absent home domain reads as foreign, so a failed local probe never silently resolves `bob@other.test` to a local `bob`); `fauna_provisioning::probe::peer_nest_url` then derives the peer's base URL by the same `resolve_handle_domain` rule the conversations recipient picker's relay `nest_url` uses. The client opens an **anonymous connection straight to that authority** and runs `fauna.actor.by_handle` there — the pre-identity discovery hop, not a home-nest relay. It is deliberately **not** the nest-proxied `fauna.nest.resolve` kind: that one refuses loopback, IP-literal and `.local` authorities by design (`discovery_core.rs`), so it can reach neither a peer on a LAN address nor any two-nest test topology. The resolved URL is then carried on the find result and handed to `fauna.inbox.send`, whose `Some` arm originates the deliver; the receiving nest runs the identical `deliver_inbox_payload_core` with `ArrivalOrigin::Federation`, so a stranger's cross-nest knock takes the recipient's own `InboxMode` route (`allow_knock` → `store_knock`) exactly as a same-nest one would — there is no federation-specific knock policy, which is why this needed no new kind.

tui shipped it as the lead app; every app now calls the one shared lookup `fauna_client_core::find_user::find_user_across_nests` (UniFFI `find_user`, wasm `findUser`, linux directly), which runs the home probe, the `is_foreign_handle_domain` ruling and the direct peer dial together and returns the nest it resolved on; the apps that have not adopted it yet owe only the page wiring. Witnesses: `bins/fauna-nest/tests/conformance_federation_channel.rs` `inbox_send_cross_nest_stores_a_knock_for_a_stranger` (the `allow_knock` + `ArrivalOrigin::Federation` arm, headless) and `tests/e2e-unified/tests/test_contacts_cross_nest_knock.py` (the app journey, two real nests; exclusion class (8) — one nest dialing another is refused in docker as non-global, so the catalog cell comes from standalone and live runs).

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

*Moved 2026-10-17 to [`federation-shared-folders.md`](federation-shared-folders.md), verbatim* — the kinds, gates and wire rules of the cross-nest shared-folder and channel-append plane: the substrate-vs-policy model, the structural foreign-member and foreign-writer gates, the Welcome relay's set name and owner label, the folder-owner marker and its succession arm, recipient-side access discovery, the cross-nest writer roster read, relay serving across nests and § The public folder read plane. Every `§ Cross-nest shared folders + channel append` reference (and every `→ *…*` item under it) resolves there. The kinds themselves stay listed in § Federation residue surface above.

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

**Throttling (ruled 2026-10-13; BUILT
2026-10-13 — `FederationRouterBuilder::add_with_budget`).** Both kinds ride the channel's per-source-IP
and per-nest throttles like every other kind, but at the **writer-plane budget,
not the channel default**: a federation kind's budget is declared beside its
handler at registration (§ Security → *Rate limiting is the primary DoS
defense*), and `backup.changes.record` and `folder.changes.record` declare
`WRITER_RECORD_MAX_EVENTS = 600` per 60 s window (one window for the whole
channel; only the count varies per kind). **Why the default is wrong here:** the
30-per-minute default is sized for human-paced kinds, and a backup pass is a
machine writing two custody records per segment plus a mirror and a journal —
so under the default the first refused record aborted the `(destination, kind)`
pass, a first pass over more than about 14 segments could never drain in one
sweep, moved about 30 records per 15-minute tick, and read as failed toward the
lease's stall threshold the whole time (seen 2026-10-13 on the linux e2e sweep).
**What the throttle bounds on this plane is the pre-gate cost, nothing more:** an
unauthorized caller's request costs a decode, three hex parses and one point
read before `require_backup_writer` refuses it, and an authorized writer's
post-gate cost is owner-charged — `segment-backup-protocol.md` § Cross-location
backup protocol → *Reclaim rate-cap* already rules that **quota-charging is the
rate cap** on this plane and that a defense-in-depth cap stays unbuilt until a
measured abuse signal. So the budget is sized as an abuse bound on a point-read
path: ten a second sustained, the `RECORDS_DOOR_LIMITER_CONFIG` precedent
(`bridge_rate_limit.rs`), a plane of the same shape (grant-gated, quota-charged
record door) — far above any human-paced rate, still a hard bound per source IP
(/64) and per nest. Both checks — IP first, then nest — honour the kind's
budget, so several source nests behind one /64 share one 600-per-minute bucket
per kind. **A legitimate writer may still reach it** (tiny segments on a
LAN-fast destination, a shared /64, version skew), which is why a trip is never
a failure to the writer: the coordinator treats `rate_limited` as backpressure —
wait, retry, and after a full window pause the tuple with its progress kept
(`segment-backup-protocol.md` § Cross-location backup protocol → *Backpressure*
owns that contract). **The same answer for both record kinds, one difference:**
`folder.changes.record` is the same shape (member-gated, owner-charged,
exactly-once by content — [`federation-shared-folders.md`](federation-shared-folders.md) § Cross-nest shared folders + channel append) and its
rate is a relaying nest's whole user population's edit rate on foreign-homed
sets, so it takes the same budget; its backpressure stays the relaying client's
own retry, because that write has a user behind it and the relay passes the
peer's `rate_limited` through unchanged, while the backup coordinator is the one
autonomous writer and owns its own. The two `write_token.mint` kinds keep the
default (one mint per pass per tuple, bounded by the token TTL). **Refused: a
batched record kind.** Batching changes nothing structural — one request per
segment pair still outruns 30 per minute on a fast link, a larger batch widens
the crash-replay window between upload and custody (the per-path
record-before-advance ordering is the data-loss guarantee), the folder plane
cannot batch a client's single record, and it would still need the budget and
the backpressure. Nothing new to configure: every number is a Rust constant,
refutable once real backup telemetry exists.

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

- **Reachability probe = a boolean, never a count — on every arm.** The
  anonymous `fauna.actor.by_handle` reply carries `addressable: bool` ("target has
  ≥1 usable key package, one-time **or** last-resort"). This leaks yes/no, not a
  number, and adds no new anonymous surface. A client resolving a **handle** —
  same-nest or foreign — reads that bit from the reply it already holds and
  makes no second call; a client resolving a bare **64-hex actor id** (no handle
  to look up) asks the authed same-nest
  `fauna.conversations.keypackage.addressable` kind (`{actor_id}` →
  `{addressable: bool}`; User-class, a pure read, the same DB predicate as
  `by_handle`'s bit), which tells a signed-in account nothing
  `fauna.conversations.keypackage.fetch` would not already tell it, destructively.
  **A count is the wrong probe twice over:** it discloses the number, and it
  counts one-time packages only — the top-up gauge must leave the last-resort
  package out — so an actor down to the last-resort package reads `0` to a count
  probe while `take_key_package` still serves them (next bullet); `addressable`
  is the predicate that matches the fallback.
- **The starter-key count is its owner's alone (ruled 2026-10-09).** The authed
  same-nest `fauna.conversations.keypackage.count` kind exists for the bearer's
  own top-up gauge — every Settings → Encryption page, the status page and the
  shared session's `ensure_keypackages` read it for the caller's own id — and
  answers **only** that: a request naming any other actor, from any User-class
  caller the admin included, is refused with
  `fauna.conversations.permission_denied`. A same-nest neighbour is "someone
  looking you up" too, and `../../features/encryption-settings.md` outcome 12
  promises them the bit, never the number; the request keeps its `actor_id`
  field (every app already passes its own), the handler compares it with the
  caller. Nothing in the product needs another account's exact supply — a
  readiness question is the boolean above.
- **Last-resort key package (mandatory).** Every actor publishes one reusable
  last-resort key package at onboarding (standard MLS). `take_key_package` returns a
  one-time KP if the pool is non-empty, else the last-resort KP **without deleting
  it**. This (a) keeps a target reachable after its one-time pool is drained and
  (b) makes `addressable` ~equivalent to "exists", so the probe leaks nothing beyond
  what a successful `by_handle` already revealed.
- **Fetch exhaustion is the real hazard** (one-time KPs are consumed). Mitigated by
  the last-resort fallback, the channel's per-originating-nest and per-source-IP
  throttles, the **nest-signature gate** (fetch is never anonymous) — and, the
  one bound a stranger cannot multiply, a **per-TARGET budget (ruled 2026-10-17)**:
  summed over every originating nest and source, at most 10 one-time KPs per hour
  leave one user's pool through `fauna.federation.keypackage.fetch`
  (`bridge_rate_limit::KEYPACKAGE_FETCH_TARGET_LIMITER_CONFIG`), and a fetch past
  it is served the last-resort KP, consuming nothing. Any fresh keypair passes the
  hello and the fetch names no requester, so the per-nest bucket is keyed on an id
  the caller mints and no relationship can gate the kind without closing open
  federation (§ Trust model: a stranger's first conversation starts with this
  fetch); the target's own bucket is the key that holds. Over budget the target
  stays reachable — new conversations start on the shared last-resort package
  until the window turns — and the pool, half the client's top-up target, is never
  drained between two top-ups.
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
  collide. **The hello is metered too (2026-10-17):** each costs the listener one
  Ed25519 verify and one sign, so `serve_listener` admits at most
  `FEDERATION_HELLO_MAX_PER_IP` (10) per source IP per window before the verify
  runs, and refuses the rest `rate_limited`. **A kind that stores or notifies
  for a caller it has no binding to also spends a per-TARGET budget (ruled and
  built 2026-10-17):** the source IP bounds one address, not a botnet, so every
  open-federation kind that writes for a stranger is bounded on something this
  nest owns — a hosted account, the admin's queue, the whole table — summed over
  every origin. The key-package budget is § Key packages'; the other four are
  owned beside their kinds: `post.forward`'s unpaired forwards
  ([`nest/private-mode.md`](nest/private-mode.md) § Post Forwarding, step 2),
  the federated follow's stranger intents
  ([`../behavior/federated-follow.md`](../behavior/federated-follow.md)
  § Decision 2), `abuse_report.deliver` per subject and per nest
  ([`../behavior/moderation.md`](../behavior/moderation.md) § Routing), and the
  report/trend imports' table-wide row caps
  ([`../behavior/report-sharing.md`](../behavior/report-sharing.md)
  § Federation exchange, [`../behavior/trending.md`](../behavior/trending.md)
  § Federation exchange). A refusal is `rate_limited`, which every sender's
  durable queue retries. The same review closed the companion `welcome.deliver` flood
  (reject delivery to a non-registered local actor before any write). All
  federation/anonymous limiter `DashMap`s are periodically swept (`main.rs`,
  `lib.rs`) so a rotating-source flood can't grow them unboundedly. Reuse
  `bins/fauna-nest/src/bridge_rate_limit.rs`. **The budget is a property of the
  KIND, declared beside its handler at registration (ruled 2026-10-13):** every
  kind takes the channel default unless its registration names its own, and
  the two grant-gated writer-plane record kinds do (§ Nest-writer backup plane
  → *Throttling* owns the budget and its sizing rule); one window for the
  whole channel, only the count varies. The auth-bootstrap kinds ride none
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
  `major.minor`** (anti-fingerprinting), and so is every other anonymous discovery
  answer that carries a version — the fediverse node-info document and the Nostr
  relay's NIP-11 document (`version-compatibility.md` § Dimension 3 holds the
  negotiation-side constraint). **The ONE ruled exception is `GET /api/v1/health`,
  which serves the exact `version`, `commit` and `build_id` to anyone, by design
  (ruled 2026-10-09, confirming the 2026-06-01 exposed-ports review's L2
  acceptance), and — on the same terms — the sha256
  of every binary the image ships (`binaries`, what the deploy binary gate
  compares: `image-pipeline.md` § Verifying a deploy carried the binaries).** The health identity exists so the
  release pipeline can tell one artifact from another from OUTSIDE the box with no
  credential — a fresh pre-claim box has none to present, a degraded box
  (`version-compatibility.md` § 2.2) opens no database and so can verify no one,
  and a door into the box was refused on its own grounds (`image-pipeline.md`
  § Image tags & channels) — and what it discloses is public for public software:
  the commit and the version name a public release, the build id is a run number,
  and the binary hashes are hashes of files in a public image.
  So the coarsening of the discovery surfaces is least-disclosure for their
  purpose (compatibility negotiation needs `major.minor` and nothing more), not a
  secret the health identity betrays; the marginal fingerprint it adds is
  accepted. Should a deployment ever find that leak unacceptable, the ruled order
  is the docstring's: replace the deploy-verify and promotion signal first, gate
  the fields after (`routes::health`, `build_identity.rs`). The anonymous
  **secret-guess** surfaces —
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
- **Two byte fetches stay off the channel (decided 2026-10-19).** A video
  segment's cache-on-fetch (`GET /api/v1/video/segments/<hash>`,
  `video_routes.rs`) and a follow-delivered room post's sealed body
  (`GET /api/v1/blob/<digest>`, `room_post_view.rs`) are HTTPS GETs to the
  peer's **public, content-addressed** read routes, not federation kinds: the
  bytes are verified against the hash the requester already holds, the routes
  answer any client the same way, and nothing about the peer's identity is
  asserted, so the channel's handshake would buy nothing. Their dial is the
  SSRF guard's (`ssrf::peer_fetch_get`: https only, every resolved address
  global and pinned, no redirect followed, the body read capped) because the
  peer URL is one a remote party recorded (`deployment-threat-model.md`
  § Concern 2, item 3); loopback peers are admitted only on the test state.
- **Uniform migration + mixed-version safety.** The channel absorbs every
  Fauna↔Fauna federation call (§ Federation residue surface carries the
  authoritative, current row/kind count — kept there only, not restated here
  where it would go stale) onto `fauna.federation.*` kinds. During the migration
  the channel and the HTTP interim coexisted with capability negotiation (a nest dialed the
  channel, falling back to the HTTP interim when a peer offered no
  `/api/v1/federation/ws`). **Slice 5 (2026-06-03) retired the HTTP interim**
  once the channel was the production version floor: Fauna deploys as a single
  nest (the project's live nest) then, so the floor was met trivially on deploy. Capability
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

- **Moderation obligation delivery + relayed appeal (§ Federation residue surface, that row) — RULED 2026-10-18, BUILT 2026-10-19 (nest).** Both kinds are registered (`moderation_federation.rs`, `forbid_replay: false`), the delivery rides `abuse_report_outbox` and the per-target budgets sit beside the abuse-report pair in `bridge_rate_limit.rs`; what was built and its two-nest witness are [`../behavior/moderation.md`](../behavior/moderation.md) § Implementation status today, the mechanism § Legal takedown → *The author homed on another nest*. Owed: the issuer named on the tui queue row and the two-nest tui witness.
- **The profile read across nests (§ Federation residue surface, that row) — RULED 2026-10-19, BUILT 2026-10-19 (nest).** `fauna.federation.profile.get` is registered (`federation_handlers::register_profile_federation_handlers`, `forbid_replay: false`, 30 s) and serves `latest_profile_bytes` only for a hosted actor whose profile decodes with `audience = Everyone`, every other case the one `not_found`; the relay `fauna.profile.get_remote` originates it through `federation_pool::originate_profile_get`. What was built, its two-nest witness and the app legs still open are [`../ui/profile.md`](../ui/profile.md) § Implementation status today.
- **The federated follow (§ Federation residue surface, the *Federated follow* row) — RULED 2026-10-16 as target state, DESIGNED the same day, BUILT in the nest 2026-10-17.** All five kinds are registered (`follow.{subscribe,unsubscribe,state,key_blob.fetch}` in `bins/fauna-nest/src/follow_federation.rs`, `follow.deliver` in `follow_delivery.rs`), `outbox` carries a `peer_nest_id` target, and `fauna.federation.post.delete` has its follow arm (`follow_delivery::delete_custody`); the succession re-points are not built; the design, what is built and the remaining build rows are [`../behavior/federated-follow.md`](../behavior/federated-follow.md) § Implementation status today.
- **The per-target key-package budget and the hello's per-IP meter (ruled and BUILT 2026-10-17, § Key packages — privacy & exhaustion and § Security → *Rate limiting*).** `federation_handlers::keypackage_fetch_handler` checks `AppState::keypackage_fetch_target_limit` (swept with the other limiters in `main.rs`) and serves `CacheDb::last_resort_key_package` past it; `federation_channel::serve_listener` checks `hello_admits` before `listener_handshake` verifies. Pinned by `conformance_federation_channel.rs`'s `keypackage_fetch_from_fresh_keypairs_cannot_drain_a_pool` (three fresh nest identities together take exactly the budget; every later fetch is served the last-resort package) and `federation_hello_is_throttled_per_source_ip`. The same-nest `fauna.conversations.keypackage.fetch` is not budgeted: its callers are this nest's own accounts.
- **The per-target budgets on the other open kinds (ruled and BUILT 2026-10-17, § Security → *Rate limiting*).** One limiter, `AppState::federation_target_budget` (`bridge_rate_limit::FEDERATION_TARGET_LIMITER_CONFIG`, a one-hour window, swept in `main.rs`), holds the `post.forward`, follow-intent and abuse-report families, each under its own `*_TARGET_MAX` constant; the report/trend caps are `db::reports::MAX_PEER_REPORT_ROWS_TOTAL` and `db::trends::MAX_PEER_TREND_ROWS_TOTAL`, trimmed once per imported batch. Pinned by `bins/fauna-nest/tests/conformance_federation_target_budgets.rs` — one test per kind, each dialling from several fresh nest identities that together cannot exceed the bound while the legitimate caller (a paired relay, a subscriber already seen, a re-delivered report, the next batch) still succeeds. The welcome and inbox deliveries also check the recipient's quota before the payload store spills (`routes::inbox_quota_order_tests`), so a refused delivery leaves no blob for the sweep's grace to carry.
- **Owner-only `keypackage.count` and the by-id `keypackage.addressable`
  probe (ruled 2026-10-09, § Key packages — privacy & exhaustion) — BUILT
  2026-10-11.** `conversations_handlers::keypackage_count_handler` refuses a
  target other than the caller with `fauna.conversations.permission_denied`;
  `keypackage_addressable_handler` answers `has_usable_key_package` (one-time
  or last-resort) to any User-class caller. In `FaunaMlsBackend::resolve_address`
  the handle form promotes on the `by_handle` reply's `addressable` bit with no
  second call and the actor-id form asks `ConversationsRpc::keypackage_addressable`,
  so a last-resort-only peer now resolves. The e2e readiness barriers read the
  owner's own count or `conv_api.actor_addressable`. Witnesses:
  `conformance_conversations_keypackage.rs` (`count_refuses_another_actors_supply`,
  `addressable_*`), `fauna_mls_backend_tests.rs::resolve_address_last_resort_only_peer_resolves`,
  and the tier_3 `test_starter_key_supply.py::test_a_signed_in_neighbour_learns_only_whether_you_can_be_reached`.
- *Moved 2026-10-17 to [`federation-shared-folders.md`](federation-shared-folders.md) § Implementation status today, verbatim:* the build record of the cross-nest shared-folder plane — the nest plane, client read side and write plane (2026-07-19/20), relay serving across nests, the cross-nest owner label, the cross-nest writer roster read, and the folder-owner marker's succession arm.
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
  mismatched answerer, including over an answer cached before the pin, and
  that the dial connects to `ClassifiedPeerHost::dial_addr` rather than
  re-resolving the name (an unresolvable `.invalid` URL dialed by its judged
  address).
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
  `federation_pool::originate_*`; `post.get`'s production originator is the trend
  import's fetch (`exchange_originator`, since 2026-07-14), and — since
  2026-10-16 — the `fauna.posts.get_remote` deep-link relay is its second (§ Federation
  residue surface, the post-fetch row); the discovery poller stores a `fetch_url`
  the *client* reads.
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
- **The id→handle announce — BUILT 2026-09-10 ([`federation-shared-folders.md`](federation-shared-folders.md) § Cross-nest shared folders +
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
  mint `fauna.account.age_nonce` (2026-08-24, `behavior/account-age-band.md`
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
