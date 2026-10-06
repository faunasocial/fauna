# Data Flow — target state

Owns: data-flow
Status: ratified
Authority: owns the end-to-end per-content-kind flow narratives; endpoint classification → [api-layers.md](api-layers.md), WS-RPC framing → [transport.md](transport.md), segment/manifest mechanics → [message-segment-store.md](message-segment-store.md), proxy+worker → [nest/worker.md](nest/worker.md), content-scorer placement → [content-scoring.md](content-scoring.md).

> **Audience:** every area; particularly per-app and nest work tracing how content moves end-to-end.
> **Purpose:** the concrete reference of the data-movement contract per content kind — bridge posts into the unified feed, same-nest and cross-nest DMs, group messages, knocks, file sync, and where deployment topology and push notifications hook in.
>
> Last verified: 2026-07-12 | Sources: `bins/fauna-nest/src/{bridge_method_allowlist.rs,conversations_handlers.rs,contacts_handlers.rs,inbox_handlers.rs,sync_handlers.rs,federation_handlers.rs,blob_routes.rs,ws.rs,rpc_router.rs,posts_handlers.rs,routes.rs}`, `libs/fauna-protocol/src/push_events.rs`, `libs/fauna-carv2/`, `libs/fauna-segment-store/`, `libs/fauna-core/src/obligation.rs`

## Implementation status today

All flows narrated below are implemented and ride their stated live transports. Two
deployment-scope notes: (a) the proxy + worker mode ([nest/worker.md](nest/worker.md)) is
**dormant on production single-box deploys** (standalone is the default) and, even where enabled,
its proxy-side wiring today covers only `Post` (replicate + the sole read-fallback consumer) and
`Inbox` (replicate only, no read fallback) — `Blob`/`Chunk`/`Manifest` have working worker-side
store/fetch handlers but **no proxy-side producer or consumer calls them yet** (generic fallback
across every `PayloadKind` is target-state, [nest/worker.md](nest/worker.md) § Payload Types / §
Read Fallback); (b) `GET /api/v1/segments/{kind}/...` is the permanent segment byte route
(ruled 2026-10-01, [api-layers.md](api-layers.md) § HTTP residue and `segment-backup-protocol.md` § Byte-source endpoint), not a facade awaiting retirement.

## Goal

Specify how content moves through fauna end-to-end, per content kind. Each section below is a concrete data-movement contract — the path a unit of content takes from origin (bridge ingestion, sender's client, sync daemon, etc.) through nest classification, storage, indexing, and fan-out to its consumers. The contracts are uniform across content kinds where possible (channels and groups share the MLS ciphertext pipeline; bridge posts and native posts share the feed-query pipeline) and explicit about cross-nest variants where the transport diverges.

Every app-originated leg below is a WS-RPC kind on the per-actor bearer WebSocket; every
nest↔nest leg is a `fauna.federation.*` kind on the federation channel ([federation.md](federation.md)).
The former HTTP twins for these flows were deleted across the WS-RPC-everywhere rip-outs
(2026-05-23 → 2026-06-19); the surviving HTTP byte/residue surfaces are inventoried in
[api-layers.md](api-layers.md).

---

### Bridge post to unified feed

A Bluesky post (or any bridge content) flows from ingestion, through nest-side storage and
indexing, to feed queries. **The nest classifies nothing at ingest** — it holds only ciphertext +
floor metadata for restricted content, and public posts get no content-scorer treatment either
(`content-scoring.md` § Architectural rules: "the nest never runs a content scorer"). A relayed
post from a paired private nest follows the identical no-classification path
(`bins/fauna-nest/src/federation_handlers.rs` post-forward handler).

```
Bridge daemon                   fauna-nest                           Client
    │                               │                                  │
    │  fauna.posts.create           │                                  │
    │  (signed-envelope Post,       │                                  │
    │   source = "bluesky")         │                                  │
    │ ─────────────────────────────>│                                  │
    │                               │─ verify seal shape / signature   │
    │                               │  (Storage::ingest_post)          │
    │                               │─ store in content table          │
    │                               │─ index in content_fts (FTS5)     │
    │                               │                                  │
    │                               │  fauna.feed.posts                │
    │                               │<─────────────────────────────────│
    │                               │  (feed rules filter by tags,     │
    │                               │   labels, source, media, etc.)   │
    │                               │─────────────────────────────────>│
```

1. The post's author reaches the ingest with a sign-over-CID `Post` envelope — an app through `fauna.posts.create` (its HTTP twin `POST /api/v1/posts` was deleted in the rip-out; User-class only, and own-write: `post.author` must be the caller, [`../ui/feed.md`](../ui/feed.md) § Post creation), a bridge through its own kind, which builds the post under the bridged account's id and runs the same ingest. The envelope is a canonical dag-cbor body, its source field set to the protocol name on a bridged post (e.g. `"bluesky"`); see `docs/goal/architecture/serialization.md` § Embed-as-bytes for signed payloads and `docs/goal/architecture/transport.md` § Wire format
2. Nest verifies the envelope shape / signature (`Storage::ingest_post`, `bins/fauna-nest/src/routes.rs::ingest_post_core`) — a rejection maps to a 400 `RpcError`; no content is read to reach this decision
3. Post stored in `content` table keyed by its `Cid` (codec `0x71` dag-cbor, multihash `0x1e` BLAKE3-256, per `docs/goal/architecture/serialization.md` § CID shape), indexed in FTS5 (`content_fts`) via the same store-time projection (body = `Post::body_text()`, the public preview when a post is gated)
4. Client queries `fauna.feed.posts` -- feed rules filter content by tags, labels, source, media, etc.; scoring metadata a downstream capability-holder has written to the bus (`content-scoring.md` § The scoring-metadata bus) composes into the ordering key (`content-moderation-and-ranking.md` § Composition model)
5. `content_meta.{quarantined,suppressed}` gate every feed query (`bins/fauna-nest/src/db/feeds.rs`) but have **zero production writers today** (verified against code): `set_post_quarantined`/`set_post_suppressed` (`bins/fauna-nest/src/db/moderation.rs:271,281`) are called only from tests (that file's own, plus one in `nostr/store.rs` exercising export/materialization) — not by ingest, not by the legal-takedown path, and not by any labeler write path (`fauna.labels.attach` writes only `content_labels`, `bins/fauna-nest/src/label_handlers.rs`). Legal takedown instead sets the separate `content_meta.legal_takedown_ref` flag, gated in the same feed queries alongside `quarantined`/`suppressed` (`../behavior/moderation.md` § Legal takedown owns that mechanism) — the two are easy to conflate but are distinct columns with distinct writers. Nor is there an ingest-time obligation rule: no production caller evaluates `libs/fauna-core/src/obligation.rs::evaluate_obligations` at `EnforcementPoint::Ingest` (only its own unit tests do). The obligation-rule framework itself is no longer dark, though — it was revived 2026-07-15/16 at a *different* enforcement point, `EnforcementPoint::ClientRender`, as the shared engine behind the family-safety content-policy pillar (`render_verdict`, called client-side via the FFI/WASM `content_render_verdict` across apps; owner: [`../behavior/family-client-enforcement.md`](../behavior/family-client-enforcement.md) § Content policy) — a client-render decision, not an ingest gate, so it doesn't touch `content_meta`. Quarantined posts are not-found to any caller but the author or an admin (the gate is live and read-tested; it simply has no production writer yet).

### DM send (same nest)

Two actors on the same nest exchange an MLS-encrypted direct message. All legs are WS-RPC kinds
on each actor's bearer connection.

1. Sender fetches peer's key package: `fauna.conversations.keypackage.fetch` (consumption/expiry policy owned by [`../behavior/direct-messages.md`](../behavior/direct-messages.md))
2. Sender creates MLS `DmChannel` locally, generating a Welcome message
3. Sender delivers Welcome: `fauna.conversations.welcome.deliver`
4. Nest stores Welcome in recipient's inbox via `push_inbox` (durable copy for offline catch-up) and emits the `fauna.conversations.welcome.received` push (`PushEvent::Welcome { welcome_bytes, channel_id, … }`) so the recipient can join the MLS channel inline — no follow-up inbox fetch needed on the typed-push path
5. Recipient processes Welcome, joins MLS channel
6. Sender encrypts message with MLS, sends: `fauna.conversations.channel.send`
7. Nest appends the ciphertext envelope to the per-channel `__conv/<channel_id_hex>` segment store, assigning a per-channel monotonic `seq` (storage mechanism: [message-segment-store.md](message-segment-store.md))
8. Nest notifies all channel subscribers via the `fauna.conversations.channel.message` push (`PushEvent::ChannelMessage { channel_id, data }`)
9. Recipient fetches (`fauna.conversations.channel.fetch` / `.list_for_actor`) and decrypts locally

### DM send (cross-nest)

Same channel creation and message posting as above. The difference: the key-package fetch and
Welcome delivery to the foreign actor ride the **nest↔nest federation channel**; message retrieval
is a membership-gated **pull relay** the recipient's own nest originates on its drain cycle (the
general mechanism, arbitrary unpaired deployments) — a **paired** private↔public pair may instead
eagerly buffer-pull the same messages (diagrammed below) as an `is_paired`-gated optimization, not a
precondition (mechanism + gating owned by
[`../behavior/direct-messages.md`](../behavior/direct-messages.md) § Technical Flow — Cross-Nest,
steps 3–4).

```
Actor A (client)     Nest-1                          Nest-2              Actor B (client)
    │                   │                               │                       │
    │ keypackage.fetch  │  fauna.federation.            │                       │
    │ (peer on Nest-2)  │  keypackage.fetch             │                       │
    │ ─────────────────>│ ─────────────────────────────>│                       │
    │ <─────────────────│<──────────────────────────────│                       │
    │                   │                               │                       │
    │ welcome.deliver   │  fauna.federation.            │                       │
    │ ─────────────────>│  welcome.deliver              │                       │
    │                   │ ─────────────────────────────>│─ deliver to B's inbox │
    │                   │                               │─ welcome.received push│
    │                   │                               │ ─────────────────────>│
    │ channel.send      │                               │                       │
    │ ─────────────────>│   fauna.federation.sync.      │                       │
    │                   │   mls_pull (cursor poll)      │                       │
    │                   │<──────────────────────────────│                       │
    │                   │──────────────────────────────>│─ channel.message push │
    │                   │   fauna.federation.sync.      │ ─────────────────────>│
    │                   │   mls_ack (up_to_seq)         │                       │
    │                   │<──────────────────────────────│                       │
```

*(diagram shows the **paired** buffer-pull path; the general/unpaired case instead has Nest-2
originate `fauna.federation.channel.fetch` to Nest-1 on B's drain cycle — a pull, not a push —
gated to B's verified home `nest_id`.)*

1. Actor A asks its own nest for B's key package (`fauna.conversations.keypackage.fetch` with B's cross-nest address); Nest-1 originates `fauna.federation.keypackage.fetch` to Nest-2 on the federation channel
2. Actor A creates the DM channel and delivers the Welcome (`fauna.conversations.welcome.deliver`); Nest-1 originates `fauna.federation.welcome.deliver` carrying `nest_url`, `channel_type`, and `channel_id`; Nest-2 lands it in Actor B's inbox and pushes `fauna.conversations.welcome.received`
3. Messages flow through Actor A's nest channel (`fauna.conversations.channel.send` on Nest-1). Actor B's nest retrieves them via the membership-gated `fauna.federation.channel.fetch` pull relay (the general case) — or, when Nest-1 and Nest-2 are paired, the eager buffer-pull below (an optimization, not a precondition):
   - Nest-2 pulls `fauna.federation.sync.mls_pull` from Nest-1 with a `since_seq` cursor (bounded batches; cap owned by [federation.md](federation.md))
   - After processing, Nest-2 acks `fauna.federation.sync.mls_ack` with `up_to_seq` — delivered messages are tombstoned on Nest-1 and physically reclaimed later by conv compaction ([message-segment-store.md](message-segment-store.md))

### Group message

A group conversation is an end-to-end room ([`conversation-rooms.md`](../behavior/conversation-rooms.md)), built by the MLS-native "fork a group from a 1:1" mechanism ([`direct-messages.md`](../behavior/direct-messages.md) § Group-Forked Threads). All legs are WS-RPC kinds on the caller's bearer connection; cross-nest member legs ride the
federation channel exactly as in the cross-nest DM flow. (The nest-side `fauna.conversations.group.*` plane that once sat beside this flow was retired 2026-09-26 — [`groups.md`](../behavior/groups.md).)

1. A member adds a participant to a `(FaunaMls, OneToOne)` thread: the client fetches the participant's key package (`fauna.conversations.keypackage.fetch`), forks a new N-member MLS group and authors the commit
2. New members join the shared MLS group by Welcome (`fauna.conversations.welcome.deliver`, carrying `channel_id` + `group_id`; a cross-nest member's Welcome rides `fauna.federation.welcome.deliver`)
3. Messages ride the MLS ciphertext plane shared with DMs: `fauna.conversations.channel.send` and its `fauna.conversations.channel.message` push fan-out
4. The committing device reports the room's roster to its home nest (`fauna.conversations.room.roster_report`) — a member-reported mirror the nest uses for routing, custody and relay decisions, never a read authority (MLS decides who can read)

### Contact request (knock)

```
Sender              Nest                    Recipient
  │                   │                         │
  │ fauna.inbox.send  │                         │
  │ ─────────────────>│                         │
  │                   │─ check inbox_mode       │
  │                   │  open? auto-accept      │
  │                   │  allow_knock? store      │
  │                   │  contacts_only? gate     │
  │                   │  closed? reject          │
  │                   │                         │
  │                   │  push: fauna.knock      │
  │                   │────────────────────────>│
  │                   │                         │
  │                   │  fauna.knocks.accept    │
  │                   │<────────────────────────│
  │                   │─ status → "accepted"    │
```

1. Sender sends the signed contact-request tuple via `fauna.inbox.send` (same-nest it local-delivers; cross-nest the home nest originates `fauna.federation.inbox.deliver` to the recipient nest)
2. Nest checks recipient's `inbox_mode` (from `Profile`):
   - `open`: auto-accept, contact status becomes "accepted" immediately
   - `allow_knock` (default): store knock, contact status becomes "pending", push `fauna.knock` (`PushEvent::Knock { sender_id, summary }`)
   - `contacts_only`: only deliver if sender is already an accepted contact
   - `closed`: reject
3. Recipient lists pending knocks: `fauna.knocks.list` — sender, sender_node, summary, created_at (read/set the policy itself via `fauna.inbox.mode.{get,set}`)
4. Recipient responds:
   - `fauna.knocks.accept` -- contact status becomes "accepted", knock deleted
   - `fauna.knocks.block` -- contact status becomes "blocked", knock and its doorbell notification deleted (it trains nothing: a block is not a spam verdict — `../behavior/mail-spam.md` § Implicit signals are forbidden)
   - `fauna.knocks.unblock` -- reverses a block
   - `fauna.knocks.dismiss` -- contact relationship deleted entirely (sender can knock again later)
5. Optional: `fauna.contacts.confirm` promotes "accepted" to "confirmed" (roster reads: `fauna.contacts.{list,status}`)
6. A background task expires stale knocks and accepted-but-unconfirmed contacts

### File sync

```
Device A              Nest                              Device B
  │                        │                              │
  │  fauna.sync.register   │                              │
  │ ──────────────────────>│                              │
  │                        │                              │
  │  POST chunks           │                              │
  │ ──────────────────────>│                              │
  │  POST manifests        │                              │
  │ ──────────────────────>│                              │
  │  fauna.sync.changes    │                              │
  │    .record             │                              │
  │ ──────────────────────>│                              │
  │                        │  fauna.sync.changed (push)   │
  │                        │─────────────────────────────>│
  │                        │  fauna.sync.changes.list     │
  │                        │<─────────────────────────────│
  │                        │  GET manifests / chunks      │
  │                        │<─────────────────────────────│
```

**Control plane (WS-RPC):** clients manage sync — device registration, change records, status,
folders, conflicts — via WS-RPC kinds on the bearer connection: `fauna.sync.register`,
`fauna.sync.changes.{record,list,supersede}`, `fauna.sync.{status,files,backup_status}`,
`fauna.sync.devices.{list,delete}`, `fauna.sync.conflicts.{list,report,resolve}`, plus the
`fauna.folders.*` folder surface. Apps never talk to a separate sync daemon directly
(`app-guidelines.md` § "The nest is the control plane for sync"); behavior owner:
[`../behavior/file-sync.md`](../behavior/file-sync.md).

**Byte plane (HTTP residue):** bulk bytes stay HTTP — `POST/GET /api/v1/chunks*` and
`/api/v1/manifests*` (inventory owner: [api-layers.md](api-layers.md)). The former device
WebSocket `GET /api/v1/sync/ws` is removed ([`../behavior/file-sync.md`](../behavior/file-sync.md)
§ Relay serving → *The `/sync/ws` data plane leaves with the daemon*).

1. Device registers: `fauna.sync.register` with device_id, label, capabilities (default: `read,write`)
2. Device holds its ordinary WS-RPC bearer connection — the same one that carries the control plane above
3. Device uploads file content as chunks: `POST /api/v1/chunks` -- each chunk BLAKE3-hashed, encrypted, compressed, stored in blob store
4. Device uploads chunk manifest: `POST /api/v1/manifests` -- lists all chunk hashes for a file
5. Device records change (`fauna.sync.changes.record`, the one ingest rail — it routes `web`-mode sets into `web_files` and fires the nudge below): path, manifest_hash, size_bytes, change_type
6. **Remote-change nudge (the delivery path for an ordinary second device of the same user):** the nest fires a best-effort push, `fauna.sync.changed` (`PushEvent::SyncChanged`), at every other same-nest device connected to the set. A nudged device — and, identically, one that simply reconnects after being offline — pulls what it's missing itself: `fauna.sync.changes.list` with a `since` cursor, then the manifest and chunks over the byte plane. A missed push costs only latency; the periodic rescan is the correctness backstop. Full mechanism: [`../behavior/file-sync.md`](../behavior/file-sync.md) § Remote-change nudge.
7. **(RETIRED 2026-08-18.)** A second, narrower nest-initiated push — "destination forwarding" over admin-registered `folder_destinations` rows (`SyncOrchestrator`) — was deleted with that phantom rail (folders re-model row 7): no app or daemon ever created a row, so the forward never ran in production and every device has always relied entirely on step 6.

### Deployment architecture

fauna-nest deploys **standalone** by default: one process serves everything — WS-RPC, HTTP
residue, SQLite storage, blob store. It needs no configuration beyond the artifact-set data dir
(`FAUNA_DATA_DIR`, deployment-artifact IPC — no human edits it); the domain is claimed via the
app UI.

The optional **proxy + worker** mode adds distributed overflow capacity: a separate worker
process connects over `GET /internal/worker/ws` (Ed25519 challenge-response) and can store/fetch
any `PayloadKind` (`Inbox`, `Post`, `Blob`, `Chunk`, `Manifest`) at the wire-protocol level, but
today's proxy-side wiring only *drives* that for `Post` (fire-and-forget replicate + the sole
transparent read-fallback consumer) and `Inbox` (replicate only); `GET /internal/router-status`
reports health/capacity to the `fauna-router` load balancer. Topology, wire protocol, auth, and
the full per-kind scope are owned by [`nest/worker.md`](nest/worker.md) § Payload Types / § Read
Fallback — this doc only places the mode in the flow picture: replication happens *after* local
storage on the flows above and is invisible to clients.

### Push notifications

Server-initiated events ride the same per-actor WS-RPC connection as typed `PushEvent` Push
frames. The authoritative wire-kind ↔ payload table lives in [transport.md](transport.md) § Push
events (regenerated from the `PushEvent` enum, which is the source of truth for the count) — this
doc only names the trigger points the flows above produce:

- a channel/group ciphertext append pushes `fauna.conversations.channel.message` (step 8 of the DM flow);
- a Welcome landing in an inbox pushes `fauna.conversations.welcome.received` (step 4);
- a stored knock pushes `fauna.knock` (step 2 of the knock flow);
- a non-Welcome inbox delivery pushes `fauna.inbox.item`;
- a recorded file-sync change pushes `fauna.sync.changed` to every other connected device of the set (step 6 of the file sync flow);
- unified notifications (mentions, RSVPs, …) push `fauna.notification`;
- a connection that overflowed its push queue receives `fauna.protocol.resync_required` and re-pulls its snapshots (backpressure contract: [transport.md](transport.md) § Backpressure).

### At-rest storage — CARv2 segments and manifests

The load-bearing flow invariant (CBOR-DAG-everywhere Layer 3, closed 2026-05-17): **the wire
bytes for a record (signed post, channel ciphertext, …) are byte-identical to that record's block
bytes inside a CARv2 segment — one canonical form, one CID, one signature, whether in flight or
stored.** A record's identity is its dag-cbor CID (codec `0x71`, multihash `0x1e` BLAKE3-256,
36 bytes), the same identity vocabulary across mail, posts, channels, calendars.

Mechanism owners: segment file shape (CARv2 + dag-cbor sidecar), the outer per-kind manifest,
active-vs-finalized read paths, tombstoning and compaction →
[message-segment-store.md](message-segment-store.md) § Segment file format (the `libs/fauna-index`
IndexManifest is a single-block CARv2 sharing the same `KindManifest` type — same owner section);
canonical bytes / CID shape / sign-over-CID → [serialization.md](serialization.md); the WS-RPC
frame carrying the same bytes (embed-as-bytes) → [transport.md](transport.md) § Wire format.

#### Blob URLs (CID-keyed)

Blob bytes are content-addressed by their CID and flow over HTTP (never a WS-RPC payload — size
budget per [transport.md](transport.md) § Wire format); classification rationale for these
residue endpoints → [api-layers.md](api-layers.md).

| Method | Path | Behavior |
|--------|------|----------|
| `GET` | `/api/v1/blob/{cid_b32}` | Returns `application/octet-stream`. Server verifies `blake3(stored_body) == cid.digest()` before returning. |
| `PUT` | `/api/v1/blob/{cid_b32}` | Server-side CID-match verification before accepting upload (rejects if `blake3(body) != cid.digest()`). Idempotent under matching CIDs. |

`{cid_b32}` is the canonical base32-lower multibase CIDv1 encoding (`b` prefix) from
[serialization.md](serialization.md) § CID. Implementation `bins/fauna-nest/src/blob_routes.rs`;
blob bytes are stored via the shared `BlobStoreBackend` — the hex
`GET /api/v1/blob/<hex>` shape shares the same backend and bytes, and both shapes are
permanent ([api-layers.md](api-layers.md) § Remaining HTTP). The metadata side (upload-receipt, transcode status) rides WS-RPC kinds
per [core-client-kind-catalog.md](core-client-kind-catalog.md) § Blobs & Media.
