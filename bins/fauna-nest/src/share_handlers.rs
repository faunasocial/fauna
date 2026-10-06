//! Share-link control-plane WS-RPC handlers (bearer connection) — part of
//! the WS-RPC-everywhere migration (tracked internally).
//!
//! A **net-new** feature, not a transport migration: there is no HTTP twin. A
//! ShareToken is client-minted, stateless and self-verifying
//! (`fauna_core::share::ShareToken`), so:
//!
//! - `fauna.share.create` — the client mints the token and supplies its
//!   base64url form; this handler decodes+verifies it, requires the token's
//!   `author` to equal the connection actor, and **registers** its metadata in
//!   `share_tokens` (idempotent on the derived token-id). It does NOT mint.
//! - `fauna.share.list` — the calling actor's registered tokens, newest first.
//! - `fauna.share.revoke` — flag one of the caller's tokens revoked.
//!
//! The public `GET /share/{token}` (`share_routes.rs`) stays HTTP residue and
//! gains a revocation-list check (revoked → `410 Gone`).
//!
//! Gate `User | Admin` (a personal share surface; an admin shares too) —
//! enforced in `bridge_method_allowlist::is_permitted`. Wire types +
//! design: `libs/fauna-protocol/src/share.rs`; goal doc
//! `docs/goal/architecture/api-layers.md` § Share.

use std::time::Duration;

use fauna_core::share::{self, ShareToken};
use fauna_protocol::share::{
    ShareCreateReply, ShareCreateRequest, ShareListReply, ShareListRequest, ShareRecord,
    ShareRevokeReply, ShareRevokeRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::db::share_tokens::ShareTokenRow;
use crate::routes::{AppState, parse_32_bytes as parse_token_id};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for the `fauna.share.*` kinds.
const NS: &str = "share";

/// The largest sealed filename a registration may carry. A `SealedLabel` of a
/// name is the name plus ~40 bytes of envelope; 4 KiB is far past any real
/// filename while keeping one registry row bounded.
const MAX_FILENAME_SEALED_BYTES: usize = 4 * 1024;

// ── error / encode helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn coded(code: &str, detail: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::coded_ns(NS, code, detail)
}

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// Project a stored row onto the wire record (byte ids → hex).
fn row_to_record(row: ShareTokenRow) -> ShareRecord {
    ShareRecord {
        token_id: hex::encode(row.token_id),
        manifest_hash: hex::encode(row.manifest_hash),
        expires_at: row.expires_at,
        public: row.public,
        revoked: row.revoked,
        created_at: row.created_at,
        key_in_fragment: row.key_in_fragment,
        filename_sealed: serde_bytes::ByteBuf::from(row.filename_sealed),
        extra: Default::default(),
    }
}

/// Best-effort E2EE (content-key) manifest detection for the mint-side type
/// gate (INFO-2, FS-BIND family): `true` only when the token's manifest is
/// present in the local blob store and positively parses as a content-key
/// `ChunkManifest` (`stored_hashes.is_some()` — the marker the serve-side twin,
/// `share_routes.rs` step 8b, refuses with 403). A public share link can only
/// ever emit ciphertext for such a set (the nest holds no content key), so
/// registering the token would hand the sharer a dead link.
///
/// Best-effort by design: no backup service, an absent manifest
/// (mint-before-upload is legal), or an undecodable blob all return `None`
/// and register as before — the serve-side gate stays authoritative.
///
/// `Some(true)` = positively content-key-sealed, `Some(false)` = positively
/// plaintext. The fragment-keyed arm reads the mirror: a declared token is
/// refused only over a POSITIVELY plaintext manifest (there is nothing to key),
/// and the serve side refuses the rest.
async fn manifest_is_e2ee(state: &AppState, manifest_hash: &[u8; 32]) -> Option<bool> {
    let svc = state.backup_service.as_ref()?;
    let hash = fauna_core::data::ContentHash::from_digest_raw(*manifest_hash);
    let blob = svc.local_blob_store().get(&hash).await.ok()??;
    let bytes = crate::backup::decode_blob(&blob, svc.encryption_key()).ok()?;
    let manifest =
        fauna_core::encoding::canonical_decode::<fauna_core::chunk::ChunkManifest>(&bytes).ok()?;
    Some(manifest.stored_hashes.is_some())
}

// ── fauna.share.create ──────────────────────────────────────────────────────

fn create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.share.create").await?;
            let req: ShareCreateRequest = decode(&payload).map_err(malformed)?;

            // Decode + verify the client-minted token. `from_base64url` runs the
            // sign-over-CID two-step, so a token that decodes here is already
            // signature-verified.
            let token = ShareToken::from_base64url(&req.token)
                .map_err(|e| coded("invalid_request", format!("invalid share token: {e}")))?;

            // The registering actor must be the token's author — the nest never
            // registers a token on another actor's behalf.
            if token.author.0 != actor_id {
                return Err(coded(
                    "permission_denied",
                    "share token author is not the calling actor",
                ));
            }

            // Mint-side gate for the BORN-GATED restricted-share flag. `/share/{token}` serves only public tokens, so registering
            // a `public: false` one would hand the sharer a dead link — the
            // same failure the E2EE gate below exists to prevent, so it gets
            // the same treatment: refuse at share time, honestly.
            //
            // This is the door to gate when restricted shares are actually
            // built. They need a real audience on `ShareToken` (the type has no
            // field to check against today) plus the app UI that sets it —
            // "who may open this link" is a user choice, so by the one-
            // configuration-surface invariant it is app UI + nest state, never
            // a bare wire flag. Until then the flag is refused rather than
            // silently honored, which is what it was: the serve-side check it
            // used to reach compared the caller to nothing at all.
            if !token.public {
                return Err(coded(
                    "restricted_shares_unimplemented",
                    "restricted share links are not implemented — mint the token with public: true",
                ));
            }

            // The envelope travels with a fragment-keyed token and with nothing
            // else (`share-links.md` § The private-file extension): a declared
            // token without one could never open, and one on a public link
            // would rest a key for nothing.
            let key_envelope = match (token.key_in_fragment, req.key_envelope.as_deref()) {
                (true, Some(env)) if !env.is_empty() => {
                    if env.len() > share::MAX_KEY_ENVELOPE_BYTES {
                        return Err(coded(
                            "invalid_request",
                            format!(
                                "key envelope is {} bytes (limit {})",
                                env.len(),
                                share::MAX_KEY_ENVELOPE_BYTES
                            ),
                        ));
                    }
                    // The token rides the URL path every request log sees, so
                    // a private link's name travels only inside the envelope.
                    if !token.filename.is_empty() {
                        return Err(coded(
                            "invalid_request",
                            "a fragment-keyed token carries an empty filename — the name \
                             travels sealed in the key envelope",
                        ));
                    }
                    Some(env)
                }
                (true, _) => {
                    return Err(coded(
                        "invalid_request",
                        "a fragment-keyed token registers with its key envelope",
                    ));
                }
                (false, Some(_)) => {
                    return Err(coded(
                        "invalid_request",
                        "a key envelope rides only a fragment-keyed token",
                    ));
                }
                (false, None) => None,
            };

            // The author's sealed copy of the name (`share-links.md` § The
            // filename rests sealed) is stored opaque — the nest holds no key
            // to open or check it — and is the only form the name rests in. A
            // registration without one would list as a row nobody can name;
            // refuse it rather than rest it.
            let filename_sealed = req.filename_sealed.as_slice();
            if filename_sealed.is_empty() {
                return Err(coded(
                    "invalid_request",
                    "a share link registers with its sealed filename",
                ));
            }
            if filename_sealed.len() > MAX_FILENAME_SEALED_BYTES {
                return Err(coded(
                    "invalid_request",
                    format!(
                        "sealed filename is {} bytes (limit {MAX_FILENAME_SEALED_BYTES})",
                        filename_sealed.len()
                    ),
                ));
            }

            // Mint-side E2EE type gate (INFO-2) and its mirror exemption. An
            // undeclared token whose manifest is positively content-key-sealed
            // is refused — the GET path can only serve ciphertext for it and
            // refuses (403, `share_routes.rs`), so registering would hand out a
            // dead link. A DECLARED token is the other way round: it registers
            // over a sealed manifest and is refused over a positively plaintext
            // one, which has nothing to key. Both best-effort (an absent
            // manifest registers); the serve side stays authoritative.
            match (
                token.key_in_fragment,
                manifest_is_e2ee(&state, &token.manifest_hash).await,
            ) {
                (false, Some(true)) => {
                    return Err(coded(
                        "unsupported_encrypted_set",
                        "share links are not available for end-to-end encrypted folders",
                    ));
                }
                (true, Some(false)) => {
                    return Err(coded(
                        "invalid_request",
                        "a fragment-keyed link names a sealed file; this manifest is \
                         plaintext, so there is nothing to key",
                    ));
                }
                _ => {}
            }

            // Derive the registry id from the same bytes the GET path hashes.
            let token_id = share::token_id_from_base64url(&req.token)
                .map_err(|e| coded("invalid_request", format!("token id: {e}")))?;
            let expires_at = i64::try_from(token.expires).unwrap_or(i64::MAX);

            let row = match key_envelope {
                Some(env) => state
                    .db
                    .register_private_share_token(
                        &token_id,
                        &actor_id,
                        &token.manifest_hash,
                        filename_sealed,
                        expires_at,
                        env,
                    )
                    .await
                    .map_err(internal)?,
                None => state
                    .db
                    .register_share_token(
                        &token_id,
                        &actor_id,
                        &token.manifest_hash,
                        filename_sealed,
                        expires_at,
                        token.public,
                    )
                    .await
                    .map_err(internal)?,
            };

            encode_reply(&ShareCreateReply {
                share: row_to_record(row),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.share.list ────────────────────────────────────────────────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.share.list").await?;
            let _req: ShareListRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .list_share_tokens_for_author(&actor_id)
                .await
                .map_err(internal)?;
            let shares = rows.into_iter().map(row_to_record).collect();
            encode_reply(&ShareListReply {
                shares,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.share.revoke ──────────────────────────────────────────────────────

fn revoke_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.share.revoke").await?;
            let req: ShareRevokeRequest = decode(&payload).map_err(malformed)?;

            let token_id = parse_token_id(&req.token_id)
                .ok_or_else(|| coded("invalid_request", "invalid token_id hex"))?;

            let revoked = state
                .db
                .revoke_share_token(&token_id, &actor_id)
                .await
                .map_err(internal)?;
            if !revoked {
                return Err(coded(
                    "not_found",
                    "share token not found or not owned by you",
                ));
            }
            encode_reply(&ShareRevokeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ─────────────────────────────────────────────────

/// Register the share-link control plane on the **bearer** router. All three are
/// `forbid_replay = false` @5 s (a read + idempotent local mutations) — see
/// `KindRegistry::register_share_kinds`.
pub fn register_share_handlers(b: &mut RpcRouterBuilder) {
    for (kind, handler) in [
        ("fauna.share.create", create_handler()),
        ("fauna.share.list", list_handler()),
        ("fauna.share.revoke", revoke_handler()),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(5),
                handler,
            },
        );
    }
}
