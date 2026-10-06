//! WS-RPC handlers for the NIP-46 bunker control plane —
//! `fauna.nostr.bunker.{create_invite,list,revoke,set_label}`
//! (`docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer, control
//! plane bullet). A nest-enforced roster with mint/revoke verbs —
//! deliberately not `fauna.bridges.set_settings` (not a settings blob) and
//! not the capability-grant plane (the app receives no key material).
//!
//! All four kinds are **User-class, caller-scoped**: every query keys on the
//! authenticated connection's actor (`hex::encode(actor_id)`), never a
//! request field. Caller-class enforcement
//! lives in `bridge_method_allowlist::is_permitted` (User-only arms). The
//! policy cores are `crate::nostr::bunker` (S2); these handlers are the
//! transport shell.
//!
//! A fifth kind, `fauna.nostr.bunker.bind`, is the oracle's door (TP11): the
//! ONE kind here a third-party **principal** reaches, served by
//! [`principal_bind_handler`] on a principal session and by no actor class.

use std::time::Duration;

use fauna_protocol::nostr::{
    BindBunkerClientReply, BindBunkerClientRequest, BunkerAppEntry, CreateBunkerInviteReply,
    CreateBunkerInviteRequest, ListBunkerAppsReply, ListBunkerAppsRequest, RevokeBunkerAppReply,
    RevokeBunkerAppRequest, SetBunkerAppLabelReply, SetBunkerAppLabelRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::nostr::bunker;
use crate::rpc_errors::internal;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

use crate::rpc_errors::{encode_reply, malformed};

/// No linked account / no custodial key — the same wire surface the DM
/// cluster uses for its custodial-gate failures.
fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("nostr", reason)
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// The relay URL a connect string names, composed nest-side — single source
/// of truth, the same resolution the NIP-65/10050 self-advertisement uses
/// (`nostr::relays::preferred_public_relay_url`): prefer the actor's
/// `nostr_push` peer's public relay (`account-data-plane.md` § The ratified
/// decisions — a paired head is private, so its public serving box
/// is its reachable face), falling back to this box's own domain when there
/// is no such pairing.
async fn connect_relay_url(state: &crate::routes::AppState, actor_hex: &str) -> String {
    let host = state.handle_domain();
    crate::nostr::relays::preferred_public_relay_url(state, actor_hex)
        .await
        .unwrap_or_else(|| format!("wss://{host}/nostr"))
}

// ── fauna.nostr.bunker.create_invite ───────────────────────────────

fn create_invite_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.bunker.create_invite").await?;
            let _req: CreateBunkerInviteRequest = decode(&payload).map_err(malformed)?;
            let actor_hex = hex::encode(actor_id);
            let now = crate::db::now_epoch_secs() as u64;

            let conn = state.db.conn().await;
            let invite = bunker::create_invite(&conn, &actor_hex, now)
                .map_err(|e| invalid_params(&e.to_string()))?;
            drop(conn);

            // Nudge the sync worker's bunker-drain reconciler NOW (best-effort;
            // full contract on `NostrState::bunker_wake_tx`): the fresh
            // signer's `#p` filter must be standing on the paired public box
            // before the app's first (ephemeral, never-replayed) request —
            // the worker's next 60s tick is too late for scan-the-QR-and-
            // connect. T6 proves the wake→respawn mechanism live.
            let _ = state.nostr.bunker_wake_tx.try_send(());

            let relay_url = connect_relay_url(&state, &actor_hex).await;
            let connect_string = format!(
                "bunker://{}?relay={relay_url}&secret={}",
                invite.signer_pubkey, invite.secret
            );
            encode_reply(&CreateBunkerInviteReply {
                connection_id: invite.connection_id,
                connect_string,
                signer_pubkey: invite.signer_pubkey,
                expires_at: now + bunker::INVITE_TTL_SECS,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.nostr.bunker.list ────────────────────────────────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.bunker.list").await?;
            let _req: ListBunkerAppsRequest = decode(&payload).map_err(malformed)?;
            let actor_hex = hex::encode(actor_id);

            let conn = state.db.conn().await;
            let apps = bunker::list_apps(&conn, &actor_hex).map_err(internal)?;
            drop(conn);

            encode_reply(&ListBunkerAppsReply {
                apps: apps
                    .into_iter()
                    .map(|a| BunkerAppEntry {
                        id: a.id,
                        app_pubkey: a.app_pubkey,
                        label: a.label,
                        status: a.status,
                        created_at: a.created_at,
                        last_used_at: a.last_used_at,
                        use_count: a.use_count,
                        expires_at: a.expires_at,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.nostr.bunker.revoke ──────────────────────────────────────

fn revoke_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.bunker.revoke").await?;
            let req: RevokeBunkerAppRequest = decode(&payload).map_err(malformed)?;
            let actor_hex = hex::encode(actor_id);

            let conn = state.db.conn().await;
            let revoked =
                bunker::revoke_app(&conn, &actor_hex, req.connection_id).map_err(internal)?;
            drop(conn);

            encode_reply(&RevokeBunkerAppReply {
                revoked,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.nostr.bunker.set_label ───────────────────────────────────

fn set_label_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.bunker.set_label").await?;
            let req: SetBunkerAppLabelRequest = decode(&payload).map_err(malformed)?;
            let actor_hex = hex::encode(actor_id);

            let conn = state.db.conn().await;
            let updated = bunker::set_label(&conn, &actor_hex, req.connection_id, &req.label)
                .map_err(internal)?;
            drop(conn);

            encode_reply(&SetBunkerAppLabelReply {
                updated,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.nostr.bunker.bind ────────────────────────────────────────

/// The actor-side half of `fauna.nostr.bunker.bind`: the kind's wire contract
/// lives in the actor router (one kind, one wire contract), but NO actor
/// class holds it — the matrix arm is `ThirdParty` only — so this handler
/// runs the central gate and is refused there, always. The kind is served by
/// [`principal_bind_handler`] on a principal session.
fn bind_actor_handler() -> RpcHandler {
    Box::new(|state, actor_id, _payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.bunker.bind").await?;
            // Unreachable while the matrix arm admits no actor class.
            Err(crate::rpc_errors::central_permission_denied())
        })
    })
}

/// `fauna.nostr.bunker.bind` for a third-party **principal** (TP11 — the
/// oracle's door, `ui/nostr.md` § The nest as the user's NIP-46 signer → *A
/// principal as a bunker client*). Binds the principal's NIP-46 client key
/// to the account's signer and answers the secret-less connect string.
///
/// The dispatch chokepoint already checked the ceiling and that the
/// principal's scopes name a Nostr-custodian `identity.op` class. Here: the
/// key must be well-formed, and the owner must have minted a LIVE
/// `identity.op` grant naming such a class to the key the principal attested
/// — a principal cannot park a client key before the user granted anything.
/// The binding is never the authority: every request re-resolves the grant
/// at the signer (`nostr/oracle.rs`).
pub(crate) fn principal_bind_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let denied = |why: &str| crate::rpc_errors::permission_denied_ns("nostr", why);
            let req: BindBunkerClientRequest = decode(&payload).map_err(malformed)?;
            if !crate::nostr::oracle::is_client_pubkey(&req.client_pubkey) {
                return Err(malformed("client_pubkey must be 64 lowercase hex"));
            }
            let Some(holder) = caller.holder_x25519 else {
                return Err(denied("the principal attested no key"));
            };
            let now = crate::db::now_epoch_secs();
            let grants = state
                .db
                .fetch_capability_grants_for_holder_owned_by(&caller.account, &holder, now)
                .await
                .map_err(internal)?;
            let now = now as u64;
            if !crate::nostr::oracle::any_grant_admits_this_custodian(
                grants.iter().map(Vec::as_slice),
                now,
            ) {
                return Err(denied("no live identity.op grant for this signer"));
            }

            let actor_hex = hex::encode(caller.account);
            let conn = state.db.conn().await;
            let signer_pubkey = bunker::bind_client(
                &conn,
                &actor_hex,
                &caller.principal_id,
                &req.client_pubkey,
                now,
            )
            .map_err(|e| invalid_params(&e.to_string()))?
            .map_err(|_| denied("the client key is bound by another principal"))?;
            drop(conn);

            // The signer's `#p` filter must be standing before the client's
            // first request — the `create_invite` wake, same reason.
            let _ = state.nostr.bunker_wake_tx.try_send(());

            let relay_url = connect_relay_url(&state, &actor_hex).await;
            encode_reply(&BindBunkerClientReply {
                connect_string: format!("bunker://{signer_pubkey}?relay={relay_url}"),
                signer_pubkey,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────

/// Deadlines mirror the client-side twin
/// (`fauna_protocol::kind::register_nostr_bunker_kinds`): `create_invite` is
/// provision-weight (30 s), the rest light (5 s); all replay-safe (the
/// capabilities mint/revoke precedent — a replayed invite lapses on TTL, a
/// replayed revoke/set_label is idempotent).
pub fn register_nostr_bunker_handlers(b: &mut RpcRouterBuilder) {
    let light = || Duration::from_secs(5);
    b.add(
        "fauna.nostr.bunker.create_invite",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: create_invite_handler(),
        },
    );
    b.add(
        "fauna.nostr.bunker.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: light(),
            handler: list_handler(),
        },
    );
    b.add(
        "fauna.nostr.bunker.revoke",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: light(),
            handler: revoke_handler(),
        },
    );
    b.add(
        "fauna.nostr.bunker.set_label",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: light(),
            handler: set_label_handler(),
        },
    );
    // Principal-only (TP11): light, replay-safe — a re-bind re-points the
    // principal's one client row.
    b.add(
        "fauna.nostr.bunker.bind",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: light(),
            handler: bind_actor_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fauna_bridge_nostr::signing::Keypair;
    use zeroize::Zeroizing;

    use super::*;
    use crate::db::CacheDb;
    use crate::nostr::{db, key_crypto};
    use crate::test_support::{every_row_opens_under, seat_deployment_seed, serving_generation};

    /// The deployment-seed rotation's hand-off window for the bunker signer,
    /// driven causally: the ceremony has committed while a serving generation
    /// not yet torn down still holds the retired seed (`box-recovery.md`
    /// § Deployment-seed rotation → *The bounded hand-off window*). The
    /// account's first invite mints its signer, and when that generation
    /// answers the invite the signer must still be sealed under the seed the
    /// database holds. A signer sealed under the retired copy never opens
    /// again, and the satellite walk refuses it on every later rotation.
    #[tokio::test]
    async fn a_first_invite_in_the_rotation_window_seals_the_signer_under_the_successor_seed() {
        let (a, b, c) = (
            Zeroizing::new([0xa1u8; 32]),
            Zeroizing::new([0xb2u8; 32]),
            Zeroizing::new([0xc3u8; 32]),
        );
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        crate::nostr::init_db(&db).await.expect("nostr tables");
        seat_deployment_seed(&db, &a).await;
        let actor = [0x44u8; 32];
        db.create_user(&actor, "free", "test").await.unwrap();
        // A custodial account, deposited under the seed of the day — the
        // ceremony re-keys it with everything else.
        let user_key = Keypair::generate();
        {
            let deposit = key_crypto::encrypt_nostr_privkey(&a, &user_key.secret_bytes()).unwrap();
            let conn = db.conn().await;
            db::link_account(
                &conn,
                &hex::encode(actor),
                &user_key.public_key_hex(),
                "generated",
                Some(&deposit),
                None,
                None,
            )
            .unwrap();
        }
        let outgoing = Arc::new(serving_generation(db.clone(), &a));

        db.rotate_deployment_seed(&a, &b)
            .await
            .expect("the ceremony runs")
            .expect("and commits");

        create_invite_handler()(
            outgoing,
            actor,
            fauna_protocol::encode_canonical(&CreateBunkerInviteRequest::default())
                .expect("encode"),
        )
        .await
        .expect("the outgoing generation answers the first invite");
        assert!(
            every_row_opens_under(
                &db,
                "nostr_bunker_signers",
                "encrypted_privkey",
                crate::nest_kek::BUNKER_SIGNER_CONTEXT,
                &b,
            )
            .await,
            "a first invite answered by the outgoing generation sealed the bunker signer under \
             the retired deployment seed"
        );

        db.rotate_deployment_seed(&b, &c)
            .await
            .expect("the next rotation commits — the signer does not wedge the walk")
            .expect("and is no rule refusal");
    }
}
