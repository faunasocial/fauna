//! Native-client FFI for **client host-address reporting** — the windows / apple
//! / android leg of the `clients-host-address-onboarding` fan-out. Linux drives
//! the shared `fauna_client_dns::host_address::report_host_address` directly (it
//! links the Rust crates); the UniFFI apps cannot, so this wraps the **same
//! shared logic** as one free async fn (priority #2/#3; the native twin of a
//! desktop app's direct drive).
//!
//! Per priority #2 there is **no** new logic here. [`report_host_address`]:
//!   1. builds a [`DnsAdminClient`] over the authed `Arc<NestClient>` and reads
//!      the dial-address the client used to reach the nest
//!      (`NestClient::nest_url`);
//!   2. calls the shared decision fn, which classifies the dial-address (never
//!      publishing a private/LAN one), resolves a public name via the
//!      [`NativeHostAddressProbe`] (real tokio DNS; STUN deferred → `None`), and
//!      reports the public IP via `fauna.dns.set_host_address` when determinable;
//!   3. returns the [`FfiHostAddressOutcome`] so the per-app launch glue can log
//!      it (fire-and-forget — a `SkippedNoPublicIp` on a LAN box is normal, and a
//!      `Failed` retries idempotently on the next connect).
//!
//! Call at onboarding/claim success and at the client's **universal post-auth
//! hook**, admin-gated (the same place the client runs
//! `self_heal_deployment_seed_custody`).
//! Idempotent last-writer-wins on the nest, so repeats are harmless.
//!
//! Gated behind the default-on `host-address` feature so the Go mail-bridge
//! `--no-default-features` FFI build drops it (the bridge is a server with no
//! onboarding surface — same dead-code rationale as `deployment-seed`).
//! `docs/goal/architecture/nest/domains-and-tls-bootstrap.md`
//! § Host-address acquisition.

use std::sync::Arc;

use fauna_client_dns::DnsAdminClient;
use fauna_client_dns::host_address::{HostAddressOutcome, NativeHostAddressProbe};

use crate::nest_client::FfiNestClient;

/// Outcome of [`report_host_address`], the FFI twin of
/// [`fauna_client_dns::host_address::HostAddressOutcome`]. All three are
/// fire-and-forget — the per-app glue only **logs** (no user-facing surface):
/// [`Reported`](Self::Reported) = the nest now knows its public IPv4;
/// [`SkippedNoPublicIp`](Self::SkippedNoPublicIp) = a LAN box with no reflector, or
/// a name that would not resolve to a global address (the spec'd safe floor — no
/// alarm); [`Failed`](Self::Failed) = the RPC was refused/dropped, retried on the
/// next connect (a non-admin caller lands here too, harmlessly).
#[derive(uniffi::Enum)]
pub enum FfiHostAddressOutcome {
    /// Reported this public IPv4 as the deployment's `nest_ipv4` == `mail_ipv4`.
    Reported {
        /// The reported public IPv4 (single-box: nest and mail share it).
        nest_ipv4: String,
    },
    /// No public IP was determinable → reported **nothing**; the nest keeps its
    /// self-signed floor + weak resolve-gate. Expected on a home-LAN box.
    SkippedNoPublicIp,
    /// The `set_host_address` RPC failed (transport fault or nest rejection).
    /// Non-fatal — a later connect retries idempotently.
    Failed {
        /// The transport / rejection error, for the log line.
        error: String,
    },
}

impl From<HostAddressOutcome> for FfiHostAddressOutcome {
    fn from(o: HostAddressOutcome) -> Self {
        match o {
            HostAddressOutcome::Reported(req) => Self::Reported {
                nest_ipv4: req.nest_ipv4,
            },
            HostAddressOutcome::SkippedNoPublicIp => Self::SkippedNoPublicIp,
            HostAddressOutcome::Failed(error) => Self::Failed { error },
        }
    }
}

/// Report the nest's **public** IP so it gates ACME HTTP-01 on the *strong*
/// resolve-check and assembles the apex/`mail.` records — the native
/// (windows/apple/android) twin of linux's direct
/// `fauna_client_dns::host_address::report_host_address` drive and the web
/// `reportHostAddress` binding (`box`/`domains-and-tls-bootstrap.md`
/// § Host-address acquisition). The admin client is the authority; call it at
/// onboarding/claim success and idempotently at the universal post-auth hook,
/// **admin-gated** by the caller (a non-admin's call is refused nest-side and
/// surfaces as [`FfiHostAddressOutcome::Failed`], so gating avoids a pointless
/// failing RPC on every non-admin connect). Fire-and-forget: the glue logs the
/// outcome. Never publishes a private/LAN address (the safety invariant lives in
/// the shared fn).
#[fauna_uniffi_async::export]
pub async fn report_host_address(nest: Arc<FfiNestClient>) -> FfiHostAddressOutcome {
    let dns = DnsAdminClient::new(nest.nest_arc());
    // The address the client used to reach the nest — classified public/private.
    let dial_url = nest.nest_arc().nest_url();
    let probe = NativeHostAddressProbe;
    fauna_client_dns::host_address::report_host_address(&dns, &dial_url, &probe)
        .await
        .into()
}
