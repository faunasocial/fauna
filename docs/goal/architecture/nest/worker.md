# Nest: Worker — target state

Owns: nest-worker
Status: ratified
Authority: the optional sidecar storage-worker architecture — proxy+worker topology, the /internal/worker/ws Ed25519 challenge-response handshake, the ProxyCommand/WorkerMessage wire protocol + PayloadKind semantics, fire-and-forget replication + transparent read fallback, request multiplexing, worker access control (today's CLI surface + the enrollment-style target), and the /internal/router-status capacity contract; shared nest internals (SQLite, blob store, WS-RPC, background tasks) → common.md, flow-picture placement → ../data-flow.md, endpoint classification → ../api-layers.md.

The destination for the optional sidecar storage worker — a separate process that supplements a public nest's local capacity by holding replicated copies of inbox items, posts, blobs, chunks, and manifests. Source: `bins/fauna-nest/src/nest_link/`. Read § Implementation status today first — the live vs. dormant split is load-bearing.

## Goal

An optional sidecar architecture that lets a public nest scale its storage capacity beyond a single machine without changing the client-visible surface. The proxy nest serves all client requests, stores authoritative state locally, and replicates fire-and-forget to one or more workers connected over an authenticated WebSocket. On read miss the proxy transparently falls back to the worker; clients see the same API and the same response shape regardless. Workers are supplemental capacity and warm copies, not a required durability target.

---

## What Is the Worker?

The worker is an optional sidecar process that provides distributed overflow storage for a nest. It:

- Connects to the main nest (the proxy) via WebSocket
- Stores and retrieves payloads on behalf of the proxy

Without a worker authorized, the nest runs in standalone mode and the worker subsystem is inactive.

**Worker authorization — target vs. today (ratified 2026-07-09).** Attaching supplemental storage to a nest is an admin **choice**, so the target authorization surface is **enrollment-style, app-set**: the worker enrolls like a bridge service-user, the admin approves it from the app UI (like a pending bridge), the proxy discovers the worker's keypair from the enrollment row, and the allow-list lives in nest DB state — no CLI knob (`bins/fauna-nest-daemon/src/main.rs` already names enrollment the correct pattern and deliberately authorizes no worker for the co-located MDA). **Today's surface is none: the `--worker-key` / `--worker-allow-ip` CLI flags were removed 2026-10-02 (§ Implementation status today), so the proxy authorizes no worker until the enrollment path lands.**

---

## Deployment Modes

### Standalone (default)

Single `fauna-nest` process handles everything: HTTP, SQLite, WebSocket, blob store. No additional configuration needed.

### Proxy + Worker

The main nest acts as the proxy. A separate worker process provides overflow capacity.

```
                   ┌──────────────────────┐
                   │     fauna-router     │  (load balancer)
                   │  GET /internal/      │
                   │  router-status       │
                   └──────────┬───────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
       ┌────────────┐  ┌────────────┐  ┌────────────┐
       │  fauna-nest │  │  fauna-nest │  │  fauna-nest │  (proxies)
       │  (proxy)    │  │  (proxy)    │  │  (proxy)    │
       └──────┬──────┘  └──────┬──────┘  └──────┬──────┘
              │                │                │
              ▼                ▼                ▼
       ┌────────────┐  ┌────────────┐  ┌────────────┐
       │   worker   │  │   worker   │  │   worker   │
       └────────────┘  └────────────┘  └────────────┘
```

**Proxy**: Serves the full client surface (WS-RPC + the residual HTTP routes), stores data locally in SQLite, replicates to the worker.

**Worker**: Connects via WebSocket to `GET /internal/worker/ws`, receives and serves payloads.

`fauna-router` is optional multi-nest hosting infrastructure (its own binary + artifact-set config), absent from every standard single-box deployment — the proxy+worker pair works without it. Its config is **wiring only** — where its backends are, what to bind, timeouts: the things whatever stood the deployment up already knows. It holds no *policy*, because policy is never artifact-set. A nest's registration posture, its user ceiling and its handle domain are admin choices, made in an app and held as nest state, and the router learns all three from each backend's own report (§ Router Status Endpoint); putting any of them in this file would make it a knob no user or admin can reach. The reserved-handle list is the other non-config kind of value — a hard-coded correctness constant (`fauna_protocol::handle::RESERVED_HANDLES`, shared with the nest's own gates), chosen by nobody.

**Authorization surface on the proxy:** none today — the CLI flags that once carried it were removed (§ Implementation status); see § What Is the Worker? for the enrollment-style target. Loopback is always allowed.

---

## Authentication

Authentication is an Ed25519 challenge-response handshake over WebSocket, completed before any data flows:

```
Proxy                                Worker
  │                                     │
  │  AuthChallenge { challenge: 32-hex }│
  │ ─────────────────────────────────> │
  │                                     │─ sign challenge with Ed25519 private key
  │  AuthResponse { public_key, sig }  │
  │ <─────────────────────────────────  │
  │─ verify public_key == authorized key│
  │─ verify Ed25519 signature           │
  │                                     │
  │  (proxy waits for Hello)            │
  │  Hello { max_storage_bytes,         │
  │          current_usage_bytes,       │
  │          payload_count }            │
  │ <─────────────────────────────────  │
  │                                     │
  │  (connection established)           │
```

1. Proxy sends `AuthChallenge` with a 32-byte random hex challenge.
2. Worker signs the raw challenge bytes with its Ed25519 private key.
3. Worker responds `AuthResponse { public_key: hex, signature: hex }`.
4. Proxy verifies the public key matches the authorized worker key (today none is configurable outside tests) and the signature is valid.
5. Worker sends `Hello` with capacity stats (today placeholder values — `max_storage_bytes: 0`, "not tracking limits in phase 1").
6. Connection is live.

If no worker is authorized on the proxy (today: always, outside tests), the connection is rejected immediately.

---

## Wire Protocol

All messages are JSON over WebSocket using `serde_json` tagged enums (`#[serde(tag = "type")]`).

### Proxy → Worker (`ProxyCommand`)

| Command | Fields | Purpose |
|---------|--------|---------|
| `AuthChallenge` | `challenge: String` | 32-byte hex random challenge |
| `Store` | `request_id: u64, kind: PayloadKind, key: String, payload: String, inbox_row_id: Option<i64>` | Store a payload (payload is hex-encoded) |
| `Fetch` | `request_id: u64, kind: PayloadKind, key: String` | Retrieve a payload |
| `Delete` | `request_id: u64, kind: PayloadKind, key: String` | Remove a previously-`Store`d payload (delete twin of `Store`, added 2026-07-16 for the paired-replica tombstone leg — see § Replication Model) |
| `Ping` | `ts: u64` | App-level heartbeat probe — legacy; the proxy no longer sends it (superseded 2026-08-01 by the standard WS-frame heartbeat, § Health and Heartbeat). Kept in the enum only so an older worker build that still expects it stays wire-compatible during a rolling upgrade |

### Worker → Proxy (`WorkerMessage`)

| Message | Fields | Purpose |
|---------|--------|---------|
| `AuthResponse` | `public_key: String, signature: String` | Challenge response |
| `Hello` | `max_storage_bytes: u64, current_usage_bytes: u64, payload_count: u64` | Capacity report on connect |
| `StoreAck` | `request_id: u64, ok: bool, error: Option<String>` | Storage confirmation |
| `FetchResult` | `request_id: u64, found: bool, payload: Option<String>` | Retrieved payload (hex-encoded) |
| `DeleteAck` | `request_id: u64, ok: bool, error: Option<String>` | Removal confirmation — `ok: true` on an idempotent already-gone delete too, matching `delete_post_core`'s AlreadyGone-is-success contract |
| `Pong` | `ts: u64` | App-level heartbeat reply — legacy; no longer the proxy's liveness signal (see `Ping`), but a worker still answers if it ever receives an app-level `Ping`, so an older proxy build is still answered |

### Payload Types (`PayloadKind`)

Store-status, fetch-status, and delete-status are separate axes — a "working" worker-side handler does not imply any proxy-side producer or consumer exists (see § Implementation status today):

| Kind | Worker-side store | Worker-side fetch | Worker-side delete | Proxy-side today | Key format |
|------|-------------------|-------------------|---------------------|------------------|------------|
| `Inbox` | Working | **Unsupported** (Phase 1 — always not-found) | **Unsupported** (loud `DeleteAck { ok: false }`) | Replicated after `push_inbox`; no read fallback, no delete propagation | actor_id hex (32 bytes) |
| `Post` | Working | Working | Working | Replicated after `put_post`; the sole read-fallback consumer; the sole delete-propagation consumer (`spawn_replicate_delete` after `delete_post_core`) | post_id hex (32 bytes) |
| `Blob` | **Unsupported** (loud `StoreAck { ok: false }`) | **Unsupported** (always not-found) | **Unsupported** (loud `DeleteAck { ok: false }`) | **No proxy-side store/fetch/delete path exists** | blob hash (hex, 32 bytes) |
| `Chunk` | **Unsupported** (loud `StoreAck { ok: false }`) | **Unsupported** (always not-found) | **Unsupported** (loud `DeleteAck { ok: false }`) | **No proxy-side store/fetch/delete path exists** | chunk hash (hex, 32 bytes) |
| `Manifest` | **Unsupported** (loud `StoreAck { ok: false }`) | **Unsupported** (always not-found) | **Unsupported** (loud `DeleteAck { ok: false }`) | **No proxy-side store/fetch/delete path exists** | manifest hash (hex, 32 bytes) |

No worker holds a blob store (2026-09-13). The worker's `Blob`/`Chunk`/`Manifest` store and fetch handlers existed as dormant scaffolding — nothing ever constructed a worker with a store — and the fetch handler read a digest the proxy named with no legal-takedown gate, so wiring it in would have opened a door onto a store the nest's withhold never reaches ([`../../behavior/moderation.md`](../../behavior/moderation.md) § Legal takedown → *The blob-serve door*). They were removed rather than enrolled: a worker-side withhold check would consult a table nothing replicates to the worker. Holding replicated blobs stays target-state (§ Implementation status today); building it means a worker-side store whose fetch arm consults a withhold the worker actually carries.

---

## Replication Model

Replication is fire-and-forget. After storing locally, the proxy spawns an async task to replicate to the worker. If the worker is disconnected, the spawn is a no-op — data remains available locally.

- `spawn_replicate_post()` — called after `put_post()` succeeds
- `spawn_replicate_inbox()` — called after `push_inbox()` succeeds
- `spawn_replicate_delete()` — the delete twin of `spawn_replicate_post()` (added 2026-07-16); called as step 6 of `delete_post_core` — after a delete's own steps, and again on a retry that finds the post already gone when the post-delete witness names the retrying actor — sends `ProxyCommand::Delete { kind: Post, .. }` and clears the `worker_replication` tracking row (`unmark_replicated`) on success so `replication_count` never over-counts a deleted post. A marker left behind (no worker connected, a failed ack) is re-driven by `post_delete_redrive` — at every worker connect and in its hourly pass — so a replica never outlives its original for want of a retry (`feed.md` § Post deletion → Propagation owns the rule). **Post only** — there is no delete twin for `spawn_replicate_inbox()`; inbox items are never proactively removed from the worker replica. Full cross-surface propagation narrative (Nostr/Bluesky/outbox/ActivityPub legs alongside this one): [`../../ui/feed.md`](../../ui/feed.md) § Post deletion → Propagation.

There is no write-ahead log and no retry queue. The worker is treated as supplemental capacity, not a required durability target.

---

## Read Fallback

**Scope today: posts only.** The sole proxy-side fallback call-site is the post read path (with local re-cache); no inbox or blob/chunk/manifest fallback exists proxy-side, and worker-side Inbox fetch is unsupported (Phase 1). Generic fallback across the other kinds is target-state. If a local post fetch fails (data evicted or missing), the proxy transparently tries the worker:

```
Client                    Proxy                    Worker
  │                         │                         │
  │  post read              │                         │
  │ ──────────────────────> │                         │
  │                         │─ local fetch (miss)     │
  │                         │  Fetch { kind, key } ──>│
  │                         │<── FetchResult { found, │
  │                         │      payload }          │
  │<── response ────────────│                         │
```

The fallback is invisible to the client — same API, same response format, just higher latency on cache miss.

---

## Request-Response Dispatch

Concurrent requests to the worker are multiplexed over a single WebSocket connection using a `request_id` counter. The proxy maintains a `HashMap<request_id, oneshot::Sender>` of in-flight requests. When a `StoreAck`, `FetchResult`, or `DeleteAck` arrives, the matching sender is resolved and the caller's `await` unblocks.

Requests time out after 30 seconds if no response arrives.

---

## Health and Heartbeat

`/internal/worker/ws` is one of the "single-task loop" endpoints in
[`../transport-connection.md`](../transport-connection.md) § Connection lifecycle, driven by the shared
`ws::ServerHeartbeat` helper — that section owns the general server-half heartbeat mechanism and
its cadence (a standard WS Ping frame every `ping_interval`, connection closed as dead if no
inbound frame of any kind re-arms the deadline within `liveness_timeout`; production defaults to
30s / 60s). This WS-frame heartbeat replaced the app-level `ProxyCommand::Ping`/`WorkerMessage::Pong`
pair as the actual liveness signal (2026-08-01, `bins/fauna-nest/src/nest_link/proxy.rs`) — those
wire messages stay in the protocol only for rolling-upgrade compatibility (§ Wire Protocol).

| Mechanism | Detail |
|-----------|--------|
| Auto-reconnect | Worker retries connection every 5 seconds on disconnect (`nest_link/client.rs`) |

---

## Router Status Endpoint

`GET /internal/router-status` is used by the `fauna-router` load balancer to poll backend health:

```json
{
  "nest_id": "<hex pubkey>",
  "healthy": true,
  "max_users": 5000,
  "current_users": 142,
  "labels": [],
  "version": "0.1.0",
  "registration_open": true,
  "invite_required": false,
  "handle_domain": "fauna.social"
}
```

**This endpoint is the router's only source of nest policy, by design.** The router is a frontend, not an authority: every field here is a projection of **app-set nest state**, so the admin's app remains the one surface that can change any of it. A router-side copy of any of these values — a `max_users` or a registration posture in the router's own config file — would be a second authority reading a source no user or admin can reach, able only to disagree with the first. That is the same config theatre already ruled out for `fauna.admin.set_serving_port` on a router-fronted nest (`common.md` § Serving ports), and the router carries no such knobs. The report is additive-only: an older router ignores fields it does not know, and a nest predating a field simply omits it.

`max_users` is derived from the **nest's own app-set node storage cap** (the boot-resolved `nest_max_storage_bytes` DB row — not the worker's Hello capacity report, which is a placeholder today) divided by 200 MiB, defaulting to 5000 if no cap is set. The `fauna-router` polls this at its `health_check_interval_secs` (default 30 seconds) and adopts it as the backend's live ceiling; a backend it has not yet polled offers **no** capacity, so a stale or unreadable report can never invent room on a nest. The router itself is optional multi-nest infrastructure (§ Deployment Modes).

`registration_open` / `invite_required` are the live registration posture, projected from the app-set `RegistrationMode` enum by [`RegistrationMode::to_wire_booleans`][`fauna_protocol::node_policy::RegistrationMode`] — the same projection, through the same function, that `nest.info` reports to clients predating the enum (posture owner: [`public-mode.md`](public-mode.md) § Registration Modes). Invite-only is `open` **+** `invite_required`; the incoherent fourth state is unrepresentable.

**The posture is reported, not delegated.** Enforcement stays on the nest (`fauna.account.register`), which is the only thing that decides whether anyone is admitted. The router used the posture for exactly one thing — projecting NodeInfo's `openRegistrations` over its healthy backends — and **never gated a registration on it**: it forwarded, and the nest's own refusal (`fauna.account.registration_closed` / `invite_required` / `free_limit_reached`) was what reached the caller. Both of those router routes are removed (§ `fauna-router`); the router still adopts the posture on each poll, and a future frontend must keep the same report-not-gate rule.

`handle_domain` (added 2026-07-17) is the domain the backend mints handles under — its app-set primary domain (`identity_domain`), else the artifact seed; omitted (`null`) on a domainless box, never the `"localhost"` placeholder. The router adopts the first healthy backend's report (it fed the removed router NodeInfo's `handleDomain` metadata); the value is informational.

---

## Access Control

The worker WebSocket endpoint is not publicly accessible:

- **Endpoint**: `GET /internal/worker/ws`
- **IP check**: Only loopback (`127.0.0.1`, `::1`) is allowed
- Requests from other IPs receive `403 Forbidden`
- **Fails closed on a missing peer address** (fixed 2026-08-22 — a prior gap treated a missing `ConnectInfo` as "skip the check", which was wrong on both production serving paths since `ConnectInfo` is injected on both TLS and plain-HTTP): the handler rejects with `403 Forbidden` and logs a warning rather than admitting the connection, matching the sibling `sidecar_channel::sidecar_ws_upgrade` handler's allowlist

---

## Worker routing (no tunnel layer)

`WorkerClient` reaches the worker over its ordinary URL. Until 2026-08-23 it
could also route through a WireGuard tunnel via `ConnectionRouter` — tunnel-first
when a router was attached and the tunnel healthy, public URL otherwise — but
that whole layer went with the WireGuard stack (user-directed; owner
[`../../behavior/p2p.md`](../../behavior/p2p.md)). Nothing replaces it: a worker
deployment that wants a private path uses ordinary network topology, not a
fauna-managed tunnel.

## Use Cases

| Scenario | How it helps |
|----------|-------------|
| **Capacity overflow** | Proxy running low on local disk; worker absorbs new writes |
| **Multi-proxy shared storage** | Multiple proxy instances replicate to one worker for a shared secondary copy |
| **Warm standby** | Worker holds a replicated copy of critical inbox and post data |

---

## Implementation status today

Re-verified 2026-07-09 (cluster-#4 review). The flow-level split:

**LIVE in the production nest (proxy side):**

- Proxy accept + Ed25519 challenge-response auth on `/internal/worker/ws` (`nest_link/proxy.rs`), IP allow-list, heartbeat, request multiplexing.
- **Inbox + Post fire-and-forget replication** (`spawn_replicate_inbox`/`spawn_replicate_post` after `push_inbox`/`put_post`; no-op when disconnected).
- **Post read-fallback with local re-cache** — the sole fallback consumer (§ Read Fallback).
- **Post delete propagation** (`spawn_replicate_delete`, added 2026-07-16) — the delete twin of `spawn_replicate_post`; called as step 6 of `delete_post_core` (and on a witnessed `AlreadyGone` retry), re-driven at worker connect by `post_delete_redrive::spawn_replica_redrive`, `ProxyCommand::Delete`/`WorkerMessage::DeleteAck` on the wire (§ Wire Protocol), idempotent + fire-and-forget like the create-side replicate. Post only — no delete twin exists for inbox replication. Cross-surface narrative: [`../../ui/feed.md`](../../ui/feed.md) § Post deletion → Propagation.

**DORMANT scaffolding with no production caller:**

- The worker-side `WorkerClient` (`nest_link/client.rs`) — nothing in the repo constructs it outside tests ("`WorkerClient` has no production caller"); the co-located desktop daemon authorizes no worker.
- **No worker-side blob store at all** — the dormant Blob/Chunk/Manifest store+fetch handlers were removed 2026-09-13 (§ Payload Types), because their fetch arm read a proxy-named digest with no legal-takedown gate; **no proxy-side code ever stored or fetched those kinds either**, so no blob overflow capacity exists today on either side.
- **No standalone worker binary exists yet.** (Do not repurpose `bins/fauna-storage` for it without a decision — that placeholder is claimed by the backup track as an S3-compatible store. It stays a workspace member but ships as no release asset: the public release workflow builds and uploads only real binaries, 2026-10-01.)

**Ruled 2026-10-01, removed 2026-10-02 — the worker flags do not ship.** Worker authorization was the `--worker-key`/`--worker-allow-ip` CLI flags, and nothing in production is a worker (the list above), so the flags authorized a peer that does not exist — and once published they would have been the documented way to attach one. They are removed: the proxy's `WorkerState` holds no key outside tests (the tier_3 link tests already inject one through the struct), the worker and sidecar WebSocket gates admit loopback only, and the Windows nest service stops passing its device token as a worker key (the co-located MDA authenticates by enrollment there too, as `fauna-nest-daemon` already does). The authorization surface arrives with the worker itself, as the enrollment-style app-set authorization ratified 2026-07-09 (§ What Is the Worker?) — an in-app setting with no interim flag. Building the enrollment path + the worker binary + blob overflow (its proxy side, and a worker-side store whose fetch arm consults a withhold the worker actually carries) is the worker-productization track — unscheduled.

**`fauna-router` — undeployed, not a release artifact, and unable to onboard an actor (2026-10-02).** The crate is real and builds, and this doc's `/internal/router-status` contract exists for it, but no deployment runs it (it is absent from the nest image *by design* — it is a separate artifact; the image's `fauna-router` UID 1003 belongs to **`fauna-sni-router`**, a different binary — `security.md` § UIDs, `installers/docker.md`). **Its client-facing HTTP surface is removed (ruled 2026-10-01, done 2026-10-02):** the four routes `GET /api/v1/node-info`, `GET /api/v1/handle-available/{handle}`, `POST /api/v1/register` and `GET /api/v1/actor/by-handle/{handle}` were twins of nest routes the nest deleted, and no app called them (every app reaches all four over the anonymous WS-RPC kinds). The route-table sync on `DELETE /api/v1/account` / `PUT /api/v1/profile/handle` named nest routes that no longer exist and is removed with them. The router now serves no `/api/v1/*` route of its own: it forwards every request to the backend its routing table names for the actor. With registration gone nothing writes that table, so **the router cannot onboard an actor until a roaming-nest WS-RPC frontend is designed**, and it is not in the public release asset list (`release-integrity.md`). The crate stays as the starting point for that frontend: its backend pool still polls `/internal/router-status` and adopts each backend's capacity, posture and handle domain, and its capacity-based backend selection is kept for the frontend to call.

Until 2026-07-16 it read three admin choices from its own TOML — `[registration] open` / `invite_required` / `max_free_users` (the *same trio* the nest deleted when the posture became app-set) and a per-backend `[[backends]] max_users`. All four are **deleted**. The registration gates went with them: the router forwarded and the nest refused, which it always did underneath — the router's register path was already mapping the nest's refusal codes, so the config gate could only ever have contradicted it. `max_users` now comes from the poll this doc always said it came from — the code had been discarding the response body and routing on the config value instead, which is the drift this closed. A fifth knob, `[registration] rate_limit_per_hour`, is **also deleted**: nothing read it — a knob that silently did nothing while implying registration was throttled (throttling is the nest's `peer_addr`-keyed anon-surface limit).

**The last two `[registration]` keys were resolved 2026-07-17 and the section is fully retired** (stale configs still parse; every key inert). `handle_domain` was a router-side copy of nest state: the nest now reports its handle domain on the poll (additive `handle_domain` field, § Router Status Endpoint) and the router adopts the first healthy backend's report (it fed the router's since-removed NodeInfo `handleDomain`). `reserved_handles` was a router-side copy of a *constant*: the shared list moved to `fauna_protocol::handle::RESERVED_HANDLES` (subset-pinned against `fauna_core::web::RESERVED_SUBDOMAIN_LABELS`) and the nest's own gates read it (as the router's pre-flights did, until they went with its HTTP surface) — the earlier note here that "there is no handle-availability kind" was recon drift: `fauna.handle.available` has existed on the nest all along (its HTTP twin is deleted; `public-mode.md` § Handle resolution). The same pass closed a nest-side instance of the identical theatre class: the nest's `--reserved-handle` CLI flag is deleted, and the reserved list is the shared constant, enforced unconditionally on every boot (pins: `register_refuses_reserved_handles_on_the_default_config`, `reserved_handle_flag_is_retired`).

**No config debt remains in `fauna-router`:** the `[acme]` section, parsed but never read, was **deleted** 2026-10-01 with the baseline removal of dead shapes, as the sibling `[wireguard]` section was 2026-08-23 with the stack (backends are dialed by `tunnel_ip` directly; the binary serves plain HTTP behind external TLS). A stale file still carrying either parses, inert. The router's own TLS runtime, when built, designs its configuration then.

The worker WebSocket at `GET /internal/worker/ws` is an internal nest-to-worker protocol; Spec Y's WS-RPC migration covers the actor-facing client surface only. The worker `ProxyCommand`/`WorkerMessage` JSON-tagged-enum protocol is out-of-scope for Spec Y and stays as-is for now.
