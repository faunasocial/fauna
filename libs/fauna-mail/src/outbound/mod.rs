//! Outbound mail policy modules.
//!
//! Implements `docs/goal/behavior/smtp-server.md` § Outbound delivery as
//! pure-Rust policy types behind trait boundaries (retry-schedule, bounce-
//! policy, backscatter rules) so the Go MTA and the retired `libs/fauna-bridge-smtp`
//! crate (both consume, or consumed, the same logic).
//!
//! Modules land here one item at a time per the TODO; each is feature-
//! gated through this module's parent `outbound` feature.

pub mod arc;
pub mod autoreply;
pub mod backscatter;
pub mod classifier;
pub mod dane;
pub mod dkim;
pub mod dsn;
pub mod metrics;
pub mod mta_sts;
pub mod mx;
pub mod received_strip;
pub mod retry;
pub mod tlsrpt;

/// Build a plain `hickory-resolver` async DNS resolver over the
/// system/root recursive config — no DNSSEC validation, no other options.
/// The shared constructor for every native resolver that needs nothing
/// beyond that ([`mta_sts::LiveMtaStsFetcher::new`],
/// [`tlsrpt::LiveTlsrptPolicyFetcher::new`], and `bins/fauna-nest`'s
/// `dns_verifier::LiveRecordResolver::new`, which builds no options either —
/// found byte-identical across all three by the same-crate/cross-crate arms
/// of the dev-fleet near-duplicate-function scanner). [`dane::lookup_tlsa`]
/// deliberately does NOT use this: it needs `ResolverOpts::validate = true`
/// (DNSSEC chain validation for TLSA), a real behavioral difference this
/// constructor must not paper over.
#[cfg(feature = "outbound-net")]
pub fn build_hickory_resolver() -> anyhow::Result<hickory_resolver::TokioResolver> {
    use anyhow::Context;
    hickory_resolver::TokioResolver::builder_tokio()
        .context("failed to build DNS resolver")?
        .build()
        .context("failed to construct DNS resolver")
}
