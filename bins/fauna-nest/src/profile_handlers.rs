//! WS-RPC handlers for the user-facing `fauna.profile.*` surface — the
//! per-user *detail* read (`get`) the Profile page (and contact-row / feed-
//! author tap-through) renders identity from, and the owner's own-write
//! (`set`). See `docs/goal/ui/profile.md` § Where logic lives.
//!
//! `fauna.profile.get(actor_id) -> Profile` is the **read** half. It lives in
//! the singular `fauna.profile.*` namespace alongside the account surface's
//! `fauna.profile.handle.change` (`account_handlers::register_account_user_handlers`).
//! The handler reuses the same stored-content read the
//! `activitypub::actor_routes` AP actor route already performs — the latest
//! `schema = 'profile'` content row for the target author — and serves the
//! stored bytes verbatim as `ProfileGetReply::body`; the client decodes them
//! (`fauna-client-profile::decode_profile`, the shared
//! `fauna_core::encoding::decode_profile`). No business logic is duplicated.
//!
//! `fauna.profile.set(body) -> ()` is the **write** half (ratified 2026-06-18,
//! `profile.md` § Where logic lives → *Profile publish/edit*). It is a
//! **client-originated own-write** — the nest holds no secret key, so it cannot
//! sign a `Profile`; every genuine profile originates on the owner's client,
//! which `sign_and_pack`s it into the signed `EmbedAsBytes` wire. The handler
//! **verifies the Ed25519 signature at ingest, asserts the inner
//! `Profile.actor_id` is the authenticated caller**, then stores the signed
//! bytes as the `schema = 'profile'` content row (id = `blake3(body)`) and
//! **prunes the author's superseded profile rows in the same transaction**
//! (keep-latest-1; the only row dropped is the author's own superseded display
//! state — recreatable, no user-data loss). Write is immediate (no
//! `handle.change`-style delay).
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (`fauna.profile.{get,set}` → `User | Admin`, like `fauna.account.get` /
//! `fauna.profile.handle.change` — the human running the client manages their
//! own profile; an admin actor has one too). Kind registry metadata twin:
//! `KindRegistry::register_profile_kinds`.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::OptionalExtension;

use fauna_core::data::{Capability, Profile};
use fauna_core::encoding::{
    EmbedAsBytes, canonical_decode, decode_signed_bytes, verify_authoring_envelope,
};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    profile::{ProfileGetReply, ProfileGetRequest, ProfileSetReply, ProfileSetRequest},
};

use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn invalid_request(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_request_ns("profile", reason)
}

/// No stored profile for the requested actor (the actor has not yet published
/// one via `fauna.profile.set` — onboarding is publish-on-first-edit).
fn not_found(reason: &str) -> RpcError {
    crate::rpc_errors::not_found_ns("profile", reason)
}

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("profile", reason)
}

use crate::rpc_errors::internal;

use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── fauna.profile.get ──────────────────────────────────────────

fn profile_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.profile.get").await?;
            let req: ProfileGetRequest = decode(&payload).map_err(malformed)?;
            let target = fauna_core::hex32::decode(&req.actor_id)
                .map_err(|_| invalid_request("invalid actor_id hex"))?;

            let payload = latest_profile_bytes(&state, &target).await?;

            match payload {
                Some(body) => encode_reply(&ProfileGetReply {
                    body: serde_bytes::ByteBuf::from(body),
                    extra: std::collections::BTreeMap::new(),
                }),
                None => Err(not_found("no profile for actor")),
            }
        })
    })
}

/// The latest stored `schema = 'profile'` content row for `actor`, verbatim —
/// the one read `fauna.profile.get`, `activitypub::actor_routes::get_actor`
/// and the ATProto profile round-trip all share, so none of them can drift on
/// *which* row is the current profile (keep-latest-1 by `created_at`).
pub(crate) async fn latest_profile_bytes(
    state: &Arc<AppState>,
    actor: &[u8; 32],
) -> Result<Option<Vec<u8>>, RpcError> {
    let conn = state.db.conn().await;
    conn.query_row(
        "SELECT payload FROM content WHERE author = ?1 AND schema = 'profile'
         ORDER BY created_at DESC LIMIT 1",
        rusqlite::params![actor.as_slice()],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| internal(format!("query profile: {e}")))
}

// ── fauna.profile.set ──────────────────────────────────────────

/// Ingest one signed `Profile` wire as `actor`'s current profile — the single
/// profile write door.
///
/// `fauna.profile.set` is one caller; the ATProto PDS profile round-trip
/// (`atproto-pds-full.md` F2.3) is the other, and it goes through *here* rather
/// than writing the content row itself for the same reason
/// [`crate::routes::ingest_post_core`] is shared by the post round-trip: this
/// gate re-runs the full signature chain on the real bytes, so an arm that
/// produces a profile through it provably cannot produce one the ordinary
/// publish path would reject. A second door would be a second, unverified one.
///
/// **The verify accepts a delegated signature** (D10 + F2.3): the wire may be
/// signed by the account's server-held authoring sub-key when the
/// identity-signed cert rides in `signer_auth` and grants
/// [`Capability::UpdateProfile`]. It stays STRICT signed-only — unlike the
/// lenient read-side `decode_profile`, a bare/unsigned profile is rejected
/// here, because every genuine profile is signed by *someone* (the owner's
/// client or, for an external-app edit, the delegated sub-key) and accepting a
/// bare one would let any caller publish an unattributable profile.
pub(crate) async fn ingest_profile_core(
    state: &Arc<AppState>,
    actor_id: [u8; 32],
    body: &[u8],
) -> Result<(), RpcError> {
    let wire = canonical_decode::<EmbedAsBytes>(body)
        .map_err(|e| invalid_request(&format!("not a signed profile envelope: {e}")))?;
    let signer_auth = wire.signer_auth.clone();
    let (signed_bytes, env) = wire
        .into_signed()
        .map_err(|e| invalid_request(&format!("malformed profile envelope: {e}")))?;
    let profile: Profile = decode_signed_bytes(&signed_bytes)
        .map_err(|e| invalid_request(&format!("decode profile: {e}")))?;
    verify_authoring_envelope(
        &profile,
        &signed_bytes,
        &env,
        signer_auth.as_deref(),
        &Capability::UpdateProfile,
        profile.updated_at,
    )
    // Carry the chain's own step message: which of the six steps refused is
    // exactly what a cross-binary failure needs named (a swallowed cause here
    // cost a tier_3 round on 2026-07-29). The step text names key roles and
    // capability words only — no secret material.
    .map_err(|e| invalid_request(&format!("profile signature verification failed: {e}")))?;

    // Own-write: the signed profile's actor_id MUST be the caller. The
    // signature already binds the bytes to `profile.actor_id`; this binds
    // `profile.actor_id` to the authenticated connection actor, so a
    // caller can only publish their OWN profile. It binds the delegated arm
    // too — a sub-key may only ever author its own grantor's profile, since
    // the chain verify already tied `cert.actor_id` to `profile.actor_id`.
    if profile.actor_id.0 != actor_id {
        return Err(permission_denied(
            "profile.actor_id does not match the authenticated caller",
        ));
    }

    // Content-addressed id ⇒ a byte-identical re-submit is idempotent
    // (`INSERT OR REPLACE` re-writes the same row). Microseconds, matching
    // every other `content` row's `created_at` (db::posts::put_post).
    let id: [u8; 32] = *blake3::hash(body).as_bytes();
    let created_at = crate::db::now_epoch_millis() * 1000;

    // Scoped so the non-Send conn guard + tx are definitively dropped
    // before the projection nudge's await below.
    {
        let conn = state.db.conn().await;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| internal(format!("begin profile.set tx: {e}")))?;
        crate::db::content::insert_content(
            &tx, &id, "profile", &actor_id, created_at, body, None, "fauna", None,
        )
        .map_err(|e| internal(format!("insert profile: {e}")))?;
        // Keep-latest prune (same txn): drop the author's superseded profile
        // rows. recreatable — the only row dropped is the author's own
        // superseded display state, replaced by this newer signed profile;
        // no user-data loss (`profile.md` § Where logic lives, write
        // semantics).
        tx.execute(
            "DELETE FROM content WHERE author = ?1 AND schema = 'profile' AND id != ?2",
            rusqlite::params![actor_id.as_slice(), id.as_slice()],
        )
        .map_err(|e| internal(format!("prune superseded profiles: {e}")))?;
        tx.commit()
            .map_err(|e| internal(format!("commit profile.set tx: {e}")))?;
    }

    // ATProto PDS projection nudge (S3, `atproto-pds-bridge.md`
    // § Where logic lives): the `app.bsky.actor.profile` record at
    // rkey `self` may need re-projecting from the new profile. Fired
    // after the successful write; best-effort (the bridge re-fetches
    // `fauna.bridges.atproto.fetch_profile` on its poll too).
    crate::bridge_atproto_handlers::notify_bridges_atproto_projection_ready(state, Some(actor_id))
        .await;

    Ok(())
}

fn profile_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.profile.set").await?;
            let req: ProfileSetRequest = decode(&payload).map_err(malformed)?;
            ingest_profile_core(&state, actor_id, &req.body).await?;
            encode_reply(&ProfileSetReply {
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_profile_handlers(b: &mut RpcRouterBuilder) {
    // Replay semantics + rationale: see `KindRegistry::register_profile_kinds`.
    // get is a pure read (replay-safe @5s), like fauna.posts.get /
    // fauna.account.get.
    b.add(
        "fauna.profile.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: profile_get_handler(),
        },
    );
    // set is a client-signed own-write. Replay-safe @10s: the content row id is
    // `blake3(body)` and the insert is `INSERT OR REPLACE`, so a byte-identical
    // re-submit re-writes the same row + re-runs the same idempotent keep-latest
    // prune (like fauna.posts.create).
    b.add(
        "fauna.profile.set",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: profile_set_handler(),
        },
    );
}
