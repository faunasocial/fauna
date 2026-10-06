//! Nest **transport/abuse** policy — single-row admin overrides for nest's
//! **own** client-facing TLS listener caps. Distinct from the mail-scoped
//! `db/mail_policy.rs` domains (which nest serves to the Go mail bridge over
//! `fetch_config`): this one is read **nest-side** by `serve_tls` at boot and
//! hot-reloaded thereafter (`put_policy` calls `set_max` on the live limiter).
//!
//! By the product invariant a per-IP connection cap is an admin-tunable
//! abuse knob (same class as spam thresholds) → client-set nest config, not
//! an environment variable. The write path is the
//! `fauna.transport.put_policy` Admin RPC; this is its store. Persisted as a
//! single JSON blob on the fixed `id = 1` row, through the shared
//! [`super::singleton`] primitives (the same pair `db/mail_policy.rs` uses):
//! a missing row / missing field decodes as `None` ("use the catalog
//! default"), and the JSON shape lets a later track grow the policy (the
//! global cap / handshake timeout / header-read timeout are sibling
//! nest-listener transport knobs) without a migration.
//!
//! Spec: `docs/goal/architecture/transport-connection.md` § Abuse posture item (2).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::CacheDb;

/// Override for nest's own TLS-listener transport/abuse caps. Each `Some(v)`
/// sets the value; `None` keeps the catalog default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransportPolicyOverrides {
    /// Per-source-IP concurrent-connection cap on nest's TLS listener (keyed
    /// on the PROXY-v2-resolved real client IP; loopback-exempt). `None`
    /// keeps the `fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP` catalog default.
    pub max_conns_per_ip: Option<u32>,
}

/// The effective (override-or-default) transport policy. Non-optional: every
/// field has resolved to a concrete value. Produced by
/// [`TransportPolicyOverrides::effective`]. The cap `serve_tls` enforces
/// resolves the same way, treating a zero override as unset — see
/// `crate::resolve_tls_per_ip_cap` (the single source of truth so the boot
/// read, the hot-reload `set_max` on a `put_policy`, and the `get_policy` view
/// all agree).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTransportPolicy {
    pub max_conns_per_ip: u32,
}

impl TransportPolicyOverrides {
    /// Resolve each unset field to its catalog default (the `None ⇒ default`
    /// semantics mirror the mail alias-policy nest-side overlay). The per-IP
    /// default is `fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP`.
    pub fn effective(&self) -> ResolvedTransportPolicy {
        ResolvedTransportPolicy {
            max_conns_per_ip: self
                .max_conns_per_ip
                .unwrap_or(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP as u32),
        }
    }
}

impl CacheDb {
    /// Read the single JSON-blob override row, or the all-`None` default when
    /// no row has been written yet.
    pub async fn get_transport_policy(&self) -> Result<TransportPolicyOverrides> {
        self.read_singleton_json("transport_policy").await
    }

    /// Upsert the single JSON-blob override row. Idempotent on the fixed
    /// `id = 1` row; replaces the whole blob (the admin form submits the
    /// complete policy, so this is a PUT not a merge).
    pub async fn put_transport_policy(&self, overrides: TransportPolicyOverrides) -> Result<()> {
        self.write_singleton_json("transport_policy", &overrides)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_get_returns_default() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.get_transport_policy().await.unwrap(),
            TransportPolicyOverrides::default()
        );
    }

    #[tokio::test]
    async fn effective_uses_catalog_default_when_unset() {
        let db = CacheDb::open_in_memory().unwrap();
        let eff = db.get_transport_policy().await.unwrap().effective();
        assert_eq!(
            eff.max_conns_per_ip,
            fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP as u32
        );
    }

    #[tokio::test]
    async fn put_then_get_round_trips_and_effective_overrides() {
        let db = CacheDb::open_in_memory().unwrap();
        let put = TransportPolicyOverrides {
            max_conns_per_ip: Some(64),
        };
        db.put_transport_policy(put.clone()).await.unwrap();
        assert_eq!(db.get_transport_policy().await.unwrap(), put);
        assert_eq!(
            db.get_transport_policy()
                .await
                .unwrap()
                .effective()
                .max_conns_per_ip,
            64,
            "override wins over the catalog default"
        );
    }

    #[tokio::test]
    async fn put_is_idempotent_overwrite_on_fixed_row() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_transport_policy(TransportPolicyOverrides {
            max_conns_per_ip: Some(99),
        })
        .await
        .unwrap();
        db.put_transport_policy(TransportPolicyOverrides {
            max_conns_per_ip: Some(128),
        })
        .await
        .unwrap();
        assert_eq!(
            db.get_transport_policy().await.unwrap().max_conns_per_ip,
            Some(128)
        );
    }
}
