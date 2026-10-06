//! seal_for_audience + SealedBlob.

use fauna_core::crypto::encrypt_backup_chunk;
use fauna_core::subscription::crypto::{derive_post_key, encrypt_content};
use fauna_mls::blob_crypto::encrypt_blob;

use crate::audience::{Audience, AudienceClass, RestrictedPostAudience};

/// A blob sealed under the audience's per-blob key.
///
/// `class` is the dispatch tag the nest's eventual strict-verifier reads to
/// pick which AEAD envelope shape to verify. PublicPost variants carry the
/// raw bytes unchanged (no seal); every other variant's `bytes` is an AEAD
/// envelope per the spec's dispatch table.
#[derive(Debug, Clone)]
pub struct SealedBlob {
    pub bytes: Vec<u8>,
    pub class: AudienceClass,
}

/// Seal `plaintext` under the audience's per-blob key.
///
/// Dispatches by audience variant — see the design spec § Upload entrypoint
/// contract for the full table.
pub fn seal_for_audience(audience: &Audience, plaintext: &[u8]) -> SealedBlob {
    match audience {
        Audience::Library { backup_key } => {
            let bytes = encrypt_backup_chunk(backup_key, plaintext)
                .expect("encrypt_backup_chunk only fails on AEAD-internal panics");
            SealedBlob {
                bytes,
                class: AudienceClass::Library,
            }
        }
        Audience::Conversation { epoch_secret, .. } => {
            let bytes = encrypt_blob(epoch_secret.as_ref(), plaintext)
                .expect("encrypt_blob only fails on AEAD-internal panics");
            SealedBlob {
                bytes,
                class: AudienceClass::Conversation,
            }
        }
        Audience::RestrictedPost {
            post_id,
            audience: RestrictedPostAudience::Group { epoch_secret, .. },
        } => {
            let per_post_key = derive_post_key(epoch_secret, post_id);
            let bytes = encrypt_content(&per_post_key, plaintext);
            SealedBlob {
                bytes,
                class: AudienceClass::GroupRestrictedPost,
            }
        }
        Audience::RestrictedPost {
            post_id,
            audience: RestrictedPostAudience::Period { period_key, .. },
        } => {
            let per_post_key = derive_post_key(period_key, post_id);
            let bytes = encrypt_content(&per_post_key, plaintext);
            SealedBlob {
                bytes,
                class: AudienceClass::PeriodRestrictedPost,
            }
        }
        // The two plaintext audiences share one arm and one wire class: a blob
        // that rests in the clear has exactly one shape, whatever made it
        // public. They differ only in why (a post's signature vs. the owner's
        // folder declassification), which the variant records for the reader.
        Audience::PublicPost { .. } | Audience::PublicFolder { .. } => SealedBlob {
            bytes: plaintext.to_vec(),
            class: AudienceClass::PublicPost,
        },
    }
}
