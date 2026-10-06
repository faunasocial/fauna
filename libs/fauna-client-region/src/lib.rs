//! The app side of the region content plane (`docs/goal/behavior/region-blocking.md`
//! § The content plane) — shared by every app, so the only per-app code is the
//! one-function platform leaf that names the declared region
//! ([`source`]), the paint, and where the at-rest bytes live.
//!
//! The flow, per device:
//!
//! 1. The shell's leaf hands a [`DeclaredRegion`] (or `None`) to
//!    [`RegionPlane::new`], with [`effective_registry`].
//! 2. [`RegionPlane::load`] restores the last-known-good record **before** the
//!    first fetch.
//! 3. [`fetch_chain`] asks the nest's relay for every region on the declared
//!    chain; each reply goes through [`RegionPlane::apply_reply`], which verifies
//!    it in shared Rust (the nest is a relay, not a trust point), and the shell
//!    persists [`RegionPlane::to_bytes`].
//! 4. The render folds [`RegionPlane::rule_sets`] as the third source of the
//!    composed engine, with [`RegionPlane::labels_for`]'s scorer factors joined
//!    to each item's labels; the settings surface paints [`RegionPlane::view`].

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures;
pub mod iso3166;
pub mod plane;
pub mod registry;
pub mod render;
pub mod source;
#[cfg(not(target_arch = "wasm32"))]
pub mod store;

pub use plane::{PolicyRow, PolicyState, RegionPlane, RegionView};
pub use registry::effective_registry;
pub use render::{RegionPlaceholder, RegionVerb, placeholder_for};
pub use source::{
    DeclaredRegion, RegionSource, declared_from_bcp47, declared_from_posix_locale,
    region_from_posix_locale,
};

use fauna_core::region_authority::{
    PAYLOAD_KIND_CONTENT_POLICY, REFRESH_INTERVAL_SECS, RegionCode,
};
use fauna_core::scoring::LabelerPostInput;
use fauna_protocol::RpcRequester;
use fauna_protocol::region::{RegionArtifactGetReply, RegionArtifactGetRequest};

/// The relay kind (`bins/fauna-nest/src/region_relay.rs`).
pub const KIND_REGION_ARTIFACT_GET: &str = "fauna.region.artifact.get";

/// Ask the nest's relay for each region's content policy, in chain order.
///
/// Each answer is paired with its region; a failed ask is an `Err` the caller
/// **drops** — a failed fetch writes nothing (§ Fail posture), so only the `Ok`
/// arms ever reach [`RegionPlane::apply_reply`].
pub async fn fetch_chain<R: RpcRequester>(
    nest: &R,
    chain: &[RegionCode],
) -> Vec<(RegionCode, Result<RegionArtifactGetReply, String>)> {
    let mut out = Vec::with_capacity(chain.len());
    for region in chain {
        let reply = nest
            .request::<_, RegionArtifactGetReply>(
                KIND_REGION_ARTIFACT_GET,
                RegionArtifactGetRequest {
                    region: region.clone(),
                    payload_kind: PAYLOAD_KIND_CONTENT_POLICY.to_string(),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| e.to_string());
        out.push((region.clone(), reply));
    }
    out
}

/// Whether a relay refresh is due: at the first ask (`last_refresh` `None` —
/// login), then on the shared cadence every app and the nest run
/// ([`REFRESH_INTERVAL_SECS`]).
pub fn refresh_due(last_refresh: Option<u64>, now: u64) -> bool {
    last_refresh.is_none_or(|t| now.saturating_sub(t) >= REFRESH_INTERVAL_SECS)
}

/// The module input for an item the app holds as rendered parts rather than as
/// a `Post` — a feed card or a conversation bubble, post-decrypt. The same
/// fields `LabelerPostInput::from_post` maps, so a `wasm` scorer reads a card
/// exactly as it reads the post behind it.
pub fn scorer_input(
    author: fauna_core::identity::ActorId,
    text: &str,
    hashtags: &[String],
    has_media: bool,
) -> LabelerPostInput {
    LabelerPostInput {
        text: (!text.is_empty()).then(|| text.to_string()),
        hashtags: hashtags.to_vec(),
        has_media,
        media_type: None,
        duration_ms: None,
        author,
    }
}

#[cfg(test)]
mod tests;
