//! `fauna.region.artifact.get` — the app's region relay
//! (`region-blocking.md` § The content plane → *How an app obtains its region's
//! policy*).
//!
//! An app asks its own nest for one region's published artifact of one payload
//! kind; the nest answers **from its cache** and never from the network on the
//! request path. The cache is refilled by the region tier's worker
//! ([`crate::region_tier::refresh_relay_once`]) on the feature plane's cadence,
//! and a pair's first ask schedules one refill in the background so the app
//! need not wait a whole cadence for it.
//!
//! **A relay, not a trust point.** The reply is the envelope exactly as the nest
//! verified and stored it; the app verifies the signature — and, when served,
//! inclusion — itself, in shared Rust. The one thing this kind teaches the nest
//! is which regions its own apps declare, which it already knows more precisely
//! from the connection (§ The content plane states that rather than hiding it).
//!
//! **What this module will never grow**: a kind that submits an artifact, or an
//! admin lever over what is relayed (region-blocking.md invariant 5;
//! `region_tier.rs` rule 1). The answer is read from the cache without
//! consulting the registry at read time: the registry decides what is *fetched*
//! (rule 2) and what is *retired* (de-listing), never what a read returns.

use std::sync::Arc;
use std::time::Duration;

use fauna_core::region_authority::{
    InclusionEvidence, PAYLOAD_KIND_CONTENT_POLICY, PAYLOAD_KIND_FEATURE_POLICY, PolicyArtifact,
};
use fauna_protocol::region::{RegionArtifactGetReply, RegionArtifactGetRequest};

use crate::routes::AppState;
use crate::rpc_errors::internal;

/// The kind this module serves.
pub const KIND_REGION_ARTIFACT_GET: &str = "fauna.region.artifact.get";

/// The payload kinds the relay serves — the planes that have a published
/// artifact. A kind outside this set names no plane and is refused at the door
/// rather than cached as demand the worker would fetch 404s for forever.
const RELAYED_KINDS: [&str; 2] = [PAYLOAD_KIND_CONTENT_POLICY, PAYLOAD_KIND_FEATURE_POLICY];

/// Register the relay. Mirror any change in `KindRegistry::register_region_kinds`.
pub fn register_region_relay_handlers(b: &mut crate::rpc_router::RpcRouterBuilder) {
    b.add(
        KIND_REGION_ARTIFACT_GET,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: artifact_get_handler(),
        },
    );
}

fn artifact_get_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                KIND_REGION_ARTIFACT_GET,
                internal,
            )
            .await?;
            let req: RegionArtifactGetRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("region", e))?;
            if !RELAYED_KINDS.contains(&req.payload_kind.as_str()) {
                return Err(crate::rpc_errors::invalid_params_ns(
                    "region",
                    format!(
                        "unknown payload kind {:?} — the relay serves {RELAYED_KINDS:?}",
                        req.payload_kind
                    ),
                ));
            }

            // The ask IS the demand — but only for an ENROLLED region
            // : an unenrolled region is never fetched and records no
            // attempt (region-blocking.md § Implementation status today,
            // the relay paragraph), so recording nothing here is the door
            // half of that rule, not just the worker's. A de-listed
            // region's earlier demand row (written while it was enrolled)
            // is untouched by this check and still read below, which is
            // what keeps the demand-survives-de-listing property intact.
            // Public, unauthenticated-adjacent abuse surfaces are bounded
            // by construction (`docs/goal/architecture/transport-connection.md`
            // § Abuse posture); this is the same discipline applied to a
            // class-`User` door over a ~33M-code key space with no other
            // cap, rate or eviction on the table it would otherwise grow.
            demand_relay_artifact(
                &state,
                &crate::region_tier::demand_door_registry(&state),
                &req.region,
                &req.payload_kind,
            )
            .await
            .map_err(internal)?;

            let cached = state
                .db
                .relay_artifact(&req.region, &req.payload_kind)
                .await
                .map_err(internal)?;
            crate::rpc_errors::encode_reply(&reply_from(cached, crate::db::now_epoch_secs()))
        })
    })
}

/// Record demand for `(region, payload_kind)` against `registry`, spawning a
/// refill on the first ask. Returns `Ok(())` and touches nothing when the
/// region is not enrolled in `registry` : an unenrolled region is
/// never fetched and records no attempt (region-blocking.md § Implementation
/// status today, the relay paragraph). The one shape [`artifact_get_handler`]
/// and `region_tier::demand_situs_content_policies` both need.
///
/// Takes `registry` explicitly — the same seam
/// [`crate::region_tier::refresh_relay_once`] uses — rather than reading
/// [`crate::region_tier::relay_registry`] itself, so a test can exercise the
/// enrolled branch with a fixture registry: the compiled-in one is empty
/// today, which would otherwise make the enrolled branch untestable.
pub async fn demand_relay_artifact(
    state: &Arc<AppState>,
    registry: &fauna_core::region_authority::RegionRegistry,
    region: &fauna_core::region_authority::RegionCode,
    payload_kind: &str,
) -> anyhow::Result<()> {
    if registry.region(region).is_none() {
        return Ok(());
    }
    if state
        .db
        .request_relay_artifact(region, payload_kind)
        .await?
    {
        schedule_refill(state, region.clone(), payload_kind.to_string());
    }
    Ok(())
}

/// One background refill for a pair's first ask. Scoped to the serve
/// generation, so it is aborted with the server rather than outliving it.
/// Whether the region is enrolled is decided there, against the same registry
/// the worker uses — an unenrolled region is not fetched and records nothing.
///
/// Also what a region declaration schedules for its situs chain's content
/// policies (`region_tier`'s set handler), the nest-as-publisher leg's own
/// first ask.
pub(crate) fn schedule_refill(
    state: &Arc<AppState>,
    region: fauna_core::region_authority::RegionCode,
    payload_kind: String,
) {
    let task_state = Arc::clone(state);
    state.scope_handle(tokio::spawn(async move {
        crate::region_tier::refresh_relay_one(
            &task_state,
            &region,
            &payload_kind,
            &crate::region_tier::relay_registry(),
        )
        .await;
    }));
}

/// Build the reply from a cache row — a pure function of the row and the clock,
/// so the read path consults nothing else.
///
/// A stored envelope or evidence blob that no longer decodes is answered as
/// absent, loudly: it was verified when it was stored, so a failure here is this
/// build's decoder or a damaged row, and the honest answer to an app is "no
/// document" — never an envelope the nest could not read back.
pub(crate) fn reply_from(
    cached: Option<crate::db::region_tier::RelayCached>,
    now: i64,
) -> RegionArtifactGetReply {
    let Some(cached) = cached else {
        return RegionArtifactGetReply::default();
    };
    let envelope = cached.envelope.as_deref().and_then(|bytes| {
        fauna_protocol::decode_strict::<PolicyArtifact>(bytes)
            .map_err(|e| tracing::warn!("region relay: a cached envelope does not decode: {e}"))
            .ok()
    });
    // Evidence travels with ITS envelope, never on its own.
    let evidence = envelope
        .as_ref()
        .and(cached.evidence.as_deref())
        .and_then(|bytes| {
            fauna_protocol::decode_strict::<InclusionEvidence>(bytes)
                .map_err(|e| tracing::warn!("region relay: cached evidence does not decode: {e}"))
                .ok()
        });
    // Stale only where the nest has actually been trying: an unenrolled region
    // is never attempted, so it never reads as stale. Measured from the last
    // time the log ANSWERED — or, for a pair it has never answered, from the
    // first ask — so an unreachable log goes stale while the worker keeps
    // trying, which is what the warning is for.
    let stale = cached.attempted_at.is_some()
        && now.saturating_sub(cached.reached_at.unwrap_or(cached.requested_at))
            > crate::region_tier::STALE_AFTER.as_secs() as i64;
    RegionArtifactGetReply {
        envelope,
        evidence,
        last_checked_at: cached.reached_at.map(|t| t.max(0) as u64),
        stale,
        extra: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::reply_from;
    use crate::db::region_tier::RelayCached;
    use crate::region_tier::STALE_AFTER;

    fn row(requested_at: i64, attempted_at: Option<i64>, reached_at: Option<i64>) -> RelayCached {
        RelayCached {
            envelope: None,
            evidence: None,
            requested_at,
            attempted_at,
            reached_at,
        }
    }

    /// **Staleness reads the last time the log ANSWERED, not the last attempt**
    /// — so an unreachable log goes stale while the worker keeps trying, which
    /// is the one case the warning exists for. Keyed on the attempt, a worker
    /// that failed every six hours would bump the clock each time and never
    /// warn at all.
    #[test]
    fn an_unreachable_log_goes_stale_while_the_worker_keeps_trying() {
        let stale_after = STALE_AFTER.as_secs() as i64;
        let now = 100 * stale_after;

        // Reached recently: fresh, however recent the last attempt.
        assert!(!reply_from(Some(row(0, Some(now), Some(now - 60))), now).stale);

        // Attempted a moment ago, but the log last answered long ago: stale.
        assert!(
            reply_from(
                Some(row(0, Some(now - 60), Some(now - stale_after - 1))),
                now
            )
            .stale,
            "a failing worker must not keep the channel looking fresh"
        );

        // Never answered, asked long ago, still being tried: stale.
        assert!(reply_from(Some(row(now - stale_after - 1, Some(now), None)), now).stale);
    }

    /// Never attempted — an unenrolled region, or a pair asked for a moment ago —
    /// is never stale: no channel has gone quiet. And no row at all is the
    /// fresh-subject answer, "no document", not an error.
    #[test]
    fn a_pair_never_attempted_is_never_stale_and_no_row_is_no_document() {
        let now = 100 * STALE_AFTER.as_secs() as i64;
        let reply = reply_from(Some(row(0, None, None)), now);
        assert!(!reply.stale);
        assert_eq!(reply.envelope, None);
        assert_eq!(reply.last_checked_at, None);

        let none = reply_from(None, now);
        assert_eq!(none.envelope, None);
        assert!(!none.stale);
    }
}
