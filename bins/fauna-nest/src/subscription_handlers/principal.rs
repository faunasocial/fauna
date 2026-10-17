//! The `fauna:follow:write` arm's three principal handlers
//! (`authorization-server.md` § Scope grammar → *The sixth arm*): follow and
//! unfollow as the account — the free `followers` tier only — and the
//! followers bit of its status. A paid tier is a purchase the user makes, so
//! no principal handler names one, writes one or reveals one.
//!
//! Each runs on a resolved [`PrincipalCaller`] (the gate in
//! `principal_handlers` has already checked the `ThirdParty` ceiling and the
//! arm), and calls the same cores the account's own handlers do:
//!
//! - **subscribe** refuses any tier but `followers` (`permission_denied`),
//!   fills the subscriber's ML-KEM encapsulation key from the account's
//!   registry entry (`fauna.subscriptions.encaps_key.publish`) and refuses the
//!   follow while there is none (`fauna.subscriptions.pq_key_unpublished`) —
//!   never a classical degrade — then runs [`super::subscribe_core`], whose
//!   self-follow, unhosted-author and hidden-tier refusals it inherits;
//! - **unsubscribe** is served only while `followers` is the one tier the
//!   account holds at that author — leaving a paid tier is the user's own act
//!   — and enqueues the `followers` removal for the author's client, exactly
//!   as every removal is committed;
//! - **status.get** answers following, pending or neither: `tier` is
//!   `followers` or nothing, never a paid tier's name, never an expiry.
//!
//! The two writes are rate-ceilinged per principal with the oracle's
//! `RateWindow` ([`crate::db::CacheDb::admit_follow_write`]).

use std::sync::Arc;

use bytes::Bytes;
use fauna_core::subscription::FOLLOWERS_TIER;
use fauna_protocol::subscriptions::{
    StatusGetReply, StatusGetRequest, SubscribeRequest, UnsubscribeReply, UnsubscribeRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use super::{
    MLKEM768_ENCAPS_KEY_LEN, follow_pending, internal, malformed_upload, not_subscribed,
    permission_denied, subscribe_core,
};
use crate::principal_handlers::{PrincipalCaller, PrincipalHandler};
use crate::routes::AppState;
use crate::rpc_errors::{coded_ns, encode_reply, rate_limited_ns};

/// The refusal of a follow whose account has published no encapsulation key:
/// the author could only wrap classical, and a silently weaker wrap for a
/// third-party follow is a posture hole (`authorization-server.md` § Scope
/// grammar → *The sixth arm*). The account's own apps publish the key at
/// session start, so the remedy is the user opening one.
fn pq_key_unpublished() -> RpcError {
    coded_ns(
        "subscriptions",
        "pq_key_unpublished",
        "the account has published no subscriber encapsulation key; open one of its apps",
    )
}

/// Admit one follow write against the principal's window, or refuse it.
async fn admit(state: &Arc<AppState>, caller: &PrincipalCaller) -> Result<(), RpcError> {
    let admitted = state
        .db
        .admit_follow_write(
            &caller.account,
            &caller.principal_id,
            u64::try_from(crate::db::now_epoch_secs()).unwrap_or(0),
        )
        .await
        .map_err(internal)?;
    if admitted {
        Ok(())
    } else {
        Err(rate_limited_ns("subscriptions"))
    }
}

/// The three kinds and their handlers, for the principal handler table.
pub(crate) fn principal_handlers() -> Vec<(&'static str, PrincipalHandler)> {
    vec![
        ("fauna.subscriptions.subscribe", subscribe_handler()),
        ("fauna.subscriptions.unsubscribe", unsubscribe_handler()),
        ("fauna.subscriptions.status.get", status_get_handler()),
    ]
}

fn subscribe_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload: Bytes| {
        Box::pin(async move {
            let mut req: SubscribeRequest = decode(&payload).map_err(malformed_upload)?;
            if req.tier != FOLLOWERS_TIER {
                return Err(permission_denied(
                    "a principal follows: only the followers tier may be subscribed",
                ));
            }
            // The key is the account's published one, never the request's: a
            // principal holds no identity seed, so whatever it sent could only
            // be a key nobody can open — or one it can.
            let ek = state
                .db
                .get_subscriber_encaps_key(&caller.account)
                .await
                .map_err(internal)?
                .filter(|ek| ek.len() == MLKEM768_ENCAPS_KEY_LEN)
                .ok_or_else(pq_key_unpublished)?;
            admit(&state, &caller).await?;
            req.mlkem_encaps_key = Some(serde_bytes::ByteBuf::from(ek));
            encode_reply(&subscribe_core(&state, caller.account, &req).await?)
        })
    })
}

fn unsubscribe_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload: Bytes| {
        Box::pin(async move {
            let req: UnsubscribeRequest = decode(&payload).map_err(malformed_upload)?;
            let author_id = req.author_id.0;
            let held = state
                .db
                .get_subscribed_tiers(&author_id, &caller.account)
                .await
                .map_err(internal)?;
            if !held.iter().any(|t| t == FOLLOWERS_TIER) {
                return Err(not_subscribed("not following this author"));
            }
            if held.iter().any(|t| t != FOLLOWERS_TIER) {
                // Leaving a paid tier is the user's own act; unfollowing
                // while one is held would leave it behind a follow the
                // tier's cascade re-grants.
                return Err(permission_denied(
                    "a principal unfollows only an author whose sole tier held is followers",
                ));
            }
            admit(&state, &caller).await?;
            let request_id = state
                .db
                .insert_subscribe_request(
                    &author_id,
                    &caller.account,
                    FOLLOWERS_TIER,
                    "unsubscribe",
                    None,
                )
                .await
                .map_err(internal)?;
            encode_reply(&UnsubscribeReply::Queued { request_id })
        })
    })
}

fn status_get_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload: Bytes| {
        Box::pin(async move {
            let req: StatusGetRequest = decode(&payload).map_err(malformed_upload)?;
            let author_id = req.author_id.0;
            let following = state
                .db
                .is_subscriber(&author_id, &caller.account, FOLLOWERS_TIER)
                .await
                .map_err(internal)?;
            let follow_pending =
                follow_pending(&state, &author_id, &caller.account, following).await?;
            encode_reply(&StatusGetReply {
                tier: following.then(|| FOLLOWERS_TIER.to_string()),
                expires_at: None,
                auto_approve: false,
                follow_pending,
                extra: Default::default(),
            })
        })
    })
}
