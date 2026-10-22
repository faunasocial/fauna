//! The oracle's profile custodian: a third-party **principal** edits the
//! display fields of the account's profile as the account, signed with the
//! account's delegated authoring sub-key `K` (TP11).
//!
//! Owner: `docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle* (the `profile.author` row); the
//! key and its delegation: `docs/goal/behavior/atproto-pds-full.md` § D10;
//! which fields are the owner's to edit: `docs/goal/ui/profile.md` § Where
//! logic lives → *Field ownership*. The vocabulary is
//! `fauna_core::identity_op`, the wire `fauna_protocol::profile_author`, the
//! authorization path shared with the posting class (`authoring_oracle`).
//! `fauna.profile.author.set` runs, in this order:
//!
//! 1. the request's own shape — no member this build does not name, at least
//!    one field, and no `avatar` / `banner`: a picture names a media item the
//!    account already holds, which no principal can put yet, so either is
//!    refused with the typed `fauna.profile.author.media_unavailable` until
//!    the media plane's arm lets one — a refusal that lifts within the ruled
//!    policy, never a widening;
//! 2. the owner's LIVE `identity.op:profile.author` grant to the principal's
//!    attested key — re-read per request, so a revoke refuses the very next
//!    call;
//! 3. the delegation present, live by the wall clock and carrying
//!    `Capability::UpdateProfile` — revoking it destroys `K`, and the next
//!    call is refused;
//! 4. the rate ceiling (`fauna_core::identity_op::RateWindow`, on the
//!    principal's `profile_author_principals` row);
//! 5. read-modify-write over the account's stored profile
//!    ([`crate::profile_handlers::current_or_fresh_profile`]): only the named
//!    display fields change; `nests`, `admin_nests`, `recovery_head`,
//!    `inbox_mode` and `load_hint` are carried unchanged and are never the
//!    principal's to set; sign with `K`, the identity-signed cert embedded,
//!    and ingest through the one profile write door
//!    (`profile_handlers::ingest_profile_core`); record one
//!    `profile_author_ops` row.
//!
//! The account's external-apps switch is the session gate
//! (`principal_handlers::resolve_principal`, per call). The principal's
//! session, never the account's, is what reaches here: the kind's matrix arm
//! is `ThirdParty` only. Every policy refusal answers the same
//! `fauna.profile.permission_denied` — never which check failed; the log
//! names it — and the ceiling answers `rate_limited`.
//!
//! **The result is a delegated profile**, with the consequence
//! `docs/goal/behavior/identity-succession.md` states for every delegated
//! writer: a harvest seeds no anchor from it, and the owner's next sign-in
//! restores them.

use fauna_core::data::{Capability, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
use fauna_core::identity::ActorId;
use fauna_core::identity_op::IdentityOpClass;
use fauna_protocol::RpcError;
use fauna_protocol::profile_author::{KIND_SET, ProfileAuthorSetReply, ProfileAuthorSetRequest};
use rusqlite::{Connection, params};

use crate::authoring_oracle::{
    AuthoringClass, OPS_KEPT_PER_PRINCIPAL, Refusal, authorize_and_load, count_use, now_secs,
};
use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// The class this custodian performs, and where its state lives.
const CLASS: AuthoringClass = AuthoringClass {
    class: IdentityOpClass::ProfileAuthor,
    capability: Capability::UpdateProfile,
    principals_table: "profile_author_principals",
    ns: "profile",
};

/// The record's `op` column — the class's one operation.
const OP_SET: &str = "set";

/// Why step 1 refused: a policy refusal, or the typed media one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeRefusal {
    /// Answered as every policy refusal is.
    Policy(Refusal),
    /// A picture: no principal can put the media item it names yet.
    MediaUnavailable,
    /// The request changes nothing.
    Empty,
}

impl ShapeRefusal {
    fn rpc(self) -> RpcError {
        match self {
            ShapeRefusal::Policy(r) => r.rpc(KIND_SET, CLASS.ns),
            ShapeRefusal::MediaUnavailable => RpcError::new(
                "fauna.profile.author.media_unavailable",
                "error.profile.author.media_unavailable",
            ),
            ShapeRefusal::Empty => {
                crate::rpc_errors::invalid_params_ns(CLASS.ns, "the request names no field")
            }
        }
    }
}

/// Step 1, the pure half: the fields the request names, in the record's
/// order — or why it is refused.
pub fn check_shape(req: &ProfileAuthorSetRequest) -> Result<Vec<&'static str>, ShapeRefusal> {
    if !req.extra.is_empty() {
        return Err(ShapeRefusal::Policy(Refusal::Unauthorized(
            "an unnamed request member",
        )));
    }
    if req.avatar.is_some() || req.banner.is_some() {
        return Err(ShapeRefusal::MediaUnavailable);
    }
    let named: Vec<&'static str> = [
        ("display_name", req.display_name.is_some()),
        ("bio", req.bio.is_some()),
        ("links", req.links.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect();
    if named.is_empty() {
        return Err(ShapeRefusal::Empty);
    }
    Ok(named)
}

/// Apply the request's display fields to `profile` — the empty string
/// clearing a text field — leaving every other field as it is.
pub fn apply(profile: &mut fauna_core::data::Profile, req: ProfileAuthorSetRequest) {
    let text = |s: String| (!s.is_empty()).then_some(s);
    if let Some(name) = req.display_name {
        profile.display_name = text(name);
    }
    if let Some(bio) = req.bio {
        profile.bio = text(bio);
    }
    if let Some(links) = req.links {
        profile.links = links;
    }
}

/// Record a performed operation: the principal's counters and one
/// `profile_author_ops` row naming the fields set, trimming to the newest
/// [`OPS_KEPT_PER_PRINCIPAL`] in the same write.
fn record(
    conn: &Connection,
    account: &[u8; 32],
    principal_id: &[u8],
    fields: &[&str],
    now: u64,
) -> anyhow::Result<()> {
    count_use(conn, CLASS.principals_table, account, principal_id, now)?;
    let actor_hex = hex::encode(account);
    conn.execute(
        "INSERT INTO profile_author_ops (actor_id, principal_id, at, class, op, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            actor_hex,
            principal_id,
            now,
            CLASS.class.name(),
            OP_SET,
            format!("{OP_SET} {}", fields.join(" "))
        ],
    )?;
    conn.execute(
        "DELETE FROM profile_author_ops
          WHERE actor_id = ?1 AND principal_id = ?2 AND id NOT IN
            (SELECT id FROM profile_author_ops WHERE actor_id = ?1 AND principal_id = ?2
              ORDER BY id DESC LIMIT ?3)",
        params![actor_hex, principal_id, OPS_KEPT_PER_PRINCIPAL],
    )?;
    Ok(())
}

/// `fauna.profile.author.set` for a third-party **principal**.
pub(crate) fn principal_set_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: ProfileAuthorSetRequest =
                fauna_protocol::decode_strict(&payload).map_err(malformed)?;
            let fields = check_shape(&req).map_err(ShapeRefusal::rpc)?;
            let now = now_secs();
            let (signer, cert) = match authorize_and_load(&state, &caller, &CLASS, now).await? {
                Ok(s) => s,
                Err(r) => return Err(r.rpc(KIND_SET, CLASS.ns)),
            };
            let mut profile =
                crate::profile_handlers::current_or_fresh_profile(&state, &caller.account).await?;
            apply(&mut profile, req);
            profile.actor_id = ActorId(caller.account);
            profile.updated_at = Timestamp::now();
            let (bytes, env) = sign_envelope(&signer, &profile)
                .map_err(|e| internal(format!("sign delegated profile: {e}")))?;
            let wire =
                canonical_encode(&EmbedAsBytes::from_signed(bytes, env).with_signer_auth(cert))
                    .map_err(|e| internal(format!("encode delegated profile: {e}")))?;
            // The one profile write door, on the real bytes — no second door.
            crate::profile_handlers::ingest_profile_core(&state, caller.account, &wire).await?;
            let conn = state.db.conn().await;
            record(&conn, &caller.account, &caller.principal_id, &fields, now).map_err(internal)?;
            drop(conn);
            encode_reply(&ProfileAuthorSetReply {
                body: serde_bytes::ByteBuf::from(wire),
                extra: Default::default(),
            })
        })
    })
}

/// The actor-side half: the kind's wire contract lives in the actor router,
/// but NO actor class holds it — the matrix arm is `ThirdParty` only — so
/// this runs the central gate and is refused there, always.
fn actor_handler() -> RpcHandler {
    Box::new(move |state, actor_id, _payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(&state, &actor_id, KIND_SET)
                .await?;
            // Unreachable while the matrix arm admits no actor class.
            Err(crate::rpc_errors::central_permission_denied())
        })
    })
}

/// Register the door — the client-side twin is
/// `fauna_protocol::kind::register_profile_author_kinds`.
pub fn register_profile_author_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_SET,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: std::time::Duration::from_secs(10),
            handler: actor_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::{InboxMode, Profile, ProfileLink};

    fn base() -> Profile {
        Profile {
            actor_id: ActorId([7; 32]),
            display_name: Some("Alice".into()),
            bio: Some("bio".into()),
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::ContactsOnly,
            recovery_head: None,
            audience: fauna_core::data::ProfileAudience::ThisNest,
            updated_at: Timestamp(1),
        }
    }

    #[test]
    fn the_shape_names_the_fields_and_refuses_pictures_unknowns_and_nothing() {
        let req = ProfileAuthorSetRequest {
            bio: Some("x".into()),
            links: Some(vec![]),
            ..Default::default()
        };
        assert_eq!(check_shape(&req), Ok(vec!["bio", "links"]));
        for pic in [
            ProfileAuthorSetRequest {
                avatar: Some("ab".into()),
                ..req.clone()
            },
            ProfileAuthorSetRequest {
                banner: Some("ab".into()),
                ..req.clone()
            },
        ] {
            assert_eq!(check_shape(&pic), Err(ShapeRefusal::MediaUnavailable));
        }
        let mut newer = req.clone();
        newer
            .extra
            .insert("pronouns".into(), fauna_protocol::Value::Bool(true));
        assert_eq!(
            check_shape(&newer),
            Err(ShapeRefusal::Policy(Refusal::Unauthorized(
                "an unnamed request member"
            )))
        );
        assert_eq!(
            check_shape(&ProfileAuthorSetRequest::default()),
            Err(ShapeRefusal::Empty)
        );
    }

    /// Absent keeps, present sets, the empty string clears — and nothing the
    /// request cannot name moves.
    #[test]
    fn apply_changes_only_the_named_display_fields() {
        let mut p = base();
        let links = vec![ProfileLink {
            label: "l".into(),
            uri: "https://x.example".into(),
        }];
        apply(
            &mut p,
            ProfileAuthorSetRequest {
                display_name: Some(String::new()),
                links: Some(links.clone()),
                ..Default::default()
            },
        );
        assert_eq!(p.display_name, None);
        assert_eq!(p.bio.as_deref(), Some("bio"));
        assert_eq!(p.links, links);
        assert_eq!(p.inbox_mode, InboxMode::ContactsOnly);
    }
}
