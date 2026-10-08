# Login / Auth — target state

Owns: login
Status: ratified
Authority: the auth model + ceremonies — Ed25519 identity shape (no passwords, actor id = pubkey), the three pre-identity WS-RPC kinds `fauna.auth.{handshake,challenge,verify}` (signed-message forms incl. the nest binding — § Binding the nest, which also governs the device and custody handshakes' binding — the `client_nonce` fold + the ReplayGuard single-use rule, drift window, TTLs, side effects, error-code table), and the bearer-token/session model (`fauna.sessions.{list,revoke,revoke_all,lockout}` + the pre-identity `fauna.account.lockout` recovery kind); the user-facing launch flow (routing, wizard exits, persistence shape) → [`onboarding.md`](onboarding.md); the credential-storage contract + per-platform routing → [`../architecture/apps/common.md`](../architecture/apps/common.md) § Credential storage; the three-slot identity store → [`../architecture/long-term-store.md`](../architecture/long-term-store.md); wire framing + the anonymous connection → [`../architecture/transport.md`](../architecture/transport.md) § Pre-identity; TLS channel binding → [`../architecture/security.md`](../architecture/security.md) § Transport trust; device-add → [`devices.md`](devices.md); test IDs → `tests/e2e-unified/ui.yaml`.

Last verified: 2026-09-24 | Sources: `bins/fauna-nest/src/lib.rs`, `auth_handlers.rs` + `auth_core.rs` (the `fauna.auth.*` kinds — the sole auth transport), `challenge_auth.rs` (`ChallengeStore`), `session_handlers.rs` (`fauna.sessions.*`), `account_core.rs` + `account_handlers.rs` (lockout; `register_core` — the registration door, § Errors), `invite_core.rs::submit_invite_request_core` (the other registration door) + `db/admin.rs::is_actor_registered` (what both doors read), `libs/fauna-protocol/src/auth.rs` (`expires_in`, `deadline_on_own_clock`, `BEARER_REFRESH_BUFFER_SECS`, `challenge_verify`), `libs/fauna-launch-machine/src/machine.rs` (`refresh_internal`, `ttl_refresh_loop`, `fresh_bearer`)

## Implementation status today

Every sign-in ceremony below is live end-to-end on all seven apps. **The session-management kinds are nest-only**: `fauna.sessions.{list,revoke,revoke_all}` and the two lockout kinds (`fauna.account.lockout` / `fauna.sessions.lockout`) are built and conformance-tested nest-side, but no client crate wraps them and no app ships a sessions list or a panic-button surface, so nothing calls them — the app half is designed and ratified 2026-09-25, its tui-first build queued ([`../ui/sessions.md`](../ui/sessions.md); behavior owner [`devices.md`](devices.md) § Session Management, whose § Implementation status today also carries the build status of the lockout gate this doc ratifies at § Silent Challenge — ruled 2026-09-25, built: the mint-time gate 2026-10-01 and its app half 2026-10-03) (the three non-lockout kinds were wrongly read as live here until the 2026-09-19 feature census — `rg 'fauna\.sessions\.' apps libs` finds only `libs/fauna-protocol`; the lockout half was established at the ruling, 2026-08-24 — which is also why their `duration_secs` could be retired to a hard-coded window with no compat impact, and later removed from the wire). The HTTP twins are deleted; no other gaps between target and code. New-IP detection is live on both WS mints since 2026-09-24 (§ the handshake's side effects), pinned by `routes::new_sign_in_notice_tests` through the real dispatcher. One extension built nest-side 2026-07-24: all three seed-signature ceremonies (and the emergency lockout) refuse an actor whose identity has been succeeded, with a `superseded` error naming the successor and where to fetch the statement — owner: [`identity-succession.md`](identity-succession.md) § Enforcement on the home nest. As of 2026-08-01 the refusal reaches the client intact on **both** mint paths — `WsChallengeBearer` (via its latch) and `LaunchMachineBearer` (via `fauna-launch-machine`'s terminal superseded state) — and stops the retry loop on each instead of being retried forever; **tui** renders the affordance, routing to the existing identity-import flow. The remaining per-app legs (linux's face, the five FFI/web apps, and naming the *verified* successor) are gated (tracked internally); the affordance's own contract is owned by [`succession-propagation.md`](succession-propagation.md) § Propagation.

**Every login signature names its nest — ruled and built in place 2026-09-23 (§ Binding the nest).** The nest verifies only the nest-bound forms of `fauna.auth.{handshake,verify,device_handshake,custody_handshake}` — no unbound accept path was kept, a user-ruled pre-user reset (the third one-time write-off, `../architecture/version-compatibility.md` § Dimension 2). **Built on every signer:** the four shared mints in `fauna_anon_client` (every native app, the sync agent, the custody client — and `bins/fauna-sync` until its removal, 2026-10-02), the launch machine's native connector, the wizard's silent challenge and the recovery readers (`fauna-onboarding-machine`), the web SPA's `challengeVerify` and one-shot handshakes (`fauna-wasm`), the Go mail bridge's bearer acquisition, and the Python e2e fixtures — each reads the identity through the one shared reader (`fauna_client_core::nest_trust::read_login_binding`, its Go and Python twins) before signing. **Witnesses:** the in-process refusal of a blob addressed to another nest on every ceremony (`conformance_auth.rs`, `auth_core::device_auth_tests`), the SPKI-compared native read refusing a substituted cert before any login is signed (`tls_channel_binding_roundtrip.rs`), the web pre-sign pin check (`nest_trust` `rotation_tests`), the Go fake-nest suite, and — since the nest verifies only the bound form — every green mint in the tier-3 suites. Retired with it: the never-sent `build_auth_request` builders (client-core, wasm, FFI) that signed the unbound handshake.

**Refresh under a wrong client clock — ruled 2026-09-21, built on every seat 2026-09-23; per-seat wrong-clock witnesses partly open.** § When to use which assigns every app-held bearer, refresh included, to the silent challenge, and § Token lifetime on the client's clock anchors every client deadline to the client's own clock over the additive `expires_in` reply field (nest-side on every mint). **Built:** the nest (`auth_core` mints carry `expires_in`; `auth_handlers` puts it on both replies); `fauna-launch-machine` — refresh is `AuthConnector::silent_challenge`, `TokenStatus::Valid` is on the machine's clock, the TTL loop floors its re-arm, `LaunchMachine::fresh_bearer` owns the spend rule (`fauna_nest_http::LaunchMachineBearer` reads it) — for **tui and linux**; the shared mint `fauna_anon_client::mint_bearer_over_silent_challenge` (over the one raw ceremony `fauna_protocol::auth::challenge_verify`, which the launch path's classifier also rides), which `fauna-client`'s `WsChallengeBearer` + `TokenCache` and the `fauna-ffi` `mint_bearer` export use — so the four UniFFI apps hold to it (as did `bins/fauna-sync` until its removal, 2026-10-02); and **web**, whose `getAuthToken` re-mints through wasm `challengeVerify` (the `mintBearer` export is deleted) and caches `now + expires_in` on both mint paths (`token-deadline.ts`). Every `fauna_anon_client` mint — the handshake family's included — anchors `MintedBearer::expires_at` at receipt, so no holder above it can compare the nest's clock against its own. **Witnesses:** launch-routing smoke case M for the ahead direction — tui, linux, web, macos and ios (2026-09-29) — whose offset reaches every seat's refresh clock because every bearer holder anchors and compares on the one client clock `fauna_protocol::client_clock` (the launch machine through `launch_clock`, `fauna-anon-client`'s mint anchor, `fauna-client`'s `TokenCache`; web's `clientNowSecs()` reads the launch chunk's copy); `bins/fauna-nest/tests/ws_challenge_bearer_roundtrip.rs` against a nest serving only the silent challenge (a handshake fallback is refused there); the anchor's both directions in `fauna-anon-client` and `token-deadline.test.ts`. **Open (tracked):** case M's arms for windows and android — the shared half is built (`FfiNestClient::launch_token_json_for_test` publishes the held `WsChallengeBearer`'s schedule, `refresh_held_bearer_for_test` forces its refresh, and macos/ios already consume both through FaunaKit); each of those two shells still has to publish the `launch_token` key and answer `launch_refresh_token`, and android's run waits on its e2e venue. The lockout gap this widens is `devices.md` § Implementation status today item (3).

**The registration doors' accepted exception to the opaque code (§ Errors, ruled 2026-09-24) is the code as it stands** — both doors already refuse every `users` row through `db::is_actor_registered`, which reads no standing on purpose, so no nest change landed with the ruling; the four door pins (both doors, the status read, the public handle probe) are in `bins/fauna-nest/tests/conformance_suspension.rs`, the mint's in `conformance_auth.rs::verify_rejects_suspended_actor`. **The app side is built (2026-09-25, shared Rust, all 7 apps through the shared snapshot):** the wizard maps a submit's `actor_exists` to the typed terminal `InviteRequestError::AlreadyRegistered`, rendered as its own sentence rather than a "try again" ([`onboarding.md`](onboarding.md) § 3 Invite request); pinned by `machine_lifecycle.rs::invite_submit_already_registered_is_terminal_and_localized` and, against a real nest, `onboarding_ws_rpc_roundtrip.rs::invite_request_submit_by_registered_actor_maps_to_already_registered`.

---

## Goal

A single Ed25519-keypair-based auth model. No passwords, no
derivation, no MFA. The actor ID IS the public key. Two ceremonies carry
every authenticated session, both now pre-identity WS-RPC kinds on the
**anonymous WS connection** (every auth HTTP twin has been deleted): a
two-round-trip silent-challenge ceremony — `fauna.auth.challenge` +
`fauna.auth.verify` — for **every bearer an app holds**: the launch fast path
(which returns cached `handle`/`domain`/`tier` metadata in one ceremony) and
every later refresh of that bearer, because the ceremony is immune to client
clock skew and a signed-in device must stay signed in on a wrong clock, not
only sign in on one; and a stateless single-round-trip direct auth —
`fauna.auth.handshake` — for tests, scripts and machine-to-machine callers whose
clocks are not a user's device.

---

## Identity

All apps use **Ed25519 key-based authentication**. There are no
passwords. Identity is a 32-byte Ed25519 signing key (64 hex chars).
The actor ID is the corresponding public key.

| Concept | Value |
|---------|-------|
| Secret key | 32 bytes (64 hex chars) — Ed25519 signing key, never sent off-device |
| Actor ID | 32 bytes (64 hex chars) — Ed25519 public key |
| Bearer token | Opaque `{actor_hex}.{random_64_hex}`, 1-hour TTL |

Generation is `ActorKeypair::generate()` in `libs/fauna-core` (32
random bytes → public key). Import is the user pasting the 64-char hex
secret. Same shape, no derivation.

Per-platform key storage is in [Per-platform key storage](#per-platform-key-storage) below.

---

## Two auth endpoints

> **Migrated to WS-RPC — COMPLETE (CBOR-DAG-everywhere Layer 5 / WS-RPC-everywhere
> Track A).** All three ceremonies below are pre-identity WS-RPC
> kinds — `fauna.auth.handshake` (direct auth, replaced `/auth/token`),
> `fauna.auth.challenge` + `fauna.auth.verify` (silent challenge) — on the
> **anonymous WS connection** specified in
> [`../architecture/transport.md`](../architecture/transport.md)
> § Pre-identity (anonymous) connection. The wire ceremony (signed message,
> drift window, reply fields, side effects) is preserved exactly as
> documented below; only the transport moved (HTTP JSON → DAG-CBOR WS-RPC
> frame). `auth.handshake`/`auth.verify` mint the same opaque bearer,
> and the client then opens the authenticated `GET /api/v1/ws/{actor_id}`
> with that bearer — the bearer remains the one universal credential.
> **Built nest-side (Track A1, 2026-05-23):** the three kinds run on the
> anonymous WS connection via the shared `auth_core` ceremony, and **every auth
> HTTP twin has since been deleted** (the last, `/auth/token`, at the rip-out
> endgame 2026-06-13).
> **App adoption — all three ceremonies are WS-RPC end-to-end.**
> `fauna-client`'s `AuthClient` mints its bearer over `fauna.auth.{challenge,verify}`
> (`libs/fauna-client/src/ws_challenge_bearer.rs::WsChallengeBearer`), so the
> native UniFFI apps (`FfiNestClient` → windows/macOS/iOS/android, via the
> shared FFI `mint_bearer`) and `bins/fauna-sync` never touch HTTP `/auth/token`
> (tracked internally § S3 step A part 1). And
> `fauna-launch-machine` (the `apps/fauna-linux` and `apps/fauna-tui` launch
> flows, each wiring its own `LaunchMachineBearer`) drives **all three**
> ceremonies over the anonymous WS connection via its `AuthConnector` seam
> (`WsAuthConnector` → `fauna_anon_client` / `fauna_rpc_wasm`): the silent
> challenge over `fauna.auth.{challenge,verify}` for the launch AND, since
> 2026-09-21, for the runtime/401-reactive refresh (§ When to use which; the
> machine's handshake refresh path is deleted). No client↔nest auth path touches
> HTTP. Both mints run new-IP detection on the WS path (§ the handshake's side
> effects, *New-IP detection*).

### Binding the nest — every login signature names the nest it is addressed to (ratified 2026-09-23)

A login signature that names no nest is a bearer mint at **every** nest where its signer is known. That is the designed shape of linked nests ([`linked-nests.md`](linked-nests.md): the user's one identity, registered on both), and more generally of any nest a client signs into — so a nest the user signs into could forward the blob and hold a bearer for the user elsewhere, with no TLS or WebPKI break needed: the receiving nest holds the signature in application plaintext. The silent challenge made that window continuous rather than one-shot (every app-held refresh, about hourly per seat, is such a signature), and it bypasses the per-scope capability-grant model (`../goal/principles.md` § The user always controls their data): what a box may do on the user's behalf is the set of grants the user minted, never "everything, because the user once signed in there". So:

- **Every bearer-minting signature binds the receiving nest's identity.** `fauna.auth.handshake` signs `actor_id ‖ timestamp_be ‖ nest_id ‖ client_nonce`, `fauna.auth.verify` signs `actor_id ‖ nonce ‖ nest_id`, and the device and custody handshakes bind `nest_id` before their nonce tail (the single-source builders in `fauna_protocol::auth`; tags `AUTH_HANDSHAKE_V2`, `AUTH_VERIFY_V2`, `DEVICE_HANDSHAKE_V2`, `CUSTODY_HANDSHAKE_V2` in `fauna_protocol::sig_domain`). Each request carries `nest_id` (64-hex) as a required field. **The nest requires it to be its own identity** (`AppState::bound_identity` — the key `fauna.auth.nest_handshake` proves) and refuses any other with `fauna.auth.invalid_request`, *before* any signature work; the signed message it verifies is built over its **own** identity, never the request's field, so a verifier that skipped the compare would still refuse the bytes. A request without `nest_id` is malformed.
- **The identity source is a possession-proven read off the same connection, before signing** — the same shape the nest-bound NAT-mode commit uses ([`../architecture/transport-connection.md`](../architecture/transport-connection.md) § Binding the nest): `fauna.auth.nest_handshake` over a fresh client nonce, through the one shared reader `fauna_client_core::nest_trust::read_login_binding` (the Go bridge and the Python fixtures carry byte-identical twins). **Native TLS compares the binding's SPKI against the cert this connection received**, so a relaying box cannot present the real nest's identity over its own channel — the relay is defeated. **Web and plaintext are possession-only** (a browser exposes no received cert; a loopback dev nest serves none). **On web** the client checks the read identity against its TOFU pin for the origin **before signing** — a relay presenting the real nest's identity is caught from the second contact on, and a first contact at a relaying origin is the same residual `../architecture/security.md` § Transport trust already documents for the web pin, now strictly narrower (it needs a relay *and* a first contact, where the unbound form needed neither). **Plaintext carries no pin check**: the native connector over `ws://` and the Python fixtures sign over the proven identity as read — the loopback dev/e2e nest's posture, where the channel never leaves the host. The Go mail bridge is possession-only with no pin on every endpoint: its loopback dial never leaves the host, and its non-loopback dial reaches its artifact-set endpoint under WebPKI verification. A box that proves no identity, or whose proof fails, gets **no** login signature; a refusal of the read classifies exactly as the ceremony's own would (a degraded nest's `fauna.nest.outdated` still reaches the update prompt).
- **No unbound accept path exists.** The unbound `.v1` forms were retired **in place** on 2026-09-23 by user ruling — no real users yet, the public repository's first release being minted the same day — the third one-time write-off recorded in `../architecture/version-compatibility.md` § Dimension 2; the retired tags stay registered in `sig_domain` so their bytes are never reused. The *mechanisms* for a future versioned upgrade (per-context tags, `NestHandshakeReply` capability adverts, the `extra` catch-all) are untouched; what was not kept is any accept path for a peer that predates this.

`bins/fauna-nest/src/lib.rs` registers **no** auth HTTP route — every auth
ceremony rides the pre-identity anonymous WS connection:

- `fauna.auth.handshake` — direct auth (`auth_handlers`, shared
  `auth_core::direct_auth_core`). Its `POST /api/v1/auth/token` HTTP twin was
  **deleted at the rip-out endgame** once every app minted over the kind.
- `fauna.auth.challenge` — nonce issuance (`auth_handlers`, shared
  `auth_core::issue_challenge_core`).
- `fauna.auth.verify` — nonce-signed token issuance (`auth_handlers`, shared
  `auth_core::verify_core`).

The `POST /api/v1/auth/{token,challenge,verify}` HTTP twins were all **deleted**
in the WS-RPC-everywhere rip-out once their consumers migrated; the
`ChallengeStore` the challenge/verify twins used backs the live kinds.
Challenge + verify mint every bearer an app holds (launch and refresh);
`handshake` is for callers that are not a user's device (tests, scripts,
machine-to-machine) — comparison table at end.
The ceremony semantics below describe the shared `auth_core` core; it is
identical across the three WS-RPC kinds.

### Direct Auth — `fauna.auth.handshake` (formerly `POST /api/v1/auth/token`)

Stateless single round-trip. **Target: tests, scripts and machine-to-machine
callers only** — no app-held bearer is minted here (§ When to use which; a
client timestamp the nest holds to ±30 s is the wrong freshness rule for a
user's device, whose clock may be hours wrong). Its callers are the ~20 test
fixtures minting via `common.auth.mint_token_via_handshake`, the shared
`fauna_anon_client::mint_bearer_over_handshake` (scripts, and one-shot mints
with no held deadline such as the onboarding recovery-config read) and the
web client's machine-to-machine one-shots (backup-destination resolve).
(The IMAP/SMTP bridge does not use this path — the Go `fauna-mail-bridge`
authenticates to nest over WS-RPC with its enrolled service-user keypair;
the legacy `fauna-bridge-daemon` consumer was deleted at the I6 cutover.)

Request:

```json
{
  "actor_id": "<64-char hex pubkey>",
  "timestamp": <ms-since-epoch>,
  "signature": "<128-char hex>",
  "client_nonce": <32 bytes>,
  "nest_id": "<64-char hex of the receiving nest's identity>"
}
```

Signed message: the domain-tagged `AUTH_HANDSHAKE_V2 ‖ actor_id_bytes ‖
timestamp_be_bytes ‖ nest_id ‖ client_nonce` —
`fauna_protocol::auth::handshake_signed_message(actor_id, timestamp, nest_id, client_nonce)`
(tagged-only; rule #8, `key-material-hierarchy.md`; `nest_id` per § Binding
the nest, read off the connection before signing). Every
`fauna.auth.handshake` signer (`fauna_anon_client::mint_bearer_over_handshake`,
web's `build_handshake_request`, the Python fixtures' `mint_token_via_handshake`)
folds a fresh 32-byte nonce in. Timestamp must be within ±30 seconds of server time
(`MAX_TIMESTAMP_DRIFT_MS = 30_000`). A verified signature is **single-use within
that window** (a `ReplayGuard` records it; the deterministic Ed25519 signature is
a unique token for its signed message), so an on-path attacker who captured one
request cannot replay it verbatim to re-mint a token — matching the nonce-once
property of the challenge/verify path (security review § L4). The
per-request nonce additionally lets **two legitimate concurrent same-actor
apps** coexist: without it, two apps signing the same `(actor, timestamp)`
in the same millisecond produce a byte-identical signature and the second is
wrongly rejected as a replay (the auth-handshake fix); distinct nonces ⇒
distinct signatures ⇒ both mint. The same nonce doubles as the TLS
channel-binding nonce (`security.md` § Transport trust, Axis 1).

Success reply (`HandshakeReply`):

```json
{
  "token": "<opaque>",
  "token_id": "<16-char hex>",
  "expires_at": <unix_seconds>,
  "expires_in": <s>
}
```

Token TTL: 3600 s (`TOKEN_TTL_SECS`). `expires_at` is on the nest's clock;
`expires_in` (added 2026-09-21; required since 2026-09-24) is the same deadline as seconds from the
reply — the client's scheduling input, § Token lifetime on the client's clock.
(The kind also returns an optional `cert_binding` — the TLS channel-binding
proof; `security.md` § Transport trust.)

**The client half of that lifetime is a pre-expiry buffer, and the two form an
inequality rather than a matched pair.** A client treats a cached bearer as
spent once it is within `fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS`
(60 s) of `expires_at`, so an in-flight request never presents a token that
lapses mid-flight; that one constant is read by every bearer cache and by the
launch-machine TTL-refresh loop. **A TTL at or below the buffer makes every
minted token born spent** — no cache ever serves one, every request re-mints,
and the refresh loop (sleeping `expires_at - buffer - now`) stops sleeping at
all. Each side stays internally consistent while this happens, so it is pinned
at compile time beside `TOKEN_TTL_SECS` rather than left to tests. The bulk-byte
plane runs the same client buffer against a 600 s TTL — a tighter margin, pinned
the same way.

**A cache can only decline to serve a token it already holds, so the contract
has a third participant: whatever mints the successor.** While an app runs that
is the app. App-dead it is the sync agent's own renewal loop
([`sync-agent-credentials.md`](../architecture/apps/sync-agent-credentials.md) § Credential model),
which mints `fauna_protocol::auth::AGENT_RENEW_LEAD_SECS` (300 s) before
`expires_at`. The loop always knows that deadline: every bearer an app hands
the agent carries its expiry on the machine's own clock
(`fauna_nest_http::BearerSource::bearer_with_expiry`), and a bearer whose
source cannot say it is not handed over at all — there is no blind cadence.
**The lead must also stay strictly below `TOKEN_TTL_SECS`.** Above the TTL
every minted bearer is born already inside its own renewal lead, so the loop
mints in a hot cycle from every enrolled machine. That is not observable from
either end (the agent renewed on schedule, the nest expired on schedule, and
no app is running), and the agent is a second binary that cannot see
`TOKEN_TTL_SECS` at all, so the constant is owned in `fauna_protocol::auth` —
the one crate the agent and the nest share — and pinned beside
`TOKEN_TTL_SECS` exactly as the buffer is.

Side effects (all preserved exactly from the deleted HTTP twin, in the shared
`auth_core::direct_auth_core`):

- **New-IP detection.** If the client IP differs from the last recorded IP
  for this actor, the server fires `SecurityEvent::NewTokenIssued`; the first
  IP an actor is ever seen from records silently. The IP is the anonymous
  connection's recorded peer — the direct TCP peer, or the PROXY-v2 source
  behind the SNI router — which the dispatcher hands the handler
  (`dispatch_core::current_caller_ip`); the HTTP twin read `X-Forwarded-For` /
  the TCP peer. **`fauna.auth.verify` runs the same detection** (shared
  `auth_core::note_sign_in_address`, one last-IP record per actor across both
  mints), because the silent challenge carries every bearer an app holds: a
  sign-in from a stolen key lands there, not on the handshake. Consequence
  accepted with it: the silent refresh re-mints about hourly per seat, so a
  device whose address changes (a phone moving between networks) rings once
  per change; ruled "differs from the last recorded IP", not "never seen
  before", so a return to an earlier address rings too.
- **Account-lockout enforcement.** Yields `fauna.auth.account_locked` (the
  twin's 423 Locked, `locked_until` in the error details) if the account is
  locked. Enforced in `bins/fauna-nest/src/auth_core.rs` (the handshake-time
  `locked_until` check); the account is placed into lockout by
  `bins/fauna-nest/src/account_core.rs::lockout_core`, invoked via the
  pre-identity kind `fauna.account.lockout`.
- **Registration is always required.** An actor with no account is rejected with
  `fauna.auth.not_registered` — in **every** registration mode, on every nest.
  Holding a valid self-signed token proves key possession, never admission; an
  account is created only by the registration ceremony (`fauna.account.register`,
  which mints a handle), the admin claim, or an admin admitting a user. *(The old
  **auto-registration** behavior — an unregistered actor auto-created on first
  handshake whenever `require_registration` was false — is **removed** as of
  2026-07-12. It minted handle-less ghost accounts and could silently re-open even
  a `closed` nest. Owner of the registration posture: `../architecture/nest/public-mode.md`
  § Registration Modes.)*

Errors (the live kind returns `fauna.auth.*` `RpcError`s; the HTTP statuses are
the deleted twin's historical mapping):

| Status | `RpcError` code | Reason |
|---|---|---|
| 400 | `fauna.auth.invalid_request` | invalid actor_id hex / signature hex / public key, or a `nest_id` that does not name this nest (§ Binding the nest) |
| 401 | `fauna.auth.timestamp_drift` / `fauna.auth.signature_failed` | timestamp drift > 30 s, signature verification failed, or a replayed (already-used) signature inside the window (opaque — no replay-vs-bad-sig oracle) |
| 403 | `fauna.auth.not_registered` | actor not registered on a private nest, **or suspended on any nest** (opaque — no suspended-vs-unregistered oracle; [`admin.md`](admin.md) § 2 Users → *Cutting a user off*) |
| 423 | `fauna.auth.account_locked` | account locked (`locked_until` in error details) |

**The registration doors are the accepted, stated exception to the opaque code
(ruled 2026-09-24).** `fauna.account.invite_request.submit` and
`fauna.account.register` refuse *every* actor that already holds a `users` row —
a suspended one included — with `fauna.account.actor_exists`, the same code an
active account gets and never a suspended-specific one (`db::is_actor_registered`
reads no standing on purpose). Opacity is not available at a door: a
never-registered actor's submit *succeeds* (a pending row) and its open-mode
register *mints* the account, so hiding a suspended actor's standing there would
mean either accepting the request — a second admission path into an account that
already exists, whose one exit is the admin's Restore
([`admin.md`](admin.md) § 2 Users → *Cutting a user off*) — or faking a success
with no row behind it, a lie the applicant then polls against for ever. Both are
refused. What the refusal discloses is bounded three ways: the doors are
Ed25519-signed by the actor, so only the key holder learns it, and only about
their own account; the launch flow never sends a stored identity down this path
([`onboarding.md`](onboarding.md) § App-launch routing → *the previously-signed-in
row* — a refused sign-in lands the retry surface, and "Use a different nest" →
`handle_entry` is the one route back into the wizard); and the bit is already
public in a wider form — `fauna.handle.available` reports a suspended user's
handle taken and `fauna.actor.by_handle` resolves it, exactly as for an active
user. The two adjacent surfaces do **not** leak, measured 2026-09-24: the
wizard's handle check rides `fauna.auth.challenge`/`verify` and no handle kind
(`fauna-onboarding-machine`'s one handle-keyed call is `fauna.actor.by_handle`,
on the phrase-only restore, which resolves a suspended user exactly as an active
one), so a suspended identity classifies
exactly as an unregistered one; and `fauna.account.invite_request.status` answers
`fauna.account.invite_request_not_found` to an invite-admitted actor whether
suspended or not, because approval *consumes* the request row. In one sentence:
**suspended-vs-unregistered is opaque at the mint (handshake, challenge, verify);
the registration doors refuse a suspended key holder as already registered, and
that is the whole disclosure.** Pinned in
`bins/fauna-nest/tests/conformance_suspension.rs` (both doors, the status read,
the public handle probe) and `conformance_auth.rs::verify_rejects_suspended_actor`
(the mint).

### Silent Challenge — `fauna.auth.challenge` then `fauna.auth.verify`

Two-round-trip ceremony for **every bearer an app holds**. At app start, when
the local identity store has both `secret` and `nest_url`, it is the launch
fast path: it returns the bearer token *plus cached metadata* (`handle`,
`domain`, `tier`) in one ceremony so the client doesn't need a follow-up call
to populate "Welcome back, @alice@nest.example". Every later refresh of that
bearer — TTL-scheduled or 401-reactive — is the same ceremony again (the
metadata is then ignored; the launch owns the cached identity), because it is
the one ceremony a wrong client clock cannot refuse (§ When to use which).

> **Transport:** pre-identity WS-RPC kinds `fauna.auth.challenge` /
> `fauna.auth.verify` on the anonymous connection (`auth_handlers`, shared
> `auth_core`). The `POST /api/v1/auth/{challenge,verify}` HTTP twins were
> **deleted** in the WS-RPC-everywhere rip-out. The request/response JSON shown
> below is the deleted twin's wire form, kept here as the canonical *field*
> reference — the live kinds carry the same fields as DAG-CBOR, and the HTTP
> status codes below map to `fauna.auth.*` `RpcError`s (404-not-registered →
> `fauna.auth.not_registered`, 401 → signature-failed, etc.).

**Step 1 — `fauna.auth.challenge`:**

```json
{ "actor_id": "<64-char hex pubkey>" }
```

Response (200):

```json
{ "nonce": "<64-char hex>", "expires_in": <s>, "expires_at": <unix> }
```

Nonce TTL: 300 s (`CHALLENGE_TTL_SECS`). The in-memory store is keyed
by the random nonce, so an actor may hold several outstanding nonces
at once — two apps of the same actor (the same identity launching
in two tabs / on two devices) that request a challenge concurrently
each get an independent nonce, neither clobbering the other's. Each
nonce is single-use (consumed on verify) and re-checked against its
owning actor; unconsumed nonces simply expire at TTL.

**Step 2 — `fauna.auth.verify`:**

```json
{
  "actor_id": "<64-char hex pubkey>",
  "nonce": "<64-char hex>",
  "signature": "<128-char hex>",
  "nest_id": "<64-char hex of the receiving nest's identity>"
}
```

Signed message: the domain-tagged `AUTH_VERIFY_V2 ‖ actor_id_bytes ‖
nonce_bytes ‖ nest_id` — **no timestamp**
(`fauna_protocol::auth::challenge_verify_signed_message`; `nest_id` per
§ Binding the nest, read off the connection before the challenge is even
requested). The server-issued nonce supplies freshness, so this flow is
immune to client clock skew (relevant on freshly-booted devices whose NTP
hasn't run yet).

Success response (200):

```json
{
  "token": "<opaque>",
  "token_id": "<16-char hex>",
  "handle": "<handle or empty>",
  "domain": "<nest domain>",
  "tier": "<tier name>",
  "expires_at": <unix_seconds>,
  "expires_in": <s>
}
```

`expires_at`/`expires_in` carry the same contract as the handshake reply's
(§ Token lifetime on the client's clock): the client schedules on `expires_in`
anchored to its own clock, never on `expires_at` against its own clock.

The `domain` is the deployment's **identity domain** — `AppState::handle_domain()`,
the projection of the primary `mail_domains` row set at claim (**not** the stale
`--handle-domain` boot seed, which left a domainless-then-claimed box reporting
`localhost`). A handle is addressable on every active local domain but is reported
here under this one canonical identity domain; see
[`mail-multidomain.md`](mail-multidomain.md) § Multi-domain handles.

Like the handshake reply, the live kind also returns an optional `cert_binding`
— the TLS channel-binding proof, symmetric with the handshake path (`security.md`
§ Transport trust) — omitted from the JSON above as it's covered there.

Errors:

| Status | Reason |
|---|---|
| 400 | invalid actor_id / nonce / signature hex, **or a `nest_id` that does not name this nest** (§ Binding the nest — the live kind's `fauna.auth.invalid_request`) |
| 401 | signature verification failed |
| 404 | actor not registered on this nest, **or suspended on any nest**, **or** nonce invalid/expired/already consumed |

The "actor not registered" outcome (the deleted twin's 404; the live kind's
`fauna.auth.not_registered`) is the explicit signal the launch flow uses to drop
into the onboarding wizard at the `invite_request` stage, per
[`onboarding.md`](onboarding.md) § App-launch routing.

**Verify refuses a suspended actor** (built 2026-09-23) with that same opaque
`fauna.auth.not_registered` the handshake answers (§ Errors above: no
suspended-vs-unregistered oracle), so suspension's "no new connection
authenticates" ([`admin.md`](admin.md) § 2 Users → *Cutting a user off*) holds
on the mint every app rides. What the app then shows is owned by
[`onboarding.md`](onboarding.md) § App-launch routing → *the previously-signed-in
row* (designed 2026-09-24): a stored identity + `nest_url` refused with this code
by a claimed nest lands the sign-in-refused surface — never the invite wizard —
distinguishing on client-local knowledge only; the wire code stays opaque.

Beyond that refusal the challenge kinds carry **two** security side effects,
both shared with the handshake (§ the handshake's side effects): new-IP
detection, and — **ruled 2026-09-25 (user)** — **account-lockout
enforcement**: `fauna.auth.verify` refuses a locked account with
`fauna.auth.account_locked` (`locked_until` in the details), checked after
`refuse_if_superseded` and the suspension check in the same order
`direct_auth_core` uses, so a thief's own lockout never masks a superseded
refusal. **Admins are exempt (user ruling 2026-10-01)**, mirroring the use-time
exemption ([`../architecture/api-layers.md`](../architecture/api-layers.md)
§ Layer 1): a locked sole admin keeps the verify mint until a
cannot-lock-the-last-admin guard exists, and the exemption lifts at both doors together. Since every app-held bearer (launch and refresh) is minted here, this
is the only lockout gate any app passes, and it is what delivers the refusal to
the locked surface on every launch path ([`devices.md`](devices.md) § The locked
state). *Implementation status: the mint-time gate is built — `auth_core::verify_core`
reads `locked_until` (admins exempt), `SilentChallengeOutcome::Locked` carries it,
and the launch machine lands it terminal with `LaunchSnapshot::locked_until_secs`.
This is the mint half; the use-time half is the paragraph below, and the app
half — the machine's one refresh at `locked_until`, the bearer mints' locked latch
and the typed `FfiError` — is built too ([`devices.md`](devices.md) § The locked
state; § Implementation status today item (3)).* A bearer is refused at *use* regardless: every bearer door asks the actor's standing
([`../architecture/api-layers.md`](../architecture/api-layers.md) § Layer 1:
Core Client API → *What `caller_class_for_actor` refuses*). Neither path ever creates an account: registration is always a
separate, explicit ceremony (§ above).

### Token lifetime on the client's clock

**A client never compares the nest's absolute `expires_at` against its own
clock.** Every deadline a client derives from a mint — the TTL-refresh sleep,
the bearer cache's spend rule (`BEARER_REFRESH_BUFFER_SECS`), the own-session-id
pruning — is compared against the *client's* clock, so a nest-absolute
`expires_at` read there is wrong by exactly the device's clock error: hours
ahead saturated the launch machine's sleep to zero (a hot re-mint loop after
every successful refresh), hours behind served a token long dead. So both mint
replies carry `expires_in` (seconds from the reply, additive 2026-09-21) and the
client anchors it to its own clock **at receipt**:
`deadline = now_client + expires_in`
(`fauna_protocol::auth::deadline_on_own_clock`, the one conversion every bearer
holder makes; in `fauna-launch-machine` it is what `TokenStatus::Valid {
expires_at_secs }` holds). Nothing about the client's clock is corrected or
reported to the nest, and the nest's own freshness rules (the handshake's ±30 s
window, the challenge nonce TTL) are untouched — the client stops *using* a
clock the nest never read, that is all. `expires_in` is required — every nest
sends it (the older-nest fallback to `expires_at` retired 2026-09-24 with the
compat-remnant sweep; only an unreadable client clock still keeps the nest's
absolute deadline), and the launch
machine's refresh loop floors its re-arm after its own refresh at one buffer
interval, so no combination of clocks can make it mint faster than once per
`BEARER_REFRESH_BUFFER_SECS`. The ahead-direction witness is launch-routing
smoke case M (§ E2E test login).

### When to use which

| Path | Transport | Why |
|---|---|---|
| App launch — silent challenge | `fauna.auth.challenge` + `fauna.auth.verify` (WS-RPC) | Need handle/domain/tier in one round-trip + clean not-registered signal + clock-skew immunity |
| TTL-pre-expiry refresh during session | `fauna.auth.challenge` + `fauna.auth.verify` (WS-RPC) | The device that signed in on a wrong clock must stay signed in on it: the handshake's ±30 s timestamp would refuse the first refresh. Metadata ignored. (Ruled 2026-09-21; built on every seat — § Implementation status today) |
| 401-reactive refresh | `fauna.auth.challenge` + `fauna.auth.verify` (WS-RPC) | Same as TTL refresh |
| Headless sync agent's app-dead renewal | `fauna.auth.device_handshake` | A separate kind: [`../architecture/apps/sync-agent-credentials.md`](../architecture/apps/sync-agent-credentials.md). A headless box signs in through fauna-tui, which takes the app rows above ([`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md) § Headless deployment); the legacy `bins/fauna-sync` daemon, which minted through `WsChallengeBearer`, was removed 2026-10-02. |
| Tests, scripts, machine-to-machine | `fauna.auth.handshake` | No nonce ceremony (e.g. `common.auth.mint_token_via_handshake`); not a user's device clock |
| Device-add flow | `fauna.auth.handshake` | See [`devices.md`](devices.md) |

---

## Sessions

Tokens are tracked server-side in the token store. Session management is
WS-RPC: `fauna.sessions.list` lists the current actor's active tokens,
`fauna.sessions.revoke` revokes one by `token_id`, and
`fauna.sessions.revoke_all` invalidates every session except the one named
by `keep_token_id` — the connection drops the raw bearer after the upgrade
handshake, so the client supplies the `token_id` it learned at mint (the
`fauna.auth.{handshake,verify}` reply returns `token_id` alongside the
token). The HTTP twins (`GET /api/v1/account/sessions`,
`DELETE …/{token_id}`, `POST …/revoke-all`) were **deleted** in the
WS-RPC-everywhere rip-out (`lib.rs` § deletion notes). Emergency lockout is
two kinds: the authed `fauna.sessions.lockout` disables auth for the
hard-coded 24-hour window,
and its **pre-identity twin `fauna.account.lockout`** — an Ed25519-signed
request on the anonymous WS connection (`account_core.rs`) — is the recovery
channel for when a secret leak is suspected and no authenticated session
exists (the former no-bearer `POST /api/v1/account/lockout` route was
deleted in the same rip-out; the signed ceremony moved onto the anonymous
connection unchanged). Lockout window semantics: ±300 s request-timestamp
window; the lock lasts a hard-coded 24 hours — the request carries no
duration.

---

## Per-platform key storage

What auth stores durably: the Ed25519 secret, plus the cached login
metadata the silent challenge refreshes (`handle`, `domain`, `tier`) and
the nest URL (the web slot names are listed in
[`../architecture/apps/web.md`](../architecture/apps/web.md) § localStorage Keys). **Where** each platform
stores it — the cross-app contract (platform secure store, never
plaintext on disk) and the per-platform routing table — is owned by
[`../architecture/apps/common.md`](../architecture/apps/common.md)
§ Credential storage (registry: `credential-storage`); each app doc
owns its platform mechanics.

Long-term store schema (identity + cached metadata + pending-invite
slot) is documented in
[`../architecture/long-term-store.md`](../architecture/long-term-store.md).
The pending-invite slot shape is in
[`onboarding.md`](onboarding.md) § Long-term store contract.

---

## Keypair generation

| Platform | Implementation |
|----------|---------------|
| Web | WASM (`generateKeypair()` from `libs/fauna-wasm`) |
| Windows | C# `CryptoService`, delegating all cryptographic operations to the uniffi-generated fauna-ffi bindings (byte layouts guaranteed to match Apple/Android/Linux/Web/tui — `FaunaApp.Core/Services/CryptoService.cs`) |
| Android | UniFFI wrapper around Rust |
| iOS / macOS | FaunaKit (UniFFI wrapper around Rust) |
| Linux | Direct Rust (`fauna_core::identity`) |
| tui | Direct Rust (`fauna_core::identity`) |

---

## E2E test login

`tests/e2e-unified/actions/auth.py` and `conftest.py` fixtures:

- `nest_instance` — starts a nest via the bridge HTTP API; tests get
  its URL.
- `app` — fresh launch (shows identity-choice screen).
- `logged_in_app` — bypass path: identity + nest_url + token are
  seeded into the app's long-term store via the test-state protocol;
  the app skips the wizard and renders `feed-view`.
- **A wrong client clock is a launch parameter, never the box's clock.** The clock-skew immunity above is witnessed by launching an app whose launch clock is hours wrong: `libs/fauna-protocol/src/client_clock.rs` is the client's one `now` — every app-held bearer's deadline anchoring (§ Token lifetime on the client's clock), session-id pruning and refresh schedule, read by the launch machine (as `launch_clock`), `fauna-anon-client`'s mint anchor and `fauna-client`'s `TokenCache` alike, so the offset reaches the refresh on every seat and a witness can never pass on a clock the refresh does not read — plus a compile-gated offset seeded from `FAUNA_E2E_CLOCK_OFFSET_SECS` on first use (web: the same-named localStorage key, seeded into the launch chunk and read back by the SPA's `clientNowSecs()`). It is not the nest's `Timestamp` and the nest never reads it, and it stays separate from the per-domain e2e clocks (`audit_clock`, `delegation_clock`, …) (convention 15; absent from a release build). Two witnesses in `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py`: **case L** (six hours *behind*) — the launch signs in through the real silent challenge, the app's own clock is read back as the wrong one, and the same skew signed into `fauna.auth.handshake` is refused with `fauna.auth.timestamp_drift` on the same nest, so the green is a claim about which ceremony launch takes, not about a tolerant nest; **case M** (six hours *ahead*) — the app's `launch_token` state (the bearer it actually holds and refreshes) shows the bearer's deadline a full TTL away on its own clock (the scheduling leg; the pre-ruling shape reads about −6 h and spins), then the refresh is forced through the production refresh path (`launch_refresh_token`) and must keep the session (no `session_generation` move, no error surface) while the app records exactly one new own session and the nest's `fauna.sessions.list` holds it (the ceremony leg, observed from outside).

### Per-app exceptions

*(`architecture/e2e-conventions.md` § Cross-app e2e conventions, point 4, points here.)* The login
**flow** itself has no per-app exceptions — the two-phase key-based
ceremony (identity, then nest) is identical on all seven apps, with all
IDs in ui.yaml's `onboarding` section. The only per-app variance is the
**e2e bypass mechanism** (how a test seeds a logged-in state): Windows
accepts `--reset --nest-url URL --secret HEX` CLI args; Apple/Linux/Android/tui
receive state via the bridge protocol; web seeds `localStorage` via the
test-state protocol. Per-app details in
`docs/goal/architecture/apps/`.

API-only auth tests live at
`tests/e2e-unified/tests/test_api_helpers.py:test_auth_token` and the
cross-nest tests at `tests/e2e-unified/tests/api/test_cross_nest_api.py`.
