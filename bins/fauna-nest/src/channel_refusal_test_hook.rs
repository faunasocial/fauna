//! Test-only HTTP endpoints that make one channel refuse one **class** of
//! `fauna.conversations.channel.send` envelope, so a tier_3 journey can stage a
//! succession sweep that is genuinely *partial*.
//!
//! ## Why a hook, and why this exact shape
//!
//! The post-succession group sweep re-points every group in three ordered wire
//! steps per channel (`fauna_client_recovery::group_sweep::sweep_one`): the old
//! leaf's **add-successor** Commit, the in-group succession **statement** (an
//! Application envelope), then the successor's **remove-old** Commit. A sweep
//! that completes owes nothing, which is why `recovery-kit-sweep-retry-button`
//! — the affordance that finishes an unfinished sweep — had never once been
//! rendered or pressed by any e2e on any app (measured 2026-08-27: every
//! succession journey in both suites takes the render gate's ABSENCE half).
//!
//! Producing the owing arm needs exactly one wire step to fail, and **which**
//! step decides whether the retry can finish:
//!
//! * Refusing the **add** Commit is not it. `commit_add_successor` merges the
//!   add into the old engine locally before the publish, so a refusal there
//!   leaves the old engine an epoch ahead of the group with no way to
//!   re-author — the retry answers `NeedsMemberReAdd`, which is § Propagation's
//!   member-side remedy, not a finishable sweep.
//! * Refusing the **statement** is. The add landed, the successor joined, and
//!   `sweep_one` aborts the group before remove-old *by design* (so that "the
//!   old leaf is removed" keeps implying "the statement was offered to the
//!   members"). The group is left in precisely the resumable state the retry's
//!   own branch was written for: the successor holds the group and the old leaf
//!   is still seated, so the retry re-posts the statement and commits
//!   remove-old — `ResumedRemoveOnly`.
//!
//! So the hook selects by envelope **class**, never by a count of sends: a
//! class is decided per request with no ordering to lose a race against, which
//! is what keeps the staged arm deterministic rather than timing-dependent
//! (`e2e-conventions.md` point 14).
//!
//! ## What it does NOT do
//!
//! It refuses before `ingest_channel_envelope`, so a refused send consumes no
//! seq and no storage — the channel is exactly where it was, which is what
//! makes the fault a *flake* (safe to press again) rather than a hole. It is a
//! per-channel switch held in memory for the process's life; nothing here
//! touches at-rest state.
//!
//! Gated on `test-hooks` **alone** (like `bridge_status_test_hook` /
//! `link_preview_test_hook`) so it is present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build and in no
//! production build whatsoever (`e2e-conventions.md` point 15 — the feature is
//! the boundary, not a runtime env gate).
//!
//! Consumer: `tests/e2e-unified/tests/test_succession_sweep_retry.py`.
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// Which class of envelope a channel refuses.
///
/// Deliberately not an `Option<bool>`-shaped "refuse everything": the sweep's
/// three steps split across both classes, and the whole point is to fail one of
/// them (see the module doc). `All` exists for the blunt case a future journey
/// may want and costs nothing to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelRefusal {
    /// Refuse `ChannelEnvelope::Application` — the sweep's in-group succession
    /// statement, and ordinary chat.
    Application,
    /// Refuse `ChannelEnvelope::Commit` — membership changes and key updates.
    Commit,
    /// Refuse every envelope on the channel.
    All,
}

impl ChannelRefusal {
    /// Whether `body` (a raw `ChannelSendRequest.envelope`) is refused.
    ///
    /// **Lenient decode, mirroring the non-claimant commit rate cap in
    /// `channel_send_core`**: a body that does not decode as a
    /// `ChannelEnvelope` is simply "not of either class" here, never an error —
    /// the real decode happens downstream and owns that verdict. `All` still
    /// refuses it, because `All` is about the channel, not the payload.
    pub fn refuses(self, body: &[u8]) -> bool {
        use fauna_protocol::conversations::ChannelEnvelope;
        match self {
            Self::All => true,
            Self::Application => matches!(
                ChannelEnvelope::from_bytes(body),
                Ok(ChannelEnvelope::Application(_))
            ),
            Self::Commit => matches!(
                ChannelEnvelope::from_bytes(body),
                Ok(ChannelEnvelope::Commit(_))
            ),
        }
    }
}

#[derive(Deserialize)]
struct RefuseBody {
    /// `"application"` | `"commit"` | `"all"` — see [`ChannelRefusal`].
    envelope: String,
}

/// `POST /api/v1/test/conversations/channel/{channel_id}/refuse` — arm the
/// refusal. `channel_id` is the 64-hex channel id the client sends.
/// Returns `{"ok": true, "channel_id": <id>, "envelope": <class>}`.
async fn arm_refusal(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    Json(body): Json<RefuseBody>,
) -> impl IntoResponse {
    let class = match body.envelope.as_str() {
        "application" => ChannelRefusal::Application,
        "commit" => ChannelRefusal::Commit,
        "all" => ChannelRefusal::All,
        other => {
            return ApiError::bad_request(format!(
                "`envelope` must be one of application|commit|all, got {other:?}"
            ))
            .into_response();
        }
    };
    let Some(id) = decode_channel_id(&channel_id) else {
        return ApiError::bad_request("`channel_id` must be 64 hex characters").into_response();
    };
    state
        .channel_send_refusal
        .lock()
        .expect("channel_send_refusal mutex poisoned")
        .insert(id, class);
    Json(json!({ "ok": true, "channel_id": channel_id, "envelope": body.envelope })).into_response()
}

/// `POST /api/v1/test/conversations/channel/{channel_id}/clear` — disarm one
/// channel (test isolation, and the step a journey takes before proving the
/// retry can finish). Returns `{"ok": true}`.
async fn clear_refusal(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
) -> impl IntoResponse {
    let Some(id) = decode_channel_id(&channel_id) else {
        return ApiError::bad_request("`channel_id` must be 64 hex characters").into_response();
    };
    state
        .channel_send_refusal
        .lock()
        .expect("channel_send_refusal mutex poisoned")
        .remove(&id);
    Json(json!({ "ok": true })).into_response()
}

fn decode_channel_id(hex: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(hex.trim()).ok()
}

/// Mount the `/api/v1/test/conversations/channel/{id}/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/conversations/channel/{channel_id}/refuse",
            post(arm_refusal),
        )
        .route(
            "/api/v1/test/conversations/channel/{channel_id}/clear",
            post(clear_refusal),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::conversations::ChannelEnvelope;

    fn app_bytes() -> Vec<u8> {
        ChannelEnvelope::Application(vec![1, 2, 3])
            .to_bytes()
            .unwrap()
    }

    fn commit_bytes() -> Vec<u8> {
        ChannelEnvelope::Commit(vec![4, 5, 6]).to_bytes().unwrap()
    }

    /// The class split is the whole mechanism: the sweep's statement and its
    /// two commits must be separable, or the staged arm is the un-finishable
    /// `NeedsMemberReAdd` one instead of the resumable one.
    #[test]
    fn each_class_refuses_only_its_own_envelope() {
        assert!(ChannelRefusal::Application.refuses(&app_bytes()));
        assert!(!ChannelRefusal::Application.refuses(&commit_bytes()));
        assert!(ChannelRefusal::Commit.refuses(&commit_bytes()));
        assert!(!ChannelRefusal::Commit.refuses(&app_bytes()));
        assert!(ChannelRefusal::All.refuses(&app_bytes()));
        assert!(ChannelRefusal::All.refuses(&commit_bytes()));
    }

    /// An undecodable body is "not of either class" rather than an error — the
    /// downstream strict decode owns that verdict, and a hook that errored here
    /// would turn a malformed-envelope test into a hook failure.
    #[test]
    fn an_undecodable_body_is_refused_only_by_all() {
        let junk = b"not an envelope";
        assert!(!ChannelRefusal::Application.refuses(junk));
        assert!(!ChannelRefusal::Commit.refuses(junk));
        assert!(ChannelRefusal::All.refuses(junk));
    }
}
