//! WS-RPC handlers for the user-facing spam-classifier preferences
//! surface — `fauna.spam.{get_preferences,set_preferences}`. A faithful
//! transport migration of the old `GET|PUT /api/v1/spam/preferences` routes;
//! the HTTP twins were **DELETED** in the WS-RPC-everywhere rip (the
//! `moderation_routes.rs` spam-preference handlers are gone).
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (User-only arms for these kinds).

use std::collections::BTreeMap;
use std::time::Duration;

use fauna_protocol::decode_strict as decode;
use fauna_protocol::spam::{
    SpamGetPreferencesRequest, SpamPreferences, SpamSetPreferencesRequest,
    per_mille_to_probability, probability_to_per_mille,
};

use crate::db::SpamPreferences as DbSpamPreferences;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// `db::SpamPreferences` → the wire reply shape. The probability↔per-mille
/// conversion is the shared `fauna_protocol::spam` definition (dag-cbor forbids
/// floats — `serialization.md` § Floats — so the f64 stays the internal/DB
/// representation; every app + the nest round it identically).
fn to_wire(p: DbSpamPreferences) -> SpamPreferences {
    SpamPreferences {
        spam_threshold: probability_to_per_mille(p.spam_threshold),
        phishing_threshold: probability_to_per_mille(p.phishing_threshold),
        extra: BTreeMap::new(),
    }
}

// ── fauna.spam.get_preferences ─────────────────────────────────

fn get_preferences_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.spam.get_preferences").await?;
            // Empty request — the connection actor is the subject.
            let _req: SpamGetPreferencesRequest = decode(&payload).map_err(malformed)?;
            let prefs = state
                .db
                .get_spam_preferences(&actor_id)
                .await
                .map_err(|e| {
                    tracing::error!("get_spam_preferences error: {e}");
                    internal("storage error")
                })?;
            encode_reply(&to_wire(prefs))
        })
    })
}

// ── fauna.spam.set_preferences ─────────────────────────────────

fn set_preferences_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.spam.set_preferences").await?;
            let req: SpamSetPreferencesRequest = decode(&payload).map_err(malformed)?;

            // Partial update: only Some(_) fields change (mirrors the HTTP
            // twin's per-field `payload.get(...)`). Wire thresholds are
            // per-mille [0, 1000] → clamped to probability [0.0, 1.0].
            let mut prefs = state
                .db
                .get_spam_preferences(&actor_id)
                .await
                .map_err(|e| {
                    tracing::error!("get_spam_preferences error: {e}");
                    internal("storage error")
                })?;
            if let Some(v) = req.spam_threshold {
                prefs.spam_threshold = per_mille_to_probability(v);
            }
            if let Some(v) = req.phishing_threshold {
                prefs.phishing_threshold = per_mille_to_probability(v);
            }

            state
                .db
                .upsert_spam_preferences(&actor_id, &prefs)
                .await
                .map_err(|e| {
                    tracing::error!("upsert_spam_preferences error: {e}");
                    internal("storage error")
                })?;

            // Echo the resulting state (the HTTP twin returned only
            // `{"status":"updated"}`).
            encode_reply(&to_wire(prefs))
        })
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_spam_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.spam.get_preferences",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_preferences_handler(),
        },
    );
    b.add(
        "fauna.spam.set_preferences",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_preferences_handler(),
        },
    );
}
