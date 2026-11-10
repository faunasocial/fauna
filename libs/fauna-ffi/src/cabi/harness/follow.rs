//! The federated follow's two client halves, app-less — so a `[nest]` witness
//! of what the nests do with a follow across them (`federated-follow.md`
//! § Decision 5: delivery stops at a leave, a removal reaches the follower's
//! nest, a succession keeps the follow) observes real follows with no app in
//! it.
//!
//! - [`fauna_harness_follow`] — the subscriber's gesture: the shared
//!   [`SubscriptionsClient::subscribe_across_nests`] /
//!   [`SubscriptionsClient::unsubscribe_across_nests`] the apps' follow and
//!   leave buttons call, the signed `FollowIntent` minted from the seed and
//!   naming the connected nest.
//! - [`fauna_harness_author_pass`] — the author's client: one author-pump
//!   tick ([`SubscriptionsAuthor::reconcile_once`], what every native app's
//!   reconcile loop runs, so a queued follow is approved and its `KeyBlob`
//!   minted), or a removal of one subscriber
//!   ([`SubscriptionsAuthor::remove_subscriber`], the roster's remove door),
//!   each over a [`Seat`]'s period-key custody (`fauna.state.period-keys`) so
//!   the key a tick mints rests where the account's next device reads it.
//! - [`fauna_harness_redeem`] — the buyer's redeem gesture: the shared
//!   [`PaymentsClient::claims_redeem_across_nests`] the apps' claim-redeem
//!   button calls, so a code minted on another nest is redeemed through the
//!   buyer's own nest carrying their own signed intent (`monetization.md`
//!   § Pillar 3 → *Which nest an in-app redeem binds on*).

use std::ffi::c_char;
use std::sync::Arc;

use fauna_client::NestClientError;
use fauna_client_account_runtime::period_keys::PlanePeriodKeys;
use fauna_client_payments::PaymentsClient;
use fauna_client_subscriptions::orchestration::SubscriptionsAuthor;
use fauna_client_subscriptions::{FollowError, SubscriptionsClient};
use fauna_core::identity::ActorId;
use fauna_protocol::CrossNestError;

use super::{CloneKeypair, Outcome, decode_call, encode_err, err, run, with_client, with_seat};
use crate::FfiError;
use crate::cabi::FfiBuffer;

fn actor_id(bytes: &[u8], what: &str) -> Result<ActorId, FfiError> {
    <[u8; 32]>::try_from(bytes)
        .map(ActorId)
        .map_err(|_| err(format!("{what} must be a 32-byte actor id")))
}

/// A follow call's result as a nest answer: the reply or the nest's refusal;
/// an intent the client could not mint is a local failure.
fn followed<T>(
    r: Result<T, FollowError<NestClientError>>,
) -> Result<Result<T, NestClientError>, FfiError> {
    match r {
        Ok(v) => Ok(Ok(v)),
        Err(FollowError::Nest(e)) => Ok(Err(e)),
        Err(FollowError::Intent(why)) => Err(err(format!("follow intent: {why}"))),
    }
}

/// **The subscriber's follow or leave across nests**, as the account whose
/// seed is `secret`, through the shared
/// [`SubscriptionsClient::subscribe_across_nests`] (or its leave twin) — the
/// call every app's `profile-follow-button` and
/// `subscription-mine-unsubscribe-button` make.
///
/// `request` is the dag-cbor array `[leave, author_id, tier, home_nest_url]`
/// (a bool, 32 bytes, a text string, a text string or null — null or blank is
/// the plain local follow). `out` receives `{"ok": <the subscribe or
/// unsubscribe reply>}` or `{"err": <RpcError>}`.
///
/// # Safety
///
/// As [`super::fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_follow(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (leave, author, tier, home): (bool, fauna_protocol::ByteBuf, String, Option<String>) =
            decode_request!(call.req)?;
        let author = actor_id(&author, "author_id")?;
        let identity = call.identity.clone_keypair();
        with_client(call.nest_url, call.identity, |nest| async move {
            let subs = SubscriptionsClient::new(nest);
            if leave {
                outcome!(followed(
                    subs.unsubscribe_across_nests(author, tier, &identity, home)
                        .await
                )?)
            } else {
                outcome!(followed(
                    subs.subscribe_across_nests(author, tier, &identity, home)
                        .await
                )?)
            }
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// **The buyer's claim-code redeem**, as the account whose seed is `secret`,
/// through the shared [`PaymentsClient::claims_redeem_across_nests`] — the
/// call every app's claim-redeem button makes: the local redeem when no home
/// is named, else the redeemer's own signed `ClaimRedeemIntent` naming the
/// connected nest, which relays it to the code's home.
///
/// `request` is the dag-cbor array `[code, author_id, author_home_url]` (a
/// text string, 32 bytes, a text string or null). `out` receives `{"ok": <the
/// ClaimRedeemReply>}` or `{"err": <RpcError>}`.
///
/// # Safety
///
/// As [`super::fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_redeem(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (code, author, home): (String, fauna_protocol::ByteBuf, Option<String>) =
            decode_request!(call.req)?;
        let author = actor_id(&author, "author_id")?;
        let identity = call.identity.clone_keypair();
        with_client(call.nest_url, call.identity, |nest| async move {
            let reply = PaymentsClient::new(nest)
                .claims_redeem_across_nests(code, &identity, author, home)
                .await;
            outcome!(match reply {
                Ok(v) => Ok(v),
                Err(CrossNestError::Nest(e)) => Err(e),
                Err(CrossNestError::Intent(why)) => {
                    return Err(err(format!("redeem intent: {why}")));
                }
            })
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// **The author's client, app-less**, as the account whose seed is `secret`:
/// over a [`Seat`]'s period-key custody, either one author-pump tick
/// ([`SubscriptionsAuthor::reconcile_once`] — heal staged removals, then
/// auto-approve every queued follow of an `auto_approve` tier and commit
/// every queued leave) or the removal of one subscriber
/// ([`SubscriptionsAuthor::remove_subscriber`]), or the creation of one
/// offered tier, undesignated or selling one post
/// ([`SubscriptionsAuthor::create_tier`] — its
/// period key in this account's custody, as an app's tier editor leaves it).
/// The seat publishes once and signs out.
///
/// `request` is the dag-cbor array `["reconcile"]`, `["remove", tier,
/// subscriber_id]` or `["create", tier, rank, auto_approve, unlocks_post]`
/// (`unlocks_post` the hex post id a per-post tier sells, or null/absent). `out` receives
/// `{"ok": {"resumed": n, "approved": n, "resume_error": text|null,
/// "drain_error": text|null}}` for a tick, `{"ok": {"removed": true}}` for a
/// removal, `{"ok": {"created": bool}}` for a creation; a removal or creation
/// the author's client refused is a local failure.
///
/// # Safety
///
/// As [`super::fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_author_pass(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let req: Vec<serde_json::Value> = decode_request!(call.req)?;
        let op = req
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| err("the request names no op"))?
            .to_string();
        with_seat(
            call.nest_url,
            call.identity,
            async |nest, identity, seat| {
                let handle = seat.handle.clone();
                let author = SubscriptionsAuthor::over(
                    Arc::clone(nest),
                    identity.clone_keypair(),
                    Arc::new(PlanePeriodKeys::new(move || Some(handle.clone()))),
                );
                let value = match op.as_str() {
                    "reconcile" => {
                        let pass = author.reconcile_once().await;
                        serde_json::json!({
                            "resumed": pass.resumed,
                            "approved": pass.approved,
                            "resume_error": pass.resume_error,
                            "drain_error": pass.drain_error,
                        })
                    }
                    "remove" => {
                        let tier = req
                            .get(1)
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| err("remove names no tier"))?;
                        let subscriber = req
                            .get(2)
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| err("remove names no subscriber (hex)"))?;
                        let subscriber = hex::decode(subscriber)
                            .map_err(|e| err(format!("subscriber id hex: {e}")))?;
                        let subscriber = actor_id(&subscriber, "subscriber_id")?;
                        author
                            .remove_subscriber(tier, subscriber)
                            .await
                            .map_err(|e| err(format!("remove the subscriber: {e}")))?;
                        serde_json::json!({ "removed": true })
                    }
                    "create" => {
                        let name = req
                            .get(1)
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| err("create names no tier"))?;
                        let rank = req
                            .get(2)
                            .and_then(|v| v.as_u64())
                            .and_then(|r| u32::try_from(r).ok())
                            .ok_or_else(|| err("create names no rank"))?;
                        let auto_approve = req.get(3).and_then(|v| v.as_bool()).unwrap_or(false);
                        let unlocks_post = req.get(4).and_then(|v| v.as_str()).map(str::to_string);
                        let created = author
                            .create_tier(
                                name,
                                rank,
                                None,
                                None,
                                None,
                                auto_approve,
                                unlocks_post,
                                None,
                                false,
                            )
                            .await
                            .map_err(|e| err(format!("create the tier: {e}")))?;
                        serde_json::json!({ "created": created })
                    }
                    other => return Err(err(format!("unknown author op {other:?}"))),
                };
                let errors = seat.publish().await?;
                if !errors.is_empty() {
                    return Err(err(format!("the seat's publishing pass: {errors:?}")));
                }
                fauna_core::encoding::canonical_encode(&value)
                    .map(Outcome::Ok)
                    .map_err(encode_err)
            },
        )
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}
