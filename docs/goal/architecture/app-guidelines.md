# App Guidelines — target state

Owns: app-guidelines
Status: ratified
Authority: the cross-app rule book — the 9 app rules, the anti-patterns, the protocol-specific-vs-unified litmus test, and the system-architecture diagram; defers endpoint/kind classification to architecture/api-layers.md, cross-app mechanics + the platform-binding tables to architecture/apps/common.md, WS-RPC framing to architecture/transport.md, and each model's mechanism to its owner (auth ceremony → behavior/login.md, content/CID wire shape → architecture/serialization.md, MLS protocol → behavior/direct-messages.md, contact-edge lifecycle → ui/contacts.md, synced account settings (the former `UserConfig`) → architecture/config-dissolution.md).

Rules for building fauna apps. **Read this and `api-layers.md` before starting any app work.**

## Goal

Every fauna app — across web, linux, windows, macos, ios, android, tui — presents the same unified surface to the user (one feed, one inbox, one bridges list, one notifications stream, one search) backed by a single nest abstraction layer that normalizes external protocols (Bluesky, Nostr, ActivityPub) into fauna's data model. Clients hold MLS encryption authority; the nest relays ciphertext. The 9 rules below, together with the identity/content/encryption/profile models and the anti-patterns, are the target shape every app conforms to — uniformity over per-app divergence is the engineering priority.

## Implementation status today

The rule book matches shipped code: the WS-RPC-everywhere migration is complete (control-plane HTTP twins deleted; kinds are the surface — `apps/common.md` § Implementation status), the unified interact/notification surfaces are adopted, and all seven apps follow the rules below with no outstanding rule-9 violations. The formerly-declared gap — macOS driving its sync daemon via `launchctl`/CLI (`SyncDaemonManager.swift`) instead of the `fauna.sync.*` kinds — was closed 2026-07-13 with the B2 shared-engine cutover (`SyncDaemonManager` deleted outright; `apps/macos.md` § Sync). The per-user sync-agent macOS provisions today is lifecycle-tending of the app's own helper (the ratified Windows pattern), not daemon control-plane management (`apps/sync-agent.md` § Implementation status today).

## System Architecture

```
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│  Web (Svelte)│  │Android (Kt) │  │  iOS (Swift) │  ... more apps
└──────┬───────┘  └──────┬───────┘  └──────┬───────┘
       │                 │                 │
       │   WS-RPC (bearer subprotocol)     │
       └─────────────────┼─────────────────┘
                         │
                   ┌─────▼─────┐
                   │ fauna-nest │  ← the server ("nest")
                   │  (Rust)    │
                   └──┬──┬──┬──┘
                      │  │  │
          ┌───────────┘  │  └───────────┐
          ▼              ▼              ▼
   ┌────────────┐ ┌────────────┐ ┌────────────┐
   │  Bluesky   │ │   Nostr    │ │ActivityPub │  ... protocol modules,
   │  module    │ │  module    │ │  module    │     all in-process
   └────────────┘ └────────────┘ └────────────┘
          │              │              │
          ▼              ▼              ▼
   ┌────────────┐ ┌────────────┐ ┌────────────┐
   │ AT Protocol│ │Nostr relays│ │ Fediverse  │
   └────────────┘ └────────────┘ └────────────┘
```

**The nest is the abstraction layer.** All external protocol content is normalized into fauna's data model before reaching apps. Apps talk to the nest, not to external protocols directly. Bluesky, Nostr, and ActivityPub are **in-process, feature-gated modules inside the `fauna-nest` binary** (protocol logic in `libs/fauna-bridge-{atproto,nostr,activitypub}`, nest glue in `bins/fauna-nest/src/{bluesky,nostr,activitypub}`) — none is a separate daemon process. The one genuinely separate bridge process is the mail bridge (`bins/fauna-bridges`), a peer WS-RPC client of the nest (rule 9's *Bridges* bullet), not pictured above.

### Identity Model

Every actor has an **Ed25519 keypair** (`ActorKeypair` in `libs/fauna-core/src/identity.rs`):
- **ActorId**: 32-byte public key — the universal identifier
- **Signing**: Ed25519 for posts, profiles, tombstones, delivery receipts
- **Key agreement**: Ed25519 converts to X25519 for Diffie-Hellman (used by MLS)
- **Identity succession (loss/theft recovery)**: a compromised or lost key is replaced by `IdentitySuccession` — a genuinely new `ActorId`, authorized by the actor's offline-only `RecoveryKey` rather than by the old key itself (a thief could hold the old key, so old+new dual-signing would authorize a takeover, not recovery) — owned by `behavior/identity-succession.md`
- **Multi-device**: `DeviceAuthorization` grants a capability set to secondary device keys (`behavior/devices.md` § DeviceAuthorization owns the enum + the device-signed-authoring acceptance rule) — `RenewBearer` is a narrow additive grant for app-dead sync-agent bearer renewal only, not a general capability (`apps/sync-agent-credentials.md` § Credential model)

### Content Model

The fundamental content unit is `Post` (`libs/fauna-core/src/data.rs`):

```
Post {
    author: ActorId,
    body: PostBody,          // Text, Media, TextWithMedia, Structured, Video
    references: Vec<Reference>,  // Reply, Repost, Quote, React, Upvote, Downvote
    created_at: Timestamp,
    expires_at: Option<Timestamp>,
    gated: Option<GatedInfo>,    // encrypted/paywalled content preview
    content_warning: Option<String>,
}
```

`Post` no longer carries a `signature` field on the struct — sign-over-CID
ships the signature in a `SignedEnvelope` alongside the canonical bytes per
the embed-as-bytes wire shape in `docs/goal/architecture/serialization.md`
§ Embed-as-bytes for signed payloads.

Rich text uses **Facets** (byte-range annotations): `Mention`, `Link`, `Tag`.

Media is content-addressed: `MediaItem { blob_hash: ContentHash, media_type, size_bytes, dimensions, thumbnail }`, plus two additive optional fields a bridged item carries — `remote_url` and `alt` — owned by [`../behavior/bridges.md`](../behavior/bridges.md) § Unified feed ingestion → *Bridge ingestion* ruling 4.

Video uses HLS: `VideoSegment { hash, resolution, codec, bitrate }` with `VerificationAnchor` perceptual hashes for integrity.

Posts are identified by their `Cid` (codec `0x71` dag-cbor, multihash `0x1e` BLAKE3-256 of the canonical dag-cbor bytes — see `docs/goal/architecture/serialization.md` § CID shape). The legacy per-kind `PostId([u8; 32])` newtype has been retired: `PostId` is now a type alias for system-wide `Cid`, kept only for kind-named clarity at API boundaries (`libs/fauna-core/src/data.rs`).

### Encryption Model (MLS)

Messaging uses the **MLS** (Message Layer Security) protocol (`libs/fauna-mls/`). The nest is a **relay only** — it stores and forwards ciphertext but cannot decrypt; clients own the engine, key packages, and encrypt/decrypt. The channel protocol (channel types, key-package lifecycle, Welcome flow, send/receive kinds) is owned by `behavior/direct-messages.md`; the per-platform library boundary by `apps/common.md` § MLS.

### Profile Model

```
Profile {
    actor_id: ActorId,
    display_name, bio, avatar, banner: Option<ContentHash>,
    links: Vec<ProfileLink>,
    nests: Vec<NestEntry>,        // nests this actor uses (with roles: Social, Mls)
    admin_nests: Vec<AdminNestEntry>,
    inbox_mode: InboxMode,        // Open, AllowKnock (default), ContactsOnly, Closed
}
```

## App Rules

### 1. Use unified feeds as the primary content view

All content — native fauna posts, bridged Bluesky posts, bridged Nostr notes, federated ActivityPub posts — arrives in the unified feed via the `fauna.feed.*` WS-RPC kinds. Each post carries a `source` indicating its origin protocol. Apps should render a **single feed view** with source badges, not separate timelines per protocol.

**Reference:** all seven apps use this pattern.

### 2. Use unified inbox for all messaging

All direct messages — fauna-native MLS DMs, bridged Bluesky Chat, bridged Nostr DMs, email — arrive in the unified inbox, drained over the WS-RPC kinds:

```
fauna.inbox.fetch   # peek undelivered items (no status change)
fauna.inbox.ack     # mark items delivered, after durably applying them
```

(The `GET /api/v1/inbox/{actor_id}` HTTP twin was deleted in the WS-RPC-everywhere rip — its mark-on-read drain had a data-loss bug the fetch/ack split fixes. A read-only *display* surface peeks and never acks; only a durable-apply consumer acks — see `core-client-kind-catalog.md` § Inbox & Messaging.) Apps should render a **single inbox view**, not separate message views per protocol.

### 3. Use bridge management APIs for bridge connections

Bridge linking, unlinking, follows, and settings use the **generic bridge management API** — the typed `fauna.bridges.*` WS-RPC kinds (the HTTP twins were deleted in the T9+T10 sweep; `api-layers.md` § Layer 3 owns the kind catalog):

```
fauna.bridges.list            # list available bridges + status
fauna.bridges.link            # link (starts OAuth/auth flow); forbid_replay
fauna.bridges.link_challenge  # proof-of-possession challenge before an external-signer link
fauna.bridges.unlink          # unlink
fauna.bridges.set_settings    # update per-bridge settings
fauna.bridges.list_follows    # follows through this bridge
fauna.bridges.add_follow      # follow an external account; forbid_replay
fauna.bridges.remove_follow   # unfollow
fauna.bridges.list_follow_requests     # follow requests waiting on your account
fauna.bridges.resolve_follow_request   # approve or refuse one; idempotent
```

Reach them through the platform binding (`fauna-client-bridges::BridgesClient` natively; `fauna-ffi`'s `FfiBridgesClient` for the UniFFI/C# apps; `fauna-wasm` for web) — see `apps/common.md` § Nest Connection → *WS-RPC client binding*. Only the link dialog needs protocol-specific fields (e.g., Bluesky handle, Nostr npub), composed as a `fauna_cbor::Value` `params` tree. Everything else is generic.

### 4. Protocol-specific APIs: when they ARE appropriate

Some features are genuinely protocol-specific and have no unified equivalent. These are legitimate uses of protocol-specific APIs:

| Feature | API | Why it's OK |
|---------|-----|-------------|
| Bluesky thread context | `bluesky.feed.thread` (WS-RPC kind; HTTP twins deleted 2026-06-05) | Thread structure is protocol-specific |
| Nostr zap totals | `nostr.zaps.total` (WS-RPC kind; HTTP twin `/api/v1/nostr/zaps/{event_id}` deleted 2026-07-22) | Zaps (Lightning payments) are Nostr-specific |
| Nostr badges | `nostr.badges.list` (WS-RPC kind; HTTP twin `/api/v1/nostr/badges/{pubkey}` deleted 2026-07-22) | Badges are Nostr-specific |
| Nostr signed-event publish | `nostr.events.publish_signed` (WS-RPC kind; HTTP twin `/api/v1/nostr/publish-signed` deleted 2026-07-22) | NIP-07 browser-extension signing is Nostr-specific |

Account linking is **not** in this table for Bluesky, Nostr, or ActivityPub: all three ride the unified `fauna.bridges.{link,list,unlink}` surface (no `/api/v1/nostr/link` route exists; the duplicate `/api/v1/activitypub/*` control-plane HTTP routes — including `enable` — were **ripped 2026-07-16** in favor of `fauna.bridges.link` with link mode `enable`, following the Nostr precedent — `behavior/activitypub.md` § Architecture → *Control plane*; only the Bluesky OAuth `auth/callback` redirect stays HTTP residue). Bluesky feed discovery was removed (custom-feed subscription is the unified `fauna.bridges.feeds.*`).

**The rule:** Use protocol-specific APIs only for features that are **genuinely unique to a protocol** and cannot be expressed in unified terms.

### Features that SHOULD be unified (not protocol-specific)

The nest knows the origin protocol of every post and can route interactions and notifications to the correct bridge internally.

| Feature | Wrong (protocol-specific) | Right (unified) | Status |
|---------|--------------------------|-----------------|--------|
| Interactions | ~~`/api/v1/bluesky/interact/*`~~ (removed 2026-06-05) | `fauna.posts.interact` — nest propagates to origin protocol | **Done** — the unified kind is the only interaction surface (HTTP twin deleted). |
| Notifications | (no bluesky notification route ever existed) | `fauna.notifications.{list,mark_read,count}` — nest aggregates from all protocols | **Done** — Bluesky notifications are polled (`notif_sync`) and translated into the unified table; HTTP twins deleted (T4). |

**Interactions:** the unified interact kind routes to Bluesky (like/repost/reply/quote), Nostr (like/repost — signed and published to relay list), and ActivityPub (Like/Announce via delivery queue). The nest resolves the post's source and dispatches to the correct protocol handler. Nostr reply/quote via the unified kind are future work.

**Notifications:** "Someone liked your post" is the same event whether the liker is on Bluesky, Nostr, or fauna-native. The nest aggregates notifications from all protocols into a unified stream, just as it aggregates posts into unified feeds and messages into unified inbox.

### 5. MLS: clients own encryption

The nest relays ciphertext; clients own the MLS engine, key-package publication/maintenance, Welcome processing, and all encrypt/decrypt. The flows and kinds (`fauna.conversations.*`) are owned by `behavior/direct-messages.md`; the per-platform library boundary table by `apps/common.md` § MLS — do not re-derive either here.

### 6. Real-time updates via WS-RPC

```
GET /api/v1/ws/{actor_id}
Sec-WebSocket-Protocol: fauna.v1, bearer.<token>
```

The single per-actor WebSocket carries the WS-RPC transport (Spec Y): bearer travels in the `Sec-WebSocket-Protocol` subprotocol header (the legacy `?token=` query param is gone), and the connection multiplexes Request/Reply, Cancel, and typed Push frames in canonical DAG-CBOR. Clients use the shared `libs/fauna-client` façade (`NestClient::request*` + `subscribe_pushes`) to maintain a single persistent connection per actor and trigger UI updates from typed pushes rather than polling. Push-frame catalog + framing: `transport.md`.

### 7. Settings: per-app vs server-side

- **Server-side:** account settings (inbox mode, spam preferences, bridge settings, email filters, …) — synced between devices as account-state plane kinds; where each former `UserConfig` field went: [`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *The kinds* (the `__config` blob rail retired 2026-10-02, the same § → *The closure order*, step (6)).
- **Per-app:** UI preferences (theme, layout), notification settings, local cache settings.

### 8. Contact flow

Knock-send rides `fauna.inbox.send` (a ContactRequest is an inbox message); the knock/contact lifecycle rides `fauna.knocks.{list,accept,block,dismiss}` + `fauna.contacts.{confirm,list}`, gated by the peer's `InboxMode`. The contact-edge lifecycle (semantics, unblock rules, page behavior) is owned by `ui/contacts.md`; the kind table by `apps/common.md` § Contacts.

### 9. Sync: always through the nest

The nest is the **control plane for sync**. All sync management goes through the nest API (the `fauna.sync.*` / `fauna.folders.*` / `fauna.filesync.*` WS-RPC kinds) — apps never drive a *separate* sync daemon over a side channel.

This rule governs the **control/management plane**, not where the byte-syncing runs. The sync *engine* (watcher → chunk pipeline → conflict-aware apply → state DB) is the shared `fauna-sync-engine` crate (`libs/fauna-sync-engine`); it runs in one of two deployments, **both of which route management through the nest**: **in-process** inside a desktop app (iOS keeps this shape entirely; on linux/macOS/windows it has **narrowed to app-side one-shots** — photo ingress and restore walks — now that the always-resident engines run in deployment 2, per `apps/linux.md` § File Sync. The in-app segment-backup driver that stayed alongside them is **deleted on every app** (the slice-5 flip, complete 2026-08-16) and never moved to this agent: the D6 milestone (host it agent-side) was **withdrawn 2026-07-23** in favor of the nest-side model — the source nest itself drives segment backup, not any client or agent — see `apps/sync-agent.md` § D6 destination-auth gap + `backup-restore.md` § Background Tasks → *Flip status (slice 5)*), as the **per-user sync+backup agent** (`bins/fauna-sync-agent`) — a helper process in the user's own logon session, hosting the always-resident engines, **live on all three desktops** (ratified 2026-07-18; linux + macOS cut over 2026-07-19, windows 2026-07-22 — `apps/sync-agent.md`) and, on a NAS/server, behind fauna-tui. A third deployment, the **autonomous headless daemon** (`bins/fauna-sync`), was removed 2026-10-02; the headless story is fauna-tui + the per-user agent, which collapsed it into deployment 2 — `apps/sync-agent.md` § Headless deployment owns the ruling. The rules below are about (a) management flowing through the nest and (b) not reaching a *separate* daemon via IPC/CLI — neither forbids an in-process engine.

**Crate layering of the `db` floor (ruled 2026-08-03; extraction EXECUTED 2026-08-10): the floor lives in `libs/fauna-account-store`; `fauna-sync-engine` re-exports it.** The 2026-08-03 ruling kept `src/db.rs` — the client-side state-dir + SQLite *floor* (per-set state DBs, `device.db`, actor-scoped dir adoption/migration) — in-crate until a production consumer wanted `db` without the engine. That trigger arrived with the account data plane ([`account-data-plane.md`](account-data-plane.md) § The account store: the store grows out of this floor), and the migration ran exactly as pre-ratified here: `db.rs` moved verbatim into the new `libs/fauna-account-store` member (auto-covered by the image's wholesale `COPY libs/` — `build-system.md` § Dockerfile workspace-member coverage gate), together with its inherent-`impl` satellites `succession_drain.rs`/`succession_progress.rs` (the orphan rule requires impls on `SyncDb` to live beside the type), and `fauna-sync-engine` keeps `pub use fauna_account_store::{db, succession_drain, succession_progress};` so no call site re-paths. What stays true: "app X links the sync engine but runs no engine" is *not* a smell to fix — the crate is engine **and** (re-exported) storage floor, and linking it for the floor alone remains the intended shape; a *new* floor-only consumer may instead take `fauna-account-store` directly. The boundary invariants moved with the code and are now crate invariants declared in `fauna-account-store`'s docs: **the floor grows no dependency on the engine or its graph** (tokio/reqwest/notify/MLS — a change needing one is a layering question to raise, not a dep to add; foundation facades `tracing`/`serde`/`serde_json` are within the floor), and **nothing that constructs a `SyncEngine` belongs to the floor** — the client-side segment-backup coordinator (`BackupCoordinator`) was the illustrative case at the time of this ruling: it built a `SyncEngine` in production, sitting *on top of* the engine, so it stayed engine-side even though it touched the floor. That coordinator has since been deleted outright (the client upload coordinator was retired at the segment-backup slice-5 flip; segment backup for nest-originated kinds is now driven nest-side — [`../behavior/backup-destinations.md`](../behavior/backup-destinations.md) § Implementation status today), so the rule now has no live example in this crate — restated for whatever next construct in `fauna-sync-engine` builds a full `SyncEngine`: that code stays engine-side, never the floor. The account plane above the floor followed the same pattern on 2026-09-27: its wasm-capable half moved to `libs/fauna-account-plane`, which `fauna-sync-engine` re-exports module by module ([`account-client-lifecycle.md`](account-client-lifecycle.md) § The client-side lifecycle → *The trigger fired*, ruling (1), owns the split).

- **Folders, devices, sync status, conflicts, location configuration** — all managed via the nest's sync kinds, whichever deployment runs the engine. (The only surviving HTTP sync surface is the byte plane — chunk and manifest upload and download; the `/api/v1/sync/ws` data-plane upgrade was removed 2026-10-02 with the daemon that dialed it — `../behavior/file-sync.md` § Relay serving.)
- **A *separate* sync process (the per-user agent, on any desktop or behind fauna-tui on a headless box) registers with and is controlled by the nest.** It connects via WebSocket, receives configuration, and syncs according to its instructions. Apps do not need to know where it runs or how to reach it.
- **Desktop apps do NOT reach a *separate* sync daemon directly** — no pipes, no CLI invocations, no local IPC **for sync management**. The "nearest nest" may be on the same device (localhost), on the LAN, or remote (WAN). (iOS runs the engine **in-process** entirely and is not "communicating with a daemon" at all.) On linux/macOS/windows the always-resident engines run in each app's own per-user sync-agent — a genuinely separate process — but the app's relationship to *that* agent is **lifecycle-tending of its own helper** (spawn/stop: linux via a systemd user unit, macOS via `launchctl`, windows via the per-logon `Run` key), not driving a *separate* daemon's control plane over a side channel, so it is not a rule-9 violation; the local `fauna-ipc` seam each app uses to reach its own agent carries only the structurally-local (capability provisioning, device-local location↔set binding, local status) — never per-set policy, which stays in the nest rows. See `apps/sync-agent.md` § Implementation status today.
- **Bridges are peer WS-RPC clients of nest, not co-located IPC.** `fauna-mail-bridge` connects to nest over WS-RPC + DAG-CBOR on `/api/v1/ws/{actor_id}` with its own service-user keypair — no pipes, no Unix socket, no co-located side channel. Apps access bridge features through the nest's bridge management API (the `fauna.bridges.*` WS-RPC kinds).
- **Headless sync:** Because the agent is controlled through the nest, sync works without any GUI — on a NAS or server, fauna-tui provisions the per-user agent, which then syncs autonomously (`apps/sync-agent.md` § Headless deployment). Any app (phone, desktop, web) can manage its folders and status through the nest.

## Anti-Patterns

These mistakes have been made. Do not repeat them.

### Do NOT build protocol-specific timelines

**Wrong:** Separate "Bluesky Timeline" view, separate "Nostr Feed" view, separate "Fediverse Feed."

**Right:** Single unified feed from the `fauna.feed.*` kinds with source badges. Bluesky/Nostr/AP content is already normalized and included.

**Why:** On 2026-03-27, ~2,700 lines of protocol-specific views were built for the Linux app that duplicated what the unified feed already provides. The standalone Bluesky feed endpoints (`/api/v1/bluesky/feed/{timeline,feed,author/{did}}`) were **removed** for exactly this reason (2026-06-05, zero consumers); only the protocol-unique thread view remains.

### Do NOT build protocol-specific DM views

**Wrong:** Separate "Bluesky DMs" view, separate "Nostr DMs" view.

**Right:** Single unified Conversations surface. Bluesky DMs are synced into it by the in-nest worker (`bluesky::dm_worker`, the Bluesky leg of the bridged-conversation family), not by a client calling protocol-specific DM routes — the `/api/v1/bluesky/dm/*` HTTP routes were **removed** (2026-06-05, zero consumers; `direct-messages.md`).

### Do NOT call bridge proxy endpoints from app code

**Wrong:** App calling `/api/v1/bridge/imap/some/path` directly.

**Right:** Use the bridge management API (the `fauna.bridges.*` WS-RPC kinds) for link/unlink/follows/settings. The transparent bridge proxy this anti-pattern warns against (`/api/v1/bridge/{name}/*`, forwarding to a co-located bridge-daemon process) no longer exists at all — it was deleted along with the legacy bridge-daemon stack at the I6 mail-bridge cutover (`api-layers.md` § residue, historical row). Bluesky/Nostr/ActivityPub run as in-process nest modules (§ System Architecture above) and the mail bridge is a peer WS-RPC client (rule 9's *Bridges* bullet), so there is no nest-internal HTTP surface left to proxy to.

### Do NOT build protocol-specific settings pages (beyond link/unlink)

**Wrong:** Dedicated "Bluesky Settings" page with protocol-specific configuration.

**Right:** Bridge settings are managed through the `fauna.bridges.set_settings` WS-RPC kind. The only protocol-specific UI needed is the link dialog (which needs protocol-specific credentials like Bluesky handle or Nostr nsec). Even Bluesky's one setting (`write_through`, the cross-post mode) rides `fauna.bridges.set_settings` — the deprecated `/api/v1/bluesky/settings` HTTP twin was **removed** (2026-06-05).

## Reference: Which APIs do existing apps use?

| Feature | Surface | Protocol-specific? |
|---------|---------|-------------------|
| Main feed | `fauna.feed.*` | No — unified |
| Post creation | `fauna.posts.create` | No — unified |
| Inbox/DMs | `fauna.inbox.{send,fetch,ack}` | No — unified |
| MLS channels (incl. group chat) | `fauna.conversations.{channel,keypackage,welcome}.*` | No — unified |
| Contacts | `fauna.knocks.*`, `fauna.contacts.*` | No — unified |
| Search | `fauna.search.query` | No — unified |
| Bridge management | `fauna.bridges.*` | No — unified |
| Interactions | `fauna.posts.interact` (all apps) | No — unified |
| Notifications | `fauna.notifications.{list,mark_read,count}` | No — unified (Bluesky bridge polling feeds it) |
| Bluesky thread view | `bluesky.feed.thread` | Yes — genuinely protocol-specific |
| Nostr zaps | `nostr.zaps.total` | Yes — genuinely protocol-specific |
| Nostr badges | `nostr.badges.list` | Yes — genuinely protocol-specific |

This table is the litmus test: if a feature uses only unified surfaces, every app should work the same way. Protocol-specific APIs are only for features that are genuinely unique to a protocol (thread structure, Lightning zaps, badges), not for cross-cutting concerns like interactions and notifications.
