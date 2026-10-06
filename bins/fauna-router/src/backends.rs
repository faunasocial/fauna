//! Backend pool — health tracking and capacity-based assignment.
//!
//! Each backend wraps a running `fauna-nest` instance reachable over WireGuard.
//! The pool picks the healthiest backend with the most remaining capacity for
//! new user registrations.
//!
//! No served route consumes the registration side of the pool today: the HTTP
//! register and NodeInfo routes that read it (`select_for_registration`,
//! `accepts_registrations`, `handle_domain`) were removed before the public
//! release, and the roaming-nest WS-RPC frontend that will onboard actors again
//! is not yet designed (`nest/worker.md` § `fauna-router`). The poll still runs:
//! it decides backend health, and its report is that frontend's input.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::BackendConfig;
use crate::db::ProxyDb;

// ── RouterStatus ─────────────────────────────────────────────────────────────

/// The backend nest's `GET /internal/router-status` body.
///
/// Contract owner: `docs/goal/architecture/nest/worker.md` § Router Status
/// Endpoint. Every field is a projection of **client-set nest state**, which is
/// why the router keeps no copy of any of it in its own config: the nest is the
/// authority and this poll is how the router learns its choices.
///
/// Additive-only, and tolerant by construction: unknown fields from a newer nest
/// are ignored, and fields absent from a status body fall back to a
/// conservative default rather than failing the poll.
#[derive(Debug, Default, Deserialize)]
pub struct RouterStatus {
    /// Ceiling derived from the nest's client-set node storage cap.
    #[serde(default)]
    pub max_users: u64,
    /// Live registration posture, projected as `nest.info` projects it. A status
    /// body omitting the field (serde default) reads as `false`; see
    /// [`BackendPool::accepts_registrations`] for why a conservative answer is
    /// safe here.
    #[serde(default)]
    pub registration_open: bool,
    /// `true` when the posture is invite-only. Invite-only is still *open* —
    /// registration is accepted, with an invite.
    #[serde(default)]
    pub invite_required: bool,
    /// The domain the backend mints handles under — its client-set primary
    /// domain (or artifact seed); `None` on a domainless box (or an absent
    /// field, via the serde default). Read by [`BackendPool::handle_domain`];
    /// the router's own `[registration] handle_domain` TOML
    /// key was the last hand-edited copy of this and is deleted.
    #[serde(default)]
    pub handle_domain: Option<String>,
}

// ── BackendState ─────────────────────────────────────────────────────────────

/// Runtime state for a single backend nest.
///
/// The fields above the divider are **artifact wiring** — which nests exist and
/// where to reach them, set by whatever stood the deployment up. The fields
/// below it are **client-set nest state**, learned from every poll and never
/// configured here (see [`RouterStatus`]).
pub struct BackendState {
    pub name: String,
    /// 32-byte nest identity key.
    pub nest_id: [u8; 32],
    /// WireGuard tunnel IP of the backend.
    pub tunnel_ip: IpAddr,
    /// HTTP port the backend listens on.
    pub port: u16,
    /// Deployment-assigned labels (e.g. `["region:eu-west"]`).
    pub labels: Vec<String>,

    /// Maximum number of users this backend should serve — the nest's own
    /// client-set ceiling, adopted from the latest poll. `0` until the first
    /// successful poll, so a backend the router has never heard from offers no
    /// capacity (it is also unhealthy until then).
    pub max_users: AtomicU64,
    /// The backend's live registration posture, adopted from the latest poll.
    pub registration_open: AtomicBool,
    /// The backend's reported handle domain, adopted from the latest poll.
    pub handle_domain: RwLock<Option<String>>,
    /// `true` once the backend has passed a health check.
    pub healthy: AtomicBool,
    /// Wall-clock time of the most recent health check attempt.
    pub last_health_check: RwLock<Instant>,
}

impl BackendState {
    /// Adopt the nest's latest self-report. This is the *only* way capacity,
    /// posture and handle domain enter the router.
    pub fn apply_status(&self, status: &RouterStatus) {
        self.max_users.store(status.max_users, Ordering::Relaxed);
        self.registration_open
            .store(status.registration_open, Ordering::Relaxed);
        *self.handle_domain.write().unwrap() = status.handle_domain.clone();
    }

    /// Current remaining capacity: `max_users - count_by_nest`.
    ///
    /// Returns `0` if the nest is already at or over capacity, or if the DB
    /// query fails.
    pub fn remaining_capacity(&self, db: &ProxyDb) -> u64 {
        let max_users = self.max_users.load(Ordering::Relaxed);
        let used = db.count_by_nest(&self.nest_id).unwrap_or(max_users);
        max_users.saturating_sub(used)
    }
}

// We hand-roll Debug to avoid requiring Debug on Instant / AtomicBool.
impl std::fmt::Debug for BackendState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendState")
            .field("name", &self.name)
            .field("tunnel_ip", &self.tunnel_ip)
            .field("port", &self.port)
            .field("max_users", &self.max_users.load(Ordering::Relaxed))
            .field("healthy", &self.healthy.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

// ── BackendPool ───────────────────────────────────────────────────────────────

/// Collection of all configured backends.
#[derive(Debug, Default)]
pub struct BackendPool {
    pub backends: Vec<Arc<BackendState>>,
}

impl BackendPool {
    /// Build the pool from configuration.
    ///
    /// Backends start *unhealthy* and become healthy once the first
    /// `health_check` passes.
    ///
    /// Returns an error if `nest_id` cannot be decoded from hex or `tunnel_ip`
    /// cannot be parsed as an IP address.
    pub fn from_config(configs: &[BackendConfig]) -> Result<Self> {
        let mut backends = Vec::with_capacity(configs.len());
        for cfg in configs {
            let nest_id: [u8; 32] = fauna_core::hex32::decode(&cfg.nest_id)
                .with_context(|| format!("backend '{}': invalid nest_id hex", cfg.name))?;
            let tunnel_ip: IpAddr = cfg.tunnel_ip.parse().with_context(|| {
                format!(
                    "backend '{}': invalid tunnel_ip '{}'",
                    cfg.name, cfg.tunnel_ip
                )
            })?;

            backends.push(Arc::new(BackendState {
                name: cfg.name.clone(),
                nest_id,
                tunnel_ip,
                port: cfg.port,
                labels: cfg.labels.clone(),
                // Capacity and posture are the nest's to state, not the config's
                // to assert: both stay at their fail-closed zero until the first
                // poll reports them.
                max_users: AtomicU64::new(0),
                registration_open: AtomicBool::new(false),
                handle_domain: RwLock::new(None),
                healthy: AtomicBool::new(false),
                last_health_check: RwLock::new(Instant::now()),
            }));
        }
        Ok(Self { backends })
    }

    /// Find a backend whose `nest_id` matches `id`.
    pub fn find_by_nest_id(&self, id: &[u8; 32]) -> Option<Arc<BackendState>> {
        self.backends.iter().find(|b| &b.nest_id == id).cloned()
    }

    /// Pick the healthy backend with the most remaining capacity.
    ///
    /// Returns `None` if no healthy backend has any remaining capacity.
    pub fn select_for_registration(&self, db: &ProxyDb) -> Option<Arc<BackendState>> {
        self.backends
            .iter()
            .filter(|b| b.healthy.load(Ordering::Relaxed))
            .filter_map(|b| {
                let cap = b.remaining_capacity(db);
                if cap > 0 { Some((b, cap)) } else { None }
            })
            .max_by_key(|(_, cap)| *cap)
            .map(|(b, _)| Arc::clone(b))
    }

    /// Does any healthy backend currently accept registrations?
    ///
    /// The deployment accepts new users iff some nest behind it says it does
    /// (it fed the removed HTTP NodeInfo's `openRegistrations`). It is
    /// a **report, not a gate** — the router never refuses a registration on the
    /// strength of it. Enforcement belongs to the nest alone
    /// (`fauna.account.register`), which is what keeps the two from disagreeing;
    /// a second gate here could only ever contradict the authority.
    ///
    /// A status body without the posture report therefore reads as not-open, which
    /// costs nothing: registrations are still routed to it and it still answers
    /// authoritatively. The only effect is a conservative report.
    pub fn accepts_registrations(&self) -> bool {
        self.backends.iter().any(|b| {
            b.healthy.load(Ordering::Relaxed) && b.registration_open.load(Ordering::Relaxed)
        })
    }

    /// The deployment's handle domain, as reported by the first healthy backend
    /// that states one (poll order — in practice the backends of one deployment
    /// share a domain) — nest state via the poll, never a router config key
    /// (it fed the removed HTTP NodeInfo's `handleDomain` metadata). `None`
    /// until a healthy backend reports a domain.
    pub fn handle_domain(&self) -> Option<String> {
        self.backends
            .iter()
            .filter(|b| b.healthy.load(Ordering::Relaxed))
            .find_map(|b| b.handle_domain.read().unwrap().clone())
    }

    /// Perform a health check against `backend`.
    ///
    /// Issues a `GET /internal/router-status` request and **adopts the reported
    /// capacity and posture** — the poll is how client-set nest state reaches the
    /// router, so discarding the body would leave the router routing on stale
    /// assumptions. Marks the backend healthy on a `2xx` whose body parses,
    /// unhealthy otherwise (including network errors): a backend whose report is
    /// unreadable is one whose capacity we do not know, and inventing one could
    /// overfill a nest.
    pub async fn health_check(backend: &BackendState, client: &reqwest::Client) {
        let url = format!("{}/internal/router-status", Self::backend_url(backend));
        let status: Option<RouterStatus> = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => match r.json::<RouterStatus>().await {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!(
                        backend = %backend.name,
                        error = %e,
                        "router-status body did not parse; treating backend as unhealthy"
                    );
                    None
                }
            },
            Ok(r) => {
                tracing::debug!(backend = %backend.name, status = %r.status(), "router-status not ok");
                None
            }
            Err(e) => {
                tracing::debug!(backend = %backend.name, error = %e, "router-status unreachable");
                None
            }
        };

        match status {
            Some(s) => {
                backend.apply_status(&s);
                backend.healthy.store(true, Ordering::Relaxed);
            }
            None => backend.healthy.store(false, Ordering::Relaxed),
        }

        if let Ok(mut guard) = backend.last_health_check.write() {
            *guard = Instant::now();
        }
    }

    /// Base URL for direct communication with `backend`.
    pub fn backend_url(backend: &BackendState) -> String {
        format!("http://{}:{}", backend.tunnel_ip, backend.port)
    }
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(name: &str, nest_id_hex: &str, tunnel_ip: &str, port: u16) -> BackendConfig {
        BackendConfig {
            name: name.to_string(),
            nest_id: nest_id_hex.to_string(),
            tunnel_ip: tunnel_ip.to_string(),
            port,
            labels: vec![],
        }
    }

    /// A backend whose nest has reported the given ceiling and an open posture —
    /// the state a pool reaches after one successful poll.
    fn reported(max_users: u64) -> RouterStatus {
        RouterStatus {
            max_users,
            registration_open: true,
            ..Default::default()
        }
    }

    #[test]
    fn from_config_parses_backends() {
        let id_a = "aa".repeat(32); // 64 hex chars
        let id_b = "bb".repeat(32);
        let configs = vec![
            make_config("nest-a", &id_a, "10.0.0.1", 3001),
            make_config("nest-b", &id_b, "10.0.0.2", 3002),
        ];

        let pool = BackendPool::from_config(&configs).expect("from_config failed");

        assert_eq!(pool.backends.len(), 2);

        let a = &pool.backends[0];
        assert_eq!(a.name, "nest-a");
        assert_eq!(a.port, 3001);
        assert_eq!(a.tunnel_ip, "10.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(a.nest_id, [0xaa; 32]);
        assert!(!a.healthy.load(Ordering::Relaxed), "should start unhealthy");

        let b = &pool.backends[1];
        assert_eq!(b.name, "nest-b");
        assert_eq!(b.nest_id, [0xbb; 32]);
    }

    #[test]
    fn from_config_rejects_bad_hex() {
        let cfg = make_config(
            "bad",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "10.0.0.1",
            3000,
        );
        assert!(BackendPool::from_config(&[cfg]).is_err());
    }

    #[test]
    fn from_config_rejects_wrong_length_hex() {
        // 62 hex chars → 31 bytes
        let cfg = make_config("short", &"aa".repeat(31), "10.0.0.1", 3000);
        assert!(BackendPool::from_config(&[cfg]).is_err());
    }

    #[test]
    fn from_config_rejects_bad_ip() {
        let cfg = make_config("bad-ip", &"cc".repeat(32), "not-an-ip", 3000);
        assert!(BackendPool::from_config(&[cfg]).is_err());
    }

    #[test]
    fn find_by_nest_id_works() {
        let id_a = "aa".repeat(32);
        let id_b = "bb".repeat(32);
        let configs = vec![
            make_config("nest-a", &id_a, "10.0.0.1", 3001),
            make_config("nest-b", &id_b, "10.0.0.2", 3002),
        ];

        let pool = BackendPool::from_config(&configs).unwrap();

        let found = pool.find_by_nest_id(&[0xaa; 32]);
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "nest-a");

        let found_b = pool.find_by_nest_id(&[0xbb; 32]);
        assert!(found_b.is_some());
        assert_eq!(found_b.unwrap().name, "nest-b");

        // Unknown nest_id returns None.
        assert!(pool.find_by_nest_id(&[0xff; 32]).is_none());
    }

    #[test]
    fn select_for_registration_picks_most_capacity() {
        let db = ProxyDb::open_in_memory().unwrap();

        // Populate nest-a with 60 users, nest-b with 10 users.
        // max_users = 100 for both → nest-b has more capacity.
        let nest_a_id = [0xaa; 32];
        let nest_b_id = [0xbb; 32];

        for i in 0u8..60 {
            let actor = vec![i; 32];
            db.insert_route(&actor, &nest_a_id, &format!("user_a_{i}"))
                .unwrap();
        }
        for i in 0u8..10 {
            let actor = vec![200 + i; 32];
            db.insert_route(&actor, &nest_b_id, &format!("user_b_{i}"))
                .unwrap();
        }

        let configs = vec![
            make_config("nest-a", &"aa".repeat(32), "10.0.0.1", 3001),
            make_config("nest-b", &"bb".repeat(32), "10.0.0.2", 3002),
        ];
        let pool = BackendPool::from_config(&configs).unwrap();

        // Both nests report the same ceiling → nest-b has more room left.
        pool.backends[0].apply_status(&reported(100));
        pool.backends[1].apply_status(&reported(100));

        // Both start unhealthy → no selection.
        assert!(pool.select_for_registration(&db).is_none());

        // Mark both healthy.
        pool.backends[0].healthy.store(true, Ordering::Relaxed);
        pool.backends[1].healthy.store(true, Ordering::Relaxed);

        let selected = pool.select_for_registration(&db).unwrap();
        assert_eq!(
            selected.name, "nest-b",
            "nest-b has more remaining capacity"
        );
    }

    #[test]
    fn backend_url_format() {
        let cfg = make_config("x", &"ee".repeat(32), "10.99.0.1", 8080);
        let pool = BackendPool::from_config(&[cfg]).unwrap();
        let url = BackendPool::backend_url(&pool.backends[0]);
        assert_eq!(url, "http://10.99.0.1:8080");
    }

    // ── the nest is the authority for capacity + posture ──────────────────────

    /// Capacity comes from the nest's client-set storage cap, reported on every
    /// poll — never from a router-side config value. A backend the router has
    /// not heard from yet has **no** capacity (it also starts unhealthy), so a
    /// stale or absent report can never invent room on a nest.
    #[test]
    fn a_backend_has_no_capacity_until_the_nest_reports_it() {
        let db = ProxyDb::open_in_memory().unwrap();
        let pool =
            BackendPool::from_config(&[make_config("nest-a", &"aa".repeat(32), "10.0.0.1", 3001)])
                .unwrap();
        let a = &pool.backends[0];

        assert_eq!(
            a.remaining_capacity(&db),
            0,
            "capacity must not exist before the nest has reported one"
        );

        a.apply_status(&RouterStatus {
            max_users: 100,
            registration_open: true,
            ..Default::default()
        });
        assert_eq!(a.remaining_capacity(&db), 100);
    }

    /// The nest's live client-set cap is adopted on every poll, so lowering the
    /// cap from a client shrinks the router's view without touching the router.
    #[test]
    fn capacity_tracks_the_nests_live_cap_across_polls() {
        let db = ProxyDb::open_in_memory().unwrap();
        let pool =
            BackendPool::from_config(&[make_config("a", &"aa".repeat(32), "10.0.0.1", 3001)])
                .unwrap();
        let a = &pool.backends[0];

        a.apply_status(&RouterStatus {
            max_users: 100,
            registration_open: true,
            ..Default::default()
        });
        assert_eq!(a.remaining_capacity(&db), 100);

        // The admin lowers the nest's storage cap in their client; the next poll
        // reports the smaller ceiling.
        a.apply_status(&RouterStatus {
            max_users: 10,
            registration_open: true,
            ..Default::default()
        });
        assert_eq!(a.remaining_capacity(&db), 10);
    }

    /// `openRegistrations` is a projection of the backends' live posture. A status
    /// body without the posture report is reported as not-open rather than guessed
    /// open — this is a *report* only, so a conservative answer costs nothing:
    /// registration itself is still routed and the nest still decides.
    #[test]
    fn accepts_registrations_reflects_backend_posture_only_when_healthy() {
        let pool = BackendPool::from_config(&[
            make_config("a", &"aa".repeat(32), "10.0.0.1", 3001),
            make_config("b", &"bb".repeat(32), "10.0.0.2", 3002),
        ])
        .unwrap();
        let (a, b) = (&pool.backends[0], &pool.backends[1]);

        let open = RouterStatus {
            max_users: 100,
            registration_open: true,
            ..Default::default()
        };
        let closed = RouterStatus {
            max_users: 100,
            registration_open: false,
            ..Default::default()
        };

        a.apply_status(&closed);
        b.apply_status(&open);
        assert!(
            !pool.accepts_registrations(),
            "an unhealthy backend's posture must not be advertised"
        );

        a.healthy.store(true, Ordering::Relaxed);
        assert!(
            !pool.accepts_registrations(),
            "only nest-a is healthy, and it is closed"
        );

        b.healthy.store(true, Ordering::Relaxed);
        assert!(
            pool.accepts_registrations(),
            "a healthy open backend means the deployment accepts registrations"
        );

        // The admin closes nest-b from their client.
        b.apply_status(&closed);
        assert!(!pool.accepts_registrations());
    }

    /// Invite-only is `open` + `invite_required` — the deployment still accepts
    /// registrations (with an invite), so the pool must not report it closed.
    #[test]
    fn an_invite_only_backend_still_accepts_registrations() {
        let pool =
            BackendPool::from_config(&[make_config("a", &"aa".repeat(32), "10.0.0.1", 3001)])
                .unwrap();
        let a = &pool.backends[0];
        a.healthy.store(true, Ordering::Relaxed);

        a.apply_status(&RouterStatus {
            max_users: 100,
            registration_open: true,
            invite_required: true,
            ..Default::default()
        });
        assert!(pool.accepts_registrations());
    }

    /// A status body that omits the posture still parses — the report
    /// is additive — and degrades to not-open rather than failing the poll.
    #[test]
    fn a_status_body_without_posture_still_parses() {
        let body = serde_json::json!({
            "nest_id": "aa",
            "healthy": true,
            "max_users": 250,
            "current_users": 3,
            "version": "0.1.0",
        });
        let parsed: RouterStatus =
            serde_json::from_value(body).expect("a posture-less status body must parse");
        assert_eq!(parsed.max_users, 250);
        assert!(!parsed.registration_open);
        assert!(!parsed.invite_required);
    }
}
