//! WS-RPC handlers for the Layer-3 Bridge Management user-facing
//! surface (the `fauna.bridges.*` kinds end-user clients invoke from
//! the bridges page). Distinct from `bridge_blob_handlers` /
//! `bridge_routing_handlers` / `bridge_imap_handlers` /
//! `bridge_caldav_handlers`, which are the daemon-internal
//! (MTA/MDA) plane on the same namespace.
//!
//! Caller-class enforcement is the existing `bridge_method_allowlist`
//! gate (User arms for these kinds); unknown actors fall back to
//! `CallerClass::User` per `caller_class_for_actor`.
//!
//! Slice progress + per-kind replay semantics are tracked internally as
//! part of the WS-RPC-everywhere migration.

use std::time::Duration;

use fauna_core::data::{FeedSourceOperation, FeedSources};
use fauna_protocol::{
    RpcError, Value,
    bridges_ui::{
        AddFollowReply, AddFollowRequest, BridgeSetting, BridgeStatus as WireBridgeStatus,
        CreateFeedReply, CreateFeedRequest, DeleteFeedReply, DeleteFeedRequest, FeedSubscription,
        LinkChallengeRequest, LinkRequest, ListBridgesReply, ListBridgesRequest, ListFeedsReply,
        ListFeedsRequest, ListFollowRequestsReply, ListFollowRequestsRequest, ListFollowsReply,
        ListFollowsRequest, RemoveFollowReply, RemoveFollowRequest, ResolveFollowRequestReply,
        ResolveFollowRequestRequest, SetSettingsReply, SetSettingsRequest, UnlinkReply,
        UnlinkRequest,
    },
    decode_strict as decode,
};

use crate::bridge_management::{BridgeError, BridgeStatus};
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ─────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, internal, malformed};

/// Validation error for fields whose length / emptiness the wire shape
/// can't constrain on its own (CBOR allows empty `tstr`). Mirrors the
/// HTTP twin's 400 bad-request surface for the same checks.
fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("bridges", reason)
}

/// Not-found error for the feeds CRUD surface — fires when a delete
/// targets a row that doesn't exist or belongs to another actor.
/// Reuses the existing `fauna.bridges.not_found` code (same i18n bucket
/// the BridgeProvider-trait kinds use); the typed wire shape stays
/// uniform across the bridges namespace.
fn feed_not_found(id: i64) -> RpcError {
    crate::rpc_errors::not_found_ns("bridges", format!("feed subscription {id} not found"))
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── Provider-trait error mapping ───────────────────────────────────

/// Maps a `BridgeError` (provider-trait outcome) into a typed
/// `RpcError`. `not_found`/`unavailable` collapse to
/// `fauna.bridges.not_found`, link-state and validation errors to
/// specific kinds, provider-side upstream failures to `provider_error`,
/// everything else to `fauna.protocol.internal`.
pub(crate) fn bridge_error_to_rpc(err: BridgeError) -> RpcError {
    let (code, i18n) = match err.code.as_str() {
        "not_found" | "unavailable" => ("fauna.bridges.not_found", "error.bridges.not_found"),
        "not_linked" => ("fauna.bridges.not_linked", "error.bridges.not_linked"),
        "invalid_mode" => ("fauna.bridges.invalid_mode", "error.bridges.invalid_mode"),
        "invalid_params" => (
            "fauna.bridges.invalid_params",
            "error.bridges.invalid_params",
        ),
        "already_linked" => (
            "fauna.bridges.already_linked",
            "error.bridges.already_linked",
        ),
        "identity_in_use" => (
            "fauna.bridges.identity_in_use",
            "error.bridges.identity_in_use",
        ),
        "proof_required" => (
            "fauna.bridges.proof_required",
            "error.bridges.proof_required",
        ),
        "provider_error" => (
            "fauna.bridges.provider_error",
            "error.bridges.provider_error",
        ),
        _ => ("fauna.protocol.internal", "error.protocol.internal"),
    };
    let mut e = RpcError::new(code, i18n);
    e.details = Some(Box::new(Value::String(err.error)));
    e
}

// ── fauna.bridges.list ─────────────────────────────────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.list").await?;
            let _req: ListBridgesRequest = decode(&payload).map_err(malformed)?;

            let registry = match &state.bridge.providers {
                Some(r) => r,
                None => {
                    return encode_reply(&ListBridgesReply {
                        bridges: consented_bridge_rows(&state, &actor_id).await?,
                        extra: Default::default(),
                    });
                }
            };

            let actor_hex = hex::encode(actor_id);
            let mut bridges = Vec::with_capacity(registry.all().len());
            for provider in registry.all() {
                let supports_follows = provider.supports_follows();
                let supports_follow_requests = provider.supports_follow_requests();
                let available = provider.available(&state).await;
                if !available {
                    // Still surface `link_modes` for an as-yet-unlinked actor —
                    // mirrors `available_for_link` in link_handler: a provider's
                    // deposit-creating mode(s) must stay DISCOVERABLE even when
                    // `available` is false, or the bootstrap fix there is
                    // unreachable from every app (no UI ever offers it).
                    // Submitting still runs the real `available_for_link` check,
                    // so this exposes nothing a client couldn't already attempt
                    // blind — it only makes the attempt discoverable.
                    //
                    // An already-linked-but-unavailable actor (e.g. nip07/remote
                    // on a box with zero deposits anywhere) keeps the prior
                    // degraded view here — narrower, pre-existing edge case, not
                    // touched by this fix.
                    let link_modes = match provider.status(&state, &actor_hex).await {
                        Ok(s) if !s.linked => s.link_modes,
                        _ => None,
                    };
                    bridges.push(WireBridgeStatus {
                        id: provider.id().to_string(),
                        name: provider.name().to_string(),
                        available: false,
                        linked: false,
                        identity: None,
                        mode: None,
                        settings: Vec::new(),
                        supports_follows,
                        supports_follow_requests,
                        link_modes,
                        glyph: None,
                        // The provider's own account of why, when it has one —
                        // `link_block` renders it verbatim beside the disabled
                        // control instead of the generic
                        // `bridges.no_link_method` (`bridges.md` § A bridge
                        // that cannot be linked right now, rule 2).
                        error: provider.unavailable_reason(&state).await,
                        extra: Default::default(),
                    });
                    continue;
                }
                // Test-only fault injection ahead of the real provider call —
                // see `AppState::bridge_status_override`'s doc comment for
                // why this is the only reachable path to the un-linkable-mode
                // degraded shape from a real running nest.
                #[cfg(feature = "test-hooks")]
                let override_for_bridge = state
                    .bridge_status_override
                    .lock()
                    .expect("bridge_status_override mutex poisoned")
                    .get(provider.id())
                    .cloned();
                #[cfg(feature = "test-hooks")]
                let status_result = match override_for_bridge {
                    Some(crate::bridge_management::BridgeStatusOverride::Error(msg)) => {
                        Err(BridgeError::provider_error(&msg))
                    }
                    Some(crate::bridge_management::BridgeStatusOverride::NoApplicableModes) => {
                        Ok(BridgeStatus {
                            linked: false,
                            identity: None,
                            mode: None,
                            settings: Vec::new(),
                            link_modes: None,
                        })
                    }
                    None => provider.status(&state, &actor_hex).await,
                };
                #[cfg(not(feature = "test-hooks"))]
                let status_result = provider.status(&state, &actor_hex).await;
                match status_result {
                    Ok(BridgeStatus {
                        linked,
                        identity,
                        mode,
                        mut settings,
                        link_modes,
                    }) => {
                        append_search_policy_settings(
                            &state,
                            &actor_hex,
                            provider.id(),
                            &mut settings,
                        )
                        .await;
                        bridges.push(WireBridgeStatus {
                            id: provider.id().to_string(),
                            name: provider.name().to_string(),
                            available: true,
                            linked,
                            identity,
                            mode,
                            settings,
                            supports_follows,
                            supports_follow_requests,
                            link_modes,
                            glyph: None,
                            error: None,
                            extra: Default::default(),
                        })
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "bridges_ui",
                            bridge_id = provider.id(),
                            error = %e.error,
                            "bridge status error"
                        );
                        bridges.push(WireBridgeStatus {
                            id: provider.id().to_string(),
                            name: provider.name().to_string(),
                            available: true,
                            linked: false,
                            identity: None,
                            mode: None,
                            settings: Vec::new(),
                            supports_follows,
                            supports_follow_requests,
                            link_modes: None,
                            glyph: None,
                            error: Some(e.error),
                            extra: Default::default(),
                        });
                    }
                }
            }
            bridges.extend(consented_bridge_rows(&state, &actor_id).await?);
            encode_reply(&ListBridgesReply {
                bridges,
                extra: Default::default(),
            })
        })
    })
}

/// One row per consented third-party conversation bridge on the caller's
/// roster — a principal whose document declares a `bridge` block
/// (`third-party.md` § The manifest → *The `bridge` block*): the TP2
/// settings-card seat, and the roster `fauna_feed::classify_sources` reads a
/// bridge's glyph from. `linked` is whether a grant family is still live; the
/// display name is the resolved label the consent card showed.
async fn consented_bridge_rows(
    state: &AppState,
    actor_id: &[u8; 32],
) -> Result<Vec<WireBridgeStatus>, RpcError> {
    Ok(state
        .db
        .list_third_party_principals(actor_id)
        .await
        .map_err(internal)?
        .into_iter()
        .filter_map(|p| {
            let bridge = p.declared_bridge?;
            Some(WireBridgeStatus {
                name: p.label.unwrap_or_else(|| bridge.id.clone()),
                glyph: Some(bridge.glyph),
                id: bridge.id,
                available: true,
                linked: p.live_grants > 0,
                identity: None,
                mode: None,
                settings: Vec::new(),
                supports_follows: false,
                supports_follow_requests: false,
                link_modes: None,
                error: None,
                extra: Default::default(),
            })
        })
        .collect())
}

/// Append the two uniform Search-corpus rows to a content bridge's settings.
///
/// Registry-supplied, exactly once, here — never copied into a provider's own
/// `status()`. That placement is the mechanism behind the policy's "one policy
/// for every bridge, no per-bridge special cases" rule
/// (`content-index.md` § Bridge content in the Search corpus): a new content
/// bridge inherits both controls by existing, and no provider can drift its
/// own copy. Mail and any other all-private bridge get nothing — their content
/// is never eligible for `content_fts`.
async fn append_search_policy_settings(
    state: &AppState,
    actor_hex: &str,
    bridge_id: &str,
    settings: &mut Vec<BridgeSetting>,
) {
    if !fauna_protocol::bridge_search_policy::is_content_bridge(bridge_id) {
        return;
    }
    let policy = {
        let conn = state.db.conn().await;
        crate::db::bridge_search::get_policy(&conn, actor_hex, bridge_id)
    };
    match policy {
        Ok(p) => settings.extend(
            fauna_protocol::bridge_search_policy::search_policy_settings(
                p.show_in_search,
                p.post_limit,
            ),
        ),
        Err(e) => tracing::warn!(
            target: "bridges_ui",
            bridge_id,
            error = %e,
            "reading bridge search policy failed; omitting the search rows"
        ),
    }
}

/// Split the two registry-owned search-policy keys out of a `set_settings`
/// payload: persist them, re-apply the resulting policy to the resting corpus,
/// and return the remaining object for the provider.
///
/// Re-applying immediately is what makes the two write-side arms of the policy
/// real rather than eventual: **toggle-off purges** the bridge's rows and
/// **lowering the cap prunes** to it, both at the setting write
/// (`content-index.md` § Bridge content in the Search corpus — "both structural
/// at the setting-write path, same lockstep discipline"). A user who turns the
/// toggle off sees the rows gone on their next search, not after the next
/// inbound event.
async fn take_search_policy_settings(
    state: &AppState,
    actor_hex: &str,
    bridge_id: &str,
    settings: Value,
) -> Result<Value, BridgeError> {
    use fauna_protocol::bridge_search_policy::{SEARCH_POST_LIMIT_KEY, SHOW_IN_SEARCH_KEY};

    let Value::Map(mut map) = settings else {
        return Ok(settings);
    };
    let show = map.remove(SHOW_IN_SEARCH_KEY);
    let limit = map.remove(SEARCH_POST_LIMIT_KEY);
    let rest = Value::Map(map);

    if show.is_none() && limit.is_none() {
        return Ok(rest);
    }
    if !fauna_protocol::bridge_search_policy::is_content_bridge(bridge_id) {
        return Err(BridgeError::invalid_params(
            "this bridge has no search-corpus settings",
        ));
    }

    let conn = state.db.conn().await;
    let mut policy = crate::db::bridge_search::get_policy(&conn, actor_hex, bridge_id)
        .map_err(|e| BridgeError::provider_error(&format!("read search policy: {e}")))?;
    if let Some(v) = show {
        policy.show_in_search = match v {
            Value::Bool(b) => b,
            _ => {
                return Err(BridgeError::invalid_params(
                    "show_in_search must be a boolean",
                ));
            }
        };
    }
    if let Some(v) = limit {
        let n = match v {
            Value::Integer(i) if i >= 0 => i,
            _ => {
                return Err(BridgeError::invalid_params(
                    "limit_posts_in_search must be a non-negative integer",
                ));
            }
        };
        policy.post_limit = fauna_protocol::bridge_search_policy::clamp_post_limit(
            u32::try_from(n).unwrap_or(u32::MAX),
        );
    }
    crate::db::bridge_search::set_policy(&conn, actor_hex, bridge_id, policy)
        .map_err(|e| BridgeError::provider_error(&format!("write search policy: {e}")))?;
    crate::db::bridge_search::reconcile_bridge_corpus(&conn, bridge_id)
        .map_err(|e| BridgeError::provider_error(&format!("reconcile search corpus: {e}")))?;
    Ok(rest)
}

// ── fauna.bridges.set_settings ─────────────────────────────────

fn set_settings_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.set_settings").await?;
            let req: SetSettingsRequest = decode(&payload).map_err(malformed)?;

            let registry = state.bridge.providers.as_ref().ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found("bridges not configured"))
            })?;
            let provider = registry.get(&req.bridge_id).ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found(&format!(
                    "bridge '{}' not found",
                    req.bridge_id
                )))
            })?;

            // Defense-in-depth: `available:false` only hides the bridge from
            // `list` + the UI; it doesn't block a direct mutating call. Gate
            // the create/modify ops (set_settings/link/add_follow) on
            // availability so a bridge that can't run on this nest (e.g. Nostr
            // on an encrypted nest, per its storage-mode gate) can't be driven
            // out-of-band. Cleanup ops (unlink/remove_follow) stay ungated so a
            // user can always remove a now-unavailable bridge's state
            // (user-controls-data invariant). Reads stay ungated.
            if !provider.available(&state).await {
                return Err(bridge_error_to_rpc(BridgeError::unavailable()));
            }

            let actor_hex = hex::encode(actor_id);
            // The two search-policy keys are registry-owned, so they are taken
            // out here and never reach the provider — the same placement that
            // put them into `list`'s reply. What is left is the provider's own
            // settings object, unchanged.
            let settings =
                take_search_policy_settings(&state, &actor_hex, &req.bridge_id, req.settings)
                    .await
                    .map_err(bridge_error_to_rpc)?;

            provider
                .update_settings(&state, &actor_hex, settings)
                .await
                .map_err(bridge_error_to_rpc)?;
            encode_reply(&SetSettingsReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// Family-safety feed-sources gate (family-safety.md § Guardian policy
/// pillar 1): `feed_sources = "block"` refuses NEW external sources for a
/// supervised account — bridge account links, follows, and external feed
/// subscriptions. Already-connected sources keep flowing until the guardian
/// removes them; cleanup ops (unlink / remove_follow / feeds.delete) stay
/// deliberately ungated so a locked-down account can still be cleaned up.
///
/// **The redeem path (v1.x, § Feed-source approvals).** A blocked ward can ask
/// their guardian, whose approve mints a single-use grant keyed on this exact
/// `(bridge_id, operation, target)` — never a nest-side replay of the operation,
/// which for an interactive OAuth `link` would mean running it as the ward hours
/// later. The ward redeems by simply retrying, which lands back here: a matching
/// unexpired grant is **consumed, and only then** does the caller perform the
/// operation.
///
/// Consume-first is the ratified crash-safe order. A crash after the consume
/// burns the grant and the ward re-asks (fail-closed, and the guardian is asked
/// again — the safe direction); perform-first would leave a window in which two
/// concurrent retries both see the grant and both perform. For the same reason
/// this returns `Ok(())` *having already spent* the grant: every caller below
/// performs its operation immediately after, and a caller that bails between the
/// two burns one grant rather than double-spending it.
///
/// Callers must pass the operation and target **from the decoded request**, so
/// the gate runs after `decode` — the grant matches the object exactly, and a
/// gate that ran before parsing could only ever match a wildcard.
///
/// **Placement rule — every caller gates in the same window:** after `decode`
/// and after all *deterministic, read-only* checks (shape/length validation, the
/// registry and provider lookups, the availability probe), and immediately
/// before the operation itself. Gating any earlier spends the grant on a retry
/// that was going to fail regardless — and since re-asking cannot succeed
/// either while the bridge stays unconfigured or unavailable, that is a loop the
/// ward cannot exit. Gating any *later* — after a side effect — would break
/// consume-before-perform. The window is exactly between the two, and every
/// check that precedes the gate must stay a pure read.
async fn require_feed_sources_allowed(
    state: &AppState,
    actor_id: &[u8; 32],
    operation: FeedSourceOperation,
    target: &str,
    bridge_id: &str,
) -> Result<(), RpcError> {
    let Some(policy) = state
        .db
        .get_guardian_policy(actor_id)
        .await
        .map_err(internal)?
    else {
        return Ok(()); // not supervised — no gate
    };
    // Fail-closed parse (the established knob rule): a value this binary cannot
    // name resolves to `block`, never `allow` — a newer nest may store one, and
    // fail-open there would silently void the ward's protection. Pinned by
    // `an_unnameable_feed_sources_value_fails_closed_at_the_gate`.
    if FeedSources::from_wire(&policy.feed_sources) != FeedSources::Block {
        return Ok(());
    }
    // Blocked — unless the guardian left a grant for exactly this object.
    if state
        .db
        .consume_feed_grant(actor_id, bridge_id, operation.as_str(), target)
        .await
        .map_err(internal)?
    {
        return Ok(());
    }
    Err(crate::rpc_errors::guardian_approval_required_ns(
        "bridges",
        "this account cannot add new feed sources — ask your guardian",
    ))
}

// ── fauna.bridges.link ─────────────────────────────────────────

fn link_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.link").await?;
            let req: LinkRequest = decode(&payload).map_err(malformed)?;

            let registry = state.bridge.providers.as_ref().ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found("bridges not configured"))
            })?;
            let provider = registry.get(&req.bridge_id).ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found(&format!(
                    "bridge '{}' not found",
                    req.bridge_id
                )))
            })?;

            // Gate create/modify on availability (see set_settings_handler).
            // `available_for_link` (not `available`) — a provider's
            // deposit-creating mode(s) may need to run precisely when
            // `available()` is still false (see its doc comment).
            if !provider.available_for_link(&state, &req.mode).await {
                return Err(bridge_error_to_rpc(BridgeError::unavailable()));
            }

            // Gated after the read-only registry/availability checks and
            // immediately before the link, so a retry against a not-configured
            // or unavailable bridge cannot burn the ward's single-use grant —
            // a loop the ward could not exit, since re-asking cannot succeed
            // either. Still strictly before the operation itself
            // (consume-before-perform): every check above is a pure read.
            //
            // A link ask carries an EMPTY target: approving a link approves
            // connecting that bridge, and the OAuth `mode` is mechanism, not
            // scope (family-safety.md § Feed-source approvals). Keying the grant
            // on the mode would strand the ward whose retry picks another one.
            require_feed_sources_allowed(
                &state,
                &actor_id,
                FeedSourceOperation::Link,
                "",
                &req.bridge_id,
            )
            .await?;

            let actor_hex = hex::encode(actor_id);
            let resp = provider
                .link(&state, &actor_hex, &req.mode, req.params)
                .await
                .map_err(bridge_error_to_rpc)?;
            encode_reply(&resp)
        })
    })
}

// ── fauna.bridges.link_challenge ───────────────────────────────

/// Mint the proof-of-possession challenge an external signer signs before a
/// `link` in that mode is accepted. Same permission and availability gates as
/// `link` (it is `link`'s first half); no guardian feed-source gate — nothing
/// is linked yet, and the single-use grant must survive until the link itself.
fn link_challenge_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.link_challenge").await?;
            let req: LinkChallengeRequest = decode(&payload).map_err(malformed)?;

            let registry = state.bridge.providers.as_ref().ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found("bridges not configured"))
            })?;
            let provider = registry.get(&req.bridge_id).ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found(&format!(
                    "bridge '{}' not found",
                    req.bridge_id
                )))
            })?;
            if !provider.available_for_link(&state, &req.mode).await {
                return Err(bridge_error_to_rpc(BridgeError::unavailable()));
            }

            let actor_hex = hex::encode(actor_id);
            let resp = provider
                .link_challenge(&state, &actor_hex, &req.mode)
                .await
                .map_err(bridge_error_to_rpc)?;
            encode_reply(&resp)
        })
    })
}

// ── fauna.bridges.unlink ───────────────────────────────────────

fn unlink_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.unlink").await?;
            let req: UnlinkRequest = decode(&payload).map_err(malformed)?;

            let registry = state.bridge.providers.as_ref().ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found("bridges not configured"))
            })?;
            let provider = registry.get(&req.bridge_id).ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found(&format!(
                    "bridge '{}' not found",
                    req.bridge_id
                )))
            })?;

            let actor_hex = hex::encode(actor_id);
            provider
                .unlink(&state, &actor_hex)
                .await
                .map_err(bridge_error_to_rpc)?;
            encode_reply(&UnlinkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.bridges.list_follows ─────────────────────────────────

fn list_follows_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.list_follows").await?;
            let req: ListFollowsRequest = decode(&payload).map_err(malformed)?;

            let registry = state.bridge.providers.as_ref().ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found("bridges not configured"))
            })?;
            let provider = registry.get(&req.bridge_id).ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found(&format!(
                    "bridge '{}' not found",
                    req.bridge_id
                )))
            })?;

            let actor_hex = hex::encode(actor_id);
            let follows = provider
                .list_follows(&state, &actor_hex)
                .await
                .map_err(bridge_error_to_rpc)?;
            encode_reply(&ListFollowsReply {
                follows,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.bridges.add_follow ───────────────────────────────────

fn add_follow_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.add_follow").await?;
            let req: AddFollowRequest = decode(&payload).map_err(malformed)?;

            let registry = state.bridge.providers.as_ref().ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found("bridges not configured"))
            })?;
            let provider = registry.get(&req.bridge_id).ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found(&format!(
                    "bridge '{}' not found",
                    req.bridge_id
                )))
            })?;

            // Gate create/modify on availability (see set_settings_handler).
            if !provider.available(&state).await {
                return Err(bridge_error_to_rpc(BridgeError::unavailable()));
            }

            // Gated after the read-only registry/availability checks and
            // immediately before the follow, for the reason spelled out in
            // `link_handler`: an unavailable-bridge retry must not burn the
            // ward's single-use grant. Still strictly before the operation
            // (consume-before-perform); every check above is a pure read.
            //
            // The follow id is the target — a grant for one follow must never
            // unlock a different one.
            require_feed_sources_allowed(
                &state,
                &actor_id,
                FeedSourceOperation::Follow,
                &req.id,
                &req.bridge_id,
            )
            .await?;

            let actor_hex = hex::encode(actor_id);
            provider
                .add_follow(
                    &state,
                    &actor_hex,
                    &req.id,
                    req.petname.as_deref(),
                    req.extra,
                )
                .await
                .map_err(bridge_error_to_rpc)?;
            encode_reply(&AddFollowReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.bridges.remove_follow ────────────────────────────────

fn remove_follow_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.remove_follow").await?;
            let req: RemoveFollowRequest = decode(&payload).map_err(malformed)?;

            let registry = state.bridge.providers.as_ref().ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found("bridges not configured"))
            })?;
            let provider = registry.get(&req.bridge_id).ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found(&format!(
                    "bridge '{}' not found",
                    req.bridge_id
                )))
            })?;

            let actor_hex = hex::encode(actor_id);
            provider
                .remove_follow(&state, &actor_hex, &req.follow_id)
                .await
                .map_err(bridge_error_to_rpc)?;
            encode_reply(&RemoveFollowReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.bridges.list_follow_requests ─────────────────────────

/// The follow requests waiting on the caller's own account on the named
/// bridge (`bridges.md` § Follow requests). Self-scoped exactly as
/// `list_follows` is: the provider is asked about the calling actor and no
/// other.
fn list_follow_requests_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.list_follow_requests").await?;
            let req: ListFollowRequestsRequest = decode(&payload).map_err(malformed)?;

            let registry = state.bridge.providers.as_ref().ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found("bridges not configured"))
            })?;
            let provider = registry.get(&req.bridge_id).ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found(&format!(
                    "bridge '{}' not found",
                    req.bridge_id
                )))
            })?;

            let actor_hex = hex::encode(actor_id);
            let requests = provider
                .list_follow_requests(&state, &actor_hex)
                .await
                .map_err(bridge_error_to_rpc)?;
            encode_reply(&ListFollowRequestsReply {
                requests,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.bridges.resolve_follow_request ───────────────────────

/// Approve or refuse one of the caller's own waiting follow requests.
///
/// No guardian `feed_sources` gate, deliberately: that gate governs what an
/// account reads; a follower is audience, and approving one grants nothing
/// the default *accept by itself* position does not already grant
/// (`bridges.md` § Follow requests).
fn resolve_follow_request_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.resolve_follow_request").await?;
            let req: ResolveFollowRequestRequest = decode(&payload).map_err(malformed)?;

            let registry = state.bridge.providers.as_ref().ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found("bridges not configured"))
            })?;
            let provider = registry.get(&req.bridge_id).ok_or_else(|| {
                bridge_error_to_rpc(BridgeError::not_found(&format!(
                    "bridge '{}' not found",
                    req.bridge_id
                )))
            })?;

            let actor_hex = hex::encode(actor_id);
            provider
                .resolve_follow_request(&state, &actor_hex, &req.id, req.approve)
                .await
                .map_err(bridge_error_to_rpc)?;
            encode_reply(&ResolveFollowRequestReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.bridges.feeds.list ───────────────────────────────────

fn feeds_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.feeds.list").await?;
            let _req: ListFeedsRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .list_bridge_feeds(&actor_id)
                .await
                .map_err(internal)?;
            let subscriptions = rows
                .into_iter()
                .map(|s| FeedSubscription {
                    id: s.id,
                    bridge: s.bridge,
                    feed_uri: s.feed_uri,
                    name: s.name,
                    created_at: s.created_at,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&ListFeedsReply {
                subscriptions,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.bridges.feeds.create ─────────────────────────────────

/// Length caps mirror the HTTP twin's `subscribe_bridge_feed`
/// validation. `tstr` lets CBOR carry any length, so the handler
/// enforces them itself.
// `pub(crate)` so the family-safety feed-source ask (`family_handlers.rs`) caps
// its `bridge_id` / `target` / `label` at exactly what the operations below
// accept. Shared rather than re-declared because a *tighter* cap on the ask
// would make some legal operations unaskable — the ward would face a refusal
// with no way to request it.
pub(crate) const MAX_BRIDGE_LEN: usize = 64;
pub(crate) const MAX_FEED_URI_LEN: usize = 2048;
pub(crate) const MAX_NAME_LEN: usize = 128;

fn feeds_create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.feeds.create").await?;
            let req: CreateFeedRequest = decode(&payload).map_err(malformed)?;

            if req.bridge.is_empty() || req.feed_uri.is_empty() || req.name.is_empty() {
                return Err(invalid_params("bridge, feed_uri, and name are required"));
            }
            if req.bridge.len() > MAX_BRIDGE_LEN {
                return Err(invalid_params("bridge name too long"));
            }
            if req.feed_uri.len() > MAX_FEED_URI_LEN {
                return Err(invalid_params("feed_uri too long"));
            }
            if req.name.len() > MAX_NAME_LEN {
                return Err(invalid_params("name too long"));
            }

            // Gated after the shape checks and immediately before the write, so
            // a malformed retry cannot burn the ward's single-use grant — but
            // still strictly before the operation itself (consume-before-perform).
            // The feed URI is the target: a grant for one feed never unlocks
            // another. `name` is the ask's label — display-only, never a key.
            require_feed_sources_allowed(
                &state,
                &actor_id,
                FeedSourceOperation::Feed,
                &req.feed_uri,
                &req.bridge,
            )
            .await?;

            let id = state
                .db
                .subscribe_bridge_feed(&actor_id, &req.bridge, &req.feed_uri, &req.name)
                .await
                .map_err(internal)?;
            encode_reply(&CreateFeedReply {
                id,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.bridges.feeds.delete ─────────────────────────────────

fn feeds_delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.bridges.feeds.delete").await?;
            let req: DeleteFeedRequest = decode(&payload).map_err(malformed)?;

            let deleted = state
                .db
                .unsubscribe_bridge_feed(req.id, &actor_id)
                .await
                .map_err(internal)?;
            if !deleted {
                return Err(feed_not_found(req.id));
            }
            encode_reply(&DeleteFeedReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_bridges_ui_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.bridges.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_handler(),
        },
    );
    b.add(
        "fauna.bridges.set_settings",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_settings_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_follows",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_follows_handler(),
        },
    );
    // T3 — OAuth-flow start; forbid_replay=true so the auto-retry path
    // can't double-invoke the upstream nonce / pending-state machinery.
    // 30 s default deadline absorbs the upstream round-trip envelope.
    b.add(
        "fauna.bridges.link",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: link_handler(),
        },
    );
    // Proof-of-possession challenge ahead of an external-signer link: a local
    // nonce mint, replay-safe (a re-issue supersedes), the 5 s read envelope.
    b.add(
        "fauna.bridges.link_challenge",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: link_challenge_handler(),
        },
    );
    b.add(
        "fauna.bridges.unlink",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: unlink_handler(),
        },
    );
    // T4 — duplicate-follow is a server-enforced constraint; replay
    // would surface as a spurious conflict, so forbid the auto-retry
    // path. 5 s deadline matches the existing follow-list read.
    b.add(
        "fauna.bridges.add_follow",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: add_follow_handler(),
        },
    );
    b.add(
        "fauna.bridges.remove_follow",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: remove_follow_handler(),
        },
    );
    // Follow requests — a pure read and an idempotent answer, both
    // replay-safe at 5 s (see register_bridges_ui_kinds).
    b.add(
        "fauna.bridges.list_follow_requests",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_follow_requests_handler(),
        },
    );
    b.add(
        "fauna.bridges.resolve_follow_request",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: resolve_follow_request_handler(),
        },
    );
    // T5 — cross-bridge feed-subscription CRUD. All replay-safe at 5 s
    // (see register_bridges_ui_kinds for the per-kind rationale).
    b.add(
        "fauna.bridges.feeds.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feeds_list_handler(),
        },
    );
    b.add(
        "fauna.bridges.feeds.create",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feeds_create_handler(),
        },
    );
    b.add(
        "fauna.bridges.feeds.delete",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feeds_delete_handler(),
        },
    );
}

// The `json_to_cbor` / `cbor_to_json` helpers and their unit tests
// retired with the HTTP twins (T9+T10). The trait now carries `Value`
// directly on the dynamic surfaces, so there's no JSON↔CBOR boundary
// inside the handler. End-to-end coverage lives in
// `tests/conformance_bridges_ui.rs` (router dispatch + typed reply
// shape).
