//! The oracle's posting custodian: a third-party **principal** authors a
//! public post as the account, or retracts one it authored, signed with the
//! account's delegated authoring sub-key `K` (TP11).
//!
//! Owner: `docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle* (the `post.author` row); the key
//! and its delegation: `docs/goal/behavior/atproto-pds-full.md` § D10. The
//! vocabulary is `fauna_core::identity_op`, the wire
//! `fauna_protocol::post_author`, the authorization path shared with the
//! profile class (`authoring_oracle`). Both kinds run, in this order:
//!
//! 1. the request's own shape — a create carries no member this build does
//!    not name (an unknown member may be a restriction the custodian cannot
//!    honour) and references only public posts, by reply, quote or repost;
//!    a delete names a post this principal's record shows it authored;
//! 2. the owner's LIVE `identity.op:post.author` grant to the principal's
//!    attested key — re-read per request, so a revoke refuses the very next
//!    call;
//! 3. the delegation present, live by the wall clock and carrying
//!    `Capability::Post` (D10's authoring-time expiry point) — revoking it
//!    destroys `K`, and the next call is refused;
//! 4. the rate ceiling (`fauna_core::identity_op::RateWindow`, on the
//!    principal's `post_author_principals` row);
//! 5. sign with `K`, the identity-signed cert embedded, and run the SAME
//!    path the own-write door runs — `ingest_post_core` for a create,
//!    `decode_tombstone` then `delete_post_core` for a delete — then record
//!    one `post_author_ops` row.
//!
//! The account's external-apps switch is the session gate
//! (`principal_handlers::resolve_principal`, per call). The principal's
//! session, never the account's, is what reaches here: the kinds' matrix arm
//! is `ThirdParty` only. Every policy refusal answers the same
//! `fauna.posts.permission_denied` — never which check failed; the log names
//! it — and the ceiling answers `rate_limited`.
//!
//! **The per-operation record is the authorship ledger.** A delete is served
//! only for a post a `create` row of THIS principal names; the record keeps
//! the newest [`OPS_KEPT_PER_PRINCIPAL`] rows per principal, so a post whose
//! create row has rolled out is the owner's to delete from their own app.

use std::sync::Arc;

use fauna_core::data::{Capability, Post, PostId, Reference, Timestamp, Tombstone};
use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
use fauna_core::identity::ActorId;
use fauna_core::identity_op::IdentityOpClass;
use fauna_protocol::RpcError;
use fauna_protocol::post_author::{
    KIND_CREATE, KIND_DELETE, PostAuthorCreateReply, PostAuthorCreateRequest,
    PostAuthorDeleteReply, PostAuthorDeleteRequest,
};
use rusqlite::{Connection, OptionalExtension, params};

pub use crate::authoring_oracle::OPS_KEPT_PER_PRINCIPAL;
use crate::authoring_oracle::{AuthoringClass, Refusal, authorize_and_load, count_use, now_secs};
use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// The class this custodian performs, and where its state lives.
const CLASS: AuthoringClass = AuthoringClass {
    class: IdentityOpClass::PostAuthor,
    capability: Capability::Post,
    principals_table: "post_author_principals",
    ns: "posts",
};

/// The record's `op` column for a create.
const OP_CREATE: &str = "create";
/// The record's `op` column for a delete.
const OP_DELETE: &str = "delete";

/// Step 1 for a create, the pure half: no member this build does not name,
/// and only the reference arms this class carries. Returns the reply,
/// quote and repost targets the caller then checks are public.
pub fn check_create_shape(req: &PostAuthorCreateRequest) -> Result<Vec<PostId>, Refusal> {
    if !req.extra.is_empty() {
        return Err(Refusal::Unauthorized("an unnamed request member"));
    }
    req.references
        .iter()
        .map(|r| match r {
            Reference::Reply { post_id }
            | Reference::Quote { post_id }
            | Reference::Repost { post_id } => Ok(*post_id),
            _ => Err(Refusal::Unauthorized(
                "a reference arm this class does not carry",
            )),
        })
        .collect()
}

/// The post the custodian signs: the principal's fields, the account as the
/// author, the nest's clock, and nothing that restricts the audience.
#[must_use]
pub fn build_post(account: &[u8; 32], req: PostAuthorCreateRequest, now: Timestamp) -> Post {
    Post {
        author: ActorId(*account),
        created_at: now,
        body: req.body,
        references: req.references,
        expires_at: None,
        gated: None,
        content_warning: req.content_warning,
        origin: None,
    }
}

/// Does this principal's record show it authored `post_id`?
fn principal_authored(
    conn: &Connection,
    account: &[u8; 32],
    principal_id: &[u8],
    post_id: &[u8; 32],
) -> anyhow::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM post_author_ops
              WHERE actor_id = ?1 AND principal_id = ?2 AND op = ?3 AND post_id = ?4
              LIMIT 1",
            params![hex::encode(account), principal_id, OP_CREATE, &post_id[..]],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Record a performed operation: the principal's counters and one
/// `post_author_ops` row, trimming to the newest [`OPS_KEPT_PER_PRINCIPAL`]
/// in the same write.
fn record(
    conn: &Connection,
    account: &[u8; 32],
    principal_id: &[u8],
    op: &str,
    post_id: &[u8; 32],
    now: u64,
) -> anyhow::Result<()> {
    count_use(conn, CLASS.principals_table, account, principal_id, now)?;
    let actor_hex = hex::encode(account);
    conn.execute(
        "INSERT INTO post_author_ops (actor_id, principal_id, at, class, op, post_id, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            actor_hex,
            principal_id,
            now,
            CLASS.class.name(),
            op,
            &post_id[..],
            format!("{op} {}", hex::encode(post_id))
        ],
    )?;
    conn.execute(
        "DELETE FROM post_author_ops
          WHERE actor_id = ?1 AND principal_id = ?2 AND id NOT IN
            (SELECT id FROM post_author_ops WHERE actor_id = ?1 AND principal_id = ?2
              ORDER BY id DESC LIMIT ?3)",
        params![actor_hex, principal_id, OPS_KEPT_PER_PRINCIPAL],
    )?;
    Ok(())
}

/// Is the post `id` public, as far as this nest knows? A post it holds is
/// public when it carries no `gated` block (a gate tier, a room or a sale
/// all ride there); a post it does not hold is someone else's to keep
/// private, and naming its id reveals nothing the principal did not hold.
async fn target_is_public(
    state: &Arc<AppState>,
    account: &[u8; 32],
    id: &PostId,
) -> Result<bool, RpcError> {
    let digest = crate::db::posts::cid_to_digest(id);
    match crate::routes::get_post_core(state, Some(*account), digest).await {
        crate::routes::GetPostOutcome::Found(bytes)
        | crate::routes::GetPostOutcome::AdminBlocked {
            body: Some(bytes), ..
        } => Ok(Post::decode_resolved_bytes(&bytes).is_some_and(|p| p.gated.is_none())),
        crate::routes::GetPostOutcome::AdminBlocked { body: None, .. } => Ok(false),
        crate::routes::GetPostOutcome::NotFound => Ok(true),
        crate::routes::GetPostOutcome::LegalTakedown { .. } => Ok(false),
        crate::routes::GetPostOutcome::Error => Err(internal("reading a referenced post")),
    }
}

/// `fauna.posts.author.create` for a third-party **principal**.
pub(crate) fn principal_create_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: PostAuthorCreateRequest =
                fauna_protocol::decode_strict(&payload).map_err(malformed)?;
            let targets = match check_create_shape(&req) {
                Ok(t) => t,
                Err(r) => return Err(r.rpc(KIND_CREATE, CLASS.ns)),
            };
            for target in &targets {
                if !target_is_public(&state, &caller.account, target).await? {
                    return Err(Refusal::Unauthorized("a reference to a non-public post")
                        .rpc(KIND_CREATE, CLASS.ns));
                }
            }
            let now = now_secs();
            let (signer, cert) = match authorize_and_load(&state, &caller, &CLASS, now).await? {
                Ok(s) => s,
                Err(r) => return Err(r.rpc(KIND_CREATE, CLASS.ns)),
            };
            let post = build_post(&caller.account, req, Timestamp::now());
            let (bytes, env) = sign_envelope(&signer, &post)
                .map_err(|e| internal(format!("sign delegated post: {e}")))?;
            let wire =
                canonical_encode(&EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert))
                    .map_err(|e| internal(format!("encode delegated post: {e}")))?;
            // The own-write door's own path — one ingest, no second door.
            let post_id =
                crate::routes::ingest_post_core(&state, caller.account, &bytes::Bytes::from(wire))
                    .await
                    .map_err(|e| match e {
                        crate::routes::PostCreateError::Ingest(api) => {
                            let mut e = RpcError::new(
                                "fauna.posts.ingest_failed",
                                "error.posts.ingest_failed",
                            );
                            e.details = Some(Box::new(fauna_protocol::Value::String(api.message)));
                            e
                        }
                        crate::routes::PostCreateError::Internal(msg) => internal(msg),
                    })?;
            let conn = state.db.conn().await;
            record(
                &conn,
                &caller.account,
                &caller.principal_id,
                OP_CREATE,
                &post_id,
                now,
            )
            .map_err(internal)?;
            drop(conn);
            encode_reply(&PostAuthorCreateReply {
                post_id: hex::encode(post_id),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.posts.author.delete` for a third-party **principal**.
pub(crate) fn principal_delete_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: PostAuthorDeleteRequest =
                fauna_protocol::decode_strict(&payload).map_err(malformed)?;
            let Some(digest) = crate::routes::parse_32_bytes(&req.post_id) else {
                return Err(crate::rpc_errors::invalid_params_ns(
                    "posts",
                    "invalid post_id hex",
                ));
            };
            {
                let conn = state.db.conn().await;
                if !principal_authored(&conn, &caller.account, &caller.principal_id, &digest)
                    .map_err(internal)?
                {
                    return Err(Refusal::Unauthorized("not a post this principal authored")
                        .rpc(KIND_DELETE, CLASS.ns));
                }
            }
            let now = now_secs();
            let (signer, cert) = match authorize_and_load(&state, &caller, &CLASS, now).await? {
                Ok(s) => s,
                Err(r) => return Err(r.rpc(KIND_DELETE, CLASS.ns)),
            };
            let tombstone = Tombstone {
                author: ActorId(caller.account),
                post_id: fauna_cbor::Cid::from_digest_dag_cbor(digest),
                created_at: Timestamp::now(),
            };
            let (bytes, env) = sign_envelope(&signer, &tombstone)
                .map_err(|e| internal(format!("sign delegated tombstone: {e}")))?;
            let wire =
                canonical_encode(&EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert))
                    .map_err(|e| internal(format!("encode delegated tombstone: {e}")))?;
            // The own-write delete's gate, on the real bytes.
            let verified = fauna_core::encoding::decode_tombstone(&wire)
                .map_err(|e| internal(format!("delegated tombstone rejected at the gate: {e}")))?;
            let outcome = match crate::routes::delete_post_core(
                &state,
                caller.account,
                &verified,
                digest,
                crate::routes::RenderSite::Now,
            )
            .await
            {
                Ok(o) => o,
                Err(crate::routes::PostDeleteError::NotAuthor) => {
                    return Err(
                        Refusal::Unauthorized("not the account's post").rpc(KIND_DELETE, CLASS.ns)
                    );
                }
                Err(crate::routes::PostDeleteError::Internal(msg)) => return Err(internal(msg)),
            };
            // The relay `fauna.posts.delete` does, so a forwarded copy does
            // not outlive the post (`feed.md` § Post deletion → Propagation).
            crate::routes::maybe_enqueue_delete_outbox(&state, &caller.account, &wire).await;
            let conn = state.db.conn().await;
            record(
                &conn,
                &caller.account,
                &caller.principal_id,
                OP_DELETE,
                &digest,
                now,
            )
            .map_err(internal)?;
            drop(conn);
            encode_reply(&PostAuthorDeleteReply {
                post_id: hex::encode(digest),
                deleted: matches!(outcome, crate::routes::PostDeleteOutcome::Deleted),
                extra: Default::default(),
            })
        })
    })
}

/// The actor-side half: the kinds' wire contract lives in the actor router,
/// but NO actor class holds them — the matrix arm is `ThirdParty` only — so
/// this runs the central gate and is refused there, always.
fn actor_handler(kind: &'static str) -> RpcHandler {
    Box::new(move |state, actor_id, _payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(&state, &actor_id, kind)
                .await?;
            // Unreachable while the matrix arm admits no actor class.
            Err(crate::rpc_errors::central_permission_denied())
        })
    })
}

/// Register the door — the client-side twin is
/// `fauna_protocol::kind::register_post_author_kinds`.
pub fn register_post_author_handlers(b: &mut RpcRouterBuilder) {
    for kind in [KIND_CREATE, KIND_DELETE] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: std::time::Duration::from_secs(10),
                handler: actor_handler(kind),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::PostBody;

    fn req(references: Vec<Reference>) -> PostAuthorCreateRequest {
        PostAuthorCreateRequest {
            body: PostBody::Text {
                content: "hi".into(),
                facets: vec![],
            },
            references,
            content_warning: None,
            extra: Default::default(),
        }
    }

    fn pid(b: u8) -> PostId {
        fauna_cbor::Cid::from_digest_dag_cbor([b; 32])
    }

    #[test]
    fn the_shape_admits_threading_references_and_refuses_the_rest() {
        let refs = vec![
            Reference::Reply { post_id: pid(1) },
            Reference::Quote { post_id: pid(2) },
            Reference::Repost { post_id: pid(3) },
        ];
        assert_eq!(
            check_create_shape(&req(refs)),
            Ok(vec![pid(1), pid(2), pid(3)])
        );
        for r in [
            Reference::Upvote { post_id: pid(1) },
            Reference::Downvote { post_id: pid(1) },
            Reference::React {
                post_id: pid(1),
                emoji: "x".into(),
            },
        ] {
            assert!(check_create_shape(&req(vec![r])).is_err());
        }
    }

    /// A member this build does not name — a newer client's audience
    /// restriction, say — is refused, never dropped: dropping it would post
    /// publicly what was meant restricted.
    #[test]
    fn an_unnamed_member_is_refused() {
        let mut r = req(vec![]);
        r.extra
            .insert("gated".into(), fauna_protocol::Value::Bool(true));
        assert_eq!(
            check_create_shape(&r),
            Err(Refusal::Unauthorized("an unnamed request member"))
        );
    }

    #[test]
    fn the_built_post_is_public_and_the_accounts() {
        let account = [7u8; 32];
        let post = build_post(&account, req(vec![]), Timestamp(42));
        assert_eq!(post.author, ActorId(account));
        assert!(post.gated.is_none());
        assert!(post.origin.is_none());
        assert_eq!(post.created_at, Timestamp(42));
    }
}
