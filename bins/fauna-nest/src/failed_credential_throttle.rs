//! The failed-credential throttle — the nest's backstop against a client that
//! keeps presenting a credential the nest has already refused.
//!
//! Authority: `docs/goal/architecture/transport-connection.md` § Abuse posture →
//! *The failed-credential throttle*. The incident that motivated it: a sync
//! agent holding a dead device credential dialled a production nest about eight
//! times a second for four days, every bearer upgrade answered `401` and every
//! renewal mint `fauna.auth.not_registered`. The per-IP request-rate governor
//! (`rate_limit.rs`, 100 req/s) never saw it, and nothing told the client to back
//! off. The client-side dial budget (`fauna_ws_substrate::dial_budget`) is the
//! primary fix; this is the server half, for every client bug — ours or a third
//! party's — that the client half does not reach.
//!
//! **What it counts: refusals, never attempts.** A bucket spends only on an
//! attempt that already failed. That is what makes it lockout-safe on the
//! surfaces `transport-connection.md` rules unthrottled for lockout reasons
//! (the bearer upgrade, the auth-bootstrap ceremony): a valid credential never
//! spends anything here, so no flood — from any source, claiming any identity
//! — can make a legitimate credential's attempt fail that would otherwise
//! succeed.
//!
//! **The key is the composite `(source IP × claimed identity)`**, never the bare
//! claimed identity: an actor id is public and a claimed one is unproven, so a
//! bucket keyed on it alone would let a remote flooder claiming someone's actor
//! id spend that actor's budget — and turn the legitimate holder's own honest
//! refusal (an expired bearer, answered `401` so it re-mints) into a `429` hold.
//! Keying on the source too confines each bucket to one address, the same
//! unspoofable key every anonymous throttle uses; the claimed identity then
//! splits a shared NAT so a broken neighbour's refusals never spend another
//! identity's budget. The source is the one the connection caps and the
//! governor already resolve (the PROXY-v2 source behind the SNI router,
//! canonicalised by `anonymous_rate_limit::ip_key_bytes`).
//!
//! **Per surface, two answer shapes:**
//! * the bearer **WS upgrades** (`GET /api/v1/ws/{actor_id}`, claimed identity
//!   the path actor; `GET /api/v1/principal/ws`, claimed identity the presented
//!   token's unverified `client_id`) validate first and convert only an over-budget *refusal* into
//!   `429 Too Many Requests` + `Retry-After` — the answer the native client's
//!   dial budget holds on (`transport-connection.md` § *A `429` on the upgrade
//!   holds every dial to that nest*). A valid bearer always upgrades.
//! * **`fauna.auth.device_handshake`** (claimed identity the renewal device key)
//!   refuses an over-budget bucket *before* the work, with the RPC plane's
//!   `fauna.protocol.rate_limited`. Refusing before the work is lockout-safe
//!   there for the reason stated above: only refusals fill the bucket, and a
//!   renewal key with a live grant is never refused — so its bucket fills only if
//!   someone on its own source address presents the same (never published)
//!   device key and fails, over and over. It is not converted after the work,
//!   because the refusal it would replace (`not_registered`) is terminal for a
//!   correct client and `rate_limited` is retryable.
//!
//! Every number here is a Rust constant — no user or admin would choose them
//! (`principles.md` § One configuration surface).

use std::net::IpAddr;
use std::time::Duration;

use axum::http::{HeaderValue, StatusCode, header::RETRY_AFTER};
use axum::response::{IntoResponse, Response};

use crate::bridge_rate_limit::{Limiter, LimiterConfig};

/// The sliding window the refusals are counted over.
pub const FAILED_CREDENTIAL_WINDOW: Duration = Duration::from_secs(60);

/// Refusals one `(source, claimed identity, surface)` bucket absorbs per window
/// before the throttle answers instead. A correct client needs one or two per
/// window: an expired bearer is refused once and re-minted, a dead renewal grant
/// is refused once and is terminal. Ten is far above that and still holds the
/// 2026-09-24 flood (eight a second) to its first second and a quarter.
pub const FAILED_CREDENTIAL_MAX: u32 = 10;

/// The `Retry-After` an over-budget upgrade is answered with, in seconds — the
/// window, which bounds how long a full sliding window can take to regain room.
pub const RETRY_AFTER_SECS: u64 = FAILED_CREDENTIAL_WINDOW.as_secs();

// The native client caps any hold it reads (`dial_budget::MAX_RETRY_AFTER`,
// 15 min) so a bogus value cannot park it for a day; a nest asking for longer
// than the client will honour would be asking for nothing.
const _: () = assert!(
    RETRY_AFTER_SECS <= fauna_ws_substrate::dial_budget::MAX_RETRY_AFTER.as_secs(),
    "the throttle's Retry-After must stay within the client's hold cap"
);

/// One throttled surface. Its name is the bucket's third key slot, so the
/// surfaces never share a budget, and the gate name its shed line carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// `GET /api/v1/ws/{actor_id}` — claimed identity: the path actor.
    WsRpcUpgrade,
    /// `fauna.auth.device_handshake` — claimed identity: the renewal device key.
    DeviceHandshake,
    /// `GET /api/v1/principal/ws` — claimed identity: the presented access
    /// token's **unverified** `client_id`, hashed to the key width
    /// ([`principal_claimed_identity`]). A refused token proves nothing, so the
    /// claim is exactly as unproven as the path actor of the bearer upgrade,
    /// and the source half of the key confines it the same way.
    PrincipalUpgrade,
}

/// How many surfaces there are — the shed array's length.
const SURFACES: usize = 3;

/// The claimed-identity key of a principal upgrade: `sha256(client_id)`. A
/// `client_id` is a URL of any length; the bucket key is 32 bytes.
pub fn principal_claimed_identity(client_id: &str) -> [u8; 32] {
    <sha2::Sha256 as sha2::Digest>::digest(client_id.as_bytes()).into()
}

impl Surface {
    pub fn name(self) -> &'static str {
        match self {
            Surface::WsRpcUpgrade => "ws_rpc_upgrade",
            Surface::DeviceHandshake => "device_handshake",
            Surface::PrincipalUpgrade => "principal_upgrade",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// The throttle: one sliding-window limiter over every surface, plus one shed
/// reporter per surface so a burst on one never suppresses another's line.
pub struct FailedCredentialThrottle {
    limiter: Limiter,
    shed: [fauna_conn_limit::ShedCounter; SURFACES],
}

impl Default for FailedCredentialThrottle {
    fn default() -> Self {
        Self::with_config(LimiterConfig {
            window: FAILED_CREDENTIAL_WINDOW,
            max_events: FAILED_CREDENTIAL_MAX,
        })
    }
}

impl FailedCredentialThrottle {
    pub fn with_config(config: LimiterConfig) -> Self {
        Self::with_shed(
            config,
            std::array::from_fn(|_| fauna_conn_limit::ShedCounter::new()),
        )
    }

    /// Same, with caller-supplied shed counters — a test seam, so a test can
    /// assert the reporting decision without a wall-clock wait.
    pub fn with_shed(
        config: LimiterConfig,
        shed: [fauna_conn_limit::ShedCounter; SURFACES],
    ) -> Self {
        Self {
            limiter: Limiter::with_config(config),
            shed,
        }
    }

    /// Record one refused attempt. Returns `true` iff the bucket was already
    /// full — the caller answers with the throttle's refusal instead of its own.
    /// An over-budget refusal records nothing more (the window drains on its
    /// own), and is reported through the surface's shed counter.
    pub fn note_refusal(
        &self,
        surface: Surface,
        source: Option<IpAddr>,
        claimed: &[u8; 32],
    ) -> bool {
        let over = !self
            .limiter
            .check(&source_key(source), claimed, surface.name());
        if over {
            self.report_shed(surface, source);
        }
        over
    }

    /// `true` iff the bucket is full, recording nothing — the before-the-work
    /// check of a surface that sheds early (`device_handshake`). A refusal found
    /// here is reported through the surface's shed counter.
    pub fn is_exhausted(
        &self,
        surface: Surface,
        source: Option<IpAddr>,
        claimed: &[u8; 32],
    ) -> bool {
        let full = self
            .limiter
            .is_full(&source_key(source), claimed, surface.name());
        if full {
            self.report_shed(surface, source);
        }
        full
    }

    /// Drop buckets whose every refusal has left the window.
    pub fn sweep(&self) -> usize {
        self.limiter.sweep()
    }

    /// At most one `warn!` per `fauna_conn_limit::SHED_LOG_INTERVAL` per
    /// surface, carrying the batch count. The printed source is a SAMPLE: the
    /// counter sums every bucket of the surface (`transport-connection.md`
    /// § Abuse posture, "The printed identifier on a shed line is a sample,
    /// never an attribution").
    fn report_shed(&self, surface: Surface, source: Option<IpAddr>) {
        let Some(batch) = self.shed[surface.index()].record() else {
            return;
        };
        let sample = source.map_or_else(|| "none".to_string(), |ip| ip.to_string());
        tracing::warn!(
            gate = %surface.name(),
            "failed-credential throttle reached; shedding (sample: {sample}) \
             ({batch} shed since last line)"
        );
    }
}

/// The source half of the composite key — `ip_key_bytes`' canonical form (an
/// IPv6 source keyed on its /64). A missing source keys on the zero slot rather
/// than failing open: the production listeners always carry one, and a missing
/// one must never be a way around the bucket.
fn source_key(source: Option<IpAddr>) -> [u8; 32] {
    source.map_or([0u8; 32], crate::anonymous_rate_limit::ip_key_bytes)
}

/// The over-budget upgrade answer: `429` with `Retry-After`.
pub fn too_many_requests() -> Response {
    let mut resp = (
        StatusCode::TOO_MANY_REQUESTS,
        "too many refused credentials",
    )
        .into_response();
    resp.headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from(RETRY_AFTER_SECS));
    resp
}

/// Periodic sweeper, the same shape as every other limiter's.
pub fn spawn_sweeper(
    throttle: std::sync::Arc<FailedCredentialThrottle>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    crate::sweeper::spawn_periodic_sweeper(interval, true, move || {
        let throttle = throttle.clone();
        async move {
            throttle.sweep();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const ACTOR: [u8; 32] = [7u8; 32];
    const OTHER_ACTOR: [u8; 32] = [8u8; 32];

    fn ip(d: u8) -> Option<IpAddr> {
        Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, d)))
    }

    fn shipped() -> FailedCredentialThrottle {
        FailedCredentialThrottle::default()
    }

    #[test]
    fn a_refusal_flood_is_throttled_after_the_budget() {
        let t = shipped();
        for n in 0..FAILED_CREDENTIAL_MAX {
            assert!(
                !t.note_refusal(Surface::WsRpcUpgrade, ip(1), &ACTOR),
                "refusal {n} is inside the budget and keeps its own answer"
            );
        }
        for _ in 0..50 {
            assert!(t.note_refusal(Surface::WsRpcUpgrade, ip(1), &ACTOR));
        }
    }

    #[test]
    fn a_neighbour_on_the_same_address_keeps_its_own_budget() {
        let t = shipped();
        for _ in 0..=FAILED_CREDENTIAL_MAX {
            t.note_refusal(Surface::WsRpcUpgrade, ip(1), &ACTOR);
        }
        assert!(!t.note_refusal(Surface::WsRpcUpgrade, ip(1), &OTHER_ACTOR));
    }

    /// The lockout case the composite key exists for: a flooder elsewhere
    /// claiming this actor's (public) id spends nothing of the holder's budget.
    #[test]
    fn a_remote_flooder_claiming_an_actor_spends_nothing_of_its_holders_budget() {
        let t = shipped();
        for _ in 0..500 {
            t.note_refusal(Surface::WsRpcUpgrade, ip(66), &ACTOR);
        }
        assert!(!t.note_refusal(Surface::WsRpcUpgrade, ip(1), &ACTOR));
        assert!(!t.is_exhausted(Surface::DeviceHandshake, ip(1), &ACTOR));
    }

    #[test]
    fn surfaces_do_not_share_a_budget() {
        let t = shipped();
        for _ in 0..=FAILED_CREDENTIAL_MAX {
            t.note_refusal(Surface::WsRpcUpgrade, ip(1), &ACTOR);
        }
        assert!(!t.note_refusal(Surface::DeviceHandshake, ip(1), &ACTOR));
    }

    #[test]
    fn the_pre_check_records_nothing() {
        let t = shipped();
        for _ in 0..1000 {
            assert!(!t.is_exhausted(Surface::DeviceHandshake, ip(1), &ACTOR));
        }
        for _ in 0..FAILED_CREDENTIAL_MAX {
            t.note_refusal(Surface::DeviceHandshake, ip(1), &ACTOR);
        }
        assert!(t.is_exhausted(Surface::DeviceHandshake, ip(1), &ACTOR));
    }

    #[test]
    fn a_missing_source_is_throttled_not_waved_through() {
        let t = shipped();
        for _ in 0..FAILED_CREDENTIAL_MAX {
            t.note_refusal(Surface::WsRpcUpgrade, None, &ACTOR);
        }
        assert!(t.note_refusal(Surface::WsRpcUpgrade, None, &ACTOR));
    }

    #[test]
    fn the_answer_is_429_with_a_retry_after_the_client_honours() {
        let resp = too_many_requests();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let secs: u64 = resp.headers()[RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(secs, RETRY_AFTER_SECS);
        assert!(Duration::from_secs(secs) <= fauna_ws_substrate::dial_budget::MAX_RETRY_AFTER);
    }

    #[test]
    fn a_burst_of_sheds_reports_once_per_surface() {
        let shed: [fauna_conn_limit::ShedCounter; SURFACES] = std::array::from_fn(|_| {
            fauna_conn_limit::ShedCounter::with_interval(Duration::from_secs(3600))
        });
        let t = FailedCredentialThrottle::with_shed(
            LimiterConfig {
                window: FAILED_CREDENTIAL_WINDOW,
                max_events: 1,
            },
            shed,
        );
        t.note_refusal(Surface::WsRpcUpgrade, ip(1), &ACTOR);
        for _ in 0..20 {
            assert!(t.note_refusal(Surface::WsRpcUpgrade, ip(1), &ACTOR));
        }
        // The upgrade surface's counter has spoken once; a burst never
        // suppresses another surface's first line.
        assert!(t.shed[Surface::WsRpcUpgrade.index()].record().is_none());
        assert!(t.shed[Surface::DeviceHandshake.index()].record().is_some());
    }
}
