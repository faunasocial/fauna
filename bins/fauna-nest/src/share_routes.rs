//! HTTP handlers for `GET /share/<token>` — serves a shared file by
//! decoding a signed ShareToken from the URL path, verifying it, and
//! reassembling the file from its content-addressed chunks — and for the
//! fragment-keyed private link's ciphertext-only arm (`GET /share/<token>`
//! answering the viewer page, `…/manifest`, `…/chunk/<i>`;
//! `docs/goal/behavior/share-links.md` § The private-file extension).
//!
//! Every arm runs the one [`admit`] prelude (signature, registry `410`,
//! author, succession, expiry, the public flag) before it reads anything.

use std::sync::Arc;

use crate::routes::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fauna_core::chunk::ChunkManifest;
use fauna_core::share::ShareToken;

/// A token the [`admit`] prelude let through.
struct Admitted {
    token: ShareToken,
    token_id: [u8; 32],
}

/// GET /share/{token}
///
/// Decodes a base64url-encoded ShareToken from the URL path, validates it,
/// and streams the reassembled file to the caller — or, for a token that
/// declares the fragment key, answers the viewer page and never a byte of the
/// file ([`private_viewer`]).
pub async fn handle_share(
    State(state): State<Arc<AppState>>,
    Path(token_b64): Path<String>,
) -> impl IntoResponse {
    let Admitted { token, token_id } = match admit(&state, &token_b64).await {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal,
    };

    // The fragment-keyed arm: a separate branch taken ONLY on a signed token
    // that declares the key, BEFORE the plaintext walk, and never handing
    // that walk a key. An undeclared token over a sealed manifest falls
    // through to the untouched 403 below.
    if token.key_in_fragment {
        return private_viewer(&state, &token, &token_id, &token_b64).await;
    }

    serve_plaintext(&state, &token).await
}

/// The gates every arm of `/share/{token}` runs before it reads a byte, in
/// order: the signature, the registry's revocation (`410`), the author's
/// registration, the succession gate (`410`), expiry (`410`), the public flag
/// (`403`). One copy, so the private arm cannot drift from the public one.
async fn admit(state: &AppState, token_b64: &str) -> Result<Admitted, Response> {
    // 1. Decode + verify the base64url token. `from_base64url` runs the
    //    sign-over-CID two-step (BLAKE3 of the inner bytes against the envelope
    //    CID, then Ed25519 over the CID with the token author's key), so a
    //    token that decodes here is already signature-verified.
    let token = match fauna_core::share::ShareToken::from_base64url(token_b64) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("share: token decode/verify error: {e}");
            return Err((StatusCode::BAD_REQUEST, "invalid token").into_response());
        }
    };

    // 2. Registry consult (Track E1 + row 89). The registry token-id is
    //    `blake3` of the canonical signed wire bytes — derived from the same
    //    base64url string `fauna.share.create` registered. One row fetch
    //    answers two gates: a revoked token is dead regardless of the other
    //    gates below (410), and the row's *presence* is the only thing a
    //    retired author's token may serve by (step 3b). For a live author an
    //    unregistered token still serves statelessly (registration is not a
    //    serving gate). A DB error fails closed — we don't serve a token whose
    //    registry state we can't confirm.
    let token_id = match fauna_core::share::token_id_from_base64url(token_b64) {
        Ok(token_id) => token_id,
        Err(e) => {
            // Unreachable in practice — `from_base64url` above already decoded
            // the same string — but handle it rather than panic.
            tracing::warn!("share: token id derivation error: {e}");
            return Err((StatusCode::BAD_REQUEST, "invalid token").into_response());
        }
    };
    let registration = match state.db.share_token_registration(&token_id).await {
        Ok(Some(true)) => return Err((StatusCode::GONE, "token revoked").into_response()),
        Ok(reg) => reg,
        Err(e) => {
            tracing::error!("share: db error checking registration: {e}");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response());
        }
    };

    // 3. Verify author is a registered actor on this nest.
    match state.db.is_actor_registered(&token.author.0).await {
        Ok(true) => {}
        Ok(false) => {
            return Err((StatusCode::FORBIDDEN, "author not registered").into_response());
        }
        Err(e) => {
            tracing::error!("share: db error checking actor registration: {e}");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response());
        }
    }

    // 3b. Succession gate: a token authored by a RETIRED identity
    //     serves only through its registry row. The refusal plane has one rule
    //     — the old key is refused everywhere — and an unregistered token's
    //     only authority is that key's bare signature, with a client-picked
    //     expiry that bounds nothing. A registered token's authority is its
    //     (ceremony-moved, successor-owned, revocable) `share_tokens` row, so
    //     it passed the consult above and keeps serving until the successor
    //     revokes it. The author's `users` row deliberately survives the
    //     ceremony, so step 3 cannot catch this. A DB error fails closed.
    //     Ruling: `succession-aftermath.md` § Re-key scope (2026-08-15).
    if registration.is_none() {
        match state.db.succession_for(&token.author.0).await {
            Ok(Some(_)) => {
                return Err((StatusCode::GONE, "token retired").into_response());
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("share: db error checking author succession: {e}");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response());
            }
        }
    }

    // 4. Check expiry.
    if token.is_expired() {
        return Err((StatusCode::GONE, "token expired").into_response());
    }

    // 5. A non-public token is REFUSED, not served.
    //
    //    This route is ratified as one unauthenticated surface: § Share's route
    //    table says "public, no auth", and § Caller classes lists `/share/*`
    //    under "Anyone (unauthenticated)". `public: false` was the header of a
    //    restricted-share feature that was never built — no audience field on
    //    the token, no app UI to set the flag, no minter outside tests. What
    //    stood here until now was a `FaunaIdentity` header check that compared
    //    the presented actor to *nothing*, so any caller passed it by generating
    //    a keypair: a `public: false` link was exactly as open as a public one,
    //    while reading like access control.
    //
    //    Restricted shares stay unbuilt, but they are now **born gated** rather
    //    than fake: the flag gates whether the token is servable at all, and
    //    `fauna.share.create` refuses to register one (`share_handlers.rs`), so
    //    the refusal lands at share time rather than as a dead link. Building
    //    the feature means adding a real audience to `ShareToken` plus the app
    //    UI that sets it (a user choice ⇒ app surface + nest state), and this
    //    arm becomes that audience check.
    if !token.public {
        return Err((
            StatusCode::FORBIDDEN,
            "restricted share links are not implemented — only public share links are served",
        )
            .into_response());
    }

    Ok(Admitted { token, token_id })
}

/// The public link's walk: the file's plaintext, or the authoritative `403`
/// on a sealed manifest.
async fn serve_plaintext(state: &AppState, token: &ShareToken) -> Response {
    // 6-9. Read the file via the shared manifest→chunks→decode→[decrypt]→verify
    //      walk (`web_content::file_bytes::read_file_by_manifest`) — the same
    //      walk `web_content::serve`/`service` use, so this route no longer
    //      re-implements it. A public share link carries no content key, so
    //      `content_keys` is always empty: an end-to-end-encrypted (content-key
    //      sealed) manifest surfaces as `FileOpenError::Sealed` below, which is
    //      the fail-closed 403 (INFO-2 — a matching gate at
    //      share-token mint, `share_handlers::create_handler`, is a defense-in-
    //      depth follow-on). **This 403 is the authoritative serve-side E2EE
    //      gate the security review depends on — never relocate or weaken it.**
    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => {
            return (StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response();
        }
    };
    let store = backup_svc.local_blob_store();
    let file_data = match crate::web_content::file_bytes::read_file_by_manifest(
        &state.db,
        &store,
        backup_svc.encryption_key(),
        &token.manifest_hash,
        &[],
    )
    .await
    {
        Ok(bytes) => bytes,
        // A share link is the most open consumer of the walk, and a share
        // token can be minted over a manifest that names any digest — so this
        // is the arm the legal-takedown withhold exists for. 451, no body, the
        // same answer the four routes onto the store give.
        Err(crate::web_content::file_bytes::FileOpenError::Withheld) => {
            return StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response();
        }
        Err(crate::web_content::file_bytes::FileOpenError::Sealed) => {
            return (
                StatusCode::FORBIDDEN,
                "share links are not available for end-to-end encrypted folders",
            )
                .into_response();
        }
        Err(crate::web_content::file_bytes::FileOpenError::MissingManifest(_)) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(e) => {
            tracing::error!("share: file open error: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response();
        }
    };

    // 10. Serve with appropriate Content-Type and Content-Disposition.
    let content_type = fauna_core::share::content_type_for_filename(&token.filename);
    let disposition = format!(
        "attachment; filename=\"{}\"",
        token.filename.replace('"', "\\\"")
    );

    (
        StatusCode::OK,
        [
            ("content-type", content_type),
            ("content-disposition", disposition.as_str()),
        ],
        file_data,
    )
        .into_response()
}

// ── The fragment-keyed private link: ciphertext only ────────────────────────
//
// `share-links.md` § The private-file extension, *What the nest serves*. Three
// answers, each behind the same [`admit`] prelude as the public arm, then
// [`private_manifest`]: the viewer page (a navigation — [`private_viewer`]),
// the manifest + the registered envelope ([`handle_share_manifest`]), and one
// ciphertext chunk by index ([`handle_share_chunk`]). The nest never holds the
// link key, never opens the envelope and never decrypts: every byte these arms
// return is a byte it already stored.

/// The private arm's shared gate after [`admit`]: the token must declare the
/// fragment key, carry a registered envelope, and name a SEALED manifest none
/// of whose blobs is under a legal-takedown withhold. Returns the envelope, the
/// manifest's own bytes (as uploaded — their hash is the token's
/// `manifest_hash`) and the parsed manifest.
async fn private_manifest(
    state: &AppState,
    token: &ShareToken,
    token_id: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>, ChunkManifest), Response> {
    use crate::web_content::file_bytes::{FileOpenError, fetch_decoded};

    // The sub-routes exist only for a private link. An undeclared token here is
    // not refused 403 but answered as the absent route it is: its one route is
    // `GET /share/{token}`, whose walk keeps the authoritative sealed 403.
    if !token.key_in_fragment {
        return Err(StatusCode::NOT_FOUND.into_response());
    }
    // A private link opens only through its registered envelope; an
    // unregistered declared token has none anywhere, so nothing can open it.
    let envelope = match state.db.share_token_key_envelope(token_id).await {
        Ok(Some(env)) => env,
        Ok(None) => return Err((StatusCode::NOT_FOUND, "link not registered").into_response()),
        Err(e) => {
            tracing::error!("share: db error reading key envelope: {e}");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response());
        }
    };
    let Some(backup_svc) = &state.backup_service else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response());
    };
    let store = backup_svc.local_blob_store();
    let hash = fauna_core::data::ContentHash::from_digest_raw(token.manifest_hash);
    let manifest_bytes =
        match fetch_decoded(&state.db, &store, backup_svc.encryption_key(), &hash).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Err(StatusCode::NOT_FOUND.into_response()),
            Err(FileOpenError::Withheld) => {
                return Err(StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response());
            }
            Err(e) => {
                tracing::error!("share: manifest read error: {e}");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response());
            }
        };
    let manifest: ChunkManifest = match fauna_core::encoding::canonical_decode(&manifest_bytes) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("share: private link names an undecodable manifest: {e}");
            return Err(StatusCode::NOT_FOUND.into_response());
        }
    };
    // A declared token over a plaintext manifest is refused: there is nothing
    // to key, and this arm never serves plaintext.
    if manifest.stored_hashes.is_none() {
        return Err((
            StatusCode::FORBIDDEN,
            "a private share link names a sealed file; this one is not",
        )
            .into_response());
    }
    // The public walk refuses the whole file when any blob it names is
    // withheld; so does this arm, on every answer — the viewer page included —
    // rather than serving a link that dies part-way through.
    for key in manifest.store_keys() {
        if crate::blob_routes::is_legally_withheld(&state.db, &key.digest()).await {
            return Err(StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response());
        }
    }
    Ok((envelope, manifest_bytes, manifest))
}

/// The viewer page's file in the SPA build (`apps/fauna-web`'s second build
/// entry, `vite.share-viewer.config.ts`), as a path of the `/app/` service.
const SHARE_VIEWER_ENTRY: &str = "/share-viewer.html";

/// `GET /share/{token}` for a private link: the viewer page, served exactly as
/// `/app/` serves the SPA — the same service (`web_app_origin::app_router`)
/// under the same security headers ([`crate::with_spa_headers`]), so the
/// admin's web-app-origin choice binds it too: bundled serves the SPA build's
/// page, central answers the same `302` with the path kept. The redirect names
/// no fragment, so the browser carries the link key across the hop and sends
/// it to no server. No byte of the file, and nothing about it, is in the page.
async fn private_viewer(
    state: &Arc<AppState>,
    token: &ShareToken,
    token_id: &[u8; 32],
    token_b64: &str,
) -> Response {
    if let Err(refusal) = private_manifest(state, token, token_id).await {
        return refusal;
    }
    let original: axum::http::Uri = match format!("/share/{token_b64}").parse() {
        Ok(uri) => uri,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid token").into_response(),
    };
    // The SPA service sees the build's viewer entry — `share-viewer.html`, a
    // page of its own outside the app shell's root layout, so nothing of the
    // app (identity store, account runtime, polls) runs where a link key sits
    // in the address bar (`share-links.md` rule 3). The page reads its link
    // off the address bar; a redirect reads the original path.
    let mut req = axum::http::Request::builder()
        .method(axum::http::Method::GET)
        .uri(SHARE_VIEWER_ENTRY)
        .body(axum::body::Body::empty())
        .expect("static request parts are valid");
    req.extensions_mut()
        .insert(axum::extract::OriginalUri(original));
    let mut svc = crate::with_spa_headers(crate::web_app_origin::app_router(
        state.config.nest.static_dir.as_deref(),
        crate::web_app_origin::live_probe(state.clone()),
    ));
    use tower_service::Service as _;
    match svc.call(req).await {
        Ok(resp) => resp.into_response(),
        Err(e) => match e {},
    }
}

/// Headers every private-arm data answer carries: opaque bytes, never sniffed
/// into something a browser renders, never cached past a revoke, and no
/// referrer out of a page that holds a key in its address bar.
const PRIVATE_DATA_HEADERS: [(&str, &str); 3] = [
    ("x-content-type-options", "nosniff"),
    ("cache-control", "no-store"),
    ("referrer-policy", "no-referrer"),
];

/// `GET /share/{token}/manifest` — a private link's manifest (the bytes the
/// token's signed `manifest_hash` names) and its registered key envelope, as
/// one canonical `ShareFragmentManifest`.
pub async fn handle_share_manifest(
    State(state): State<Arc<AppState>>,
    Path(token_b64): Path<String>,
) -> Response {
    let Admitted { token, token_id } = match admit(&state, &token_b64).await {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal,
    };
    let (envelope, manifest_bytes, _) = match private_manifest(&state, &token, &token_id).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let body =
        match fauna_protocol::encode_canonical(&fauna_protocol::share::ShareFragmentManifest {
            manifest: serde_bytes::ByteBuf::from(manifest_bytes),
            key_envelope: serde_bytes::ByteBuf::from(envelope),
            extra: Default::default(),
        }) {
            Ok(body) => body,
            Err(e) => {
                tracing::error!("share: encode fragment manifest: {e}");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
    (
        StatusCode::OK,
        [("content-type", "application/cbor")],
        PRIVATE_DATA_HEADERS,
        body,
    )
        .into_response()
}

/// `GET /share/{token}/chunk/{index}` — the `index`-th ciphertext chunk of a
/// private link's file, by its store key, exactly as the author uploaded it.
/// One content-addressed blob per answer, so nothing here sizes a buffer off
/// a manifest field.
pub async fn handle_share_chunk(
    State(state): State<Arc<AppState>>,
    Path((token_b64, index)): Path<(String, usize)>,
) -> Response {
    use crate::web_content::file_bytes::{FileOpenError, fetch_decoded};

    let Admitted { token, token_id } = match admit(&state, &token_b64).await {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal,
    };
    let (_, _, manifest) = match private_manifest(&state, &token, &token_id).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let Some(store_key) = manifest.store_keys().into_iter().nth(index) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(backup_svc) = &state.backup_service else {
        return (StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response();
    };
    let store = backup_svc.local_blob_store();
    match fetch_decoded(&state.db, &store, backup_svc.encryption_key(), &store_key).await {
        Ok(Some(ciphertext)) => (
            StatusCode::OK,
            [("content-type", "application/octet-stream")],
            PRIVATE_DATA_HEADERS,
            ciphertext,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(FileOpenError::Withheld) => StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response(),
        Err(e) => {
            tracing::error!("share: chunk read error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;

    // ── revocation-list check (Track E1) ────────────────────────────────────

    use crate::db::CacheDb;
    use crate::routes::AppState;
    use fauna_core::share::{ShareToken, token_id_from_base64url};
    use std::sync::Arc;

    /// Register `token_str` (minted by `kp`) in the share registry.
    async fn register(db: &CacheDb, kp: &ActorKeypair, token_str: &str) -> [u8; 32] {
        let token_id = token_id_from_base64url(token_str).unwrap();
        db.register_share_token(
            &token_id,
            &kp.actor_id().0,
            &[1u8; 32],
            b"sealed",
            i64::MAX,
            true,
        )
        .await
        .unwrap();
        token_id
    }

    /// **Finding closed — the replacement for the gate that
    /// authenticated nothing.** Until 2026-08-16 a `public: false` token was
    /// served to any caller who put a freshly-minted keypair's signature in a
    /// `FaunaIdentity` header, because the presented actor was compared to
    /// nothing. It is now refused outright: `/share/{token}` is one
    /// unauthenticated surface (`api-layers.md` § Share, "public, no auth") and
    /// restricted shares are born gated until a real audience + app UI exist.
    ///
    /// The token here is otherwise perfectly serviceable — registered author,
    /// unexpired, unrevoked — so only the flag can be producing the refusal.
    /// **No `Authorization` header is offered, and that is the point:** the
    /// header is no longer read, so there is nothing a caller can present.
    #[tokio::test]
    async fn a_non_public_token_is_refused_not_served() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        db.create_user(&kp.actor_id().0, "free", "author")
            .await
            .unwrap();
        let token = ShareToken::new([1u8; 32], kp.actor_id(), "a.txt".into(), u64::MAX, false);
        let token_str = token.to_base64url(&kp).unwrap();

        let state = Arc::new(AppState::for_test(db));
        let resp = handle_share(State(state), Path(token_str))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "a public: false token must be refused, never served — the flag now \
             gates whether the token is servable at all"
        );
    }

    /// The other side of the same fence: the identical token with the flag set
    /// gets past step 5. It fails later (no backup service configured on this
    /// bare test state), which is exactly what proves the flag — not some
    /// earlier gate — is what refused its twin above.
    #[tokio::test]
    async fn a_public_token_gets_past_the_flag_gate() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        db.create_user(&kp.actor_id().0, "free", "author")
            .await
            .unwrap();
        let token = ShareToken::new([1u8; 32], kp.actor_id(), "a.txt".into(), u64::MAX, true);
        let token_str = token.to_base64url(&kp).unwrap();

        let state = Arc::new(AppState::for_test(db));
        let resp = handle_share(State(state), Path(token_str))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "public: true reaches the serve path (and stops at the absent backup \
             service), so the 403 above is the flag and nothing else"
        );
    }

    #[tokio::test]
    async fn revoked_token_returns_410_gone() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        let token = ShareToken::new([1u8; 32], kp.actor_id(), "a.txt".into(), u64::MAX, true);
        let token_str = token.to_base64url(&kp).unwrap();
        let token_id = register(&db, &kp, &token_str).await;
        assert!(
            db.revoke_share_token(&token_id, &kp.actor_id().0)
                .await
                .unwrap()
        );

        let state = Arc::new(AppState::for_test(db));
        let resp = handle_share(State(state), Path(token_str))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn unrevoked_registered_token_is_not_410() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        let token = ShareToken::new([1u8; 32], kp.actor_id(), "a.txt".into(), u64::MAX, true);
        let token_str = token.to_base64url(&kp).unwrap();
        register(&db, &kp, &token_str).await; // registered, NOT revoked

        let state = Arc::new(AppState::for_test(db));
        let resp = handle_share(State(state), Path(token_str))
            .await
            .into_response();
        // Proceeds past the revocation check (author isn't a registered actor in
        // the for_test db → 403, not 410). The point: an unrevoked token is not
        // refused by the revocation check.
        assert_ne!(resp.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn unregistered_token_is_not_410() {
        // A token never registered in the control plane still serves statelessly
        // (registration is not a serving gate) — the revocation check is a no-op.
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        let token = ShareToken::new([1u8; 32], kp.actor_id(), "a.txt".into(), u64::MAX, true);
        let token_str = token.to_base64url(&kp).unwrap();

        let state = Arc::new(AppState::for_test(db));
        let resp = handle_share(State(state), Path(token_str))
            .await
            .into_response();
        assert_ne!(resp.status(), StatusCode::GONE);
    }

    // ── succession serve-side gate ─────────────────────────────────

    /// Build an `AppState` with a working blob store and one seeded plaintext
    /// file, returning the state and the file's manifest hash — so a serve
    /// assertion can be `200 OK` rather than "not 410". The returned `TempDir`
    /// must outlive the state (it holds the blob store).
    async fn state_with_plain_file(
        db: Arc<CacheDb>,
    ) -> (AppState, [u8; 32], Vec<u8>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let backup_svc = crate::backup::service::BackupService::new(
            db.clone(),
            None,
            false,
            tmp.path().to_path_buf(),
            None,
        )
        .unwrap();
        let store = backup_svc.local_blob_store();
        let content = b"holiday snapshot bytes".repeat(8);
        let manifest_hash =
            crate::web_content::file_bytes::seed_synced_file(&store, None, &content, None).await;
        let mut state = AppState::for_test(db);
        state.backup_service = Some(Arc::new(backup_svc));
        (state, manifest_hash, content, tmp)
    }

    /// An UNREGISTERED token authored by a succeeded identity is refused with
    /// `410 Gone`. This is the gate: registration is not a serving gate
    /// for a live author, but a ceremony retires the author's key everywhere,
    /// and an unregistered token's only authority is that key's bare signature
    /// — with a thief-chosen expiry, so "until its own expiry" bounds nothing
    /// (`succession-aftermath.md` § Re-key scope, ruled 2026-08-15).
    #[tokio::test]
    async fn unregistered_token_of_a_succeeded_author_is_410() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        let old = kp.actor_id();
        db.create_user(&old.0, "free", "test").await.unwrap();

        let (state, manifest_hash, _content, _tmp) = state_with_plain_file(db.clone()).await;
        let token = ShareToken::new(manifest_hash, old, "a.txt".into(), u64::MAX, true);
        let token_str = token.to_base64url(&kp).unwrap();

        // The ceremony: the author is retired. The token was never registered,
        // so no registry row exists for any verdict to move.
        db.record_succession(&old.0, &[0xB2u8; 32], b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let resp = handle_share(State(Arc::new(state)), Path(token_str))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::GONE,
            "an unregistered token signed by a retired key must not keep serving \
             — its only authority is the key the ceremony refused everywhere"
        );
    }

    /// A REGISTERED, unrevoked token still serves after the author's ceremony:
    /// its authority is the `share_tokens` row the ceremony moved to the
    /// successor (visible in `fauna.share.list`, revocable via
    /// `fauna.share.revoke`), not the retired key's signature. This is the pin
    /// that keeps the gate from regressing the move ruling.
    #[tokio::test]
    async fn registered_token_of_a_succeeded_author_still_serves() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        let old = kp.actor_id();
        db.create_user(&old.0, "free", "test").await.unwrap();

        let (state, manifest_hash, content, _tmp) = state_with_plain_file(db.clone()).await;
        let token = ShareToken::new(manifest_hash, old, "a.txt".into(), u64::MAX, true);
        let token_str = token.to_base64url(&kp).unwrap();
        register(&db, &kp, &token_str).await;

        db.record_succession(&old.0, &[0xB2u8; 32], b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let resp = handle_share(State(Arc::new(state)), Path(token_str))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a registered unrevoked token keeps serving across a succession — \
             the moved registry row is its authority, and the successor's \
             revoke is the kill switch"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), content.as_slice());
    }

    /// A token the predecessor had already revoked stays `410` after the
    /// ceremony — the serve-side half of the "already-revoked stays
    /// revoked" ruling, so the gate cannot resurrect anything.
    #[tokio::test]
    async fn revoked_token_of_a_succeeded_author_stays_410() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        let old = kp.actor_id();
        db.create_user(&old.0, "free", "test").await.unwrap();

        let (state, manifest_hash, _content, _tmp) = state_with_plain_file(db.clone()).await;
        let token = ShareToken::new(manifest_hash, old, "a.txt".into(), u64::MAX, true);
        let token_str = token.to_base64url(&kp).unwrap();
        let token_id = register(&db, &kp, &token_str).await;
        assert!(db.revoke_share_token(&token_id, &old.0).await.unwrap());

        db.record_succession(&old.0, &[0xB2u8; 32], b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let resp = handle_share(State(Arc::new(state)), Path(token_str))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::GONE);
    }

    /// An unregistered token of a LIVE author still serves — the gate
    /// keys on the author's succession state alone, so the stateless-serving
    /// wire contract for every shipped client is untouched.
    #[tokio::test]
    async fn unregistered_token_of_a_live_author_still_serves() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        let author = kp.actor_id();
        db.create_user(&author.0, "free", "test").await.unwrap();

        let (state, manifest_hash, content, _tmp) = state_with_plain_file(db.clone()).await;
        let token = ShareToken::new(manifest_hash, author, "a.txt".into(), u64::MAX, true);
        let token_str = token.to_base64url(&kp).unwrap();

        let resp = handle_share(State(Arc::new(state)), Path(token_str))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), content.as_slice());
    }

    // ── e2ee-sealed-manifest fail-closed gate (step 8b) ─────────────────────

    /// A public share link must never serve a content-key-sealed (E2EE) file:
    /// the nest holds no content key for it, so an unguarded reassembly would
    /// either 500 or (if it fell through) leak ciphertext. This characterizes
    /// today's behavior so a later refactor onto the shared
    /// `web_content::file_bytes::read_file_by_manifest` walk provably preserves it.
    #[tokio::test]
    async fn sealed_manifest_is_forbidden_not_served() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        let actor_id = kp.actor_id();
        db.create_user(&actor_id.0, "free", "test").await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let backup_svc = crate::backup::service::BackupService::new(
            db.clone(),
            None,
            false,
            tmp.path().to_path_buf(),
            None,
        )
        .unwrap();
        let store = backup_svc.local_blob_store();
        let content = b"members-only report".repeat(10);
        let content_key = [0xC1u8; 32];
        let manifest_hash = crate::web_content::file_bytes::seed_synced_file(
            &store,
            None,
            &content,
            Some(&content_key),
        )
        .await;

        let mut state = AppState::for_test(db);
        state.backup_service = Some(Arc::new(backup_svc));

        let token = ShareToken::new(
            manifest_hash,
            actor_id,
            "report.html".into(),
            u64::MAX,
            true,
        );
        let token_str = token.to_base64url(&kp).unwrap();

        let resp = handle_share(State(Arc::new(state)), Path(token_str))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "a public share link must refuse a content-key-sealed manifest, never serve or 500 it"
        );
    }
}
