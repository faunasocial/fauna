//! `SealedStorage` — the nest's one and only `Storage` impl.
//!
//! The storage-mode axis was retired in Phase 4
//! (`docs/goal/architecture/nest/storage-modes.md`). Every nest holds user
//! content sealed at rest and reads it nowhere:
//! - Ingest **verifies seal shape** per audience class and rejects what doesn't
//!   conform; it never classifies content (classification runs at a capability
//!   position — the perimeter bridge pre-seal, the user's client post-decrypt,
//!   or a holder of a user-minted grant; `architecture/content-scoring.md`
//!   § The placement matrix).
//! - `search` serves the **floor-derived** corpus only (public post bodies,
//!   restricted-post public previews, profiles). Search over sealed content
//!   runs at a capability position against the sealed `__index` segments.
//! - ACME TLS material is written to the local `acme_dir` for the nest's own
//!   listener, AND each approved bridge service-user with an x25519 pubkey
//!   receives a `TlsCertBlob` sealed to that pubkey so the bridge can unwrap
//!   it when it next connects.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::db::CacheDb;
use crate::storage::{
    AcmeMaterial, BlobIngestItem, BlobIngestOutcome, BlobIngestVerdict, ChannelEnvelopeIngestItem,
    ChannelEnvelopeIngestOutcome, ChannelEnvelopeIngestVerdict, PostIngestItem, PostIngestOutcome,
    PostIngestVerdict, SearchHit, SearchSpec, Storage, StorageUnavailable,
};

/// The nest's storage implementation: sealed at rest, verify-on-ingest,
/// floor-derived search.
///
/// Fields are an `Arc<CacheDb>` for bridge-blob persistence + the floor-derived
/// search projection, and a `PathBuf` for the ACME-issued TLS certificate
/// directory.
pub struct SealedStorage {
    db: Arc<CacheDb>,
    acme_dir: PathBuf,
}

impl std::fmt::Debug for SealedStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedStorage")
            .field("acme_dir", &self.acme_dir)
            .finish_non_exhaustive()
    }
}

impl SealedStorage {
    /// Create a `SealedStorage`.
    ///
    /// `db` is used for bridge-blob persistence (`put_tls_cert_blob`,
    /// `list_approved_bridge_service_users`) and the floor-derived search
    /// projection (`search_with_scoping`). `acme_dir` is the directory where
    /// `fullchain.pem` / `privkey.pem` are written for the nest's own TLS
    /// listener.
    pub fn new(db: Arc<CacheDb>, acme_dir: PathBuf) -> Self {
        Self { db, acme_dir }
    }
}

// ── Storage impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl Storage for SealedStorage {
    /// Full-text search over the floor-derived projection — the corpus every
    /// nest may read, on every nest (`storage-modes.md` § What replaced each
    /// piece of the axis).
    ///
    /// `content_fts` is fed by exactly two writers, both floor/plaintext-by-
    /// design: the post projection (`CacheDb::put_post{,_index_only}` →
    /// `insert_and_index`, whose body field is `Post::body_text()` — the
    /// **public preview** when the post is gated, per `fauna_core::data::Post`)
    /// and `fts::sync_profile_row` (the handle `users` holds now). Nothing sealed-derived reaches it,
    /// so serving it needs no capability and leaks nothing. `search_with_scoping`
    /// still applies its own access scoping (public content to all; a caller's
    /// own bridge-sourced rows only to them).
    async fn search(
        &self,
        actor: &[u8; 32],
        spec: &SearchSpec,
    ) -> Result<Vec<SearchHit>, StorageUnavailable> {
        let results = self
            .db
            .search_with_scoping(
                &spec.query,
                actor,
                spec.content_type.as_deref(),
                spec.before,
                spec.after,
                spec.limit,
                spec.offset,
            )
            .await
            .map_err(|e| StorageUnavailable::internal(format!("search error: {e}")))?;

        Ok(results
            .into_iter()
            .map(|r| SearchHit {
                content_type: r.content_type,
                content_id: r.content_id,
                created_at: r.created_at,
                rank: r.rank,
                snippet: r.snippet,
            })
            .collect())
    }

    /// Write the nest's own listener PEM and fan-out wrapped `TlsCertBlob`s to
    /// every approved bridge that has an x25519 pubkey.
    ///
    /// Bridges without an x25519 pubkey are skipped with a `tracing::warn!` —
    /// this eager fan-out is a pre-seed/cache. The authoritative delivery is
    /// `seal_current_tls_cert_for_bridge` on the fetch path, which seals the
    /// on-disk cert to the requesting bridge regardless of whether it had
    /// attested at issuance time, so a skip here is never load-bearing.
    ///
    /// Any error from DB or crypto is mapped to `StorageUnavailable::internal`.
    async fn store_acme_material(&self, m: &AcmeMaterial<'_>) -> Result<(), StorageUnavailable> {
        // (a) Write the nest's own TLS listener PEM atomically.
        crate::storage::write_acme_pem_atomic(&self.acme_dir, m.cert_chain_pem, m.priv_key_pem)
            .map_err(|e| StorageUnavailable::internal(format!("write acme PEM: {e}")))?;

        // (b) Fan-out a wrapped TlsCertBlob to each approved bridge that has an
        //     x25519 pubkey.
        let approved = self
            .db
            .list_approved_bridge_service_users()
            .await
            .map_err(|e| {
                StorageUnavailable::internal(format!("list approved bridge service users: {e}"))
            })?;

        let now_unix = fauna_core::data::Timestamp::now_secs_or_zero() as u64;

        for bridge in &approved {
            let x25519_pk = match bridge.x25519_pubkey {
                Some(pk) => pk,
                None => {
                    tracing::warn!(
                        role = bridge.role.as_str(),
                        bridge_id = %bridge.bridge_id,
                        "SealedStorage::store_acme_material: approved bridge has no x25519 pubkey — \
                         skipping eager seal; it receives the cert seal-on-read at its next \
                         fetch_tls_cert_blob once attested"
                    );
                    continue;
                }
            };

            let blob_bytes = crate::storage::seal_tls_cert_blob_bytes(
                bridge.role.as_str(),
                &bridge.bridge_id,
                m.domain,
                &x25519_pk,
                m.cert_chain_pem,
                m.priv_key_pem,
                now_unix,
            )?;

            self.db
                .put_tls_cert_blob(
                    bridge.role.as_str(),
                    &bridge.bridge_id,
                    m.domain,
                    &blob_bytes,
                )
                .await
                .map_err(|e| {
                    StorageUnavailable::internal(format!(
                        "put_tls_cert_blob for {}/{}: {e}",
                        bridge.role.as_str(),
                        bridge.bridge_id
                    ))
                })?;
        }

        Ok(())
    }

    async fn seal_current_tls_cert_for_bridge(
        &self,
        role: &str,
        bridge_id: &str,
        domain: &str,
    ) -> Result<Option<Vec<u8>>, StorageUnavailable> {
        crate::storage::seal_current_tls_cert_for_bridge_impl(
            &self.db,
            &self.acme_dir,
            role,
            bridge_id,
            domain,
        )
        .await
    }

    async fn seal_current_tls_cert_for_x25519(
        &self,
        role: &str,
        bridge_id: &str,
        domain: &str,
        x25519_pk: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, StorageUnavailable> {
        crate::storage::seal_current_tls_cert_for_x25519_impl(
            &self.acme_dir,
            role,
            bridge_id,
            domain,
            x25519_pk,
        )
    }

    async fn restore_real_tls_cert(
        &self,
    ) -> Result<crate::storage::RestoreTlsMethod, StorageUnavailable> {
        crate::storage::restore_real_tls_cert_at(&self.acme_dir)
            .map_err(|e| StorageUnavailable::internal(format!("restore real TLS cert: {e}")))
    }

    /// Blob ingest: per-class envelope-shape verifier dispatching
    /// on `sidecar.class`. Sidecar fields (`mime`, `has_c2pa`, `thumbnail_hash`)
    /// populate `BlobIngestOutcome`. On accept the verdict drives
    /// `nest_blob_ingest_total{verdict=accepted}`; a per-class rule
    /// violation returns `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Blob*))`
    /// → HTTP 400 (strict, matching the post + channel pipelines —
    /// tracked internally).
    ///
    /// The nest never renders a thumbnail or strips EXIF: it cannot read the
    /// bytes. Media processing is uploader-side (`fauna_media::process`, which
    /// every app runs), and the companion thumbnail is uploaded separately
    /// as its own opaque sealed blob keyed by its BLAKE3 — only the routing
    /// hash travels in the sidecar.
    ///
    /// `sidecar = None` (the retired `application/octet-stream` shape) is
    /// rejected with `IngestRejectReason::BlobSidecarAbsent`: without the
    /// audience class the verifier can't dispatch, so the seal can't be
    /// checked. The route already 400s a non-multipart body before reaching
    /// here; this arm is the trait-level self-defense (also exercised by unit
    /// tests calling `ingest_blob` directly).
    async fn ingest_blob(
        &self,
        item: &BlobIngestItem<'_>,
    ) -> Result<BlobIngestOutcome, StorageUnavailable> {
        let sc = item.sidecar.as_ref().ok_or_else(|| {
            StorageUnavailable::ingest_rejected(
                crate::storage::IngestRejectReason::BlobSidecarAbsent,
            )
        })?;
        classify_per_class_envelope(sc.class, item.body, sc)
            .map_err(StorageUnavailable::ingest_rejected)?;
        Ok(BlobIngestOutcome {
            verdict: BlobIngestVerdict::Accepted,
            stored_bytes: bytes::Bytes::copy_from_slice(item.body),
            mime: sc.mime.clone(),
            has_c2pa: Some(sc.has_c2pa),
            thumbnail_hash: sc.thumbnail_hash,
        })
    }

    /// Post ingest: verify the post's BARE shape, signature, and
    /// gated-info consistency; reject with
    /// `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Post*))` on
    /// failure (strict — per the upload-sidecar wire design, tracked
    /// internally).
    async fn ingest_post(
        &self,
        item: &PostIngestItem<'_>,
    ) -> Result<PostIngestOutcome, StorageUnavailable> {
        classify_encrypted_post(item.body, &item.uploader)
            .map(|()| PostIngestOutcome {
                verdict: PostIngestVerdict::Accepted,
            })
            .map_err(StorageUnavailable::ingest_rejected)
    }

    /// Channel-envelope ingest: dag-cbor-decode the envelope, check
    /// the inner bytes for AEAD shape, and check the sender is a member of the
    /// channel in `actor_channels`; reject with
    /// `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Channel*))`
    /// on failure (strict — per the upload-sidecar wire design, tracked
    /// internally).
    async fn ingest_channel_envelope(
        &self,
        item: &ChannelEnvelopeIngestItem<'_>,
    ) -> Result<ChannelEnvelopeIngestOutcome, StorageUnavailable> {
        classify_envelope_shape(item.body).map_err(StorageUnavailable::ingest_rejected)?;
        let in_channel = self
            .db
            .is_actor_in_channel(&item.sender, &item.channel_id)
            .await
            .map_err(|e| {
                tracing::warn!(
                    sender = %hex::encode(item.sender),
                    channel = %hex::encode(item.channel_id),
                    "SealedStorage::ingest_channel_envelope: is_actor_in_channel failed: {e}"
                );
                StorageUnavailable::ingest_rejected(
                    crate::storage::IngestRejectReason::ChannelRosterMiss,
                )
            })?;
        if !in_channel {
            return Err(StorageUnavailable::ingest_rejected(
                crate::storage::IngestRejectReason::ChannelRosterMiss,
            ));
        }
        Ok(ChannelEnvelopeIngestOutcome {
            verdict: ChannelEnvelopeIngestVerdict::Accepted,
        })
    }
}

/// Per-class envelope-shape verifier. Dispatches on `class` to one of two
/// rule families (table in the upload-sidecar wire design, tracked
/// internally, § Per-class envelope-shape verifier). Returns `Ok(())` on accept or
/// `Err(IngestRejectReason::Blob*)` on rejection (strict — mirrors
/// `classify_encrypted_post` / `classify_envelope_shape`):
///
/// - `Library` / `Conversation` / `GroupRestrictedPost` / `PeriodRestrictedPost`:
///   AEAD-shaped bytes (length≥28, no plaintext magic) + sidecar cross-check
///   (`mime == "application/octet-stream"` and `has_c2pa == false`).
/// - `PublicPost`: bytes pass through (length≥1; any magic allowed since the
///   post's signature attests to the blob hash); sidecar MIME must be a
///   non-empty `type/subtype`.
fn classify_per_class_envelope(
    class: fauna_media::audience::AudienceClass,
    body: &[u8],
    sidecar: &fauna_media::sidecar::UploadSidecar,
) -> Result<(), crate::storage::IngestRejectReason> {
    use crate::storage::IngestRejectReason;
    use fauna_media::audience::AudienceClass::*;
    match class {
        Library | Conversation | GroupRestrictedPost | PeriodRestrictedPost => {
            // AEAD-sealed bytes: length floor + no plaintext magic prefix.
            if body.len() < 28 {
                return Err(IngestRejectReason::BlobLengthFloor);
            }
            if has_plaintext_magic_prefix(body) {
                return Err(IngestRejectReason::BlobPlaintextMagicPrefix);
            }
            // Sidecar cross-check: MIME must be octet-stream; has_c2pa must be
            // false (the C2PA flag rides inside the sealed bytes).
            if sidecar.mime != "application/octet-stream" {
                return Err(IngestRejectReason::BlobMimeClassMismatch);
            }
            if sidecar.has_c2pa {
                return Err(IngestRejectReason::BlobMimeClassMismatch);
            }
            Ok(())
        }
        PublicPost => {
            // Plaintext bytes — the post's signature attests to the blob hash.
            // Length floor = 1; bytes MAY match plaintext magic prefixes.
            if body.is_empty() {
                return Err(IngestRejectReason::BlobLengthFloor);
            }
            // MIME must be non-empty type/subtype.
            if sidecar.mime.is_empty() || !sidecar.mime.contains('/') {
                return Err(IngestRejectReason::BlobMimeEmptyPublicPost);
            }
            Ok(())
        }
    }
}

/// Delegates to `fauna_media::process::sniff_known_prefix` — the single
/// source of truth for "does this byte prefix belong to a known plaintext
/// container" shared with `Container::sniff`'s MIME classification. Prior to
/// this, this gate hand-rolled its own narrower literal table and missed
/// ISO-BMFF (HEIC/AVIF/MP4/MOV) and Matroska entirely, so a sealed-blob
/// envelope whose plaintext happened to start with one of those signatures
/// would have passed the AEAD-shape sanity check undetected.
fn has_plaintext_magic_prefix(body: &[u8]) -> bool {
    fauna_media::process::sniff_known_prefix(body).is_some()
}

/// Strict structural verification of an incoming `ChannelEnvelope`.
/// Strict-decodes the canonical dag-cbor wrapper and runs the same AEAD-shape
/// sanity check as the
/// blob pipeline (length floor + no plaintext-content magic prefix) on the
/// inner variant bytes. Returns `Ok(())` on accept or `Err(IngestRejectReason::Channel*)`
/// on rejection. Roster membership is checked separately by the caller.
fn classify_envelope_shape(body: &[u8]) -> Result<(), crate::storage::IngestRejectReason> {
    use crate::storage::IngestRejectReason;
    use fauna_mls::types::ChannelEnvelope;
    let envelope =
        ChannelEnvelope::from_bytes(body).map_err(|_| IngestRejectReason::ChannelDecode)?;
    let inner: &[u8] = match &envelope {
        ChannelEnvelope::Application(b) | ChannelEnvelope::Commit(b) => b.as_slice(),
        // A community room's envelope is sealed under the room's generation
        // key rather than an MLS ratchet, but the shape check is the same
        // one and for the same reason: it is an AEAD output, so the length
        // floor and the plaintext-magic screen apply to the ciphertext
        // exactly as they do to an MLS message's.
        ChannelEnvelope::RoomSealed { ciphertext, .. } => ciphertext.as_slice(),
        // A community room's floor delete record is the one conversation
        // envelope that is NOT an AEAD output, by design: it rides unsealed so
        // the floor can judge it without reading anything, and it names no
        // content — a room, a log position, an author, a policy version
        // (`conversation-rooms.md` § Roles and authorization → *Delete any
        // message — the mechanism* → *Community rooms*). So its shape check is
        // the record's own strict decode, never the ciphertext screen; who may
        // file one is the floor gate's judgment, made before this is reached.
        ChannelEnvelope::RoomFloorDelete(record) => {
            return fauna_mls::room_policy::SignedRoomFloorDelete::from_bytes(record)
                .map(|_| ())
                .map_err(|_| IngestRejectReason::ChannelDecode);
        }
    };
    if inner.len() < 28 {
        return Err(IngestRejectReason::ChannelAeadShape);
    }
    if has_plaintext_magic_prefix(inner) {
        return Err(IngestRejectReason::ChannelAeadShape);
    }
    Ok(())
}

/// Strict structural verification of an incoming `Post` in its `EmbedAsBytes`
/// wire shape (sign-over-CID — see `docs/goal/architecture/serialization.md`
/// § Sign-over-CID). Decodes the BARE wire as `EmbedAsBytes`, extracts the
/// inner signed bytes + envelope, decodes the inner as `Post`, verifies the
/// envelope, and — if gated — checks `encrypted_ref` non-zero and the
/// `key_access` map is internally consistent. Returns `Ok(())` on accept
/// or `Err(IngestRejectReason::Post*)` on rejection.
///
/// **Own-write:** `post.author` must be `uploader`, the authenticated
/// connection. The signature binds the bytes to
/// `post.author`; this binds `post.author` to the caller, as
/// `profile_handlers.rs` does for a profile and the delete door does for a
/// tombstone. Without it any user could create another's validly signed
/// post, and everything downstream keyed on the caller — the forward
/// queue's author stamp (which the author's deletion purges by), the bridge
/// fan-out, the engagement rows — would attribute it to the wrong person.
fn classify_encrypted_post(
    body: &[u8],
    uploader: &[u8; 32],
) -> Result<(), crate::storage::IngestRejectReason> {
    use crate::storage::IngestRejectReason;
    use fauna_core::data::{Capability, Post};
    use fauna_core::encoding::{
        EmbedAsBytes, canonical_decode, decode_signed_bytes, verify_authoring_envelope,
    };
    use fauna_core::subscription::types::KeyAccess;

    let wire: EmbedAsBytes = canonical_decode(body).map_err(|_| IngestRejectReason::PostDecode)?;
    // The delegation cert (D10) rides in `signer_auth`; capture it before
    // `into_signed()` consumes the wire.
    let signer_auth = wire.signer_auth.clone();
    let (post_bytes, post_env) = wire
        .into_signed()
        .map_err(|_| IngestRejectReason::PostDecode)?;
    let post: Post =
        decode_signed_bytes(&post_bytes).map_err(|_| IngestRejectReason::PostDecode)?;

    // Accept the author's own signature or a delegated authoring sub-key's
    // (`atproto-pds-full.md` D10). Fail-closed: a post signed by a sub-key with
    // no accompanying identity-signed `Capability::Post` cert is rejected.
    if verify_authoring_envelope(
        &post,
        &post_bytes,
        &post_env,
        signer_auth.as_deref(),
        &Capability::Post,
        post.created_at,
    )
    .is_err()
    {
        return Err(IngestRejectReason::PostSignatureMismatch);
    }
    if post.author.0 != *uploader {
        return Err(IngestRejectReason::PostUploaderNotAuthor);
    }

    crate::storage::reject_future_created_at(post.created_at)?;

    if let Some(ref gated) = post.gated {
        if gated.encrypted_ref.digest() == [0u8; 32] {
            return Err(IngestRejectReason::PostGatedRefZero);
        }
        match &gated.key_access {
            KeyAccess::Room {
                group_id,
                generation,
                ..
            } => {
                if group_id.0.is_empty() {
                    return Err(IngestRejectReason::PostKeyAccessInconsistent);
                }
                // A generation id is a community room's, and a room is named
                // by its 32-byte channel id (`ui/feed.md` § Encryption at rest
                // → *Room-restricted*): a generation beside anything else names
                // no room a reader — or this nest's reception pass — could
                // resolve. The id's own length is the type's.
                if generation.is_some() && group_id.0.len() != 32 {
                    return Err(IngestRejectReason::PostKeyAccessInconsistent);
                }
            }
            KeyAccess::Broadcast { key_blob_ref } => {
                if key_blob_ref.digest() == [0u8; 32] {
                    return Err(IngestRejectReason::PostKeyAccessInconsistent);
                }
            }
            // An arm a newer app authored and this nest has never heard of
            // (rule 3's fallthrough). Stored like any gated post: this nest
            // holds no key for any of them, so there is nothing arm-specific
            // to check beyond the signed envelope above — and refusing it
            // would make a newer app's post uncreatable here for no reason
            // this nest could state.
            KeyAccess::Unknown { .. } => {}
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::IngestRejectReason;
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use fauna_core::identity::ActorKeypair;

    /// A post authored by `identity` but signed by the delegated `sub`,
    /// optionally carrying the identity-signed `Capability::Post` cert in
    /// `signer_auth` — the D10 external-app-authoring wire.
    fn delegated_post_wire(
        identity: &ActorKeypair,
        sub: &ActorKeypair,
        with_cert: bool,
    ) -> Vec<u8> {
        let post_wire =
            fauna_client_core::post::build_post(identity, "delegated", &[], None).unwrap();
        let (post, _) = fauna_client_core::post::decode_post(&post_wire).unwrap();
        let (bytes, env) = sign_envelope(sub, &post).unwrap();
        let mut wire = EmbedAsBytes::from_signed(bytes, env);
        if with_cert {
            let da = DeviceAuthorization {
                actor_id: identity.actor_id(),
                device_key: sub.actor_id().0,
                capabilities: vec![Capability::Post],
                created_at: Timestamp(1),
                expires_at: None,
            };
            let (cb, ce) = sign_envelope(identity, &da).unwrap();
            wire = wire.with_signer_auth(EmbedAsBytes::from_signed(cb, ce));
        }
        canonical_encode(&wire).unwrap()
    }

    #[test]
    fn classify_accepts_direct_and_valid_delegated_post_and_rejects_uncertified() {
        let identity = ActorKeypair::from_secret([3u8; 32]);
        let sub = ActorKeypair::from_secret([4u8; 32]);

        // The author's own signature — the direct path — is unchanged.
        let direct = fauna_client_core::post::build_post(&identity, "hi", &[], None).unwrap();
        assert!(classify_encrypted_post(&direct, &identity.actor_id().0).is_ok());

        // A delegated post carrying a valid Capability::Post cert is accepted.
        assert!(
            classify_encrypted_post(
                &delegated_post_wire(&identity, &sub, true),
                &identity.actor_id().0
            )
            .is_ok()
        );

        // Fail-closed: the same sub-key signature with NO cert is rejected at
        // the ingest gate (never silently ingested as an unverified post).
        assert!(matches!(
            classify_encrypted_post(
                &delegated_post_wire(&identity, &sub, false),
                &identity.actor_id().0
            ),
            Err(IngestRejectReason::PostSignatureMismatch)
        ));
    }

    /// Own-write: a validly signed post is refused when anyone but its author
    /// uploads it — a stranger, or the delegated sub-key's own principal (the
    /// post is the identity's, not the sub-key's).
    #[test]
    fn classify_refuses_a_post_whose_author_is_not_the_uploader() {
        let identity = ActorKeypair::from_secret([3u8; 32]);
        let sub = ActorKeypair::from_secret([4u8; 32]);
        let stranger = ActorKeypair::from_secret([5u8; 32]);

        let direct = fauna_client_core::post::build_post(&identity, "hi", &[], None).unwrap();
        assert!(matches!(
            classify_encrypted_post(&direct, &stranger.actor_id().0),
            Err(IngestRejectReason::PostUploaderNotAuthor)
        ));
        assert!(matches!(
            classify_encrypted_post(
                &delegated_post_wire(&identity, &sub, true),
                &sub.actor_id().0
            ),
            Err(IngestRejectReason::PostUploaderNotAuthor)
        ));
    }
}
