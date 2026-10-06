//! Per-domain DNS-record body assembly for multi-domain mail hosting.
//!
//! Implements `docs/goal/behavior/mail-multidomain.md` § Per-domain DNS
//! records: the six records nest publishes per `mail_domains` row (MX, SPF,
//! DKIM, DMARC, MTA-STS, TLSRPT). Pure, RFC-spec-driven body builders with no
//! I/O — nest (and, via UniFFI, a future Go MTA bridge) calls these to get
//! spec-compliant record bodies, then dispatches the writes through its
//! DNS-provider path.
//!
//! Gated behind the pure WASM-safe `dns-records` feature so the onboarding
//! provisioner (`libs/fauna-provisioning`) builds the identical matrix the nest
//! renders (`dns-management.md` § byte-for-byte). The `multidomain`-only extras
//! — [`per_domain::build_dkim_txt_record`] (reuses
//! [`crate::outbound::dkim::SigningAlg`] for the `k=` tag),
//! [`per_domain::build_mta_sts_txt_record`] (reuses
//! [`crate::outbound::mta_sts::compute_policy_version_hash`] for the `id=`), and
//! the full [`per_domain::build_domain_dns_records`] aggregator — are
//! `#[cfg(feature = "multidomain")]`-gated inside `per_domain.rs` (those three
//! `outbound` helpers are themselves `multidomain`-gated).

pub mod host;
pub mod per_domain;
pub mod verify;
