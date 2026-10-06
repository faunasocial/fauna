//! WS-RPC handlers for the nest-held OAuth issuer key set (`fauna.oauth.*`),
//! per `docs/goal/behavior/authorization-server.md` § The issuer.
//!
//! The admin half of TP5 slice S1. The key plane
//! ([`crate::oauth_issuer_key`]) and its public readers
//! ([`crate::oauth_issuer_routes`]) already exist; these kinds are what let
//! an admin *see* the key set and *rotate* it, which § The issuer places
//! "beside `rotate_srs_secret` and `force_rotate_dkim` in the admin shell"
//! rather than on a bridge's approved card — that placement was forced only by
//! the key being reachable through a bridge, and it no longer is.
//!
//! Rotation is never scheduled (§ As built: ES256 keys do not wear out, and
//! every rotation risks JWKS-cache skew at exactly the clients least worth
//! breaking), and it has **two arms** (§ The issuer → *Two rotation arms*):
//! the ordinary one *adds* a key rather than replacing one, so tokens minted
//! seconds earlier keep verifying while the outgoing key is served until the
//! retirement horizon
//! ([`fauna_provisioning::oauth_issuer::issuer_key_retirement_horizon_secs`])
//! elapses; the **forced** one is the compromise response proper — it drops
//! every other key at once, because a forged token's acceptance is bounded by
//! its `kid` leaving the JWKS and by nothing else.
//!
//! Ungated, like everything else on this path: the issuer is up whenever the
//! nest is up, so these kinds answer on a nest that compiles no bridge at all.

use std::time::Duration;

use fauna_protocol::{
    decode_strict as decode,
    oauth_issuer::{
        ForceRotateIssuerKeyReply, ForceRotateIssuerKeyRequest, ForceRotateSessionSecretReply,
        ForceRotateSessionSecretRequest, IssuerKeyEntry, IssuerKeyStatusReply,
        IssuerKeyStatusRequest, RotateIssuerKeyReply, RotateIssuerKeyRequest,
    },
};
use fauna_provisioning::oauth_issuer::issuer_key_retirement_horizon_secs;

use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// Every `fauna.oauth.*` kind is Admin-only per
// `bridge_method_allowlist::is_permitted`.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// `fauna.oauth.issuer_key_status` (Admin) — which keys the issuer serves.
///
/// Reads through [`crate::oauth_issuer_key::serve_key_set`], the same door
/// `/oauth/jwks` uses, so the admin sees exactly the set a client would fetch.
/// That door is a lookup, never a mint — the active signer is seated at boot —
/// so a nest with no key seated reports the named error rather than a status
/// read quietly minting one, and a status read answered inside a
/// deployment-seed rotation's hand-off window writes nothing.
///
/// Every door here reads its seed from the database, never from the serving
/// generation: the status read needs none, and the three rotation doors read it
/// inside their own transaction (`oauth_issuer_key::rotate` says why). A nest
/// holding no deployment keypair therefore answers an internal error from the
/// door itself — not an empty reply, which would read as "rotation wiped my
/// keys".
fn issuer_key_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.oauth.issuer_key_status").await?;
            // Empty request. App-callable (Admin-class), so it is client↔nest
            // wire and TOLERATES a newer app's added field (transport.md rule 4)
            // rather than refusing the call.
            let _req: IssuerKeyStatusRequest = decode(&payload).map_err(malformed)?;

            let db = state.db.clone();
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            let keys = tokio::task::spawn_blocking(move || {
                crate::oauth_issuer_key::serve_key_set(&db.conn_blocking(), now)
            })
            .await
            .map_err(|e| internal(format!("issuer key set task: {e}")))?
            .map_err(|e| internal(format!("read issuer key set: {e}")))?;

            // `serve_key_set` orders active-first, so the signer is the first
            // entry with no `retired_at` — found rather than assumed, because a
            // reply naming the wrong key as active is worse than one naming
            // none.
            let active_kid = keys
                .iter()
                .find(|k| k.retired_at.is_none())
                .map(|k| k.kid.clone())
                .unwrap_or_default();

            encode_reply(&IssuerKeyStatusReply {
                active_kid,
                keys: keys
                    .into_iter()
                    .map(|k| IssuerKeyEntry {
                        kid: k.kid,
                        retired_at: k.retired_at,
                        extra: Default::default(),
                    })
                    .collect(),
                retirement_horizon_secs: issuer_key_retirement_horizon_secs(),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.oauth.rotate_issuer_key` (Admin) — mint a new signer, retire the
/// outgoing key.
///
/// Replay is forbidden at the router for `rotate_srs_secret`'s reason: each
/// call mints a key, so a replayed frame must not silently rotate twice. The
/// nest generates the key itself and the admin never supplies or reads its
/// private half.
fn rotate_issuer_key_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.oauth.rotate_issuer_key").await?;
            let _req: RotateIssuerKeyRequest = decode(&payload).map_err(malformed)?;

            let db = state.db.clone();
            let kid = tokio::task::spawn_blocking(move || {
                let mut conn = db.conn_blocking();
                crate::oauth_issuer_key::rotate(&mut conn)
            })
            .await
            .map_err(|e| internal(format!("issuer key rotation task: {e}")))?
            .map_err(|e| internal(format!("rotate issuer key: {e}")))?;

            // Nudge every approved PDS bridge to re-read the served set. Under
            // this arm the outgoing key stays served for its horizon, so a late
            // pickup costs nothing — the push is here so both arms take the same
            // path and the forced one below cannot be the only nudging caller.
            crate::bridge_atproto_handlers::notify_atproto_issuer_key_rotated(&state).await;

            encode_reply(&RotateIssuerKeyReply {
                kid,
                rotated_at: fauna_core::data::Timestamp::now_secs_or_zero(),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.oauth.force_rotate_issuer_key` (Admin) — mint a new signer and
/// **drop** every other key, horizon skipped: the compromise response.
///
/// Replay forbidden for the ordinary arm's reason and one more: a replayed
/// frame would drop the very key the first call minted, orphaning every token
/// it signed in between. The reply names every `kid` removed so the admin sees
/// exactly which keys stopped verifying rather than inferring it from a status
/// read that no longer lists them.
fn force_rotate_issuer_key_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.oauth.force_rotate_issuer_key").await?;
            let _req: ForceRotateIssuerKeyRequest = decode(&payload).map_err(malformed)?;

            let db = state.db.clone();
            let forced = tokio::task::spawn_blocking(move || {
                let mut conn = db.conn_blocking();
                crate::oauth_issuer_key::force_rotate(&mut conn)
            })
            .await
            .map_err(|e| internal(format!("forced issuer key rotation task: {e}")))?
            .map_err(|e| internal(format!("force-rotate issuer key: {e}")))?;

            // The nudge is part of the compromise response, not a convenience:
            // the dropped `kid`s have left the nest's served set, but a resource
            // server keeps honouring them until it re-reads, and this is what
            // makes that seconds rather than its ticker
            // (`authorization-server.md` § The issuer → *Two rotation arms*,
            // "what the forced arm does not bound"). Best-effort still — the
            // bridge's unknown-`kid` refetch and reconnect are the backstop —
            // so a push that cannot be delivered must not fail the rotation the
            // admin asked for.
            crate::bridge_atproto_handlers::notify_atproto_issuer_key_rotated(&state).await;

            encode_reply(&ForceRotateIssuerKeyReply {
                kid: forced.kid,
                rotated_at: fauna_core::data::Timestamp::now_secs_or_zero(),
                dropped_kids: forced.dropped_kids,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.oauth.force_rotate_session_secret` (Admin) — re-mint the issuer's
/// **second signer**, the HS256 secret every OAuth refresh token is MACed
/// under, so every outstanding refresh token dies at once: the forced arm's
/// sibling, and the half of the compromise response the issuer-key arm cannot
/// reach ([`crate::oauth_session_secret::force_rotate`] has the argument).
///
/// Replay forbidden for the forced issuer arm's reason: a replayed frame would
/// re-mint again and kill the generation the first call just minted — harmless
/// to security, but the admin would read one rotation where two happened. The
/// reply dates the generation replaced so a rotation that happened is
/// distinguishable from a reply that merely arrived.
fn force_rotate_session_secret_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.oauth.force_rotate_session_secret")
                .await?;
            let _req: ForceRotateSessionSecretRequest = decode(&payload).map_err(malformed)?;

            let db = state.db.clone();
            let forced = tokio::task::spawn_blocking(move || {
                let mut conn = db.conn_blocking();
                crate::oauth_session_secret::force_rotate(&mut conn)
            })
            .await
            .map_err(|e| internal(format!("forced session secret rotation task: {e}")))?
            .map_err(|e| internal(format!("force-rotate session secret: {e}")))?;

            // The re-mint above killed every refresh family the nest-hosted AS
            // MACed under the old secret. END their grant rows too, or the
            // response leaves the user's connected-apps surface listing N dead
            // connections with real scopes and a `last_used_at` reporting use of
            // a credential that cannot be used — exactly the state § The issuer
            // → *Grants recorded while the flow is unhonoured are ended at the
            // re-point* rejects, reached through this second door.
            //
            // ROTATE FIRST, END SECOND, and the order is the design: a refresh
            // presented in the window between the two steps must die on the MAC
            // rather than race the ending. Ending first would leave a live
            // secret behind an ended row for that instant, which is the one
            // ordering that can hand out what this call exists to withhold.
            //
            // Only nest-marked grants, and that is the whole reason the mark
            // exists: a grant this secret never minted (the retired bridge
            // AS's, MACed under a secret of its own — § The issuer → *Two
            // HS256 secrets, not one*) is not this rotation's to end.
            //
            // The ending itself lives with the revocation owner
            // (`bridge_atproto_handlers::end_nest_issued_oauth_sessions`) rather
            // than as a loop written here: a second spelling of revocation in
            // this plane is exactly what its one-owner discipline exists to
            // prevent, and the owner also collapses the connected-apps nudge to
            // one per affected ACTOR instead of one per grant.
            //
            // A failure is NOT swallowed: the secret is already gone, so the
            // caller must not read a clean success over a half-finished
            // response. Re-running the rotation finishes it — both halves are
            // idempotent, and the second run's count reports only what it ended
            // itself.
            let grants_ended =
                crate::bridge_atproto_handlers::end_nest_issued_oauth_sessions(&state)
                    .await
                    .map_err(|e| {
                        internal(format!(
                            "the OAuth session secret was replaced but the grants it killed \
                             could not all be ended, so the connected-apps surface still lists \
                             some of them as live; re-run the rotation to finish it: {e}"
                        ))
                    })?;

            encode_reply(&ForceRotateSessionSecretReply {
                rotated_at: forced.rotated_at,
                replaced_minted_at: forced.replaced_minted_at,
                // The sweep either committed whole or returned the error
                // above, so a count reached here is one this nest stands behind.
                grants_ended,
                extra: Default::default(),
            })
        })
    })
}

/// Register the `fauna.oauth.*` kinds with the WS-RPC router.
pub fn register_oauth_issuer_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.oauth.issuer_key_status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: issuer_key_status_handler(),
        },
    );
    b.add(
        "fauna.oauth.rotate_issuer_key",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: rotate_issuer_key_handler(),
        },
    );
    b.add(
        "fauna.oauth.force_rotate_issuer_key",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: force_rotate_issuer_key_handler(),
        },
    );
    b.add(
        "fauna.oauth.force_rotate_session_secret",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: force_rotate_session_secret_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST;
    use crate::routes::AppState;
    use std::sync::Arc;

    /// An Admin-class caller over an in-memory nest that holds a deployment
    /// signing key — the two things `force_rotate_session_secret` needs before
    /// it can do anything at all.
    async fn admin_state() -> (Arc<AppState>, [u8; 32]) {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        let admin = [1u8; 32];
        db.create_user_with_handle(&admin, "free", "root", None)
            .await
            .expect("the admin has an account");
        db.add_admin_actor(&admin[..]).await.expect("and the role");
        assert!(db.is_admin(&admin[..]).await.unwrap(), "precondition");
        let state = Arc::new(AppState {
            // The secret rests sealed under this, so the door refuses outright
            // without it.
            nest_signing_key: Some(ed25519_dalek::SigningKey::from_bytes(&[9u8; 32])),
            ..AppState::for_test(db)
        });
        (state, admin)
    }

    async fn record_grant(state: &Arc<AppState>, actor: &[u8; 32], id: &[u8]) {
        state
            .db
            .record_atproto_oauth_grant(
                actor,
                id,
                "https://app.example/client",
                Some("An App"),
                "atproto",
                &[],
                "jkt",
                crate::db::now_epoch_millis() + 1_000_000,
                None,
                OAUTH_GRANT_ISSUER_NEST,
                &crate::db::third_party_principals::UNATTESTED_DEVICE,
            )
            .await
            .expect("record the grant");
    }

    /// Live as the USER's own connected-apps surface sees it — the surface the
    /// ending exists to keep honest, rather than an internal enumeration that
    /// could agree with the sweep while both were wrong about what a user sees.
    async fn grant_is_live(state: &Arc<AppState>, actor: &[u8; 32], id: &[u8]) -> bool {
        state
            .db
            .list_atproto_oauth_grants(actor)
            .await
            .expect("read")
            .iter()
            .any(|row| row.grant_id == id)
    }

    /// **The other half of the compromise response.** The re-mint kills every
    /// nest-minted refresh token; this pins that the same act also ENDS their
    /// grant rows and reports how many, because a rotation that stopped at the
    /// secret leaves the user's connected-apps surface listing N dead
    /// connections with real scopes for up to 180 days — the state § The issuer
    /// → *Grants recorded while the flow is unhonoured are ended at the
    /// re-point* rejects, reached through this second door.
    ///
    /// Two accounts, because the ending is cross-actor: every other revocation
    /// on this plane is per-actor, and that shape cannot express "every grant
    /// this secret minted for anyone".
    #[tokio::test]
    async fn a_forced_rotation_ends_the_grants_it_killed_and_counts_them() {
        let (state, admin) = admin_state().await;
        let alice = [2u8; 32];
        let bob = [3u8; 32];

        record_grant(&state, &alice, b"nest-alice").await;
        record_grant(&state, &bob, b"nest-bob").await;

        let reply_bytes = force_rotate_session_secret_handler()(
            state.clone(),
            admin,
            fauna_protocol::encode_canonical(&ForceRotateSessionSecretRequest::default())
                .expect("encode the request"),
        )
        .await
        .expect("the admin's forced rotation succeeds");
        let reply: ForceRotateSessionSecretReply = decode(&reply_bytes).expect("decode the reply");

        assert_eq!(
            reply.grants_ended, 2,
            "both accounts' nest-minted grants were ended, and the reply says so \
             — the admin's response has no audit shape without the count"
        );
        assert!(
            !grant_is_live(&state, &alice, b"nest-alice").await
                && !grant_is_live(&state, &bob, b"nest-bob").await,
            "neither nest-minted grant is live any more: the surface must stop \
             listing a connection whose refresh family this call just killed"
        );
        assert!(reply.rotated_at > 0, "the reply still dates the rotation");
    }

    /// A second rotation ends nothing and says `0`: `grants_ended` counts what
    /// THIS response did, not how many rows carry the mark. An admin reading `4`
    /// twice would believe eight connections died.
    #[tokio::test]
    async fn a_second_forced_rotation_counts_nothing_left_to_end() {
        let (state, admin) = admin_state().await;
        record_grant(&state, &[2u8; 32], b"nest-one").await;

        let call = async || {
            let bytes = force_rotate_session_secret_handler()(
                state.clone(),
                admin,
                fauna_protocol::encode_canonical(&ForceRotateSessionSecretRequest::default())
                    .expect("encode"),
            )
            .await
            .expect("rotate");
            decode::<ForceRotateSessionSecretReply>(&bytes).expect("decode")
        };

        assert_eq!(call().await.grants_ended, 1);
        assert_eq!(
            call().await.grants_ended,
            0,
            "the second response ended nothing, and reports nothing"
        );
    }

    /// The deployment-seed rotation's hand-off window, for the three doors that
    /// mint on purpose and for the status read — driven causally, the way
    /// `room_read_key::tests::rotation_window` drives it: the ceremony has
    /// committed (the database holds the successor seed) while a serving
    /// generation not yet torn down still holds the retired seed in memory
    /// (`box-recovery.md` § Deployment-seed rotation → *The bounded hand-off
    /// window*).
    ///
    /// A rotation door that generation answers writes a row. It must seal that
    /// row under the seed the database holds, whether or not a row already
    /// existed: a row sealed under the retired seed never opens again, and the
    /// satellite walk refuses it on every later rotation, so no further
    /// rotation could commit.
    mod rotation_window {
        use super::*;
        use zeroize::Zeroizing;

        const ADMIN: [u8; 32] = [1u8; 32];

        fn seed(byte: u8) -> Zeroizing<[u8; 32]> {
            Zeroizing::new([byte; 32])
        }

        /// A nest whose `nest_keypair` holds `seed`, with an admin to call the
        /// doors.
        async fn nest_with_deployment_seed(seed: &[u8; 32]) -> Arc<CacheDb> {
            let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
            let public = ed25519_dalek::SigningKey::from_bytes(seed)
                .verifying_key()
                .to_bytes();
            db.set_nest_keypair(seed, &public)
                .await
                .expect("seat the deployment keypair");
            db.create_user_with_handle(&ADMIN, "free", "root", None)
                .await
                .expect("the admin has an account");
            db.add_admin_actor(&ADMIN[..]).await.expect("and the role");
            db
        }

        /// A serving generation built from `seed` — what the teardown replaces.
        fn generation(db: &Arc<CacheDb>, seed: &[u8; 32]) -> Arc<AppState> {
            Arc::new(AppState {
                nest_signing_key: Some(ed25519_dalek::SigningKey::from_bytes(seed)),
                ..AppState::for_test(db.clone())
            })
        }

        async fn rows(db: &Arc<CacheDb>, table: &'static str) -> i64 {
            let db = db.clone();
            tokio::task::spawn_blocking(move || {
                db.conn_blocking()
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                    .expect("count")
            })
            .await
            .expect("count task")
        }

        /// Every ciphertext `table` holds opens under `seed` — the issuer's
        /// retired keys included, because the walk re-keys every row.
        async fn every_row_opens_under(
            db: &Arc<CacheDb>,
            table: &'static str,
            context: &'static str,
            seed: &[u8; 32],
        ) -> bool {
            let (db, seed) = (db.clone(), *seed);
            tokio::task::spawn_blocking(move || {
                let conn = db.conn_blocking();
                let mut stmt = conn
                    .prepare(&format!("SELECT secret_wrapped FROM {table}"))
                    .expect("prepare");
                let wrapped = stmt
                    .query_map([], |r| r.get::<_, Vec<u8>>(0))
                    .expect("query")
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .expect("rows");
                !wrapped.is_empty()
                    && wrapped
                        .iter()
                        .all(|w| crate::nest_kek::unwrap_32(context, &seed, w).is_ok())
            })
            .await
            .expect("open task")
        }

        /// Drive one minting door through the window: seat a row first when
        /// `row_exists`, run the ceremony, answer the door from the outgoing
        /// generation, then show every row opens under the successor seed and
        /// the next rotation commits.
        async fn the_door_seals_under_the_successor_seed(
            door: fn() -> RpcHandler,
            request: bytes::Bytes,
            table: &'static str,
            context: &'static str,
            row_exists: bool,
        ) {
            let (a, b, c) = (seed(0xa1), seed(0xb2), seed(0xc3));
            let db = nest_with_deployment_seed(&a).await;
            let outgoing = generation(&db, &a);
            if row_exists {
                door()(outgoing.clone(), ADMIN, request.clone())
                    .await
                    .expect("the door answers before the ceremony");
                assert!(
                    every_row_opens_under(&db, table, context, &a).await,
                    "precondition: {table} rests under the seed of the day"
                );
            }

            db.rotate_deployment_seed(&a, &b)
                .await
                .expect("the ceremony runs")
                .expect("and commits");

            door()(outgoing, ADMIN, request)
                .await
                .expect("the door answers inside the window");
            assert!(
                every_row_opens_under(&db, table, context, &b).await,
                "{table} (a row existed: {row_exists}): a door answered by the outgoing \
                 generation sealed under the retired deployment seed"
            );

            db.rotate_deployment_seed(&b, &c)
                .await
                .expect("the next rotation commits — nothing the door wrote wedges the walk")
                .expect("and is no rule refusal");
        }

        #[tokio::test]
        async fn the_ordinary_issuer_rotation_seals_under_the_successor_seed() {
            for row_exists in [false, true] {
                the_door_seals_under_the_successor_seed(
                    rotate_issuer_key_handler,
                    fauna_protocol::encode_canonical(&RotateIssuerKeyRequest::default())
                        .expect("encode"),
                    "oauth_issuer_keys",
                    crate::nest_kek::OAUTH_ISSUER_CONTEXT,
                    row_exists,
                )
                .await;
            }
        }

        #[tokio::test]
        async fn the_forced_issuer_rotation_seals_under_the_successor_seed() {
            for row_exists in [false, true] {
                the_door_seals_under_the_successor_seed(
                    force_rotate_issuer_key_handler,
                    fauna_protocol::encode_canonical(&ForceRotateIssuerKeyRequest::default())
                        .expect("encode"),
                    "oauth_issuer_keys",
                    crate::nest_kek::OAUTH_ISSUER_CONTEXT,
                    row_exists,
                )
                .await;
            }
        }

        #[tokio::test]
        async fn the_forced_session_secret_rotation_seals_under_the_successor_seed() {
            for row_exists in [false, true] {
                the_door_seals_under_the_successor_seed(
                    force_rotate_session_secret_handler,
                    fauna_protocol::encode_canonical(&ForceRotateSessionSecretRequest::default())
                        .expect("encode"),
                    "oauth_session_secret",
                    crate::nest_kek::OAUTH_SESSION_CONTEXT,
                    row_exists,
                )
                .await;
            }
        }

        /// The status read answered by the outgoing generation, on a nest with
        /// no issuer key seated, mints nothing — so the next rotation commits.
        #[tokio::test]
        async fn a_status_read_in_the_window_mints_nothing() {
            let (a, b, c) = (seed(0xa1), seed(0xb2), seed(0xc3));
            let db = nest_with_deployment_seed(&a).await;
            let outgoing = generation(&db, &a);

            db.rotate_deployment_seed(&a, &b)
                .await
                .expect("the ceremony runs")
                .expect("and commits");

            let status = issuer_key_status_handler()(
                outgoing,
                ADMIN,
                fauna_protocol::encode_canonical(&IssuerKeyStatusRequest::default())
                    .expect("encode"),
            )
            .await;
            assert_eq!(
                rows(&db, "oauth_issuer_keys").await,
                0,
                "a status read inside the rotation window minted the issuer key under the \
                 retired deployment seed"
            );
            assert!(
                status.is_err(),
                "no key is seated, and a read says so rather than filling the gap"
            );

            db.rotate_deployment_seed(&b, &c)
                .await
                .expect("the next rotation commits")
                .expect("and is no rule refusal");
        }
    }
}
