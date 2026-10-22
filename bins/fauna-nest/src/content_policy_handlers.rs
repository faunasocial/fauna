//! The nest admin's published rules — `fauna.admin.content_policy.get` /
//! `.set` (`docs/goal/behavior/nest-rules.md` § The nest content policy → *Nest
//! state, written in the app*), the self-description's carriage of them
//! (*Carriage — the self-description*) and the change notice (§ Acceptance at
//! joining, and a change of rules → *A change is announced*).
//!
//! The document is the shared content-policy grammar, validated here with the
//! same module the editor pre-validates with (`fauna_core::content_policy`), for
//! publisher `nest:<this nest's domain>`; an accepted write is stamped with the
//! next sequence and `issued_at`, signed with the identity key the
//! self-description names (`NestInfoReply::nest_id`), stored, audited, and
//! announced. The nest evaluates its own rules in exactly one place — the public
//! pages it publishes (`crate::web_content::region`) — and nowhere else.

use std::sync::Arc;
use std::time::Duration;

use fauna_core::content_policy::{
    ContentPolicyDocument, MAX_CONTENT_POLICY_PAYLOAD_BYTES, PolicyStatus, Publisher,
};
use fauna_protocol::content_policy::{
    AdminContentPolicyGetReply, AdminContentPolicyGetRequest, AdminContentPolicySetReply,
    AdminContentPolicySetRequest, SignedContentPolicy,
};

use crate::db::content_policy::StoredNestContentPolicy;
use crate::routes::AppState;
use crate::rpc_errors::internal;

pub const KIND_CONTENT_POLICY_GET: &str = "fauna.admin.content_policy.get";
pub const KIND_CONTENT_POLICY_SET: &str = "fauna.admin.content_policy.set";

/// How many times a write re-stamps after losing the sequence to a concurrent
/// one before it gives up — two admins saving at the same instant is the only
/// way to lose it, so a second attempt all but always lands.
const SET_ATTEMPTS: usize = 3;

/// This module's admin-plane kinds for an admin-consented principal —
/// the nest page's published rules — each its actor handler behind
/// [`crate::principal_handlers::admin_twin`] (`authorization-server.md`
/// § Scope grammar → *The admin arms* names the set).
pub(crate) fn principal_admin_handlers()
-> Vec<(&'static str, crate::principal_handlers::PrincipalHandler)> {
    use crate::principal_handlers::admin_twin;
    let kinds: [(&'static str, crate::rpc_router::RpcHandler); 2] = [
        ("fauna.admin.content_policy.get", get_handler()),
        ("fauna.admin.content_policy.set", set_handler()),
    ];
    kinds
        .into_iter()
        .map(|(kind, actor)| (kind, admin_twin(kind, actor)))
        .collect()
}

pub fn register_content_policy_handlers(b: &mut crate::rpc_router::RpcRouterBuilder) {
    // Mirror any change in `KindRegistry::register_admin_kinds`.
    for (kind, handler) in [
        (KIND_CONTENT_POLICY_GET, get_handler()),
        (KIND_CONTENT_POLICY_SET, set_handler()),
    ] {
        b.add(
            kind,
            crate::rpc_router::RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(5),
                handler,
            },
        );
    }
}

/// The rules in force as this nest serves them: the stored record, its
/// signature re-made with the current identity key should it no longer verify
/// under it (the identity rotated since the write — `box-recovery.md`). The
/// re-signing is deterministic (Ed25519), so it changes nothing a member
/// compares but the signature, and the sequence a member keyed its
/// last-accepted rules by stays the same.
pub async fn rules_in_force(state: &AppState) -> anyhow::Result<Option<StoredNestContentPolicy>> {
    let Some(mut stored) = state.db.get_nest_content_policy().await? else {
        return Ok(None);
    };
    let key = state.nest_identity.public_key_bytes();
    if stored.policy.verify(&key).is_err() {
        let signature = fauna_core::content_policy::sign_nest_policy(
            &state.nest_identity.signing_key,
            &stored.policy.document,
            stored.policy.sequence,
            stored.policy.issued_at,
        )
        .map_err(anyhow::Error::msg)?;
        tracing::info!(
            sequence = stored.policy.sequence,
            "nest rules: the stored signature is not the current identity's; serving it re-signed"
        );
        stored.policy.signature = signature.to_vec();
    }
    Ok(Some(stored))
}

fn get_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                KIND_CONTENT_POLICY_GET,
                internal,
            )
            .await?;
            let _req: AdminContentPolicyGetRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("content_policy", e))?;
            let policy = rules_in_force(&state)
                .await
                .map_err(internal)?
                .map(|stored| stored.policy);
            crate::rpc_errors::encode_reply(&AdminContentPolicyGetReply {
                policy,
                extra: Default::default(),
            })
        })
    })
}

/// Why `document` cannot be this nest's rules, as the sentence the admin's
/// editor shows — `None` when it can.
fn defect(document: &ContentPolicyDocument, domain: &str, encoded_len: usize) -> Option<String> {
    if encoded_len > MAX_CONTENT_POLICY_PAYLOAD_BYTES {
        return Some(format!(
            "the document is {encoded_len} bytes, over the {MAX_CONTENT_POLICY_PAYLOAD_BYTES}-byte bound"
        ));
    }
    match document.status(&Publisher::Nest(domain.to_string())) {
        PolicyStatus::Applied => None,
        // A newer grammar than this nest implements would be stored and
        // signed as rules nobody on this nest's build could read — the editor
        // writes the version it shares with the nest, so this is a defence.
        PolicyStatus::InertUnimplementedVersion { version } => Some(format!(
            "the document is grammar version {version}, which this nest does not implement"
        )),
        PolicyStatus::Malformed(defect) => Some(defect.to_string()),
    }
}

fn refused(defect: String) -> Result<bytes::Bytes, fauna_protocol::RpcError> {
    crate::rpc_errors::encode_reply(&AdminContentPolicySetReply {
        policy: None,
        defect: Some(defect),
        extra: Default::default(),
    })
}

fn set_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                KIND_CONTENT_POLICY_SET,
                internal,
            )
            .await?;
            let req: AdminContentPolicySetRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("content_policy", e))?;

            // The rules' publisher is the nest's claimed domain — the one its
            // self-description names. A box that has claimed none has no name
            // for its rules to be written under, and no member to bind.
            let Some(domain) = state.handle_domain_if_set() else {
                return refused(
                    "this nest has no domain yet, and its rules are published under it".into(),
                );
            };
            let encoded =
                fauna_core::content_policy::encode_document(&req.document).map_err(internal)?;
            if let Some(defect) = defect(&req.document, &domain, encoded.len()) {
                return refused(defect);
            }

            for _ in 0..SET_ATTEMPTS {
                let current = rules_in_force(&state).await.map_err(internal)?;
                // Re-sending the rules in force is no change: it keeps their
                // signed record, so no member is told of a change that is not
                // one (the kind is replay-tolerant, `forbid_replay = false`).
                if let Some(current) = &current
                    && current.domain == domain
                    && current.policy.document == encoded
                {
                    return crate::rpc_errors::encode_reply(&AdminContentPolicySetReply {
                        policy: Some(current.policy.clone()),
                        defect: None,
                        extra: Default::default(),
                    });
                }
                let read_sequence = current.as_ref().map(|c| c.policy.sequence);
                let sequence = read_sequence.unwrap_or(0) + 1;
                let policy = SignedContentPolicy::sign(
                    &req.document,
                    sequence,
                    crate::db::now_epoch_secs(),
                    &state.nest_identity.signing_key,
                )
                .map_err(internal)?;
                let stored = StoredNestContentPolicy {
                    domain: domain.clone(),
                    policy,
                };
                if state
                    .db
                    .put_nest_content_policy(&stored, read_sequence, &actor_id)
                    .await
                    .map_err(internal)?
                {
                    announce_change(&state).await;
                    crate::web_content::service::rerender_after_publisher_policy_change(
                        &state,
                        "a change of the nest's rules",
                    )
                    .await;
                    return crate::rpc_errors::encode_reply(&AdminContentPolicySetReply {
                        policy: Some(stored.policy),
                        defect: None,
                        extra: Default::default(),
                    });
                }
            }
            Err(internal(
                "the nest's rules kept changing under this write; try again",
            ))
        })
    })
}

/// Tell every connected member that the self-description is stale
/// (`fauna.nest.info_changed` → `StaleSurfaces::nest_info`), so their device
/// re-reads it and finds the newer sequence. Approved bridges hold connections
/// too and are no members: a bridge has no rules to read. Best-effort like
/// every push — a member who misses it re-reads on reconnect.
async fn announce_change(state: &Arc<AppState>) {
    let bridges: std::collections::HashSet<[u8; 32]> =
        match state.db.list_approved_bridge_service_users().await {
            Ok(bridges) => bridges.into_iter().map(|b| b.ed25519_pubkey).collect(),
            Err(e) => {
                tracing::warn!(
                    "nest rules: cannot list the bridges to spare the change notice: {e:#}"
                );
                Default::default()
            }
        };
    for actor in state.ws.connected_actor_ids() {
        if bridges.contains(&actor) {
            continue;
        }
        state.ws.notify_push(
            &actor,
            fauna_protocol::PushEvent::NestInfoChanged(Default::default()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both kinds are registered here and in the shared kind registry.
    #[test]
    fn both_kinds_are_registered() {
        let mut b = crate::rpc_router::RpcRouter::builder();
        register_content_policy_handlers(&mut b);
        let router = b.build();
        let registry = fauna_protocol::kind::KindRegistry::full();
        for kind in [KIND_CONTENT_POLICY_GET, KIND_CONTENT_POLICY_SET] {
            assert!(router.kind_meta(kind).is_some(), "{kind}");
            assert!(registry.contains(kind), "{kind}");
        }
    }
}
