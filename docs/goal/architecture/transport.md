# Transport — nest ↔ client wire layer — target state

Owns: transport, ws-rpc
Status: ratified — **partitioned by concept 2026-09-06**: the connection itself (§§ Connection lifecycle, Graceful shutdown, Pre-identity) moved to [`transport-connection.md`](transport-connection.md); what stays is everything carried *over* a connection
Authority: owns the WS-RPC surface carried **over** a connection (frames, request lifecycle, idempotency and reconnect-with-resume, cancellation, push and `seq`, backpressure, namespace, forward-compat mechanics, crate map, telemetry, test surface). **NOT owned here either — split 2026-09-06:** the connection itself — its lifecycle and reconnect rules, graceful shutdown, and the pre-identity (anonymous) dial with its connection-level abuse caps → [`transport-connection.md`](transport-connection.md). Defers: HTTP endpoint classification + the residue inventory → api-layers.md; byte-level wire contract → serialization.md; federation channel mechanism → federation.md; compat policy → version-compatibility.md.

Partitioned by concept on 2026-09-06, when this doc reached **228,978 B** — ~10 days from the 262,144 B whole-file read ceiling. Three adjacent sections were one concept and carried 66% of the measured week's growth: § Connection lifecycle, § Graceful shutdown and § Pre-identity (anonymous) connection, i.e. **the connection itself** rather than what travels over it. They went to [`transport-connection.md`](transport-connection.md) as one contiguous range; a routing stub sits at the original location, so every `§ Connection lifecycle` and `§ Pre-identity` citation still resolves in one hop.

## Goal

The authenticated UI surface between nest and clients is a single WebSocket per actor, carrying DAG-CBOR-framed Request / Reply / Push / Cancel envelopes routed by kind string — idempotent retries, drop-cancellation, typed push events, bounded backpressure with an explicit ResyncRequired signal. **All seven apps ride it** (linux, web, windows, macOS, iOS, android — adopted 2026-05 → 2026-06; tui — parity 2026-07-19). The protocol layer (L3) is transport-agnostic so the same envelopes ride the federation channel and the P2P peer channel; the bounded HTTP residue (byte transfer, RFC-mandated protocols, monitoring) is inventoried in `api-layers.md`. Every UI request flows over WS-RPC; new feature surfaces extend WS-RPC rather than growing HTTP. Design history: ratified 2026-05-04 (tracked internally). Implementation anchors: `libs/fauna-protocol/`, `libs/fauna-client/`, `libs/fauna-ws-substrate/`, `bins/fauna-nest/src/{ws.rs,rpc_router.rs,routes.rs}`.

## Layers

L3 (protocol) is transport-agnostic. L2 + L1 are per-substrate adapters
in consumer crates.

| Layer | Concern | Where |
|---|---|---|
| L3 — Protocol | Frame types, codec, dispatcher, kind registry, forward-compat | `libs/fauna-protocol/` |
| L2 — Framing | Per-substrate framing (WS binary frames, future raw-TCP length-prefix) | adapter in consumer crate |
| L1 — Substrate | TCP / TLS / WebSocket / iroh-QUIC | adapter in consumer crate |

Two L2+L1 adapters exist or are planned:

- `libs/fauna-ws-substrate/src/adapter.rs` — WebSocket-over-TLS via `tokio-tungstenite` (the substrate-neutral `TungsteniteAdapter` + keepalive, shared by the bearer client and the nest↔nest federation channel); the client's connect step (`Sec-WebSocket-Protocol: fauna.v1, bearer.<token>` + SPKI pin) is `libs/fauna-client/src/ws_adapter.rs`.
- `libs/fauna-peer-channel/` (the Y.1 peer channel; adapter + `PeerChannel` host landed 2026-06-30, slices 2–4) — length-prefixed CBOR (`[u32 BE len][CBOR]`, ≤1 MiB; the `PeerStreamAdapter`) over the **substrate-agnostic** `ByteStream` the `fauna-transport` seam yields (iroh `PeerConn::open_stream` today; a second, WireGuard-backed impl existed and was proven interchangeable before being **deleted 2026-08-23** — owner [`../behavior/p2p.md`](../behavior/p2p.md)), hosting the L3 `RpcDispatcher` peer-symmetrically via the `PeerChannel` wrapper (`over_stream`/`open` to construct, `request()` to originate, `serve(PeerHandlers)` for a kind-routed handler map, `peer_identity()`/`path()` accessors) — and, since slice 7, the substrate-agnostic **`PeerNode`** lifecycle over it (`start(Arc<dyn PeerTransport>, display_name)` → `listen()`+serve the base `fauna.peer.*` kinds + `dial` outbound; the shared node both native P2P consumers hold, taking `Arc<dyn PeerTransport>` so the crate stays iroh-free); auth is layered above (the caller verifies the per-pair witness against `PeerConn::peer_identity()` — PT-2/PT-3), no bearer. Proven at the time by a duplex round-trip and composition tests over **both** a real iroh loopback connection and a real WireGuard `listen()` connection while both existed — demonstrating the seam was genuinely substitutable rather than shaped around one substrate, ahead of the WireGuard leg's later retirement. (Predates: this was originally scoped into `libs/fauna-peer` over WG-tunneled TCP specifically; the pluggable seam made the channel substrate-agnostic, so it lives in its own crate one layer up.)

**The L3 driver ends two ways, and dropping the dispatcher is one of them
(ratified 2026-08-30).** `RpcDispatcher` only *enqueues* an outbound frame; the
driver future the caller spawns is what writes it to the sink. So the driver
exits when **either** the transport closes **or** every dispatcher handle is
dropped — and in the second case it first drains whatever is already queued.
Both halves are load-bearing and neither is optional:

- *Drain, so a hang-up can carry a reason.* A listener that refuses a handshake
  has just queued an `unauthenticated` reply. Aborting the driver on the next
  line races that frame and, under load, usually wins; the dialer then sees its
  pending oneshot dropped and synthesises `fauna.protocol.disconnected`. A
  refused credential arriving as *"the socket went away"* is not a cosmetic
  difference — it is the one thing a sidecar most needs told precisely
  (§ Future directions → the sidecar credential's lifetime), and the same single
  defect once read as two different bugs on two different sidecars.
- *Exit, so hanging up cannot leak.* Draining alone would leave "stop aborting"
  meaning "detach", and a detached driver owns the socket: an unauthenticated
  peer could hold the task open for as long as it liked. The dispatcher-drop
  exit is what makes the graceful path also the bounded one.

Consequences for consumers: a caller that awaits the driver *before* dropping
its dispatcher (the reconnect supervisor's shape, § Connection lifecycle) is
unaffected — the driver has already exited by the closed transport, which is
also what keeps `PushBroker::bridge_from` terminating naturally. **A nest
listener never aborts its driver**; it drops its dispatcher handles and lets the
driver finish, which is why the listeners spawn their driver without retaining
the handle. A client that drops its dispatcher now closes its own connection
promptly instead of waiting for the server or a GC — on wasm, where `spawn_local`
has no abort handle at all, that is the only way to close one.

The L3 mandate is enforced by `scripts/check-protocol-deps.sh`, run by the
`protocol-integration-test-check` heavy gate ([`merge-gate-catalog.md`](merge-gate-catalog.md)
§ The heavy gate catalog):
`fauna-protocol`'s dependency tree must not contain `tungstenite`,
`tokio-tungstenite`, `axum`, `reqwest`, `hyper`, or `hyper-util`. Adding a
WebSocket-aware concept to the protocol crate is a design break, not a
convenience.

## Wire format

DAG-CBOR (RFC 8949 canonical CBOR plus IPLD restrictions: shortest-form
integers, length-first then bytewise map-key ordering (IPLD dag-cbor
canonical form; see `docs/goal/architecture/serialization.md`), no float
NaN/Inf, definite-length encoding). The canonical CDDL lives in
`libs/fauna-protocol/schemas/`.

Four frame types, discriminated by integer field `0`:

| Frame | Direction | Discriminant | Carries |
|---|---|---|---|
| `Request` | client → server | `0:0` | `correlation_id`, `kind`, `idempotency_key`, payload, optional replay-forbidden + deadline_ms |
| `Reply` | server → client | `0:1` | `correlation_id`, payload (or `RpcError`), explicit `ok: bool` |
| `Push` | server → client | `0:2` | `kind`, payload, per-connection ascending `seq` |
| `Cancel` | client → server | `0:3` | `correlation_id` of an in-flight request |

Integer keys throughout the envelope (~6 bytes saved per frame vs string
keys; matters under steady-state push load). Payloads themselves can use
string keys — that's per-feature CDDL author's choice.

`RpcError` shape (carried in `Reply.payload` when `ok=false`):
`{ code: tstr, message: LocalizedText, ?details: any, * tstr => any }` — the
trailing catch-all is § Schema and forward-compat discipline rule 4, which
binds here like every other client↔nest payload struct: an `RpcError` is not
only decoded at the edge but **relayed**, a federating nest decoding a peer
nest's error and re-emitting it to its own client untouched
(`federation_pool::decode_peer_reply` → `rpc_errors::map_peer_relay_error`).
`details: any` is the opaque per-kind payload, not a substitute for it. (The
map was closed until 2026-09-11 — an artifact of `error.cddl` being the oldest
schema file rather than a decision: `schemas/README.md` has made the open map
"default for all payloads" since the same 2026-05-04 commit that wrote the
closed one, and no record ever claimed an exemption.) Error codes
follow the same namespace policy as kinds — `fauna.<area>.<error>`
upstream, `fauna.protocol.*` reserved for infrastructure errors
(`unknown_kind`, `cancelled`, `replay_too_large`, `timeout`, `internal`,
`unauthenticated` — a kind outside the pre-identity allowlist requested
on an anonymous connection; see § Pre-identity (anonymous) connection).

**In-memory representation (decided 2026-07-30 — wire shape unaffected):**
`RpcError.details` is `Option<Box<Value>>` in Rust. Measured on the rustc-1.99
pin: `fauna_cbor::Value` (= the third-party `ipld_core::ipld::Ipld`, not ours to
shrink) is 96 bytes, which put `RpcError` at 176 — over clippy's 128-byte
`result_large_err` default, reachable from nearly every fallible function in
the tree (`RpcResult` returns it; the per-area error enums wrap it), and
`Result` is as big as its largest variant, so every success path paid for the
rarely-present `details`. Boxing the *field* takes the type to 80 bytes with
zero signature or `?`-site changes anywhere — the alternative, `Box<RpcError>`
at every seam, was rejected as a fleet-wide rewrite of the wire-adjacent layer
for the same bytes. Serde encodes through the `Box`, so the canonical encoding
is byte-identical — pinned by `fauna-protocol`'s `rpc_error_wire_bytes_pin`
(exact-bytes fixture captured on the pre-box representation) — and the type
sits back under the default lint, so no `clippy.toml` threshold carve-out
exists for it.

**`message` is boxed too (2026-09-11 — same reasoning, same wire-invisibility).**
Giving `RpcError` and `LocalizedText` their rule-4 `extra` catch-alls costs 24
bytes each and landed the type on exactly 128. That is **over** the line, not
on it: clippy's `result_large_err` fires at *at least* 128 ("the `Err`-variant
is at least 128 bytes"), so the threshold is inclusive — a fact the guard test
`rpc_error_stays_under_default_result_large_err` had encoded as `<= 128` and
which therefore went green on precisely the size that turned the lint red
(fixed to `< 128` in the same change; measured, not reasoned). Boxing `message`
takes `RpcError` to **64 bytes** — below even the pre-catch-all 80 — so the
lint has genuine headroom rather than sitting on its boundary. Reads
(`err.message.key`) are unchanged via `Deref`; only construction sites spell
the `Box`.

Max frame: the **2 MiB WS message cap**
(`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`, enforced symmetrically
nest-side and client-side — `transport-connection.md` § Abuse posture). There
is no separate protocol-level decode cap below it. No streaming/chunking at
the protocol level — anything larger is HTTP residue.

> **✅ RESOLVED (2026-07-12) — the cap is permanent and universal; bulk payloads
> leave the RPC plane.** The 2026-07-10 contradiction (mail promises
> `max_message_bytes` = 50 MB while this cap binds at 2 MiB — proven by
> `tests/e2e-unified/tests/api/test_ws_message_size_cap.py`, with the mail
> consequence established by
> `test_mail_bridge_mta.py::test_inbound_message_over_the_ws_rpc_cap_is_never_accepted_then_lost`)
> is settled as follows, keeping the security-reviewed value untouched
> for **every** caller class — no global raise, no bridge-class raise, no
> protocol-level chunking:
>
> 1. **The RPC plane never carries a payload above a feature's *inline
>    ceiling*** — a feature-owned constant that leaves the frame headroom for
>    that feature's envelope (its other request fields plus the frame
>    overhead). A payload above the ceiling crosses on the **bulk-byte plane**
>    (the `/api/v1/chunks` + `/api/v1/manifests` HTTP carve-out)
>    and rides the RPC as a **reference**, exactly as WebDAV file
>    chunks already do (`../behavior/webdav-server.md` § Bulk-byte plane) and
>    as the cap's original fix direction anticipated ("raised only for the
>    bulk-sync path"). A **bridge-class cap raise was considered and REJECTED**: it
>    contradicts this section's own "anything larger is HTTP residue", it
>    buffers whole multi-MB internet-origin frames in nest memory (the
>    allocation amplification the cap exists to prevent — the byte plane
>    streams instead), and it leaves first-party clients unable to read mail
>    that external MUAs could — a per-app divergence.
> 2. **Mail is the first consumer, and its reference legs are LIVE (2026-07-12).**
>    Sealed bodies at or below the mail inline ceiling ride inline exactly as
>    today; larger bodies cross as bulk-plane references — the inbound and
>    submission ingest legs and the IMAP `FETCH` leg are built and proven
>    tier_3 at 6 MB, byte-for-byte. (APPEND / import / outbound-due are not
>    yet.) The ceiling constant, perimeter enforcement, and reply codes are
>    owned by `../behavior/mail-message-size.md` § Message size limits; the IMAP legs
>    by `../behavior/imap-server.md`, the import leg by
>    `../behavior/mailbox-migration.md`.
>    **⚠ Crossing the frame was never the whole blocker:** `max_message_bytes`
>    (default 50 MB) is still not deliverable, because a sealed body must also
>    *rest* inside one CARv2 record. That ceiling — and what the perimeter
>    therefore enforces and advertises today — is smtp-server.md's to state;
>    do not infer it from this section.
> 3. In-handler byte guards sized above the cap (e.g. the import batch bound)
>    are defense-in-depth bounds on *referenced* totals, never inline-frame
>    promises.

**Corollary — count-paged serve replies are byte-budgeted (landed 2026-07-13).**
Any reply that assembles a *page of stored records* (a count-limited log read)
must fit this frame, so the page is cut at a byte budget (frame − 64 KiB
headroom) **before** the record that would overflow — close early, **never
skip-and-continue**: every consumer walks these pages by a contiguous cursor,
so a record served out of order is silently lost to it (and where an ack purges
the source, irrecoverably). A head record alone over the budget freezes the
page loudly. To keep that freeze unreachable, `fauna.conversations.channel.send`
**refuses** at ingest any envelope larger than `SERVE_PAGE_BUDGET_BYTES −
RECORD_WIRE_OVERHEAD` — the inbound 2 MiB frame alone was a looser cap than the
serve budget, so a record accepted in the `(≈1.94 MiB, 2 MiB]` window would have
frozen its own channel's drain for every member, silent to the client (an empty
page reads as "drained"). The kind-agnostic cut lives at
`bins/fauna-nest/src/segments/mod.rs::take_page_within_budget`, shared by the
conv serve surfaces (`fauna.conversations.channel.fetch`,
`fauna.federation.channel.fetch`, `fauna.federation.sync.mls_pull`), the
mail relay (`fauna.federation.sync.mail_pull` — whose deployment-level
statement is owned by
`nest/deployment-home-with-public-relay.md` § Relay frame budget), and the
backup custody reads (`fauna.backup.generation.list` +
`fauna.backup.custody.list`, paged 2026-07-29 — their cursor is
**server-minted** (`next_cursor` on the reply; the order tiebreaker is a
rowid no client can see), so their walk terminates on an *absent*
`next_cursor` rather than an empty page; contract owned by
`segment-backup-protocol.md` § Custody grace window (T)). Client-side
counterpart: the shared poll walks page until an **empty** page — a short page
never means "drained" (paging discipline owned by `../behavior/devices.md`
§ Implementation status today, fetch-paging completeness). The outbound due
batch (`fauna.bridges.fetch_outbound_due`) is *not* a cursor walk — rows are
independent — but owes the same frame budget, and (landed 2026-07-18, S9.3)
now gets it: `fetch_outbound_due_handler`'s `OUTBOUND_REPLY_BUDGET_BYTES` cut
closes the reply early on the same shape (remaining due rows ride the next
poll), and a row whose raw body alone would overrun the inline budget stages
to the byte plane as a reference instead of forcing an over-frame reply.
Mechanism owned by `../behavior/mail-message-size.md` § Message size limits (the
staged-envelope rule).

### SignedEnvelope and embed-as-bytes (summary — owner: serialization.md)

Signed payloads ride the wire as a `SignedEnvelope` (36-byte CID +
64-byte Ed25519 signature over the CID bytes, **not** the content bytes)
delivered in the **embed-as-bytes** shape — the raw canonical dag-cbor
bytes the publisher signed travel as a CBOR byte string beside the
envelope, never as a re-encoded nested map:

```
payload = { envelope: { cid: <Cid>, sig: <bytes 64> }, bytes: <canonical dag-cbor> }
```

Receivers verify in two encoder-free steps — `blake3(bytes)` against the
CID digest, then `ed25519_verify` over the CID bytes — and run the
structural decode of the inner content **only after** verification
succeeds (a malformed-but-verified inner payload is a publisher bug, not
a receiver-side security problem). The byte-level contract — canonical
form, CID shape, why sign-over-CID, the third-party verification recipe —
is owned by `serialization.md`; this doc only pins how the shape sits in
a frame.

## Request lifecycle

**Client** (`libs/fauna-client/src/client.rs`):

1. Caller invokes `client.request::<Req, Reply>(kind, payload)`.
2. Crate allocates per-connection ascending `correlation_id` and a fresh 16-byte `idempotency_key` from `getrandom`.
3. **If the WS is down at this point** — the reconnect supervisor cleared the dispatcher on a transient idle-drop, **or has not landed the first connection yet** — the future *waits* (bounded by the request's deadline) for `connection_state == Connected` rather than failing fast. Nothing was sent on the wire yet (`was_in_flight:false`), so this wait is safe for *every* kind, `forbid_replay` included: it cannot double-apply anything. If the deadline elapses still disconnected → `Err(RpcDisconnected { was_in_flight: false })`. This is what keeps a transient reconnect from surfacing a spurious error banner on an otherwise-healthy read. While it waits, the reconnect supervisor retries a refused dial at the initial pace for it instead of on the idle backoff curve ([`transport-connection.md`](transport-connection.md) § Connection lifecycle). The wait ends at once when no connection can come: a client torn down by `disconnect()` answers `RpcDisconnected { was_in_flight: false }`, and one whose supervisor stopped for good answers why ([`transport-connection.md`](transport-connection.md) § Connection lifecycle).

> **Step 3 covers the post-login race, so an app must not hand-roll a retry
> around its first read.** `NestClient::connect` returns once the WS handshake
> has been *initiated*, not completed, so a page that mounts immediately after
> login issues its read with the dispatcher slot still `None` — the same state
> step 3 already parks on, reached by the initial connect rather than a
> reconnect. An app-side "retry the read N times, sleeping between" wrapper
> therefore buys no tolerance it did not already have, and instead multiplies
> the budget: N attempts × the kind's full deadline. ~40 such wrappers had
> accumulated across linux, android, windows and apple before this was
> checked; tui, which always called its machine
> `hydrate()` once, was right. Pinned by `fauna-client`'s
> `request_issued_while_disconnected_waits_for_reconnect` (transport layer) and
> `fauna-client-mail-settings`'s `hydrate_waits_for_socket` (machine layer,
> over the production `rpc_glue` constructor).
>
> A retry *above* the transport is still right for the cases step 3 cannot see:
> a read that is not a single `NestClient` RPC (a local-store load, or a
> composite that touches disk first), or one racing nest-side **readiness** —
> the nest connected and answering, but not yet holding what the read asks for.
> That is a `Rejected`, not a transport fault — the two-class split
> `fauna_protocol::NestSeamError::{Transient, Rejected}` already draws
> (`libs/fauna-protocol/src/requester.rs`) — and only the caller can clear it.
4. Pending entry registered against the dispatcher; Request frame encoded and handed to the dispatcher's **bounded** outbound queue. The queue drains only as fast as the peer accepts bytes, so a peer that stops reading — without closing the socket — fills it; the enqueue is therefore raced against the same budget as the reply wait, and a frame that never got in yields a **never-sent** error rather than parking. Never-sent is the classification that matters to the caller: it may re-issue with a **fresh** `idempotency_key`, where a sent-then-lost request must reuse its own.
5. The returned future awaits reply, timeout, or disconnect:
   - Reply → decode payload → resolve.
   - Timeout (default 30 s, override via `request_with_deadline`) → drop pending entry → `Err(RpcTimeout)`. The deadline bounds the *whole* call: it is one budget, spent across step 3's wait, step 4's enqueue and this reply wait in turn, never re-granted at each stage.
   - Connection drops *after* the frame was sent → the future resolves `Err(RpcDisconnected { was_in_flight: true })`.
6. Drop-cancellation: dropping the future before resolution sends a `Cancel { correlation_id }` frame (see § Cancellation).

The `request*` family looks up `KindRegistry::meta(kind)` for
`forbid_replay` and `default_deadline` so per-kind metadata applies
without per-call boilerplate.

**Steps 4-5 are written once, for every transport.** Encode → dispatch → await
→ classify → decode is `fauna_protocol::RpcDispatcher::request_typed` (and its
pre-encoded half `request_encoded`), which the native `NestClient`, the wasm
`WsRpcClient`, the pre-identity `fauna-anon-client` and the sidecar dial client
all call;
each maps the shared `TypedRequestError` onto its own error vocabulary — the
four clients each their own public error type —
so *which of the five things went wrong* is decided in one place while the
user-facing vocabulary stays per-crate. The one genuinely per-runtime piece is
the deadline backstop, injected by the caller as a plain future: native passes
`tokio::time::sleep`, wasm a `gloo_timers::TimeoutFuture` (there is no
`tokio::time` on `wasm32`). The classification the shared path performs is
against `fauna_protocol::DISCONNECTED_CODE` — the code the dispatcher
*synthesises* on transport close, never one the nest sends. (Nest itself was a
fifth caller, originating to its algorithm sidecar, until that sidecar was
removed 2026-10-01; nest originates to no sidecar today.)

**Step 3 is deliberately not shared.** It chooses *which* dispatcher to send on,
so it sits upstream of any one dispatcher and exists only where a reconnect
supervisor owns the slot; it stays in `fauna-client`, which deducts the time it
spent from the budget it hands the shared path — "the deadline bounds the whole
call" holds either way. The fixed-dispatcher clients (anon, sidecar) have no
step 3 at all: for them a closed connection is always sent-then-dropped.

**Server** (`bins/fauna-nest/src/routes.rs::dispatch_request`, sharing its
idempotency-check + spawn-and-cache mechanics with the federation channel via
`bins/fauna-nest/src/dispatch_core.rs`):

1. Decode frame; reject Reply/Push from clients (protocol violation → close 4400).
2. Idempotency lookup, two tiers: the per-connection LRU (`RpcConnection.idempotency_cache`, bounded 1000 entries, 5-min TTL; replies over 64 KiB are cached as a `too_large` marker, not re-playable bytes; hit → re-send the cached Reply frame verbatim), then — authenticated connections, LRU miss — the **durable** table (`db/rpc_idempotency.rs`, keyed `(actor, idempotency_key)`; hit → the Reply is **rebuilt** with the current correlation_id; see § Idempotency and reconnect-with-resume).
3. Kind lookup in `RpcRouter`. Miss → `RpcError { code: "fauna.protocol.unknown_kind" }`.
4. Effective deadline = `req.deadline_ms ?? meta.default_deadline`.
5. **Acquire a global in-flight-handler permit** (`AppState.handler_semaphore`, `Semaphore::new(MAX_INFLIGHT_HANDLERS = 2048)`, `bins/fauna-nest/src/dispatch_core.rs:52`) **before** spawning the handler — `acquire_owned().await` blocks the caller's read loop as backpressure once the global cap is saturated (one socket pipelining an unbounded flood of requests can no longer spawn unbounded handler tasks), rather than failing the request; the permit moves into the handler task and releases on completion/abort/timeout. It bounds the **handler** only: the step-7 reply task is spawned after that release, deliberately, because this semaphore is global — the per-actor path shares it, so letting one peer's parked replies hold permits would starve every client's dispatch. What bounds the reply half is step 7's budget. Shared by both the per-actor and federation dispatch paths (added from a 2026-06-23 nest firewall-exposure review).
6. Spawn handler task; register `AbortHandle` in `pending_handlers` for Cancel-target lookup.
7. Encode Reply, insert into idempotency cache (or `too_large` marker if > 64 KiB), `try_send` via `RpcConnection.ws_tx` — overflow on a Reply is fatal (close 1011); overflow on a Push triggers ResyncRequired (next section).

**Serving over a dispatcher — the peer-symmetric planes.** Steps 1–7 above describe the per-actor connection, whose Reply carriage is a non-blocking `try_send` onto its own `ws_tx`. The three planes that instead serve *through* an `RpcDispatcher` — the **peer channel** (client↔client over iroh: enrolled siblings, custodians), the **federation channel** (nest↔nest) and the **sidecar channel** (nest↔its co-located iroh relay sidecar) — carry the Reply on the dispatcher's own bounded `out_tx`, where an enqueue *awaits* instead of failing. Both halves of the same obligation are therefore stated on the serving side:

- **Admission is capped per channel.** `fauna_peer_channel::serve_requests` — the shared serve loop of the peer channel and the sidecar dialer — takes a `SERVE_MAX_INFLIGHT` permit before each spawn and holds it across the handler *and* its reply. The cap equals `fauna_protocol::OUTBOUND_CAPACITY`, so admission cannot outrun what the outbound queue can drain, and it is **per channel**, not global, so one stalled peer cannot starve the others. Saturating it parks the loop; the dispatcher's inbound channel then fills and the driver drops further Requests at the door — the valve `RpcDispatcher::new` documents. (The federation and sidecar-listener sides come in through `dispatch_core::spawn_dispatch` and are capped by step 5's global permit instead.)
- **No served Reply may park forever.** Every reply on these planes goes through `fauna_peer_channel::send_reply_bounded`, which races the enqueue against `REPLY_ENQUEUE_BUDGET`. Losing that race is `DispatchError::EnqueueTimeout` — a **never-sent** classification — and it means the peer has stopped draining, which § Backpressure already rules a dead connection: the serve loop stops serving that channel rather than retrying or dropping the Reply. Without this the cap alone would wedge the channel, since a permit held behind an unbudgeted enqueue never returns.

The two bounds are independent and both are load-bearing. Until 2026-09-02 neither existed on these planes: the driver's own read/write split (§ Connection lifecycle) had incidentally supplied the only backpressure — a parked write stopped inbound reads — and removing that starvation, correctly, left serve-side admission with nothing bounding it at all. Reverting the split is not an available fix; it reintroduces the denial of service the split closed.

`RpcRouter` is built once at app startup in `bins/fauna-nest/src/lib.rs`'s
`build_rpc_router()` via per-area `register_<area>_handlers(&mut
RpcRouterBuilder)` calls — 77 areas as of 2026-09-09 (auth, discovery,
account, claim, invite, bridges, conversations, feed, posts, sync,
subscriptions, transport, backup, recovery, tip, region, nostr zap-signer,
…); the list in `lib.rs` is the roster. (The federation table
is a separate `FederationRouter` builder, built and
registered independently in `lib.rs` — see § Future directions — and is
not counted here.)

## Idempotency and reconnect-with-resume

Wire envelope is uniform — every Request carries `idempotency_key`. The
server cache replays the prior Reply on cache hit regardless of the
kind's `forbid_replay` flag (the original op succeeded once; the question
is just "what did it return?"). The forbid is about the *client* not
auto-retrying.

Per-kind metadata (declared in CDDL, exposed via `KindRegistry`) drives
client-side behavior:

- `forbid_replay = true` → client crate refuses `request_auto_retry`; on disconnect the future resolves with `RpcDisconnected { was_in_flight: true }` so the application makes the call.
- `forbid_replay = false` → `request_auto_retry` mints one `idempotency_key`, reuses it across attempts, and on disconnect waits for `connection_state == Connected` then re-issues with the same key. Bounded retry attempts (default 3, exponential backoff).

> **An auto-retry IS deduplicated — by the durable tier, not the LRU (built
> 2026-08-13, W4 (account-data-plane.md § Workstreams) phase 3).** The per-connection cache
> (`ws.rs`, `IdempotencyCache::new()` per connection) still protects only a
> re-send **within one connection's** lifetime — `request_auto_retry` waits
> for the *reconnect* before re-issuing, so the retry always lands on a fresh
> connection whose LRU is empty. What answers it there is the **durable
> idempotency table** (`bins/fauna-nest/src/db/rpc_idempotency.rs`,
> `account-offline-mutation.md` § The offline-mutation contract → *Nest-side durable
> idempotency*): the per-actor sink records each `ok` Reply per
> `(actor, idempotency_key)` and consults the table on an LRU miss, rebuilding
> the Reply with the current correlation_id (recorded frame bytes would carry
> a correlation_id the retrying client never allocated). Three scoping rules,
> each pinned: `ok` replies only (a durably replayed transient error would
> wedge the retrying intent for the whole retention window — an error implies
> no effect, so re-running is the wanted outcome), `Read`-class kinds exempt,
> and no durable tier on the anonymous connection (its placeholder actor would
> alias every anonymous caller). Retention 7 days, swept.
>
> **`forbid_replay = false` remains an assertion that the handler itself is
> naturally idempotent** — the durable tier is defense-in-depth above it, not
> a license to weaken it: a crash between a handler's commit and the durable
> record (or a swept row) still re-runs the handler with the same key. A kind
> whose handler is not idempotent under a repeated call with the same key must
> be `forbid_replay = true`.

The `forbid_replay` gate above governs only the **`was_in_flight:true`**
case — the request *had been sent* and the connection then dropped, so
whether it ran on the server is ambiguous and re-issuing needs the
idempotency-key replay (or an explicit application decision). It is
**distinct** from the **`was_in_flight:false`** case (Request lifecycle
step 3): there nothing was ever sent, so *every* `request*` method —
not just `request_auto_retry` — simply waits for the supervisor to
reconnect (bounded by the deadline) and then sends, regardless of
`forbid_replay`. No replay, no double-apply, no per-call-site opt-in.

`request_with_key(kind, idempotency_key, payload)` is the explicit-replay
path for application code that wants to re-issue after a `was_in_flight:true`
disconnect.

## Cancellation

Explicit fourth frame type. Three cases:

1. **Drop the future client-side** → the `RpcCall` handle's `Drop` impl (`libs/fauna-protocol/src/dispatcher.rs::RpcCall`, armed at construction in `request_raw`) sends `Cancel { correlation_id }` if still armed; server aborts handler via the registered `AbortHandle`; server replies with `RpcError { code: "fauna.protocol.cancelled" }` if the abort took effect before the handler completed. `RpcCall::await_reply()` disarms (`self.armed = false`) only *after* the internal `rx.await` resolves, so a drop while the future is genuinely suspended awaiting the reply (the client's own timeout, `tokio::time::timeout(remaining, call.await_reply())` in `libs/fauna-client/src/client.rs:360`; or any external drop of the request future) still finds `armed == true` and sends the Cancel (fixed 2026-07-22, `dispatcher.rs` — regression-pinned by `dispatcher::tests::drop_while_awaiting_reply_sends_cancel_frame`, which drives a real drop-while-suspended future and asserts the Cancel frame lands on the wire; previously the disarm ran as `await_reply`'s first synchronous statement, before the suspension point, so it always fired too early and no first-party call site's drop/timeout ever actually cancelled the server-side handler).
2. **Cancel races a completed handler** → server silently discards the Cancel; the original Reply was already in flight or sent. No second Reply for a Cancel that has nothing to abort.
3. **Cancel arrives after client's pending entry is gone** → no-op on both sides.

Handlers are expected to be cancel-safe. Tokio's drop semantics handle
most cases; long-running handlers should `tokio::task::yield_now()` or
otherwise check for cancellation.

## Push events and `seq` numbering

Server-initiated, no reply expected, lossy-tolerant. Each push carries a
per-connection ascending `seq` (envelope key 8). Client tracks last-seen
`seq`; gap detection: `expected = last + 1`; observed `seq > expected` ⇒
`expected..seq` were dropped.

Per-connection (not per-actor): a multi-device actor each opens its own
WS, each with its own `seq` space. Reconnect resets `seq` to 0 — the
client crate clears `last_seq` on connect; application observers re-pull
through their snapshot-refresh path.

### Third-party event doors (ratified 2026-09-05; all three doors built 2026-10-05)

Servers and devices integrating as third-party principals ([`third-party.md`](third-party.md)) need "something changed" without polling. Two doors, one event vocabulary, one filter, one cursor:

- **The frame is the plane's scope-tagged nudge, reduced to its scope** — `fauna.sync.changed` carrying `scope` and nothing else (`SyncChangedPayload::scope_nudge`: the required `folder` field empty, no hash address). The scope is `ext:<kind>`, so it names the kind; the key, the epoch and the row itself are what the principal's own walk of that scope's feed learns (`fauna.sync.changes.list`, [`third-party-kinds.md`](third-party-kinds.md) § The record doors). Ruled 2026-10-05 at the build, narrowing the first draft's `(kind, key, epoch, cursor)`: a frame naming a key would tell a listener *which* row moved, which the walk already says to the one party allowed to know, and nothing on the nudge path holds it.
- **The filter** (`fauna_bridge_atproto::fauna_scope::event_reaches`, shared Rust): a frame reaches a session that holds `fauna:events:subscribe` **and** may list the scope — today an `ext:<kind>` scope a `records` qualifier covers, structurally, the record door's own reach check. It is applied to the **live** reach at the moment of delivery — the row's granted scopes ∩ the token's, the account's authority, its external-apps switch (`principal_handlers::resolve_principal`, the read every call makes) — so a switch turned OFF silences the push exactly as it refuses every call. The arm takes no qualifier: what it subscribes to is bounded by the other arms, never widened by it ([`../behavior/authorization-server.md`](../behavior/authorization-server.md) § Scope grammar owns the arm).
- **The cursor is the nest-log `seq`** — the one nest-wide order every feed row is written in, and the coordinate `fauna.sync.changes.list`'s `since` walks. A single scalar therefore covers every scope a principal reaches, survives a restart, and needs no per-principal state on the nest. A poll answers the reachable scopes whose newest row lies past the cursor, in feed order, and the cursor of the page it read — which also walks past rows the filter dropped, revealing no more than the `seq` values the record door already serves.
- **WS-RPC push** (device apps, hosted code): whenever a scope-tagged nudge fires for the account's own scopes, the frame is pushed to every principal session the filter admits (`events_doors::on_scope_changed`, beside the account's own unfiltered nudge). A principal receives no other push.
- **The poll — `fauna.events.poll`, the arm's ceiling kind:** `{cursor?}` → `{frames, cursor}`, answered at once. It is what makes the `events` arm an arm under the closed table's rule that an arm exists only while a ceiling kind names it ([`apps/bridges.md`](apps/bridges.md) § Capability-allowlist enforcement owns the ceiling); a session catches up with it after a reconnect, the push being best-effort.
- **HTTP** (remote servers): `GET /api/v1/events?cursor=&wait=` — the poll as a **long-poll**, admitted like the deposit door (a DPoP-bound access token, `htm` GET, `htu` the door) and dispatched through the same gate: answered as soon as a reachable scope moves past the cursor, else held until a change on the account wakes it or `wait` (at most 25 s, under the common proxy idle timeout) runs out, answering no frames and the same cursor. JSON: `{"frames": [{"scope": …}], "cursor": N}`. **Long-poll only (ruled 2026-10-05):** an SSE stream would be a second shape for the same answer, and the long-poll's cursor already gives it every property the stream would. Residue classification: [`api-layers.md`](api-layers.md) § HTTP residue.
- **The webhook** (remote servers, optional): the principal's document declares `events_uri` ([`third-party.md`](third-party.md) § The manifest owns the member's grammar — `https` on the publisher's own host, validated when the manifest is verified); the nest POSTs a signed, payload-free notification and the server fetches through the poll or the record door. Delivery is best-effort with the cursor as the correctness backstop — the plane's own nudge-plus-walk rule. **The notification is a security event token (ruled 2026-10-05):** a JWT with `typ: secevent+jwt` (RFC 8417) signed by the **issuer's ES256 key** — which key is [`key-material-hierarchy.md`](key-material-hierarchy.md) § Audience: deployment infrastructure → *Issuer signing key* → *What it signs*' ruling; the receiver verifies it against `/oauth/jwks`, the key set it already trusts as this issuer's client — carrying `iss` (the issuer), `aud` (the principal's `client_id`), `sub` (the subject the principal's tokens carry), `iat`, `jti`, and one event, `urn:fauna:event-type:scope-changed`, whose only member is `cursor`: the newest nest-log `seq` among the scopes the principal reaches, so a server already there fetches nothing. No scope, no key, no content. Delivered by push (RFC 8935): one `POST` with `Content-Type: application/secevent+jwt`, any `2xx` accepting it. **The walk reads principal rows, not the session registry** — a remote server with no live session is who this door is for — filtered by the push's own live reach (the row's granted scopes, the account's authority, its external-apps switch); it runs off the put path, **one delivery in flight per principal**, a change landing meanwhile owed exactly one more (a burst costs the publisher's server at most two notifications). The `POST` rides the one SSRF-guarded dial (`oauth_as_client::dial` over `ssrf::guarded_dial`: no redirects, a pinned resolution), is bounded per request, retried with backoff on no answer or a server-side status, final on a client-side one, and dropped after the last attempt — the cursor catches the server up at its next poll. `bins/fauna-nest/src/events_webhook.rs`.

**Events never carry content.** The `PushEvent` enum's exhaustive `invalidates()` match (below) gains no third-party-specific variant — a principal receives the nudge the user's own apps receive, filtered and reduced to its scope; nothing here changes what an app re-reads.

### Which surfaces a push invalidates

*"What do I have to re-read now?"* is answered **once, in shared Rust**:
`fauna_protocol::StaleSurfaces` plus `PushEvent::invalidates()`, living beside
the `PushEvent` enum in `push_events.rs`. Apps do not classify pushes; they
consume the classification.

Two rules make it hold:

- **The match is exhaustive, with no wildcard arm.** `push_events.rs` is the one
  file a variant is ever added to, so a new kind that forgets to declare its
  surfaces is a compile error rather than a surface that silently never
  refreshes on some subset of apps. A kind whose consumer lives elsewhere — MLS
  channel data, per-record mail, every `fauna.bridges.*` kind routed to a bridge
  rather than an app — still gets an arm, saying `StaleSurfaces::NONE` out loud.
- **The seam answers staleness, never side effects.** A desktop toast, the sync
  engine's `pull_set_now` nudge, and the payload-parameterized per-folder device
  fetch stay at each app's call site, because they are per-app or per-payload.
  What is *stale* follows from the wire kind alone, so it belongs with the kind.

The flags are *logical* surfaces, not any app's widget tree: an app folds them
onto whatever it re-reads (tui serves `knocks` and `contacts` from one roster op;
linux serves `contacts` with a contacts + member-reviews pair), and an app that
has not built a surface ignores its flag. `StaleSurfaces::on_reconnect()` is the
full sweep, and the invariant that it **covers every kind's own set** is pinned by
a test rather than by review.

The two recovery paths differ by exactly the surfaces **no push feeds**: a
reconnect stales the feed and the ward's supervision read (`family` —
`fauna.family.status`, the `supervised-indicator`, the `family-tab` gate and the
three client-enforced guardian pillars) as well, because nothing else recovers
either; `ResyncRequired` — which the nest emits when it drops pushes on overflow —
stales everything *but* those two, since a dropped push cannot have staled a
surface no push feeds. The `family` flag exists because
[`../behavior/family-client-enforcement.md`](../behavior/family-client-enforcement.md)
§ Content policy's clause 1 names "cold launch and WS reconnect" as the two
moments the supervision refresh fires, and the two apps that derive their sweep
from this seam had no way to fire the second one until the seam said so (2026-09-13).

> **Implementation status today (2026-08-22): tui and linux derive from the seam;
> the other five apps still hand-derive their own.** This section previously
> described the agreement as prose — each app "matching the linux re-fetch set" —
> and tui's own `Resync` type carried a doc comment asking a human to *"keep the
> two in step when either grows a surface"*. That is not a mechanism, and it had
> already failed three times: linux's reconnect sweep was missing
> `CalendarChanged`'s documented backstop (patched by hand once tui's twin was
> written), and **both** of linux's recovery paths were missing the Media page
> and the Bluesky pending-consent list until the seam landed — the precise
> scenario `ResyncRequired` exists to cover. web, windows, macOS, iOS and android
> keep their hand-written classification for now; adopting the seam is a batched
> trickle-down, tracked in the app queues, and is behavior-preserving for each
> app except where it closes that app's own drift.
>
> **The unblocking prerequisite landed 2026-08-27:
> `StaleSurfaces::for_kind(kind: &str)`, keyed by the wire kind string, plus the
> wasm and UniFFI exports.** Neither web nor the three UniFFI apps see the
> `PushEvent` enum at all — web classifies off the wasm `(kind, payload)` face
> (`PushEvent` is untagged with no `Deserialize`, by design) and
> android/windows/apple off `FfiPushEvent`, which flattens most kinds into
> `Other { kind }` — so a kind-string entry point was the only piece with design
> latitude. `for_kind` cannot share `invalidates()`'s compile-time
> exhaustiveness (a `&str` match has no way to be checked against future
> variants); the property `for_kind(event.kind()) == event.invalidates()` is
> pinned by a test instead (`for_kind_matches_invalidates`), the same trade this
> file already makes for `on_reconnect()`'s coverage property — **adding a
> surface to `invalidates()` means adding the matching arm to `for_kind` too.**
> Exposed as `WsRpcClient.staleSurfacesForPushKind` / `staleSurfacesOnReconnect`
> (wasm, `libs/fauna-wasm/src/rpc.rs`) and `stale_surfaces_for_push_kind` /
> `stale_surfaces_on_reconnect` returning a new `FfiStaleSurfaces` UniFFI record
> (`libs/fauna-ffi/src/nest_client.rs`, gated `push-subscription` like its
> `FfiPushEvent` sibling — a UniFFI record derived directly on the shared type
> would have reached the Go mail-bridge binding tree regardless of gating on the
> functions using it, the `OrphanedStoreDisplay` lesson).
>
> **web adopted the seam 2026-08-27** — its four
> push-driven pages (`notifications`, `contacts` → knocks, `events` →
> `fauna.calendar.changed`, `FoldersSection` → `fauna.sync.changed`) now check
> `staleSurfacesForPushKind(kind).<surface>` instead of matching the kind string
> by hand. **The audit found two real gaps, both closed in the same change:**
> none of the four pages reacted to `fauna.protocol.resync_required` at all
> (the exact-match checks silently ignored it, even though two of the four
> pages' own comments claimed parity with linux's `ResyncRequired` sweep) — the
> classifier-based check fixes this for free, since `for_kind` answers
> `on_reconnect()` minus `feed` for that kind; and the Events page had **no
> reconnect arm whatsoever** (a push dropped across a socket gap stayed
> unrecovered until the user navigated away and back), now added following the
> `contacts`/`notifications` pattern. `conversations.ts`'s three push kinds
> (`ChannelMessage`, `Welcome`, `MailReceived`) are unchanged by design — they
> map to `StaleSurfaces::NONE` (owned by the conversations receive loop, not a
> snapshot surface).
>
> **android adopted the seam 2026-08-27**, and needed
> one more shared-Rust piece: `FfiPushEvent` (the UniFFI-flattened union) has no
> kind string of its own for a modeled variant (only `Other` carries one), so
> `stale_surfaces_for_push_event(event: &FfiPushEvent) -> FfiStaleSurfaces`
> (`libs/fauna-ffi/src/nest_client.rs`) classifies the flattened event directly
> — an **exhaustive match with no wildcard arm**, the same guarantee
> `PushEvent::invalidates()` keeps, so a future `FfiPushEvent` variant is a
> compile error here until classified rather than a client that silently stops
> reacting to it. `ApiClient.kt`'s central push dispatch now derives which of
> its three push ticks (`notificationTick`/`calendarChangedTick`/
> `folderChangedTick`) to bump from this classifier's booleans instead of a
> hand-matched `when` over the enum; the knocks/contacts/account/bluesky
> surfaces (no dedicated push tick on android) fall back to the existing
> `reconnectTick` fan-out. **The audit found the same class of gap web's did:**
> `EventsVM` and the two folders/media VMs (`MediaVM`, `DevicesVM`) had **no
> reconnect arm at all** — `reconnectTick`'s collector set covered
> knocks/contacts/notifications/account (plus the feed) but never events or
> media, so neither an ordinary reconnect nor a `ResyncRequired` sweep ever
> recovered them. Fixed by subscribing all three to `reconnectTick`, which also
> means `ResyncRequired`'s existing `_reconnectTick` trigger now genuinely
> covers every surface as `on_reconnect()` promises. ⚠ **One gap found but NOT
> fixed in this pass, flagged rather than silently left:** `AtprotoVM` has no
> push OR reconnect arm whatsoever (a pre-existing absence, not something this
> adoption introduced — unlike account, which the row also leaves alone for the
> same reason across every app checked so far) — building live refresh there is
> new feature work, not a classifier-adoption fix, and is left for a follow-up.
>
> **macOS + iOS adopted the seam 2026-08-28**, sharing
> one FaunaKit leg. `FaunaClient.startPushObserver`'s central dispatch now
> derives which NotificationCenter signal to post
> (`.faunaNotificationReceived`/`.faunaCalendarChanged`/`.faunaMediaChanged`)
> from `staleSurfacesForPushEvent(event:)`'s booleans instead of a hand-matched
> `switch` over `FfiPushEvent`; `SyncChanged`'s folder-specific consumers (the
> File Provider relay, the sync-agent pull, the per-row device-activity signal)
> still match the raw event directly, since the flattened booleans carry no
> folder name — the classifier decides only whether the cross-set Media signal
> fires. **The audit found the same class of gap android's did:** the Events
> pages (`EventSplitView.swift` macOS, `CalendarListView.swift` iOS) and the
> Folders page's per-set device-activity section (`FoldersContent.swift`) had
> **no reconnect arm at all** — `.faunaReconnected` covered
> knocks/contacts/notifications/account/media (`ContactSplitView`, the
> notifications pages, `MediaExplorerContent`'s own `.onReconnect`) but never
> events or the per-folder activity signal, so neither an ordinary reconnect
> nor a `ResyncRequired` sweep ever recovered them. Fixed by adding
> `.onReconnect` to both Events views and the device-activity section, each
> re-running the same diff-before-swap refresh its push arm already uses —
> which also means `ResyncRequired`'s `.faunaReconnected` fan-out now genuinely
> covers every surface `on_reconnect()` promises. No web-class gap existed:
> `.resyncRequired` was already folded into the same `.faunaReconnected`
> fan-out before this leg. ⚠ **The same pre-existing `AtprotoVM`-class gap
> exists and is left alone for the same reason:** `AtprotoSettingsView` loads
> once on mount with no push or reconnect arm at all — flagged, not fixed, per
> android's precedent. Verified: `swift-test` 502/502; e2e green on both
> targets — `test_push_live_refresh.py`, `test_caldav_external_appears.py`,
> `test_folders.py::test_folder_device_activity_reflects_recorded_changes`.
>
> **windows adopted the seam 2026-09-06**, closing the last remaining leg. `NestRpcClient.DispatchPush`'s central
> dispatch now derives whether to fire `CalendarPushChanged`/`FolderChangedPushed`
> from `StaleSurfacesForPushEvent(ev).{events,media}` instead of firing them
> unconditionally on a raw match; the payload-carrying reactions themselves
> (which calendar, which folder) and the sync-agent `PullFolderNow` nudge stay a
> raw match on the decoded event, since the classifier's booleans carry no
> payload — mirrors apple's split exactly. **The audit found the same class of
> gap android's and apple's did:** `EventsPage`, `MediaPage`, and `FoldersPage`'s
> per-set device-activity section had **no reconnect arm at all** — `Reconnected`
> covered feed/notifications/contacts/the unread badge/family-status (`FeedPage`,
> `NotificationsViewModel`, `ContactsViewModel`, `MainViewModel`, `MainPage`) but
> never events, media, or folder device-activity, so neither an ordinary
> reconnect nor a `ResyncRequired` sweep ever recovered them — a dropped push
> stayed unrecovered until the next poll tick (Events) or the page was left and
> re-entered (Media, Folders). Fixed by subscribing all three to `Reconnected`,
> which also means `ResyncRequired`'s existing fan-out now genuinely covers
> every surface `on_reconnect()` promises. No web-class gap existed: `Reconnected`
> already covers `ResyncRequired` (`NestRpcClient.cs`'s `Notification`/
> `ResyncRequired` arm, pinned by `NestRpcPushDispatchTests.cs`). ⚠ **The same
> pre-existing `AtprotoVM`-class gap exists and is left alone for the same
> reason:** windows has no ATProto settings surface built yet at all, so there is
> nothing to wire — flagged, not new debt. Verified: `NestRpcPushDispatchTests.cs`
> (extended for the classifier gate); e2e green — `test_push_live_refresh.py`,
> `test_caldav_external_appears.py --app windows`,
> `test_folders.py::test_folder_device_activity_reflects_recorded_changes --app windows`.
> All seven apps have now reported for the classifier adoption.

> **⚠ Implementation status today: client-side `seq` gap detection, as described
> in the two paragraphs above, is NOT implemented anywhere — the design was
> never built past the wire field.** Server-side allocation is real (each fresh
> `RpcConnection` starts `seq: AtomicU64::new(0)`, `bins/fauna-nest/src/ws.rs:250`,
> ascending via `next_push_seq`, `ws.rs:264-267` — so "reconnect resets seq to 0"
> holds from the server's perspective, a new connection is a new counter). But no
> client anywhere tracks a `last_seq` or compares `seq > expected`: the dispatcher
> discards the field at the L3 boundary — `Frame::Push(push) => { let event =
> PushEvent::from_push(&push.kind, push.payload); ... }`
> (`libs/fauna-protocol/src/dispatcher.rs:205-206`) calls `PushEvent::from_push`,
> whose signature (`libs/fauna-protocol/src/push_events.rs:834`) takes only
> `(kind, payload)` — `seq` is never passed through, so no downstream consumer
> (native or wasm) can see it, let alone track it. The gap-recovery this
> paragraph describes is, in the actually-shipped design, **entirely
> server-initiated**: an overflowing per-subscriber outbound channel makes the
> *server* emit `ResyncRequired` (§ Backpressure and `ResyncRequired`), and the
> reconnect-driven snapshot re-pull below is the real recovery path. Client-side
> `seq` gap detection remains a documented-but-unbuilt refinement, not a current
> behavior — treat any future implementation as new work, not a bug fix.
>
> **Implementation status today (2026-06-09): the reconnect re-pull is wired
> on linux, windows, web, apple (macOS + iOS), and tui.** The shared `fauna_client::NestClient::subscribe_reconnects()`
> (a `watch<u64>` bumped on every `Connected` *after the first* — derived from the
> `connection_state` watch, so the federation channel is unaffected) gives every
> client a uniform "reconnected" signal that excludes the initial connect.
> Linux consumes it as `WsEvent::Reconnected` and re-fetches the **feed** (which
> has no poll backstop) plus the same snapshots `ResyncRequired` sweeps
> (knocks/contacts/notifications/account); without it a post that arrived while
> the client was disconnected stayed invisible until a manual refresh. The native
> UniFFI apps consume the same watch through the landed FFI binding
> `FfiNestClient::subscribe_reconnects() -> FfiReconnectSubscription` (async
> `next() -> Option<u64>`); **windows** drives it via a pump raising
> `INestRpcClient.Reconnected` on the UI thread, which the live surface VMs
> (feed/notifications/contacts) re-fetch on. **Apple (macOS + iOS, shared
> FaunaKit)** opens one long-lived subscription in `FaunaClient.start()`
> (`startReconnectObserver` — captures `APIClient`, not `self`) and posts
> `.faunaReconnected` on each bump; live surfaces attach the shared `onReconnect`
> SwiftUI modifier and re-pull (feed via `FeedVM.rehydrate()`, plus
> knocks/contacts/notifications/account, matching the linux set). **Web** has no native `NestClient`,
> so its wasm twin (`libs/fauna-rpc-wasm` `run_reconnect_loop`) fires a JS
> `setOnReconnected` callback on every `Connected` after the first (the
> `has_connected` gate mirrors the watch's "after the first" semantics); the SPA
> bumps a shared `reconnectTick` store that the mounted feed page re-fetches on —
> and, since 2026-07-12, the mounted notifications + contacts pages too (they carry
> no poll, so the reconnect re-pull is what recovers a push dropped across the gap).
> **Android** consumes the same signal over the FFI binding: `ApiClient`
> pumps `FfiNestClient.subscribeReconnects()` onto a `reconnectTick` SharedFlow
> (`replay = 0`, so a VM created after a reconnect doesn't replay a stale bump),
> which the live surface VMs collect and re-pull on — `FeedVM.refresh()` (feed list +
> selected feed posts, fallback "local"), `ContactsVM.refresh()` (knocks + contacts),
> `NotificationsVM.loadNotifications()`, `AccountSettingsVM` (quota) — matching the
> linux re-fetch set. Guarded by
> `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_feed_rehydrate`
> (GREEN linux/windows/web/tui; macOS/iOS/android pass once the e2e harness flips the
> xfail — the apple Swift wiring + the android Kotlin wiring both landed, but their UI
> e2e is env-blocked off macOS/the host emulator respectively).

> **tui (2026-07-19) consumes the same watch natively.** Like linux, `fauna-tui`
> is a native Rust app, so it reads `NestClient::subscribe_reconnects()` directly
> (the reconnect pump in `apps/fauna-tui/src/session.rs`) → one
> `DataMessage::Reconnected` per bump → `App::apply_resync(Resync::on_reconnect())`
> (`apps/fauna-tui/src/app.rs`), which re-pulls the feed (`refresh_feeds` +
> `select_feed` — the feed's only recovery, no poll) plus notifications, contacts
> (roster + knocks), and the account/quota cell, matching linux's re-fetch set.
> tui was the last app owing this fan-out (the M9 parity sweep found it as the
> one real behaviour gap); GREEN e2e by `test_nest_flip_resilience.py`
> (`test_nest_flip_resilience` + `test_nest_flip_feed_rehydrate`) `--client tui`,
> whose tui skips were removed when this landed.

> **Android's dedicated `fauna.knock` seam (`subscribe_knocks`, distinct from the
> generic push stream below) landed 2026-07-19 — closing a real
> gap: `subscribeKnocks` had zero app-code call sites before this (only the reconnect
> sweep recovered a knock).** `ApiClient.startKnockPump` pumps
> `FfiNestClient.subscribeKnocks()` onto a `knockTick` SharedFlow (same `replay = 0`
> idiom as `reconnectTick`), which `ContactsVM` collects and re-fetches on — so a
> knock now appears on a **mounted** contacts screen with no navigation, matching
> windows' `NestRpcClient.StartKnockPump`/`KnockReceived` shape (§ below). No e2e
> proof yet (android e2e is host-emulator-gated, unrun, same posture as every
> other android wiring in this doc).

> **Apple (macOS + iOS, shared FaunaKit) closed the same gap 2026-07-19** — apple was
> the last app without a dedicated knock pump. `FaunaClient.startKnockObserver`
> (wired in `start()`, and explicitly in the macOS in-process e2e agent, exactly like
> `startPushObserver`) drives one long-lived loop over `APIClient.subscribeKnocks()`
> (the `FfiNestClient.subscribeKnocks` twin) and posts `.faunaKnockReceived`, which the
> contacts surface re-fetches on via the new `onKnockReceived` modifier — the fourth
> sibling of `onReconnect`/`onPushNotification`/`onCalendarChanged`. Before this, apple
> consumed only the generic `subscribePushes` stream, where `fauna.knock` (a dedicated
> broker kind, not an `FfiPushEvent` variant) arrived as `Other` and was ignored, so a
> live knock reached apple nowhere but the `ResyncRequired` sweep. It is the first
> app to gain an **e2e** for the dedicated-knock-pump design: `test_knock_live_refresh.py`
> (the knock twin of `test_push_live_refresh.py`) fires a real `PushEvent::Knock` +
> stored `knocks` row via the new `POST /api/v1/test/push/knock` `test-hooks` endpoint
> (`push_test_hooks.rs`, driving the production `store_knock` path) and asserts the
> mounted contacts page grows a `knock-card` with no navigation. **GREEN `--client macos`,
> and red-verified** (with `startKnockObserver` disabled the same test fails "knock-card
> count stuck at 0 … 30s after the test-hook stored a real knock row and fired a real
> PushEvent::Knock", so the green is non-vacuous — the pump is what makes it pass). The
> `--client ios` leg (identical shared FaunaKit path, `FaunaiOSLib` host-compiled here)
> is entrusted to the apple e2e harness — a cold iOS run
> needs the full multi-slice `apple-ffi`.
>
> **Windows: `test_knock_live_refresh.py --app windows` GREEN (2026-09-14).** `NestRpcClient.StartKnockPump`
> is started on both login paths, and its red was never the pump: the knock row's WinUI template root
> named itself after a handle `fauna.knocks.list` does not carry, and UI Automation prunes a `Grid`
> with no automation name, so `knock-card` counted 0 while the row rendered. The row now names itself
> with the shared `short_id` of the sender. **tui: GREEN (2026-09-14)** — its 2026-08-28 red did not
> reproduce on current code, and the one load-sensitive step it shared with `test_push_live_refresh.py`
> (a 1 s handshake sleep before a push the nest drops without a live socket) is now the connection
> barrier in both tests. **linux: GREEN (2026-09-15)**, and its red was not the push path either — the
> same shape as windows. linux's test id is the GTK widget name, and the knocks list renamed each row to
> its sender's actor id right after the row builder stamped `knock-card` on it, so the row rendered
> with its children's ids and no `knock-card`. `test_knock_sender_display.py` now asserts every
> `knock-sender` sits in a `knock-card` on every app, so a row that loses its container id fails there
> directly instead of reading as a pump that never fired.

> **Conversations / MLS re-pulls on reconnect too (native 2026-07-17, web
> 2026-07-18) — done on every app.** Until this landed, conversations was the
> **one** live surface that ignored `subscribe_reconnects`, relying on its 30 s
> backstop ticker alone
> (this doc previously called that "self-heals via its own backstop ticker", which
> undersold the cost). A ticker is a *backstop*, not parity: because a push is a
> transient broadcast, anything the nest tried to deliver while the socket was down
> is never re-broadcast and only a **pull** recovers it — so after a flap every other
> surface re-pulled at once while MLS delivery waited up to a **full tick**, on top of
> the reconnect backoff itself. This is the latency an over-load-attributed real-MLS
> receive-path e2e red on the apple apps was tracing (2026-07-16); it presented as
> load-correlated because load makes a socket flap likelier, which hid the cause.
>
> **Native (shared, so every native app at once — priority #2):** the shared
> receive loop (`fauna_conversations::session::start_receive_loop`) gained a
> `ConvPushEvent::Reconnected` wake, fed by `NestConversationsPush` selecting on
> `NestClient::subscribe_reconnects()` alongside its three push kinds
> (`libs/fauna-client-conversations`). The wake runs the **same full sweep** a tick
> does — durable-inbox drain first, then every rail's cursor poll — which is
> idempotent by construction (each rail dedups on its own cursor), so a reconnect
> racing a tick costs one redundant cheap poll, never a duplicate delivery. Guarded
> by `fauna_mls_backend_tests::session_receive_loop_sweeps_every_rail_on_reconnect`,
> which pins the sweep on **two** rails (mail + drain) so a partial sweep fails.
>
> **Web (2026-07-18):** the SPA cannot run that loop (the `tokio::select!` is
> `cfg(not(wasm32))`; the `Rc`-based wasm client is `!Send`), so it hand-mirrors the
> arms in `apps/fauna-web/src/lib/conversations.ts`. It now has a **reconnect arm**
> (`subscribeReconnectSweep`, registered app-wide from `startReceivePoll`) that
> subscribes to the `reconnectTick` store — the same signal feed/notifications/
> contacts use — and, on each reconnect, runs the same full both-rail sweep the
> ticker does through the single-flight `pump()` (so it can never overlap a ticker
> or push pass on the `!Send` MLS engine; each rail's cursor dedups a reconnect that
> races a tick, exactly as native). Its former gate — the wasm client announcing
> `Connected` on synchronous handle creation, which would fire the wake against a
> socket not yet open — was closed first (§ Connection lifecycle: the establish-probe
> means `mark_connected`/`on_reconnected` fire only on a proven connection). Verified
> by svelte-check; the reconnect-timing behaviour's full web e2e regression is
> deferred to a quiet machine (`web-e2e-session-timeout-under-load`).

> **Implementation status today (2026-07-18): every app *receives* pushes on the
> one authenticated socket, and every app — linux + web + android + apple +
> windows + tui — *consumes* the generic stream. No app owes a pump anymore.** The
> shared `RpcDispatcher` decodes each
> `Frame::Push` into a typed `PushEvent` and broadcasts it
> (`dispatcher.rs::push_subscriber`). **Linux** is a native Rust app and reads that
> stream through `fauna_client::NestClient::subscribe_pushes()` directly, feeding its
> central `app.rs` `WsEvent::Push(e) => match e { … }` dispatch.
>
> The **UniFFI apps (windows/apple/android) cannot reach `fauna_client` directly**
> — they see Rust only through `libs/fauna-ffi`, which until 2026-07-17 exported *no*
> generic push subscription: only the dedicated `subscribe_knocks` and the
> `subscribe_reconnects` watch. So `fauna.notification` (and every other kind) was
> **invisible to all three**, and this § previously overstated the position by saying
> "native apps read that stream" without that carve-out. The gate is now
> `FfiNestClient::subscribe_pushes() -> FfiPushSubscription` (async
> `next() -> Option<FfiPushEvent>`, `libs/fauna-ffi/src/nest_client.rs`), the UniFFI
> twin of linux's dispatch. `FfiPushEvent` is deliberately **not** a 1:1 mirror of
> `PushEvent`: it flattens the client-actionable kinds (`Notification`,
> `AccountUpdated`, `CalendarChanged`, `ResyncRequired`) and maps everything else to
> `Other { kind }` carrying the wire kind string — any unmodelled kind, a retired
> one such as the former `fauna.event.rsvp` (§ below) included, arrives as `Other`
> — both because
> several variants wrap
> cross-crate types uniffi-bindgen cannot emit (the `bridge_routing::*` pushes;
> until 2026-08-23, `PeerSignal`'s `fauna_wireguard` `SignalMessage`), and
> because the rest are no-ops
> in linux's dispatch too or are handled *inside* shared Rust (the conversations rail
> owns its own `subscribe_kind`). Decode therefore lives in Rust once, and a kind
> added later surfaces as `Other` rather than vanishing. **Android** consumes it via
> `ApiClient.startPushPump` → a `notificationTick` SharedFlow that `NotificationsVM`
> re-fetches on (the twin of its `reconnectTick` pump); `ResyncRequired` reuses that
> reconnect sweep. ⚠ The android **wiring landed and is unit/compile-verified, but its
> UI e2e is env-blocked off the host emulator** — `test_push_live_refresh.py
> --client android` is written and needs no edit; it has not been *run*. Same posture
> as the reconnect re-pull row above; do not read android's row as e2e-proven.
> **Apple (macOS + iOS, shared FaunaKit)** consumes it identically: `FaunaClient.start()`
> opens one long-lived `startPushObserver` loop over `APIClient.subscribePushes()` and
> `switch`es each `FfiPushEvent` — `Notification` posts `.faunaNotificationReceived`
> (the notifications surface re-pulls via the `onPushNotification` modifier, the twin of
> android's `notificationTick`), `ResyncRequired` reuses the `.faunaReconnected` sweep,
> the rest are ignored. This landed the same commit that **deleted apple's dead second
> socket** (`WebSocketClient` on `/api/v1/ws/{actor}?token=`, the query-auth form the
> nest now 401s — it never opened and its JSON parse could never match the CBOR
> `PushEvent`s, exactly like web's deleted `$lib/ws.ts`); knock/RSVP/MLS delivery never
> depended on it (MLS rides the conversations rail's own `subscribe_kind`). Verified on
> **macOS** by `test_push_live_refresh.py --client macos` and on **iOS** by the same
> test `--client ios` (2026-07-17 — 1 passed in a solo iOS e2e window; the identical
> shared-FaunaKit path). **Windows** consumed the seam 2026-07-18:
> `NestRpcClient.StartPushPump()` mirrors the existing `StartKnockPump()` shape
> over `FfiNestClient.SubscribePushes()` — `Notification`/`ResyncRequired` both
> raise the existing `Reconnected` event (reusing the fan-out
> `NotificationsViewModel` already subscribes to via `RefreshOnReconnect`, the
> same sweep set linux/web/android/apple use for these two kinds); `AccountUpdated`/
> `Other` are no-ops. Windows was the last app owing a *pump*.
>
> **tui (2026-07-19) consumes the generic stream natively**, reading
> `NestClient::subscribe_pushes()` directly (the push pump in `session.rs`) →
> `DataMessage::Push` → `PushEvent::invalidates()` → `App::apply_resync`
> (`apps/fauna-tui/src/app.rs`), the tui
> twin of linux's central `WsEvent::Push` match: `Notification` → the notifications
> page re-fetches, `Knock` → contacts, `AccountUpdated` → account, `ResyncRequired`
> → the snapshot sweep. The conversations kinds (`ChannelMessage`/`Welcome`/
> `MailReceived`) are owned by the shared receive loop's own `subscribe_kind`
> (no-op in the central dispatch, exactly as linux), and every other kind has no
> tui consumer surface yet — a catch-all no-op, the native-Rust equivalent of the
> FFI apps' `Other`. GREEN by `test_push_live_refresh.py` and
> `test_knock_live_refresh.py --app tui` (re-verified 2026-09-14).
>
> **Web** has no native `NestClient`, so its wasm twin exposes the *same* stream to JS:
> `fauna_rpc_wasm`'s `set_on_push_event` → the `#[wasm_bindgen]`
> `WsRpcClient.setOnPushEvent((kind, payload))`, re-subscribed to each new
> dispatcher by the reconnect loop, and fanned out to mounted pages by the SPA's
> `rpc.ts` `onPushEvent` (a single wasm slot, multiplexed). `PushEvent` is
> `#[serde(untagged)]`, so JS receives the variant's payload object **bare**,
> under the same wire kind string a native app matches on — there is no
> per-kind marshalling to drift as kinds are added (locked by
> `push_event_serializes_untagged_as_its_bare_payload`). This **replaced**
> `apps/fauna-web/src/lib/ws.ts`, a second raw WebSocket that still spoke the
> `?token=` query-auth form the nest has removed (`routes.rs::ws_handler` answers
> 401 without the `fauna.v1, bearer.<token>` subprotocol) — so it never delivered
> a push and merely reconnected every 3 s for as long as the Events page was open.
>
> **Web consumes the seam on its live surfaces as of 2026-07-12.** The SPA cannot
> run the shared receive loop — `ConversationsSession::start_receive_loop`'s
> `tokio::select!` is `cfg(not(wasm32))`, because the `Rc`-based wasm RPC client is
> `!Send` — so `apps/fauna-web/src/lib/conversations.ts` hand-mirrors that loop's
> *arms*: a 30 s backstop ticker (the same cadence as native's
> `DEFAULT_CONV_POLL_SECS`, which was **not** slowed when pushes landed) plus a push
> arm on `fauna.conversations.{channel.message,welcome.received}` (wakes the MLS DM
> rail) and `fauna.mail.received` (wakes the SMTP rail). Both arms funnel through one
> **serialized, coalescing pump** — the guarantee the native `select!` gets for free
> by living in a single task. Web needs it explicitly: its ticker and its WS callback
> are independent JS callers against one `RefCell`-interior wasm manager and one
> single-threaded MLS engine, so an overlap could double-ingest a Welcome
> (double-spending the MLS init key — the same hazard linux avoids by no-op'ing
> `PushEvent::Welcome` in its central dispatch) or panic on a re-entrant
> `borrow_mut()`. The two page-level surfaces follow linux's central-dispatch arms:
> `fauna.notification` → the notifications page re-fetches; `fauna.knock` → the
> contacts page re-fetches; both also re-pull on `reconnectTick`, since a push fired
> while the socket was down is never replayed.
>
> **The `fauna.event.rsvp` gap is CLOSED by design (ratified 2026-07-17): the kind
> was DEAD-REGISTERED until it left the wire 2026-09-24, and `fauna.calendar.changed`
> does its job.** The full verdict, in four parts:
>
> 1. **`fauna.event.rsvp` never gets a producer — its payload is unfillable.** The
>    RSVP write path is **client-side**: the client decrypts, mutates `PARTSTAT` +
>    the `interested` sidecar (`libs/fauna-client-caldav/src/mutate.rs`
>    `apply_rsvp`), re-seals, and PUTs `fauna.bridges.put_event_ciphertext`. At that
>    handler the nest holds only the **writer's `actor_id`, `calendar_id`,
>    `uid_hash` (blake3 of the UID), sizes and timestamp** — so
>    `EventRsvpPayload{event_id, actor_id, status}` cannot be built (`status` **is**
>    the sealed `PARTSTAT`; `event_id` is a plaintext UID the nest deliberately does
>    not hold), and routing to "the organizer"/"the attendees" is impossible (the
>    roster is sealed; `notify_push`'s only axis is `connections_for(actor_id)`).
>    The payload shape is a vestige of the plaintext `fauna.events.*` surface
>    retired 2026-06-14.
> 2. **The kind left the wire 2026-09-24 (the compat-remnant sweep).** Under the
>    one-exception guard it was NOT removable: the first shipped production image
>    (dispatched 2026-06-13 for the alpha start) carried the legacy
>    producer (`event_social_routes.rs` emitted it to the event author + co-hosts),
>    making it a **no-longer-has-a-caller** kind — so from 2026-07-17 it stayed
>    registered-but-dead (enum variant + kind string + encode arm, annotated DEAD
>    in `push_events.rs`). The 2026-09-23 user-population ruling overrode that
>    ([`version-compatibility.md`](version-compatibility.md) § Dim 2, the fourth
>    ratified exception — the population judgment is the user's to make, and was
>    made): variant, kind string, encode arm, CDDL type and the clients'
>    ignore-arms are gone, and a pre-retirement nest's push lands as `Unknown`,
>    ignored like every other unmodelled kind (the ids it would carry match
>    nothing current clients hold anyway).
> 3. **Privacy verdict: a client attaches NO new plaintext to a sealed PUT.** The
>    replacement needs none: `put_event_ciphertext` / `delete_event` /
>    `provision_calendar` already carry `actor_id` + `calendar_id` in plaintext, so
>    `fauna.calendar.changed` adds **zero** new wire plaintext. The nest's
>    event-level identifier stays `uid_hash`-only; plaintext event ids and statuses
>    stay retired. (`uid_hash` itself would be an additive, no-new-leak payload
>    field if a consumer ever needs event-level scoping — deliberately omitted
>    from v1.)
> 4. **Recipient rule: own-device fanout only.** `fauna.calendar.changed`
>    (`CalendarChangedPayload{actor_id, calendar_id}`) fires at the **calendar
>    owner's** connections after every durable calendar write — put
>    Created/Updated, delete Deleted, provision Created/metadata-Updated, never the
>    no-DB-change outcomes (Idempotent/AlreadyExists/NotFound/PreconditionFailed/
>    CalendarMissing) — whether the writer was the owner's own device or an
>    external MUA through the MDA (which writes under the served actor's id, so an
>    external CalDAV write reaches the owner's Fauna apps live). **Cross-actor
>    RSVP propagation stays on the iMIP `REPLY` email path**
>    ([`caldav-server.md`](../behavior/caldav-server.md) § The one operation with a
>    cost): the reply's inbox landing already fires `fauna.mail.received` at the
>    organizer, whose client merge + re-PUT then fires `fauna.calendar.changed` at
>    the organizer's own devices — the organizer's lag shrinks with no new
>    cross-actor machinery and no standing capability. The push is a best-effort
>    nudge; the quick-appearance poll (where built) and the reconnect re-pull
>    remain the correctness backstop.
>
> Producer: `bridge_caldav_handlers.rs::notify_calendar_changed` (the caldav twin
> of `segments::notify_mail_received`), pinned by the emit/no-emit tests in that
> file's test module. Consumers: linux `app.rs` re-lists calendars; web
> `events/+page.svelte` re-lists + re-queries (web has no quick-appearance poll,
> so this push is its while-on-page mechanism); android `ApiClient.startPushPump`
> → `calendarChangedTick` → `EventsVM`; **windows consumed 2026-07-18** — a new
> `INestRpcClient.CalendarPushChanged` event (distinct from the `FfiPushEvent`
> variant name to avoid a naming collision), subscribed once by `EventsPage`
> (`NavigationCacheMode.Required`, so a page-lifetime subscription is correct —
> no per-navigation leak) and dispatched straight into the SAME `Poll_Tick` path
> the existing ~10s quick-appearance poll already drives (`RefreshIfChangedAsync`
> + the `AnyChanged` rebuild), so the push is purely a latency win over the
> poll, not a second code path; apple still rides `FfiPushEvent::CalendarChanged`
> once its pump/arm lands.
>
> **`fauna.addressbook.changed` — the carddav twin (RATIFIED 2026-08-05;
> PRODUCER + the index-side consumer BUILT 2026-08-06; the address-book page
> consumer BUILT on tui 2026-09-21, on linux, web and android 2026-09-24, on
> macos + ios 2026-09-25 and on windows 2026-09-26 — all seven apps).** Ratified as part of the
> content-index third-ingest-class ruling (`../behavior/content-index-ingest.md`
> § Ingest triggers, v1 owns the index-side consumer — the contacts reconcile
> walk needs a mid-session change signal; the address-book pages get the same
> live-refresh nudge their calendar twins already consume). The calendar
> verdict above transfers point for point: **own-device fanout** at the **book
> owner's** connections after a durable card or book write — put
> Created/Updated, delete Deleted, book provision/metadata-Updated, never the
> no-DB-change outcomes (the carddav DB already leaves ctag/modseq untouched
> on a byte-identical PUT, so idempotent retries emit nothing) — whether the
> writer was the owner's own device or an external CardDAV MUA through the
> MDA. Payload `AddressBookChangedPayload{actor_id, addressbook_id}` — both
> already plaintext at the write handlers, so **zero new wire plaintext**;
> card-level identity stays `uid_hash`-only and is deliberately omitted from
> v1, exactly as the calendar payload omits its event scoping. Best-effort
> nudge under this section's doctrine: the index builder's attach-time
> reconcile walk and the pages' reconnect re-pulls and nav-in re-reads are the
> correctness backstop. **Producer built 2026-08-06:**
> `bridge_carddav_handlers.rs::notify_addressbook_changed`, called from the six
> durable arms — provision Created, provision metadata-Updated (inside the
> `hms_before != hms_after` gate, so a byte-identical PROPPATCH stays silent),
> put Created, put Updated, delete-card Deleted, delete-addressbook Deleted —
> and from none of the no-DB-change arms, the same rule the placement-record
> appends beside each call site already follow. Pinned by five emit/no-emit
> tests in that file's test module, mirroring the calendar block. On the UniFFI
> boundary it is the typed `FfiPushEvent::AddressBookChanged{actor_id,
> addressbook_id}` and folds to `FfiStaleSurfaces::address_book` — promoted off
> `Other` together with android's page consumer, the first UniFFI reader (both
> behind the `push-subscription` feature the Go build excludes, so no Go binding
> regen). **The index-side consumer
> is built** (`IndexBuilderLauncher::corpus_changed(NestCorpus::AddressBook)`,
> `libs/fauna-conversations/src/session.rs`, landed the same day as the
> producer — the content-index contacts reconcile walk this event
> exists to nudge). **The page consumer is built on tui (2026-09-21):** the kind
> folds to `StaleSurfaces::address_book` — in `invalidates()` and `for_kind`
> alike, so the wasm and UniFFI kind-string classifiers answer it too — and
> tui's `apply_resync` re-reads the books and the open book's cards, page-gated
> because a contacts app's first sync is one push per card (the flag's own doc).
> Witness: `test_addressbook.py::test_a_card_added_from_another_app_appears_while_on_the_address_book`.
> **Linux, web and android followed (2026-09-24):** linux in `apply_stale`
> (gated on the Address Book view's GTK map state), web off
> `staleSurfacesForPushKind(kind).address_book` while its Address Book segment
> shows, android off `ApiClient.addressBookChangedTick` while `ContactsVM`'s
> segment is the Address Book. Every one keeps the open book open across the
> re-list and drops a card reply for a book the user has since left.
> **macos + ios followed (2026-09-25):** `FaunaClient.startPushObserver` posts
> `.faunaAddressBookChanged` off `stale.addressBook`; the shared FaunaKit
> `AddressBookView` (mounted only while the Address Book segment shows)
> observes it via `onAddressBookChanged` into `AddressBookVM.refreshFromPush`,
> which keeps the open book open and drops a card reply for a book the user
> has since left — the same contract, since `AddressBookVM` is a per-view
> `@State` instance rather than one the client can reach directly.
> **Windows followed (2026-09-26), the last app:** `NestRpcClient.DispatchPush`
> raises a distinct `INestRpcClient.AddressBookPushChanged` event (the
> `CalendarPushChanged` naming precedent above) gated on the shared classifier's
> `stale.addressBook`; `ContactsPage` subscribes while it is the page on screen
> and re-reads only while its Address Book segment shows — off the push and off
> `Reconnected`, the reconnect re-pull the paragraph above names as the backstop.
> The open-book rule
> lives in the unit-tested `AddressBookOpenBook` (`FaunaApp.Core`): a re-list
> keeps the open book open, and a card reply for a book the user has since left
> is dropped — the same contract as every other app.
>
> Test surface: `tests/e2e-unified/tests/test_push_live_refresh.py` fires a real
> `PushEvent::Notification` (the `test-hooks` endpoint) and asserts a **mounted**
> page grows the row with no navigation — the notifications surface has no poll on
> any client, so only the push can satisfy it. The conversations rail is deliberately
> **not** that probe: its backstop drops to 2 s under the e2e agent, which would mask
> a dead push arm.
>
> **The conversations rail now has its OWN probe (web, built 2026-08-15).** Both
> prerequisites this paragraph used to name as missing exist:
> `tests/e2e-unified/tests/test_conv_rail_push_wakes_web.py` mutes the web backstop
> ticker through `WebBridgeDriver.set_conv_poll_secs` → `window.__fauna_setConvPollSecs`
> → `setConvPollSecs` (the web twin of native's `FAUNA_CONV_POLL_SECS`, which the SPA
> cannot take as an env var), and takes its second real MLS engine from the existing
> `real_faunamls_linux_sender` fixture. With the ticker muted the push arm is the rail's
> only trigger, so the test fails if that arm dies. Two properties make the mute a
> *causal* barrier rather than a wall-clock bet (§ conventions, point 14): the setter
> **re-arms the sleep already in flight**, so on return the next ticker sweep is
> provably a full new interval away; and `conv_receive_cycles` — which counts ticker,
> poke and reconnect sweeps but *not* single-rail push wakes — is asserted **frozen**
> across the delivery window, so a sweep from any other arm fails the run instead of
> silently earning it. The knob's wiring (both its ends, the re-arm, and its single
> automation-only importer) is pinned at tier_1 in `test_web_conv_poll_knob.py`,
> because an inert knob would turn that test green for the wrong reason.
>
> The rail's **opposite** arm is proven natively by
> `test_fauna_mls_two_client_inbox_drain.py`, which suppresses the push arm
> (`FAUNA_E2E_SUPPRESS_CONV_PUSH`) and leaves the durable drain. Each arm is now
> covered alone rather than jointly.

> **`fauna.sync.changed` — the folder download nudge (built same-nest 2026-07-23).**
> The `fauna.calendar.changed` shape applied to shared/synced folders: a best-effort
> nudge that shrinks *download* latency for shared-set collaboration, with the engine's
> periodic reconcile as the correctness backstop (`file-sync.md` § Remote-change nudge
> owns the behaviour). Producer: `bins/fauna-nest/src/sync_handlers.rs::notify_sync_changed`,
> fired from `record_change_core` after every durable `sync_changes` insert (so it covers
> both the same-nest `fauna.sync.changes.record` handler and the federation relay into a
> set homed here) at every **same-nest** participant — `list_channel_actors(channel)` for a
> shared set (owner + roster members), the owner's own devices for an unshared one. Recipient
> rule is thus own-nest fan-out like calendar's, but across the *set's* roster rather than a
> single actor; a cross-nest member has no connection here, so same-nest falls out by
> construction and no federation kind grows. Payload carries only the set **name** (the
> identifier the whole sync IPC/engine stack keys on — no id→name resolution on the latency
> path); it adds no new wire plaintext (`folders.changes.record` already carries the set in
> plaintext). Consumer: a receiving resident engine schedules an immediate off-cadence pull —
> `always_resident::run_watch_loop`'s wake arm, driven on **linux**/the per-user agent by
> `app.rs` `PushEvent::SyncChanged` → the `PullFolderNow` IPC → the agent's per-engine
> `wake_senders` channel. Fired unconditionally after a durable insert (best-effort — a rare
> byte-identical replay's redundant nudge is a harmless no-op pull; a missed push costs only
> latency). Other apps absorb the new variant with no arm (`FfiPushEvent::Other`, web's
> untagged passthrough, tui's catch-all); their real reaction arms + the Windows on-demand
> host's are entrusted. **Same-nest only**: a cross-nest nudge is a named
> candidate needing its own federation design (`file-sync.md` § Remote-change nudge).

The typed push payloads (defined in `libs/fauna-protocol/src/push_events.rs`;
the enum + `PushEvent::kind()` are the source of truth — regenerate this table
from them when editing; 36 kinds as of 2026-10-01):

| Wire kind | Payload type |
|---|---|
| `fauna.knock` | `KnockPayload` |
| `fauna.account.update` | `AccountUpdatedPayload` |
| `fauna.notification` | `NotificationPayload` |
| `fauna.peer.wake` | `PeerWakePayload` |
| `fauna.calendar.changed` | `CalendarChangedPayload` |
| `fauna.addressbook.changed` | `AddressBookChangedPayload` (own-device fanout, carddav twin of `calendar.changed` — `../behavior/content-index-ingest.md` § Ingest triggers) |
| `fauna.conversations.channel.message` | `ChannelMessagePayload` |
| `fauna.conversations.welcome.received` | `WelcomePayload` (MLS Welcome bytes inline; no follow-up fetch) |
| `fauna.inbox.item` | `InboxItemPayload` |
| `fauna.protocol.resync_required` | `ResyncRequiredPayload { dropped_count }` |
| `fauna.segments.changed` | `SegmentsChangedPayload` |
| `fauna.mail.received` | `MailReceivedPayload` |
| `fauna.mail.flags_changed` | `MailFlagsChangedPayload` (an `INBOX` flag write — a wake for `fauna.email.inbox.flag_changes`, `mail-app-surface.md` § Read state) |
| `fauna.sync.changed` | `SyncChangedPayload { folder }` (same-nest folder download nudge — `file-sync.md` § Remote-change nudge) |
| `fauna.bridges.push.mailbox_state` | `bridge_routing::BridgeMailboxStatePush` (IDLE/NOTIFY — `imap-server.md`) |
| `fauna.bridges.config_changed` | `bridge_routing::BridgeConfigChangedPush` (`mail-bridge-lifecycle.md` § Running) |
| `fauna.bridges.atproto.sessions_changed` | `atproto_pds::BridgeAtprotoSessionsChangedPush` (`atproto-pds-full.md` § WS-RPC kind surface) |
| `fauna.bridges.atproto.projection_ready` | `atproto_pds::BridgeAtprotoProjectionReadyPush` (`atproto-pds-bridge.md` § Where logic lives) |
| `fauna.bridges.atproto.issuer_key_rotated` | `atproto_pds::BridgeAtprotoIssuerKeyRotatedPush` (the nest issuer's key-set nudge — `authorization-server.md` § The issuer → *The teaching is one WS-RPC feed carrying both halves*) |
| `fauna.atproto.consent_requested` | `atproto_pds::AtprotoConsentRequestedPush` (own-device fanout — `atproto-pds-full.md` § WS-RPC kind surface) |
| `fauna.bridges.atproto.permission_set_requested` | `atproto_pds::BridgeAtprotoPermissionSetRequestedPush` — the nest→bridge half of the permission-set request call; the bridge answers over the BRIDGE-class kind `fauna.bridges.atproto.deliver_permission_set` (`atproto-oauth-provider.md` § Implementation status today, the 2026-09-25 bullet) |
| `fauna.bridges.outbound_ready` | `bridge_routing::BridgeOutboundReadyPush` (`smtp-server.md` § Outbound delivery) |
| `fauna.bridges.rescore_ready` | `bridge_routing::BridgeRescoreReadyPush` (`content-scoring.md` § Timing) |
| `fauna.bridges.spam_baseline_publish` | `bridge_routing::BridgeSpamBaselinePublishPush` (`mail-spam.md` § Encrypted-mode interaction) |
| `fauna.bridges.push.spam_model_updated` | `bridge_routing::BridgeSpamModelUpdatedPush` (`mail-spam.md`) |
| `fauna.bridges.push.spam_model_reset` | `bridge_routing::BridgeSpamModelResetPush` (`mail-spam.md`) |
| `fauna.delegation.lease_changed` | `LeaseChangedPayload` (`participants.md` § Coordination primitive) |
| `fauna.bridges.push.import_progress` | `bridge_routing::BridgeImportProgressPush` (`mailbox-migration.md` § Progress lives nest-side) |
| `fauna.bridges.push.import_error` | `bridge_routing::BridgeImportErrorPush` (`mailbox-migration.md`) |
| `fauna.bridges.push.import_complete` | `bridge_routing::BridgeImportCompletePush` (`mailbox-migration.md`) |
| `fauna.bridges.push.export_progress` | `bridge_routing::BridgeExportProgressPush` (`mail-export.md` § Session row model) |
| `fauna.bridges.push.export_error` | `bridge_routing::BridgeExportErrorPush` (`mail-export.md`) |
| `fauna.bridges.push.export_complete` | `bridge_routing::BridgeExportCompletePush` (`mail-export.md`) |
| `fauna.push.notification` | `PushNotificationPayload { title, body, url }` (a `ws-device` push row's delivery over the device's own connection — `apps/common.md` § Push Notifications → *Transports*) |
| `fauna.sync.chunk.wanted` | `SyncChunkWantedPayload { request_id, folder, store_key }` (the relay's ask to the one connection that announced the folder; answered on the bulk rail — `file-sync.md` § Relay serving) |

Application code on the nest side calls `state.ws.notify_push(actor,
PushEvent::...)`; the WS layer encodes, allocates `seq`, and emits.

## Backpressure and `ResyncRequired`

Per-subscriber outbound channel is `mpsc::channel(256)` (bounded). On
`try_send` failure:

- **Reply or Cancel** → close the connection with `WS_CLOSE_INTERNAL` (1011). Replies cannot be dropped; if the client isn't draining, the connection is dead.
- **Push** → increment `dropped_pushes`; set `needs_resync = true`.

The Reply rule is the per-actor connection's, whose `try_send` fails fast. The peer-symmetric planes reach the same verdict through a budget rather than a `try_send`, because their enqueue awaits — how, and what stopping serving means there, is § Request lifecycle's (*Serving over a dispatcher*).

On the next successful emit (or after a 5-second resync-coalesce timer
if no other emit happens), if `needs_resync` is true, the layer sends
`PushEvent::ResyncRequired { dropped_count }` carried by a normal Push
frame with its own ascending `seq`. Flag and counter reset after a
successful send.

Client behavior: the typed `PushEvent::ResyncRequired` surfaces on the
broadcast channel; application observers (bridges' observer,
conversations' equivalent, etc.) invalidate their snapshot cache and
re-pull. Treated as "missed all events since last seq." Client-side
outbound queue is also bounded (default 64); request calls block on
queue drain.

## Connection lifecycle

**§ Connection lifecycle, § Graceful shutdown and § Pre-identity (anonymous) connection → [`transport-connection.md`](transport-connection.md)** (2026-09-06 concept partition) — the connection itself: how one is established and re-established, how it is closed cleanly, and the anonymous Layer-0 variant with its own caps and throttles. What stays here is everything carried *over* a connection.

## Graceful shutdown

**§ Graceful shutdown → [`transport-connection.md`](transport-connection.md)** (2026-09-06 concept partition).

## Pre-identity (anonymous) connection

**§ Pre-identity (anonymous) connection → [`transport-connection.md`](transport-connection.md)** (2026-09-06 concept partition).

## Schema and forward-compat discipline

Five rules enforced in code review and by the conformance test suite:

1. **Maps over arrays** for record-shaped data. Arrays force positional encoding — adding a field is a breaking change.
2. **Append-only fields with optional modifiers.** New fields are optional (`?` in CDDL); renaming/repurposing a key is a breaking change. Removal requires a deprecation cycle.
3. **An unknown arm in every tagged union a different release can decode — or a written reason it has none.** The kind-routed envelopes try known variants in order and fall through to `Unknown { kind, payload: fauna_cbor::Value }`; every other enum takes one of the four answers of § *Rule 3 in full* below, scoped like rule 4 by who decodes.
4. **Map values decoded with key preservation.** Unknown keys captured into a side-channel `extra: BTreeMap<String, fauna_cbor::Value>` via `#[serde(flatten, default)]` and re-emitted on encode — **universal across the client↔nest payload structs** (2026-06-15; the sweep that closed `version-compatibility.md` § 5 item 6's wire half). Per-payload opt-out via the `strict` annotation `#[serde(deny_unknown_fields)]`, used by the version-locked, in-image mail-bridge (MTA/MDA) data-plane (`fauna-protocol::bridge_routing`): both ends ship in one Docker image, so there is no cross-version skew to tolerate and strictness is the right dev-time-mismatch guard. **The opt-out is scoped by WHO DECODES, not by which module a struct lives in.** `bridge_routing` is a mixed module: the kinds an *app* calls — the `admin-mail` read/write twins (`get_mail_config`, the five `put_<substruct>_policy` writes, the alias twin, `rotate_srs_secret`) and every struct nested inside their payloads — are **client↔nest wire** and carry the catch-all like any other, because I2 makes that peer skewable in both directions; only the structs a *bridge* decodes keep the opt-out (ratified 2026-08-27). One narrow exception stays strict deliberately: `ProvisionRecipientMlsPubkeyRequest`. Its additive fields (`mlkem_ek`, `epoch_keys`) were **capability-negotiated** until the 2026-09-24 compat-remnant sweep retired both tokens (`pq-hybrid`, `mail-epoch-schedule` — [`version-compatibility.md`](version-compatibility.md) § Dimension 2); every nest since the baseline accepts both, so apps now send them unconditionally, and a FUTURE additive field there arrives with its own capability token so a newer app never sends an older nest a field it cannot name — negotiation is a different, equally sound answer to the same skew question, and the struct's doc comment records it. (`decode_strict` never sets `deny_unknown_fields` globally, so a non-strict struct *without* the catch-all silently drops unknown keys — harmless for compat, lossy for relay; the sweep removed that gap on the client↔nest surface.) **"Lossy for relay" is not hypothetical — it is why the rule reaches `RpcError`:** a federating nest decodes a *peer* nest's error and re-emits it to its own client untouched (`federation_pool::decode_peer_reply` → `rpc_errors::map_peer_relay_error`), so a key a newer peer added dies in transit without the catch-all. Serde is already non-strict, so mere *tolerance* never needs `extra`; relay fidelity is what does. **Two things to check when adding a catch-all to an existing struct** (both measured 2026-09-11): its derived `PartialEq` now compares `extra`, so any site using whole-struct equality as a *value* comparison silently changes meaning — compare the meaningful fields instead (`folders::PlaceFlags::point` is the worked example); and on a struct reachable from `Result`, the extra 24 bytes can cross clippy's `result_large_err` threshold, which fires at **at least** 128 bytes, not above it. Where the field name is already taken by a domain field, bind the catch-all to another Rust name — `#[serde(flatten)]` never emits under its field name, so this is not a wire change (`bridges_ui::BridgeFollow::unknown_keys`).
5. **Canonical encoding enforced.** `encode_canonical` emits canonical bytes; `decode_strict` rejects non-canonical input. The conformance suite pins known frames to known bytes.

### Rule 3 in full — which enums take an unknown arm (ruled 2026-10-01)

**Why the rule is scoped, and why before the first public release.** A serde enum with no unknown arm fails to decode a variant added after the reader was built, and the failure is never confined to the enum: it takes down everything decoded in the same call — the whole reply, the whole sealed record, the whole stored file. An arm added later does not reach a reader already released. So within a major version an enum is additive only if its arm shipped in the first release that carried it, and an enum that ships closed makes its next variant a compatibility break ([`version-compatibility.md`](version-compatibility.md) I3). As first written the rule named "every tagged union" and was held only for the two envelopes: a census on 2026-10-01 found 409 deserializable enums in the tree, nine with an arm. This section is the scope the rule always needed.

**Scope — who decodes (the test rule 4 uses).** The rule binds an enum when a value of it can be decoded by a build other than the one that encoded it: **(a)** the client↔nest, nest↔nest and device↔device wire; **(b)** anything at rest — a database column, a file, a sealed, signed or content-addressed record, an account-plane row, an MLS application payload — that a later or earlier release, or another device, reads; **(c)** the app↔sync-agent IPC, whose two ends differ for as long as an updated app meets a still-running agent ([`apps/sync-agent.md`](apps/sync-agent.md) § Local agent health). It does not bind: **(d)** a version-locked hop, where both ends ship in one artifact — the bridge-decoded half of `bridge_routing` (rule 4's `strict` opt-out), the nest's channel to its relay sidecar, the nest's own configuration file; **(e)** a type that never crosses a release boundary — a view model, snapshot or action that crosses only the in-process FFI or wasm boundary to the same build's UI, or one whose serde derive no encoder uses because the wire or stored form is a string the reader projects; **(f)** a foreign protocol's shape, whose evolution belongs to that protocol's specification. A downgrade on one device is a different release reading (b): the at-rest rule of [`version-compatibility.md`](version-compatibility.md) § Dimension 1 covers every file an app or the agent keeps.

**The four answers.** Every enum in (a)–(c) has exactly one, recorded in the ledger named below.

1. **Open, carrying.** Required wherever any reader can write the value back out: relay it, rewrite or merge the record it sits in, echo it in a later request, or author a new version of a signed record from the decoded one. The arm holds what it could not read and re-encodes it unchanged — `Other(String)` for an enum whose variants are all units (the shape [`mls-group-key-material.md`](mls-group-key-material.md) already requires of the capability list); for an enum with data variants, a last variant that captures the whole undecoded value as `fauna_cbor::Value`, or as its canonical bytes where the enum also crosses FFI. Rule 5 makes the round trip exact: `decode_strict` admits only canonical input and `encode_canonical` re-emits it byte for byte, so a signature computed over the re-encoded record still verifies. An externally tagged enum with data variants always takes this form, whatever its readers do — serde has no lossy arm for it.
2. **Open, collapsing.** Allowed only for an all-unit enum, or a tagged one whose unknown arm can be a unit, whose readers only consume the value: `#[serde(other)] Unknown`. The collapse loses the original, so the arm is never written back: it carries `#[serde(skip_serializing)]`, and a path that would re-emit it fails loudly instead of replacing a newer value with "unknown".
3. **Record-level skip.** The enum is, or sits only inside, an independently framed record of a stream or set whose reader (i) skips a record it cannot decode without stalling the stream, (ii) leaves that record's bytes where they are — never rewrites, compacts or overwrites it — and (iii) loses only what the three obligations of [`version-compatibility.md`](version-compatibility.md) § *MLS application-message payloads* allow. The answer is a property of the reader loop, so it is pinned by a test on the loop, not by the enum. A request/reply frame is such a record when an undecodable frame fails its one call, at once and typed, and nothing else.
4. **Closed by design.** A new variant is a compatibility break, or ships behind something that keeps it from every reader built before it. Five grounds, one named on the enum's ledger line:
   - **request** — decoded only by the peer that executes it, never stored, echoed or relayed. Refusing an unknown variant is the right answer, and the refusal reaches the sender typed, for that one request; a new variant ships as a new kind, or behind a capability advert the sender reads first (I4's negotiate-down).
   - **consensus** — every reader must reach the same decision from the value: a room policy in the MLS group context, an account-plane lattice record whose merge ranks it, a grant event whose presence decides liveness, a signed invite. An older reader that guesses does not degrade, it disagrees — it forks the group, or keeps the row a newer replica superseded — so "most restrictive" is no rescue. A new statement ships as a new kind, a new extension type or a negotiated capability, which an older reader meets as machinery it knows it lacks. A merge function never ranks a value it cannot decode: it fails, so the row is skipped unadvanced and presented again to a later build.
   - **ladder** — a version stamp refuses a newer file before the enum is met, and a new variant raises the stamp in the same commit: the placement journals (Dimension 1's 2026-08-24 exemption), the region policy's grammar version, the index manifest, the wrapped-blob header. The stamp is read before the strict decode, or the refusal reads as corruption.
   - **extension point** — the enum already carries its open arm by construction: `PostBody::Structured { schema, .. }` takes every later content shape, with its text fallback and its media items where an older reader and an older nest read them.
   - **fixed** — a two-state envelope or a decode-shape helper with no future variant (`Ok`/`Err`, string-or-array, a foreign standard's closed value set held in our own stored form).

**What an unknown value does (both open answers).** Every `match` gives the unknown arm the behaviour of the most restrictive known variant, or stricter: an unknown role holds no privilege, an unknown audience shows to the owner alone, an unknown availability denies, an unknown filter condition never matches and an unknown filter action does nothing, an unknown status renders neutral and offers no action. **An unknown arm never grants and never deletes.** No UI offers it and no build writes it except to pass a carried value through. The duty binds a hand-written projection exactly as it binds a serde arm: wherever a reader turns a wire or stored string, integer or undecodable blob into an enum, the fallback is the restrictive reading — never a live variant chosen because it was convenient. Three such fallbacks were in the tree when this was ruled: an unknown stored mail-filter action read as `Discard`, an undecodable rule list read as "matches everything", an unknown report subject read as an actor. The duty has a writer's half, the MLS payload ruling's obligations generalised: a variant added later may carry only what an older reader can safely ignore — nothing it would have to pin, enforce or count. That needs a new kind.

**Inside a signed or content-addressed record.** Where the record travels embed-as-bytes, verification never depends on the arm: the signature is checked over the carried bytes before any structural decode ([`serialization.md`](serialization.md) § Embed-as-bytes for signed payloads), a relay forwards those bytes and never a re-encode, and a content-derived id is computed over them. Exactness matters where the signature is computed over the **re-encoded** struct, or over signing bytes rebuilt from decoded fields, and there the answer follows from who must agree. If readers only need the value preserved — a profile the owner's other device edits and re-signs — the carrying arm keeps it and rule 5 keeps the bytes. If readers must agree on what it means, the enum is closed on the consensus ground.

**The store around the enum.** An arm protects a decode; it does not protect a file whose reader answers "does not decode" by loading defaults and saving them. Every at-rest store an app or the agent rewrites wholesale refuses to rewrite what it could not read (Dimension 1), so an unknown variant inside it is either carried by its arm or left untouched with the file.

**The ledger and the gate.** The per-enum answers are `tools/check-additive-evolution/enum_ledger.txt`: one line per enum for every crate that holds an enum of (a)–(c), one line per crate for a crate whose enums are all (e). `tools/check-additive-evolution` enforces it — its note that enums were out of scope rested on this rule holding, and it did not. A key is `<crate>::<module path>::<Enum>`: the file's path under `src/` with `lib`/`mod`/`main` dropped, then any inline `mod`; an enum declared inside a function keys at its enclosing module; `#[cfg(test)]` code is neither scanned nor listed. **State:** every `Deserialize` enum in `libs/fauna-*`, `bins/` and the Rust apps — deriving it or implementing it by hand — is listed, every line names an enum that exists, and an enum listed `open` carries an arm — a `#[serde(other)]` unit variant, a last `#[serde(untagged)]` variant, or a hand-written `impl Deserialize` — so a new enum cannot ship closed by omission and closing one is a written, reviewed line. **Diff:** removing or renaming a variant of an enum the merge base's ledger puts in scope — any answer but `local`, `locked` or `foreign`, the (d)–(f) above, where no reader of another release meets the variant — is blocked like a field removal, with a `ratified-breaks.txt` line `rust <crate>::<module>::<Enum>::<Variant> removed` the only way through; the variant's decode-side spellings count, so a rename that keeps the old name as a serde `alias` removes nothing, and dropping the alias later does. The removal binds from the base's line because that is the contract a removal breaks: an enum flipped to `local` in the same change is still held, and a base older than the ledger promised nothing. Adding a variant to a closed enum, other than one closed on the request ground, is blocked unless its ledger line changes in the same commit to name the variant and what keeps it from older readers. An `owed-` line marks an answer ruled and not yet built; none may land on main (`--no-owed` refuses any).

`libs/fauna-protocol/tests/conformance.rs` walks `schemas/test_vectors/*.bin`
and asserts byte-for-byte round-trip equality — executed by the
`protocol-integration-test-check` heavy gate
([`merge-gate-catalog.md`](merge-gate-catalog.md) § The heavy gate catalog). A
schema-evolution gate (`scripts/check-cddl-evolution.py`,
cheap-tier `cddl-evolution-check`, run by both merge scripts — added
2026-08-19) diffs `libs/fauna-protocol/schemas/*.cddl` against the merge-base
and blocks removals, renames, type changes, or making optional fields
required. It is a coarse line-level v1 (a CDDL parser is a future revision);
its behavior is pinned by `scripts/test_check_cddl_evolution.py`. **Both this
gate and its Rust twin below honour ONE allowlist,
`libs/fauna-protocol/schemas/ratified-breaks.txt`** (added 2026-09-24): a
user-ratified in-place break — a [`version-compatibility.md`](version-compatibility.md)
§ Dimension 2 write-off — is listed there per finding, keyed to the exact
**transition** it ratified (`<gate> <key> <transition> <ratified-on>
<ratification>`: `cddl Type removed` / `cddl Type.field <transition>` /
`rust module::Struct.field <transition>` / `rust <crate>::<module>::<Enum>::<Variant> removed`
(an enum variant, § *Rule 3 in full*), the transition `removed`,
`optional→required` or `retyped→<Type>` — `<Type>` the gate's whitespace-free
rendering of the new type, added 2026-09-30 with the first ratified retype), in
the commit that carries the ratification, and the gates skip exactly those
findings: an entry excuses only the finding of its own transition on its own
key — an `optional→required` entry never excuses a later removal or retype of
the same field, a `Type removed` entry no field edit of a same-named type, a
`retyped→<Type>` entry only a retype TO `<Type>` (a later retype away from it
is a fresh finding). Because a removed name never
comes back, both gates refuse a head that revives a key the list records as
`removed` (which is what lets a `removed` entry stay on the list for ever);
each gate ALSO refuses a head whose own-gate entries are missing one the merge
base carried, so deleting an entry — reviving the name in the same change or
not — is caught on its own: the list only grows, enforced as a base⊆head
check, not merely a side effect of the revival refusal. A line whose gate
token is neither `cddl` nor `rust` fails BOTH gates outright (exit 2) — each
treats an unrecognized token as malformed rather than silently skipping it; a
line naming a real token but otherwise malformed (too few fields, an
unratifiable transition, a bare type tightened) fails only that token's OWN
gate (exit 2) — the sibling gate skips a well-formed line for the other
token, same as always, without reading its body. Who may write a line is owned
by [`version-compatibility.md`](version-compatibility.md) § Dimension 2, the
fourth exception's *Who writes a ledger line* rule: a sweep-class remnant line
is a session's own transcription in the removal's commit, any other break is a
user ruling given in the moment. The list exists because the async check diffs against
the last all-green tip, which cannot advance while a ratified break keeps the
check red — which is also why an entry lands WITH its break rather than ahead of
it.
Its Rust-struct analogue, **`tools/check-additive-evolution`**, applies the
same blocked set to the actual `Serialize`/`Deserialize` structs the CDDL
files don't enumerate — every `fauna-protocol` wire payload,
`fauna-segment-store`'s at-rest serde types, and `fauna-core`'s
FFI-crossing/at-rest strict types (widened 2026-08-27; `fauna-sync-engine`'s
at-rest daemon config, `SyncConfig`, was in scope from then until it left
with the daemon on 2026-10-02) — with a `syn` parser rather than
line heuristics. It keys structs by
module-qualified name (so moving a struct between files isn't flagged),
ignores whole-struct removals/renames (an internal refactor or the `.v2`-kind
escape hatch — caught at the kind level by the CDDL gate), and also asserts
every non-strict, `Deserialize`-implementing wire struct carries the rule-4
`extra` catch-all (or `deny_unknown_fields`) unless its key is named in the
committed grandfather baseline
(`tools/check-additive-evolution/catch_all_baseline.txt`) — a **state** check
over the whole tree against that explicit, by-key baseline rather than a
diff against a base commit, so a violation on a struct older than the
merge-base stays visible instead of being caught only on the one commit that
introduced it; pinned by `cargo test -p
check-additive-evolution`. Since 2026-10-02 the same tool holds every
crate's deserializable enums against `enum_ledger.txt` — the state and diff
checks § *Rule 3 in full* → *The ledger and the gate* owns — and its pass
line counts the ledger's `owed-` lines; `--no-owed` fails on any. **Implementation status today (2026-08-21): wired
as the CHECK-tier `additive-evolution-check` gate** (the async dev-fleet merge-gate check,
one build machine — both crates are cross-platform Rust with no per-target
divergence a second machine's run would catch) — unlike its CDDL twin it runs
`cargo`, so the synchronous merge path's no-compile rule places it on the
asynchronous heavy tier rather than alongside `cddl-evolution-check` on the
cheap tier, even though it is itself
fast (~1s warm); [`merge-gate-catalog.md`](merge-gate-catalog.md) § The heavy gate
catalog has the full rationale and measurements.

## Namespace policy

- **Upstream:** kind strings prefixed `fauna.<area>.<verb>` for both RPC kinds and push-event kinds. Examples: `fauna.bridges.link`, `fauna.account.update`, `fauna.protocol.resync_required`, `fauna.segments.list`, `fauna.segments.changed`.
- **Forks:** kind strings prefixed by reverse-DNS (`com.acme.bridges.foo`) or bech32-pubkey (`npub1abc….<rest>`). Math-deterministic, no central registry needed.
- **`RpcError.code`** follows the same policy. `fauna.protocol.*` reserved for infrastructure.
- **No wire-level enforcement.** Strings under 256 bytes, non-empty. Convention documented in `libs/fauna-protocol/schemas/README.md`.
- **Lint (advisory):** `libs/fauna-protocol/tests/namespace_policy.rs` checks a pinned sample list of upstream kinds against `^fauna(\.[a-z][a-z0-9_]*)+$` (it is a spot-check on known kinds, not a registry scan; the <256-byte / non-empty bound above is likewise convention, not enforced at decode).

The Nostr lesson made concrete: numeric kinds break at scale; honor-system
policies fail; math-deterministic namespaces survive. Routing/durability
semantics live in separate envelope fields, never overloaded into the
kind string.

## Crate map

```
libs/fauna-protocol/                  # L3 — transport- AND runtime-agnostic (compiles native + wasm32)
├── Cargo.toml                        # fauna-cbor, serde, bytes, fauna-i18n, fauna-core; tokio target-gated (sync-only on wasm, no rt/net→mio); fauna-wireguard behind default-on `p2p`; `js` feature forwards fauna-core/js for wasm time
├── schemas/
│   ├── envelope.cddl                 # Request/Reply/Push/Cancel + RpcError + LocalizedText
│   ├── error.cddl
│   ├── push_events.cddl              # push payloads with byte-pinned vectors (subset; the enum is the roster)
│   ├── protocol.cddl                 # fauna.protocol.echo, fauna.protocol.resync_required
│   ├── bridges_ui.cddl               # bridges-page user-facing WS-RPC payloads (distinct from wrapped_blob.cddl's daemon-internal MTA/MDA plane)
│   ├── segments.cddl                 # fauna.segments.list request/reply
│   ├── wrapped_blob.cddl             # wrapped-blob wire shapes carried as opaque bstr in WS-RPC payloads
│   ├── README.md                     # CDDL authoring rules + namespace policy
│   └── test_vectors/                 # binary CBOR conformance vectors
├── src/
│   ├── lib.rs
│   ├── envelope.rs                   # Frame/Request/Reply/Push/Cancel + encode_frame/decode_frame
│   ├── codec.rs                      # encode_canonical, decode_strict (Value re-exported from fauna-cbor)
│   ├── error.rs                      # RpcError + LocalizedText
│   ├── unknown.rs                    # Unknown { kind, payload }
│   ├── dispatcher.rs                 # RpcDispatcher::new(stream) -> (Self, driver future); runtime-agnostic, caller drives (no internal spawn, no Send bound)
│   ├── requester.rs                  # RpcRequester trait (AFIT) — the native/wasm transport seam the per-feature client crates are generic over
│   ├── kind.rs                       # KindRegistry + RpcKindMeta
│   ├── push_events.rs                # PushEvent enum + typed payload structs (one per kind — see § Push events)
│   └── protocol_kinds.rs             # EchoRequest / EchoReply
└── tests/
    ├── conformance.rs                # against schemas/test_vectors/*.bin
    ├── forward_compat.rs             # Unknown decode path + extra-field preservation
    ├── namespace_policy.rs           # advisory prefix-shape check
    ├── payments_codec.rs             # fauna.payments.* round-trip (encode_canonical/decode_strict)
    ├── subscriptions_codec.rs        # fauna.subscriptions.* round-trip, incl. byte-for-byte ByteBuf checks
    ├── wsrpc_nil_container_contract.rs # Rust half of the Go↔Rust nil-vs-empty-list wire contract (serialization.md § WS-RPC nil-container & float invariants)
    └── wsrpc_request_cross_language.rs # decode-then-re-encode byte-equality against Go-produced request fixtures (bridge_routing/wrapped_blob types)

libs/fauna-ws-substrate/              # substrate-neutral native WS transport; shared by the bearer client + nest↔nest federation channel (Spec Y2 slice 4)
├── src/
│   ├── lib.rs                        # re-exports adapter + supervisor surfaces
│   ├── adapter.rs                    # TungsteniteAdapter: tungstenite ↔ Bytes Stream/Sink; 30s/60s keepalive; ReconnectSignal close-code mapping; ensure_tls_provider
│   ├── handshake.rs                  # shared bearer-connect pieces: actor_ws_url, `fauna.v1, bearer.<token>` header, MAX_RPC_WS_MESSAGE_SIZE-capped WebSocketConfig — lifted out of fauna-client + fauna-anon-client's duplicated connect steps
│   ├── tls_verify.rs                 # the capturing TLS cert verifier (channel-binding SPKI capture) shared by the bearer client and nest↔nest federation dialer; fauna-anon-client re-exports it
│   ├── supervisor.rs                 # run_supervisor (backoff, dispatcher-slot, ConnectionState) + the SupervisedChannel seam (parameterised over the auth handshake)
│   └── testing.rs                    # mpsc_pair + QueueChannel harness (feature `test-util`)

libs/fauna-client/                    # bearer client: connect step + public façade
├── src/
│   ├── lib.rs                        # re-exports KindRegistry/PushEvent/RpcError/RpcKindMeta; ConnectionState (from substrate)
│   ├── client.rs                     # NestClient — request*, subscribe_pushes, lifecycle
│   ├── ws_adapter.rs                 # subprotocol-bearer connect (SPKI pin; URL/header/frame-cap call into fauna_ws_substrate::handshake); re-exports substrate adapter types
│   ├── reconnect.rs                  # ClientChannel — the client's SupervisedChannel impl (bearer connect + push bridge + 4401 refresh)
│   ├── push.rs                       # PushBroker + KindSubscriber
│   ├── auth_client.rs                # AuthClient — signed-request builders, token caching (auth.rs was removed 2026-06-13; fauna-protocol/src/auth.rs is a distinct, still-live file)
│   ├── ws_challenge_bearer.rs        # WsChallengeBearer — native app-held bearer path minting over fauna.auth.{challenge,verify} (mint_bearer_over_silent_challenge)
│   ├── ws_device_handshake_bearer.rs # WsDeviceHandshakeBearer — device-key sibling minting over fauna.auth.device_handshake (store-principal processes)
│   ├── ws_custody_handshake_bearer.rs # WsCustodyHandshakeBearer — custodian-key sibling minting over fauna.auth.custody_handshake (custody-grant pulls)
│   ├── token_cache.rs                # TokenCache — the double-checked-lock refresh cache shared by the three WS bearer mints above
│   ├── error.rs                      # NestClientError (Rpc/RpcDisconnected/RpcTimeout/SubprotocolMismatch)
│   └── types.rs                      # re-exports ConnectionState from fauna-ws-substrate
└── tests/
    ├── rpc_round_trip.rs
    ├── reconnect_resume.rs
    ├── push_dispatch.rs
    ├── backpressure.rs
    └── auth_flow.rs

libs/fauna-rpc-wasm/                  # L2+L1 wasm WebSocket adapter (web SPA); wasm-only (#![cfg(wasm32)])
├── src/
│   ├── adapter.rs                    # gloo-net WebSocket ↔ RpcDispatcher; subprotocol-bearer handshake; close-code mapping (wasm twin of fauna-ws-substrate/adapter.rs)
│   ├── client.rs                     # WsRpcClient — two transports: Own (RpcDispatcher + spawn_local driver + reconnect loop + JS token-provider) and Shared (over the port); impls RpcRequester; request_raw_bytes = the port owner's half
│   ├── shared_port.rs                # SharedRpcPort — the typed JS interface (emitted into every chunk's .d.ts) a non-core chunk rides the core chunk's socket through; WsRpcClient::over_port
│   └── error.rs                      # WsRpcError (+ to_js/from_js, the port's tagged refusal crossing)

bins/fauna-nest/src/                  # server side
├── ws.rs                             # WsState, RpcConnection, IdempotencyCache, notify_push
├── rpc_router.rs                     # RpcRouter + RpcRouterBuilder
├── routes.rs                         # ws_handler, handle_ws, dispatch_request, dispatch_cancel — subprotocol validation, token check, dispatch
├── dispatch_core.rs                  # shared idempotency-check + spawn/cache/Reply core, reused by the per-actor path above and the federation channel
├── protocol_test.rs                  # fauna.protocol.echo handler + register_protocol_handlers (`test-hooks` builds only)
└── lib.rs                            # build_app — RpcRouter assembly via per-area register_*_handlers
```

## Migration policy (HTTP → WS-RPC)

Y itself ships two atomic slices:

1. **Push-event encoding flip** — the then-current push events (the table above has since grown) moved from `serde_json::to_vec` to canonical CBOR via `encode_frame(Frame::Push(...))`. Single change across nest's `notify_*` call sites.
2. **`WsState` rewrite** — bounded channels, `RpcRouter` carriage, idempotency cache, deadline + cancel handling, subprotocol-validated handshake.

Beyond that, **per-feature specs each carry their own slice**:

1. Identify the HTTP endpoints the feature owns.
2. Define the WS-RPC request/reply types. **Current convention (evolved since the original recipe):** most features declare these as Rust structs in `libs/fauna-protocol/src/<feature>.rs` (covered by the `tools/check-additive-evolution` syn gate, above) rather than a per-feature CDDL file — as of 2026-09-09 there are ~99 `src/*.rs` wire modules but only 7 `schemas/*.cddl`, reserved for **push payloads** and at-rest types that carry byte-pinned conformance vectors. Add a `schemas/<feature>.cddl` only for a push payload (in `push_events.cddl`) or where a pinned vector is wanted.
3. Implement nest-side handlers; register via `register_<feature>_handlers(&mut RpcRouterBuilder)`. **And add each kind's connection-class arm in `bins/fauna-nest/src/bridge_method_allowlist.rs`** — the central per-kind capability gate every live connection passes; an unlisted kind default-denies (`fauna.bridges.permission_denied`), a listed kind refused on caller class answers with its family's `fauna.<ns>.permission_denied` (contract owned by [`api-layers.md`](api-layers.md) § Caller-class authorization → *Refusal codes at the gate*), and handler-direct conformance tests bypass the gate, so a missing arm surfaces only on a real client call (the `fauna.payments.*` family hit exactly this, 2026-07-12).
4. Implement client-side typed wrappers in a per-feature client crate (`libs/fauna-client-<feature>/`).
5. Mark HTTP twins `#[deprecated(note = "use WS-RPC kind fauna.<area>.<verb>; HTTP removed in <milestone>")]`; log `tracing::warn!` on each hit.
6. Update tests; e2e UI tests automatically become WS-RPC tests once the per-feature client crate routes through `NestClient::request`.

First pilot was bridges (Spec 1 — shipped); conversations, feed, and the
remaining areas followed per-feature, through the WS-RPC-everywhere rip-outs
(2026-05-23 → 2026-06-19).

**Removal of the deprecated HTTP twin:** a twin is deleted once **every
app consumes the WS-RPC kind, verified per-app at the rip-out** —
no telemetry gate. Per-feature; not a single big-bang removal. (Ratified
2026-07-07, cluster #2 triage — replaces the original telemetry-window
wording, which was never the exercised practice.)

## HTTP residue

A bounded set of HTTP surfaces stays non-WS-RPC — each externally forced
(RFC-mandated protocol, off-the-shelf monitoring convention, or content-typed
byte transfer). **The residue inventory — which endpoints stay HTTP and why —
is owned by `api-layers.md` § Remaining HTTP** (the single reconciled list;
this doc no longer carries a copy — ratified 2026-07-07, cluster #2 triage).
The migration history per endpoint group (which twin was deleted when) also
lives there.

Two byte-source notes that are transport-owned: blob bytes serve on two URL
shapes (`GET|PUT /api/v1/blob/{cid_b32}` and the hex shape `POST /api/v1/blob`
/ `GET /api/v1/blob/<hex>`, one `BlobStoreBackend`), and segment bytes on
`/api/v1/segments/{kind}/{actor}/{segment_id}[/meta]` — all permanent, ruled
2026-10-01 (`api-layers.md` § Remaining HTTP owns the list).

Reaching "residue-only" took **two orthogonal tracks** that are easy to
conflate. **WS-RPC-everywhere** is the *transport* axis — move each
interactive request/reply off its HTTP route onto a `fauna.<area>.<verb>`
kind on the per-actor WebSocket; this is what removes an HTTP call (auth
bootstrap, discovery, admin, nest-to-nest sync, inbox delivery, share-link
control plane — all shipped, twins deleted, 2026-05-23 → 2026-06-19).
**CBOR-DAG-everywhere** (Layers 3 + 5) is the *encoding / at-rest framing*
axis — DAG-CBOR on the wire, CARv2 at rest — whose one transport-visible
residue item, a segments-byte-source → `GET /api/v1/blob/{cid_b32}`
consolidation, was ruled away 2026-10-01 (the segment route is permanent). The two entangle (a WS-RPC frame is DAG-CBOR by construction,
so a transport migration also advances Layer 3) but the work is distinct.

## Telemetry

Two observability surfaces. The privacy invariant: **none of these
record payload contents** — only shape (kind, code, size, duration).

**The nest exposes no metrics endpoint** (removed from this section
2026-10-01: `bins/fauna-nest` carries no metrics infrastructure, so the
per-kind RPC counters an earlier draft listed here named nothing). Its RPC
observability is the structured tracing below; the only scraped metrics on
a box are the Go mail bridge's loopback `/metrics`
([`../behavior/smtp-server.md`](../behavior/smtp-server.md) § Metrics
surface).

**Client-side metrics** (shared client crate; opt-in via a `MetricsRecorder` trait — target state, unbuilt: no such trait exists today):

- `rpc_unknown_kind_observed_total{kind}` — when client decodes a Push of unknown kind. Headline fork-observability metric: answers "which fork-prefixed kinds are reaching us" without enforcement.
- `rpc_disconnect_total{cause}`, `rpc_pending_in_flight`, `rpc_round_trip_duration{kind}`.

**Structured tracing** — every RPC request gets a `rpc_request{actor_id,
kind, correlation_id}` span; unknown kinds, backpressure drops, and
subprotocol mismatches each warn-log with shape (no payload).

## Test surface

| Layer | Where | What it covers |
|---|---|---|
| Codec / round-trip / canonical form | `libs/fauna-protocol/tests/conformance.rs` | encode → decode preserves; pinned bytes against `schemas/test_vectors/` |
| Forward-compat | `libs/fauna-protocol/tests/forward_compat.rs` | synthesized "future" frames decode as `Unknown`; extra fields preserved |
| Namespace policy | `libs/fauna-protocol/tests/namespace_policy.rs` | upstream kinds match `fauna.<area>.<verb>` (advisory) |
| Client RPC round-trip | `libs/fauna-client/tests/rpc_round_trip.rs` | full round trip with idempotency cache |
| Client reconnect-resume | `libs/fauna-client/tests/reconnect_resume.rs` | mid-RPC disconnect; replay vs surface; request issued while disconnected waits for reconnect then succeeds (`was_in_flight:false`) |
| Client push dispatch | `libs/fauna-client/tests/push_dispatch.rs` | typed routing; broker overflow → `Lagged(n)` recovery |
| Client backpressure | `libs/fauna-client/tests/backpressure.rs` | outbound-queue saturation surfaces `RpcDisconnected` |
| Client auth flow | `libs/fauna-client/tests/auth_flow.rs` | bearer-via-subprotocol; 4401 close → reauth |
| Nest WS handshake | `bins/fauna-nest/tests/subprotocol_handshake.rs` | header validation; missing-token / wrong-actor close codes |
| Nest dispatch | `bins/fauna-nest/tests/{echo_round_trip,cancel_dispatch,backpressure_resync,push_event_typed}.rs` | echo end-to-end; cancel aborts handler; overflow → ResyncRequired; typed push round-trips |
| E2E echo | `tests/e2e-unified/tests/test_api_helpers.py::test_protocol_echo_round_trip` | `fauna.protocol.echo` end-to-end via real WS, all apps |
| E2E benign-flip resilience | `tests/e2e-unified/tests/test_nest_flip_resilience.py` | tier_3, via the shared `common.nest.restart_nest` helper. `test_nest_flip_resilience`: a connected client survives a Watchtower-style nest restart (same `/data`, same identity) — auto-reconnect, silent bearer re-mint, in-gap write waits-then-succeeds (`was_in_flight:false`), live push resumes, no re-onboarding (GREEN linux + web). `test_nest_flip_feed_rehydrate`: a post the client had not yet seen appears after reconnect with no manual refresh — the feed re-hydrates; since 2026-08-22 "the reconnect re-fetch ran" is proven by the shared `feed_reloads` counter pair (`fauna_e2e_agent::FEED_RELOADS_KEY` — pigeonhole against a pre-flip baseline; mechanism entry in `e2e-latency-independent-assertions.md` § Implementation status today) instead of a settle-sleep absence assert on every other delivery path. Track-2 fan-out is code-complete on all 7 apps (windows, web, macos/ios, android, tui — parity 2026-07-19, the last app owing it); the per-app `xfail` was removed 2026-07-13 (2026-07-19 for tui) — GREEN verified linux/windows/web/tui, macos/ios/android verified by code-trace (running their UI e2e still needs their own platform; their `feed_reloads` state legs are entrusted, and until they land their runs of the rehydrate test refuse loudly at the barrier) |
| Federation channel (api harness) | `tests/e2e-unified/clients/ws_rpc_federation_client.py` + `tests/.../api/test_{namespace_sync,post_forwarding,cross_nest_api}.py` | tier_3 Python `FederationChannelClient` runs the `fauna.federation.hello` handshake (first frame, signed by the initiator nest's identity key; loopback `spki_sha256=""`) then drives the `fauna.federation.{sync.pull,sync.push,post.forward,keypackage.fetch,welcome.deliver}` peer kinds over `/api/v1/federation/ws` |

## Design decisions worth knowing

These are the choices most likely to surface in code review or in
follow-on specs. Spec Y has the full rationale for each.

- **DAG-CBOR (not BARE/Protobuf/Cap'n Proto).** Deterministic by spec; 100k+/multi-fork precedent in IPFS, Filecoin, ATproto; mature multi-language bindings. Replaces an earlier BARE decision.
- **WebSocket per RFC 6455 (not WebTransport, not SSE).** WebTransport revisit no earlier than 2028.
- **Bearer in `Sec-WebSocket-Protocol` (not `?token=`).** Subprotocol header beats query param for log hygiene.
- **Hybrid replay: uniform `idempotency_key` envelope + per-kind `forbid_replay` opt-out.** Covers 95% naturally; rare dangerous ops opt out.
- **Kind strings (not numbers).** Math-deterministic fork namespacing; the Nostr cautionary tale.
- **Bounded channels, never drop a Reply, drop Pushes with ResyncRequired marker.** Mature push protocols all converge here.
- **Hand-rolled Rust types pinned by CDDL conformance** (not codegen). UniFFI ergonomics; thin Rust CDDL-codegen ecosystem; the existing generated-file freshness gate (the cheap merge tier; `just check-generated` is the by-hand form) fits.
- **L3 transport-agnostic.** `fauna-protocol` has no WS dependency; future `fauna-peer/` migration to Y.1-over-WG-tunneled-TCP is a re-host, not a rewrite.
- **One socket per actor holds across web's wasm chunks through a shared rpc port, not a shared client (ratified 2026-09-25).** Vite instantiates every wasm chunk with its own linear memory, so the SPA core chunk's `WsRpcClient` cannot be handed to the folders / media / backups / labeler-catalog / atproto-settings chunks as a value, and the chunk discipline (`apps/web.md` § WASM Integration) forbids passing a wasm-bindgen object across. What crosses is pure data over one typed JS interface, `SharedRpcPort` (declared once in `fauna-rpc-wasm/src/shared_port.rs` and emitted into every chunk's `.d.ts`; implemented once, by `rpc.ts` over the singleton): a request is `(kind, 16-byte idempotency key, canonical DAG-CBOR payload) → Promise<canonical DAG-CBOR reply>`, run by the core's `WsRpcClient.requestRaw` on the one socket with the same registry deadline and the same wait for a mid-reconnect gap every core request gets; a refusal crosses as a tagged object one function pair in the same crate writes and reads, the wire `RpcError` inside it as its own canonical bytes, so both chunks classify it identically. A chunk client (`WsRpcClient::over_port`) therefore has no reconnect loop, socket or bearer of its own — the connection-lifecycle consequence is `transport-connection.md` § Connection lifecycle's. Rejected alternatives: folding every page machine into the core chunk (against the chunk-splitting the bundle-size budget rests on), and waking each chunk loop on the core's reconnect (N sockets kept, only the lag removed).

## Future directions

- **Spec Y2 — federation transport (BUILT; authority: `federation.md`).** Nest↔nest
  Fauna federation rides a long-lived peer-symmetric WS-RPC channel
  (`GET /api/v1/federation/ws`), reusing Y.1 at L3 — channel-level mutual
  nest-key handshake (`fauna.federation.hello`, `federation_sig`), a
  `FederationRouter` + `fauna.federation.*` allowlist, the native 30 s
  keepalive. The peer-auth model (nest signs on behalf of its client with its
  long-lived nest Ed25519 key; the peer verifies against the originating
  `nest_id`) replaces the client bearer a foreign client cannot hold. The
  carrier was built in slice 4 (2026-06-02/03) and the sanctioned HTTP interim
  **retired in slice 5 (2026-06-03)** — the channel is the sole Fauna↔Fauna
  carrier; non-Fauna federation stays HTTP+JSON-LD per ActivityPub. Mechanism,
  kind inventory, and mixed-version rules: `federation.md` (owner). Design
  history: ratified 2026-05-30 + 2026-06-01 (tracked internally).
- **Internal sidecar WS-RPC channel (BUILT 2026-06-08/09; its one rider is the
  iroh relay — `behavior/p2p.md` § Architecture).** A fourth nest WS connection
  class beside per-actor bearer / anonymous / federation: a channel to a
  co-located sidecar process, authenticated
  by a sidecar bearer token on a first-frame handshake (`fauna.sidecar.hello`,
  not the nest-key hello) and keyed on the verified sidecar scope. The
  **sidecar dials nest** (`GET /internal/relay/ws`, loopback-gated), attesting
  its X25519 public key in the hello; it then originates
  `fauna.relay.fetch_tls_cert` up the channel and nest answers with the
  `relay.<apex>` cert sealed to that key. The relay holds the channel for its
  whole life (re-dialing when it drops) and asks nest about every endpoint
  that connects to it (`fauna.relay.admit` — `behavior/p2p.md` § The relay;
  no channel or no answer is a refusal). Nest serves the channel on a lean
  loop — an allowlist of the kinds named here, no router — and originates
  exactly one kind down it, `fauna.relay.cert_changed`, when the cert on disk
  changes (`nest/tls-certificates.md` § Keeping the cert alive). The image always runs the relay; it stands by, handed no cert, until the nest has a public name of its own (`behavior/p2p.md` § The relay), and the nest reads "a relay is connected" off this channel — there is no flag. Sidecars on this channel also originate
  `fauna.sidecar.log_events` up the duplex (schema/contract owned by
  `apps/observability.md` § The sidecar log plane; rollout progress is
  authoritative in its § Implementation status today). The channel was built
  for the algorithm sidecar (`/internal/algorithm/ws`, the `fauna.algorithm.*`
  kinds), which was removed whole 2026-10-01
  (`core-client-kind-catalog.md` § Algorithm & Reputation); the class outlived it.
  - **The sidecar credential's lifetime is one nest session; a refused credential
    ENDS the sidecar process (ratified 2026-08-29).** Nest mints a fresh random
    token per sidecar on **every boot** and writes it to `/data/sidecar-token-<scope>`
    (mode 0600, nest-owned), so a token is only ever valid for the nest session
    that minted it. A sidecar reads its token **once**, at start: its s6 run-script
    `cat`s the file as root and passes it through `env`, which is forced for the
    relay — the `fauna-relay` UID may never read the token file itself
    (`security.md` § UID isolation). Therefore **nothing inside a running sidecar can refresh its
    credential**, and any nest restart strands every already-running sidecar on a
    token nest has forgotten. The rule that closes it: a sidecar retries in-process,
    with capped exponential backoff, for as long as its failures are nest simply
    **not being reachable** — but the first failure at the `fauna.sidecar.hello`
    ends the process, which s6 restarts with the token file's current contents. A
    refused credential is therefore a routine, expected end (exit `78`, `EX_CONFIG`),
    never a crash. Two consequences are load-bearing: **(a)** a bare
    `fauna.protocol.disconnected` at the handshake counts as a refusal — being
    wrong in this direction costs one supervised restart while being wrong the other
    way costs the process's whole remaining life. **The rule stands, but its original
    premise no longer does (2026-08-30).** It was written when a nest listener's
    teardown could outrun the `unauthenticated` frame it had queued, making the two
    genuinely indistinguishable; § Layers' driver-lifetime rule closed that, so a
    *nest rejection* now arrives as its real code. What remains indistinguishable is
    a genuine mid-handshake network drop, which is reason enough to keep treating a
    bare `disconnected` as credential-implicated — `credential_implicated` is
    unchanged. The gain is for the human reading the logs, who can now tell a refusal
    from a broken socket; **(b)** the verdict is sound only for the *dial* — a request that
    fails over an already-authenticated channel proves the token good and is retried,
    never exited on (the relay's cert fetch against a pre-claim box is exactly this).
    One implementation: `libs/fauna-sidecar-client` (`CredentialVerdict`,
    `retry_while_reaching_nest`, `serve_while_reaching_nest`), shared by both
    sidecars. The image starts the relay sidecar unconditionally, so it can
    start before nest has minted this boot's tokens and read an empty or stale
    one: that is this same recovery — refused at its hello, ended, restarted by
    s6 against the fresh file — and needs no boot-order rule. Proof: `bins/fauna-nest/tests/conformance_sidecar_channel.rs::a_refused_credential_hands_the_sidecar_back_to_its_supervisor`
    (tier_3, latency-independent) over the shared crate's own tier_1 policy tests.
    Before this, both sidecars retried the forgotten token forever — observed
    2026-08-29 as `unknown sidecar token` for a full 60 s budget under load on a
    witness that passed in 7 s alone.
- **P2P-transport seam — iroh ADOPTED (2026-06-28, reversible); design
  ratified 2026-06-27 (tracked internally).**
  The P2P path is substrate-agnostic behind the shared `libs/fauna-transport`
  trait (`dial`/`listen`/`PeerConn{open_stream,accept_stream,peer_identity,path}`,
  symmetric over an actor-or-nest Ed25519 key). Landed: the trait crate; the
  iroh-QUIC impl (`libs/fauna-iroh`, feature-gated OFF in the shipping
  image); the substrate-agnostic Y.1 peer channel + `PeerNode` lifecycle
  (`libs/fauna-peer-channel`, § Layers); consumers rewired — linux `P2pService`
  (slice 7) and the `fauna-ffi`/android `peer_tunnel_*` surface (slice 8 —
  deleted 2026-10-02 with its `tunnel` feature, which no build enabled once
  android's dead service went; linux is the only node host; web stays
  relay-only). A
  second impl — the bespoke userspace-WireGuard stack
  (`fauna_peer::transport_wg`) — was landed and then **deleted 2026-08-23**
  (user-directed; owner [`../behavior/p2p.md`](../behavior/p2p.md)). The peer
  channel was proven over both real iroh and real WireGuard connections while
  both existed, which is what demonstrated the seam is genuinely substitutable
  rather than shaped around one substrate. The nest advertises
  a single `relay` capability token on `fauna.nest.info` when either relay
  backend can serve, plus `NestInfoReply.iroh_relay_url`; the self-hosted
  `bins/fauna-iroh-relay` sidecar is always run by the image, SNI-routed at
  `relay.<domain>`, tier_4-proven — its TLS/ACME-SAN mechanics are owned by
  `nest/tls-certificates.md` § the infra-subdomain SAN coupling + `behavior/dns-management.md`.
  The `fauna-peer` → Y.1 migration is SCHEDULED (user, 2026-06-30) as
  **foundational refactoring of a dormant subsystem** — the peer data plane
  carries no live traffic (file-sync is 100% nest-mediated), so the compat
  surface is untouched; iroh stays a reversible opt-in component, never a
  wholesale substrate swap. Prototype results tracked internally (2026-06-28).

## Implementation status

> **Rule 3's scope (§ Schema and forward-compat discipline → *Rule 3 in full*, ruled 2026-10-01) — the ledger and its gate are built; the arms are partly built, the four skip pins are built.** `tools/check-additive-evolution/enum_ledger.txt` lists all 400 enums with their answers: 48 open (9 at the ruling, and since 2026-10-02 the signed and sealed record enums of `fauna-core` — `Post`'s facets and references, `Profile`, the former `UserConfig`'s sub-records (account-state plane values since the rail retired 2026-10-02 — [`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *The kinds*), delegation, the custody scope set — plus `KemSuiteId`, the feature-gate and feed-rule enums, eleven `fauna-protocol` reply and filter enums, the enums of the records every device of an account merges and rewrites — the channel history slice, the drafts blob, the cue rollup, the topic model — two of which, `TypedAddress` and `ThreadFlavor`, carry their arm across FFI as `Unknown { canonical }`, and four `fauna-core` enums whose decoder is hand-written and already open), 8 covered by a record-level skip, 43 closed on a named ground, 267 out of scope (version-locked, never crossing a release boundary, or foreign) — and **34 owed**: 30 unknown arms (16 carrying, 14 collapsing) and 4 skip loops whose behaviour is not yet built or pinned. **Re-counted 2026-10-02, after the last two skip pins landed:** the ledger's per-enum lines read 65 open, 12 skip, 42 closed and 12 owed — every owed line a collapsing arm, no skip loop owed; the figures above are the earlier tally and the ledger wins. The five `fauna-ipc::bridge` lines (one closed on the request ground, four locked) left the same day with the FaunaBridge service's diagnostic pipe they described ([`installers/windows.md`](installers/windows.md) § Feature Tree). The `ChannelMessageBody` and `CustodyCeremonyMessage` skips are pinned (an unknown top-level body through `poll_inbound_conv`; an unknown ceremony step refused with nothing captured). Every closed-consensus and closed-ladder enum says so on its type; the wrapped blob and the placement manifests read their format stamp before the strict decode, so a later version refuses as unsupported rather than as corrupt. (The census counted 409; the gate, built 2026-10-02, does not scan `#[cfg(test)]` code, so twelve test-only lines left, and one named an enum already deleted. The same day it began scanning an enum that derives no `Deserialize` but has a hand-written impl, which added the four.) `tools/check-additive-evolution` holds the ledger as the state and diff checks above (the `additive-evolution-check` heavy gate) and counts the `owed-` lines on every pass; `--no-owed` is red while any remains. The feature-gate and feed-rule arms (2026-10-02): an unknown gated feature collapses (no reader writes back a feature it did not author — the ledger first listed it carrying), has no row in any app and is refused by the nest's writes; an unknown tier renders as another rule-setter; an unknown availability is carried and enforces as a deny; an unknown feed rule (or body hint inside one) is carried byte for byte through `fauna.feed.get` and `fauna.feed.update`, matches nothing, and stops discovery for its feed — and the nest stores one only as the echo of a rule the feed already holds, refusing a new one typed. The six account-plane merge functions that ranked an undecodable value lowest fail on it since 2026-10-02 — a row-content refusal, so the walk skips the row unaccounted and `reconcile` presents it again to a build that reads it (`fauna_core::generation::join_device_reach` stays total: its record is a struct, off the ledger); the two frame loops of the sync-agent IPC answer an undecodable frame with a dropped connection and a silent timeout. The three unsafe projections the ruling names read restrictively since 2026-10-02: an unrecognised stored mail-filter action is carried as its string and does nothing, an undecodable rule list reads as one never-matching condition (the mail bridge leaves out, filter by filter, a filter it cannot run), and an unrecognised stored report subject is carried, never read as an actor. Where a `fauna-protocol` enum's UniFFI mirror reaches the apps, a carried value crosses as its canonical bytes (`Unknown { cbor }`) on a mirror no app switches over, and a collapsed reply on a mirror the apps do switch over is answered as a typed error, so no app gains a case. All of it lands before the 2026-10 baseline.

> **web holds one WebSocket per actor across its wasm chunks — BUILT 2026-09-25, closing the gap recorded 2026-09-24.** Until then each wasm chunk that talked to the nest built its own `WsRpcClient` (`libs/fauna-rpc-wasm`) with its own reconnect loop — six beside the SPA core's (the folder wizard and Devices/Folders machine, media, backups, the labeler catalog, atproto settings), each redialling on its own jittered backoff (ceiling 60 s), so after an outage the app's `connection` observable read online while a page's machine was still asleep in backoff and answered a gesture `not connected`. Now every chunk client is built over the core chunk's socket through the **shared rpc port** (§ Design decisions; the web mechanism is `apps/web.md` § Transport): `WsRpcClient::over_port` in `fauna-rpc-wasm`, the core's `WsRpcClient.requestRaw` running the request on the one socket. Witnessed by two tests in `tests/e2e-unified/tests/test_nest_flip_resilience.py`: `test_page_machines_open_no_socket_of_their_own` (the nest's `ws_connections` does not grow as Folders, Media, Backups and Personalization build their machines — **web-only by declaration**: every native app runs a companion sync agent that holds sockets for the same actor on its own schedule, by design, so a nest-side count cannot attribute a socket to a page machine there; their one-client shape — one `NestClient` per session, `apps/common.md` § Nest Connection — is a rule each app's glue upholds, not something the structure enforces, as windows' bullet below shows; measured 2026-09-25 on tui, 3 → 4 sockets with no set created) and `test_a_folders_gesture_lands_the_moment_the_app_reads_online_after_a_flip` (cross-app: a Folders create the moment the app reads online after a flip lands within its ordinary budget); plus the port's own browser tests (`fauna-rpc-wasm/src/shared_port.rs`, `just wasm-test-check`). The one deliberate second socket per actor on web is the both-ends pairing seam's client to a *peer* nest — a second nest, not a second socket to the same one.

> **windows holds one authenticated socket per actor per session start — BUILT 2026-09-28.** Until then the universal post-auth hook (`App.StartMainAppAsync`) fired four passes that each built a one-shot `FfiNestClient` and `Connect()`ed it beside the session's `NestRpcClient`: the seed-map fan-out, the co-admin custody self-heal, the content-sealing epoch refresh and the critical-alert sweep loop (the TLS auto-renew cadence made a sixth outside e2e). The wizard's sign-in follow-ups (the captured DNS credential, the one-tap trust mint, the deferred recovery kit) and the post-claim serving enablement and seed custody each added one more on a first sign-in. One session start logged five `authenticated WS connect` lines for one actor within 9 ms, and one e2e recovery journey spent the process's whole per-nest dial burst (`transport-connection.md` § The dial budget). Now every one of them rides the session's own client through `INestRpcClient`; the wizard's follow-ups are queued for their actor (`PostSignInHandoff`) and run by the post-auth hook. The one helper that still dials its own is the launch-time recoverable-box read (`DeploymentSeedCustody.LoadRecoverableBoxesAsync`), which runs only when the launch could not bring a session up. Pinned by `DeploymentSeedCustodyTests.RunPostAuthSequenceAsync_RidesTheSessionClientInOrder` and `PostSignInHandoffTests`.

> **The principal session ([`transport-connection.md`](transport-connection.md) § Connection lifecycle → *The principal session*, ratified 2026-10-01) — built 2026-10-02.** `GET /api/v1/principal/ws` (`bins/fauna-nest/src/principal_session.rs`) runs the issuer's resource-server gate at the upgrade, binds the connection to the principal, registers it in `WsState`'s principal registry apart from the account's sockets, closes it `4401` at the token's `exp`, and is swept by `fauna.principals.revoke` and by every actor-wide teardown. **Door (c) is ruled and owed (2026-10-02):** a grant family's ending closes the sessions its tokens opened, and both forced rotation arms close every principal session — the binding's `sid`, the gate's per-RPC family read, the sweep after every ending and the census entries are the build; until it lands, a session whose family ended lives to its token's `exp`, at most 15 minutes. Every request on it dispatches through the `ThirdParty` gate ([`apps/bridges.md`](apps/bridges.md) § Capability-allowlist enforcement). Pinned at tier_1 by `principal_session::tests` and end to end by `tests/e2e-unified/tests/api/test_third_party_session.py`. A third-party web app on its own origin reaches the issuer's plane cross-origin — redeems its code and reads the `DPoP-Nonce` the dial leans on — since 2026-10-02 ([`../behavior/authorization-server.md`](../behavior/authorization-server.md) § The issuer → *Cross-origin access* owns the posture).

> **Third-party event doors (§ Push events → *Third-party event doors*, ratified 2026-09-05) — all three doors built 2026-10-05.** The `fauna:events:subscribe` arm (`fauna_scope::FaunaScopeArm::EventsSubscribe`, the filter `event_reaches`), its ceiling kind `fauna.events.poll` and the HTTP door `GET /api/v1/events` live in `bins/fauna-nest/src/events_doors.rs`, with the push hooked into the scoped nudge (`sync_handlers::notify_sync_changed_scoped`) and the cursor read from the nest log (`db::account_state::ext_scope_heads_since`). Pinned at tier_1 by `events_doors::tests` (two principals of two publishers: the poll and the push name only the scopes a session may list; a token without the arm cannot poll; the external-apps switch silences the push) and end to end by `tests/e2e-unified/tests/api/test_third_party_events.py` (two admitted publishers, the push under a causal barrier, the long-poll answered by the next write). **The webhook** (`bins/fauna-nest/src/events_webhook.rs` — the security event token under the issuer key, ruled in the bullet above): pinned at tier_1 by `events_webhook::tests` (the token verifies as a receiver would and the access-token verifier refuses it; deliveries coalesce per principal), `events_doors::tests` (the walk selects by the live reach; the switch silences it), `fauna_protocol::kind_manifest::tests` (`events_uri` refuses anything but `https` on the publisher's host) and `db::third_party_principals::tests` (the member rides to the row; the latest consent wins), and end to end by the same e2e test (A's server receives a verifiable token naming A's `client_id`, A's `sub` and a cursor past B's write; B, unsubscribed, is POSTed nothing).

> **`KindRegistry` wired into the production clients (2026-07-31) —
> implemented.** Until this landed, the registry was **inert**: every
> production client built it via `KindRegistry::default_with_protocol_kinds()`,
> which registers exactly one kind (`fauna.protocol.echo`), and all 54
> `register_*_kinds` methods — 352 declared kinds — were called only from
> `kind.rs`'s own test module. So the § above described behavior no shipped
> client had: every kind fell back to the spec defaults, i.e. a 30 s deadline
> and, the sharp one, **`forbid_replay = false`** — auto-retry permitted even
> for kinds whose author had explicitly written `forbid_replay: true` in a
> method that never ran. `KindRegistry::full()` is now the production
> constructor and the six client entry points (`fauna-client`,
> `fauna-rpc-wasm` ×3, `fauna-anon-client` ×2) plus `fauna-mail::lookup_kind`
> use it.
>
> The same metadata is declared twice — nest-side per handler in
> `register_<area>_handlers`, client-side in `register_<area>_kinds`. Reconciling
> them found **zero** `forbid_replay` or `default_deadline` disagreements on the
> ~155 kinds both tables declared, and **197 kinds the nest dispatches that the
> registry did not declare at all** (129 `fauna.bridges.*`, 19
> `fauna.subscriptions.*`, 13 `fauna.filesync.*`, …). Those 197 were lifted
> verbatim from the nest's own registrations, so no kind's declared semantics
> changed; what changed is that the declarations now take effect. The two tables
> are held in lockstep by `bins/fauna-nest/src/rpc_router.rs::
> router_and_kind_registry_agree_on_every_kind`, which fails on a missing kind
> or either kind of drift.
>
> **`Request.replay_forbidden` populated from the registry (2026-07-31) —
> implemented.** The wire mirror `envelope.rs` describes is now emitted:
> `RpcDispatcher` holds a set-once `KindRegistry`, and `request_raw` sets field
> `5` to `Some(true)` exactly when that registry says the kind is
> replay-forbidden. `Some(false)` is deliberately never sent — the nest reads
> the field as `unwrap_or(false)`, so it would put a byte pair on every read for
> no information, and never sending it keeps the field's *presence* meaningful.
> Consequence worth knowing: `dispatch_core.rs`'s "caller missing
> replay_forbidden hint" warning, which used to fire for every forbid-replay
> request, is now a **stale-client detector** — it fires only for a caller that
> does not know the kind's metadata.
>
> The registry is attached to the dispatcher rather than passed per call, so
> there is no per-call-site opt-in to forget; for the reconnecting bearer client
> it rides `SupervisedChannel::kind_registry()`, which re-attaches on every
> reconnect (the dispatcher is rebuilt per connection). A dispatcher with **no**
> registry sends no hint, which is the correct value for the peer and sidecar
> channels — `KindRegistry` is the client's table and deliberately declares
> none of their kinds, and neither of those tables has a forbid-replay kind to
> hint about.
>
> **The federation channel carries its own registry (2026-08-01) —
> implemented.** The federation table *does* have forbid-replay kinds, so a
> hint-less federation dispatcher made the peer's "caller missing
> replay_forbidden hint" warning fire on every cross-nest keypackage fetch.
> Both channel ends now attach a metadata-only registry **derived from the
> serving `FederationRouter`** (`federation_router.rs::hint_registry`,
> attached in `federation_channel::{dial, serve_listener}`) — never an
> extension of the client's `KindRegistry`, per the scope note above. The same
> derivation replaced `federation_pool::originate`'s per-call-site
> `retry_safe: bool` (`FederationRouter::retry_safe` = `!forbid_replay`): one
> table now serves the kind, emits the wire hint, and decides the §4.D
> dead-link re-send, so those three can no longer drift apart. Each kind's
> idempotence rationale lives at its declaration site in
> `federation_handlers.rs`; the value set is pinned by
> `federation_router.rs::the_federation_replay_forbidden_set_is_exactly_these_kinds`.
>
> **Partly done — the `forbid_replay` value audit.** The flags are now
> *effective* (they were inert before the wiring above) but were authored
> handler-side under the mistaken assumption that the idempotency cache would
> catch a double-apply. The audit opened 2026-07-31 and covers every permitted
> kind that is **mutating by verb**: each one is an unreviewed assertion that
> its handler is naturally idempotent.
>
> Five kinds have been audited. Four hold and stay permitted:
> `fauna.backup.writer_grant.{register,revoke}`,
> `fauna.backup.generation.restore` (idempotent only *by consumption* — the
> successful call unretains the row it promoted, which makes that unretain
> load-bearing for replay safety), and `fauna.payments.claims.redeem` (a
> same-actor retry deliberately re-grants rather than erroring). The fifth was
> **wrong and is now `forbid_replay = true`: `fauna.payments.claims.mint`.** It
> allocates a fresh random code per call and inserts a new row keyed on it, with
> the external reference derived from that same code, so nothing dedups. That
> is the first confirmed wrong value, and it was on the money path. Rationale is recorded at each declaration site rather than
> here, so it cannot drift away from the flag it justifies.
>
> **Three more wrong values, found 2026-07-31 by structural search** rather than
> by reading kinds in file order. The search predicate is the one the `mint`
> defect generalises to: *does the handler key its row on a value it allocates
> itself?* If yes, no column can dedup a replay.
>
> - **`fauna.inbox.send` → `true`.** The strongest of the three, because it
>   cannot become idempotent without a schema change: `push_inbox_with_quota`
>   derives `content_id` from `blake3(recipient ‖ now_millis ‖ INBOX_NONCE ‖
>   payload)`, mixing in a timestamp *and* a monotonic counter, so the key is
>   unique **by construction** on every call, and neither the delivery row nor
>   the recipient's `inbox_bytes_used` accounting can dedup. Its old `false`
>   cited "the federation leg is idempotent (idempotency cache); the local leg
>   matches the retiring HTTP twin's at-least-once semantics" — both halves fail:
>   the cache cannot catch a retry (above), and "at-least-once" is a description
>   of non-idempotence, not a justification for permitting replay.
> - **`fauna.admin.invite_codes.create` → `true`.** The `mint` shape exactly: an
>   empty `code` makes the handler allocate a fresh `generate_invite_code()` and
>   plain-`INSERT` a row keyed on it, so nothing dedups a replay. The
>   admin-supplied-code branch *is* idempotent (UNIQUE on `code`); since the flag
>   is per-kind, the non-idempotent branch decides it.
> - **`fauna.bridges.train_spam_classifier` → `true`** *(the kind left the wire 2026-10-02 — `mail-spam.md` § Implementation status item 4b; the reasoning carries to `put_spam_model`, the one remaining spam write ingress)*. Training is an
>   accumulator, so a repeated same-key call double-weights the same message in
>   the user's Bayesian filter. Its `TRAIN_DEDUP_WINDOW` is **not** a replay
>   guard — a 1-second, in-memory, process-local collapse of the MUA's
>   simultaneous STORE+MOVE double-signal, necessarily expired by the time a
>   post-reconnect retry arrives. (The window retired 2026-09-25 for the durable
>   one-lesson rule, `mail-spam.md` § 3; the flag stays `true` — an intervening
>   opposite-label lesson re-opens the key, so the rule is a product invariant,
>   not a transport guarantee.)
>
> Each carries a hazard pin asserting the *harm*, not the flag, so it stays
> meaningful if the handler is ever made idempotent. The replay-forbidden set is
> therefore **29**.
>
> ⚠️ One correction landed with them: `register_admin_kinds`' doc comment
> justified `false` across all 51 admin kinds partly by "the per-connection
> idempotency cache replays the first reply on a recovered connection". That
> clause was **false** and is deleted — only the handler-side guards it also
> names make those kinds replay-safe.
>
> **Most mutating kinds remain unreviewed** (the 2026-08-01 pass below settled
> the import + `storage.migrate` families — a tally would be honest
> bookkeeping, never coverage, since the mutating denominator is not knowable
> from the kind string). Not urgent by caller —
> `request_auto_retry` has zero production callers, and `fauna-mail::lookup_kind`
> exports the metadata over UniFFI but the Go bridge never calls it — so no
> shipped code acts on these flags today. It must be settled before the first
> auto-retry caller ships. The replay-forbidden set is pinned exhaustively by
> `kind::tests::the_replay_forbidden_set_is_exactly_these_kinds`, because the
> router/registry parity gate enforces only *agreement*: a coordinated
> `true` → `false` edit in both tables passes it clean, as a mutation confirmed.
> Tracked internally.
>
> **Two more client-table wrong values (2026-08-01): the mailbox-migration
> import kinds.** `fauna.bridges.import_message` and `import_message_batch`
> are now `forbid_replay = true`, on two grounds recorded at both declaration
> sites: the session's progress counters are accumulators
> (`imported_count = imported_count + ?` — even a deduped replay is
> recounted, so the wizard's persisted progress drifts), and the dedup guard
> is **narrower than the kind** (`skip_dedup: true` skips it entirely, so a
> replay stores the same message twice — the `invite_codes.create`
> weakest-branch rule). Interrupted imports resume via `list_import_sessions`
> + per-mailbox cursors, the feature's own design, never via a blind wire
> retry. The rest of the import family stays permitted with rationale at the
> nest declaration site: `start_import_session` is at-most-once by the
> per-(actor, source) lock, and the five state transitions are at-most-once
> by their `WHERE state IN (allowed_from)` guards. The
> `fauna.storage.migrate.*` family was audited the same day and stayed
> permitted (its old rationale's idempotency-cache clause — the falsified
> one — was deleted); the family itself was retired 2026-09-27
> (`nest/common.md` § Blob Store → Backend).
> Hazard pins: `a_skip_dedup_repeat_stores_the_same_message_twice` +
> `a_deduped_repeat_is_still_recounted` (`bridge_import_handlers.rs`). The
> replay-forbidden set is **31**.
>
> **The federation table's audit (2026-08-01) found two more wrong values —
> fixed**, and unlike the client table's, the federation flags are acted on
> **today**: `FederationRouter::retry_safe` now drives the §4.D dead-link
> re-send, so a wrong `false` here was an *active* double-apply on a mid-call
> channel drop, not a latent one. Both are the `fauna.inbox.send` defect
> arriving over the channel: **`fauna.federation.inbox.deliver` → `true`** and
> **`fauna.federation.welcome.deliver` → `true`** each land in
> `push_inbox_with_quota`, whose content id is unique by construction
> (timestamp + monotonic nonce — the inbox is deliberately an append log), so
> a re-sent delivery is a fresh row that neither the store nor the recipient's
> quota accounting can collapse. Both old rationales leaned on
> the per-connection idempotency cache, which never survives the redial the
> retry path takes. The federation set — those two plus the destructive
> `keypackage.fetch` — is pinned exhaustively by
> `federation_router.rs::the_federation_replay_forbidden_set_is_exactly_these_kinds`;
> the remaining federation rows' idempotence rationales were re-verified and
> consolidated at the `meta()` declaration site in `federation_handlers.rs`.
>
> **The mail family (2026-08-02) — one wrong value, and the first decided
> value in the `fauna.bridges.*` table.** That family is structurally
> different from the ones above: its ~70 kinds do not carry per-kind metadata
> but share **five constants** built at the top of
> `KindRegistry::register_bridge_kinds` (`fetch`, `fetch_short`, `provision`,
> `revoke`, `routing`), every one of them `forbid_replay: false` — so no kind
> in it had ever carried an individually-considered value. A flip therefore
> *splits a constant*, and the rationale belongs on the constant or on the kind
> lifted out of it, never silently per-kind.
>
> - **`fauna.bridges.check_submission_quota` → `true`.** The one wrong value.
>   Its name reads like a query and it had inherited `fetch_short` — a *read*
>   constant — but its only effect is a consuming debit:
>   `try_consume_submission_quota` is a bare read-modify-write
>   (`used = used + recipient_count`) keyed on `(actor, day_bucket)` with **no
>   dedup key**, so nothing collapses a repeat and the debit is not idempotent.
>   The day bucket is a counter key, not a replay guard. Lifted out of
>   `fetch_short` into its own literal. Hazard pin:
>   `a_replayed_submission_quota_check_debits_the_allowance_twice`.
> - **The five `routing`-weight kinds stay permitted, now decided rather than
>   inherited**, with the rationale recorded on the constant.
>   `ingest_inbound_mail`, `submit_inbound_mail` and `append` are
>   *content-addressed*: `message_id` is
>   `blake3(domain_tag ‖ actor ‖ timestamp ‖ body)` where the timestamp is the
>   caller's `public_metadata.timestamp` — no server clock and no nonce, which
>   is exactly what makes them the inverse of `fauna.inbox.send` — so a replay
>   derives the same id, the `segment_records` lookup hits, and every
>   downstream effect is gated on the resulting `inserted` / placement-outcome
>   flag (no second segment append, no second UID allocation, no duplicate
>   placement record, no duplicate arrival push). `put_event_ciphertext` and
>   `put_card_ciphertext` key on the caller's 32-byte `uid_hash` and upsert.
> - **One non-idempotent effect is known and ACCEPTED** on the two inbound
>   kinds, and is recorded rather than fixed: the family-safety
>   null-reverse-path probe (`consume_sent_msgid_correlation`) decrements a
>   correlation budget at *verdict* time, before the dedup gate, so a replay
>   spends a second unit. Three grounds, the third decisive — it fails toward
>   *holding* a DSN for guardian review, never toward delivering one; the path
>   is SMTP, already at-least-once, so the remote MTA's own retry does the same
>   thing and forbidding the WS replay would not remove it; and forbidding it
>   would push the bridge toward not retrying a delivery whose message half is
>   safely dedup-guarded, trading an over-held bounce for actual **mail loss**.
>   `bridges.atproto.*` was left to the ATProto PDS build still in flight,
>   whose kinds already carry per-kind rationale at their declaration sites.
> - **`report_auth_event` / `report_session_close` stay `false`, decided.** Read
>   via the predicate the defect above generalises to — *a read- or
>   telemetry-shaped verb hiding a write* — and both came back clean: each is
>   idempotent **by construction**, deriving an `idempotency_hash` over its full
>   content tuple and `INSERT OR IGNORE`ing against a UNIQUE index.
>
> The replay-forbidden set is **32**. The naming predicate is the reusable
> output of this pass: in a family whose flags are inherited, the kinds worth
> reading first are the ones whose *verb* promises less than the handler does.

> **`forbid_replay` audit, 80th pass (2026-08-02) — the MODERATION family,
> all 12 kinds read to their writes.** One wrong value, and it was reachable by
> a **second ingress into an accumulator whose first ingress had already been
> decided**: `fauna.moderation.train` → `true` *(back to `false` 2026-10-02: the kind no
> longer trains — its one write is the report capture, whose insert/delete is
> idempotent; `mail-spam.md` § Implementation status item 4b)*. The handler fed
> `BayesianClassifier::train` → `SpamModel::apply_forward_delta_ngrams`, which
> does `entry.spam/ham.saturating_add(1)` per distinct n-gram plus the class
> message counter, with **no dedup key**; a replay applies the same delta twice
> and skews the caller's own spam filter. The defect was **latent** — the shared
> `ModerationClient::train` issues a plain `request(...)` and that crate has no
> `request_auto_retry` call sites, so nothing was double-training; what was
> wrong is the *assertion* that a future caller may safely auto-retry it.
> `fauna.bridges.train_spam_classifier`
> reaches that identical accumulator and was set `true` with a written rationale
> — so the two ingresses had contradicted each other, one decided and one
> inherited, since the WS-RPC migration.
> - **The generalisable predicate this pass adds: an accumulator reached by more
>   than one kind must carry the same value at every ingress.** Grep for the
>   *shared mutation helper*, not the kind name — the flag lives per kind, the
>   non-idempotency lives in the helper they share.
> - **A dedup window is NOT replay safety, and the older rationale said so.**
>   `train_spam_classifier`'s comment records why its `TRAIN_DEDUP_WINDOW` guard
>   does not confer it: the window is 1 s, in-memory and process-local, meant to
>   collapse an MUA's simultaneous STORE+MOVE double-signal, whereas a replay
>   only follows a *reconnect* — necessarily far outside it. So the fix here is
>   the flip, not a dedup mirror. (The window itself retired 2026-09-25 for the
>   history-backed one-lesson rule, `mail-spam.md` § 3; the flag stays.)
> - **The other 11 stay `false`, now decided rather than inherited**, with the
>   rationale recorded at the declaration site: `stats`/`actions`/`*.status` are
>   pure reads; `model_sync` (removed 2026-10-02) adopted on a strict `>`;
>   `report_share.set`/`signal_share.set` are last-wins bool writes;
>   `signal_contribute` goes through `capture_signal`, whose per-factor
>   insert/delete return their own changed-bit and gate the recompute (the
>   `capture_report` shape); `appeal`/`legal_takedown` append an
>   audit or obligation row a replay would duplicate — a redundant transparency
>   entry, never a second application of an effect, and `legal_takedown` is
>   Admin-only with a last-wins withhold flag.
> - **The family's registered rationale had been refuted twice over.** Its doc
>   comment argued three times that retries are safe because "the per-connection
>   idempotency cache replays rather than re-executes" them — which the § above
>   (`the idempotency cache does NOT deduplicate an auto-retry`) forbids, and
>   which its own sibling kind had already contradicted in code. Rewritten to
>   argue from handler idempotency, per this doc's criterion.
>
> **Same pass, second unit: both `revoke` constants are now decided, no flip.**
> All four non-atproto kinds on them (`revoke_dkim_blob`,
> `revoke_wrapped_mls_blob`, `revoke_wrapped_submission_token`,
> `fauna.capabilities.revoke`) are a `DELETE` keyed on a caller-supplied key
> whose rows-affected is explicitly discarded, replying a **constant**
> `{ ok: true }`. They are the clean counter-example to the consume-shaped class
> above — converge in state *and* in reply — and the difference is one line:
> **replying a constant instead of the lookup's outcome.** Where a consume-shaped
> kind can honestly do that, it avoids the misleading-answer class entirely and
> needs neither a flip nor a lookup.
>
> The replay-forbidden set is **33**.
>
> **The `provision` constants are decided, no flip — and the pass's real find
> reverses an earlier decision (2026-08-02).** Both `provision` constants (the
> seven non-atproto `register_bridge_kinds` members and the two
> `fauna.capabilities.{mint,renew}`) are idempotent by construction and stay
> permitted, with rationale at each constant: every one is a caller-keyed
> `INSERT OR REPLACE` — the key is a request field, or the canonical index of
> the caller's own blob — replying a constant, so state and reply both converge.
> `register_service_user`'s writes are set-once-frozen; `capabilities.mint`'s
> per-owner quota is explicitly replace-aware, so a replayed mint is not charged
> a second slot. Every mutation helper under both constants was grepped for
> further callers and no second **wire** ingress disagrees (the extras are
> nest-internal: the DKIM rotation-mint task, ACME's cert seal-on-read, and the
> in-process web-serve holder's boot-time self-enrollment).
>
> - **`fauna.bridges.start_import_session` → `true`. This REVERSES the
>   2026-08-01 ruling recorded above**, which called it "at-most-once by the
>   per-(actor, source) lock" and filed it as the wrong-answer-never-double-apply
>   shape. The at-most-once half is right; the classification is not. In the
>   consume-shaped class the caller supplies the key, so a lookup restores the
>   lost answer — but here the id is minted server-side (`Uuid::new_v4`), so a
>   replay mints a *second* id, trips the lock its own first call took, and
>   returns `import_source_locked`, leaving the caller with **no id at all**.
>   Worse, that error states an import is already running — `mailbox-migration.md`
>   § Architectural rules renders it "on another device" — so after a replay the
>   nest confidently reports a device that does not exist. And the lookup remedy
>   is **unavailable**: `SourceLocked` is also the genuine second-device case,
>   and once the id is server-minted nothing on the wire separates a replay from
>   a second device, so answering with the existing session's id would defeat
>   what the lock is for. Forbidding the blind retry is the only fix that
>   converges the answer: the caller then sees `RpcDisconnected { was_in_flight }`
>   — the honest "don't know" that is precisely the trigger for the documented
>   `list_import_sessions` resume path. It also removes an inconsistency inside
>   one flow, since `import_message{,_batch}` were already `true`. Hazard pin:
>   `a_replayed_create_trips_its_own_lock_and_loses_the_session_id`.
> - **The generalisable lesson: "no double-apply" is not sufficient grounds for
>   `false`.** The criterion this doc states is *natural idempotence*, which
>   covers the reply as well as the state. A kind that converges in state while
>   answering falsely still fails it — and when the caller cannot key the
>   recovery, the misleading answer is not a cosmetic wrinkle but the loss of the
>   only handle on the work that was done.
> - **A registry gap found by the same pass, fixed:**
>   `fauna.bridges.atproto.rotate_as_key` was dispatched by the nest from
>   an earlier commit but never added to `KindRegistry`, so
>   `router_and_kind_registry_agree_on_every_kind` was **red on the main branch**
>   and clients fell back to the spec defaults for it. Registered mirroring the
>   router (5 s, permitted) to close the drift without changing behavior. That
>   mirrored `false` was explicitly **inherited, not decided** — and **RULED
>   2026-08-02 (F4 slice 8d): the pass was right, and it is now `true` on both
>   sides**, deadline moved to 30 s to match. Each call mints a fresh P-256 key
>   and moves the current blob into the single previous-generation slot, so a
>   replay advances the chain twice and discards the pre-rotation key, and the
>   reply names a `kid` the first call never returned. The distinction that
>   settles it: an **admin** pressing the button twice is intended, while a
>   **transport** replay is invisible and destroys a generation nobody chose to
>   destroy; `forbid_replay` governs only the second.
>   Flipped at the first moment it was free to flip — slice 8d is the kind's
>   first client caller, so there was no shipped behavior to preserve, and there
>   is now. Harm pinned by
>   `bridge_atproto_handlers::tests::rotate_as_key::a_replayed_rotation_discards_the_pre_rotation_key`
>   (it asserts the discarded key and the divergent kid, not the flag).
>
> The replay-forbidden set is **35**.

> **The `fetch` shared constant closes, 82nd pass (2026-08-02) — one more flip.**
> 47 kinds carried the shared `fetch` meta constant; the last high-yield block of
> the family audit read all 33 in-scope kinds to their SQL (the `atproto.*` nine
> stay walled off while the full-PDS work is live on them; the five import
> state transitions are scoped separately). **`fauna.bridges.copy` → `true`**,
> lifted out of the constant: `copy_within_locked` allocates the destination UID
> from the dest mailbox's `uid_next` and `INSERT`s the placement keyed on that
> fresh UID, so nothing in the request identifies the copy — a replay stores a
> second placement per source UID, a true double-apply (duplicate mail plus a
> second RFC 9208 quota charge), not the 76th pass's misleading-answer class.
> `fauna.bridges.move` stays `false`, decided rather than inherited: `move` runs
> the same helper but consumes its own source rows first (copy-then-expunge
> under one lock), so a replay finds them gone and copies nothing — state
> converges even though the reply diverges. The other 31 stay `false`, decided
> with rationale at the constant and the C.4/C.5 declaration sites. Full
> per-kind detail: `../behavior/imap-server.md` (owner of the IMAP-command
> mapping). Hazard pin: `replayed_copy_duplicates_the_message_hazard_pin`.
>
> The replay-forbidden set is **36**.

> **`fauna.conversations.group.remove` joined the set at declaration
> (2026-08-18)**, not through an audit pass: the roster's new remove/leave
> kind carries `forbid_replay = true` for the same reason as its sibling
> `group.invite` — the removal fan-out to the removed member's home nest is
> an externally-visible side effect. The replay-forbidden set is **37**.
> (`group.invite` and `group.remove` were since retired with the group plane,
> 2026-09-26, and left the set.)

> **Revocation teardown (2026-07-10, tracked internally) —
> implemented.** `RpcConnection`'s revocation `watch` + `WsState::disconnect_actor`
> (`bins/fauna-nest/src/ws.rs`), the 4401 close in `run_connection` /
> `close_revoked` (`bins/fauna-nest/src/routes.rs`), and the calls from
> `session_handlers` / `account_core` (lockout), `admin_ws_handlers` (suspend),
> `eviction.rs` (the ladder), and `pending_actions` (deletion — whose executor
> gained `AppState` for exactly this; it previously held only a `CacheDb` and so
> could not revoke at all, leaving a deleted user's bearer live to its 1 h TTL and
> its socket live indefinitely). Before this, `4401` had **no nest-side producer**:
> every app already handled the close code, and nothing emitted it. Proven by
> `bins/fauna-nest/tests/conformance_revocation_teardown.rs` (tier_3, real
> WebSocket: 4401-not-1001, dispatch stops, idempotent + no-op for an actor with
> no connections).

> **The four missed paths + the bridge third piece (2026-07-12) —
> implemented.** A 2026-07-11 sweep found four
> authority-stripping paths calling neither half; all four now run
> `AppState::revoke_actor_authority` (tokens + `disconnect_actor` +
> `BridgePushRegistry::remove_mda`): bridge service-user revocation (all three
> sites), the executor's `admin.remove` / `admin.change_role` arms, and the
> legacy `admin.suspend_user` pending-drain (since removed with its action type
> by the compat-remnant sweep — `fauna.admin.users.suspend` suspends
> immediately and queues nothing). The Go bridge's reconnect-time
> whoami now maps the capability gate's refusal to revoked
> (`wsrpc.WhoamiIndicatesRevoked`) — a revoked bridge could never see a
> `status=revoked` reply mid-run, so the pre-existing `wh.Status == "revoked"`
> arm was unreachable and the bridge only learned of a mid-run revoke on a
> natural WS drop + supervisor restart. Proven by
> `bins/fauna-nest/tests/conformance_bridge_revocation_teardown.rs` (tier_3,
> real WebSocket: 4401 + push stream stops + a re-minted reconnect gets
> nothing), the caller pins in `bridge_blob_handlers` /
> `tests/pending_actions.rs`, and `wsrpc`'s `TestWhoamiIndicatesRevoked`.

> **Dispatch-gate half (2026-07-10) — implemented.**
> `caller_class_for_actor` now reads `users.{suspended,
> locked_until}` in one query (`CacheDb::actor_authority`) and denies three ways
> instead of one: a **locked-out** actor (previously enforced only at token mint,
> leaving the emergency control the weakest of the three), a **suspended** actor
> (unchanged), and an actor with **no `users` row** — which previously fell
> through to `CallerClass::User`, so a deleted actor's open socket kept dispatching
> and the function's "None when the actor is unknown" contract was a lie. Admins
> resolve before the lookup and stay exempt (§ above). The `Admin ⊇ User` converse
> is now enforced at the **add** door too: `fauna.admin.admins.add` refuses a
> target with no `users` row, at the handler and in the pending-action executor.

**Current state (re-baselined 2026-07-07, cluster #2 review):** the WS-RPC
surface is fully live. Spec Y plans 1–4 landed; **all seven apps ride the
per-actor WS** (linux tracked internally; web via the wasm façade,
2026-05-23; windows/macos/ios/android via the shared FFI; tui via the shared
Rust client stack, parity 2026-07-19). The protocol crate
is runtime-agnostic and compiles to `wasm32-unknown-unknown`; the nest runs
bounded per-actor channels, the kind-routed `RpcRouter` (76 per-area handler
registrations as of 2026-08-24), the idempotency cache, deadline + cancel handling, and typed
`PushEvent` frames with per-connection `seq`. All push events ride canonical
CBOR end-to-end; the legacy `?token=` query parameter is gone.

Dated provenance (tombstones — each twin's deletion is final):

- **Web façade — adopted 2026-05-23**
  (tracked internally): `fauna-protocol`
  made wasm-buildable; the `RpcRequester` seam genericizes the per-feature
  client crates over native + wasm; `libs/fauna-rpc-wasm` (gloo-net adapter +
  `WsRpcClient` + minimal reconnect loop) + the `libs/fauna-wasm` bindgen
  exposure; the SPA's `rpc.ts` owns the singleton client. Proven by
  `test_web_ws_rpc_echo.py` + `test_settings.py::test_email_filter_crud`.
- **CBOR-DAG-everywhere Layer 3 — closed 2026-05-17:** canonical byte-source
  surface `GET|PUT /api/v1/blob/{cid_b32}` (`blob_routes.rs`); CARv2-at-rest
  in `libs/fauna-carv2` (consumed by `fauna-segment-store` + `fauna-index`);
  `fauna_cbor::Cid` codec-parametric; `SignedEnvelope` stays 36 bytes.
- **Track A — pre-identity connection complete (A1–A5 landed 2026-05-23):**
  the anonymous endpoint `GET /api/v1/ws` (`routes::ws_anonymous_handler`),
  the fixed allowlist (`pre_identity_allowlist.rs`), the
  `fauna.protocol.unauthenticated` gate, and the auth-bootstrap / discovery /
  registration / claim / invite / storage-mode kinds, each reusing a shared
  `*_core.rs` ceremony. The shared `fauna-onboarding-machine` is the first
  client consumer (`nest_api::WsNestApi`). **Every onboarding + auth HTTP twin
  is deleted:** `auth/{challenge,verify}` (rip-out), `setup-status` +
  `storage-mode` (S4c2), `invite-code/verify` (S4b), `invite-requests*`
  (S4a2), `claim-admin` (S4d), `register` (S4f), the four discovery reads
  `node-info` / `handle-available/{h}` / `resolve-node/{d}` /
  `actor/by-handle/{h}` (rip-out endgame), and last `POST /api/v1/auth/token`
  (**deleted 2026-06-13** once every app minted over
  `fauna.auth.handshake` — the native UniFFI apps via the shared
  `fauna-ffi` `mint_bearer` export, a wrapper over
  `mint_bearer_over_handshake`; no client path mints over HTTP anymore).
  The register / invite / claim per-IP throttles were restored as dispatcher
  gates keyed on the real client IP (PROXY-v2). The last deferral closed
  2026-09-24: the dispatcher hands a handler its connection's recorded peer
  (`dispatch_core::current_caller_ip`), so both bearer mints —
  `fauna.auth.handshake` and `fauna.auth.verify` — run new-IP detection on
  the WS path (`login.md` § the handshake's side effects).

> **The mode commits are single-use (2026-08-31) — implemented; the nest
> binding is NOT.** § Pre-identity (anonymous) connection now states
> single-use as the property of every signature-as-auth kind on that
> connection. The half that is **built**: `mode_commit::validate_mode_commit`
> consumes the verified signature through the same
> `auth_core::ReplayGuard` direct auth uses, after the `is_admin` gate, at a
> 2× freshness-window TTL — so `fauna.setup.nat_mode` refuses an exact
> re-submission, opaquely (its retired byte-twin `fauna.setup.storage_mode`
> did the same until it left the wire 2026-09-24).
> Regression-pinned by `bins/fauna-nest/tests/nat_mode_api.rs::a_consumed_nat_mode_signature_cannot_move_the_posture_again`
> (the replay must not move the posture *or* restart the perimeter SMTP
> parser) with `a_freshly_signed_re_set_is_not_a_replay` as the honest-admin
> control.
>
> The **nest binding is built too (2026-08-31)** — the V2 form § Pre-identity
> (anonymous) connection ratifies (its *Binding the nest* block). The design
> fork this entry used to carry resolved as neither of its two candidates: the
> client does not need to *already hold* the nest identity (the pin/held-root
> sources that can legitimately be absent), because a **possession-proven
> read off the commit connection itself** (`nest_trust::read_login_binding`,
> the `fauna.auth.nest_handshake` cert-binding leg with a fresh nonce) learns
> exactly the key the far end holds, on every dial shape — the
> WebPKI/plaintext graduation short-circuits gate channel *trust*, not
> learnability. Built: `SETUP_NAT_MODE_V2` (`sig_domain.rs`),
> `nat_mode_signed_message` + the required `NatModeRequest.nest_id`
> (`fauna-protocol::nat_mode`), the own-identity check + nest-bound verify in
> `nat_mode_core::commit_nat_mode_core`, and the sign-on-connection seam
> (`WsRpcNestApi::submit_nat_mode` — `NestApi::submit_nat_mode` takes secret +
> mode, binding through the one shared `read_login_binding` reader). Pinned by
> `bins/fauna-nest/tests/nat_mode_api.rs`:
> `a_v2_blob_bound_to_one_nest_is_refused_by_another` (PROBE-482-B closed),
> `a_v1_signature_cannot_be_promoted_to_v2_by_stapling_a_nest_id`, and
> `an_unbound_v1_blob_is_refused_at_every_nest` — the sanctioned transition
> window (V1 verified alongside V2, gated by a `NestHandshakeReply.nat_mode_v2`
> advert) was flipped deliberately to that refusal on 2026-09-24 by the
> compat-remnant sweep (`version-compatibility.md` § Dimension 2, the fourth
> write-off).

## Reading list

In priority order:

1. `principles.md` — engineering priority (long-term uniformity, shared Rust).
2. (design ratified 2026-05-04; tracked internally) — full design rationale, all four sub-sections.
3. `libs/fauna-protocol/schemas/README.md` — CDDL authoring rules.
4. `libs/fauna-protocol/src/{envelope.rs,dispatcher.rs}` — wire types and dispatcher.
5. `libs/fauna-client/src/client.rs` — public façade for application-layer use.
6. `bins/fauna-nest/src/{ws.rs,rpc_router.rs}` — server-side dispatch.
7. RFC 8949 §4.2.1 — canonical CBOR encoding rule.
8. IPLD DAG-CBOR spec — additional restrictions on top of RFC 8949.
9. ATproto firehose spec — prior art for `seq`-numbered push events.
10. NIP-01 (Nostr) — anti-pattern reference for kind-namespace coordination at scale.
