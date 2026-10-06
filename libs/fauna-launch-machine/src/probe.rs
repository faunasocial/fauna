//! Anonymous `fauna.setup.status` probe over WS-RPC.
//!
//! Two callers, and the difference between them is why this returns a
//! [`ClaimProbe`] rather than a `bool`:
//!
//! * The silent-challenge fallback (`machine.rs`, the
//!   `SilentChallengeOutcome::NotRegistered` arm): once a verify 404 tells us the
//!   actor isn't registered on this nest, we probe whether the nest is *claimed*
//!   to route the wizard between `invite_request` (claimed → an admin can issue an
//!   invite) and `claim_code` (unclaimed → the user claims). There, an
//!   unreachable box is safely read as claimed — `invite_request` is graceful.
//! * The pending-factory-reset boot reconcile (`machine.rs`, the slot row —
//!   `common.md` § Client-state recoverability, CR-2). There, "claimed" means
//!   **delete the slot**, and the slot holds the only copy of the claim code for a
//!   box that may really have been wiped. Reading an unreachable box as claimed
//!   would destroy that code on a network blip — CR-1 again.
//!
//! So the probe answers honestly in three ways and each caller picks its own safe
//! default; collapsing `Unreachable` into `Claimed` here would silently hand the
//! reconcile a data-losing default.
//!
//! This replaced the former HTTP probe (`GET /api/v1/setup-status` via
//! `fauna_provisioning::probe::probe_setup_status_at`) as part of the HTTP
//! retirement. The wire is now the
//! pre-identity anonymous connection (`GET /api/v1/ws`, `fauna.setup.status`);
//! the per-target connector is `fauna-anon-client` (native) /
//! `fauna-rpc-wasm` (wasm), both `RpcRequester`, so the kind call is written
//! once over `R`. One fresh connection per probe (no reuse → no stale-
//! connection class), mirroring the old per-request HTTP shape.

use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::{SetupStatusReply, SetupStatusRequest};

/// Pre-identity allowlist kind read here. Mirrors
/// `fauna-onboarding-machine`'s `kinds::SETUP_STATUS` and the nest's
/// `bins/fauna-nest/src/discovery_handlers.rs` registration.
const SETUP_STATUS_KIND: &str = "fauna.setup.status";

/// What an anonymous `fauna.setup.status` probe learned about a nest.
///
/// `Unreachable` is a distinct variant, not an error folded into `Claimed`,
/// precisely because the two callers need opposite defaults for it (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimProbe {
    /// The nest answered, and an admin has claimed it.
    Claimed,
    /// The nest answered, and it is still fresh/unclaimed.
    Unclaimed,
    /// The nest did not answer (connect, transport, decode, or server error).
    /// Says nothing about whether the box is claimed — only that we don't know.
    Unreachable,
}

/// Probe whether the nest at `nest_url` has been claimed.
pub(crate) async fn probe_setup_status(nest_url: &str) -> ClaimProbe {
    match probe_claimed_impl(nest_url).await {
        Ok(true) => ClaimProbe::Claimed,
        Ok(false) => ClaimProbe::Unclaimed,
        Err(()) => ClaimProbe::Unreachable,
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn probe_claimed_impl(nest_url: &str) -> Result<bool, ()> {
    let client = fauna_anon_client::AnonymousNestClient::connect(nest_url)
        .await
        .map_err(|_| ())?;
    request_claimed(&client).await
}

#[cfg(target_arch = "wasm32")]
async fn probe_claimed_impl(nest_url: &str) -> Result<bool, ()> {
    let client = fauna_rpc_wasm::AnonymousWsRpcClient::connect(nest_url).map_err(|_| ())?;
    request_claimed(&client).await
}

/// Single `fauna.setup.status` round trip, generic over the per-target
/// anonymous connector.
async fn request_claimed<R: RpcRequester>(client: &R) -> Result<bool, ()> {
    let reply: SetupStatusReply = client
        .request(SETUP_STATUS_KIND, SetupStatusRequest::default())
        .await
        .map_err(|_| ())?;
    Ok(reply.claimed)
}
