//! The at-rest seal checks for the per-user mail-spam model and its training
//! history (`docs/goal/behavior/mail-spam.md` § Encrypted-mode interaction).
//!
//! The per-user model rests ONLY sealed to the actor's recipient key, and the
//! nest holds only the public half: it can seal *to* the actor (the cold-start
//! seed `fetch_spam_model` seals on read) but can never read, train, undo or
//! merge a stored model. Every mutation runs at a capability holder — the
//! user's client or the AUTH'd MDA session — and lands through
//! `fauna.bridges.put_spam_model`, which uses these checks to refuse a
//! plaintext model blob or a plaintext training delta. Content detection, not
//! a mode branch: the nest cannot verify a seal it cannot open, but it can
//! always tell that bytes ARE the plaintext shape, and refuses those.

use std::collections::BTreeSet;

use fauna_mail::spam::SpamModel;

/// Slack a seal's envelope (HPKE encapsulation, tag, dag-cbor framing) adds on
/// top of the plaintext it wraps. The nest cannot open a seal, so every opaque
/// field is bounded by its plaintext bound plus this.
pub const SEAL_ENVELOPE_SLACK: usize = 4 * 1024;

/// Cap on a sealed model blob and on a holder's re-sealed copy: the model's own
/// `cap_to_bytes` bound plus the envelope.
pub const SEALED_MODEL_MAX_BYTES: usize =
    fauna_mail::spam::MODEL_MAX_BYTES_DEFAULT + SEAL_ENVELOPE_SLACK;

/// Cap on a history row's sealed delta. A delta that could not fit in a model
/// under its own cap is of no use to an undo, so the model bound applies.
pub const SEALED_DELTA_MAX_BYTES: usize =
    fauna_mail::spam::MODEL_MAX_BYTES_DEFAULT + SEAL_ENVELOPE_SLACK;

/// Cap on a history row's sealed subject: one subject line plus the envelope.
pub const SEALED_SUBJECT_MAX_BYTES: usize = 4 * 1024;

/// Cap on a history row's opaque `message_id`: the mail path's 32-byte content
/// id and the conversation path's id string (`conv:<64 hex>:<seq>`, or a
/// bridged `<row@bridged>`) both fit; the id is never hashed or interpreted.
pub const HISTORY_MESSAGE_ID_MAX_BYTES: usize = 256;

/// Per-actor ceiling on `spam_training_history` rows. The insert prunes the
/// actor's oldest rows beyond it in the same transaction (a rate limit would
/// silently drop lessons from a bulk Junk move).
pub const SPAM_HISTORY_MAX_ROWS_PER_ACTOR: i64 = 10_000;

/// True iff `bytes` is a **non-empty** blob that does not decode as a plaintext
/// `SpamModel` — i.e. an opaque sealed model. An empty blob is not sealed; a
/// valid plaintext model decodes; a sealed `wrapped_blob` (dag-cbor, not the
/// model's serde_json) fails to decode → `true`.
pub fn is_sealed_model_blob(bytes: &[u8]) -> bool {
    !bytes.is_empty() && SpamModel::from_bytes(bytes).is_none()
}

/// True iff `bytes` decodes as a plaintext training delta — the serde_json of
/// the distinct n-gram set an event added (`SpamModel::delta_ngrams`), the
/// shape the retired server-side train wrote into
/// `spam_training_history.model_delta_applied`. A sealed delta (an opaque
/// `wrapped_blob`) never does.
pub fn is_plaintext_delta(bytes: &[u8]) -> bool {
    serde_json::from_slice::<BTreeSet<String>>(bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_sealed_model_blob_distinguishes_plaintext_from_opaque() {
        // Empty ⇒ NOT sealed.
        assert!(!is_sealed_model_blob(&[]));
        // A valid plaintext SpamModel decodes ⇒ NOT sealed.
        let plaintext = SpamModel::new().to_bytes();
        assert!(!is_sealed_model_blob(&plaintext));
        // Opaque bytes (a sealed `wrapped_blob` is dag-cbor, not the model's
        // serde_json) do NOT decode ⇒ sealed.
        assert!(is_sealed_model_blob(&[0xEEu8; 300]));
    }

    #[test]
    fn is_plaintext_delta_detects_the_ngram_set_shape() {
        let delta = SpamModel::delta_ngrams("buy cheap pills now");
        assert!(is_plaintext_delta(&serde_json::to_vec(&delta).unwrap()));
        assert!(is_plaintext_delta(b"[]"));
        assert!(!is_plaintext_delta(&[0xEEu8; 64]));
        assert!(!is_plaintext_delta(&[]));
    }
}
