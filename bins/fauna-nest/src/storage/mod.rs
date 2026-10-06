//! The nest's storage layer. Byte storage itself lives on `CacheDb` /
//! `BlobStoreBackend`; this trait owns the operations that need a policy
//! decision at the storage boundary: seal-shape verification at ingest,
//! floor-derived server-side search, and ACME-issued-TLS handling.
//!
//! **There is one implementation** ([`SealedStorage`]) — the storage-mode axis
//! was retired in Phase 4 (`docs/goal/architecture/nest/storage-modes.md`).
//! Every nest stores user content sealed at rest; reading happens only at
//! capability positions (the AUTH'd MDA session, the user's client, or a holder
//! of a user-minted grant), never in the nest process. The classify / FTS-body /
//! thumbnail arms that used to run "because the box is trusted" are gone with
//! the axis, not gated behind a flag.

pub mod sealed;

pub use sealed::SealedStorage;

use async_trait::async_trait;
use std::sync::Arc;

/// Why an ingest was rejected. Maps to the `reason` label on
/// `nest_{post,channel,blob}_ingest_total{verdict,reason}` and to the
/// snake_case message body of the 400 response.
///
/// Post + channel variants landed in the strict flip from the upload-sidecar
/// wire design (tracked internally). Blob variants landed in the strict flip
/// once all six per-app upload-sidecar tracks closed (tracked internally)
/// → the blob pipeline now rejects rather than permissively stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestRejectReason {
    PostDecode,
    PostSignatureMismatch,
    PostGatedRefZero,
    PostKeyAccessInconsistent,
    ChannelDecode,
    ChannelAeadShape,
    ChannelRosterMiss,
    /// Encrypted-mode blob upload arrived without the `UploadSidecar`
    /// (legacy `application/octet-stream` shape). The verifier can't dispatch
    /// without the audience class, so the seal can't be checked → reject.
    BlobSidecarAbsent,
    /// Sealed-audience blob bytes start with a recognizable plaintext content
    /// magic prefix (PNG / JPEG / PDF / GIF / WebP) — a clear sign the uploader
    /// didn't seal.
    BlobPlaintextMagicPrefix,
    /// Blob bytes are shorter than the ChaCha20-Poly1305 output floor
    /// (12-byte nonce + 16-byte tag = 28 bytes), or empty for a public post.
    BlobLengthFloor,
    /// The sidecar's `mime` / `has_c2pa` cross-check failed for a sealed
    /// audience class (`mime` must be `application/octet-stream`, `has_c2pa`
    /// must be false — the real values ride inside the sealed bytes).
    BlobMimeClassMismatch,
    /// A `PublicPost` blob's sidecar `mime` was empty / not `type/subtype`.
    BlobMimeEmptyPublicPost,
    /// A post's remote-asserted `created_at` sits further ahead of this nest's
    /// own clock than its door's cushion allows
    /// ([`POST_CREATED_AT_MAX_FUTURE_SKEW_MICROS`] for a Fauna-native post) —
    /// a remote party future-dating a post to pin it atop the chronological
    /// feed (`docs/goal/ui/feed.md` § The read model). The past is never
    /// bounded: backdating and archive imports
    /// (`docs/goal/behavior/archive-import.md` § What each category becomes)
    /// sort exactly where their `created_at` says.
    PostCreatedAtInFuture,
    /// The post is validly signed, but its `author` is not the connection
    /// that uploaded it: a caller may create only its OWN posts — the profile
    /// door's own-write rule. A delegated sub-key post passes, because its
    /// `author` is the identity the cert chain ties the sub-key to.
    PostUploaderNotAuthor,
}

impl IngestRejectReason {
    /// Stable snake_case label for the metric `reason` field and for the
    /// HTTP 400 message body (`ingest rejected: <label>`).
    pub fn as_snake_case(&self) -> &'static str {
        match self {
            Self::PostDecode => "post_decode",
            Self::PostSignatureMismatch => "post_signature_mismatch",
            Self::PostGatedRefZero => "post_gated_ref_zero",
            Self::PostKeyAccessInconsistent => "post_key_access_inconsistent",
            Self::ChannelDecode => "channel_decode",
            Self::ChannelAeadShape => "channel_aead_shape",
            Self::ChannelRosterMiss => "channel_roster_miss",
            Self::BlobSidecarAbsent => "blob_sidecar_absent",
            Self::BlobPlaintextMagicPrefix => "blob_plaintext_magic_prefix",
            Self::BlobLengthFloor => "blob_length_floor",
            Self::BlobMimeClassMismatch => "blob_mime_class_mismatch",
            Self::BlobMimeEmptyPublicPost => "blob_mime_empty_public_post",
            Self::PostCreatedAtInFuture => "post_created_at_in_future",
            Self::PostUploaderNotAuthor => "post_uploader_not_author",
        }
    }
}

/// How far ahead of this nest's own clock a Fauna-native post's `created_at`
/// may sit before its door refuses it. Generous cushion for honest clock drift
/// (mirrors [`crate::oauth_as_gates::DPOP_PROOF_MAX_SKEW_SECS`]'s reasoning),
/// not a compatibility window — there is no bound at all going into the past.
pub const POST_CREATED_AT_MAX_FUTURE_SKEW_MICROS: u64 = 120_000_000; // 2 minutes

/// The same cushion for a bridged foreign-protocol plane, where the value is a
/// remote server's or client's clock and a refusal is silent to its author:
/// the hour the ActivityPub inbox already allows a peer's signed `Date`.
/// Wider on purpose — a tight cushion would silently drop an honest peer whose
/// clock drifts, and an hour still ends the pin the bound exists to stop.
#[cfg(any(feature = "activitypub", feature = "nostr", feature = "bluesky"))]
pub const BRIDGED_CREATED_AT_MAX_FUTURE_SKEW_MICROS: u64 = 3_600_000_000; // 1 hour

/// Refuse a Fauna-native post — one a Fauna author's client signed — whose
/// `created_at` sits more than [`POST_CREATED_AT_MAX_FUTURE_SKEW_MICROS`] ahead
/// of this nest's own clock.
///
/// `content.created_at` is the column the local feed sorts on, so every door
/// that writes a remote-asserted value into it applies this bound, or the
/// bridged planes' `reject_future_bridged_created_at`, first. Which door
/// applies which is deliberately not a list kept here — a list in this comment
/// once named two doors while three more shipped outside it.
/// `tests::every_content_created_at_writer_is_partitioned` enrols every
/// production writer of the column and holds each bounded door to its call, so
/// a new door fails there until someone classifies it
/// (`docs/goal/ui/feed.md` § The read model).
pub(crate) fn reject_future_created_at(
    created_at: fauna_core::data::Timestamp,
) -> Result<(), IngestRejectReason> {
    refuse_ahead_of_now(created_at, POST_CREATED_AT_MAX_FUTURE_SKEW_MICROS)
}

/// [`reject_future_created_at`] for a bridged foreign-protocol plane — the
/// ActivityPub inbox's `Create`/`Update{Note}`, the Nostr sweep and the
/// Bluesky feed ingest — at [`BRIDGED_CREATED_AT_MAX_FUTURE_SKEW_MICROS`].
#[cfg(any(feature = "activitypub", feature = "nostr", feature = "bluesky"))]
pub(crate) fn reject_future_bridged_created_at(
    created_at: fauna_core::data::Timestamp,
) -> Result<(), IngestRejectReason> {
    refuse_ahead_of_now(created_at, BRIDGED_CREATED_AT_MAX_FUTURE_SKEW_MICROS)
}

/// The one comparison both bounds share, made in `u64` — the domain
/// `Timestamp` lives in. Narrowing to `i64` first wraps every value past
/// `i64::MAX` negative, and a negative instant is ahead of nothing.
fn refuse_ahead_of_now(
    created_at: fauna_core::data::Timestamp,
    cushion_micros: u64,
) -> Result<(), IngestRejectReason> {
    let latest = fauna_core::data::Timestamp::now()
        .0
        .saturating_add(cushion_micros);
    if created_at.0 > latest {
        return Err(IngestRejectReason::PostCreatedAtInFuture);
    }
    Ok(())
}

/// Hard error returned by a storage operation. The `kind` is the stable tag
/// call sites map to an HTTP status / IMAP response; `reason` is for logs.
#[derive(Debug, Clone)]
pub struct StorageUnavailable {
    pub kind: StorageUnavailableKind,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageUnavailableKind {
    /// An internal failure while performing a storage operation (e.g. a DB
    /// or crypto error during ACME-cert wrapping). Maps to HTTP 500.
    Internal,
    /// The strict per-class verifier rejected this ingest. Carries the
    /// specific reject reason for metric + 400-body emission.
    IngestRejected(IngestRejectReason),
}

impl StorageUnavailable {
    pub fn internal(reason: impl Into<String>) -> Self {
        Self {
            kind: StorageUnavailableKind::Internal,
            reason: reason.into(),
        }
    }
    pub fn ingest_rejected(reason: IngestRejectReason) -> Self {
        Self {
            kind: StorageUnavailableKind::IngestRejected(reason),
            reason: format!("ingest rejected: {}", reason.as_snake_case()),
        }
    }
}

impl std::fmt::Display for StorageUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}
impl std::error::Error for StorageUnavailable {}

impl StorageUnavailable {
    /// Map this error to a canonical `ApiError` for HTTP responses.
    ///
    /// Mapping:
    /// - `Internal` → 500 with `self.reason` (callers may log before calling).
    /// - `IngestRejected` → 400 with `ingest rejected: <reason>`.
    pub fn into_api_error(self) -> crate::api_error::ApiError {
        match self.kind {
            StorageUnavailableKind::Internal => crate::api_error::ApiError::internal(self.reason),
            StorageUnavailableKind::IngestRejected(_) => {
                crate::api_error::ApiError::bad_request(self.reason)
            }
        }
    }
}

/// Parameters for `search` (built by the `fauna.search.query` handler).
pub struct SearchSpec {
    pub query: String,
    pub content_type: Option<String>,
    pub before: Option<i64>,
    pub after: Option<i64>,
    pub limit: i64,
    pub offset: i64,
}

/// One search hit (mirrors today's `db.search_with_scoping` row shape).
#[derive(Debug)]
pub struct SearchHit {
    pub content_type: String,
    pub content_id: String,
    pub created_at: i64,
    pub rank: f64,
    pub snippet: String,
}

/// A freshly-observed ACME cert + private key.
pub struct AcmeMaterial<'a> {
    pub domain: &'a str,
    pub cert_chain_pem: &'a [u8],
    pub priv_key_pem: &'a [u8],
}

// ── Blob ingest ──────────────────────────────────────────────────────────────

/// A blob being ingested via `POST /api/v1/blob`. The `uploader` actor id is
/// recorded for audit; `body` is the raw payload as received over the wire;
/// `sidecar` is the uploader-supplied `UploadSidecar` from the multipart wire
/// shape (`Some(...)` on the multipart path; `None` is refused — the route
/// answers 400 and the trait refuses — see the upload-sidecar wire design,
/// tracked internally).
pub struct BlobIngestItem<'a> {
    pub uploader: [u8; 32],
    pub body: &'a [u8],
    pub sidecar: Option<fauna_media::sidecar::UploadSidecar>,
}

/// What the caller should write to the blob store + metadata after the
/// trait method has had its say.
///
/// `stored_bytes` is byte-identical to `item.body` (the nest stores what the
/// uploader sealed, opaque); `mime` and `has_c2pa` are the uploader's sidecar
/// assertions (fixed to `application/octet-stream` / `false` for every sealed
/// audience class — the real values ride *inside* the sealed bytes). Media
/// processing — MIME sniff, EXIF/IPTC strip, C2PA detect, thumbnail render —
/// is uploader-side (`fauna_media::process`), which is what every app
/// already does; the nest cannot read the bytes to do it (`ui/media.md`
/// § Encryption at rest).
#[derive(Debug)]
pub struct BlobIngestOutcome {
    pub verdict: BlobIngestVerdict,
    pub stored_bytes: bytes::Bytes,
    pub mime: String,
    pub has_c2pa: Option<bool>,
    /// Declarative thumbnail hash from the `UploadSidecar`. `Some(h)` when the
    /// client declared a thumbnail companion blob (uploaded out-of-band as its
    /// own sealed blob keyed by `h`). The route writes it to
    /// `blob_metadata.thumbnail_hash`.
    pub thumbnail_hash: Option<[u8; 32]>,
}

/// Outcome of per-class envelope-shape verification.
///
/// Only `Accepted` remains: the per-class verifier **rejects** a malformed
/// envelope with `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Blob*))`
/// rather than storing it permissively, matching the post + channel pipelines
/// (`docs/goal/architecture/encryption-at-rest.md` § Don't do these). The
/// rejection cases live on the `Err` arm with the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobIngestVerdict {
    /// Blob-ingest accepted. Rejection cases live on `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Blob*))`.
    Accepted,
}

impl BlobIngestVerdict {
    pub fn as_metric_label(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
        }
    }
}

// ── Post ingest ──────────────────────────────────────────────────────────────

/// A post being ingested via `fauna.posts.create`. `uploader` is the
/// authenticated connection's actor id, which the post's signed `author` must
/// equal (own-write); `body` is the raw dag-cbor-encoded `Post` as received
/// over the wire.
pub struct PostIngestItem<'a> {
    pub uploader: [u8; 32],
    pub body: &'a [u8],
}

/// Outcome of post-ingest verification. The route handler stores `item.body`
/// either way (permissive interim); the verdict drives the
/// `nest_post_ingest_total{mode,verdict}` tracing metric.
#[derive(Debug)]
pub struct PostIngestOutcome {
    pub verdict: PostIngestVerdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostIngestVerdict {
    /// Post-ingest accepted. Rejection cases live on `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Post*))`.
    Accepted,
}

impl PostIngestVerdict {
    pub fn as_metric_label(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
        }
    }
}

// ── Channel envelope ingest ──────────────────────────────────────────────────

/// A channel envelope being ingested via `POST /api/v1/channel/{channel_id}`.
/// `sender` is the bearer auth's actor id (recorded for audit + roster check);
/// `channel_id` identifies the channel; `body` is the raw dag-cbor-encoded
/// `ChannelEnvelope` as received over the wire.
pub struct ChannelEnvelopeIngestItem<'a> {
    pub sender: [u8; 32],
    pub channel_id: [u8; 32],
    pub body: &'a [u8],
}

/// Outcome of channel-envelope ingest verification. The route handler stores
/// `item.body` either way (permissive interim); the verdict drives the
/// `nest_channel_ingest_total{mode,verdict}` tracing metric.
#[derive(Debug)]
pub struct ChannelEnvelopeIngestOutcome {
    pub verdict: ChannelEnvelopeIngestVerdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEnvelopeIngestVerdict {
    /// Channel-envelope ingest accepted. Rejection cases live on `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Channel*))`.
    Accepted,
}

impl ChannelEnvelopeIngestVerdict {
    pub fn as_metric_label(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
        }
    }
}

#[async_trait]
pub trait Storage: Send + Sync + std::fmt::Debug {
    /// Server-side search over the **floor-derived** corpus — public post
    /// bodies, restricted-post public previews (`Post::body_text()` is the
    /// preview when `gated.is_some()`), and profile handles/bios. Every nest
    /// serves it: it reads nothing sealed (`content_fts` is fed only by the
    /// post projection + `fts::sync_profile_row`, which indexes the handle — verified 2026-07-12, writer renamed 2026-09-23), so there is
    /// no posture on which it could leak.
    ///
    /// Search over *sealed* content (mail, calendar, conversations) is not on
    /// this seam at all — it runs at a capability position: the AUTH'd MDA
    /// session or the user's client, against the sealed `__index` segments
    /// (`behavior/content-index.md`).
    async fn search(
        &self,
        actor: &[u8; 32],
        spec: &SearchSpec,
    ) -> Result<Vec<SearchHit>, StorageUnavailable>;

    /// Persist a freshly-observed cert+key: write `fullchain.pem` / `privkey.pem`
    /// for the nest's own listener AND wrap (cert+key) to each registered bridge
    /// service-user x25519 pubkey → store `TlsCertBlob`s.
    async fn store_acme_material(&self, m: &AcmeMaterial<'_>) -> Result<(), StorageUnavailable>;

    /// Authoritative seal-on-read for the bridge TLS-cert fetch path
    /// (`fauna.bridges.fetch_tls_cert_blob`). Seal the cert currently on disk
    /// (`{acme_dir}/fullchain.pem` + `privkey.pem` — whatever ACME or the
    /// self-signed path last wrote) freshly to the bridge identified by
    /// `(role, bridge_id)`, keyed by the caller-supplied `domain` and sealed to
    /// that bridge's attested x25519 pubkey, and persist + return the wrapped
    /// blob bytes.
    ///
    /// This is what makes the cert reach *every* bridge with no way to
    /// misconfigure: the eager [`store_acme_material`] fan-out only seals to
    /// bridges that were approved+attested *at issuance time* and keys on the
    /// domain the watcher happened to pass, so a bridge that attests *after*
    /// ACME succeeds — or fetches under its `PrimaryDomain` rather than the apex
    /// — would otherwise never receive a blob. Seal-on-read removes both
    /// dependencies: the on-disk PEM is the single source of truth and the
    /// requester's own `(domain, x25519)` are the seal key.
    ///
    /// Returns `Ok(None)` when there is no cert on disk yet, or no
    /// approved+attested bridge row matches `(role, bridge_id)` — the caller
    /// then falls back to any previously-stored blob. The default impl returns
    /// `Ok(None)` (for test doubles, which have no `acme_dir`);
    /// [`SealedStorage`] overrides it.
    async fn seal_current_tls_cert_for_bridge(
        &self,
        _role: &str,
        _bridge_id: &str,
        _domain: &str,
    ) -> Result<Option<Vec<u8>>, StorageUnavailable> {
        Ok(None)
    }

    /// Seal the cert currently on disk to the relay sidecar's attested X25519
    /// (the recipient it declared in its `fauna.sidecar.hello`), returning the
    /// wrapped `TlsCertBlob` bytes. The relay opens it with its own X25519 secret
    /// and serves `relay.<apex>` TLS — it never reads `/data/acme` itself
    /// (`security.md` § UID isolation). No DB lookup / no persist (see
    /// [`seal_current_tls_cert_for_x25519_impl`]); the default returns `Ok(None)`
    /// (test doubles have no `acme_dir`), overridden by [`SealedStorage`].
    async fn seal_current_tls_cert_for_x25519(
        &self,
        _role: &str,
        _bridge_id: &str,
        _domain: &str,
        _x25519_pk: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, StorageUnavailable> {
        Ok(None)
    }

    /// Switch the nest's own TLS listener back to a real (CA-issued) cert after
    /// an admin self-signed override (`store_acme_material` of a self-signed
    /// cert): restore the preserved real cert if one was backed up, else leave
    /// the live cert serving and let ACME self-heal. The default errors (test
    /// doubles have no `acme_dir`); [`SealedStorage`] overrides it. See
    /// [`restore_real_tls_cert_at`].
    async fn restore_real_tls_cert(&self) -> Result<RestoreTlsMethod, StorageUnavailable> {
        Err(StorageUnavailable::internal(
            "this Storage impl has no acme_dir",
        ))
    }

    /// Pre-store verification for the blob upload path (`POST /api/v1/blob`).
    ///
    /// Dispatch on the sidecar's `AudienceClass` to a per-class envelope-shape
    /// verifier (strict). An absent sidecar, or a per-class rule violation
    /// (plaintext content magic prefix, length below the AEAD floor, sidecar
    /// `mime`/`has_c2pa` cross-check failure), returns
    /// `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Blob*))`,
    /// which the route maps to HTTP 400 `{"error":"ingest rejected: <reason>"}`.
    async fn ingest_blob(
        &self,
        item: &BlobIngestItem<'_>,
    ) -> Result<BlobIngestOutcome, StorageUnavailable>;

    /// Pre-store verification for the post upload path (`fauna.posts.create`).
    ///
    /// Verify the dag-cbor-encoded `Post` decodes, its signature verifies
    /// against `post.author`, `post.author` is the uploader (own-write), and
    /// — if `gated.is_some()` — the `encrypted_ref`
    /// is non-zero and the `key_access` map is internally consistent
    /// (`Mls.group_id` non-empty; `Broadcast.key_blob_ref` non-zero). Rejection
    /// → `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Post*))`.
    async fn ingest_post(
        &self,
        item: &PostIngestItem<'_>,
    ) -> Result<PostIngestOutcome, StorageUnavailable>;

    /// Pre-store verification for the channel-envelope upload path
    /// (`POST /api/v1/channel/{channel_id}`).
    ///
    /// Dag-cbor-decode the `ChannelEnvelope`, check the inner bytes for AEAD
    /// shape (length floor + no plaintext-content magic prefix), and check that
    /// the sender is a member of the channel in `actor_channels`. Rejection →
    /// `Err(StorageUnavailable::ingest_rejected(IngestRejectReason::Channel*))`.
    async fn ingest_channel_envelope(
        &self,
        item: &ChannelEnvelopeIngestItem<'_>,
    ) -> Result<ChannelEnvelopeIngestOutcome, StorageUnavailable>;
}

pub type SharedStorage = Arc<dyn Storage>;

// ── Shared write helper ───────────────────────────────────────────────────────

/// Write `data` to `path` atomically: write to `<path>.tmp`, fsync, rename,
/// fsync the parent directory. The durability sequence lives in
/// [`fauna_segment_store::atomic_save`] — this wrapper only fixes the tmp name.
///
/// The parent-dir fsync arrived with that shared primitive; the hand-rolled
/// body this replaced stopped after the rename, so a power loss could lose the
/// rename entry for a cert/key that had itself been synced.
pub(crate) fn atomic_write(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fauna_segment_store::atomic_save(path, &tmp, data)
}

/// Write `{acme_dir}/fullchain.pem` and `{acme_dir}/privkey.pem` atomically.
/// Creates the directory if it doesn't exist. Used by [`SealedStorage`] and by
/// the seal-on-read fetch path so the PEM-write logic stays in one place.
pub(crate) fn write_acme_pem_atomic(
    acme_dir: &std::path::Path,
    cert_chain_pem: &[u8],
    priv_key_pem: &[u8],
) -> std::io::Result<()> {
    std::fs::create_dir_all(acme_dir)?;
    let cert_path = acme_dir.join(crate::acme::CERT_FILENAME);
    let key_path = acme_dir.join(crate::acme::KEY_FILENAME);

    // TLS switch-back support ("store the LE cert and re-use it"): if a
    // *self-signed* cert is about to overwrite an existing *real* (CA-issued)
    // cert, preserve the real cert+key to the backup slot first so
    // `restore_real_tls_cert` can put it back without re-running ACME (which
    // would risk Let's Encrypt rate limits). Only the real→self-signed
    // transition is backed up: real→real (an ACME renewal, incl. the cert
    // watcher's re-store of an unchanged cert) needs no backup, and
    // self-signed→self-signed must not clobber the preserved real cert.
    if crate::acme::pem_is_self_signed(cert_chain_pem)
        && let Ok(existing_cert) = std::fs::read(&cert_path)
        && !crate::acme::pem_is_self_signed(&existing_cert)
        && let Ok(existing_key) = std::fs::read(&key_path)
    {
        atomic_write(
            &acme_dir.join(crate::acme::REAL_CERT_BACKUP_FILENAME),
            &existing_cert,
        )?;
        atomic_write(
            &acme_dir.join(crate::acme::REAL_KEY_BACKUP_FILENAME),
            &existing_key,
        )?;
        tracing::info!(
            "preserved the existing real TLS cert to the backup slot before a \
             self-signed cert overwrite (restore via fauna.bridges.restore_real_tls_cert)"
        );
    }

    atomic_write(&cert_path, cert_chain_pem)?;
    atomic_write(&key_path, priv_key_pem)?;
    Ok(())
}

/// Read the cert+key currently on disk under `acme_dir`. `None` if either PEM
/// is absent (fresh nest before the first ACME/self-signed write). Shared by
/// the seal-on-read fetch path in both storage impls.
pub(crate) fn read_acme_pem(acme_dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let cert = std::fs::read(acme_dir.join(crate::acme::CERT_FILENAME)).ok()?;
    let key = std::fs::read(acme_dir.join(crate::acme::KEY_FILENAME)).ok()?;
    Some((cert, key))
}

/// Seal `(cert_chain_pem, priv_key_pem)` into a wrapped `TlsCertBlob` for one
/// bridge and return its canonical wire bytes. The single seal helper shared by
/// the ACME/self-signed fan-out ([`Storage::store_acme_material`]) and the
/// seal-on-read fetch path ([`Storage::seal_current_tls_cert_for_bridge`]) so
/// the on-wire shape stays byte-identical across both. `expires_at` is a
/// 90-day re-fetch hint (the cert PEM carries its own real `notAfter`).
pub(crate) fn seal_tls_cert_blob_bytes(
    role: &str,
    bridge_id: &str,
    domain: &str,
    x25519_pk: &[u8; 32],
    cert_chain_pem: &[u8],
    priv_key_pem: &[u8],
    now_unix: u64,
) -> Result<Vec<u8>, StorageUnavailable> {
    use fauna_mls::wrapped_blob::{TlsCertBlob, TlsCertBundle, seal_tls_cert};

    let bundle = TlsCertBundle {
        cert_chain: cert_chain_pem.to_vec(),
        priv_key: priv_key_pem.to_vec(),
        issued_at: now_unix,
        expires_at: now_unix.saturating_add(90 * 24 * 3600),
    };
    let blob: TlsCertBlob =
        seal_tls_cert(&bundle, role, bridge_id, domain, x25519_pk).map_err(|e| {
            StorageUnavailable::internal(format!("seal_tls_cert for {role}/{bridge_id}: {e}"))
        })?;
    blob.to_canonical_bytes().map_err(|e| {
        StorageUnavailable::internal(format!(
            "TlsCertBlob::to_canonical_bytes for {role}/{bridge_id}: {e}"
        ))
    })
}

/// Seal the cert currently on disk under `acme_dir` to one approved+attested
/// bridge, persisting + returning the wrapped blob bytes. Shared by both
/// storage impls' [`Storage::seal_current_tls_cert_for_bridge`] override.
/// `Ok(None)` when there is no on-disk cert, or no approved bridge row with an
/// x25519 pubkey matches `(role, bridge_id)`.
pub(crate) async fn seal_current_tls_cert_for_bridge_impl(
    db: &crate::db::CacheDb,
    acme_dir: &std::path::Path,
    role: &str,
    bridge_id: &str,
    domain: &str,
) -> Result<Option<Vec<u8>>, StorageUnavailable> {
    let Some(parsed_role) = crate::db::bridge_service_users::BridgeRole::parse(role) else {
        return Ok(None);
    };
    let Some((cert_pem, key_pem)) = read_acme_pem(acme_dir) else {
        return Ok(None);
    };
    let x25519_pk = match db
        .find_approved_bridge_by_role_and_id(parsed_role, bridge_id)
        .await
        .map_err(|e| StorageUnavailable::internal(format!("find approved bridge: {e:#}")))?
        .and_then(|row| row.x25519_pubkey)
    {
        Some(pk) => pk,
        None => return Ok(None),
    };

    let now_unix = fauna_core::data::Timestamp::now_secs_or_zero() as u64;
    let blob_bytes = seal_tls_cert_blob_bytes(
        role, bridge_id, domain, &x25519_pk, &cert_pem, &key_pem, now_unix,
    )?;
    db.put_tls_cert_blob(role, bridge_id, domain, &blob_bytes)
        .await
        .map_err(|e| StorageUnavailable::internal(format!("put_tls_cert_blob: {e:#}")))?;
    Ok(Some(blob_bytes))
}

/// Seal the cert currently on disk under `acme_dir` to the relay sidecar's
/// **directly-supplied** X25519 (the one it attested in its
/// `fauna.sidecar.hello`), returning the wrapped blob bytes. `Ok(None)` when no
/// cert is on disk yet (fresh nest — the relay retries on its refresh timer).
///
/// Unlike [`seal_current_tls_cert_for_bridge_impl`] this does **no** DB identity
/// lookup and does **not** persist to `bridge_tls_cert_blobs`: the relay is a
/// single co-located sidecar whose seal recipient is the live channel's attested
/// X25519, not an enrolled+approved bridge row, and it always re-fetches fresh
/// (seal-on-read), so a stale persisted blob keyed on a prior X25519 would be a
/// liability rather than a fallback. The `(role, bridge_id, domain)` triple is
/// only the AAD binding the seal/unseal share (`fauna_mls::wrapped_blob`).
pub(crate) fn seal_current_tls_cert_for_x25519_impl(
    acme_dir: &std::path::Path,
    role: &str,
    bridge_id: &str,
    domain: &str,
    x25519_pk: &[u8; 32],
) -> Result<Option<Vec<u8>>, StorageUnavailable> {
    let Some((cert_pem, key_pem)) = read_acme_pem(acme_dir) else {
        return Ok(None);
    };
    let now_unix = fauna_core::data::Timestamp::now_secs_or_zero() as u64;
    let blob_bytes = seal_tls_cert_blob_bytes(
        role, bridge_id, domain, x25519_pk, &cert_pem, &key_pem, now_unix,
    )?;
    Ok(Some(blob_bytes))
}

/// How [`restore_real_tls_cert_at`] recovered (or scheduled recovery of) a
/// real TLS cert. Surfaced on the wire so the admin UI can say what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreTlsMethod {
    /// A real cert preserved by a prior self-signed switch was copied back into
    /// the live PEM slot; the cert watcher hot-reloads it, so the listener
    /// serves the real cert within a couple of seconds — an instant restore
    /// that skips the ACME round-trip.
    FromBackup,
    /// No preserved real cert existed, so nothing was changed. This is **not**
    /// an error or a dead end: when ACME is enabled the lifecycle task
    /// self-heals a self-signed cert to a real one automatically on its next
    /// poll (`acme_http01::cert_lifecycle_task`, "obtaining a real certificate
    /// to replace it") — obtain-then-overwrite, no outage. Nothing to do here.
    NoBackupSelfHeals,
}

/// Switch the nest's TLS listener back to a real (CA-issued) cert after a
/// self-signed override — the **instant**, non-destructive path. If a real cert
/// was preserved by [`write_acme_pem_atomic`] it is restored (and consumed) so
/// the listener serves it within ~2 s. With no backup this is a no-op: the ACME
/// lifecycle task self-heals the self-signed cert to a real one on its own (so
/// there is never a reason to delete the live cert and risk an outage).
pub(crate) fn restore_real_tls_cert_at(
    acme_dir: &std::path::Path,
) -> std::io::Result<RestoreTlsMethod> {
    let backup_cert = acme_dir.join(crate::acme::REAL_CERT_BACKUP_FILENAME);
    let backup_key = acme_dir.join(crate::acme::REAL_KEY_BACKUP_FILENAME);
    let cert_path = acme_dir.join(crate::acme::CERT_FILENAME);
    let key_path = acme_dir.join(crate::acme::KEY_FILENAME);

    if let (Ok(cert), Ok(key)) = (std::fs::read(&backup_cert), std::fs::read(&backup_key)) {
        // Put the preserved real cert back; the cert watcher reloads it.
        atomic_write(&cert_path, &cert)?;
        atomic_write(&key_path, &key)?;
        // The backup has served its purpose — drop it so a future self-signed
        // switch re-preserves the (now-live) real cert afresh.
        let _ = std::fs::remove_file(&backup_cert);
        let _ = std::fs::remove_file(&backup_key);
        Ok(RestoreTlsMethod::FromBackup)
    } else {
        // No backup: leave the live self-signed cert serving and let the ACME
        // lifecycle task self-heal it to a real cert. Never delete the only cert.
        Ok(RestoreTlsMethod::NoBackupSelfHeals)
    }
}

/// A `Storage` impl that implements only the required trait methods and takes
/// **every provided default** — notably `seal_current_tls_cert_for_bridge` /
/// `seal_current_tls_cert_for_x25519`, which default to `Ok(None)`.
///
/// Exists so a test can prove a handler does NOT delegate to `state.storage()`
/// for something it must own itself: install this, and a TLS cert that is still
/// delivered can only have been sealed by the handler from `acme_dir` directly
/// (`sidecar_channel::relay_fetch_tls_cert`,
/// `bridge_blob_handlers::fetch_tls_cert_blob_handler` — both decoupled from the
/// user-data storage seam on purpose).
#[cfg(test)]
#[derive(Debug)]
pub struct DefaultSealsStorage;

#[cfg(test)]
#[async_trait]
impl Storage for DefaultSealsStorage {
    async fn search(
        &self,
        _: &[u8; 32],
        _: &SearchSpec,
    ) -> Result<Vec<SearchHit>, StorageUnavailable> {
        Ok(Vec::new())
    }
    async fn store_acme_material(&self, _: &AcmeMaterial<'_>) -> Result<(), StorageUnavailable> {
        Ok(())
    }
    async fn ingest_blob(
        &self,
        _: &BlobIngestItem<'_>,
    ) -> Result<BlobIngestOutcome, StorageUnavailable> {
        Err(StorageUnavailable::internal("test double"))
    }
    async fn ingest_post(
        &self,
        _: &PostIngestItem<'_>,
    ) -> Result<PostIngestOutcome, StorageUnavailable> {
        Err(StorageUnavailable::internal("test double"))
    }
    async fn ingest_channel_envelope(
        &self,
        _: &ChannelEnvelopeIngestItem<'_>,
    ) -> Result<ChannelEnvelopeIngestOutcome, StorageUnavailable> {
        Err(StorageUnavailable::internal("test double"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TLS switch-back (backup-on-self-signed + restore) ─────────────────

    /// A CA-signed leaf (issuer != subject) standing in for a real Let's
    /// Encrypt cert, plus its key PEM.
    fn cn_params(san: &str, cn: &str) -> rcgen::CertificateParams {
        // rcgen's default DistinguishedName is empty; without an explicit CN
        // both issuer and subject DNs would be empty and every cert would read
        // as "self-signed". Set a CN so issuer/subject are meaningful.
        let mut params = rcgen::CertificateParams::new(vec![san.to_string()]).unwrap();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, cn.to_string());
        params
    }

    fn ca_signed_leaf_pem() -> (String, String) {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = cn_params("test-ca", "Test CA");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf_params = cn_params("leaf.example", "leaf.example");
        let leaf_cert = leaf_params.signed_by(&leaf_key, &ca_cert, &ca_key).unwrap();
        (leaf_cert.pem(), leaf_key.serialize_pem())
    }

    /// A self-signed leaf (issuer == subject), like the admin-synthesized cert.
    fn self_signed_pem() -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = cn_params("self.example", "self.example")
            .self_signed(&key)
            .unwrap();
        (cert.pem(), key.serialize_pem())
    }

    #[test]
    fn pem_is_self_signed_distinguishes_ca_signed_from_self_signed() {
        let (real_pem, _) = ca_signed_leaf_pem();
        let (ss_pem, _) = self_signed_pem();
        assert!(
            !crate::acme::pem_is_self_signed(real_pem.as_bytes()),
            "CA-signed leaf must read as NOT self-signed"
        );
        assert!(
            crate::acme::pem_is_self_signed(ss_pem.as_bytes()),
            "self-signed leaf must read as self-signed"
        );
    }

    #[test]
    fn self_signed_overwrite_preserves_real_cert_and_restore_brings_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path();
        let (real_cert, real_key) = ca_signed_leaf_pem();
        let (ss_cert, ss_key) = self_signed_pem();

        // 1) A real (ACME) cert lands first — no backup created (nothing to preserve).
        write_acme_pem_atomic(acme_dir, real_cert.as_bytes(), real_key.as_bytes()).unwrap();
        assert!(
            !acme_dir
                .join(crate::acme::REAL_CERT_BACKUP_FILENAME)
                .exists()
        );

        // 2) An admin self-signed cert overwrites it → the real cert is preserved.
        write_acme_pem_atomic(acme_dir, ss_cert.as_bytes(), ss_key.as_bytes()).unwrap();
        let backed_up =
            std::fs::read(acme_dir.join(crate::acme::REAL_CERT_BACKUP_FILENAME)).unwrap();
        assert_eq!(
            backed_up,
            real_cert.as_bytes(),
            "real cert preserved verbatim"
        );
        // Live cert is now the self-signed one.
        let live = std::fs::read(acme_dir.join(crate::acme::CERT_FILENAME)).unwrap();
        assert_eq!(live, ss_cert.as_bytes());

        // 3) A second self-signed write must NOT clobber the real backup.
        let (ss2_cert, ss2_key) = self_signed_pem();
        write_acme_pem_atomic(acme_dir, ss2_cert.as_bytes(), ss2_key.as_bytes()).unwrap();
        let still = std::fs::read(acme_dir.join(crate::acme::REAL_CERT_BACKUP_FILENAME)).unwrap();
        assert_eq!(
            still,
            real_cert.as_bytes(),
            "real backup survives self→self"
        );

        // 4) Restore → the real cert is live again, backup consumed.
        let method = restore_real_tls_cert_at(acme_dir).unwrap();
        assert_eq!(method, RestoreTlsMethod::FromBackup);
        let live = std::fs::read(acme_dir.join(crate::acme::CERT_FILENAME)).unwrap();
        assert_eq!(
            live,
            real_cert.as_bytes(),
            "real cert restored to live slot"
        );
        assert!(
            !acme_dir
                .join(crate::acme::REAL_CERT_BACKUP_FILENAME)
                .exists()
        );
    }

    #[test]
    fn restore_with_no_backup_is_nondestructive_self_heal_takes_over() {
        let dir = tempfile::tempdir().unwrap();
        let acme_dir = dir.path();
        let (ss_cert, ss_key) = self_signed_pem();
        // Self-signed cert is live, but there was never a real cert to back up.
        write_acme_pem_atomic(acme_dir, ss_cert.as_bytes(), ss_key.as_bytes()).unwrap();
        assert!(
            !acme_dir
                .join(crate::acme::REAL_CERT_BACKUP_FILENAME)
                .exists()
        );

        // No backup → no-op. The live self-signed cert MUST survive (the ACME
        // lifecycle self-heals it to a real cert without ever deleting the only
        // cert on disk, so HTTPS never goes down — even across a restart).
        let method = restore_real_tls_cert_at(acme_dir).unwrap();
        assert_eq!(method, RestoreTlsMethod::NoBackupSelfHeals);
        assert!(acme_dir.join(crate::acme::CERT_FILENAME).exists());
        assert!(acme_dir.join(crate::acme::KEY_FILENAME).exists());
    }

    // ── Blob ingest: strict verdict shape ─────────────────────────────────────
    //
    // `blob_api.rs::blob_upload_download_roundtrip` covers the happy-path
    // integration round-trip; the unit tests below pin the strict per-class
    // verifier's verdict matrix that integration tests can't easily exercise.

    fn sealed_storage() -> SealedStorage {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        SealedStorage::new(db, std::path::PathBuf::from("/tmp/test-acme-blob-ingest"))
    }

    /// A Library-class sidecar (AEAD-sealed audience): `mime` must be
    /// octet-stream and `has_c2pa` false, matching the per-class verifier.
    fn library_sidecar() -> fauna_media::sidecar::UploadSidecar {
        fauna_media::sidecar::UploadSidecar {
            class: fauna_media::audience::AudienceClass::Library,
            mime: "application/octet-stream".to_string(),
            has_c2pa: false,
            thumbnail_hash: None,
        }
    }

    /// Extract the `IngestRejectReason` from a `StorageUnavailable`, panicking
    /// if it is any other kind.
    fn reject_reason(err: &StorageUnavailable) -> IngestRejectReason {
        match err.kind {
            StorageUnavailableKind::IngestRejected(r) => r,
            other => panic!("expected IngestRejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sealed_storage_blob_ingest_rejects_absent_sidecar() {
        // Strict flip: the retired octet-stream shape (sidecar = None) can't be
        // verified without the audience class → reject (trait-level self-defense;
        // the route 400s a non-multipart body before reaching here).
        let body: Vec<u8> = (0u8..64).collect();
        let err = sealed_storage()
            .ingest_blob(&BlobIngestItem {
                uploader: [1u8; 32],
                body: &body,
                sidecar: None,
            })
            .await
            .unwrap_err();
        assert_eq!(reject_reason(&err), IngestRejectReason::BlobSidecarAbsent);
    }

    #[tokio::test]
    async fn sealed_storage_blob_ingest_accepts_aead_shaped_bytes() {
        // 12-byte nonce ‖ ciphertext+tag (random 64 bytes is plausibly sealed).
        let body: Vec<u8> = (0u8..64).collect();
        let outcome = sealed_storage()
            .ingest_blob(&BlobIngestItem {
                uploader: [1u8; 32],
                body: &body,
                sidecar: Some(library_sidecar()),
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, BlobIngestVerdict::Accepted);
        // The nest stores opaque bytes only — no MIME sniff, no thumbnail render;
        // the served metadata comes verbatim from the sidecar.
        assert_eq!(outcome.mime, "application/octet-stream");
        assert_eq!(outcome.has_c2pa, Some(false));
        assert!(outcome.thumbnail_hash.is_none());
        assert_eq!(outcome.stored_bytes.as_ref(), body.as_slice());
    }

    #[tokio::test]
    async fn sealed_storage_blob_ingest_rejects_png_magic() {
        // A PNG signature under a sealed audience is a clear sign the uploader
        // didn't seal — reject, don't store.
        let mut body = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        body.extend_from_slice(&[0u8; 64]);
        let err = sealed_storage()
            .ingest_blob(&BlobIngestItem {
                uploader: [2u8; 32],
                body: &body,
                sidecar: Some(library_sidecar()),
            })
            .await
            .unwrap_err();
        assert_eq!(
            reject_reason(&err),
            IngestRejectReason::BlobPlaintextMagicPrefix
        );
    }

    #[tokio::test]
    async fn sealed_storage_blob_ingest_rejects_jpeg_magic() {
        let mut body = vec![0xFF, 0xD8, 0xFF];
        body.extend_from_slice(&[0u8; 64]);
        let err = sealed_storage()
            .ingest_blob(&BlobIngestItem {
                uploader: [3u8; 32],
                body: &body,
                sidecar: Some(library_sidecar()),
            })
            .await
            .unwrap_err();
        assert_eq!(
            reject_reason(&err),
            IngestRejectReason::BlobPlaintextMagicPrefix
        );
    }

    #[tokio::test]
    async fn sealed_storage_blob_ingest_rejects_too_short() {
        // Less than ChaCha20-Poly1305 minimum (12-byte nonce + 16-byte tag = 28 bytes).
        let err = sealed_storage()
            .ingest_blob(&BlobIngestItem {
                uploader: [4u8; 32],
                body: &[0u8; 10],
                sidecar: Some(library_sidecar()),
            })
            .await
            .unwrap_err();
        assert_eq!(reject_reason(&err), IngestRejectReason::BlobLengthFloor);
    }

    // ── Post ingest: strict verdict shape ─────────────────────────────────────
    //
    // The strict impl does BARE-decode + signature-verify + (if gated)
    // encrypted_ref shape + key_access internal consistency, all per the
    // permissive interim documented for item 1(a) (tracked internally).

    fn signed_text_post(kp: &fauna_core::identity::ActorKeypair) -> Vec<u8> {
        signed_text_post_at(kp, fauna_core::data::Timestamp::now())
    }

    fn signed_text_post_at(
        kp: &fauna_core::identity::ActorKeypair,
        created_at: fauna_core::data::Timestamp,
    ) -> Vec<u8> {
        use fauna_core::data::{Post, PostBody};
        use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
        let post = Post {
            author: kp.actor_id(),
            created_at,
            body: PostBody::Text {
                content: "hello fauna".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let (bytes, env) = sign_envelope(kp, &post).unwrap();
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap()
    }

    fn signed_gated_post(
        kp: &fauna_core::identity::ActorKeypair,
        gated: fauna_core::subscription::types::GatedInfo,
    ) -> Vec<u8> {
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
        let post = Post {
            author: kp.actor_id(),
            created_at: Timestamp::now(),
            body: PostBody::Text {
                content: "preview text".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: Some(gated),
            content_warning: None,
            origin: None,
        };
        let (bytes, env) = sign_envelope(kp, &post).unwrap();
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap()
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_accepts_well_formed_signed_post() {
        let kp = fauna_core::identity::ActorKeypair::generate();
        let body = signed_text_post(&kp);
        let outcome = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, PostIngestVerdict::Accepted);
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_flags_undecodable_bytes_as_malformed() {
        let err = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: [9u8; 32],
                body: b"not a valid dag-cbor-encoded Post at all",
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::PostDecode)
        );
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_flags_tampered_post_as_malformed() {
        use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode};
        let kp = fauna_core::identity::ActorKeypair::generate();
        let body = signed_text_post(&kp);
        // Decode wire → flip a byte in the envelope's signature portion
        // (bytes 36..100 of the 100-byte envelope; bytes 0..36 are the
        // CID and stay intact). The CID still matches the inner bytes
        // (canonical form passes), but ed25519 verify fails on the
        // corrupted signature → strict ingest maps to
        // PostSignatureMismatch. Tampering wire.bytes directly would
        // break canonical form and hit PostDecode first.
        let mut wire: EmbedAsBytes = canonical_decode(&body).unwrap();
        wire.envelope[36] ^= 0xff;
        let tampered = canonical_encode(&wire).unwrap();
        let err = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &tampered,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::PostSignatureMismatch)
        );
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_flags_unsigned_post_as_malformed() {
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
        let kp = fauna_core::identity::ActorKeypair::generate();
        let post = Post {
            author: kp.actor_id(),
            created_at: Timestamp::now(),
            body: PostBody::Text {
                content: "no signature".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        // Sign properly via sign_envelope, then zero the 64-byte
        // signature portion of the envelope (keep the 36-byte CID prefix
        // intact so into_signed parses and the canonical-form check
        // passes). verify_envelope's ed25519 step fails on the all-zero
        // signature → strict ingest maps to PostSignatureMismatch.
        // (An all-zero CID prefix would fail into_signed earlier and hit
        // PostDecode.)
        let (bytes, env) = sign_envelope(&kp, &post).unwrap();
        let mut envelope_serialized = [0u8; 100];
        envelope_serialized[0..36].copy_from_slice(env.cid().as_bytes());
        // Leave bytes 36..100 as zero (the signature portion).
        let wire = EmbedAsBytes {
            envelope: envelope_serialized.to_vec(),
            bytes,
            signer_auth: None,
        };
        let body = canonical_encode(&wire).unwrap();
        let err = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::PostSignatureMismatch)
        );
    }

    // ── Post ingest: created_at future bound ────────────────────────
    //
    // A followed author future-dating a post pins it atop every follower's
    // chronological feed (`docs/goal/ui/feed.md` § The read model); the past
    // is never bounded (backdating, archive imports).

    #[tokio::test]
    async fn sealed_storage_post_ingest_rejects_created_at_an_hour_in_the_future() {
        use fauna_core::data::Timestamp;
        let kp = fauna_core::identity::ActorKeypair::generate();
        let an_hour_ahead = Timestamp(Timestamp::now().0 + 3_600_000_000);
        let body = signed_text_post_at(&kp, an_hour_ahead);
        let err = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::PostCreatedAtInFuture)
        );
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_accepts_a_decade_old_created_at() {
        // The archive-import case: a legitimately backdated post must land
        // (`docs/goal/behavior/archive-import.md` § What each category
        // becomes — `created_at` = the post's original instant).
        use fauna_core::data::Timestamp;
        let kp = fauna_core::identity::ActorKeypair::generate();
        let a_decade_ago = Timestamp(Timestamp::now().0 - 10 * 365 * 24 * 3_600_000_000);
        let body = signed_text_post_at(&kp, a_decade_ago);
        let outcome = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, PostIngestVerdict::Accepted);
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_accepts_created_at_seconds_in_the_future() {
        // Inside the skew cushion (honest clock drift), not a rejection.
        use fauna_core::data::Timestamp;
        let kp = fauna_core::identity::ActorKeypair::generate();
        let a_few_seconds_ahead = Timestamp(Timestamp::now().0 + 5_000_000);
        let body = signed_text_post_at(&kp, a_few_seconds_ahead);
        let outcome = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, PostIngestVerdict::Accepted);
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_rejects_a_created_at_past_i64_max() {
        // `Timestamp` is a `u64`. Narrowed to `i64` before the comparison, every
        // value past `i64::MAX` wraps negative — and a negative instant is ahead
        // of nothing, so the farthest future passed the bound it most plainly
        // fails.
        use fauna_core::data::Timestamp;
        let kp = fauna_core::identity::ActorKeypair::generate();
        for created_at in [Timestamp(i64::MAX as u64 + 1), Timestamp(u64::MAX)] {
            let body = signed_text_post_at(&kp, created_at);
            let err = sealed_storage()
                .ingest_post(&PostIngestItem {
                    uploader: kp.actor_id().0,
                    body: &body,
                })
                .await
                .err()
                .unwrap_or_else(|| {
                    panic!(
                        "a created_at of {} was accepted — narrowed to i64 it wraps \
                         negative and passes the future bound",
                        created_at.0
                    )
                });
            assert_eq!(
                err.kind,
                StorageUnavailableKind::IngestRejected(IngestRejectReason::PostCreatedAtInFuture)
            );
        }
    }

    // ── The writers of `content.created_at` ───────────────────────────────────

    use crate::partition_scan::Gate;

    /// Where the `created_at` a production writer of `content.created_at`
    /// stores comes from — the column the local feed sorts on. The strings are
    /// the reasons, kept beside the entries so a reader never re-derives them.
    enum CreatedAt {
        /// A remote-asserted value: a post's signed claim, a translated Note's
        /// `published`, a swept event's own stamp, a peer's index claim. The
        /// [`Gate`] is the future bound this door applies before the write,
        /// held to the production code.
        Bounded(Gate, &'static str),
        /// Hands on the value its caller gave it. Its own name is one of
        /// [`CREATED_AT_WRITERS`], so each of its callers is an entry too.
        Relay(&'static str),
        /// The SQL that writes the column.
        Primitive(&'static str),
        /// No remote-asserted value reaches the feed through this writer: the
        /// nest's own clock, a row of a plane no feed reads, the owner's own
        /// restored data, or replication inside one deployment.
        Unbounded(&'static str),
    }

    /// The column's writers, each a literal including its opening paren (the
    /// shared scanner's match). `write_post_index` is not one: it writes
    /// `content_meta`, never `content.created_at`.
    const CREATED_AT_WRITERS: &[&str] = &[
        "INTO content (",
        "insert_content(",
        "insert_and_index(",
        "insert_post_index_entry(",
        "insert_post_index_entry_with_origin(",
        "put_post(",
        "put_post_with_source(",
        "put_post_index_only(",
        "put_post_index_only_on(",
        "put_post_with_source_index_only(",
        "store_post(",
        "store_post_with_expiry(",
    ];

    /// Every production writer of `content.created_at`, keyed by
    /// `(path under src/, enclosing fn)`.
    const CREATED_AT_CENSUS: &[(&str, &str, CreatedAt)] = &[
        // ── bounded doors: a Fauna-native post ──
        (
            "routes.rs",
            "ingest_post_core",
            CreatedAt::Bounded(
                Gate::Via {
                    hops: &[
                        ("storage/sealed.rs", "ingest_post"),
                        ("storage/sealed.rs", "classify_encrypted_post"),
                    ],
                    gates: &["reject_future_created_at("],
                },
                "`fauna.posts.create`: `Storage::ingest_post` → \
                 `sealed::classify_encrypted_post` runs `reject_future_created_at` before \
                 this fn stores the post. `ingest_post` is a trait method — `sealed.rs` is its \
                 one production impl today — and is itself a seam: the future-dated bound \
                 lives two hops below the call this fn makes, in `classify_encrypted_post`, so \
                 both links are held, not only the door's own call of the trait method \
                 ",
            ),
        ),
        (
            "federation_handlers.rs",
            "post_forward_handler",
            CreatedAt::Bounded(
                Gate::Calls(&["reject_future_created_at("]),
                "the federation forward leg verifies the envelope itself and never reaches \
                 `Storage::ingest_post`, so it calls the bound directly",
            ),
        ),
        (
            "exchange_originator.rs",
            "ingest_fetched_trend_post",
            CreatedAt::Bounded(
                Gate::Calls(&["reject_future_created_at("]),
                "a peer-served trend post, after its content binding and signature verify",
            ),
        ),
        (
            "discovery.rs",
            "index_peer_candidate",
            CreatedAt::Bounded(
                Gate::Calls(&["reject_future_created_at("]),
                "a discovery candidate's `created_at` is the peer's own unsigned claim",
            ),
        ),
        // ── bounded doors: a bridged foreign-protocol plane ──
        (
            "activitypub/inbox_routes.rs",
            "handle_create",
            CreatedAt::Bounded(
                Gate::Calls(&["reject_future_bridged_created_at("]),
                "a translated Note's `published`, after the audience and relationship gates",
            ),
        ),
        (
            "activitypub/inbox_routes.rs",
            "handle_update",
            CreatedAt::Bounded(
                Gate::Calls(&["reject_future_bridged_created_at("]),
                "the same for `Update{Note}`, before any map row moves",
            ),
        ),
        (
            "nostr/inbound_lifecycle.rs",
            "ingest_translated_event",
            CreatedAt::Bounded(
                Gate::Upstream {
                    callers: &[("nostr/sync_worker.rs", "process_inbound_event")],
                    gates: &["reject_future_bridged_created_at("],
                },
                "a swept event's own `created_at`; the router refuses a future-dated event \
                 of every kind but the NIP-59 gift wrap before it dispatches",
            ),
        ),
        (
            "bluesky/feed_ingest.rs",
            "ingest_feed_posts",
            CreatedAt::Bounded(
                Gate::Calls(&["reject_future_bridged_created_at("]),
                "a polled Bluesky post's own `createdAt`, refused before the store and the \
                 map row (`bridges.md` § Unified feed ingestion → *Bridge ingestion*)",
            ),
        ),
        // ── writers no remote-asserted value reaches ──
        (
            "activitypub/inbox_routes.rs",
            "mint_synthetic_reaction",
            CreatedAt::Unbounded("a synthetic reaction post is stamped `Timestamp::now()`"),
        ),
        (
            "segments/post.rs",
            "reassert_records",
            CreatedAt::Unbounded(
                "the owner's own restore -- the post rebuild both the snapshot restore \
                 (`restore_from_manifest`) and the backup materialize share, the one door that \
                 never refuses: a future-dated own post is the user's data (`feed.md` § The \
                 read model)",
            ),
        ),
        (
            "routes.rs",
            "get_post_core",
            CreatedAt::Unbounded(
                "caches a post from this deployment's own worker, which holds only what \
                 `ingest_post_core` replicated after the native bound",
            ),
        ),
        (
            "nest_link/client.rs",
            "handle_store",
            CreatedAt::Unbounded(
                "the worker storing a post its own primary replicated \
                 (`routes::spawn_replicate_post`, from `ingest_post_core`)",
            ),
        ),
        (
            "db/inbox.rs",
            "insert_inbox_row",
            CreatedAt::Unbounded("an `inbox/message` row, stamped with the nest's clock"),
        ),
        (
            "db/bridge.rs",
            "insert_bridge_message",
            CreatedAt::Unbounded(
                "a bridge message row: its schema is the bridge type, and it gets no \
                 `content_meta` row, which every feed query joins",
            ),
        ),
        (
            "profile_handlers.rs",
            "ingest_profile_core",
            CreatedAt::Unbounded(
                "the author's own signed `profile` row, which gets no `content_meta` row",
            ),
        ),
        (
            "db/moderation.rs",
            "post_legal_takedown_txn",
            CreatedAt::Unbounded(
                "the ATProto takedown-retraction witness, a tombstone-schema row with no \
                 `content_meta`",
            ),
        ),
        (
            "db/posts.rs",
            "delete_post_projection_with_witness",
            CreatedAt::Unbounded(
                "the ATProto delete witness, a tombstone-schema row with no `content_meta`",
            ),
        ),
        (
            "db/feeds.rs",
            "seed_scored_post_for_test",
            CreatedAt::Unbounded(
                "a seed helper compiled only under test, debug_assertions or test-hooks",
            ),
        ),
        // ── relays ──
        (
            "segments/post.rs",
            "store_post",
            CreatedAt::Relay("the post body's own `created_at`, via `store_post_with_expiry`"),
        ),
        (
            "segments/post.rs",
            "store_post_with_expiry",
            CreatedAt::Relay("the post body's own `created_at`, via the `put_post` family"),
        ),
        (
            "db/posts.rs",
            "put_post",
            CreatedAt::Relay(
                "the decoded post's own `created_at`; an undecodable body is stamped with the \
                 nest's clock",
            ),
        ),
        (
            "db/posts.rs",
            "put_post_with_source",
            CreatedAt::Relay("the decoded post's own `created_at`"),
        ),
        (
            "db/posts.rs",
            "put_post_index_only",
            CreatedAt::Relay("the decoded post's own `created_at`, via `put_post_index_only_on`"),
        ),
        (
            "db/posts.rs",
            "put_post_index_only_on",
            CreatedAt::Relay(
                "the decoded post's own `created_at` (a restore's in-transaction form of \
                 `put_post_index_only`)",
            ),
        ),
        (
            "db/posts.rs",
            "put_post_with_source_index_only",
            CreatedAt::Relay("the decoded post's own `created_at`"),
        ),
        (
            "db/content.rs",
            "insert_and_index",
            CreatedAt::Relay("its caller's `created_at`, beside the FTS row"),
        ),
        (
            "db/feeds.rs",
            "insert_post_index_entry",
            CreatedAt::Relay("its caller's `created_at`, with no origin nest"),
        ),
        // ── the SQL ──
        (
            "db/content.rs",
            "insert_content",
            CreatedAt::Primitive("`INSERT OR REPLACE INTO content`"),
        ),
        (
            "db/feeds.rs",
            "insert_post_index_entry_with_origin",
            CreatedAt::Primitive("`INSERT OR IGNORE INTO content`, a payload-less stub row"),
        ),
    ];

    /// **Every production writer of `content.created_at` is classified**
    /// (`docs/goal/ui/feed.md` § The read model).
    ///
    /// The column is what the local feed sorts on, so a door that writes a
    /// remote-asserted value into it without the future bound lets one remote
    /// party pin a post atop every local user's feed. The bound's doc comment
    /// once listed the doors it guarded; three more — the ActivityPub inbox,
    /// the trend-exchange fetch and the discovery index — shipped outside that
    /// list, each found only by reading the comment against the code.
    ///
    /// So the list is a gate. It walks every production source file for the
    /// column's writers and requires each enclosing fn to be classified: a
    /// bounded door names its bound, and the bound is held to its production
    /// code; a relay must itself be a writer the walk searches for, so its
    /// callers are classified in turn. A new writer fails here until someone
    /// decides where its value comes from; a removed one fails until its entry
    /// goes.
    #[test]
    fn every_content_created_at_writer_is_partitioned() {
        use std::collections::BTreeSet;
        let found = crate::partition_scan::callers_of(CREATED_AT_WRITERS);

        let table: BTreeSet<(String, String)> = CREATED_AT_CENSUS
            .iter()
            .map(|(file, func, _)| (file.to_string(), func.to_string()))
            .collect();
        assert_eq!(
            table.len(),
            CREATED_AT_CENSUS.len(),
            "a (file, fn) pair is listed twice in CREATED_AT_CENSUS"
        );
        for (file, func, class) in CREATED_AT_CENSUS {
            let (CreatedAt::Bounded(_, why)
            | CreatedAt::Relay(why)
            | CreatedAt::Primitive(why)
            | CreatedAt::Unbounded(why)) = class;
            assert!(
                !why.trim().is_empty(),
                "every census entry states its reason"
            );
            if let CreatedAt::Relay(_) = class {
                let needle = format!("{func}(");
                assert!(
                    CREATED_AT_WRITERS.contains(&needle.as_str()),
                    "{file}::{func} is classified as a relay, but `{needle}` is not one of \
                     CREATED_AT_WRITERS, so nothing enrols its callers — add it, or classify \
                     the fn as what it is"
                );
            }
        }

        let bounded: Vec<(&str, &str, &Gate)> = CREATED_AT_CENSUS
            .iter()
            .filter_map(|(file, func, class)| match class {
                CreatedAt::Bounded(gate, _) => Some((*file, *func, gate)),
                _ => None,
            })
            .collect();
        crate::partition_scan::assert_gates_hold(&bounded);

        crate::partition_scan::assert_partitioned(
            &found,
            &table,
            "This fn writes `content.created_at`, the column the local feed sorts on. Decide \
             where its value comes from and add it to CREATED_AT_CENSUS: a remote-asserted \
             value is Bounded — `reject_future_created_at` for a Fauna-native post, \
             `reject_future_bridged_created_at` for a bridged plane, called before the write — \
             and its door is named in feed.md § The read model; a fn that hands on its caller's \
             value is a Relay and joins CREATED_AT_WRITERS; anything else says why no \
             remote-asserted value reaches the feed.",
        );
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_accepts_well_formed_gated_post_mls() {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::types::{GatedInfo, KeyAccess, MlsGroupId};
        let kp = fauna_core::identity::ActorKeypair::generate();
        let body = signed_gated_post(
            &kp,
            GatedInfo {
                encrypted_ref: ContentHash::from_digest_raw([7u8; 32]),
                key_access: KeyAccess::Room {
                    group_id: MlsGroupId(b"group-1".to_vec()),
                    epoch: 1,
                    generation: None,
                },
                tier: "gold".into(),
                tier_rank: 2,
                seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
                attachment_refs: vec![],
            },
        );
        let outcome = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, PostIngestVerdict::Accepted);
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_accepts_well_formed_gated_post_broadcast() {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::types::{GatedInfo, KeyAccess};
        let kp = fauna_core::identity::ActorKeypair::generate();
        let body = signed_gated_post(
            &kp,
            GatedInfo {
                encrypted_ref: ContentHash::from_digest_raw([8u8; 32]),
                key_access: KeyAccess::Broadcast {
                    key_blob_ref: ContentHash::from_digest_raw([9u8; 32]),
                },
                tier: "silver".into(),
                tier_rank: 1,
                seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
                attachment_refs: vec![],
            },
        );
        let outcome = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, PostIngestVerdict::Accepted);
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_flags_gated_with_zero_encrypted_ref_as_malformed() {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::types::{GatedInfo, KeyAccess, MlsGroupId};
        let kp = fauna_core::identity::ActorKeypair::generate();
        let body = signed_gated_post(
            &kp,
            GatedInfo {
                encrypted_ref: ContentHash::from_digest_raw([0u8; 32]),
                key_access: KeyAccess::Room {
                    group_id: MlsGroupId(b"group-2".to_vec()),
                    epoch: 0,
                    generation: None,
                },
                tier: "gold".into(),
                tier_rank: 2,
                seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
                attachment_refs: vec![],
            },
        );
        let err = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::PostGatedRefZero)
        );
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_flags_gated_with_empty_mls_group_id_as_malformed() {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::types::{GatedInfo, KeyAccess, MlsGroupId};
        let kp = fauna_core::identity::ActorKeypair::generate();
        let body = signed_gated_post(
            &kp,
            GatedInfo {
                encrypted_ref: ContentHash::from_digest_raw([1u8; 32]),
                key_access: KeyAccess::Room {
                    group_id: MlsGroupId(Vec::new()),
                    epoch: 1,
                    generation: None,
                },
                tier: "gold".into(),
                tier_rank: 2,
                seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
                attachment_refs: vec![],
            },
        );
        let err = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::PostKeyAccessInconsistent)
        );
    }

    /// A community room-restricted post's arm, as `build_room_post_at` writes
    /// it: the room's 32-byte channel id and the generation it sealed under.
    fn room_gated(group_id: Vec<u8>) -> fauna_core::subscription::types::GatedInfo {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::types::{GatedInfo, KeyAccess, MlsGroupId};
        GatedInfo {
            encrypted_ref: ContentHash::from_digest_raw([3u8; 32]),
            key_access: KeyAccess::Room {
                group_id: MlsGroupId(group_id),
                epoch: 0,
                generation: Some([4u8; 32]),
            },
            tier: fauna_core::subscription::ROOM_POST_TIER.into(),
            tier_rank: fauna_core::subscription::ROOM_POST_TIER_RANK,
            seal_id: ContentHash::from_digest_raw([5u8; 32]),
            attachment_refs: vec![],
        }
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_accepts_a_community_room_post() {
        let kp = fauna_core::identity::ActorKeypair::generate();
        let body = signed_gated_post(&kp, room_gated(vec![0xC7; 32]));
        let outcome = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, PostIngestVerdict::Accepted);
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_refuses_a_generation_that_names_no_room() {
        // A generation id is a community room's, and a room is named by its
        // 32-byte channel id — so a generation beside anything else names no
        // room any reader could resolve.
        let kp = fauna_core::identity::ActorKeypair::generate();
        let body = signed_gated_post(&kp, room_gated(b"group-1".to_vec()));
        let err = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::PostKeyAccessInconsistent)
        );
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_stores_an_arm_it_does_not_know() {
        // Bidirectional compat: an app newer than this nest may author an arm
        // this nest has never heard of. Refusing it would make the post
        // uncreatable here for no reason the nest can state — the nest holds
        // no key for any gated post, so there is nothing arm-specific for it to
        // check beyond the envelope it already verified.
        use fauna_core::subscription::types::KeyAccess;
        let kp = fauna_core::identity::ActorKeypair::generate();
        let mut gated = room_gated(vec![0xC7; 32]);
        gated.key_access = KeyAccess::Unknown {
            kind: "Sealed".into(),
            payload: fauna_cbor::Value::Map(Default::default()),
        };
        let body = signed_gated_post(&kp, gated);
        let outcome = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, PostIngestVerdict::Accepted);
    }

    #[tokio::test]
    async fn sealed_storage_post_ingest_flags_gated_with_zero_key_blob_ref_as_malformed() {
        use fauna_core::data::ContentHash;
        use fauna_core::subscription::types::{GatedInfo, KeyAccess};
        let kp = fauna_core::identity::ActorKeypair::generate();
        let body = signed_gated_post(
            &kp,
            GatedInfo {
                encrypted_ref: ContentHash::from_digest_raw([1u8; 32]),
                key_access: KeyAccess::Broadcast {
                    key_blob_ref: ContentHash::from_digest_raw([0u8; 32]),
                },
                tier: "silver".into(),
                tier_rank: 1,
                seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
                attachment_refs: vec![],
            },
        );
        let err = sealed_storage()
            .ingest_post(&PostIngestItem {
                uploader: kp.actor_id().0,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::PostKeyAccessInconsistent)
        );
    }

    // ── Channel-envelope ingest: strict verdict shape ─────────────────────────
    //
    // Mirrors the blob + post pipeline shape for the channel-envelope ingest
    // path. The strict impl does dag-cbor-decode of `ChannelEnvelope` +
    // AEAD-shape sanity on the inner bytes (12-byte nonce + 16-byte tag floor;
    // no plaintext-content magic prefix) + roster-membership check, all per
    // the permissive interim documented for item 1(c) (tracked internally).

    fn sealed_storage_with_db(db: std::sync::Arc<crate::db::CacheDb>) -> SealedStorage {
        SealedStorage::new(
            db,
            std::path::PathBuf::from("/tmp/test-acme-channel-ingest"),
        )
    }

    fn aead_shaped_body(seed: u8) -> Vec<u8> {
        // 12-byte nonce + 16-byte tag minimum; pad to 64 bytes of plausible
        // ciphertext+tag. Vary the first byte by seed so different envelopes
        // don't dedup in callers.
        let mut v = vec![seed; 64];
        v[0] = seed.wrapping_add(0x10); // ensure no plaintext-magic prefix
        v
    }

    fn application_envelope_bytes(seed: u8) -> Vec<u8> {
        use fauna_mls::types::ChannelEnvelope;
        ChannelEnvelope::Application(aead_shaped_body(seed))
            .to_bytes()
            .unwrap()
    }

    fn commit_envelope_bytes(seed: u8) -> Vec<u8> {
        use fauna_mls::types::ChannelEnvelope;
        ChannelEnvelope::Commit(aead_shaped_body(seed))
            .to_bytes()
            .unwrap()
    }

    #[tokio::test]
    async fn sealed_storage_channel_ingest_accepts_well_formed_application_from_member() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let sender: [u8; 32] = [0xAAu8; 32];
        let channel: [u8; 32] = [0x11u8; 32];
        db.register_actor_channel(&sender, &channel).await.unwrap();

        let body = application_envelope_bytes(0x42);
        let outcome = sealed_storage_with_db(db)
            .ingest_channel_envelope(&ChannelEnvelopeIngestItem {
                sender,
                channel_id: channel,
                body: &body,
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, ChannelEnvelopeIngestVerdict::Accepted);
    }

    #[tokio::test]
    async fn sealed_storage_channel_ingest_accepts_well_formed_commit_from_member() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let sender: [u8; 32] = [0xBBu8; 32];
        let channel: [u8; 32] = [0x22u8; 32];
        db.register_actor_channel(&sender, &channel).await.unwrap();

        let body = commit_envelope_bytes(0x43);
        let outcome = sealed_storage_with_db(db)
            .ingest_channel_envelope(&ChannelEnvelopeIngestItem {
                sender,
                channel_id: channel,
                body: &body,
            })
            .await
            .unwrap();
        assert_eq!(outcome.verdict, ChannelEnvelopeIngestVerdict::Accepted);
    }

    #[tokio::test]
    async fn sealed_storage_channel_ingest_flags_undecodable_envelope_as_malformed() {
        // Random bytes that don't dag-cbor-decode as ChannelEnvelope.
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let sender: [u8; 32] = [0xCCu8; 32];
        let channel: [u8; 32] = [0x33u8; 32];
        db.register_actor_channel(&sender, &channel).await.unwrap();

        // BARE enums start with a uvarint tag; 0xFF is out of range for a
        // two-variant enum and triggers a decode error.
        let body = vec![0xFFu8; 64];
        let err = sealed_storage_with_db(db)
            .ingest_channel_envelope(&ChannelEnvelopeIngestItem {
                sender,
                channel_id: channel,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::ChannelDecode)
        );
    }

    #[tokio::test]
    async fn sealed_storage_channel_ingest_flags_too_short_inner_as_malformed() {
        use fauna_mls::types::ChannelEnvelope;
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let sender: [u8; 32] = [0xDDu8; 32];
        let channel: [u8; 32] = [0x44u8; 32];
        db.register_actor_channel(&sender, &channel).await.unwrap();

        // Inner bytes shorter than the ChaCha20-Poly1305 floor (12+16=28).
        let body = ChannelEnvelope::Application(vec![0u8; 10])
            .to_bytes()
            .unwrap();
        let err = sealed_storage_with_db(db)
            .ingest_channel_envelope(&ChannelEnvelopeIngestItem {
                sender,
                channel_id: channel,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::ChannelAeadShape)
        );
    }

    #[tokio::test]
    async fn sealed_storage_channel_ingest_flags_png_magic_in_inner_as_malformed() {
        use fauna_mls::types::ChannelEnvelope;
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let sender: [u8; 32] = [0xEEu8; 32];
        let channel: [u8; 32] = [0x55u8; 32];
        db.register_actor_channel(&sender, &channel).await.unwrap();

        let mut inner = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        inner.extend_from_slice(&[0u8; 64]);
        let body = ChannelEnvelope::Application(inner).to_bytes().unwrap();
        let err = sealed_storage_with_db(db)
            .ingest_channel_envelope(&ChannelEnvelopeIngestItem {
                sender,
                channel_id: channel,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::ChannelAeadShape)
        );
    }

    #[tokio::test]
    async fn sealed_storage_channel_ingest_flags_non_member_sender_as_malformed() {
        // Sender is not in actor_channels for this channel — flag as malformed.
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let sender: [u8; 32] = [0x99u8; 32]; // not registered
        let channel: [u8; 32] = [0x66u8; 32];

        let body = application_envelope_bytes(0x44);
        let err = sealed_storage_with_db(db)
            .ingest_channel_envelope(&ChannelEnvelopeIngestItem {
                sender,
                channel_id: channel,
                body: &body,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.kind,
            StorageUnavailableKind::IngestRejected(IngestRejectReason::ChannelRosterMiss)
        );
    }

    // ── ACME fan-out ───────────────────────────────────────────────────────────
    //
    // `SealedStorage::store_acme_material` fans out wrapped `TlsCertBlob`s to
    // each approved bridge with an x25519 pubkey (the MDA listener-bind gate,
    // `mda.go:129-131 TLSProvider is nil`, depends on every approved bridge
    // getting one).
    //
    // Each test seeds two approved bridges (one MTA, one MDA) with x25519
    // pubkeys, calls `store_acme_material`, and asserts each bridge can
    // fetch + unseal the bundle with its own x25519 secret.
    use crate::db::bridge_service_users::BridgeRole;
    use fauna_mls::wrapped_blob::{TlsCertBlob, generate_x25519_keypair, unseal_tls_cert};

    /// Seed an approved bridge in `db` with a fresh x25519 keypair. Returns
    /// `(ed25519_pubkey, x25519_secret)` so the test can fetch+unseal the
    /// resulting `TlsCertBlob` after `store_acme_material` runs.
    async fn seed_approved_bridge(
        db: &Arc<crate::db::CacheDb>,
        role: BridgeRole,
        bridge_id: &str,
    ) -> ([u8; 32], [u8; 32]) {
        // Use deterministic bytes for ed25519 — we never sign with it in this
        // test, the value just keys the row.
        let mut ed: [u8; 32] = [0u8; 32];
        ed[..bridge_id.len().min(32)]
            .copy_from_slice(&bridge_id.as_bytes()[..bridge_id.len().min(32)]);
        // Use a fresh x25519 keypair so the seal target is unique per bridge.
        let (x_sk, x_pk) = generate_x25519_keypair();
        db.create_pending_bridge_service_user(&ed, role, bridge_id)
            .await
            .unwrap();
        db.upsert_bridge_x25519(&ed, &x_pk).await.unwrap();
        db.approve_bridge_service_user(&ed, None).await.unwrap();
        (ed, x_sk)
    }

    fn sample_acme_material<'a>(
        domain: &'a str,
        cert_pem: &'a [u8],
        key_pem: &'a [u8],
    ) -> AcmeMaterial<'a> {
        AcmeMaterial {
            domain,
            cert_chain_pem: cert_pem,
            priv_key_pem: key_pem,
        }
    }

    #[tokio::test]
    async fn sealed_storage_acme_fanout_seals_to_each_approved_bridge() {
        // Sanity-pin the fan-out shape so future refactors can't regress it
        // silently.
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let (_, mta_x_sk) = seed_approved_bridge(&db, BridgeRole::Mta, "mta-1").await;
        let (_, mda_x_sk) = seed_approved_bridge(&db, BridgeRole::Mda, "mda-1").await;

        let sealed = SealedStorage::new(db.clone(), tmp.path().to_path_buf());

        let cert_pem = b"-----BEGIN CERTIFICATE-----\nABC\n-----END CERTIFICATE-----\n";
        let key_pem = b"-----BEGIN PRIVATE KEY-----\nXYZ\n-----END PRIVATE KEY-----\n"; // gitleaks:allow
        sealed
            .store_acme_material(&sample_acme_material(
                "test.example",
                cert_pem.as_slice(),
                key_pem.as_slice(),
            ))
            .await
            .unwrap();

        for (role_str, bridge_id, secret) in
            [("mta", "mta-1", mta_x_sk), ("mda", "mda-1", mda_x_sk)]
        {
            let blob_bytes = db
                .get_tls_cert_blob(role_str, bridge_id, "test.example")
                .await
                .unwrap()
                .unwrap_or_else(|| {
                    panic!("expected TlsCertBlob fan-out for {role_str}/{bridge_id}")
                });
            let blob = TlsCertBlob::from_canonical_bytes(&blob_bytes).unwrap();
            let bundle = unseal_tls_cert(&blob, &secret).unwrap();
            assert_eq!(bundle.cert_chain, cert_pem.to_vec());
            assert_eq!(bundle.priv_key, key_pem.to_vec());
        }
    }

    #[tokio::test]
    async fn sealed_storage_acme_fanout_skips_bridge_without_x25519() {
        // An approved bridge with NULL x25519 must not block other bridges
        // from getting their sealed blob, and must not surface a hard error.
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());

        // Approved bridge WITH x25519 — should receive its blob.
        let (_, mta_x_sk) = seed_approved_bridge(&db, BridgeRole::Mta, "mta-1").await;

        // Approved bridge WITHOUT x25519 — pending → approved without
        // upsert_bridge_x25519, leaving x25519_pubkey NULL.
        let mut no_x25519_ed = [0u8; 32];
        no_x25519_ed[0] = 0xAA;
        db.create_pending_bridge_service_user(&no_x25519_ed, BridgeRole::Mda, "mda-no-x")
            .await
            .unwrap();
        db.approve_bridge_service_user(&no_x25519_ed, None)
            .await
            .unwrap();

        let sealed = SealedStorage::new(db.clone(), tmp.path().to_path_buf());

        let cert_pem = b"cert";
        let key_pem = b"key";
        sealed
            .store_acme_material(&sample_acme_material(
                "skip.example",
                cert_pem.as_slice(),
                key_pem.as_slice(),
            ))
            .await
            .unwrap();

        // The bridge with x25519 got its blob.
        let blob_bytes = db
            .get_tls_cert_blob("mta", "mta-1", "skip.example")
            .await
            .unwrap()
            .expect("approved bridge with x25519 should receive a sealed blob");
        let blob = TlsCertBlob::from_canonical_bytes(&blob_bytes).unwrap();
        let bundle = unseal_tls_cert(&blob, &mta_x_sk).unwrap();
        assert_eq!(bundle.cert_chain, cert_pem.to_vec());
        assert_eq!(bundle.priv_key, key_pem.to_vec());

        // The bridge without x25519 was skipped (no blob stored, no panic).
        assert!(
            db.get_tls_cert_blob("mda", "mda-no-x", "skip.example")
                .await
                .unwrap()
                .is_none(),
            "approved bridge with NULL x25519 should be skipped, not crash"
        );
    }

    #[tokio::test]
    async fn seal_on_read_delivers_cert_to_bridge_attested_after_issuance() {
        // The regression guard for the live example.com failure: ACME issued the
        // real cert (PEM on disk) BEFORE the mail bridge attested its x25519,
        // so the eager `store_acme_material` fan-out skipped it and no blob was
        // ever stored — the bridge's `fetch_tls_cert_blob` returned nil, its
        // TLS listener never bound, and IMAPS(993) stayed refused. Seal-on-read
        // must close that gap: the cert on disk is sealed to the bridge at
        // fetch time regardless of attestation order.
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());

        let storage = SealedStorage::new(db.clone(), tmp.path().to_path_buf());

        // (1) ACME issues while NO bridge is approved yet: writes PEM to disk,
        //     fans out to nobody.
        let cert_pem = b"-----BEGIN CERTIFICATE-----\nREAL\n-----END CERTIFICATE-----\n";
        let key_pem = b"-----BEGIN PRIVATE KEY-----\nREAL\n-----END PRIVATE KEY-----\n"; // gitleaks:allow
        storage
            .store_acme_material(&sample_acme_material(
                "fauna.example",
                cert_pem.as_slice(),
                key_pem.as_slice(),
            ))
            .await
            .unwrap();

        // (2) The mail bridge enrolls, attests x25519, and is approved AFTER
        //     issuance. The eager fan-out never ran for it, so no blob exists —
        //     this is exactly the pre-fix dead end.
        let (_, mta_x_sk) = seed_approved_bridge(&db, BridgeRole::Mta, "mta-1").await;
        assert!(
            db.get_tls_cert_blob("mta", "mta-1", "fauna.example")
                .await
                .unwrap()
                .is_none(),
            "precondition: no eager blob for a bridge that attested after issuance"
        );

        // (3) Seal-on-read (the fetch path) hands the bridge the on-disk cert.
        let bytes = storage
            .seal_current_tls_cert_for_bridge("mta", "mta-1", "fauna.example")
            .await
            .unwrap()
            .expect("seal-on-read must deliver the on-disk cert despite late attestation");
        let blob = TlsCertBlob::from_canonical_bytes(&bytes).unwrap();
        let bundle = unseal_tls_cert(&blob, &mta_x_sk).unwrap();
        assert_eq!(bundle.cert_chain, cert_pem.to_vec());
        assert_eq!(bundle.priv_key, key_pem.to_vec());

        // And it persisted the blob as a cache for the plain `get` path too.
        assert!(
            db.get_tls_cert_blob("mta", "mta-1", "fauna.example")
                .await
                .unwrap()
                .is_some(),
            "seal-on-read should also persist the freshly-sealed blob"
        );
    }

    #[tokio::test]
    async fn seal_on_read_returns_none_when_no_cert_on_disk() {
        // With no PEM on disk yet (fresh nest pre-ACME), seal-on-read is a
        // no-op so the fetch handler falls back to any stored blob / nil.
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let _ = seed_approved_bridge(&db, BridgeRole::Mta, "mta-1").await;

        let storage = SealedStorage::new(db.clone(), tmp.path().to_path_buf());

        assert!(
            storage
                .seal_current_tls_cert_for_bridge("mta", "mta-1", "fauna.example")
                .await
                .unwrap()
                .is_none(),
            "no cert on disk → seal-on-read yields None (caller falls back)"
        );
    }
}
