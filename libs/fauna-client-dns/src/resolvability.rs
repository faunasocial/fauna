//! The **native** production impl of
//! [`Dns01ResolvabilityProbe`](crate::acme_shared::Dns01ResolvabilityProbe):
//! ask the publish zone's **authoritative** nameservers, directly, whether they
//! serve the `_acme-challenge` TXT — the propagation gate's honest signal.
//!
//! The query itself lives in [`fauna_core::authoritative_dns`] — including the
//! full rationale for asking the authoritative servers directly rather than a
//! recursive resolver or DoH (negative caching; a provider control plane reports
//! what it *stored*, not what it *serves*). It is shared because the **nest**
//! runs the identical query behind `fauna.dns.probe_txt_visible`, which is how
//! the web app — with no raw DNS in the browser — gets this same signal instead
//! of a weaker approximation ([`crate::NestRelayedProbe`]). One mechanism, two
//! transports: in-process here, one RPC hop away there.

use std::net::SocketAddr;

use crate::acme_shared::Dns01ResolvabilityProbe;

/// Queries every authoritative NS of the publish zone for the challenge TXT.
#[derive(Default)]
pub struct AuthoritativeNsProbe {
    /// Test-only: skip NS discovery and treat these addresses as the zone's
    /// authoritative set (points the probe at an in-process responder).
    ns_override: Option<Vec<SocketAddr>>,
}

impl AuthoritativeNsProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test-only constructor — see `ns_override`.
    #[doc(hidden)]
    pub fn with_nameservers(ns: Vec<SocketAddr>) -> Self {
        Self {
            ns_override: Some(ns),
        }
    }
}

#[async_trait::async_trait]
impl Dns01ResolvabilityProbe for AuthoritativeNsProbe {
    async fn txt_visible(&self, zone_name: &str, record_name: &str, txt_value: &str) -> bool {
        fauna_core::authoritative_dns::authoritative_txt_visible(
            zone_name,
            record_name,
            txt_value,
            self.ns_override.as_deref(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fauna_core::authoritative_dns::spawn_responder;

    /// The delegation itself: the probe carries its NS override through to the
    /// shared primitive, so the trait impl really is the shared query.
    #[tokio::test]
    async fn visible_when_every_ns_serves_the_exact_value() {
        let a = spawn_responder("_acme-challenge.example.test.", "tok-123").await;
        let b = spawn_responder("_acme-challenge.example.test.", "tok-123").await;
        let probe = AuthoritativeNsProbe::with_nameservers(vec![a, b]);
        assert!(
            probe
                .txt_visible("example.test", "_acme-challenge.example.test", "tok-123")
                .await
        );
    }

    #[tokio::test]
    async fn not_visible_on_wrong_value_or_absent_record() {
        let ns = spawn_responder("_acme-challenge.example.test.", "tok-123").await;
        let probe = AuthoritativeNsProbe::with_nameservers(vec![ns]);
        assert!(
            !probe
                .txt_visible("example.test", "_acme-challenge.example.test", "other")
                .await,
            "wrong value must read as not-visible"
        );
        assert!(
            !probe
                .txt_visible("example.test", "_acme-challenge.other.test", "tok-123")
                .await,
            "absent record must read as not-visible"
        );
    }

    #[tokio::test]
    async fn not_visible_when_one_ns_lags() {
        let live = spawn_responder("_acme-challenge.example.test.", "tok-123").await;
        let lagging = spawn_responder("_acme-challenge.unrelated.test.", "x").await;
        let probe = AuthoritativeNsProbe::with_nameservers(vec![live, lagging]);
        assert!(
            !probe
                .txt_visible("example.test", "_acme-challenge.example.test", "tok-123")
                .await,
            "a partially-published zone (one NS lagging) must read as not-visible"
        );
    }

    #[tokio::test]
    async fn empty_ns_set_reads_as_not_visible() {
        let probe = AuthoritativeNsProbe::with_nameservers(Vec::new());
        assert!(
            !probe
                .txt_visible("example.test", "x.example.test", "v")
                .await
        );
    }
}
