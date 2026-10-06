//! Per-source rate limiting for the anonymous (pre-identity) discovery surface.
//!
//! The anonymous WS connection (`GET /api/v1/ws`, no bearer) exposes a fixed set
//! of world-reachable kinds; the subset this limiter bounds is
//! `pre_identity_allowlist::is_throttled_anonymous_kind` (which owns the set and
//! the per-class rationale). Two classes ride it:
//!
//! * **Unsigned directory/metadata oracles** (`fauna.nest.info`,
//!   `fauna.handle.available`, `fauna.nest.resolve`, `fauna.actor.by_handle`,
//!   plus the two public recovery directory reads). Because a nest signature is
//!   *attribution, not authorization* and these kinds need no signature at all,
//!   **rate limiting is the primary DoS / enumeration defense**
//!   (`docs/goal/architecture/federation.md` § Security). `by_handle` is a
//!   handle→actor_id oracle (directory harvesting), `handle.available` its
//!   inverse, and `nest.resolve` triggers DNS/SRV work (amplification).
//! * **Signature-bound recovery ceremonies** (seed escrow, the replacement veto,
//!   succession submit, the emergency `fauna.account.lockout`), where the limit
//!   bounds the **verification work an anonymous source can conscript** — the
//!   signature check *is* the unmetered work, so signature-gating argues for a
//!   throttle rather than against one.
//!
//! All are bounded here, per **source** and per **kind class** — on **every
//! connection class**, which is not what this module used to do.
//!
//! ⚠ **The bound is on the KIND, never on the connection class (fixed
//! 2026-08-24).** Every kind guarded here is a *pre-identity* kind, and the
//! dispatcher deliberately lets an **authenticated** connection call
//! pre-identity kinds (`routes.rs`, gate (1d): *"an authed connection may still
//! call them"*). While these gates were additionally scoped `conn.anonymous
//! && …`, one account of any tier converted all 18 guarded kinds from
//! "60 events / 60 s per source" to **unlimited on a single connection** — the
//! unmetered Ed25519 verifies, the unbounded nonce mints and the directory
//! enumeration alike. Widening the condition alone would have been a silent
//! no-op: an authenticated connection carries `peer_addr: None` (see below) and
//! [`check`] fails open on `None`. So the fix is a *key*, not a predicate —
//! [`check_conn`] is the only door the dispatcher should use, and it picks the
//! bucket from the class instead of skipping the check. Owner:
//! `docs/goal/architecture/federation.md` § Security.
//!
//! **Source identity.** For an anonymous connection the bucket key is
//! `RpcConnection.peer_addr`, the real client IP. On the single-box deploy the `fauna-sni-router` fronts :443 and
//! prepends a PROXY-protocol-v2 header conveying the original client address;
//! `serve_tls` parses it (trusted only from the loopback router) and injects the
//! resolved address as `ConnectInfo` via the `WithConnectInfo` wrapper (`lib.rs`).
//! Without the PROXY header the L4 splice would show the router's loopback
//! address for every client, collapsing all per-source buckets into one. A future off-host load
//! balancer would need an explicit trusted-proxy allowlist before its conveyed
//! source could be trusted (`read_optional_proxy_header` only trusts a loopback
//! peer today).
//!
//! For an **authenticated** connection the key is `RpcConnection.actor_id`, in a
//! namespace disjoint from the IP keys ([`ACTOR_SLOT`]). That is the right
//! source there on both counts: an authenticated connection has **no**
//! `peer_addr` at all (the authenticated upgrade captures no `ConnectInfo` —
//! `ws.rs`), and the account, not the address, is the scarce thing an
//! authenticated caller must spend — IP keying would also collapse every user
//! behind one NAT into a single bucket. Unlike an IP, an abusive account is
//! independently revocable (lockout / suspension).
//!
//! ⚠ **A `None` peer still fails open, and that is now a genuinely
//! test-only path.** It is reachable only for an *anonymous* connection with no
//! `ConnectInfo`, which the production plain and TLS listeners both always
//! supply. Before 2026-08-24 this sentence said "only in unit tests" while every
//! authenticated connection in production took it — the premise that made the
//! fail-open look safe. Authenticated callers no longer reach it at all: they
//! key on `actor_id`, which is never absent.
//!
//! Reuses `bridge_rate_limit::Limiter` (the same sliding-window type Y2's
//! `federation_rate_limit` reuses) rather than introducing a second limiter
//! implementation: the IP is packed into the limiter's first 32-byte key slot,
//! the second slot is unused (zero), and the kind string is the credential slot.

use std::net::{IpAddr, SocketAddr};

use crate::bridge_rate_limit::{Limiter, LimiterConfig};

/// The unused second key slot of the reused `Limiter` (its bucket key is
/// `([u8; 32], [u8; 32], String)`; the anonymous surface keys on IP + kind only).
const UNUSED_SLOT: [u8; 32] = [0u8; 32];

/// Namespace tag occupying that second slot for the **authenticated** arm's
/// buckets, so an `actor_id` can never land in the same bucket as an IP key
/// (`ip_key_bytes` zero-pads, so a crafted actor id colliding with a padded
/// address is not a risk worth leaving to arithmetic). Anonymous buckets keep
/// [`UNUSED_SLOT`], so every pre-existing bucket key is byte-identical to what
/// it was before the authenticated arm existed — this change tightens the
/// authenticated class and moves nothing for the anonymous one.
const ACTOR_SLOT: [u8; 32] = [1u8; 32];

/// Default window for the anonymous discovery surface: 60 events / 60 s **per
/// source IP per kind**, tight enough that bulk directory harvesting from one
/// source is bounded to 60 lookups/min. Tunable — the limiter is constructed
/// `with_config` in `AppState`.
///
/// ⚠ **Re-sighted 2026-08-29 and deliberately left at 60.** This budget was
/// originally sized for "a handful of `nest.resolve`/`nest.info`/`by_handle`
/// calls per session, plus a client resolving several recipients". That
/// assumption no longer describes the traffic: the recipient picker resolves a
/// foreign handle **on input**, so one typed address is on the order of a dozen
/// `by_handle` calls, and anonymous connections key on `peer_addr` — a shared
/// NAT / CGNAT / corporate egress / VPN exit is **one bucket for everybody
/// behind it**. Tripping this window is therefore ordinary traffic, not only an
/// attack.
///
/// It is left as-is because raising it is the wrong lever twice over: it would
/// weaken the harvesting bound it exists for, and the damage a tripped window
/// used to do was never the refusal itself — it was that the *client* misread
/// the refusal as "not a Fauna recipient here" and downgraded a known Fauna
/// peer to plaintext SMTP. That is fixed on the client side, where it belongs
/// (`federation.md` § Peer-auth model → *Discovery-failure semantics*): a
/// throttled probe is now a **non-answer**, so a known peer resolves as a
/// terminal error and nothing is ever sent in the clear. The remaining cost of
/// a tripped window is a retryable "lookup failed", which is honest. Debouncing
/// the per-keystroke resolve in the apps is the real fix for the *rate*, and is
/// tracked separately.
pub const fn default_config() -> LimiterConfig {
    LimiterConfig {
        window: std::time::Duration::from_secs(60),
        max_events: DISCOVERY_MAX_EVENTS,
    }
}

/// The shipped per-source discovery budget: 60 events / 60 s — the number
/// [`default_config`]'s comment above sizes, re-sights and deliberately keeps.
#[cfg(not(feature = "test-hooks"))]
const DISCOVERY_MAX_EVENTS: u32 = 60;

/// The harness budget — high enough that no e2e run can reach it.
///
/// **Why the shipped 60 cannot stand in a harness build.** It is the same
/// argument [`REGISTER_MAX_EVENTS`] makes, over the same shared source: every
/// e2e client is `127.0.0.1` against one **session-scoped** nest, which
/// `e2e-conventions.md` § point 10 rules deliberate ("the answer is never to
/// isolate it"). Both of this gate's bucket classes collapse under that:
/// anonymous traffic keys on the peer IP, so **one bucket serves the whole
/// run**, and the suite's session-scoped actors put many tests behind one
/// `actor_id` bucket too. A per-source budget over a single shared source is a
/// budget **per run**, not per test.
///
/// And the discovery surface is the chattiest thing on that budget: the peer
/// leg refreshes its brake evidence with a `fauna.nest.info` on **every pump
/// pass** while bound (`fauna_sync_engine::peer_leg::ensure_bound`), on top of
/// each app launch's own bootstrap reads and the recipient picker's
/// resolve-on-input. Sixty of those inside one sliding minute is ordinary
/// harness traffic — which is exactly what the comment above already says about
/// ordinary *production* traffic.
///
/// The failure is worse than a red: it lands inside whatever test happens to be
/// running and reads as that test's own product bug. Measured 2026-08-30 — three
/// `--app web` failures in one batch, all `'post-submit-button' still disabled
/// after 90s … feedReady=False`, every one carrying
/// `kind=fauna.nest.info code=fauna.protocol.rate_limited` in the browser
/// console; earlier batches hit the same shape and were written off as fleet
/// load, because a solo retry moves the same nest's count back under 60 and
/// looks identical to "load calmed down".
///
/// **Why a compile-time split rather than raising the budget.** Raising it is
/// the wrong lever for the reasons the comment above gives — it would weaken the
/// harvesting bound the limiter exists for. This does not touch the shipped
/// number: convention 15 puts the automation surface *outside* release
/// artifacts, so the release flavour is not merely defaulted to 60, it is unable
/// to be anything else. A `nest config` value would be configuration-file
/// theatre — no user or admin would ever choose this, which by the
/// one-configuration-surface invariant makes it a constant, not a knob. A
/// runtime loopback exemption is likewise ruled out: `transport.md` § Abuse
/// posture holds that loopback is *bounded, not exempt*.
///
/// **Scope, stated honestly.** This reaches tier_3, which builds
/// `--features test-hooks` (`conftest.py`'s `build_node`). **tier_4 does not** —
/// it runs the real Docker image, which carries no test hooks by design, so a
/// tier_4 run still meets the shipped 60. That is the correct boundary, not a
/// gap to close later.
///
/// The mechanism itself stays proven at the shipped shape: the integration
/// suite (`tests/anonymous_rate_limit.rs`) builds its own `Limiter::with_config`
/// at 3 events and never reads this constant, so the trip, its typed
/// `rate_limited` reply and the per-bucket keying are all still asserted here.
///
/// Sized far above any plausible run rather than tuned to one, per convention
/// 14's generous-budget discipline — a tuned ceiling would become a new
/// order-dependent red the first time a fixture set grew.
#[cfg(feature = "test-hooks")]
const DISCOVERY_MAX_EVENTS: u32 = 100_000;

/// Tight window for the one-time admin-claim attempt surface
/// (`fauna.auth.claim_admin`): 10 attempts / 60 s **per source IP**. The claim
/// code is a short deploy-time secret, so `claim_admin` is a credential-guess
/// surface (like a login) — a per-source limit defeats single-source brute-force
/// while leaving the legitimate admin (one successful claim, a few typo
/// retries at most) unaffected. A distributed (botnet) brute-force is the
/// deeper concern this per-source limiter alone can't close (see
/// `global_claim_config` below).
///
/// ⚠ **This throttle is a PRIMARY bound again, not defense-in-depth.** It was
/// demoted to defense-in-depth on 2026-05-31 when the claim code went 24-bit →
/// 128-bit; the 2026-07-24 user decision took the code to **40 bits** (8 chars,
/// `fauna_core::claim_code`) for transcription comfort, which promotes both
/// throttles back to load-bearing. At 10/60 s a single source needs ~209 000
/// years to exhaust 2^40. **Do not loosen or remove this without first restoring
/// the code length** (`federation.md` § Security).
/// Same reuse pattern as `default_config`.
pub const fn claim_config() -> LimiterConfig {
    LimiterConfig {
        window: std::time::Duration::from_secs(60),
        max_events: 10,
    }
}

/// **Global** (all-sources-combined) cap for the admin-claim surface
/// (`fauna.auth.claim_admin`): 60 attempts / 60 s across *every* source IP.
/// Defense in depth behind `claim_config`: the per-source limiter alone is
/// evaded by a *distributed* (botnet) brute-force — N IPs each get the full
/// per-source budget, so the aggregate guess rate scales with the botnet size.
/// A single global bucket caps the *total* attempt rate regardless of source
/// count.
///
/// ⚠ **Sizing (re-derived 2026-07-24).** Originally sized against the
/// then-24-bit code: 16.7M ÷ 60/min ≈ 190 days to exhaust — too close. The code
/// went 128-bit on 2026-05-31 (making this cap defense-in-depth), then **40 bits
/// on 2026-07-24** by user decision, which makes it load-bearing again. At
/// 60/60 s, 2^40 ≈ 1.1e12 takes **~35 000 years to exhaust** (~17 000 expected),
/// so the keyspace stays unreachable — but *because of this cap*, not despite
/// it. **Do not raise `max_events` or widen `window` without first restoring the
/// code length.**
///
/// **Availability tradeoff (accepted).** A global cap lets an attacker spam
/// `claim_admin` to keep the global bucket saturated and *delay* — never take
/// over — the legitimate admin's claim. This is bounded and recoverable: the
/// admin claims once, typically within minutes of deploy and before the nest
/// is even publicly discoverable; the sliding window refills every 60 s so a
/// retry succeeds in the gaps; the surface vanishes entirely once the nest is
/// claimed; and 60/min is generous enough that the admin's one (or few typo)
/// attempts pass under realistic load (6 distinct sources at the per-source
/// limit before the global cap engages). With the 2026-07-24 move to a 40-bit
/// code this delay-a-claim tradeoff can no longer be bought off by raising the
/// cap, so the **accepted mitigation is operational**: firewall the box to your
/// own address while claiming, then open it up. A nest is claimed once, usually
/// within minutes of deploy and before it is publicly discoverable, and the
/// surface vanishes the moment it is claimed. This is written up for users in
/// `docs/guides/nest-internet-setup.md` § If something doesn't work.
pub const fn global_claim_config() -> LimiterConfig {
    LimiterConfig {
        window: std::time::Duration::from_secs(60),
        max_events: 60,
    }
}

/// Per-source throttle for the in-band invite-code verify surface
/// (`fauna.account.invite_code.verify`): 10 attempts / 60 s **per source IP**.
/// Same surface *class* as the admin claim — guessing a valid secret over an
/// anonymous, signature-less kind — but lower severity (a valid guess yields an
/// unauthorized *account*, not admin) and the code is far stronger (10 chars of
/// a 31-symbol unambiguous alphabet ≈ 49 bits, vs the claim code's 40), so a
/// per-source bound is sufficient and no global cap is warranted. Restores the
/// per-IP limit the retired HTTP `/api/v1/invite/verify` twin carried, now that
/// the anonymous WS path exposes the real client IP (`peer_addr`). Same reuse
/// pattern as `claim_config`.
pub const fn invite_verify_config() -> LimiterConfig {
    LimiterConfig {
        window: std::time::Duration::from_secs(60),
        max_events: 10,
    }
}

/// Per-source throttle for the self-service account-registration surface
/// (`fauna.account.register`): 10 attempts / 60 s **per source IP**. Register is
/// an anonymous, signature-bound *write* that creates an account (handle
/// reservation + an Ed25519 verify), so on a `public`+open nest an unthrottled
/// source can flood registrations, exhaust the handle space, or spawn spam
/// actors (an internal security review finding;
/// `max_free_users` bounds the account *count* if set, but not the request rate
/// or the per-attempt verify cost). A legitimate actor registers **once** (plus
/// a handful of typo/handle-collision retries), so 10/min/source is generous for
/// real onboarding yet bounds single-source registration floods. This **restores**
/// the per-IP limit the retired HTTP `POST /api/v1/register` twin carried —
/// dropped on the WS migration as the (now-closed) "no peer IP yet" deferral
/// (`transport.md` § Pre-identity) and never re-wired (the `registration_limiter`
/// governor field was dead code) — now that the SNI router conveys the real
/// client IP to the anonymous WS path. Same reuse pattern as `claim_config`. The
/// **per-IPv4 / per-/64-IPv6** keying (`ip_key_bytes`) means a shared-NAT site
/// shares one bucket; 10/min tolerates the realistic sequential-signup burst.
///
/// **The budget — and only the budget — is raised in a `test-hooks` build**
/// ([`REGISTER_MAX_EVENTS`]); the window, the keying and the gate itself are
/// identical in both flavours.
pub const fn register_config() -> LimiterConfig {
    LimiterConfig {
        window: std::time::Duration::from_secs(60),
        max_events: REGISTER_MAX_EVENTS,
    }
}

/// The shipped per-source registration budget: 10 attempts / 60 s (see
/// [`register_config`] for the threat model this number answers).
#[cfg(not(feature = "test-hooks"))]
const REGISTER_MAX_EVENTS: u32 = 10;

/// The harness budget — high enough that no e2e run can reach it.
///
/// **Why the shipped 10 cannot stand in a harness build.** Every e2e actor
/// registers from `127.0.0.1` against one **session-scoped** nest, and that
/// sharing is deliberate — `e2e-conventions.md` § point 10 rules the nest the
/// designed inverse of app isolation ("the answer is never to isolate it"), and
/// one nest serving N apps is what makes cross-app journeys possible. But a
/// per-source budget over a single shared source is effectively a budget **per
/// run**, not per test: any minute in which fixtures mint more than ten actors
/// reds *every* registering fixture until the window slides. The failure lands
/// inside whatever test happens to be running and reads as that test's own
/// product bug — three reds in one 2026-08-16 scoped run were this single cause,
/// two of them folder-sharing tests that looked like "sharing is
/// broken".
///
/// **Why a compile-time split rather than a runtime knob.** Convention 15 (owner
/// [`e2e-automation-surface-gating.md`]) puts the automation surface *outside*
/// release artifacts rather than behind a runtime switch, so the release
/// flavour must not merely default to 10 — it must be unable to be anything
/// else. A `nest config` value would also be configuration-file theatre: no user
/// or admin would ever choose this, which by the one-configuration-surface
/// invariant makes it a constant, not a knob.
///
/// **Scope, stated honestly.** This reaches tier_3, which builds
/// `--features test-hooks` (`conftest.py`'s `build_node`). **tier_4 does not** —
/// it runs the real Docker image, which carries no test hooks by design, so a
/// tier_4 run still meets the shipped 10/min. That is the correct boundary, not
/// a gap to close later: putting this in a release artifact is precisely what
/// convention 15 forbids.
///
/// Sized far above any plausible run rather than tuned to one, per convention
/// 14's generous-budget discipline — a tuned ceiling would become a new
/// order-dependent red the first time a fixture set grew.
#[cfg(feature = "test-hooks")]
const REGISTER_MAX_EVENTS: u32 = 100_000;

/// Per-source throttle for the in-band invite-**request** submit surface
/// (`fauna.account.invite_request.submit`): 10 attempts / 60 s **per source IP**.
/// Submit is an anonymous, signature-bound *write* that inserts a `pending`
/// invite-request row (plus an Ed25519 verify); its only dedup is by `actor_id`,
/// trivially defeated by rotating keypairs, so an unthrottled source can flood
/// SQLite with pending rows — disk growth on a box already wedged once by disk
/// exhaustion (memory `mail-bridge-refresh-loop-disk-fill`) plus admin-UI clutter
/// (an internal security review finding). This
/// per-source rate bound pairs with the global `MAX_PENDING_INVITE_REQUESTS`
/// row-count cap in `invite_core::submit_invite_request_core` (the per-source
/// gate slows a single flooder; the global cap is the hard disk backstop a
/// *distributed* flood cannot evade). A legitimate user submits **one** request,
/// so 10/min/source is ample. Resolves the `invite_handlers` "submit throttle is
/// a separate hardening item if queue-spam is observed" deferral. Same reuse
/// pattern as `claim_config`.
///
/// **The budget — and only the budget — is raised in a `test-hooks` build**
/// ([`INVITE_REQUEST_MAX_EVENTS`]); the window, the keying and the gate itself
/// are identical in both flavours, exactly as for [`register_config`].
pub const fn invite_request_config() -> LimiterConfig {
    LimiterConfig {
        window: std::time::Duration::from_secs(60),
        max_events: INVITE_REQUEST_MAX_EVENTS,
    }
}

/// The shipped per-source invite-request budget: 10 submits / 60 s (see
/// [`invite_request_config`] for the threat model this number answers).
#[cfg(not(feature = "test-hooks"))]
const INVITE_REQUEST_MAX_EVENTS: u32 = 10;

/// The harness budget — the [`REGISTER_MAX_EVENTS`] argument, applied to the
/// surface that demonstrably needed it too.
///
/// **This one was already firing, and the evidence was hiding in a workaround.**
/// A prior pass recorded this surface as an *unmeasured* adjacency and
/// deliberately left it alone ("raising a budget that is not demonstrably a
/// problem can mask a real throttle bug"). It was measured — just not where the
/// row looked: `tests/e2e-unified/tests/test_family.py`'s own submit helper
/// carried a retry loop whose docstring states the throttle "surfaced as a bogus
/// red on tui while slower clients slipped under it". So the failure existed, was
/// diagnosed correctly, and was absorbed by a `time.sleep(5.0)` retry with a 90 s
/// deadline — precisely the wall-clock-dependent shape the project's testing
/// conventions rule DEFUNCT, and that row 303 names as the one fix to avoid. The
/// retry is deleted with this commit; the budget is what carries the load now.
///
/// **The arithmetic, since a green run on one dev machine would have proved
/// nothing.** Three app-parametrized tier_3 tests submit against the one
/// session-scoped nest — `test_admin_users_hub.py` (×2) and the alphabetically
/// adjacent `test_admin_users_action_error.py` (×1) — so a sweep costs 3 submits
/// per app. On a **three-app** machine that is 9 against a budget of 10: one
/// under, which is why the row saw "zero headroom" and why measuring there would
/// have returned a green that meant only "not this time". On a **four-app**
/// machine the same tests cost **12** — over budget today, before counting
/// `test_family.py` or the UI-driven submits in the onboarding modules. The
/// throttle's own proofs are unaffected: `register_throttles_per_source` and the
/// invite-request behavioural tests drive explicit `small_limiter()` configs, and
/// `tests/api/test_invite_requests.py` gets a fresh function-scoped nest per test.
#[cfg(feature = "test-hooks")]
const INVITE_REQUEST_MAX_EVENTS: u32 = 100_000;

/// Pack a source IP into the limiter's 32-byte key slot.
///
/// IPv4 is keyed at full address resolution. IPv6 is masked to its **/64
/// routing prefix** (the smallest block typically assigned to a single
/// subscriber) so an attacker holding a /64 — trivially many addresses — cannot
/// evade the per-source bound by rotating the host portion. IPv4-mapped IPv6
/// addresses are canonicalized to their IPv4 form so the two spellings of the
/// same client share a bucket.
pub(crate) fn ip_key_bytes(ip: IpAddr) -> [u8; 32] {
    let mut out = [0u8; 32];
    match ip {
        IpAddr::V4(v4) => out[..4].copy_from_slice(&v4.octets()),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => out[..4].copy_from_slice(&v4.octets()),
            None => out[..8].copy_from_slice(&v6.octets()[..8]),
        },
    }
    out
}

/// Returns `true` iff the call from `peer` for `kind` is permitted; records the
/// event on success.
///
/// ⚠ **Callers in the dispatcher want [`check_conn`], not this.** This is the
/// IP-keyed half, and it fails open on a `None` peer — which is every
/// authenticated connection, since the authenticated upgrade captures no
/// `ConnectInfo`. Reaching for it directly on a connection of unknown class is
/// exactly the shape that left 18 pre-identity kinds unbounded for authenticated
/// callers until 2026-08-24. A `None` peer reaches this only from an anonymous
/// connection without `ConnectInfo` (unit tests; both production listeners
/// always carry one), and there it still fails open.
pub fn check(limiter: &Limiter, peer: Option<SocketAddr>, kind: &str) -> bool {
    let Some(addr) = peer else {
        return true;
    };
    limiter.check(&ip_key_bytes(addr.ip()), &UNUSED_SLOT, kind)
}

/// Returns `true` iff the call for `kind` is permitted under the **global**
/// (source-independent) cap; records the event on success. Unlike [`check`],
/// the bucket key carries **no source IP** — both key slots are the unused
/// zero slot and `kind` is the credential — so every source shares one bucket
/// per kind. This is the distributed-brute-force bound (`global_claim_config`):
/// it must be a *separate* `Limiter` instance from the per-source one, never the
/// same limiter, or the zero-keyed global bucket would collide with a `0.0.0.0`
/// source bucket (never a real peer, but the separation keeps the two budgets
/// independent by construction). There is no `Option` peer here — the cap
/// applies whether or not a `ConnectInfo` is present.
pub fn check_global(limiter: &Limiter, kind: &str) -> bool {
    limiter.check(&UNUSED_SLOT, &UNUSED_SLOT, kind)
}

/// The dispatcher's door: returns `true` iff `conn`'s call for `kind` is
/// permitted under `limiter`, **whatever class `conn` is**; records the event on
/// success.
///
/// This is the class-independent replacement for `conn.anonymous && check(…)`.
/// The class picks the *bucket*, never whether there is one:
///
/// * anonymous → the source IP (unchanged, byte-identical bucket key);
/// * authenticated → `conn.actor_id`, in the disjoint [`ACTOR_SLOT`] namespace.
///
/// **Both classes share the limiter's configured budget, deliberately.** Each
/// gate's budget is justified by what the *kind* costs — a claim-code guess, an
/// unmetered Ed25519 verify, a nonce mint, a directory read — and none of that
/// gets cheaper because the caller authenticated; `federation.md` § Security's
/// 40-bit claim-code arithmetic in particular is stated against these exact
/// numbers, so handing the authenticated class a larger budget would loosen the
/// very bound that licenses the short code. The budgets are generous per
/// `(source, kind)` and the authenticated caller gets a bucket of its own, so a
/// first-party client is unaffected: these kinds are resolved one per user
/// action (a follow, an address resolve — `fauna-client-folders::follow_ops`,
/// `fauna-conversations::backend`), never in bulk loops.
pub fn check_conn(limiter: &Limiter, conn: &crate::ws::RpcConnection, kind: &str) -> bool {
    if conn.anonymous {
        check(limiter, conn.peer_addr, kind)
    } else {
        limiter.check(&conn.actor_id, &ACTOR_SLOT, kind)
    }
}

/// The identifier one throttle trip is attributed to in its shed line — the
/// SAME discriminant [`check_conn`] keys the ceiling on (peer for an
/// anonymous connection, actor id for an authenticated one), resolved once at
/// the call site so [`report_shed`] needs no `RpcConnection` and stays
/// unit-testable on plain values. Before this,
/// `report_shed` printed `peer` unconditionally, so an authenticated trip —
/// keyed on `actor_id` — logged a socket address that had nothing to do with
/// the decision.
pub enum ShedKey {
    Peer(Option<SocketAddr>),
    Actor([u8; 32]),
}

impl ShedKey {
    /// The identifier [`check_conn`] would key `conn` on.
    pub fn of(conn: &crate::ws::RpcConnection) -> Self {
        if conn.anonymous {
            ShedKey::Peer(conn.peer_addr)
        } else {
            ShedKey::Actor(conn.actor_id)
        }
    }
}

/// Report one throttle trip on `gate`, at most one `warn!` per
/// [`fauna_conn_limit::SHED_LOG_INTERVAL`] carrying the batch count — reuses
/// `fauna_conn_limit::ShedCounter`, the same reporter `rate_limit.rs`'s per-IP
/// governor and every `fauna_conn_limit` connection cap report through, rather
/// than a second implementation
/// (`docs/goal/architecture/transport-connection.md` § Abuse posture:
/// *"a shedding [gate] that logs nothing is indistinguishable
/// from a quiet night"*). Single-sourced so every one of the five anonymous-
/// throttle call sites in `routes.rs` reports in one shape; a caller passes
/// its own gate's counter (`AppState`'s `*_rate_limit_shed` fields — one per
/// gate, so a burst on one gate never suppresses another's line) and the gate
/// name that appears in the log line and (eventually) in an alert rule.
///
/// Before this, four of the five gates logged only at `debug!` — invisible in
/// a shipped nest, whose default filter is `info` — and the fifth
/// (`claim_admin`) logged an unbatched `warn!` per refusal, which a
/// brute-force attempt turns into a flood. Both classes collapse to the same
/// shape here.
///
/// **`(sample: …)`, and the correct key.** `shed` is one counter per gate,
/// shared across every peer/actor that gate meters, so the printed
/// identifier is one example of the batch, never its cause
/// (`transport-connection.md` § Abuse posture, "The printed identifier on a
/// shed line is a sample, never an attribution"). And it is now the
/// identifier the ceiling actually used —
/// [`ShedKey::of`] takes `check_conn`'s own discriminant — rather than always
/// the peer socket, which an authenticated trip (keyed on `actor_id`) had
/// nothing to do with.
pub fn report_shed(shed: &fauna_conn_limit::ShedCounter, gate: &str, key: ShedKey) {
    let Some(batch) = shed.record() else {
        return;
    };
    match key {
        ShedKey::Peer(peer) => tracing::warn!(
            gate = %gate,
            peer = ?peer,
            "anonymous-throttle gate tripped; shedding (sample: peer) \
             ({batch} shed since last line)"
        ),
        ShedKey::Actor(id) => tracing::warn!(
            gate = %gate,
            actor = %crate::bridge_method_allowlist::actor_prefix_hex(&id),
            "anonymous-throttle gate tripped; shedding (sample: actor) \
             ({batch} shed since last line)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

    fn v4(a: u8, b: u8, c: u8, d: u8, port: u16) -> Option<SocketAddr> {
        Some(SocketAddr::from((Ipv4Addr::new(a, b, c, d), port)))
    }

    fn small_limiter() -> Limiter {
        Limiter::with_config(LimiterConfig {
            window: std::time::Duration::from_secs(60),
            max_events: 2,
        })
    }

    #[test]
    fn trips_after_max_events_for_one_source_and_kind() {
        let l = small_limiter();
        assert!(check(&l, v4(203, 0, 113, 7, 5000), "fauna.actor.by_handle"));
        assert!(check(&l, v4(203, 0, 113, 7, 5001), "fauna.actor.by_handle"));
        // Same source IP (different ephemeral port), same kind → over the limit.
        assert!(!check(
            &l,
            v4(203, 0, 113, 7, 5002),
            "fauna.actor.by_handle"
        ));
    }

    #[test]
    fn distinct_sources_have_distinct_buckets() {
        let l = small_limiter();
        assert!(check(&l, v4(203, 0, 113, 7, 5000), "fauna.actor.by_handle"));
        assert!(check(&l, v4(203, 0, 113, 7, 5000), "fauna.actor.by_handle"));
        assert!(!check(
            &l,
            v4(203, 0, 113, 7, 5000),
            "fauna.actor.by_handle"
        ));
        // A different source IP is unaffected by the first source's exhaustion.
        assert!(check(
            &l,
            v4(198, 51, 100, 9, 5000),
            "fauna.actor.by_handle"
        ));
    }

    #[test]
    fn distinct_kinds_have_distinct_buckets() {
        let l = small_limiter();
        assert!(check(&l, v4(203, 0, 113, 7, 5000), "fauna.actor.by_handle"));
        assert!(check(&l, v4(203, 0, 113, 7, 5000), "fauna.actor.by_handle"));
        assert!(!check(
            &l,
            v4(203, 0, 113, 7, 5000),
            "fauna.actor.by_handle"
        ));
        // nest.resolve is a separate kind-class bucket for the same source.
        assert!(check(&l, v4(203, 0, 113, 7, 5000), "fauna.nest.resolve"));
    }

    #[test]
    fn ipv6_is_bucketed_by_64_prefix() {
        let l = small_limiter();
        // Two addresses in the same /64 (differ only in the host portion) share
        // a bucket → the second source cannot evade the first's exhaustion.
        let a = Some(SocketAddr::from((
            Ipv6Addr::new(0x2001, 0xdb8, 0xab, 0xcd, 0, 0, 0, 1),
            5000,
        )));
        let b = Some(SocketAddr::from((
            Ipv6Addr::new(0x2001, 0xdb8, 0xab, 0xcd, 0xffff, 0xffff, 0xffff, 0xfffe),
            5000,
        )));
        assert!(check(&l, a, "fauna.nest.info"));
        assert!(check(&l, b, "fauna.nest.info"));
        assert!(!check(&l, a, "fauna.nest.info"));
        // A different /64 is a different bucket.
        let other = Some(SocketAddr::from((
            Ipv6Addr::new(0x2001, 0xdb8, 0xab, 0xce, 0, 0, 0, 1),
            5000,
        )));
        assert!(check(&l, other, "fauna.nest.info"));
    }

    #[test]
    fn none_peer_fails_open() {
        let l = small_limiter();
        // No ConnectInfo (unit-test path) → never throttled.
        for _ in 0..10 {
            assert!(check(&l, None, "fauna.nest.info"));
        }
    }

    #[test]
    fn global_cap_ignores_source_and_trips_after_max_events() {
        let l = small_limiter(); // max_events = 2
        // Two attempts from the global bucket pass ...
        assert!(check_global(&l, "fauna.auth.claim_admin"));
        assert!(check_global(&l, "fauna.auth.claim_admin"));
        // ... the third trips regardless of which source it came from (the
        // bucket is source-independent — this is the distributed-brute-force
        // bound, the property `check` deliberately does NOT have).
        assert!(!check_global(&l, "fauna.auth.claim_admin"));
    }

    #[test]
    fn global_cap_is_independent_of_per_source_buckets() {
        // The global limiter is a distinct instance from the per-source one;
        // exhausting the global bucket for a kind does not consume a different
        // kind's global budget on the same instance.
        let l = small_limiter(); // max_events = 2
        assert!(check_global(&l, "fauna.auth.claim_admin"));
        assert!(check_global(&l, "fauna.auth.claim_admin"));
        assert!(!check_global(&l, "fauna.auth.claim_admin"));
        // A different kind has its own global bucket.
        assert!(check_global(&l, "fauna.account.invite_code.verify"));
    }

    #[test]
    fn invite_verify_throttles_per_source() {
        // The invite-verify limiter behaves exactly like the per-source claim
        // limiter — `check` keys on (IP, kind), so distinct sources are
        // independent and the same source trips after its budget.
        let l = small_limiter(); // max_events = 2
        assert!(check(
            &l,
            v4(203, 0, 113, 7, 5000),
            "fauna.account.invite_code.verify"
        ));
        assert!(check(
            &l,
            v4(203, 0, 113, 7, 5001),
            "fauna.account.invite_code.verify"
        ));
        assert!(!check(
            &l,
            v4(203, 0, 113, 7, 5002),
            "fauna.account.invite_code.verify"
        ));
        // A different source IP is unaffected.
        assert!(check(
            &l,
            v4(198, 51, 100, 9, 5000),
            "fauna.account.invite_code.verify"
        ));
    }

    #[test]
    fn config_thresholds_match_their_threat_models() {
        // The global cap must be >= the per-source cap so a single legitimate
        // source is never blocked by the global bucket before its own.
        assert!(global_claim_config().max_events >= claim_config().max_events);
        // Invite verify is per-source only (no global), tight like the claim.
        assert_eq!(invite_verify_config().max_events, claim_config().max_events);
        // Register and invite-request submit are per-source writes: generous
        // enough for a one-shot legitimate onboarding (with retries) yet bounded.
        assert!(register_config().max_events > 0);
        assert!(invite_request_config().max_events > 0);
        // Both use a 60 s window, matching the rest of the anonymous surface.
        assert_eq!(register_config().window, claim_config().window);
        assert_eq!(invite_request_config().window, claim_config().window);
    }

    /// The shipped budget is the threat model's number and nothing else.
    ///
    /// Pinned separately from the threat-model test above because
    /// [`register_config`] now varies by build flavour: only the harness build
    /// raises it, and the whole point of raising it is that the *shipped* value
    /// does not move. Compiled only into a release-flavour build, so this is the
    /// arm that would catch a harness-only widening leaking into production.
    #[cfg(not(feature = "test-hooks"))]
    #[test]
    fn shipped_register_budget_is_ten_per_minute() {
        assert_eq!(
            register_config().max_events,
            10,
            "the shipped per-IP registration budget is a security-review number \
             (§ D10) — raising it here would weaken every public nest; the \
             harness gets its own arm under `test-hooks`"
        );
    }

    /// A harness build clears a whole e2e run, not ten actors.
    ///
    /// Every e2e actor registers from `127.0.0.1` against one session-scoped
    /// nest, so the per-IP budget is effectively **per run**: at the shipped 10
    /// a run reds every registering fixture the moment its fixtures mint an
    /// eleventh actor in a minute, and the failure surfaces inside whatever test
    /// happens to be running. The ceiling here is sized
    /// far above any plausible run rather than tuned to one, per convention 14's
    /// "generous budget" discipline.
    #[cfg(feature = "test-hooks")]
    #[test]
    fn harness_register_budget_clears_a_long_e2e_run() {
        assert!(
            register_config().max_events >= 10_000,
            "a harness build must not throttle fixture actor-minting; got {}",
            register_config().max_events
        );
        // Only the budget moves. The window stays the production one so the
        // sliding-window behaviour under test is the shipped behaviour.
        assert_eq!(
            register_config().window,
            std::time::Duration::from_secs(60),
            "the harness raises the budget, never the window"
        );
    }

    /// The shipped discovery budget is the threat model's number and nothing
    /// else — same contract as [`shipped_register_budget_is_ten_per_minute`],
    /// over the surface where raising it is most explicitly the wrong lever.
    ///
    /// [`default_config`]'s own comment re-sighted this 60 on 2026-08-29 and
    /// deliberately kept it: it is the primary DoS / enumeration bound on kinds
    /// that need no signature at all, and widening it would weaken the
    /// directory-harvesting bound it exists for. This arm is what keeps the
    /// harness split below from ever becoming that widening.
    #[cfg(not(feature = "test-hooks"))]
    #[test]
    fn shipped_discovery_budget_is_sixty_per_minute() {
        assert_eq!(
            default_config().max_events,
            60,
            "the shipped per-source discovery budget is the harvesting bound \
             (`federation.md` § Security) — raising it here would weaken every \
             public nest; the harness gets its own arm under `test-hooks`"
        );
    }

    /// A harness build clears a whole e2e run's discovery traffic.
    ///
    /// Both of this gate's bucket classes collapse to one bucket per run in the
    /// harness — anonymous traffic keys on the peer IP and every e2e client is
    /// `127.0.0.1`, while the suite's session-scoped actors put many tests
    /// behind one `actor_id`. At the shipped 60 an ordinary batch trips the
    /// window and the refusal lands inside whatever test is running, reading as
    /// that test's own product bug.
    #[cfg(feature = "test-hooks")]
    #[test]
    fn harness_discovery_budget_clears_a_long_e2e_run() {
        assert!(
            default_config().max_events >= 10_000,
            "a harness build must not throttle the discovery surface a whole run \
             shares; got {}",
            default_config().max_events
        );
        // Only the budget moves. The window stays the production one so the
        // sliding-window behaviour under test is the shipped behaviour.
        assert_eq!(
            default_config().window,
            std::time::Duration::from_secs(60),
            "the harness raises the budget, never the window"
        );
    }

    /// The shipped invite-request budget is the threat model's number, same
    /// contract as [`shipped_register_budget_is_ten_per_minute`].
    ///
    /// The per-source rate bound is half of a pair — it slows a single flooder
    /// while `MAX_PENDING_INVITE_REQUESTS` backstops a distributed one — so
    /// widening it in a release build would quietly leave the disk-growth
    /// finding to the global cap alone.
    #[cfg(not(feature = "test-hooks"))]
    #[test]
    fn shipped_invite_request_budget_is_ten_per_minute() {
        assert_eq!(
            invite_request_config().max_events,
            10,
            "the shipped per-IP invite-request budget answers a security-review \
             finding (anonymous signature-bound write, deduped only by actor_id); \
             the harness gets its own arm under `test-hooks`"
        );
    }

    /// A harness build clears a whole e2e run of invite-request submits.
    ///
    /// Three app-parametrized tier_3 tests submit against the ONE session-scoped
    /// nest (`test_admin_users_hub.py` ×2, `test_admin_users_action_error.py`
    /// ×1), so a sweep costs 3 per app: 9 on a three-app dev machine, **12 on a
    /// four-app one** — over the shipped budget before counting `test_family.py`
    /// or the onboarding modules' UI-driven submits. Sized far above any
    /// plausible run rather than tuned, per convention 14.
    #[cfg(feature = "test-hooks")]
    #[test]
    fn harness_invite_request_budget_clears_a_long_e2e_run() {
        assert!(
            invite_request_config().max_events >= 10_000,
            "a harness build must not throttle fixture invite-request seeding; got {}",
            invite_request_config().max_events
        );
        // Only the budget moves — the window stays the production one.
        assert_eq!(
            invite_request_config().window,
            std::time::Duration::from_secs(60),
            "the harness raises the budget, never the window"
        );
    }

    #[test]
    fn register_throttles_per_source() {
        // `register` keys on (IP, kind) exactly like the other write surfaces:
        // distinct sources are independent, the same source trips after budget.
        let l = small_limiter(); // max_events = 2
        assert!(check(
            &l,
            v4(203, 0, 113, 7, 5000),
            "fauna.account.register"
        ));
        assert!(check(
            &l,
            v4(203, 0, 113, 7, 5001),
            "fauna.account.register"
        ));
        assert!(!check(
            &l,
            v4(203, 0, 113, 7, 5002),
            "fauna.account.register"
        ));
        // A different source IP is unaffected.
        assert!(check(
            &l,
            v4(198, 51, 100, 9, 5000),
            "fauna.account.register"
        ));
    }

    #[test]
    fn invite_request_submit_throttles_per_source() {
        let l = small_limiter(); // max_events = 2
        assert!(check(
            &l,
            v4(203, 0, 113, 7, 5000),
            "fauna.account.invite_request.submit"
        ));
        assert!(check(
            &l,
            v4(203, 0, 113, 7, 5001),
            "fauna.account.invite_request.submit"
        ));
        assert!(!check(
            &l,
            v4(203, 0, 113, 7, 5002),
            "fauna.account.invite_request.submit"
        ));
        // A different source IP is unaffected.
        assert!(check(
            &l,
            v4(198, 51, 100, 9, 5000),
            "fauna.account.invite_request.submit"
        ));
    }

    /// A throttle trip must reach the log. Before this landed four of the five
    /// anonymous-throttle gates logged only at `debug!` — invisible in a
    /// shipped nest (`main.rs` defaults the filter to `info`) — so a nest
    /// throttling a client and a nest with no traffic at all produced
    /// identical logs.
    ///
    /// Asserted through the counter `report_shed` reports *through*, the same
    /// technique `rate_limit.rs`'s `a_shed_is_reported_through_the_rate_limited_counter`
    /// uses: the counter's contract is that the first shed after a quiet
    /// period always reports and the rest are suppressed until the interval
    /// elapses, so if `report_shed` recorded, the test's own follow-up
    /// `shed.record()` is suppressed; if it recorded nothing, that call would
    /// itself be the first shed and would report. The 1-hour interval never
    /// elapses here, so nothing depends on wall-clock timing (convention 14).
    #[test]
    fn a_trip_is_reported_through_the_rate_limited_counter() {
        let shed =
            fauna_conn_limit::ShedCounter::with_interval(std::time::Duration::from_secs(3600));
        report_shed(
            &shed,
            "fauna.account.register",
            ShedKey::Peer(v4(203, 0, 113, 7, 5000)),
        );
        assert!(
            shed.record().is_none(),
            "the trip did not report a shed — this counter's first-shed-\
             reports-immediately slot was still unspent, so report_shed \
             never called it"
        );
    }

    /// A burst inside one window collapses to a single line, not one per
    /// trip — the property that makes it safe to call `report_shed` from a
    /// hot refusal path at all. Discriminating the same way the test above
    /// is: no call here touches the counter directly, so if even one of the
    /// ten `report_shed` calls below failed to record, the trailing
    /// `shed.record()` — still the counter's very first direct touch in that
    /// case — would report `Some`, not `None`.
    #[test]
    fn a_burst_of_trips_reports_once_not_once_per_trip() {
        let shed =
            fauna_conn_limit::ShedCounter::with_interval(std::time::Duration::from_secs(3600));
        for _ in 0..10 {
            report_shed(
                &shed,
                "fauna.account.invite_request.submit",
                ShedKey::Peer(None),
            );
        }
        assert!(
            shed.record().is_none(),
            "the counter's first-shed-reports-immediately slot was still \
             unspent after ten calls to report_shed — either report_shed \
             never recorded a shed, or a burst inside the interval reported \
             more than the one allowed line"
        );
    }

    /// `ShedKey::of` must take the SAME discriminant `check_conn` keys the
    /// ceiling on — peer for an anonymous connection, actor id for an
    /// authenticated one — not always the peer socket. Before this fix an
    /// authenticated trip's shed line named a socket the ceiling never
    /// looked at.
    #[test]
    fn shed_key_of_matches_check_conns_own_discriminant() {
        let state = crate::ws::WsState::new();

        let (anon_conn, _rx) = state.subscribe_anonymous(v4(203, 0, 113, 7, 5000));
        assert!(
            matches!(ShedKey::of(&anon_conn), ShedKey::Peer(p) if p == v4(203, 0, 113, 7, 5000)),
            "an anonymous connection must key its shed sample on the peer \
             address, same as check_conn"
        );

        let actor_id = [0x7Au8; 32];
        let (auth_conn, _rx) = state.subscribe(actor_id);
        assert!(
            matches!(ShedKey::of(&auth_conn), ShedKey::Actor(id) if id == actor_id),
            "an authenticated connection must key its shed sample on \
             actor_id, same as check_conn — NOT on peer_addr, which is None \
             for every authenticated connection (ws.rs subscribe())"
        );
    }
}
