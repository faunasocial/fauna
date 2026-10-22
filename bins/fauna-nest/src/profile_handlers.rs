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
//! (at most two rows: the latest, plus the author's latest identity-signed row
//! while the latest is delegated — served back on the owner's own `get` as
//! `last_direct_body`; the only rows dropped are the author's own superseded
//! display state — recreatable, no user-data loss). Write is immediate (no
//! `handle.change`-style delay).
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (`fauna.profile.{get,set}` → `User | Admin`, like `fauna.account.get` /
//! `fauna.profile.handle.change` — the human running the client manages their
//! own profile; an admin actor has one too); `get` is also `ThirdParty` under
//! the profile arm, served by [`principal_profile_get_handler`] over the same
//! read. Kind registry metadata twin: `KindRegistry::register_profile_kinds`.
//!
//! `fauna.profile.get_remote(actor_id, nest_url)` relays `get` to another
//! nest (`profile.md` § Where logic lives → *The remote read*): `User`-only,
//! it originates `fauna.federation.profile.get` and forwards the home's bytes
//! unstored; the home serves only an actor whose profile opted in. The reply
//! stamps the home's identity this nest dial-proved (`home_nest_actor_id`,
//! `posts.get_remote`'s stamp), which the reader's byte plane pins the
//! profile's avatar and banner fetch against (`transport-trust.md` § The post
//! arm → *A relayed profile's pictures*).

use std::sync::Arc;
use std::time::Duration;

use fauna_core::data::{Capability, Profile};
use fauna_core::encoding::{
    EmbedAsBytes, canonical_decode, decode_signed_bytes, verify_authoring_envelope,
};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    profile::{
        ProfileGetRemoteRequest, ProfileGetReply, ProfileGetRequest, ProfileSetReply,
        ProfileSetRequest,
    },
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
            profile_get_core(&state, &payload, Some(actor_id)).await
        })
    })
}

/// The `fauna:profile:read` principal's `fauna.profile.get`
/// (`authorization-server.md` § Scope grammar → *The profile arm*): the same
/// read the account's apps make — any actor this nest holds a profile of,
/// the signed record verbatim, `not_found` when it holds none. Nothing of the
/// account's own is consulted, so the caller is not either; the ceiling and
/// the scope check are `principal_handlers`' gate.
pub(crate) fn principal_profile_get_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, _caller, payload| {
        Box::pin(async move { profile_get_core(&state, &payload, None).await })
    })
}

/// Decode the request, read the target's latest profile, serve it verbatim.
///
/// `caller` is the account's own actor on the account's `fauna.profile.get`,
/// `None` on the principal's: when it names the target and the latest is
/// delegated, the reply carries the owner's last identity-signed row as
/// `last_direct_body` (`profile.md` § Where logic lives → *Profile
/// publish/edit* → *Write semantics*) — on the owner's own read only.
async fn profile_get_core(
    state: &Arc<AppState>,
    payload: &[u8],
    caller: Option<[u8; 32]>,
) -> Result<bytes::Bytes, RpcError> {
    let req: ProfileGetRequest = decode(payload).map_err(malformed)?;
    let target = fauna_core::hex32::decode(&req.actor_id)
        .map_err(|_| invalid_request("invalid actor_id hex"))?;

    let Some(body) = latest_profile_bytes(state, &target).await? else {
        return Err(not_found("no profile for actor"));
    };
    let last_direct_body = if caller == Some(target) && !is_direct_profile(&body, &target) {
        let latest_id: [u8; 32] = *blake3::hash(&body).as_bytes();
        let conn = state.db.conn().await;
        newest_direct_profile_row(&conn, &target, &latest_id)
            .map_err(|e| internal(format!("query profile: {e}")))?
            .map(|(_, payload)| serde_bytes::ByteBuf::from(payload))
    } else {
        None
    };
    encode_reply(&ProfileGetReply {
        body: serde_bytes::ByteBuf::from(body),
        last_direct_body,
        ..Default::default()
    })
}

/// Whether stored profile bytes are `actor`'s own identity-signed word: they
/// verify, signed directly (no delegated sub-key), about `actor` itself — not a
/// predecessor's row a succession re-pointed here. The test the two-row prune
/// and the own read share (`profile.md` § Persistence → *After an identity
/// succession, the successor RE-PUBLISHES* → base-admission rule 3).
fn is_direct_profile(body: &[u8], actor: &[u8; 32]) -> bool {
    matches!(
        fauna_core::encoding::decode_profile(body),
        Ok((p, fauna_core::encoding::AuthoringOrigin::Direct)) if p.actor_id.0 == *actor
    )
}

/// `actor`'s newest stored profile row other than `except` that is its own
/// identity-signed word ([`is_direct_profile`]), as `(id, payload)`. At most two
/// profile rows per author exist (the prune in [`ingest_profile_core`]), so the
/// walk decodes at most one row.
fn newest_direct_profile_row(
    conn: &rusqlite::Connection,
    actor: &[u8; 32],
    except: &[u8; 32],
) -> rusqlite::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut stmt = conn.prepare(
        "SELECT id, payload FROM content
          WHERE author = ?1 AND schema = 'profile' AND id != ?2
          ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![actor.as_slice(), except.as_slice()],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
    )?;
    for row in rows {
        let (id, payload) = row?;
        if is_direct_profile(&payload, actor) {
            return Ok(Some((id, payload)));
        }
    }
    Ok(None)
}

// ── fauna.profile.get_remote ───────────────────────────────────

/// The read of a profile homed on **another nest**, relayed by the reader's
/// own nest (`profile.md` § Where logic lives → *The remote read*). The
/// client asks this only after its own nest answered `fauna.profile.not_found`
/// for an actor whose home it carries.
///
/// The home serves `fauna.federation.profile.get` only for an actor it hosts
/// whose latest signed profile opted in (`audience = Everyone`); its one
/// refusal is the federation namespace's `not_found`, re-spoken here as the
/// same `fauna.profile.not_found` the same-nest read answers, so the client's
/// one fallback covers both doors. Every other typed refusal rides through
/// [`map_peer_relay_error`](crate::rpc_errors::map_peer_relay_error).
///
/// **This nest keeps nothing** (the `posts.get_remote` posture): it forwards
/// the home's bytes and ingests none of them — federation receive-write is
/// deferred (`profile.md` § Where logic lives).
///
/// **The reply names the home** (`posts.get_remote`'s posture too): the
/// `peer_nest_id` this nest dialed the channel as rides back as
/// `home_nest_actor_id`, so the reader's foreign byte plane can graduate a pin
/// for the home before fetching the pictures the profile names — a self-signed
/// home included. A same-nest `profile.get` stamps nothing.
fn profile_get_remote_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.profile.get_remote").await?;
            let req: ProfileGetRemoteRequest = decode(&payload).map_err(malformed)?;
            let target = fauna_core::hex32::decode(&req.actor_id)
                .map_err(|_| invalid_request("invalid actor_id hex"))?;
            let peer_url = req.nest_url.trim();
            if peer_url.is_empty() {
                return Err(invalid_request(
                    "nest_url must name the actor's home nest (same-nest reads use profile.get)",
                ));
            }

            match crate::federation_pool::originate_profile_get(
                &state.federation_pool,
                &state,
                peer_url,
                hex::encode(target),
            )
            .await
            {
                Ok(Ok(reply)) => {
                    // The identity `originate` just dialed the channel as
                    // (cached, so no second dial) — this nest's verified
                    // knowledge of the home, never the home's own word and
                    // never the actor-signed `nests[0]`. The reader's byte
                    // plane graduates its pin against it before fetching the
                    // profile's pictures (`transport-trust.md` § The post arm
                    // → *A relayed profile's pictures*). A lookup that fails
                    // now leaves the field absent: the reader keeps the
                    // WebPKI floor, never weaker.
                    let home_nest_actor_id = state
                        .federation_pool
                        .resolve_peer_nest_id(peer_url)
                        .await
                        .ok()
                        .map(hex::encode);
                    encode_reply(&ProfileGetReply {
                        body: serde_bytes::ByteBuf::from(reply.profile),
                        home_nest_actor_id,
                        ..Default::default()
                    })
                }
                Ok(Err(peer_err)) if peer_err.code == "fauna.federation.not_found" => {
                    Err(not_found("profile not served by its home nest"))
                }
                Ok(Err(peer_err)) => Err(crate::rpc_errors::map_peer_relay_error(
                    peer_err,
                    "the cross-nest profile read relay",
                )),
                Err(pool_err) => {
                    tracing::error!("federation profile read (get_remote): {pool_err}");
                    Err(internal("federation profile read failed"))
                }
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
    latest_profile_payloads(&conn, std::slice::from_ref(actor))
        .map(|mut rows| rows.remove(actor))
        .map_err(|e| internal(format!("query profile: {e}")))
}

/// [`latest_profile_bytes`] for many actors at once — the one statement both
/// read through, so the single read and the feed's batched author decoration
/// (`feed_routes::decorate_authors_best_effort`, `bridges.md` § Unified feed
/// ingestion → *Bridged authors* → *The local native author*) pick the same
/// row: per author, the newest by `created_at`. Actors with no profile are
/// simply absent from the map.
pub(crate) fn latest_profile_payloads(
    conn: &rusqlite::Connection,
    actors: &[[u8; 32]],
) -> rusqlite::Result<std::collections::HashMap<[u8; 32], Vec<u8>>> {
    let mut out = std::collections::HashMap::new();
    // Well under SQLite's bound-variable floor; a feed page is ≤ 200 rows.
    for chunk in actors.chunks(100) {
        let marks = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn.prepare(&format!(
            "SELECT c.author, c.payload FROM content c
             WHERE c.schema = 'profile' AND c.author IN ({marks})
               AND c.id = (SELECT c2.id FROM content c2
                           WHERE c2.author = c.author AND c2.schema = 'profile'
                           ORDER BY c2.created_at DESC LIMIT 1)"
        ))?;
        let ids = chunk.iter().map(|id| id.as_slice());
        let rows = stmt.query_map(rusqlite::params_from_iter(ids), |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (author, payload) = row?;
            if let Ok(author) = <[u8; 32]>::try_from(author.as_slice()) {
                out.insert(author, payload);
            }
        }
    }
    Ok(out)
}

/// The base of a nest-side read-modify-write edit of `actor`'s profile: the
/// latest stored one, decoded through the shared verify, or — for an account
/// that has published none — a fresh one with nothing to preserve, so its
/// empty fields are the honest starting state, not a loss. The two nest-side
/// editors share it: the ATProto profile round-trip
/// (`bridge_atproto_handlers::round_trip_profile_update`) and the oracle's
/// profile custodian (`profile_author_oracle`).
pub(crate) async fn current_or_fresh_profile(
    state: &Arc<AppState>,
    actor: &[u8; 32],
) -> Result<Profile, RpcError> {
    Ok(match latest_profile_bytes(state, actor).await? {
        Some(bytes) => {
            fauna_core::encoding::decode_profile(&bytes)
                .map_err(|e| internal(format!("stored profile does not decode: {e}")))?
                .0
        }
        None => Profile {
            actor_id: fauna_core::identity::ActorId(*actor),
            display_name: None,
            bio: None,
            avatar: None,
            banner: None,
            links: Vec::new(),
            nests: Vec::new(),
            admin_nests: Vec::new(),
            load_hint: None,
            inbox_mode: fauna_core::data::InboxMode::Open,
            recovery_head: None,
            audience: fauna_core::data::ProfileAudience::ThisNest,
            updated_at: fauna_core::data::Timestamp::now(),
        },
    })
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
    // every other `content` row's `created_at` (db::posts::put_post), and
    // strictly after the author's newest kept row, so two writes inside one
    // millisecond still leave an unambiguous latest beside the kept direct
    // row.
    let id: [u8; 32] = *blake3::hash(body).as_bytes();

    // Scoped so the non-Send conn guard + tx are definitively dropped
    // before the projection nudge's await below.
    {
        let conn = state.db.conn().await;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| internal(format!("begin profile.set tx: {e}")))?;
        let newest: Option<i64> = tx
            .query_row(
                "SELECT MAX(created_at) FROM content WHERE author = ?1 AND schema = 'profile'",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get(0),
            )
            .map_err(|e| internal(format!("read profile created_at: {e}")))?;
        let created_at = (crate::db::now_epoch_millis() * 1000).max(newest.map_or(0, |n| n + 1));
        crate::db::content::insert_content(
            &tx, &id, "profile", &actor_id, created_at, body, None, "fauna", None,
        )
        .map_err(|e| internal(format!("insert profile: {e}")))?;
        // Prune (same txn) to at most two rows: this one, plus — when this
        // one is delegated — the author's newest identity-signed row, the
        // owner's own word the writers' base admission reads `audience`,
        // `nests` and `recovery_head` from. An identity-signed write prunes
        // back to one. recreatable — the only rows dropped are the author's
        // own superseded display state, replaced by this newer signed
        // profile; no user-data loss (`profile.md` § Where logic lives,
        // write semantics).
        let keep = if signer_auth.is_some() {
            newest_direct_profile_row(&tx, &actor_id, &id)
                .map_err(|e| internal(format!("read the owner's direct profile: {e}")))?
                .map(|(keep, _)| keep)
        } else {
            None
        };
        tx.execute(
            "DELETE FROM content WHERE author = ?1 AND schema = 'profile' AND id != ?2
               AND id != ?3",
            rusqlite::params![
                actor_id.as_slice(),
                id.as_slice(),
                keep.as_deref().unwrap_or(id.as_slice())
            ],
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
    // get_remote: `get` relayed to an actor's home nest. Pure read,
    // replay-safe; 30 s for the peer round trip.
    b.add(
        "fauna.profile.get_remote",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: profile_get_remote_handler(),
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
