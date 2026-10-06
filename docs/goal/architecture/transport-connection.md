# The WS connection — establishing, holding and closing one — target state

Owns: transport-connection
Status: ratified — split verbatim out of `transport.md` on 2026-09-06; the connection state machine, its shutdown path and the anonymous dial are built and running on all seven apps, with the per-app gaps named in their own paragraphs
Authority: **the WS connection itself, on both ends** — how one is established and re-established (the dial, the establish probe, the four-state supervisor, the close-code table and which codes are terminal, the reconnect/backoff rules, the per-app connection-status surface, and the **principal session** — the token-bearing connection a third-party principal opens, its upgrade gate and its teardown), how one is **closed cleanly** (graceful shutdown, the drain and its deadlines), and the **pre-identity (anonymous) connection** (which kinds a Layer-0 dial may carry, the anonymous listener's own timeouts and socket caps, the per-key and global throttles and the aggregate backstops they are sized against). **NOT owned here** — the frames and request/reply semantics carried *over* a connection (wire format, request lifecycle, idempotency and reconnect-with-resume, cancellation, push events and `seq`, backpressure), the namespace policy, forward-compat mechanics, crate map, telemetry and test surface → [`transport.md`](transport.md); byte-level wire contract → [`serialization.md`](serialization.md); HTTP endpoint classification → [`api-layers.md`](api-layers.md); the federation channel mechanism → [`federation.md`](federation.md); compat policy → [`version-compatibility.md`](version-compatibility.md). On conflict in those domains, raise it.

Split verbatim out of [`transport.md`](transport.md) on 2026-09-06 — that doc had
reached **228,978 B**, ~10 days from the 262,144 B whole-file read ceiling, and
three of its sections were one contiguous concept carrying 66% of its weekly
growth: the connection's lifecycle, its graceful shutdown, and the anonymous
variant of it. What stays there is everything that travels *over* a connection.
A routing stub remains at the original location; prior history:
`git log --follow docs/goal/architecture/transport.md`.

> **Reading this doc.** Its text was carried **verbatim** out of
> [`transport.md`](transport.md) on 2026-09-06, so an unqualified `§ <name>`
> citation inside it may name a section that is no longer a sibling on the page.
> `§ Request lifecycle`, `§ Backpressure and \`ResyncRequired\``,
> `§ Namespace policy`, `§ Design decisions worth knowing` and
> `§ Implementation status` all stayed in [`transport.md`](transport.md);
> resolve any other unqualified name there first.

## Section map

| Section | What it holds |
|---|---|
| § Connection lifecycle | The dial and the establish probe, the four-state supervisor and its transitions, the close-code table and which codes are terminal, reconnect and backoff, the proxy and redeploy cases, the per-app connection-status surface, and *The principal session* — the token-bearing connection a third-party principal opens. |
| § Graceful shutdown | The clean close path on both ends — the drain, its deadlines, and what a client may assume when it sees one. |
| § Pre-identity (anonymous) connection | The Layer-0 dial: which kinds it may carry and which it must refuse, the anonymous listener's own timeouts and socket caps, the per-key and global throttles, and the aggregate backstops those constants are sized against. |

## Connection lifecycle

Subprotocol-validated WS upgrade at `GET /api/v1/ws/{actor_id}`:

- Header `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>` required. Wrong
  subprotocol → **HTTP 426** (`StatusCode::UPGRADE_REQUIRED`) rejected
  *before* the WS handshake completes, never a WS close frame — axum's
  `WebSocketUpgrade` cannot set a custom close code pre-upgrade
  (`bins/fauna-nest/src/routes.rs::ws_handler`; the anonymous
  `ws_anonymous_handler` and `federation_channel.rs`'s handshake share the
  identical pre-upgrade-426 shape). Missing subprotocol header → 401.
- Token validated against `state.auth.token_store`; mismatch with `actor_id` → 403.
- Server echoes `fauna.v1` back via `ws.protocols(["fauna.v1"])`.

**The client side of that 403: a client proves its two identity sources agree
before it ever dials.** The bearer names one actor and the URL path names
another only because a client can be assembled from *two independent* identity
lookups — a keypair (which becomes the path actor) and a separately-supplied
bearer mint (linux and tui hand in a `LaunchMachineBearer`, whose secret came
from persistence). Nothing structurally ties them together, so a bug in either
lookup produces a client that is refused `403` on *every* upgrade attempt while
the reconnect supervisor backs off and the app simply renders nothing — a total
blackout whose only evidence, until 2026-08-19, was a *nest-side* log line.
So the pairing is checked where it is made: `AuthClient::with_bearer_source` and
`bearer_only` compare the path actor against
`BearerSource::bearer_actor_id()` and, on disagreement, log the two actor ids
at `error` and expose the fact as `AuthClient::identity_mismatch()`. A source
that cannot attribute its token (a `StaticBearer`) returns `None` — "cannot
say", never "mismatched" — so the check stays silent for test fixtures. This is
defence in depth, not a correctness mechanism: a correct client never presents
a mismatched bearer, and the causes are fixed at their own layer — the worked
example being the 2026-08-19 fix that serialized the credential store's
read-modify-write, after an unserialized one let a straddling writer restore a
signed-out actor's stored index and secret.

Close codes (custom in WebSocket private range 4000–4999):

| Event | Code | Effect on client |
|---|---|---|
| Clean disconnect (logout) | 1000 | Reconnect loop stops |
| Server shutdown | 1001 | Reconnect with backoff |
| Auth expired / invalid | 4401 | `auth.clear_token()` → `ensure_auth()` → reconnect |
| Invalid frame / protocol violation | 4400 | Reconnect with backoff (likely a bug) |
| Internal server error | 1011 | Reconnect with backoff |
| Backpressure overflow on Reply | 1011 | Reconnect; pending requests fail with `RpcDisconnected` |
| Subprotocol mismatch | 4426 | Terminal — reconnect loop exits, no retry (see caveat below) |

**4400 and 1011 are emitted for real as of 2026-07-31; 4426 is reserved (never emitted, by construction — see caveat below).**
`RpcConnection` carries a `fatal_tx` watch holding a `FatalCloseReason`
(`ProtocolViolation` → 4400, `ReplyOverflow` → 1011); the inbound dispatch loop
sets it on a Reply/Push-from-client, a text frame, or an undecodable frame, the
three Reply-carriage sites set it when the bounded outbound channel is full, and
`routes::close_fatal` turns it into the frame. It rides beside — never merged
into — the existing `revoked_tx`, which outranks it in the send task's `biased;`
order: revocation is an authority control, a fault is not.

Two notes for anyone changing this. **First reason wins**: a later
`signal_fatal` is a no-op, because a violation makes the peer stop reading,
which then overflows the Reply channel — reporting the consequence would blame
the nest for the client's bug. **A Reply overflow is fatal, a Push overflow is
not**: the latter degrades to ResyncRequired (§ Backpressure), and conflating
them would tear down connections for ordinary push pressure. Until this landed,
the overflow site logged *"connection will be torn down"* and tore down nothing,
so the caller waited out its full deadline for a Reply already dropped.

Emitting the codes was never a *functional* fix for the first two rows — the
client's `ReconnectSignal::from_close` (`libs/fauna-ws-substrate/src/adapter.rs`)
buckets 4400, 1011, 1006 and a missing close frame identically into the same
`Retry`-with-backoff path — but it closes a real diagnostic gap: an admin
reading nest logs or a wire capture can now tell "this client is sending
garbage" from "the network blipped". Pinned by
`bins/fauna-nest/tests/ws_close_codes.rs` (real WS client, asserts the actual
`u16`, plus a nest-side negative control proving an ordinary disconnect is *not*
labelled a violation) and the `reply_overflow_*` unit tests in `ws.rs`.

**The `4426` code is reserved but structurally unreachable, by design — the
terminal behavior it names is real, driven from the pre-upgrade HTTP layer
instead.** A subprotocol mismatch is rejected at the pre-upgrade HTTP layer
(above) before any WS ever opens, so a live connection closing with code 4426
is not a path the server can reach — axum's `WebSocketUpgrade` cannot set a
close code before the handshake completes. Rather than restructure the server
around that constraint (accept the upgrade just to immediately close it,
changing the wire behavior other clients/tests assume), the client classifies
the pre-upgrade rejection directly: `map_ws_connect_err`
(`libs/fauna-client/src/ws_adapter.rs`) maps HTTP `426` on the upgrade
response to `NestClientError::SubprotocolMismatch` — the same error an
already-open connection closing with `4426` would produce
(`ReconnectSignal::SubprotocolMismatch`,
`libs/fauna-ws-substrate/src/adapter.rs:46-47,60`) — and
`ClientChannel::connect_error_is_terminal`
(`libs/fauna-client/src/reconnect.rs`) treats it the same way an identity
successor treats a superseded account: the reconnect loop exits immediately
(`SupervisorError::TerminalRefusal`) instead of backing off and retrying
forever at the 60 s ceiling. No retry or bearer refresh can fix a genuine
version skew, so failing fast is correct regardless of which of the two
mechanisms (pre-upgrade 426, or a hypothetical post-upgrade 4426 close)
produced it. Proven against a **real** upgrade rejection — a raw TCP listener
answering HTTP 426, not a synthesized enum variant
(`ws_adapter::tests::map_ws_connect_err_classifies_a_real_426_upgrade_rejection`)
— plus `reconnect::tests::subprotocol_mismatch_is_terminal` for the terminal
classification; the synthetic-close-frame test
(`supervisor.rs::supervisor_exits_on_subprotocol_mismatch`) remains as the
regression pin for the close-frame code path, should the server ever grow a
post-upgrade 4426 emission (e.g. a future protocol version negotiated
mid-connection).

**Client-side teardown on logout / re-auth (the `1000` row is a client requirement).**
Clearing or replacing the authenticated session — logout, or re-auth to a
different nest — MUST dispose the prior WS-RPC client so its reconnect loop and
connection-state pump actually stop; otherwise the old client leaks,
auto-reconnecting to the now-gone nest forever (a per-logout/per-nest-switch
leak). Rust (linux) and the wasm/JS client (web) get this from `Drop` / GC when
the client field is reassigned; the C# windows shell has no deterministic
destructor, so it disposes explicitly (`App.DisposeNestClients` →
`NestRpcClient.DisposeAsync` → `FfiNestClient.Disconnect()` + `Dispose()`) on
reset / logout / node_url re-auth.

**Revocation teardown — losing authority closes the socket.** The bearer is
validated exactly once, at the upgrade, and the actor is then baked into
`RpcConnection` for the connection's lifetime; `dispatch_core` never re-reads the
token store. So revoking an actor's tokens governs only its *next* connection.
Every path that strips an actor's authority — `fauna.sessions.lockout` and its
no-token HTTP twin, `fauna.admin.users.suspend` (live and drained from the
legacy pending queue), the eviction ladder's `warning → suspended → deleted`
transitions, the pending-action executor's account deletion and admin
role-removal/role-change arms, **the identity-succession ceremony** (the
retired identity, `fauna.recovery.succession.submit`), and **bridge
service-user revocation** (all three sites:
`fauna.bridges.revoke_service_user`, `reject_pending_bridge`, and the admin
HTTP route) — therefore calls **`WsState::disconnect_actor`** alongside
`TokenStore::revoke_actor` (the shared helper is
`AppState::revoke_actor_authority`; a few older sites perform the same halves
inline where they need a specific interleaving). Neither half alone suffices:
tokens stop the next connection, `disconnect_actor` closes the ones already
open.

> **The succession ceremony is the sharpest member of that list, and it was the
> last to join it (2026-08-23).** It did the token half alone until then, on a
> comment reasoning that a bearer outliving the sweep "still cannot
> re-authenticate, and the consult refuses it on its next handshake" — true, and
> beside the point: **a socket the thief already holds never has a next
> handshake.** Nor does the per-RPC authority gate catch it, because the ceremony
> deliberately *keeps* the retired `users` row unsuspended and unlocked (it is the
> FK target and quota home of content that still exists), so
> `caller_class_for_actor` goes on resolving the retired identity to
> `Some(CallerClass::User)`; and the supersession consult lives at the mint door,
> not on dispatch. The rule that now holds: a supersession ends the retired
> identity's open connections, not only its credentials. The general rule this pays for: **a
> "revoked" claim is about the sockets that already exist, never only about the
> credentials — check the enumeration above whenever a new authority-stripping
> path is added.**

**A third-party principal's session has a twin of its own (ratified 2026-10-01; built 2026-10-02).** It registers apart from the account's sockets, is swept by `fauna.principals.revoke` and by the ending of the grant family its token names, and is also closed by every per-actor path enumerated above — § *The principal session* → *Revocation — three doors* owns the rule.

For a **bridge** the teardown has a third piece: purge its mailbox-state push
subscriptions (`BridgePushRegistry::remove_mda`). A revoked bridge keeps its
`users` row (approval inserted it; the mint gate reads only
`users.{suspended, locked_until}`), and a Push is not an RPC, so a subscription
that outlived the revocation would keep streaming with no dispatch to gate it.
Purging makes re-arming require the `subscribe_mailbox_state` dispatch, which
the capability gate denies a non-Approved bridge; the other bridge-directed
pushes (`config_changed`, `outbound_ready`, `rescore_ready`) already exclude it
by listing *approved* bridges at emit time. The revoked bridge learns its
status from the teardown
itself: the 4401 forces its reconnect, whose whoami the gate refuses — the Go
bridge maps that refusal to revoked (it can never see a `status=revoked`
*reply* mid-run) and enters the graceful-shutdown → re-key cycle
(`mail-bridge-lifecycle.md` § Service-user re-keying).

It is the per-actor twin of `begin_shutdown` (§ Graceful shutdown): the same
`watch`-flag mechanism, scoped to one actor, but it emits **4401 rather than
1001** and performs **no drain**. The close code is the whole point — `1001`
means *reconnect with the bearer you have*, which for a revoked actor is a dead
credential; `4401` means *clear it and re-authenticate*, and the re-mint then
leads nowhere: the direct handshake (`auth_core::direct_auth_core`) denies a
suspended, locked-out, or unregistered actor outright, while challenge/verify
(`verify_core`) denies a suspended or unregistered one and still mints for a
locked-out actor, whose fresh bearer is refused at its first use (the
**standing refusal** paragraph further down this section). Either way the supervisor backs off rather
than spinning. The drain
is skipped because lockout is an *emergency* control: the socket dies now, not
after the outbound queue flushes. In-flight handlers are left to finish rather
than aborted — each was authorized when it was dispatched, each is bounded by its
deadline, and aborting mid-handler could tear a multi-statement write for no
security gain. A Push queued just before the close is therefore best-effort; the
4401 is the authoritative signal.

**The per-token twin — revoking one SESSION closes that session's sockets
(built 2026-09-20).** Everything above is keyed on the *actor*: the whole
identity loses its authority and every socket it holds dies. `fauna.sessions.
{revoke,revoke_all}` is the per-token twin — one bearer of one actor ends, and
that actor's **other** sessions keep their tokens and their sockets, because
dismissing one session a user does not recognize is not signing them out
(`../behavior/devices.md` § What a session is, and what revoking one does). The
same "validated once, at the upgrade" fact makes it necessary, and until this
landed both arms did the token half alone: the revoked session kept dispatching
as `User` on the socket it already held, and the revoke governed only its *next*
connection. The mechanism is the two-halves rule applied one level finer — the
connection remembers the `token_id` it was upgraded with (`WsState::
subscribe_with_session`, from `TokenStore::validate_with_session`, which is the
only moment the raw bearer and its row are both in hand), and the teardown
selects on that id within the actor's own subscription entry:
`WsState::disconnect_token_id` for *this session*,
`disconnect_actor_except_token_id` for *every session but the kept one*. The
shared helpers that do both halves by construction are
`AppState::revoke_session_authority` and `revoke_other_sessions_authority`,
and the census in `conformance_revocation_teardown.rs` watches every
`TokenStore` revocation method, each against its own teardown — pairing a
per-token revoke with the per-actor `disconnect_actor` would be worse than
leaving it unpaired. A connection with no recorded `token_id` resolves toward
each teardown's conservative arm: never matched by *end this session*, always
closed by *end all but this one*.

**The upgrade window — the per-token revoke is re-read once at registration
(closed 2026-09-22).** The teardown sweeps `WsState.subs`, and a connection
enters it only in `handle_ws`, *after* the 101, while its bearer was validated
*before* the 101 in `ws_handler`. A per-token revoke landing between the two
finds nothing to close, and the connection then registers carrying the revoked
`token_id`. For every per-actor path that straggler is still caught — the
dispatch gate re-reads the *actor* on every RPC (§ *complements, not
substitutes*, below) — but a per-token revoke changes nothing about the actor,
so the gate returns `User` and the straggler would dispatch for ever. So the
registration re-reads the session itself: `AppState::register_upgraded_connection`
subscribes the connection and **then** asks `TokenStore::has_session`, revoking
the connection when the row is gone (it closes 4401 before its first dispatch —
`RpcConnection::revoke` stores the flag, and `run_connection` checks it before it
reads a frame). **One read is total because of an ordering both sides keep:**
every sub-actor revoke helper (per-token and per-device) deletes the rows
strictly before it sweeps, so either the row
is already gone when the read runs, or the delete — and therefore the sweep —
has not happened yet and will find the connection in `subs`. Moving the re-read
before the subscribe, or a helper's sweep before its delete, reopens the window.
It is one read per upgrade, never per RPC: teaching the dispatch gate to read
the token store would undo "validated exactly once, at the upgrade" on the hot
path of every connection. `has_session` ignores expiry (a socket outliving its
bearer's hour is the design), and a connection with no recorded `token_id` has
no row to re-read and is never closed by it. The read also closes the
registration window of every other door that deletes token rows, but it is no
substitute for a door's own live-socket teardown, which only the sweep provides.
Pinned by `routes.rs::upgrade_revocation_race_tests` on each half: the window and the re-read's place after the subscribe (a real upgrade parked just above the subscribe while the whole revoke runs, and the re-read itself parked once it has answered, so a re-read anywhere before the subscribe answers on a still-present row the sweep has already passed),
and each helper's delete-before-sweep order (the upgrade registers after the helper's sweep and before the rest of it, where a sweep-first helper would still hold the row).

**The connection also remembers which DEVICE minted its bearer (built
2026-09-22).** The same validate-once fact makes a third field necessary: the
token row's `minted_by_device` — the renewal device key of a
`fauna.auth.device_handshake` mint — is read at the upgrade beside `token_id`
and kept as `RpcConnection::bound_device_key` for the connection's lifetime,
because a read-time join through the token store would paint a still-connected
device offline the moment its row expired. The custody handshake's tag (the
custodian's own actor id) is excluded at the upgrade (`ws::bound_device_key_for`),
and a seed-minted bearer carries none. The one consumer is
`fauna.sync.devices.list`'s `online`, whose rule — what binds, what does not,
and the actor-scoped join — is owned by `../behavior/devices.md` § Listing
Devices → *The binding*; the registration also marks the device's row
`last_seen`. Pinned by `conformance_device_online.rs`, including a real
upgrade over a socket.

**The per-device twin — removing a DEVICE closes the sockets its key minted
(built 2026-09-23).** Device removal revokes every session the device's renewal
key minted (`apps/sync-agent-credentials.md` § Credential model, decision 2),
and it has three doors: `fauna.sync.devices.delete`,
`fauna.sync.device_grant.revoke`, and graduation's severance of a marked
guardian device (`fauna.family.graduate` — that device authenticates as the
ward). All three did the token half alone until then, so the thief a device
deletion targets kept dispatching as `User` on the socket it already held: the
dispatch gate reads actor state, which a device removal leaves healthy. The
teardown selects on the connection's `bound_device_key` within the actor's own
entry (`WsState::disconnect_device_key`), and the helper every door calls does
both halves by construction, rows first (`AppState::revoke_device_authority`).
Selecting on the binding rather than on the removed `token_id`s also closes a
socket whose bearer row had already expired. The actor's direct sign-ins and
other devices keep their tokens and sockets.

**WS-RPC is the only socket a revocation has to reach (2026-10-02).** Until
then the `/sync/ws` data plane registered its seats in a registry of its own,
and every helper above swept it beside `WsState`. That data plane was removed
with the native sync daemon that was its only dialer (`../behavior/file-sync.md`
§ Relay serving → *The `/sync/ws` data plane leaves with the daemon*), so each
revocation helper in `routes.rs` — `revoke_actor_authority`,
`revoke_session_authority`, `revoke_other_sessions_authority`,
`revoke_device_authority`, and `AppState::close_actor_sockets`, which the sites
doing the per-actor halves inline call (the pre-identity emergency lockout,
suspension, the eviction ladder) — closes WS-RPC connections only. An announced relay seat
(`fauna.sync.serve.announce`) lives on its WS-RPC connection and dies with it.
The ordering rule is unchanged: every sweep runs after its token rows are
revoked. Pinned by `conformance_revocation_teardown.rs`.

**The one-frame grace — a handler that revokes its OWN caller still answers that
request.** Every path in the enumeration above strips the authority of somebody
who is not asking: an admin suspends a user, the ladder evicts an account, the
executor deletes one. For those the 4401 really is the whole message, and
dropping what was queued behind it costs the client nothing it can act on. The
identity-succession ceremony was the exception, and until 2026-09-20 the only
one: it retires the identity whose socket carries it, and its Reply —
`new_actor_id` + `succeeded_at` — is the ceremony's *product*, not a courtesy.
So
`fauna.recovery.succession.submit` revokes through the sparing form
(`AppState::revoke_actor_authority_sparing_caller`), which differs from the
ordinary teardown in exactly one respect: the connection the request arrived on
closes 4401 the instant that one Reply is on the wire, instead of before it.
Dispatch stops for it at the same moment as for every other connection — the
inbound `spared_until()` guard drops every further request from that instant,
so it is never a reprieve. The connection itself closes only *after* that one
Reply is on the wire, though, and whatever else was already queued ahead of
it — a Push included — goes out too, all of it still addressed to the
retiring identity, so nothing leaks to a thief's socket. Every **other**
connection the actor holds, which is where a seed thief's socket actually is,
is torn down unsparingly. The handler learns which connection it is from a
dispatch task-local
(`dispatch_core::CallerRef`) rather than from a widened handler signature: one
question, one place, and outside a handler the sparing form degrades to the
plain one. **Any new authority-stripping kind must ask the same question the
2026-08-23 succession fix did not** — *is this path's own Reply something the
client cannot recover without?*

**Both per-token arms answer yes, which is why they spare unconditionally
(2026-09-20).** `fauna.sessions.revoke` is always self-directed — the handler's
ownership check refuses any other actor's `token_id` — so the session being
ended is regularly the connection asking; and `revoke_all` reaches the caller's
own socket through the renewal race `../behavior/devices.md` § The client's own
session documents, where `keep_token_id` names a bearer the app has already
replaced and everything is revoked. That race is called harmless *because the
next request re-mints*, which holds only if the caller gets an answer at all: a
bare 4401 with no Reply is indistinguishable from an unrelated eviction, and the
app cannot tell the revoke committed. So unlike the per-actor form — whose
sparing variant is a separate method because its unsparing sibling is the common
case — the per-token helpers have no unsparing form; nothing ever wants one.

**The per-device doors answer yes too (2026-09-23).** `device_grant.revoke`'s
session-authorized arm and `devices.delete` are both presentable by the very
device being removed, over its own device-minted session — an agent retiring
itself, a user deleting the device in their hand — and `{revoked,
sessions_revoked}` (or `deleted`) is that caller's only evidence the removal
committed; without it the device reads a bare 4401 as an expired bearer and
re-handshakes with the key it just retired. So `revoke_device_authority` spares
the caller unconditionally, like the per-token helpers. For graduation the
caller is the guardian, whose connection is not in the ward's entry, so the
spare matches nothing.

**The signed-in lock answers yes too (2026-10-04).** `fauna.sessions.lockout` is
self-directed by construction — the actor locks its own account, over one of the
sockets the lock closes — and its Reply (`ok` + `locked_until`) is the app's only
evidence the lock committed: [`../ui/sessions.md`](../ui/sessions.md) § User
actions has the app leave its shell for the locked surface *on success*, and an
app that reads a bare 4401 instead paints a failed lock over an account that is
locked. It shipped closing every socket of the actor before its Reply was
written, so the Sessions page's lock never reported success to the app that
pressed it. It now revokes through the sparing form the succession ceremony
uses; every other connection the actor holds still closes at once, with no
drain, which is where the emergency is. The pre-identity twin
`fauna.account.lockout` needs no grace: it arrives on an anonymous connection,
which is not one of the actor's.

Between 2026-08-23 and 2026-09-12 the answer was "no" and the cost was paid in
full. Every app performs the ceremony over its signed-in connection (all seven
wrap the live `NestClient` in the `RecoveryClient`), so **every succession in
that window landed on the nest and returned `succeed_with_held_kit`'s
`Unconfirmed` arm** — the undecidable cell, where the client cannot tell "the
account moved" from "nothing happened" and must go back to the nest over a fresh
pre-identity connection to find out. ⚠ What that cost was **is per-app, and is
not a thing to assume**: measured 2026-09-12, tui and web completed the ceremony
through `ceremony::finish_unconfirmed_succession` — their aftermath journeys
passed on builds carrying the defect. linux's nine aftermath journeys are
**not** this defect's measure: they fail identically with the reply delivered,
for reasons of their own. So the
reply-dropping is client-agnostic and nest-side, and how visible it is depends
entirely on a recovery path whose whole purpose is to be rarely taken. That is
the argument for fixing it here rather than per app: a defect that routes every
user of a ceremony through its own emergency path is a defect even where the
emergency path happens to hold.

Suspension, **lockout**, and **deletion** are each additionally denied at
*dispatch* (`caller_class_for_actor`, re-read per RPC —
`../../behavior/admin.md` § 2 Users → *Cutting a user off*), which is what makes
them bite on a live connection. The gate reads `users.{suspended, locked_until}`
in one query and treats an **absent row** as a denial in its own right, so a
deleted actor's still-open socket resolves to no caller class at all.

Teardown and the dispatch gate are **complements, not substitutes**, and neither
subsumes the other:

- The **gate** closes the upgrade-time TOCTOU that teardown cannot — **for the
  per-actor paths only**. The bearer is validated once, at the upgrade; a bearer
  validated microseconds before `revoke_actor` runs yields a connection that
  registers in `WsState.subs` *after* `disconnect_actor` has already swept it,
  and the gate's per-RPC re-read of the actor denies it. It cannot do the same
  for a **per-token** revoke, which leaves the actor untouched; that window is
  closed at registration instead (§ *The per-token twin* → *The upgrade
  window*).
- **Teardown** closes what the gate cannot. It stops the **Push stream** — a Push
  is not an RPC, so no dispatch gate is ever consulted for one — and it reclaims
  the socket rather than leaving a dead connection open until its next RPC.

**Upgrade-time auth rejection (no close code).** The `4401` row covers a bearer
rejected on an *already-established* connection. A bearer rejected at the **WS
upgrade itself** — the connection never opens, so there is no close code — is
recovered the same way, or the supervisor would back off forever with a dead
token. This is the path a **nest factory-reset** triggers: the reset wipes
nest's token store, so a pinned client's cached bearer is rejected with an HTTP
`401` at the next upgrade (never via a `4401` close). The supervisor detects a
`401`-classified `connect()` failure
(`SupervisedChannel::connect_error_is_auth_rejection`; the client maps a 401
upgrade response to `NestClientError::Api { status: 401 }`), runs the same
`clear_token()` → `ensure_auth()` re-mint **once**, then retries immediately; a
still-rejected fresh bearer falls back to backoff (no refresh→401 busy loop).
That fall-through is also what a **standing refusal** lands on: the upgrade asks
the bearer's actor's standing and answers a suspended, locked-out or revoked actor
`401` too ([`api-layers.md`](api-layers.md) § Layer 1: Core Client API → *What
`caller_class_for_actor` refuses*), and a locked-out actor's re-mint over
challenge/verify still succeeds today, so its fresh bearer is refused again and
the supervisor backs off. The Go bridges' in-process redial (`bins/fauna-bridges/internal/wsrpc`, `Dial`)
applies the same rule for the same reason — a nest restart empties its in-memory
token store — so a bridge re-mints once instead of re-presenting its cached
bearer until that bearer nears expiry.
This is what keeps **factory-reset + re-claim** a client-recoverable transition
(`architecture/nest/common.md` § Client-state recoverability) rather than
stranding the client on a dead WS that silently degrades to deprecated HTTP twins.

The re-mint's `ensure_auth()` only recovers if the underlying `BearerSource`
can actually produce a fresh bearer from whatever state it is in. On a client
that authenticates via the launch machine (`LaunchMachineBearer`), the
post-reclaim relaunch runs the silent challenge while the freshly-reset nest is
still restarting, so that first challenge can fail transiently and park the
machine in `Offline { transient: true }` — a state from which `refresh_token`
(the `fauna.auth.handshake` re-mint) is a no-op, because there is no `Online`
session to refresh. So `bearer()` falls back to re-running the **silent
challenge** (`retry_silent_challenge`, which self-guards to the transient case)
before it gives up; a terminal `Offline` (bad secret, account locked) is left
untouched. Without this, the supervisor's re-mint would `clear_token()` →
`ensure_auth()` → still get nothing and back off forever — the same dead-WS
strand the upgrade-401 handling exists to prevent.

**Wasm client — re-mint on every reconnect (the browser can't see the
upgrade-401).** The native recovery above keys on the HTTP `401` that tungstenite
exposes at the failed upgrade. The **browser `WebSocket` API does not expose the
upgrade response status** — a bearer the nest no longer accepts (e.g. its
in-memory `token_store` was wiped by a redeploy) surfaces only as an ordinary
`1001`/`1006` close, indistinguishable from a network drop. So
`libs/fauna-rpc-wasm`'s `run_reconnect_loop` cannot classify the failure as auth;
instead, once any established connection drops it **force-refreshes the bearer on
every subsequent reconnect attempt** (the cached bearer is used only for the
initial connect). This is the divergence-minimal wasm twin of native's
`connect_error_is_auth_rejection` re-mint: a healthy reconnect pays one extra
(cheap, persisted-key) handshake, while a wiped-token benign flip actually
recovers instead of looping forever on the stale bearer. Proven by
`tests/e2e-unified/tests/test_nest_flip_resilience.py` (web).

**Wasm client — reset the backoff and announce `Connected` only on a *proven*
connection, never on handle creation.** Native's `channel.connect().await`
performs the real TCP/TLS/upgrade, so its `Ok` proves the connection is up and the
supervisor resets its backoff (and re-arms the 401 path) the moment it returns.
The browser `WebSocket` **constructor is synchronous handle creation** — it throws
only on a malformed URL/protocol; a down / refusing / unreachable nest still hands
back a live handle whose failure surfaces asynchronously, later, on the stream. So
`run_reconnect_loop` **cannot** treat `GlooAdapter::connect() == Ok` as
establishment: doing so reset the backoff and fired the SPA re-hydrate callback on
*every* dial against a down nest, making `MAX_BACKOFF` unreachable (a hot
~2-dials/second loop) and storming the SPA's `on_reconnected` fan-out for the whole
outage. Instead the loop **races the dispatcher driver against a short establish
probe** (the shared `fauna_protocol::reconnect::ESTABLISH_PROBE`, 1 s): only if the connection survives the probe
window does it reset the backoff and call `mark_connected`; a handle that dies
first grows the backoff and never announces `Connected`. The backoff curve + the
"survived ⇒ established" decision live in the runtime-agnostic
`fauna_protocol::reconnect` (`Backoff` + `probe_established`), shared with — and
unit-tested on — native, since the wasm-only `fauna-rpc-wasm` crate compiles to
nothing on native and so has no native test surface of its own. Survival is the
dep-agnostic establish signal chosen here (no app-level ack round-trip needed);
its one residual is a blackholed nest (SYN dropped, no close frame), which the browser
holds in `CONNECTING` past the probe and so is counted established once per
browser-connect-timeout cycle — the same "stay Connecting until the OS timeout"
shape native has, and a far rarer case than the refused/redeploy path the fix makes
correct.

**Native sync daemon — the same survival proof, because its dial proved no more
than the browser's** (the record of a dialer removed 2026-10-02 with the legacy
daemon, and the `/sync/ws` route it dialed was removed the same day). The `/sync/ws` data-plane socket
(`bins/fauna-sync`'s `run_ws_mode`) performed a real upgrade, but the nest authorized the device and
folder only after reading the daemon's Hello, and refused by dropping the socket
with no close frame. So `connect` returned `Ok` for a device
the nest was about to refuse. The daemon races its receive loop against the same
`ESTABLISH_PROBE` window (`ws_client::prove_established`), started after its
filesystem watcher so an apply made during the probe still meets echo
suppression, and its reconnect loop clears the ceiling only for a session that
proved itself. A successful bearer re-mint between attempts proves nothing about
the device and leaves the ceiling alone. The pause is the same full-jittered draw
from the shared `JitterRng` the supervisor uses, 1 s → 60 s. Resetting on the
re-mint instead is how a device the nest did not know once redialed every second,
unjittered, for as long as the daemon ran.

> **Implementation status today (2026-07-18) — the establish-probe is
> implemented** (`libs/fauna-rpc-wasm/src/client.rs` `run_reconnect_loop`
> → `fauna_protocol::reconnect::{Backoff, probe_established}`). The two bugs it
> fixes (backoff reset on synchronous handle creation; `mark_connected` firing
> before the socket is open) were **undocumented until this landed**, which is why
> they survived review across multiple sessions. The shared policy carries native
> unit tests (`fauna_protocol::reconnect::tests`, load-immune paused-future races);
> the wasm loop itself still verifies only in a browser (full web e2e regression),
> deferred to a quiet machine per `web-e2e-session-timeout-under-load`.

Heartbeat: **both ends ping, and each detects a dead link independently.** The
cadence is one pair of constants shared by the two halves —
`fauna_ws_substrate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT}` (30 s Ping, 60 s
dead-link = 2× the interval) — so they cannot drift apart. This keeps the
connection warm across idle-timeout devices on the path (NAT, firewall, a future
reverse proxy) and detects a half-open link far sooner than the OS-level TCP
keepalive (~2 h). tungstenite/axum **do not** initiate periodic pings — they only
auto-respond to a *received* Ping with a Pong at the framing layer — so each
periodic Ping is sent explicitly by the side that wants it.

- **Client half.** The native adapter
  (`libs/fauna-ws-substrate/src/adapter.rs`, `TungsteniteAdapter::poll_next` —
  shared by the bearer client channel and the nest↔nest federation channel) emits
  the Ping and performs **dead-link detection**: any inbound frame (the Pong, or
  any other traffic) re-arms a liveness deadline, and if no frame arrives within
  `KEEPALIVE_TIMEOUT` the stream ends with `Retry` so the supervisor reconnects
  proactively rather than waiting for an RPC to time out.

  Both halves of that detection live on the **Stream** side — the Sink is a pure
  passthrough — so it holds only while something keeps polling the stream. That
  is a constraint on the dispatcher driver, not just on the adapter:
  `RpcDispatcher`'s driver therefore polls its write half and its read half
  **concurrently**, never as two arms of one `select!` whose outbound arm body
  awaits the socket. A driver that blocks reads while a write is outstanding
  disables its own dead-link timer for exactly as long as the peer withholds its
  receive window — which is unbounded, and is what a peer would do on
  purpose.
- **Server half.** The nest does the mirror image on **every** WebSocket it
  serves: a Ping every `ping_interval`, and a deadline that **any** inbound frame
  re-arms. A full `liveness_timeout` of silence ends the connection as the
  disconnect it is — the loop `break`s (or the adapter's stream ends) exactly as
  it does on a peer close, so no new close code reaches the wire, and the event
  logs at `warn` (a connection reaped in silence is indistinguishable from a
  quiet night). Cadence is `WsHeartbeatPolicy` on `WsState`, defaulted from the
  shared constants; tests inject a compressed clock.

  It takes **three shapes**, one per connection-loop structure, because where the
  Ping can be emitted follows who owns the socket's sink:
  - **Two-task loops** — `routes.rs`'s `run_connection`
    — put the Ping in the outbound task and the deadline in the inbound one,
    because the sink is owned by a separate task by necessity.
  - **Single-task loops** — the nostr relay endpoint, `nest_link`'s proxy, and
    `degraded_serve` — own both directions, and drive both halves from one
    `tokio::select!` arm via the shared `ws::ServerHeartbeat` helper.
  - **Adapter-driven channels** — the sidecar channels and the federation
    listener hand their socket to a `RpcDispatcher`, so the heartbeat lives in
    `federation_channel.rs`'s `WsMessageAdapter`, the listener-side mirror of
    where the client half lives. A dead peer ends the adapter's stream, which is
    how that layer says "torn down" to the driver.

**Why the server half is a Ping and not an idle timeout.** The tempting cheap
version — "no inbound frame for N seconds ⇒ close" — is wrong for precisely the
reason § Abuse posture records at the L4 layer: quiet is legitimate, and a
**browser** client is quiet by construction (it has no ping primitive; see the
sanctioned exception below). An inbound-idle rule would therefore cut every idle
browser tab. A Ping does not, because RFC 6455 makes the Pong mandatory and the
browser's WS stack answers it *below* JavaScript: the nest asks, and every live
peer answers whether or not its application has anything to say. The distinction
is pinned by a matched pair of tests in
`bins/fauna-nest/tests/ws_server_heartbeat.rs` — the second one fails against an
idle-timeout implementation, which is its whole purpose.

The **wasm/browser client is a sanctioned exception** (priority #1 divergence,
recorded here): the W3C `WebSocket` API exposes no ping primitive, so
`libs/fauna-rpc-wasm` cannot send a protocol-level Ping. It relies on the
browser's / OS's own connection management. If a web idle-drop is ever observed
in practice, revisit with an app-level keepalive (e.g. a periodic no-op RPC);
until then the protocol-level Ping is native-only by design.

> **Implementation status today (2026-05-29; substrate-relocated 2026-06-02;
> server half added 2026-08-01, extended to every remaining endpoint the same
> day): both halves are implemented, on every WebSocket the nest serves.** Client
> half in `libs/fauna-ws-substrate/src/adapter.rs` (`TungsteniteAdapter::poll_next`
> drives a `tokio::time::Interval` that emits a `Message::Ping` every
> `KEEPALIVE_INTERVAL`, plus pong-timeout dead-link detection via
> `KEEPALIVE_TIMEOUT`).
>
> **The two adapter-driven halves share one loop (2026-08-31).** They are listed
> separately above because they sit on different sockets, not because they are
> different code: `fauna_ws_substrate::poll_ws_frames` *is* both
> `TungsteniteAdapter::poll_next` and `WsMessageAdapter::poll_next`, and
> `HeartbeatDriver` is the clock it drives. What stays per-side is only the
> message vocabulary, behind the `WsFrames` trait — the two transports bring
> genuinely different `Message` enums (`tokio-tungstenite`'s on a raw socket,
> axum's on an upgraded one), and only the client side has a reconnect supervisor
> to hand a close-derived `ReconnectSignal` to. Cadence, best-effort Ping
> emission, liveness re-arming and the dead-link end are therefore one
> implementation, and cannot drift apart the way two copies could.
>
> The wasm transport sends no Ping by design (no browser
> primitive; see the sanctioned exception above) and is covered by the server
> half, which is why that half had to be a Ping rather than an idle timeout.
>
> Server-half coverage, endpoint by endpoint — each row's *peer population* is
> the question that had to be answered before it could reap anything, and each is
> answered by a test rather than by assumption:
>
> | endpoint | shape | peer | pinned by |
> |---|---|---|---|
> | `/api/v1/ws{,/{actor_id}}` | two-task (`routes.rs` `run_connection`) | our apps, native + browser | `tests/ws_server_heartbeat.rs` |
> | `/nostr` | single-task (`nostr/relay_endpoint.rs`) | **arbitrary third-party Nostr clients** | `tests/nostr_relay_interop.rs` — a real `nostr-sdk` client held idle across many liveness windows, plus a raw vanished-peer socket |
> | `/internal/worker/ws` | single-task (`nest_link/proxy.rs`) | our own worker (`nest_link/client.rs`) | `tests/nest_link_ws_server_heartbeat.rs` |
> | degraded "needs-update" listener | single-task (`degraded_serve.rs`) | our apps, native + browser | `tests/degraded_ws_server_heartbeat.rs` |
> | sidecar channels (`bridge`/`algorithm`/`dns`/`relay`) | adapter (`WsMessageAdapter`) | the Go mail bridge, and our Rust sidecars | `federation_channel.rs`'s adapter tests; the Go bridge's Pong by `bins/fauna-bridges/internal/wsrpc/server_heartbeat_test.go` |
> | `/api/v1/federation/ws` | adapter (`WsMessageAdapter`) | another nest | as above — it now has its own half too, which is what "both ends ping" asks for; it was previously left to the dialer's |
>
> Two of those answers were worth the trouble of establishing. The **nostr**
> endpoint is the only surface facing software we do not ship: RFC 6455 makes the
> Pong mandatory, but "the spec requires it" is the same reasoning that produced
> the UA-less ActivityPub interop outage, so it is pinned by the real-client
> harness instead — and the pin is a `RelayStatus`, not a reconnect counter,
> because a real client reconnects on its own and would otherwise mask a reap.
> The **Go mail bridge**'s Pong is likewise pinned by a Go test rather than read
> out of its WebSocket library, whose source is not ours to read.
>
> `nest_link`'s proxy is the one that had a liveness timer *before* this and was
> still broken by it: an app-level `ProxyCommand::Ping`/`WorkerMessage::Pong`
> pair detected the dead worker and then left every task holding the socket,
> because the three per-connection tasks were joined by `tokio::select!` over
> their `JoinHandle`s and **dropping a `JoinHandle` does not abort its task**. It
> now runs the standard mechanism in a single task; the app-level wire variants
> remain for older workers, which answer them but never depend on receiving them.
>
> The 2026-05-29 note that this might fix the specific example.com enable-mail
> freeze was never confirmed and still needs a live VPS repro (human + VPS) —
> example.com's :443 is fronted by the `fauna-sni-router`, which terminates no TLS
> and imposes no idle timeout of its own on the spliced stream, so a
> TLS-terminating-proxy idle timeout is not the cause for it as deployed.

Reconnect supervisor in `libs/fauna-ws-substrate/src/supervisor.rs`
(`run_supervisor`, shared by the bearer client + federation channels):
exponential backoff with a **1 s → 60 s ceiling**, **full-jittered** — the actual
sleep is a uniform-random draw in `[0, ceiling]`, not the ceiling itself. The
ceiling still doubles between attempts; only the sleep is randomised. This
decorrelates the **thundering herd** a nest redeploy creates: every connected
device drops at the same instant with its ceiling reset to 1 s, so without jitter
they would all reconnect in lockstep against the just-booted, cold-cache nest.
The 60 s cap is kept (not lowered) for client↔nest because the worst-case wait
only bites after a long *outage*, not a redeploy (where the ceiling is still 1 s),
and a lower cap would only raise the attempt rate against a still-down nest. The
supervisor updates the `connection_state()` watch channel on every transition.
The client supplies its half — bearer-subprotocol connect, push bridge, 4401
bearer refresh — as a `SupervisedChannel` impl (`libs/fauna-client/src/reconnect.rs`,
`ClientChannel`).

**Every such update is a `send_replace`, never a `send`.** The channel carries a
*state*, not an event: a transition published while no receiver happens to be
alive must still be there for the next subscriber to read. `watch::Sender::send`
does the opposite — it reports `Err` and leaves the stored value untouched — and
the window is not hypothetical, because the supervisor is spawned *before*
`connect()` subscribes and `NestClient` keeps no receiver of its own in between.
Publishing through `send` is therefore how a client comes to render
"Disconnected" over a live, serving connection, and how one torn down by
`disconnect()` keeps rendering "Connected"; under CPU contention the first of
those was reproducible on demand. The rule binds every
publication site — the supervisor's own transitions and `NestClient::disconnect`
alike.

**The ceiling clears on a *proven* connection, never on the dial.** A transport
connect is not yet a connection: `on_connect` — the per-connection serving setup,
the client's push bridge or the federation `hello` — can still refuse it. So the
backoff resets at the same point `consecutive_failures` does, once the connection
has actually served, and a channel that dials fine but fails its setup backs off
on the ordinary curve like any other failure. Resetting a step earlier, on the
dial, is **not** a smaller version of this rule but the negation of it: every
iteration wipes the growth the previous failure just applied, the `grow()` on
that path becomes dead code, and the loop dials forever at the initial 1 s rate
with every constant above still reading as correct. A redeploy is unaffected —
it drops a *proven* connection, so the ceiling it restarts from is 1 s either
way, which is the whole point of the herd-spreading jitter.

**While a request waits for the connection, a refused dial is retried within the
initial ceiling.** The curve paces an *idle* reconnect, and it grows fast: three
refused dials put its ceiling at 8 s, longer than a read's whole 5 s deadline. A
request parked in the in-gap wait ([`transport.md`](transport.md) § Request
lifecycle, step 3) during a gap of a few seconds could therefore spend its
deadline while the nest was already back and the supervisor still asleep, and
fail with the "connection lost" error a passing gap must never raise. So the
supervisor keeps a count of parked requests (`fauna_ws_substrate::DialDemand`,
which `NestClient::wait_for_connected` holds for as long as it parks). While the
count is non-zero, the nap after a refused dial is a jittered draw within the
initial ceiling (1 s, or an e2e pace override's initial), and a request that
starts waiting cuts a longer nap short. These extra dials belong to the waiter,
not the curve: a refused one neither grows the ceiling nor counts toward the
`Unreachable` run, and the curve's own next dial keeps its due time. So once
nothing waits, the idle pace and the "Cannot connect" threshold are exactly what
they would have been. The extra rate is bounded by the waiting itself, since every
request's wait ends at its own deadline. It applies to the refused-dial path only.
A dial that connects but fails its per-connection setup, a 4401, and a dropped
connection keep the curve, so a waiting request can never re-create the
setup-failure dial storm below. Pinned by
`supervisor::tests::a_waiting_request_is_dialled_for_within_the_initial_ceiling`,
`…::a_request_that_starts_waiting_cuts_a_grown_nap_short`,
`…::demand_dials_neither_grow_the_curve_nor_count_toward_unreachable` and
`client::tests::a_request_waiting_out_a_gap_counts_as_dial_demand`. The wasm
reconnect loop (`fauna_rpc_wasm::run_reconnect_loop`, web) is the same rule's
other implementation: `dispatcher_within_deadline` counts as a waiter while it
parks, and both loops draw the demand nap from one function,
`fauna_protocol::reconnect::demand_nap`. On wasm a refused dial is mostly a
socket that died inside the establish probe, since the browser's `WebSocket`
constructor cannot fail on a down nest. So the loop keys the rule on the
establish verdict: a handle that never came up is a refused dial, while a proven
connection that later drops keeps the curve. The wasm crate's tests of the same
four names pin it.

**A nest that answered no is not a gap: after a refusal it *answered*, waiting requests do not hurry the next dial.** Dial-on-demand exists to catch a nest the moment it comes back. A `401` on the upgrade (after the one re-mint the upgrade-auth rule allows) or a `429` means the nest is up and refusing this client, so the supervisor sleeps out its own curve even while requests wait (`SupervisedChannel::connect_error_is_answered_refusal`; the client answers it for an upgrade `401` or `429`). Without this a dead credential with a request always parked redialled within the initial ceiling for as long as requests kept arriving — a dial a second, not one per ceiling interval. Pinned by `supervisor::tests::an_answered_refusal_gets_no_demand_dials`.

> **Implementation status today (2026-10-06): dial-on-demand on all 7 apps;
> on web the answered-refusal exemption covers only the answers a browser can
> see.** The wasm loop keeps the curve after a 4401 close. It stops for good
> on the `not_registered` mint refusal, the one mint refusal it can type. But
> the browser hides an upgrade's HTTP status, so on web an upgrade `401` or
> `429` reads as a refused dial, and a waiting request still hurries it. Each
> request's own deadline bounds that rate, and web re-mints its bearer before
> every reconnect anyway (§ *Wasm client — re-mint on every reconnect*).

**Each `connect()` retires the supervisor it replaces.** `NestClient::connect()`
is re-entrant — the account runtime re-assembles a device client per stint — and
the supervisor slot is assigned, not merely overwritten: the previous task is
aborted first. Dropping a `JoinHandle` does **not** abort its task, so an
overwrite strands a whole live supervisor that keeps its connection and keeps
dialling on its own schedule, with no handle left anywhere to stop it —
`disconnect()` can only reach the newest. The same retirement applies to the
reconnect-derive task, whose survivors would each bump `reconnect_tx` off one
`connection_state` transition and make every app re-pull its snapshot surfaces
N times per reconnect. This is the same class as `nest_link`'s proxy above, and
the two together are what turned one orphaned e2e pair into ~16,300 sockets held
`ESTABLISHED` on both ends for four and a half hours, exhausting a dev VM's
network state. Pinned by `supervisor::tests::on_connect_failure_still_grows_the_backoff`
and `client::tests::connect_twice_does_not_strand_the_first_supervisor`.

**No dialer outlives its owner.** Retiring on `connect()` covers only an overwrite; a holder that simply lets go of a client — a cache dropping it after a failed request, a mount whose assembly failed half way — used to leave its supervisor dialling for the life of the process, because the supervisor task holds the channel, never the `NestClient`, and dropping the `JoinHandle` does not abort it. So dropping the last handle to a `NestClient` aborts its supervisor and reconnect-derive task (`impl Drop for NestClient`), and the device-principal connect retry (`ws_device_handshake_bearer::spawn_connect_retry`) holds its client only weakly between attempts and ends once every owner has let go. A reconnect loop therefore exists exactly as long as something can still use its connection; a holder that wants a *fresh* connection drops or `disconnect()`s the old client, and a failed request is never a reason to rebuild one — its supervisor reconnects on its own (the sync agent rebuilds its retained control-plane client only once that client's supervisor has stopped, `SyncServiceState::nest_rpc`). This is the 2026-09-24 incident: a sync agent with a dead device credential threw its control-plane client away on every failed once-a-minute read and leaked a retry loop per failed mount, and after four days it was dialling the production nest about eight times a second. Pinned by `client::tests::dropping_a_client_stops_its_dialling`, `ws_device_handshake_bearer::tests::the_retry_ends_when_its_client_is_dropped`, and end to end by `fauna-sync-agent`'s `custodian::tests::a_dead_credential_is_dialled_on_one_ladder_not_one_per_failed_read` (a fake nest answering every upgrade `401`, thirty virtual minutes of failed reads, the dial count held to one ladder). **A superseded identity dials nothing more.** A supersession never un-happens (`succession-propagation.md`), so both bearer mints — the identity's silent challenge (`WsChallengeBearer`) and the store principal's device handshake (`WsDeviceHandshakeBearer`) — latch the `fauna.auth.superseded` refusal and answer every later mint from the latch without dialling; the device principal's latch also reaches its `AuthClient`, so its supervisor's terminal test and its connect retry stop. Before 2026-09-28 the device bearer had no latch and the challenge bearer never read its own, so on windows a retired identity's clients kept presenting its credential after the switch away from it, each bearer read a fresh dial of the budget below. **A holder the session cannot see never pins a dialer:** the local-search resolver a page's search manager holds keeps its index launcher only weakly (`fauna_client_conversations::LauncherLocalIndex`), because the launcher's flush driver lives as long as the launcher and publishes through the identity's `NestClient`; on windows an undisposed manager kept a departed identity's driver retrying once a minute. The same holds for the two seams app glue installs a `ConversationsSession` into: the devices machine's member-row join filter (`fauna-ffi`'s `SessionMlsQuery`) and the feed's room-post keys (`ConversationsSession::weak_room_post_keys`) both keep the session weakly, because a strong hold let every undisposed devices machine or feed manager keep its session — and that session's receive loop and index-lease tasks — alive past the sign-out that ended it (measured on windows 2026-09-29: one leaked session per e2e reset, each retrying against its departed client, until the Folders page drowned in refreshes); a departed session answers what an unwired seam does. Pinned by `folders_author::tests::a_departed_session_answers_not_joined` and `weak_room_post_keys_tests::a_departed_session_opens_nothing`, and by `ws_challenge_bearer::tests::a_latched_supersession_is_answered_without_dialling`, `ws_device_handshake_bearer::tests::a_superseded_principal_stops_dialling` and `fauna-client-conversations`'s `launcher_local_index_tests::a_registered_resolver_does_not_keep_the_launcher_alive`.

**The dial budget — a process-wide backstop keyed on the nest.** A backoff curve paces one loop; nothing in a curve bounds how many loops a process runs, and every dial storm to date was a count bug with every pace correct. So every native client WS dial to a nest — the bearer dial (`fauna_anon_client::tls_dial::dial_ws_trusted`, which the client supervisor and the leaf-crate authenticated connect both take) and the pre-identity dial (`connect_anonymous`, which every handshake mint takes) — first waits on `fauna_ws_substrate::dial_budget`: a token bucket per nest `host:port` per process, a burst of `DIAL_BUDGET_BURST` (256) dials and then one per `DIAL_BUDGET_REFILL` (10 s). **The burst is sized for both legitimate bursts a process makes, and the arithmetic is the constant itself:** `REDEPLOY_RECONNECT_DIALS` (64 — a redeploy drops every connection the process holds and they redial within a second: the session's client, the store principal's, a terminal app's per-feature clients, each a bearer re-mint plus the authenticated dial, measured under 24 on the widest app; the sync agent's control plane, data path and engine clients are that process's own budget) plus `SESSION_STARTS_COVERED` (16) runs of `DIALS_PER_SESSION_START` (12 — the widest session start measured 2026-09-28: the launch's silent challenge, the session client's mint and dial, the store principal's mint and dial, the post-auth silent challenge tui, linux and apple still run beside the session's own mint, and — until its first connect was gated on the grant (below) — the device principal's one to five `not_registered` handshake mints while its grant registration was in flight; the constant keeps that measurement as its ceiling). A process switching accounts back to back — sign out, sign in as another, again — is as correct as one reconnecting, and until 2026-09-28 the burst covered the reconnect alone (64): one e2e app process was paced three sign-ins into a succession journey, about 21 s in, and a person would have been paced after a handful of switches. Sixteen starts within one refill interval is many more than a person makes, and each start is cheaper for every dial it owed to nothing (below); past the burst the refill's six a minute sustains a switch a minute for ever. The refill sits below any correct loop's steady state (one dial per minute at the curve's ceiling), so the budget never paces a correct process and holds a broken one to six dials a minute whatever its loop count. **A correct process opens one authenticated socket per actor** (`transport.md` § the one-WebSocket-per-actor rule): every pass an app runs for a signed-in session rides that session's one client and never builds a one-shot client of its own. A one-shot client costs its own bearer mint and bearer dial, which is how one windows session start spent five dials for one actor until 2026-09-28 (`transport.md` § Implementation status, the windows bullet). Both are Rust constants: no user or admin would choose them (`principles.md` § One configuration surface). The wasm client is out of scope: web runs one reconnect loop per actor by construction (below). Pinned by `dial_budget::tests::a_thousand_dialers_share_one_allowance`, `…::a_redeploy_burst_is_not_paced` and `…::a_run_of_account_switches_is_not_paced` (sixteen session starts after a redeploy reconnect, at the constants above, wait for nothing). **The store principal's first connect waits for its grant (built 2026-09-28).** Until then its connect retry (`ws_device_handshake_bearer::spawn_connect_retry`) dialled `fauna.auth.device_handshake` the moment the account runtime started, racing the enrollment ceremony that registers its grant, and on a machine's first sign-in was refused `not_registered` one to five times before the grant landed — each refusal also spending the nest's failed-credential throttle for that device key (§ *The failed-credential throttle*). Now the account runtime spawns the retry behind a `ConnectGate::AfterGrant` on its grant-registered signal (`AccountStoreHandle::subscribe_grant_registered`): open from assembly when the slot already records a registration for the grant it carries (`PrincipalSlot::grant_registration_row` — every launch after the first), otherwise opened mid-pass by the engine holder the moment `ensure_enrollment_registered` returns a verdict that puts the grant on the nest (so the rest of that pass already rides the principal's connection), and on a co-located non-holder — which runs no pass — by re-reading the shared slot's latch on a 500 ms-to-30 s backoff, a local read and never a dial. It never closes again. The sync agent's host and the conformance tests that start registered pass `ConnectGate::Open`. Pinned by `ws_device_handshake_bearer::tests::a_gated_retry_dials_nothing_until_the_grant_is_registered`, `fauna-sync-engine`'s `account_runtime::tests::the_grant_registration_opens_the_principals_connect_gate`, and end to end by `conformance_account_runtime::v10_the_runtime_authenticates_as_the_store_principal` (a first sign-in against a one-refusal failed-credential bucket: exactly one principal session, the bucket untouched). **One dial a session start still owes to nothing (2026-09-28; shared Rust, the shape decided and the work queued).** tui, linux and apple run a post-auth silent challenge at every sign-in to refresh the registry's handle, domain and tier — a second full `challenge`/`verify` mint for the actor whose session client is minting its own bearer at the same instant, and whose `VerifyReply` carries the same three fields and discards them. The shape: `MintedBearer` keeps the reply's identity, the session client publishes its latest mint's identity on a watch, and the three apps' sign-in refresh reads the watch instead of dialling; the identity re-check that challenge doubled as (`security.md` § Post-auth surfacing, channel 3) is already carried by the session mint's own verdict through the supervisor's session-ending stop, and a handle rename is then caught at every hourly re-mint rather than once at login. The e2e `silent_sign_in` bridge command and windows' TTL refresh loop keep their on-demand dial: that IS the forced re-check. **The budget is per process, so an e2e factory reset clears it.** A native e2e driver keeps one app process across tests and factory-resets it in place, and a reset app stands for a new device — a new process, with a whole burst (`e2e-launch-isolation.md`, convention 10: a verdict never depends on how many tests ran before it). Left alone, the budget couples tests no real device couples: one recovery-kit journey (sign in, create a kit, succeed, lose every device, restore, sign in) spent the whole burst in about 33 s on windows, and the next test's successor then waited 10 s per dial and never reached its closing act. So the e2e flavors carry one seam, `dial_budget::clear_for_test` (feature `test-helpers`; `fauna-ffi`'s `dial_budget_clear_for_test`; compiled out of release, `e2e-automation-surface-gating.md`, convention 15), and each native app's per-test `reset` arm calls it after tearing its clients down. A shipped build has no way to clear the backstop. Pinned by `…::a_cleared_budget_is_a_fresh_process` and `…::clear_for_test_restores_the_process_burst`; windows, macOS and iOS call it since 2026-09-29; the other native apps' reset arms (linux, android, tui) do not yet, which is parity work.

**A `429` on the upgrade holds every dial to that nest.** A nest throttling a client's upgrades answers `429 Too Many Requests`, optionally with `Retry-After` in seconds. The dial that reads it records the hold in the same budget (`dial_budget::note_dial_error`), so every dial this process makes to that nest waits it out — not merely the loop that happened to receive it. No `Retry-After` holds for 60 s, and a hold is capped at 15 min so a bogus value cannot park a client for a day. The client classifies the refusal as `NestClientError::Api { status: 429 }`: not an auth rejection (no re-mint), not terminal, and an answered refusal (no demand dials). Pinned by `dial_budget::tests::a_retry_after_holds_the_whole_gate` and `…::a_retry_after_is_capped`.

**A locked account holds the reconnect loop until its unlock time.** `fauna.auth.account_locked` is terminal until the `locked_until` it carries ([`../behavior/devices.md`](../behavior/devices.md) § The locked state), which fits neither existing answer to a refused connect: stopping the loop (`SupervisorError::TerminalRefusal`, the superseded case) would strand the client past a lock that clears by itself, since nothing restarts a stopped supervisor, and backing off re-signs a ceremony the nest must refuse at every step of the curve. So the supervisor has a third answer, `SupervisedChannel::connect_error_hold` — how long a refused connect is known to stay refused. While it answers `Some` the loop holds: no backoff growth, no bearer re-mint, no SRV re-resolution, no early dial for a waiting request, and the connection state stays `Disconnected`; it asks again every `HOLD_RECHECK` (60 s, so a suspended device's stale timer cannot overrun the time) and resumes its ordinary paths the moment the answer is `None`. Consulted after the terminal test and ahead of the `401` arm, for the reason the terminal test is first. The substrate stays domain-free: `fauna-client`'s `ClientChannel` answers from its bearer mint's `LockedLatch` (`AuthClient::locked_hold`), and because the mint answers from that same latch without dialling while the lock stands, each recheck costs no network. A lock whose unlock time has passed on the client clock, or whose clock cannot be read, holds nothing and falls back to the ordinary backoff. Pinned by `supervisor::tests::a_held_refusal_waits_out_its_time_instead_of_backing_off` and `reconnect::tests::a_locked_account_holds_until_its_unlock_time_and_no_longer`.

**A supervisor that stops for good says why, to every request.** The native
supervisor ends on a clean close (1000), a connect refusal no retry clears (a
changed nest identity, a superseded identity, an HTTP 426), a 4426 close, or a
bearer refresh after 4401 that could not mint. `NestClient` records which as its
task returns, and the next `connect()` clears the record. Until then every
request on that client, one already parked for a reconnect included, fails at
once with that reason — a clean close as `RpcDisconnected { was_in_flight: false
}`, exactly as a client torn down by `disconnect()` does, every other stop as its
typed error — instead of waiting out its deadline for a reconnect that cannot
come, and `NestClient::supervisor_stop()` reads it for a caller that retries
across the stop. Before this the reason went into a `JoinHandle` nothing read:
the stop showed only as an indicator stuck on "Disconnected", and the headless
sync daemon's register hold, whose every register then timed out, logged a
version skew or a changed nest identity as an offline nest forever. Pinned by
`client::tests::a_request_after_the_supervisor_stops_fails_at_once_with_why` (a
real HTTP 426) and
`client::tests::every_supervisor_ending_names_the_error_requests_then_fail_with`.
The wasm client (`fauna_rpc_wasm`'s `WsRpcClient`) keeps the same record for
the three endings its loop has: a 1000 close answers `NotConnected`, as a client
torn down by `close()` does, a 4426 close answers `SubprotocolMismatch`, and a
token-provider rejection carrying the wire code `fauna.auth.not_registered` (the
SPA's `SignInRefusedError`, a re-mint refused after the revocation teardown's
4401) stops the loop as `Refused` rather than backing off — the one ending of
the three a current nest does reach, read typed by `sessionEndingVerdict()` and
routed to the launch surface (`onboarding.md` § Implementation status today, the
previously-signed-in row). Every other token-provider failure still backs off.
The SPA's connect wait (`rpc.ts`'s `ensureConnected`) reads the record through
`supervisorStop()`, so a page's call fails at once too (pinned by
`ensure-connected-stop-contract.test.ts`, under `just web-unit-test`). No
current nest reaches either close ending. It never closes a per-actor socket with
1000 (§ Graceful shutdown: 1001), and it answers a mismatch with the
pre-upgrade HTTP 426, which the browser reports as an ordinary close. On web
this is therefore parity plus a regression pin for the close-frame path, and a
version-skewed tab still retries until it reads "Cannot connect". The record's
*consumers* — a request or connect wait on an already-stopped or
freshly-parked client — are pinned by
`client::tests::a_request_on_a_stopped_client_fails_at_once_with_why`,
`client::tests::a_request_parked_for_a_reconnect_fails_with_the_stop_recorded_after_it`
and `client::tests::a_request_on_a_closed_client_fails_at_once`; the loop's own
close arm that *produces* the record — driven through a fake global
`WebSocket` closing with each code, not planted by hand — is pinned by
`client::tests::the_loops_own_close_arm_records_a_clean_stop` and
`client::tests::the_loops_own_close_arm_records_a_subprotocol_mismatch_stop`; the
refusal ending by `client::tests::a_refused_token_mint_stops_the_loop_as_a_sign_in_refusal`.
The two halves are composed by a third test,
`client::tests::a_request_against_the_loops_own_recorded_stop_fails_with_why`,
which drives the loop to a 4426 close and then issues a request against the
same client, asserting `SubprotocolMismatch` — the one variant no path can
answer without reading the record the loop itself wrote, not one planted by
hand. All under `just wasm-test-check`.

**Web runs ONE reconnect loop per actor, the core chunk's — a wasm chunk client
built over the shared rpc port has none of its own (ratified 2026-09-25;
the port itself is `transport.md` § Design decisions).** Until then every
page-machine chunk (folders ×2, media, backups, labeler catalog, atproto
settings) dialled its own `WsRpcClient` and so ran its own copy of the loop
above, each drawing its own jittered nap: after a benign flip the core loop
could be back — the SPA's `connection` indicator reading online — while a
chunk's loop was still asleep, up to `MAX_BACKOFF` away from its next dial,
and a gesture on that page answered `not connected`. A port-built client
(`WsRpcClient::over_port`, `fauna-rpc-wasm/src/shared_port.rs`) has no
socket, no loop and no bearer: its request crosses to the core chunk's
`requestRaw`, which runs it through the core loop's own
`dispatcher_within_deadline` — so a chunk request issued in a reconnect gap
waits out exactly the gap the core's requests wait out, bounded by the same
per-kind deadline, and comes back the moment the core loop's establish probe
passes. Its `connection_state()` is the owner's, read through the port; the
owner's supervisor stop reaches it as the port's rejection, with why; the
reconnect / state / push hooks are the owner's alone (registering one on a
port-built client logs a warning and does nothing). The e2e reconnect pace
override paces the one loop there is, through the SPA singleton. Witness:
`test_nest_flip_resilience.py::test_a_folders_gesture_lands_the_moment_the_app_reads_online_after_a_flip`
(cross-app) and, on web, `::test_page_machines_open_no_socket_of_their_own`.

`RpcDispatcher` is **runtime-agnostic**: `RpcDispatcher::new(stream)` returns
`(dispatcher, driver_future)` and never spawns internally, so the caller drives
the future on its own runtime — native via `tokio::spawn` (in the supervisor
above), wasm via `wasm_bindgen_futures::spawn_local`. This is what lets
`fauna-protocol` compile to `wasm32-unknown-unknown` for the in-progress web
façade: the browser-`WebSocket` adapter is `!Send`, so the core carries no
`Send` bound on the stream (native re-imposes `Send` merely by handing the
future to `tokio::spawn`).

### Connection-status indicator (app UI)

Every app renders a **global connection-status indicator** — a small,
always-visible text at the top of the sidebar/shell that reads **"Connected" /
"Connecting…" / "Disconnected" / "Cannot connect"**, driven directly by the
transport `ConnectionState` (the four states above; the supervisor's
`connection_state()` watch is the source of truth). It is the *visible* half of
the reconnect machinery: a Watchtower swap or transient drop shows live as
"Connecting…" and returns to "Connected" on reconnect, and a **transient** gap is
**deliberately never surfaced as an error banner / toast / `rpc disconnected`
text** — the in-gap request-wait (§ Request lifecycle step 3) already keeps the
gap from erroring; the indicator just makes it visible. The element is
`connection-status` in `ui.yaml`'s `global` section (present on every
authenticated page); tests read its text to assert the client holds/loses the
connection.

A `Reconnecting` state is **deliberately not modelled** — a *swap* is a
`Connecting`, and giving it its own word would say nothing the user can act on.

#### `Unreachable` — a persistent failure is not a transient one (ratified 2026-07-29)

`ConnectionState::Unreachable` is the fourth state: connecting has failed
`fauna_core::format::CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES` times in
a row **with no established connection in between**, so the client stops calling
the gap transient and the indicator reads **"Cannot connect"**. The counter
clears on every proven connection, and the state is **sticky** until one — an
indicator that flipped back to "Connecting…" on each retry would tell the user
nothing. The supervisor keeps retrying throughout: this is a *reporting* state,
not a terminal one.

**Why it had to exist.** The three-state model could not distinguish a
one-second redeploy from a box no client will ever reach, so both read as an
indefinite "Connecting…". A user who claimed a nest with **:80 firewalled** —
supported intent, since DNS-01 is the certificate path — got a claimed, running,
healthy-looking nest that no browser could talk to and that **reported nothing**:
the nest sits on its always-live self-signed floor (`nest/domains-and-tls-bootstrap.md`),
the page loads once the user clicks through the certificate interstitial, but
browsers do not prompt on a WebSocket handshake, so the WSS attempt just fails
forever. The Admin nav entry vanishes too, because `am-i-admin` is fail-closed on
any transport failure — downstream of the same silence, not a second bug. That is
a *works-out-of-the-box* failure whose defining property was a missing observable.

**Why it is derived client-side, from failure alone.** Nothing the nest could
tell us is reachable here — the socket is precisely what is failing, and the
projection that would name the cause (`fauna.tls.cert_status`,
`nest/tls-certificates.md` § C.4) is **admin-only and lives behind that same
socket**. So the one diagnostic that works on the box that cannot talk is the one
the client derives from its own failed attempts.

**Why the wording is generic and names no certificate.** The cause is not
uniformly knowable. A native app reads its own rustls error, but the browser
`WebSocket` API hides both the upgrade status and the TLS failure reason, so the
SPA can prove the connection is persistently failing and never *why*. A state
that named the certificate would therefore be a per-app divergence (priority
#1) that the one failing platform could not produce. Naming the cause needs a
separate, additive channel — see § Implementation status today.

**Why the threshold is a shared constant.** The native supervisor
(`fauna_ws_substrate`) and the wasm reconnect loop (`fauna_rpc_wasm`) are two
implementations of one contract; the threshold lives once in
`fauna_core::format` so web and native cannot disagree about when a gap stops
being transient. It is sized to sit above any planned redeploy — the
graceful-shutdown budget is bounded by the container `stop_grace_period` (15 s,
§ Graceful shutdown), while eight failures cost roughly a minute of jittered
backoff — so a Watchtower swap still reads "Connecting…".

The indicator is fed by the same `ConnectionState` watch on every platform, so
the three client families consume one shared reactive surface (no per-app
state field):

- **Native (linux):** subscribes to `fauna_client::NestClient::connection_state()
  -> watch::Receiver<ConnectionState>` directly via its `WsEvent` pump.
- **UniFFI (windows / apple / android):** `FfiNestClient::subscribe_connection_state()
  -> FfiConnectionStateSubscription` — the UniFFI-friendly twin of the watch
  (sibling of `subscribe_reconnects`). Its first `next().await` yields the
  *current* `FfiConnectionState`, then each transition; `None` on teardown. The
  client drives `while let Some(s) = sub.next().await { /* update the indicator */ }`.
- **Wasm (web):** `WsRpcClient.setOnConnectionStateChanged((state) => …)` — a JS
  callback fired on every transition with `"connecting" | "connected" |
  "disconnected" | "unreachable"` (distinct from `setOnReconnected`, which fires
  only on a *reconnect* and so cannot drive the Disconnected/Connecting states).
  The SPA seeds a store from `connectionState()` then binds it to the callback.

The state → i18n-key decision has one owner,
`fauna_core::format::connection_state_label`, keyed on those same lowercase
words, with anything unrecognised degrading to "Disconnected" — the honest weaker
claim — so a client meeting a newer state word never renders a blank indicator.
**Web** consumes it directly (`connectionStateLabel` over wasm), replacing the
ternary it used to keep in `+layout.svelte`. **linux, tui, android and windows**
now also resolve through the same owner — linux via a thin
`apps/fauna-linux/src/i18n::connection_state_label` wrapper, tui via a thin
resolve in `ui.rs`, android via a new `#[uniffi::export] connection_state_label`
wrapper in `fauna-ffi`, windows via `MainViewModel.OnConnectionStateChanged`
calling `Strings.Resolve(FaunaFfiMethods.ConnectionStateLabel(state))` —
replacing the hand-rolled match each used to keep. **macOS/iOS** now also
resolve through the same owner — the shared FaunaKit `ConnectionStatusBar.swift`
calls `renderLocalizedText(connectionStateLabel(state:))` instead of its own
`FfiConnectionState` switch — so all 7 apps route through one state → key
decision.

> **Implementation status today (2026-06-21): implemented on all 7 apps.**
> **linux** (`apps/fauna-linux/src/app.rs`, top of the sidebar), **web**
> (`apps/fauna-web/src/routes/+layout.svelte` + the `connectionStatus` store),
> **android** (`apps/fauna-android/.../navigation/FaunaNavHost.kt` `ConnectionStatusBar`
> + `ConnectionStatusVM` over `ApiClient.connectionState`), **windows**
> (`apps/fauna-windows/.../Views/MainPage.xaml` `connection-status` TextBox bound to
> `MainViewModel.ConnectionStatus`, fed by `NestRpcClient.StartConnectionStatePump`
> over the same `subscribe_connection_state` watch — placed at the shell top, NOT
> inside the NavigationView pane header, which WinUI does not reliably realize in the
> UIA tree for the FlaUI e2e driver), and **macOS + iOS** (the shared FaunaKit
> `ConnectionStatusBar` view in a top `.safeAreaInset` of each shell —
> `Fauna-macOS/.../MainWindow/ContentView.swift` `MainWindowView` and
> `Fauna-iOS/App/ContentView.swift` `MainTabView` — reading `FaunaClient.connectionState`,
> which `FaunaClient.startConnectionStateObserver()` pumps from
> `subscribe_connection_state`; the `automationValue` text reader dereferences the
> `@Observable FaunaClient` live so the in-process e2e read tracks each transition),
> and **tui** (`apps/fauna-tui/src/ui.rs` `connection-status`, pinned above the tab
> rows like the linux sidebar, pumping the same `fauna_ws_substrate::supervisor::ConnectionState`
> watch — `session.rs`).
> The forced-disconnect→reconnect regression guard is
> `test_nest_flip_resilience.py::test_connection_status_flips_while_nest_down`
> (green `--client macos`; `--client ios` where the in-process harness supports the
> forced-disconnect step).
>
> **`Unreachable` (2026-07-29): the state and both loops are built; two gaps
> remain open, a third is partly closed.** The variant exists in all three Rust
> surfaces (`fauna_ws_substrate::supervisor::ConnectionState`, `fauna_rpc_wasm`'s
> wasm copy, `FfiConnectionState`), both reconnect loops count consecutive
> failure-to-establish against the shared
> `CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES` and clear it on every proven
> connection, and all 7 apps render "Cannot connect"
> (`common.cannot_connect`). Pinned by
> `supervisor::tests::{persistent_connect_failure_settles_on_unreachable,
> a_proven_connection_clears_the_unreachable_run}` and
> `format::connection_state_label_maps_each_state`. Still open:
> **(1)** the tier_4 differential (a real image whose socket a browser will not
> carry, asserting web settles on "Cannot connect" while a native/TOFU client
> reaches `Connected` against the *same* box) is **half-driven**: the
> native/TOFU half is proven — a native app reaches `Connected` and passes
> `am-i-admin` against a box parked on its always-live self-signed floor (bare
> `handle=admin` claim, no mail domain). The web half is not yet driven: it
> needs a real browser navigating same-origin to the box's own served SPA (a
> cross-origin local SPA would confound the differential on the nest's default
> CORS allow-list), which needs a docker image built after both the `/app`
> same-origin serving fix and the introduction of `ConnectionState::Unreachable`
> itself — the currently available test image predates both, so this leg
> awaits a fresh production image build; **(2)** nothing yet **names the
> cause** — a nest that cannot obtain a certificate still says so nowhere a
> user can reach without SSH, and the candidate channel is an *anonymous*
> (pre-socket) read rather than the admin-only `fauna.tls.cert_status`;
> **(3) CLOSED (2026-08-17):** all 7 apps now call `connection_state_label`
> instead of mapping their own enum arms (windows landed 2026-07-29; macOS/iOS
> landed last, `ConnectionStatusBar.swift`).

### The principal session — a token-bearing connection (ratified 2026-10-01; built 2026-10-02)

A third-party principal ([`third-party.md`](third-party.md) § The principal model) holds no Ed25519 actor key and no nest bearer: its transport credential is a DPoP-bound access token from the nest's own issuer ([`../behavior/authorization-server.md`](../behavior/authorization-server.md) § Tokens authorize transport). It opens WS-RPC on a third endpoint, **`GET /api/v1/principal/ws`**, with `Sec-WebSocket-Protocol: fauna.v1, dpop.<access token>, dpop-proof.<proof JWT>` — the subprotocol carriage `bearer.<token>` already uses, and for the same reason: a browser's WebSocket API can set nothing else, so one carriage serves every execution form. No path segment names an account or a principal. The token does (`fauna_actor`, `client_id`), and a path value would be a second opinion the nest then has to reconcile — the reasoning that keeps `{actor_id}` off the anonymous endpoint. The nest echoes `fauna.v1`.

**It is the issuer's resource-server gate applied to the upgrade request — run once, before the 101.** The order is `/oauth/userinfo`'s, by the rule that the resource-server plane applies unchanged to the nest's own routes. **(1) The token:** signed by a key in the served set, `typ: at+jwt`, `iss` this issuer, **`aud` containing this issuer's identifier** ([`../behavior/authorization-server.md`](../behavior/authorization-server.md) § The issuer → *The audience is the set of readers* owns the audience rule), unexpired. **(2) The proof, strictly after the token:** `htm` `GET`, `htu` the upgrade URL in its `https` form, `ath` the hash of the presented token, a nonce this issuer minted, a `jti` the replay set has not seen, and a proving key whose thumbprint is the token's `cnf.jkt`. **(3) At least one Fauna-family scope**, else `403 insufficient_scope` — a token consented only for the PDS, or only for sign-in, opens no session. **(4) A principal row** for the token's `(fauna_actor, client_id)`, **and an account whose own authority is live** (not suspended, locked out, deleted or superseded) **and whose external-apps switch is ON** ([`../behavior/atproto-pds-full.md`](../behavior/atproto-pds-full.md) § Detailed design → F1 detail, the kill-switch bullet, owns the switch). Refusals are userinfo's: `401 invalid_token` for every token, proof, principal and account failure alike, so the answer is never an oracle about which; `401 use_dpop_nonce` only after a verified token; each with a `WWW-Authenticate: DPoP` challenge and a fresh `DPoP-Nonce`. **The nonce is the issuer's one nonce:** the same minter serves the token endpoint and this gate, so a client that has just redeemed or refreshed already holds a usable one from that reply — which is what lets a browser client, unable to read a failed upgrade's headers, dial without a second round trip (the token endpoint exposes the nonce to a foreign origin — [`../behavior/authorization-server.md`](../behavior/authorization-server.md) § The issuer → *Cross-origin access*). The upgrade is a surface of the failed-credential throttle (§ Pre-identity (anonymous) connection → *The failed-credential throttle*), keyed source × the presented token's unverified `client_id`, and a principal holds at most a fixed number of concurrent sessions (a Rust constant; one over is refused `429`).

**No pre-identity kind mints anything from an access token, and the nest never turns one into a bearer.** The existing bootstraps (`fauna.auth.device_handshake`, `fauna.auth.custody_handshake`) each end in a nest bearer, and that shape is refused here on purpose: a bearer is not bound to the DPoP key, so minting one from a sender-constrained token would hand whoever exfiltrates it a session the token alone could never have opened. Nor is an anonymous connection promoted in place: *validated exactly once, at the upgrade, and baked into the connection for its lifetime* (*Revocation teardown*, above) is the invariant every teardown rests on, and a connection that changes who it is mid-life has no upgrade to be validated at.

**The connection is the principal's, never the account's.** It carries a **principal binding** — the account it acts for, the `principal_id`, the token's scope set and its `exp` — and its actor slot is the anonymous placeholder. Three boundaries follow, and each is load-bearing. **Class comes from the binding, re-resolved per RPC:** the dispatch gate reads the principal row and the account's authority on every message and answers `ThirdParty` or nothing — never `User`, whoever the account is ([`apps/bridges.md`](apps/bridges.md) § Capability-allowlist enforcement → *`ThirdParty`* owns the class, the compiled ceiling and the scope check). **Handlers are separate:** a kind in the `ThirdParty` ceiling is served by a handler that takes the principal caller explicitly, and a handler written for an actor is never invoked for a principal — one reached by mistake would be handed the zero actor, which resolves no class, so the failure is a refusal and never the account's full reach. **Registration is separate:** a principal session joins a registry keyed on `(account, principal_id)` and never the account's own subscription entry, so it receives none of the account's Push, counts toward no presence or online decision, and never appears in the device roster; the one push that reaches it is the events door's filtered nudge ([`transport.md`](transport.md) § Push events → *Third-party event doors* owns the frame and the filter, built 2026-10-05).

**The token's `exp` bounds the session, and the socket closes at it.** The connection holds a deadline at `exp` and closes `4401` when it passes — closed, not merely refused at its next call, because a Push is not an RPC and an idle socket would otherwise outlive its credential (the bridge's mailbox-subscription case, above). In-flight handlers finish, as in every teardown. There is no in-band re-authentication: a client refreshes its token and dials a new session, and may open the new one before the old one closes. `4401` keeps its one meaning — the credential this connection was opened with is finished; get another — and whether another can be had is the token endpoint's answer, so a revoked principal learns it there (`invalid_grant`) rather than from a close code of its own.

**Revocation — three doors, and what each one needs.** **(a) `fauna.principals.revoke`** deletes the row, and the row is what the dispatch gate reads, so the next RPC on every live session is refused with no further mechanism: the per-actor shape, not the per-token one (*The upgrade window*, above, on why the two differ). That does not excuse the two rules above it. *Losing authority closes the socket:* the revoke sweeps the principal's sessions `4401` after its transaction commits — rows first, then the sweep — because a refused socket left open is still a Push recipient once the event door exists. *The upgrade window:* registration re-reads the principal row once, **after** joining the registry, and closes the connection when the row is gone; with the revoke's delete-before-sweep order one read is total, for exactly the reason it is total for a session. **(b) Every path that strips the account's authority** (the enumeration under *Revocation teardown*) closes the account's principal sessions together with its own sockets — the shared helper sweeps both registries — and the gate's per-RPC account read catches the straggler. **The account's external-apps switch OFF** is a suspension, not a revocation, and takes door (b)'s shape without its finality: the flip sweeps the account's principal sessions `4401` after its write, the per-RPC read refuses until ON, and nothing is deleted (the kill-switch bullet owns it). **(c) One grant family ending closes the sessions its tokens opened (ruled 2026-10-02; owed — the build is the gap is declared in [`transport.md`](transport.md) § Implementation status).** Until this ruling the door read *closes no socket: an access token is not revocable per call anywhere, so that session lives until its token's `exp`, at most 15 minutes* — a tail inherited from the PDS plane, where it is priced by what per-call consultation would cost there ([`../behavior/authorization-server.md`](../behavior/authorization-server.md) § As built → *The grant registry does NOT qualify as access-token revocation*: a bridge-to-nest RPC inside the pure decision function, or a bridge-side cache with a restart hole). Neither cost exists on the nest's own plane: the gate already reads the principal row, the account's authority and the external-apps switch on every RPC, in process, against the canonical rows with no cache in front, and the family's `revoked_at` is one more primary-key read beside them. A tail priced by a cost that is absent is not a price but an omission, and this section's rule — *losing authority closes the socket* — decides it. Three of the endings carry weight of their own. **The per-grant revoke is the user's act** on the connected-apps roster (`fauna.bridges.atproto.revoke_session`; today the `atproto` page's grant row, tomorrow the one roster of [`third-party.md`](third-party.md) § The roster model): a user who revoked and watched the row vanish, while the app's socket served 15 minutes more and — once the push door exists — kept receiving their events, was not given the control [`../principles.md`](../principles.md) § The user always controls their data promises; door (a) is the heavier verb (the whole principal, every family at once) and does not excuse the lighter one from being immediate. **The reuse family-kill is a theft signal** — a replayed refresh token — and the session live under that family may be the thief's. **The forced session-secret rotation is a compromise response**, and one that leaves live sockets for 15 minutes when one registry sweep closes them is half done, for the reason that arm already ends grant rows instead of leaving them to the next read. The honest client loses nothing: `/oauth/revoke` is its own sign-out, and a refresh under an ended family was refused already, so the close only moves a failure its next dial would have met up to 15 minutes earlier. **The shape is door (a)'s, so the three doors share one.** (1) The binding carries the token's `sid` — the family id, which every nest-minted access token already names (`sid` = the grant id = the family's initial refresh `jti`, [`../behavior/authorization-server.md`](../behavior/authorization-server.md) § As built → *Rotation rides the NEST's registry*); a token with no `sid` is refused at the upgrade as `invalid_token`, and the nest mints none. (2) The gate's per-RPC resolve reads the family's row beside the three it reads today — live, or the every-kind refusal — so an ended family bites at the next call with no further mechanism, and the session's own `exp` deadline stays the backstop. (3) Every ending sweeps after its commit, rows first then the sweep, as in door (a), so with the registration's one re-read after joining — extended to the family — one read is total: the per-family endings (the user's revoke, `/oauth/revoke` and the bridge-class `end_session` through their one owner `end_oauth_session`, and the reuse family-kill) close that family's sessions and no sibling family's of the same principal; the forced session-secret rotation closes every principal session, since every family it ends is nest-minted and every principal session was opened under one. The re-point's database step runs before the nest serves and has no socket to close. (4) The close is `4401` with its one meaning — the credential this connection was opened with is finished — and whether another can be had stays the token endpoint's answer (`invalid_grant`). **The forced issuer-key arm closes every principal session for the same reason:** each was admitted under a key the arm drops, and a live session is a token still being honoured after the arm has said no token signed by that key is. Both forced arms are [`../behavior/authorization-server.md`](../behavior/authorization-server.md) § The issuer → *Two rotation arms*; the sweep is one line of each, after the arm's own write. Door (a) stays the whole-principal verb. Pinned, once built, in the revocation census (`bins/fauna-nest/tests/conformance_revocation_teardown.rs`) beside the principal twin: each ending's storage call paired with its sweep.

## Graceful shutdown

Nests auto-update: a Watchtower redeploy stops the old container (SIGTERM, then
SIGKILL after the `stop_grace_period`) and starts a fresh one on the same
`/data` volume. On SIGTERM the nest shuts down **gracefully** so the swap is as
low-disruption as possible for every connected client. The sequence:

1. **Stop accepting new connections** — abort the accept-loop handle and await
   it (a dropped `JoinHandle` only detaches the task; the accept loop has no
   shutdown arm of its own and would keep running until process exit). Live
   per-connection tasks are independent `tokio::spawn`s, so they keep serving.
2. **Broadcast WS 1001 (Going Away) to every connected actor** — `WsState::begin_shutdown`
   flips a `watch` flag every live `run_connection` is watching. Each one stops
   reading new requests, **drains its in-flight handlers** (letting them finish
   and flush their Replies), then closes its socket with **1001**.
3. **Wait for the drain** — `main` waits (bounded by `ws::GRACEFUL_SHUTDOWN_TIMEOUT`,
   7 s) for every connection to close, with each connection's own in-flight drain
   bounded by `ws::SHUTDOWN_DRAIN_GRACE` (5 s).
4. **Checkpoint + exit** — `db.flush()` runs `PRAGMA wal_checkpoint(TRUNCATE)`,
   then the process exits. The whole budget fits inside the container
   `stop_grace_period` (15 s in `docker-compose.yml`), so SIGKILL never truncates
   the 1001 broadcast.

**1001, never 1000 — the load-bearing invariant.** Per § Connection lifecycle's
close-code table, `1000` maps to `CleanDisconnect` and **stops the client's
reconnect loop**; `1001` maps to `Retry` (backoff + reconnect). A planned
shutdown that ever emitted `1000` would stop every app reconnecting on every
redeploy. The shutdown path therefore emits **only** 1001; a regression test
(`bins/fauna-nest/tests/graceful_shutdown.rs`) asserts the planned-shutdown close
code is 1001, never 1000.

**Why a clean close (not just letting TCP drop).** Without the 1001 the client
only notices the drop via a TCP FIN or its `KEEPALIVE_TIMEOUT` dead-link
detector (up to 60 s) — and a non-idempotent write in flight at the drop instant
errors (`RpcDisconnected { was_in_flight: true }`) instead of finishing. The
1001 lets the client detect the drop **instantly** and reconnect promptly, and
the in-flight drain lets that write complete. In-gap reads issued *after* the
drop already wait for reconnect rather than failing (Request lifecycle step 3),
so the drain only needs to cover requests already on the wire.

**The HTTP surface is not drained, by design.** Step 1 stops the accept
*loop*, not in-flight or already-admitted connections, and nothing on the
plain-HTTP surface (blob/chunk/video upload and download, `/share/{token}`,
`/api/v1/health`, `/api/v1/nest/info`, `/api/v1/export`, …) consults
`is_shutting_down()` the way the actor WS handler does. This is accepted
rather than gated: those routes are the bulk-binary carve-out (§ everything
else rides WS-RPC), a request in flight at process exit either completes or
the client retries it the same way it retries any dropped connection, and no
committed write is lost — `db.flush()` still runs after the drain wait. A
future HTTP mutation route that is genuinely non-idempotent should gate on
`is_shutting_down()` itself rather than relying on this default.

> **Implementation status today (2026-06-08): implemented.** On Windows, which has no
> SIGTERM, a console Ctrl+C or Ctrl+Break runs the same path (2026-09-24); the e2e
> harness starts a nest in its own process group and stops it gracefully with a
> Ctrl+Break to that group (`tests/common/nest.py::stop_nest`) — before this its
> "graceful" stop was a TerminateProcess hard kill. SIGTERM path in
> `bins/fauna-nest/src/main.rs` (`begin_shutdown` → bounded drain wait →
> `db.flush`); the per-connection drain + 1001 close in
> `bins/fauna-nest/src/routes.rs` (`run_connection` / `drain_and_close`); the
> shutdown `watch` + grace constants in `bins/fauna-nest/src/ws.rs`. Proven at
> the protocol level by `bins/fauna-nest/tests/graceful_shutdown.rs` (tier_3:
> 1001-not-1000 + in-flight drain) and against the real image + `docker stop`
> by `tests/e2e-unified/tests/platform/docker/test_graceful_shutdown.py` (tier_4).
> **2026-07-10:** the in-flight-drain half of that Rust test had been vacuous
> since the central capability gate landed — it dispatched a synthetic kind the
> gate refused, so the handler never ran. It now dispatches a real allowlisted
> kind and exercises the drain for real.
> **2026-08-30**: step 1 itself was a no-op from 2026-06-08 until
> now — `main.rs` stopped the accept loop with `drop(handle)`, which detaches
> a `tokio::task::JoinHandle` rather than aborting it, so the nest kept
> accepting and serving new connections through the entire drain window and
> until process exit. Fixed to `handle.abort(); let _ = handle.await;`,
> mirroring `routes.rs::teardown_serving_generation`'s already-correct
> sibling path. `bins/fauna-nest/tests/graceful_shutdown.rs`'s new
> `aborting_the_serve_handle_refuses_new_connections_dropping_it_does_not`
> drives the real accept loop and asserts a post-abort connect is refused —
> the gap no earlier test covered, since the file's other two tests drive
> `WsState::begin_shutdown()` directly and never touch the accept loop.

## Pre-identity (anonymous) connection

Auth bootstrap (`auth.handshake` / `auth.challenge` / `auth.verify`),
account registration, public discovery (`nest.info`, `handle.available`,
`nest.resolve`, `actor.by_handle`, `setup.status`), and the one-time
admin claim (`auth.claim_admin`) all run **before any bearer token
exists**. They ride a second, anonymous WS connection rather than HTTP —
their HTTP twins are all deleted (§ Implementation status, Track A).

> **Implementation status today (2026-07-17): the connect phase is
> deadline-bounded.** The native connector
> (`libs/fauna-anon-client/src/ws.rs::connect_anonymous`) wraps its TCP + TLS +
> WS-upgrade connect (all three branches: DNS-override, `wss://`, plain
> `ws://`) in the same 30 s default deadline `dispatch::DEFAULT_DEADLINE` uses
> for the request/reply round-trip — before this, a blackholed peer (SYN
> silently dropped) hung the connect for the OS TCP SYN-retry ladder, measured
> ~127 s on Linux's default `tcp_syn_retries=6`, with no caller-side bound.
> Web is exempt (the browser `WebSocket`/`fetch` APIs carry their own
> OS/browser-level connect timeouts).

**Endpoint:** `GET /api/v1/ws` — no `{actor_id}` path segment. An
anonymous connection has no proven actor to key on, so the per-actor
path param of the authenticated `GET /api/v1/ws/{actor_id}` would be a
value the server cannot validate against a bearer (the 403
actor/bearer-mismatch check has nothing to check). Omitting it is the
honest shape; the two endpoints are distinct.

**Handshake:** `Sec-WebSocket-Protocol: fauna.v1` with **no
`bearer.<token>` element**. Server echoes `fauna.v1`. (On
`/api/v1/ws/{actor_id}` a missing bearer stays a 401 close — anonymous
access is *only* via the bare `/api/v1/ws`; the authenticated endpoint
never serves anonymous traffic.)

**Routing — a fixed pre-identity allowlist.** The connection routes only
the kinds below; a Request for any other (e.g. an authenticated Layer-1
kind) gets `RpcError { code: "fauna.protocol.unauthenticated" }` and the
connection stays open (same shape as `unknown_kind` — one disallowed
request must not kill an in-flight bootstrap). The allowlist is a single
source of truth on the nest (mirroring `bridge_method_allowlist`):

| Group | Wire kinds |
|---|---|
| Auth bootstrap | `fauna.auth.handshake`, `fauna.auth.challenge`, `fauna.auth.verify`, `fauna.auth.nest_handshake` (nest-identity channel binding — § Channel binding), `fauna.auth.device_handshake` (sync-agent renewal-grant bearer mint — signature-work-bounded like `handshake`; its refusals, never its successes, spend the failed-credential throttle, § Abuse posture), `fauna.auth.custody_handshake` (custody-session bearer mint: the `device_handshake` PoP shape plus an inline `CustodyGrant` witness, checked against a live nest custody row — same unthrottled, signature-work-bounded shape; behavior owned by `account-replica-posture.md` § Replica posture → *The custody grant + ceremony*) |
| Registration | `fauna.account.register` |
| Recovery | `fauna.account.lockout` (no-token emergency lockout; signature over the domain-tagged `account_lockout_signed_message`, throttled — the migration of `POST /api/v1/account/lockout`) |
| RecoveryKey plane | `fauna.recovery.registration.chain` (the public `actor_id → recovery_pubkey` directory read a peer verifying a succession statement needs); `fauna.recovery.escrow.{challenge,fetch}` (seed-escrow restore: a client that lost every device holds only the recovery phrase, so no session can exist — authorization is a RecoveryKey signature over the issued nonce); `fauna.recovery.replacement.{challenge,veto}` (a seed thief revoked every session and invoked the seed-signed lockout; the real owner, holding only the recovery phrase, vetoes); `fauna.recovery.succession.{submit,lookup}` (the succession ceremony itself, and the directory lookup peers holding an old actor id need). All seven throttled; behavior owned by [`../behavior/identity-succession.md`](../behavior/identity-succession.md). ⚠ **`fauna.recovery.succession.status` is deliberately NOT here** — the third succession kind serves a *server-observed* commit stamp rather than a signed artifact, so it is User-class and self-scoped; routing it anonymously would make it an oracle for when any account's recovery-from-compromise happened ([`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md) § Adjudicating what the aftermath carries across) |
| Discovery | `fauna.nest.info`, `fauna.handle.available`, `fauna.nest.resolve`, `fauna.actor.by_handle`, `fauna.setup.status` |
| Admin claim | `fauna.auth.claim_admin` |
| Invite (Track A5) | `fauna.account.invite_request.{submit,status,cancel}`, `fauna.account.invite_code.verify` |
| NAT-mode | `fauna.setup.nat_mode` (admin-signed; the signature is the auth) |
| Bridge enrollment | `fauna.bridges.request_enrollment` — **loopback-gated**: the dispatcher refuses it from any non-loopback peer (`requires_loopback_peer`) |

**No third-party principal kind is in this table, by ruling (2026-10-01).** A principal authenticates with a DPoP-bound access token, and it does so at an upgrade of its own rather than through a bootstrap here: every auth kind above ends in a nest bearer, and a bearer minted from a sender-constrained token would undo the constraint. The endpoint, the gate and the teardown are § Connection lifecycle → *The principal session*.

The four discovery kinds are additionally per-source rate-limited through the
generic gate (`is_throttled_anonymous_kind` → `anonymous_rate_limit`), and so
are the recovery ceremonies — the RecoveryKey plane above plus
`account.lockout` — **for a different reason, which the generic gate carries
two classes to serve.** An unsigned oracle is throttled because the limit is
its only bound; a signature-bound ceremony is throttled because *being
signature-bound is what makes the unmetered Ed25519 verify conscriptable* — an
anonymous flood clears the cheap pre-checks and buys asymmetric work at the
signature check. **Signature-gating therefore argues FOR a throttle, never as
an exemption from one**, and no such throttle can lock a legitimate holder out:
each ceremony is a handful of calls, once, and the bucket keys on the
unspoofable TCP peer for an anonymous connection — or on the actor, in a
disjoint namespace, for an authenticated one (the caps bind the kind on every
connection class, `federation.md` § Security, 2026-08-24) — so a flood
throttles only the flooder's own source or account.
`claim_admin` (per-source **and** a global distributed-brute-force cap),
`invite_code.verify`, `register`, and `invite_request.submit` are each
throttled too, but through their **own** dedicated dispatcher gate rather
than the generic one — thresholds and rationale are
`docs/goal/architecture/federation.md` § Security's to state, not restated
here. Only the auth-bootstrap ceremony (`handshake`/`challenge`/`verify`/
`nest_handshake`/`device_handshake`/`custody_handshake`), `setup.status`,
`invite_request.{status,cancel}`, `setup.storage_mode`, `setup.nat_mode`, and
the loopback-gated `bridges.request_enrollment` are unthrottled per attempt —
the read-only and loopback-gated ones because a per-source limit would add
nothing, and the auth-bootstrap ceremony because an attempt-counting throttle
could actively lock a legitimate actor out of bootstrap. The one bound on it is
lockout-safe by construction and counts no attempt that succeeds:
`device_handshake` rides the failed-credential throttle (§ Abuse posture →
*The failed-credential throttle*), which spends only on refusals, keyed on
`(source × claimed renewal key)`.

Wire kinds are **snake_case** per the advisory namespace lint (§ Namespace
policy — segments are `[a-z][a-z0-9_]*`, no uppercase), matching shipped
precedent (`fauna.spam.get_preferences`). Where `api-layers.md` writes a
camelCase shorthand (`actor.byHandle`, `auth.claimAdmin`) the wire form is
the snake_case spelling above. Exact per-verb spelling is pinned by each
migrating task (Track A1–A5).

**No actor state.** No actor is bound to the connection, so it emits no
Push events (nothing to route to). The idempotency cache is already
per-connection (`RpcConnection.idempotency_cache`, same bounds — 1000
entries, 5-min TTL, 64 KiB `too_large` threshold); it simply has no
owning actor here.
Telemetry counts the connection under `ws_connections_active{actor_known=false}`.

**Becoming authenticated — reconnect with the minted bearer, no
in-place upgrade.** `auth.handshake` (and `auth.verify`) mint a bearer
token exactly as the retired `POST /api/v1/auth/token` HTTP twin did: sign
the domain-tagged handshake message (±30 s drift) → opaque token, 1-hour TTL,
preserving that path's side effects (account lockout; an unregistered actor is
refused `fauna.auth.not_registered` in every registration mode — the
auto-registration branch was removed 2026-07-12, owner
[`nest/public-mode.md`](nest/public-mode.md) § Registration Modes; new-IP detection, on
`auth.verify` as well as `auth.handshake` since 2026-09-24: the SNI router
conveys the peer IP via PROXY-v2 and the dispatcher hands it to the handler
through `dispatch_core::current_caller_ip`). `auth.handshake` is now the
**sole** direct-auth transport — its HTTP twin was deleted at the rip-out
endgame. See `login.md` for the ceremony wire format. The client then opens
the authenticated `GET /api/v1/ws/{actor_id}` with
`Sec-WebSocket-Protocol: fauna.v1, bearer.<token>` — the existing handshake,
unchanged. The bearer is the bridge between the two connections; the
anonymous connection may be closed once the bearer is in hand, or kept open
for further discovery.

*Why two connections, not one upgraded in place:* each connection stays
single-purpose — anonymous-allowlist vs. authenticated-and-actor-keyed —
so the actor-keyed machinery (push routing, per-actor idempotency cache,
`actor_known` telemetry, the 4401-reauth close semantics) never has to
handle a mid-stream identity transition, and the bearer remains the one
universal credential (the bridge daemon and every app's refresh path
are unchanged). This mirrors the existing two-step "mint a credential,
then open the per-actor socket," and keeps the bearer-in-`Sec-WebSocket-Protocol`
handshake (§ Design decisions worth knowing) the *sole* way a connection
becomes authenticated. An in-place upgrade — binding the authenticated
actor onto the anonymous connection after `auth.handshake` — was
considered and rejected: it saves one TLS handshake at cold start but
adds a stateful anonymous→actor transition to every actor-keyed
subsystem. A later session may revisit if cold-start latency proves to
matter.

**Abuse posture.** The anonymous connection inherits the public-endpoint
posture of the HTTP routes it replaces. Per-source rate limits are enforced
**in nest**, keyed on the real client IP: on the single-box deploy the
`fauna-sni-router` fronting :443 conveys the original client address via a
PROXY-protocol-v2 header that `serve_tls` resolves into the connection source
(without it the L4 splice would show the router's loopback address for every
client — thesecurity review finding). Every nest accept loop — the
TLS listener and the local-domain plain-HTTP listener ride **one shared loop**
(`serve_admitted`, behind `serve_tls` / `serve_plain`; ratified 2026-08-25, so a
defence added for one listener cannot be missing on the other) — applies a
**global** connection cap, a **per-IP** connection cap (keyed on that resolved
real client IP; a loopback source is counted against its own hard-coded
**loopback ceiling** rather than the admin's cap — *Loopback is bounded, not
exempt* below), a TLS handshake timeout, and a `header_read_timeout` (Slowloris
guard), and WS upgrades cap message/frame size at 2 MiB. **The native apps
(`fauna-client`, `fauna-anon-client`) apply the same 2 MiB cap on their
tungstenite `WebSocketConfig`**, so the limit is symmetric — a malicious or
compromised nest cannot force unbounded client-side buffering by sending an
oversized frame (tungstenite's default is 64 MiB). The cap
is single-sourced as `fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`; bulk
binary transfer rides separate HTTP channels (`/api/v1/chunks/…`), not a
WebSocket.

**Both universals above are over the class of listeners and upgrades, and the
class includes the degraded "needs-update" listener** (`degraded_serve.rs`,
[`version-compatibility.md`](version-compatibility.md) § 2.2) — the one a nest
runs on, unattended, for as long as its DB stays ahead of its binary. It is the
easiest one to miss: it opens no DB, builds no `AppState`, and was written as a
mirror of the normal connection loop's *framing* rather than of the accept
loop's *bounds*. Until 2026-08-29 it was outside both — its WS upgrade set no
size cap (taking the 64 MiB library default on a path that authenticates
nobody, since there is no DB to check a bearer against) and its plain-HTTP arm
was a bare `axum::serve`, the very shape the 16 311-socket incident below
retired. Both now hold: the upgrade takes the same single-sourced 2 MiB cap,
and both of its arms ride `serve_tls` / `serve_plain` like every other
listener. **When adding a listener or an upgrade, the enumeration that keeps
these two sentences true is mechanical — `rg 'on_upgrade|WebSocketUpgrade'` and
`rg 'axum::serve'` over `bins/` — and it is the check to run, rather than
trusting that a rule written as a universal was applied as one.**

> **✅ Resolved (2026-07-12) — see § Max frame.** The cap is permanent for
> every caller class (the cap stands, client half symmetric); payloads above a
> feature's inline ceiling ride the bulk-byte plane as references instead of
> the RPC plane. Mail's mechanics: `../behavior/smtp-server.md` § Message
> size limits.

The fronting `fauna-sni-router` applies the
**same** global + per-IP caps at L4 — it observes the real client as its TCP
peer directly — via the shared `fauna-conn-limit` crate (one shape, not a copy
per binary). The Go mail/CalDAV listeners also enforce the per-IP cap on the
real client IP (the MDA peels PROXY v2 on the SNI-routed CalDAV listener; IMAP/
submission are published directly), via the Go `internal/connlimit` analogue.

**Every per-IP permit has a bounded lifetime — dead-peer detection is part of
the cap, not an optimisation.** A per-IP cap counts *live* connections, so it is
only as correct as its release path: a peer that disappears without a clean TCP
close (a suspended machine, a dropped link, a killed process, a NAT rebind)
leaves a socket `ESTABLISHED` indefinitely and burns one of that IP's slots
permanently. Accumulated, this locks a source out entirely while every component
looks healthy — the failure mode is an accept-time shed, i.e. TCP connects and
the server sends zero bytes and no certificate. Therefore **every accept loop
that takes a per-IP permit also arms TCP keepalive on the accepted socket**:
the Rust callers via `fauna_conn_limit::arm_dead_peer_detection` (120 s idle,
30 s probe interval, 4 probes ⇒ reaped within ~4 min), the Go listeners via the
runtime's own listener default (15 s idle), pinned by tests on both sides.
Keepalive rather than an idle timer, because an L4 splicer cannot distinguish a
peer that is *gone* from one that is merely *quiet* — and quiet is legitimate
for parked CalDAV connections, idle relay subscriptions, and browser WS sockets
(browsers cannot send WS Ping frames from JS). This sits **below** the WS
heartbeat of § Connection lifecycle, which is faster but only reaches sockets
that have actually become WebSockets — every one the nest serves, fauna's own
protocol and the nostr relay alike, but not a raw TLS splice through the SNI
router nor a socket still in its pre-upgrade HTTP phase, where keepalive is the
only bound there is — and note that the browser objection above is an
argument against an *idle timer*, not against a Ping: the heartbeat's server half
reaches idle browsers precisely because the Pong is answered below JS. Shed events log at `warn`,
rate-limited to one line per minute carrying the batch count: a silently
shedding cap is indistinguishable from a quiet night, which is how this stayed
invisible in production.

**The printed identifier on a shed line is a sample, never an attribution.**
`fauna_conn_limit::ShedCounter` carries exactly one batch count, shared across
every key the ceiling meters (every source IP for a per-IP cap, every actor
for the anonymous-throttle gates below) — it has no per-key dimension. So a
line reading `shedding (sample: 203.0.113.5) (48210 shed since last line)`
means 48 210 sheds happened across every key this ceiling saw during the
window, of which `203.0.113.5` is one example — never that this one address
caused all 48 210 of them. Read a shed line as evidence a ceiling is active
and under how much load, not as the identity of whatever is causing it; a
`(sample: …)`-labelled line makes that distinction unmissable at the call site
rather than relying on the reader to already know the counter's scope.

**The global per-IP request-rate governor** (`bins/fauna-nest/src/rate_limit.rs`,
recorded here 2026-08-29 — it had no owner in any goal doc until then). A
`tower` layer over **every** HTTP route, keyed on the same resolved real client
IP the connection caps use, admitting **100 requests/second** per source
(`new_ip_limiter(100)`, wired once in `lib.rs`); over budget it returns a bare
`429` from the middleware, **before any RPC frame exists** — which is why it can
never produce the `fauna.protocol.rate_limited` RPC error the anonymous
sliding-window throttles emit (those are a different plane, owned by
[`federation.md`](federation.md): per-kind, per-source budgets on the
account-creating writes). governor's keyed limiter never sheds entries, so a
periodic `retain_recent` sweeper bounds the map — without it the table grows by
one entry per distinct client IP ever seen, which became a live leak the moment
the PROXY header started resolving real (and rotating) sources.

**The failed-credential throttle — a client that keeps presenting a refused credential is told to back off** (`bins/fauna-nest/src/failed_credential_throttle.rs`, 2026-09-27). The governor above admits 100 requests a second per source, so one misbehaving client far below that is never throttled by it: the 2026-09-24 incident was a sync agent holding a dead device credential that dialled a production nest about eight times a second for four days, every bearer upgrade answered `401`, every `fauna.auth.device_handshake` `not_registered`, and nothing told it to stop (the client half — the dial budget and the refused-renewal-is-terminal rule — is § *The dial budget* above). This is the server half, for every client bug the client half does not reach. **It counts refusals, never attempts**: a bucket spends only on an attempt that already failed, so a credential the nest accepts never meets it, and no flood from any source claiming any identity can make a legitimate credential's attempt fail that would otherwise succeed — which is what keeps it clear of the lockout argument below that leaves the auth-bootstrap ceremony otherwise unthrottled. **The key is the composite `(resolved source IP × claimed identity)`, never the bare claimed identity**: an actor id is public and a claimed one is unproven, so a bucket keyed on it alone would let a remote flooder claiming someone's actor turn that holder's own honest refusal (an expired bearer, answered `401` so it re-mints) into a hold; the source half confines each bucket to one address — the same unspoofable key every anonymous throttle uses (§ Pre-identity (anonymous) connection: *a flood throttles only the flooder's own source*), IPv6 on its /64 — and the claimed half splits a shared NAT so a broken neighbour's refusals never spend another identity's budget. Two surfaces, each its own bucket and its own shed reporter (a third, the removed `/sync/ws` upgrade, left with that route on 2026-10-02): the bearer upgrade `GET /api/v1/ws/{actor_id}` (claimed identity: the path actor) validates first and turns only a refusal past the budget into **`429 Too Many Requests` + `Retry-After`** — the answer the native client's dial budget holds every dial to the nest on (§ *A `429` on the upgrade holds every dial to that nest*); and `fauna.auth.device_handshake` (the claimed renewal device key) refuses a full bucket **before** the work with the RPC plane's `fauna.protocol.rate_limited` — before, because the refusal it would otherwise replace (`not_registered`) is terminal for a correct client while `rate_limited` is retryable, and safe before, because only refusals fill the bucket and a key with a live grant is never refused, so it fills only if someone on that key's own source presents the same unpublished key and fails, again and again. **Budget: 10 refusals per sliding 60 s per bucket, `Retry-After: 60`** — a correct client needs one or two (an expired bearer is refused once and re-minted; a dead grant is refused once and is terminal), ten still holds the incident's flood to its first second and a quarter, and the `Retry-After` is the window (the longest a full sliding window can take to regain room), pinned at compile time within the client's 15-minute hold cap. No harness arm: the tier_3 suite keys each of its buckets on an actor minted for the test, and a correct client never reaches the budget. Rust constants, not knobs — no user or admin would choose them (`principles.md` § One configuration surface). Sheds report through `fauna_conn_limit::ShedCounter` per surface, one `warn!` a minute with the batch count, the source printed as a `(sample: …)`. Swept every five minutes like every other limiter map. Pinned by the module's unit tests and end to end by `tests/e2e-unified/tests/api/test_failed_credential_throttle.py` (a dead-bearer flood answered `429` + `Retry-After` past the budget; the flooded actor's valid bearer still upgrading from the same address; a neighbour's bucket untouched).

**The governor's loopback exemption became a bounded ceiling on 2026-08-30**,
so this layer now matches the ruling below on both axes. It had short-circuited
on `!ip.is_loopback()`, leaving a same-box peer unmetered and — like the accept
loop before 2026-08-22 — silent while it flooded. Two asymmetries against a
ceiling were real and neither rescued the exemption: a request *flow* drains
when the caller stops, where a leaked *socket* does not (which is why the
2026-08-22 leak wedged the whole box for hours and a request storm would not),
and a co-resident process is already inside the trust boundary and can burn CPU
without going through nest at all. What survives both is that a loopback storm
degrades nest for **external** clients while producing not one line of evidence
— exactly the failure the shed-`warn` was added for.

The landed shape is `rate_limit.rs`'s `LOOPBACK_MAX_RPS` = **65 536 requests per
second, aggregated over every same-host caller** rather than keyed per loopback
address (`127.0.0.1`, `::1` and the rest of `127.0.0.0/8` are one set of
co-resident processes on one CPU; keying them would let a runaway reset its own
budget by picking a fresh address, and it would put the loopback path back into
the keyed map the sweeper above exists to bound). The constant's own doc comment
carries the sizing from both ends and the measurement behind it; what belongs
here is **why the number is generous**. The measurement that sized it also
refuted the premise this record was written under. It is not that no measurement
existed and the bridge's rate scales with mail volume: the bridge's WS-RPC plane
never re-enters this layer at all (this is an axum middleware, so it sees a WS
*upgrade* and none of the frames after it), and what does traverse it is the
bulk-byte plane, whose loops are serial and one-request-per-chunk. Measured on a
development VM, the legitimate aggregate **plateaus** near 1 650 req/s — concurrency does
not multiply it, because the box is bandwidth-bound — while the fastest a
runaway can drive the layer at all is about 2 500 req/s. **On loopback those two
numbers are within a factor of two of each other**, since both are bounded by
the machine rather than by the network. So a request-rate ceiling here cannot
separate "busy" from "pathological"; what separates them is connection count,
which `fauna_conn_limit::LOOPBACK_MAX_CONNS` already bounds. The ceiling is
therefore deliberately a **backstop against the aggregate**, sized ~40× above
the measured legitimate plateau — 64 requests per second per permitted loopback
connection, at the 1024-connection ceiling — and not a tight bound. Tightening
it to make it bite would throttle the deployment's own bridges, which is worse
than not having it.

**Loopback is bounded, not exempt (ratified 2026-08-25).** The co-resident
peers that reach nest headerless over loopback — the in-container bridges, the
router's own traffic, a same-box app — are the deployment artifact's own
processes, so they are *not* subject to the admin-tunable abuse cap (an admin
tightening it to a handful must never starve the bridge). But they are not
unbounded either: on 2026-08-22 one leaking same-box e2e client (an
anonymous-client `Drop` that never closed its WebSocket — since fixed) held
**16,311** accepted-and-never-closed loopback sockets on a nest that logged
*nothing* — the plain-HTTP listener was a bare `axum::serve` with no admission
at all, and the per-IP limiter exempted loopback outright — until the
machine's whole network state was exhausted (every new outbound TCP connection
box-wide failed for hours). So every loopback source is counted against
`fauna_conn_limit::LOOPBACK_MAX_CONNS` — **1024**: a quarter of the 4096
global default, and two orders of magnitude above the legitimate co-resident
fleet (the bridges dial nest once per *process*, not per user). It is a
**bucket-1 constant** (`../principles.md` § One configuration surface — nobody
chooses it: the legitimate loopback count is a property of the artifact, not a
preference), independent of the admin's cap in both directions (tightening the
cap never starves loopback; widening it never widens the safety bound). A shed
at the ceiling reports through the same rate-limited `warn`, naming the
ceiling kind and the source — the line that incident's nest never had. The Go
`internal/connlimit` analogue mirrors it (`LoopbackMaxConns`, applied even when
the mail cap is set to its `0 = disabled` sentinel); the SNI router inherits it
from the shared crate. Pinned by `fauna-conn-limit`'s
`loopback_is_bounded_by_its_own_ceiling` / `…_independent_of_the_admin_cap`,
the Go `TestPerIPLimiter_LoopbackBoundedByItsOwnCeiling`, and the nest's
`tests/plain_http_admission.rs` (a real plain listener sheds the connection
past a small loopback ceiling, and still serves HTTP, `ConnectInfo`, and WS
upgrades — the plain flavour no longer rides `axum::serve`).

**Permits ride the socket (found and fixed 2026-08-25).** Building that test
exposed a latent hole in the nest's existing caps: hyper's connection future
**resolves at a WebSocket upgrade**, handing the IO to the upgrade task — so a
permit held by the accept loop's connection task was released the instant a
client connection became what it is for the rest of its life, an upgraded
WebSocket. The global and per-IP caps on the TLS listener had therefore only
ever counted concurrent *handshakes*; every live client connection was
uncounted, and the 2026-08-22 leak's 16 k upgraded sockets would have held
zero permits under any ceiling. Both permits now live inside the socket
wrapper the nest hands to TLS and to hyper (`Admitted<IO>` in
`bins/fauna-nest/src/lib.rs`), so they are released exactly when the socket
is dropped — by the upgrade task, at the real end of the connection. The SNI
router never had this hole (its permit lives with the L4 splice, which *is*
the socket's lifetime). Pinned by the same `plain_http_admission.rs`: its
held connections are WebSockets precisely so the shed assertion witnesses
permits surviving the upgrade.

**The pre-resolution window is bounded too (fixed 2026-08-30).** The
**global** permit above is taken at accept — before the connection's source is
known — and the **loopback ceiling** only starts counting *after*
`read_optional_proxy_header` resolves it, up to 10 s later (the header-read
timeout). Between the two, a stalling loopback peer (send nothing, or the
single PROXY signature byte then stall) held a global permit uncharged
against `LOOPBACK_MAX_CONNS` — a newly-documented invariant the accept loop
didn't yet enforce. Widening the loopback charge to cover this window
by taking it on the raw TCP peer earlier was considered and rejected: on a
router-fronted box *every* external client arrives with a loopback TCP peer
(the router redials nest over loopback), so a provisional loopback charge
there would cap total external concurrency at the loopback ceiling — the
router-auth TLV that distinguishes router traffic from a bridge is inside the
very header being read. Instead, `read_optional_proxy_header` now acquires a
separate, much smaller `Semaphore`
(`fauna_conn_limit::HEADER_PARSE_MAX_CONCURRENT` = 512) with a non-blocking
`try_acquire` before the first byte is read; a full gate sheds the connection
immediately (a rate-limited `warn`, same shape as the other two shed lines)
rather than let it join a stall it can't yet be distinguished from. A
legitimate router-forwarded connection has its header already buffered before
nest even accepts the socket, so it clears the gate in microseconds — real
concurrent holders sit far below 512 even under heavy legitimate load. A
leaking co-resident process can therefore hold at most
`HEADER_PARSE_MAX_CONCURRENT` global permits pre-resolution *plus*
`LOOPBACK_MAX_CONNS` post-resolution — both fixed, hard-coded fractions of the
global pool, restoring what the loopback ceiling's own doc comment already
claimed. Pinned by `bins/fauna-nest/src/lib.rs`'s `proxy_header_tests`
module: a non-loopback peer never touches the gate (unaffected by its
saturation), a loopback peer is shed with a distinguishable error when the
gate is full, and a small explicit gate sheds its overflow connection within
seconds rather than the full 10 s parse window.

*Config source of the per-IP caps.* A per-IP connection cap is an admin-tunable
abuse knob (same class as spam thresholds), so by the product invariant it is
**app-set nest config**, not a deployment knob — with one deliberate
exception. (1) The **mail** per-IP cap is app-set + hot-reloaded today —
mechanism owned by `../behavior/mail-policy-config.md`. (2) The **nest-TLS transport** per-IP cap is
**app-set nest config** too: it lives in the nest-owned `transport_policy`
domain (`db::transport_policy`, distinct from the mail-scoped `db/mail_policy.rs`
domains the bridge consumes), set via the Admin RPC pair
`fauna.transport.{put,get}_policy`, read by the accept loops at **boot** and
**hot-reloaded** thereafter: the cap lives as an atomic on the
`AppState`-shared `PerIpConnLimit` every accept loop holds (one limiter object
across the TLS, plain, and internal-loopback listeners), and a `put_policy`
calls `set_max` on that same Arc so the change binds the live listeners on their
next connection — no nest restart, and the accept hot-path still does no
per-connection DB read. (This brings the nest-TLS cap level with the mail
per-IP cap, which was already hot-reloaded — same shape.) The cap has two
sources and no third (`resolve_tls_per_ip_cap`: the admin's row, else the
constant 256): the `FAUNA_MAX_TLS_CONNECTIONS_PER_IP` environment fallback was
removed 2026-10-01, with the global cap's `FAUNA_MAX_TLS_CONNECTIONS` (now the
constant 4096) — an abuse cap is an admin's in-app choice or a constant, never
a deployment variable. There is no dedicated app UI yet (admin-RPC floor, the
same shape the mail per-IP cap shipped with). (3) The **SNI-router** per-IP cap
(`--max-connections-per-ip`)
**deliberately stays a CLI knob** — the documented exception to abuse-knob →
app-set: the router is a dumb L4 splicer with no nest-config channel, so the
fine-grained per-IP abuse policy lives in nest (which sees the real client IP via
PROXY v2), and the router cap is a coarse front-door deployment backstop, i.e.
OS-deployment topology (ratified 2026-06-04). See the internal security
review notes (2026-06-01, tracked internally). The nest **refuses to serve the API over
plain HTTP when a public domain is configured** (a local domain — `localhost` /
LAN-IP / `.local` — still serves plain HTTP for dev and tier_3); HTTPS responses
carry an `HSTS` header (apex only, no `includeSubDomains`). Direct auth
(`fauna.auth.handshake`) makes a verified signature **single-use within its
±30 s window** (replay guard), matching the nonce-once property of the
challenge/verify path; the all-zero placeholder actor never resolves to a caller
class. It exposes no write surface beyond what the legacy public registration /
auth-bootstrap / discovery routes exposed unauthenticated (now the pre-identity
WS-RPC kinds).

**Single-use is the property of every signature-as-auth kind on this
connection, not just direct auth** (ratified 2026-08-31). The signed mode
commit — `fauna.setup.nat_mode` (its retired storage-mode twin shared the
shape until 2026-09-24) — authenticates
the same way ("the signature *is* the auth (no bearer): the signing actor must
be the committed admin"), so a verified blob authorises **one** commit, not
every commit inside its ±300 s freshness window. Without that, the wire triple
`(actor_id, timestamp, signature)` is a bearer token for the NAT posture:
`nat_mode`'s write path reconciles the MTA supervisor, so a replayed
private→public flip starts the perimeter SMTP parser on a box the admin
deliberately made private. The guard is the direct-auth one above (one store,
keyed on signature bytes, so contexts cannot collide), consulted **after** the
`is_admin` gate — only a real admin can add an entry — and a consumed signature
returns the *opaque* signature failure, never a distinguishable "already used",
so the connection is no replay-vs-forgery oracle. Two consequences worth
stating because they are easy to get wrong:

- **The memory outlasts the window by 2×, deliberately.** Freshness is a
  ±window, so a blob timestamped one window in the *future* is accepted now and
  stays fresh for two more; remembering it for only one window forgets it while
  it is still usable, which is precisely the gap a replay needs.
- **This does not make the kinds `forbid_replay = true`.** They stay `false`,
  as `fauna.auth.handshake` itself does — the assertion that flag carries is
  about an honest client's auto-retry, and an honest retry re-signs with a
  fresh timestamp. The one shape that is refused, an exact byte-identical
  re-submission, is the one an attacker replays and the one the admin surface
  already recovers from by resubmitting (its save control stays enabled on
  error).

**Binding the *nest* — the V2 form (ratified 2026-08-31).** A V1 signed
message names no nest, so one blob is valid at every nest where that actor is
admin — single-use closes same-nest replay, not the cross-nest arm. The
nat-mode commit therefore has a second, **nest-bound** form: the V1 body with
the target nest's identity appended (`SETUP_NAT_MODE_V2`;
`NatModeRequest.nest_id`), refused by any nest whose own identity it does not
name. Four decided pieces:

- **The binding is the nest identity** (the channel-binding `nest_actor_id` a
  client TOFU-pins — one key, by the single-identity unification), never the
  dialled apex: an apex binding silently degrades to V1 on every LAN-IP /
  `.local` / port / reach-hint dial, exactly the home deployments the NAT axis
  exists for, while the identity read below works on every dial shape.
- **The client's identity source is a possession-proven read off the same
  connection the commit rides** — `fauna.auth.nest_handshake` with a fresh
  client nonce, verified SPKI-bound against the received cert on native TLS,
  possession-only on wasm/plaintext (`nest_trust::read_login_binding`), run
  regardless of the graduation outcome (graduation's WebPKI/plaintext
  short-circuits are about channel trust, not about what is learnable). No
  prior pin or root is needed, and this is not "trusting the nest's claim": a
  box cannot claim an identity whose key it does not hold — and a relayed
  *genuine* identity still binds the blob to the real nest it names, which is
  the wanted property. Signing happens **after** the connection exists
  (`NestApi::submit_nat_mode` takes the secret + mode; the impl signs).
- **When the source is absent, there is no commit.** A nest that rejects the
  handshake kind, withholds the binding, or presents a binding that fails
  verification gets no signature: the read is the same
  `nest_trust::read_login_binding` every login signer binds with
  ([`../behavior/login.md`](../behavior/login.md) § Binding the nest), and
  its refusal is a terminal identity verdict, never a silent downgrade.
- **V2 is the only form (since 2026-09-24).** The unbound V1 form was verified
  alongside V2 through a transition window from 2026-08-31 — emission gated on
  a `NestHandshakeReply.nat_mode_v2` capability advert, a client signing V1
  when the identity source was absent, the window's residual cross-nest V1
  acceptance pinned by a test asserting it so the retirement would be a
  deliberate flip. It was: the login ceremonies took the same nest binding **in
  place** on 2026-09-23 with no V1 accept path, and the compat-remnant sweep
  removed the NAT-mode V1 arm — the verifier's V1 leg, the advert, the
  client's sign-V1 degrade and its NAT-mode-specific binding reader — the next
  day (`version-compatibility.md` § Dimension 2, the fourth write-off; the pin
  now asserts the refusal, `an_unbound_v1_blob_is_refused_at_every_nest`). The
  `SETUP_NAT_MODE_V1` tag stays registered so its bytes are never reused.

The storage-mode commit's V1-only twin was a shim for pre-retirement clients and
left with the same sweep ([`nest/storage-modes.md`](nest/storage-modes.md)).

### Channel binding — the nest proves itself to the client

For a self-hosted / LAN / `.local` nest with **no publicly-trusted TLS cert**,
the client provisionally accepts the self-signed cert (encrypt-only) and must
then authenticate that the channel terminates at the real nest. The *trust
model and rationale* — the two-axis design, why the binding is necessary even
with TOFU, the DNS-vs-TOFU identity root — live in
`docs/goal/architecture/security.md` § Transport trust. This section owns only
the **wire leg** that carries it.

The leg rides `fauna.auth.handshake` (the first pre-identity kind a client
sends), extending its request/reply:

- **Request** gains `client_nonce: bytes` (32 random bytes the client
  generates per connection).
- **Reply** gains `cert_binding: { nest_actor_id: string, spki_sha256: bytes,
  sig: bytes }` where `sig` is the nest's `nest_signing_key` Ed25519 signature
  over `spki_sha256 ‖ client_nonce`, and `spki_sha256` is the SHA-256 of the
  `SubjectPublicKeyInfo` of the cert **the nest is currently serving** (the
  nest computes this from its own on-disk cert, never from a client-supplied
  value — see the load-bearing subtlety in security.md § Axis 1).

The client recomputes `spki_sha256` from the cert **it received** during the
TLS handshake, verifies `sig` over `that_spki ‖ client_nonce` against
`nest_actor_id`'s public key, and checks `nest_actor_id` against its identity
root (DNS `self=` for public domains; the TOFU pin store for LAN/`.local`). A
verification failure tears the connection down before any bearer or request
is sent (security.md § Connection-teardown rule). On a public-CA-valid cert
the binding is belt-and-suspenders (WebPKI already authenticated the host);
on a self-signed cert it is the *sole* authentication, replacing the
MITM-open `FAUNA_INSECURE_TLS=1 → danger_accept_invalid_certs` path.

**Status — implemented (WS leg).** `HandshakeRequest` carries an optional
`client_nonce: bytes` and `HandshakeReply` an optional
`cert_binding: { nest_actor_id, spki_sha256, sig }`
(`libs/fauna-protocol/src/auth.rs`). The nest signs the live SPKI of the cert
its own listener serves (`acme::ServedCertSpki` on `ReloadableCertResolver`,
threaded into `AppState.served_cert_spki`) over `spki_sha256 ‖ client_nonce`
with `nest_signing_key` in `auth_handlers::build_cert_binding`; the binding is
omitted when the request carries no nonce or the nest has no served cert
(plain-HTTP dev). The **client** captures the received SPKI
(`fauna_anon_client::tls_verify`), sends the nonce from both native bearer paths
(`WsChallengeBearer`, `fauna-launch-machine`), verifies the binding + TOFU-pins
the identity (`fauna_anon_client::{cert_binding,trust}`), and requires the pinned
SPKI on the bearer connection (`ws_adapter`). The residual HTTP content API +
`POST /register` reqwest leg now shares that same pin
(`trust::store_pinned_reqwest_tls`, read per-handshake), so **`FAUNA_INSECURE_TLS`
is fully removed — from WS *and* HTTP paths**. See security.md § Implementation
status today (tracked internally).
