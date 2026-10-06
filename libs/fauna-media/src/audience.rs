//! Audience enum + AudienceClass tag. See `seal.rs` for the dispatch.

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::subscription::types::MlsGroupId;
use fauna_mls::types::ChannelId;
use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroizing;

/// The five audience classes the seal layer dispatches across.
///
/// See `docs/goal/architecture/encryption-at-rest.md` Media row for the
/// canonical taxonomy. `AudienceClass` carries no key material — it is the
/// dispatch tag the nest's eventual strict-verifier reads from `SealedBlob` /
/// `UploadSidecar` to pick which AEAD envelope shape to verify.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudienceClass {
    Library,
    Conversation,
    GroupRestrictedPost,
    PeriodRestrictedPost,
    PublicPost,
}

impl AudienceClass {
    /// Whether `seal_for_audience` produces AEAD ciphertext for this class —
    /// true for every class except [`AudienceClass::PublicPost`], whose bytes
    /// pass through unchanged as signed plaintext.
    ///
    /// Drives the `UploadSidecar` the uploader declares: a ciphertext blob's
    /// sidecar MUST be `mime = "application/octet-stream"` + `has_c2pa = false`
    /// (the real MIME / C2PA flag ride *inside* the sealed bytes), which the
    /// nest's strict verifier enforces — any other sidecar MIME for a
    /// sealed class is rejected (`classify_per_class_envelope` in
    /// `bins/fauna-nest/src/storage/sealed.rs` → `mime_class_mismatch`).
    /// Only a `PublicPost` blob is plaintext, so its sidecar carries the real
    /// sniffed MIME (the Content-Type the nest serves on download).
    pub fn is_aead_sealed(&self) -> bool {
        !matches!(self, AudienceClass::PublicPost)
    }
}

/// The audience for a single blob upload.
///
/// Each variant bundles its identifier(s) + the key material `seal_for_audience`
/// needs. Owning the key bytes (wrapped in `Zeroizing`) avoids lifetime
/// annotations and gets on-drop wipe for free; the type system prevents
/// pairing the wrong key with the wrong audience class.
///
/// `Debug` is implemented manually: key material is redacted as `[REDACTED]`
/// to prevent accidental log exposure. `BackupKey` intentionally does not
/// derive `Debug` for the same reason.
pub enum Audience {
    /// Owner-only library media — sealed under the owner's `BackupKey`.
    Library { backup_key: BackupKey },
    /// Conversation-attached media — sealed under the conversation channel's
    /// MLS `derive_blob_key(epoch_secret)`.
    Conversation {
        channel_id: ChannelId,
        epoch_secret: Zeroizing<[u8; 32]>,
    },
    /// Group-restricted or audience-restricted post-attached media — sealed
    /// under `derive_post_key` from the surrounding post's per-content seal.
    RestrictedPost {
        post_id: ContentHash,
        audience: RestrictedPostAudience,
    },
    /// Public-post-attached media — no seal; bytes pass through unchanged
    /// (the post's signature attests to the blob hash).
    PublicPost { post_id: ContentHash },
    /// Content uploaded into an **owner-declassified folder** — no seal; bytes
    /// pass through unchanged, exactly as [`Audience::PublicPost`] does, and
    /// under the same [`AudienceClass::PublicPost`] wire tag (a plaintext blob
    /// is one shape on the wire; this variant adds no class, so nothing the
    /// nest verifies or a reader dispatches on changes).
    ///
    /// Distinct from `PublicPost` at the *call* site because the reason differs
    /// and only the reason is checkable: a public post's blob is plaintext
    /// because a signature attests to it, while these bytes are plaintext
    /// because their owner declassified the folder they rest in — the one
    /// exception to sealed-by-default (`encryption-at-rest.md` § Readable
    /// classes → *Owner-flipped public-audience folders*). Naming it rather
    /// than borrowing `PublicPost { post_id }` keeps a caller from having to
    /// invent a post id for a file that belongs to no post.
    ///
    /// The producer asks `FolderSummary::judge_declassification` (the owner's
    /// attestation, verified) — never an audience string of its own — and
    /// passes this only on an unsealed verdict.
    PublicFolder { folder_id: i64 },
}

pub enum RestrictedPostAudience {
    /// Group-restricted post — base key is the community group's MLS epoch
    /// secret; per-post key is `derive_post_key(group_epoch_secret, post_id)`.
    Group {
        group_id: MlsGroupId,
        epoch_secret: Zeroizing<[u8; 32]>,
    },
    /// Audience-restricted (subscription tier × period) post — base key is the
    /// period_key reaching subscribers via the post's `KeyBlob`; per-post key
    /// is `derive_post_key(period_key, post_id)`.
    Period {
        tier: String,
        period_epoch: u64,
        period_key: Zeroizing<[u8; 32]>,
    },
}

/// Manual `Debug` impl: `epoch_secret` and `period_key` are Zeroizing-wrapped
/// key material — redact them to prevent accidental log exposure.
impl fmt::Debug for RestrictedPostAudience {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestrictedPostAudience::Group { group_id, .. } => f
                .debug_struct("RestrictedPostAudience::Group")
                .field("group_id", group_id)
                .field("epoch_secret", &"[REDACTED]")
                .finish(),
            RestrictedPostAudience::Period {
                tier, period_epoch, ..
            } => f
                .debug_struct("RestrictedPostAudience::Period")
                .field("tier", tier)
                .field("period_epoch", period_epoch)
                .field("period_key", &"[REDACTED]")
                .finish(),
        }
    }
}

/// Manual `Debug` impl: `BackupKey` intentionally does not implement `Debug`
/// (avoids accidental key-material exposure in logs). We redact it explicitly.
impl fmt::Debug for Audience {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Audience::Library { .. } => f
                .debug_struct("Audience::Library")
                .field("backup_key", &"[REDACTED]")
                .finish(),
            Audience::Conversation { channel_id, .. } => f
                .debug_struct("Audience::Conversation")
                .field("channel_id", channel_id)
                .field("epoch_secret", &"[REDACTED]")
                .finish(),
            Audience::RestrictedPost { post_id, audience } => f
                .debug_struct("Audience::RestrictedPost")
                .field("post_id", post_id)
                .field("audience", audience)
                .finish(),
            Audience::PublicPost { post_id } => f
                .debug_struct("Audience::PublicPost")
                .field("post_id", post_id)
                .finish(),
            // No key material to redact — these bytes rest in the clear by the
            // owner's own choice — so the folder id prints, like a post id.
            Audience::PublicFolder { folder_id } => f
                .debug_struct("Audience::PublicFolder")
                .field("folder_id", folder_id)
                .finish(),
        }
    }
}

impl Audience {
    pub fn class(&self) -> AudienceClass {
        match self {
            Audience::Library { .. } => AudienceClass::Library,
            Audience::Conversation { .. } => AudienceClass::Conversation,
            Audience::RestrictedPost {
                audience: RestrictedPostAudience::Group { .. },
                ..
            } => AudienceClass::GroupRestrictedPost,
            Audience::RestrictedPost {
                audience: RestrictedPostAudience::Period { .. },
                ..
            } => AudienceClass::PeriodRestrictedPost,
            // Both plaintext audiences report the one plaintext wire class: the
            // class is what the nest verifies and a reader dispatches on, and a
            // declassified folder's blob has the same shape as a public post's.
            Audience::PublicPost { .. } | Audience::PublicFolder { .. } => {
                AudienceClass::PublicPost
            }
        }
    }
}
