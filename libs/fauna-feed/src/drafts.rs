//! The **posts rail**'s at-rest draft shape — the feed composer's half of
//! draft-persistence v2 (`docs/goal/behavior/reserved-folders.md` § Drafts
//! Sync; `docs/goal/ui/feed.md` § Encryption at rest).
//!
//! The twin of `fauna_conversations::DraftStore` for the `"posts"` rail of
//! `fauna_protocol::drafts::DRAFT_RAILS`: it serialises the composer's
//! in-progress state to canonical bytes the client glue seals under the owner's
//! `BackupKey` and PUTs to `__drafts`, and restores them on launch. The seal,
//! the `fauna.drafts.{get,put}` calls, the launch gate and the last-saved
//! baseline are **not** here — they live once in `fauna_client_drafts`.
//!
//! **Single-slot, no draft ids.** `feed.md`:452 left three questions to this
//! crate's design; the third — "how draft IDs are assigned" — resolves to *they
//! aren't*: the feed composer is one slot on the manager
//! ([`crate::FeedSnapshot::compose`]), the same single `"feed-compose"` key the
//! windows leg already used for its local store. So where the conversations
//! rail's snapshot carries a sorted `Vec` keyed by `ThreadId` plus a new-thread
//! slot, this rail carries one record and needs no ordering rule to be
//! byte-stable.
//!
//! **Only user-authored input rests; transient UI state does not** — which is
//! this rail's answer to `feed.md`'s first question ("which fields of
//! `FeedComposeState` survive serialisation"). [`FeedComposeState::error`] and
//! [`FeedComposeState::submitting`] describe *this app run's* attempt to post,
//! not what the user wrote, and restoring them would be a live defect rather
//! than a cosmetic one: a snapshot captured mid-submit would come back with
//! `submitting = true` and leave the composer disabled forever, with no gesture
//! that clears it. [`PostDrafts`] therefore enumerates the persisted fields
//! explicitly instead of embedding `FeedComposeState` wholesale, so a field
//! added to the composer later cannot become at-rest data by default — the
//! author has to choose.
//!
//! ⚠ **The conversations rail did NOT hold this property when this rail opened**
//! — it embedded its whole `ComposeState`, including the transient
//! `SendState::Sending` (priority #4: resolve drift toward the correct shape,
//! don't replicate it). That defect is **fixed as of 2026-08-16**, by a
//! different mechanism: its blob had already shipped, so it could not enumerate
//! a fresh record without blinding un-updated apps, and it normalises through a
//! constructor in `fauna_conversations::store::drafts` instead. Both rails now
//! rest user-authored input only.
//!
//! **Attachments are referenced by content address, never by bytes** —
//! `feed.md`'s second question. [`AttachedFile`] already holds only
//! `{name, size, blob_hash, media_type}`, exactly as the conversations rail's
//! `AttachmentDraft` does. On this rail the blob is uploaded only by the submit
//! itself (`ui/media.md` § Encryption at rest), so a resting draft's file is
//! almost always `blob_hash: None` — a *handle*, not a file: the name and size
//! are what the user sees on the composer, and the bytes live only on the
//! device that picked them. A draft restored after a relaunch, or on another
//! device, therefore names a file that device does not hold, and the submit
//! **refuses** it by name rather than posting the text alone
//! (`FeedManager::refuse_unresolved_attachment`; `feed.md` § Persistence).

use serde::{Deserialize, Serialize};

use fauna_core::encoding::{canonical_decode, canonical_encode};

use crate::compose::{AttachedFile, FeedComposeState, SellComposeState};

/// The serialised at-rest form of the feed composer — the whole posts-rail
/// draft set as one record, which for this rail is one compose slot. These are
/// the bytes sealed under the owner's `BackupKey` and stored in `__drafts` at
/// `path = "posts"` (`reserved-folders.md` § Drafts Sync step 1).
///
/// Every field is user-authored input. `#[serde(default)]` throughout so a blob
/// written by an older client (or a newer one that grew a field) still restores
/// — additive-everywhere evolution, per `version-compatibility.md`.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct PostDrafts {
    /// `compose-text-field`.
    #[serde(default)]
    pub text: String,
    /// `compose-tags-field`, raw as typed (normalization happens at submit).
    #[serde(default)]
    pub tags: String,
    /// The staged file's metadata + content address — never its bytes.
    #[serde(default)]
    pub attached_file: Option<AttachedFile>,
    /// `compose-gate-tier-select`: `None` = Public.
    #[serde(default)]
    pub gate_tier: Option<String>,
    /// `compose-gate-preview-field` — the public teaser of a gated post.
    #[serde(default)]
    pub gate_preview: String,
    /// Sell mode's parameters; `Some` *is* sell mode.
    #[serde(default)]
    pub sell: Option<SellComposeState>,
    /// `compose-gate-tier-select`'s room answer (hex channel id) — the audience
    /// the user chose, so it rests like `gate_tier`. Skipped when absent, so a
    /// draft with no room keeps the exact bytes it had before the field
    /// existed (the unchanged-set dedup above); an older client restoring one
    /// that has it drops the field and restores the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_room: Option<String>,
}

/// A persisted posts-rail blob could not be restored. The caller (the manager)
/// treats this as "no drafts" and keeps the empty composer rather than failing
/// the surface — a corrupt or newer-shape blob must never break composing.
#[derive(Debug, thiserror::Error)]
pub enum PostDraftRestoreError {
    #[error("decode posts drafts snapshot: {0}")]
    Decode(String),
}

impl PostDrafts {
    /// Capture the persistable half of a live composer state, dropping the
    /// transient fields (see the module docs).
    pub fn from_compose(compose: &FeedComposeState) -> Self {
        Self {
            text: compose.text.clone(),
            tags: compose.tags.clone(),
            attached_file: compose.attached_file.clone(),
            gate_tier: compose.gate_tier.clone(),
            gate_preview: compose.gate_preview.clone(),
            sell: compose.sell.clone(),
            gate_room: compose.gate_room.clone(),
        }
    }

    /// Apply a restored draft onto a composer state, leaving the transient
    /// fields at their live values. Used on the load-on-launch path, where the
    /// composer is freshly default — so `error`/`submitting` stay cleared
    /// rather than being resurrected from the blob (which never held them).
    pub fn apply_to(self, compose: &mut FeedComposeState) {
        compose.text = self.text;
        compose.tags = self.tags;
        compose.attached_file = self.attached_file;
        compose.gate_tier = self.gate_tier;
        compose.gate_preview = self.gate_preview;
        compose.sell = self.sell;
        compose.gate_room = self.gate_room;
    }

    /// Serialise to the canonical at-rest bytes. Byte-stable for equal logical
    /// state — the property that makes `DraftsSync`'s unchanged-set dedup work
    /// and "byte-equal drafts" a meaningful cross-device assertion. This rail
    /// needs no sort to get there (single slot; see the module docs).
    pub fn snapshot_bytes(&self) -> Vec<u8> {
        // Encoding our own owned types is infallible in practice; surface a
        // clear panic rather than threading a Result through every caller
        // (same call as the conversations rail's `snapshot_bytes`).
        canonical_encode(self).expect("canonical_encode PostDrafts")
    }

    /// Restore from bytes produced by [`Self::snapshot_bytes`] (after the client
    /// glue unseals them with the owner's `BackupKey`).
    pub fn restore_from_bytes(bytes: &[u8]) -> Result<Self, PostDraftRestoreError> {
        canonical_decode(bytes).map_err(|e| PostDraftRestoreError::Decode(e.to_string()))
    }

    /// Whether this draft holds anything worth restoring. A blob that decodes to
    /// an all-empty record is indistinguishable from no draft at all, and the
    /// composer's own default is already that — so the manager skips the notify.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::crypto::{BackupKey, decrypt_backup_chunk, encrypt_backup_chunk};
    use fauna_core::localized::LocalizedText;

    fn sample_compose() -> FeedComposeState {
        FeedComposeState {
            text: "half a thought".into(),
            tags: "walrus, quarterly".into(),
            attached_file: Some(AttachedFile {
                name: "a.png".into(),
                size: 42,
                blob_hash: Some("abc123".into()),
                media_type: Some("image/png".into()),
            }),
            gate_tier: Some("supporters".into()),
            gate_preview: "the public teaser".into(),
            sell: None,
            gate_room: None,
            error: None,
            submitting: false,
        }
    }

    #[test]
    fn a_room_answer_rests_and_comes_back() {
        let room = "c7".repeat(32);
        let compose = FeedComposeState {
            gate_tier: None,
            gate_room: Some(room.clone()),
            ..sample_compose()
        };
        let bytes = PostDrafts::from_compose(&compose).snapshot_bytes();
        let mut restored = FeedComposeState::default();
        PostDrafts::restore_from_bytes(&bytes)
            .expect("restore")
            .apply_to(&mut restored);
        assert_eq!(restored.gate_room, Some(room));
    }

    /// A draft with no room carries no `gate_room` key at all, so every draft
    /// written before the field existed re-encodes to the bytes it already had
    /// — no spurious "changed" upload to the user's other devices.
    #[test]
    fn a_draft_with_no_room_encodes_no_room_key() {
        let bytes = PostDrafts::from_compose(&sample_compose()).snapshot_bytes();
        let needle = b"gate_room";
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "an absent room must not reach the at-rest bytes"
        );
    }

    #[test]
    fn snapshot_restore_round_trips_every_authored_field() {
        let bytes = PostDrafts::from_compose(&sample_compose()).snapshot_bytes();

        let mut restored = FeedComposeState::default();
        PostDrafts::restore_from_bytes(&bytes)
            .expect("restore")
            .apply_to(&mut restored);

        assert_eq!(restored.text, "half a thought");
        assert_eq!(restored.tags, "walrus, quarterly");
        assert_eq!(
            restored
                .attached_file
                .as_ref()
                .unwrap()
                .blob_hash
                .as_deref(),
            Some("abc123"),
            "an attachment is carried by content address, so it resolves on \
             another device too",
        );
        assert_eq!(restored.attached_file.as_ref().unwrap().name, "a.png");
        assert_eq!(restored.gate_tier.as_deref(), Some("supporters"));
        assert_eq!(restored.gate_preview, "the public teaser");
    }

    #[test]
    fn sell_mode_survives_with_its_rank_knob() {
        let compose = FeedComposeState {
            gate_tier: None,
            sell: Some(SellComposeState {
                price: "5 EUR".into(),
                asking_price: String::new(),
                // The non-default answer, so a restore that silently rebuilt a
                // `SellComposeState::default()` would fail here.
                subscribers_get_it_free: false,
            }),
            ..sample_compose()
        };
        let bytes = PostDrafts::from_compose(&compose).snapshot_bytes();

        let mut restored = FeedComposeState::default();
        PostDrafts::restore_from_bytes(&bytes)
            .expect("restore")
            .apply_to(&mut restored);

        let sell = restored.sell.expect("sell mode must survive");
        assert_eq!(sell.price, "5 EUR");
        assert!(!sell.subscribers_get_it_free);
    }

    /// The load-bearing exclusion: an in-flight submit is this run's state, not
    /// the user's writing. Persisting `submitting` would restore a composer that
    /// is disabled with no gesture to clear it.
    #[test]
    fn transient_ui_state_does_not_rest() {
        let mid_submit = FeedComposeState {
            submitting: true,
            error: Some(LocalizedText::key("feed.error_submit")),
            ..sample_compose()
        };
        let bytes = PostDrafts::from_compose(&mid_submit).snapshot_bytes();

        let mut restored = FeedComposeState::default();
        PostDrafts::restore_from_bytes(&bytes)
            .expect("restore")
            .apply_to(&mut restored);

        assert!(
            !restored.submitting,
            "a draft captured mid-submit must not come back disabled",
        );
        assert!(
            restored.error.is_none(),
            "a stale failure from a previous run is not a draft",
        );
        assert_eq!(
            restored.text, "half a thought",
            "the writing still survives"
        );
    }

    /// Byte-stability is what lets `DraftsSync` skip an unchanged upload.
    #[test]
    fn snapshot_bytes_is_stable_for_equal_logical_state() {
        let a = PostDrafts::from_compose(&sample_compose()).snapshot_bytes();
        let b = PostDrafts::from_compose(&sample_compose()).snapshot_bytes();
        assert_eq!(a, b);

        // ...and equal after a round trip, which is the form the dedup baseline
        // actually compares (loaded bytes vs. freshly-serialised state).
        let mut back = FeedComposeState::default();
        PostDrafts::restore_from_bytes(&a)
            .expect("restore")
            .apply_to(&mut back);
        assert_eq!(PostDrafts::from_compose(&back).snapshot_bytes(), a);
    }

    /// Two composers differing only in a transient field must produce the SAME
    /// bytes — otherwise every submit attempt would push a no-op upload to the
    /// user's other devices.
    #[test]
    fn a_transient_change_alone_produces_no_new_bytes() {
        let idle = PostDrafts::from_compose(&sample_compose()).snapshot_bytes();
        let submitting = PostDrafts::from_compose(&FeedComposeState {
            submitting: true,
            ..sample_compose()
        })
        .snapshot_bytes();
        assert_eq!(idle, submitting);
    }

    /// The full at-rest path, mirroring the conversations rail's proof:
    /// serialise → seal under `BackupKey` → unseal → restore → byte-equal.
    #[test]
    fn seal_unseal_round_trips_byte_equal() {
        let key = BackupKey::derive(&[7u8; 32]);
        let plain = PostDrafts::from_compose(&sample_compose()).snapshot_bytes();

        let sealed = encrypt_backup_chunk(&key, &plain).unwrap();
        // The nest stores exactly these, opaque: ciphertext carrying the
        // ChaCha20 `0x01` version byte, never the plaintext.
        assert_ne!(sealed, plain);
        assert_eq!(sealed[0], 0x01);

        let unsealed = decrypt_backup_chunk(&key, &sealed).unwrap();
        assert_eq!(unsealed, plain);
        assert_eq!(
            PostDrafts::restore_from_bytes(&unsealed)
                .expect("restore")
                .snapshot_bytes(),
            plain,
        );
    }

    #[test]
    fn restore_rejects_garbage() {
        let err = PostDrafts::restore_from_bytes(&[0xFF, 0xFF, 0xFF]).unwrap_err();
        assert!(matches!(err, PostDraftRestoreError::Decode(_)));
    }

    #[test]
    fn an_empty_composer_round_trips_and_reports_empty() {
        let bytes = PostDrafts::from_compose(&FeedComposeState::default()).snapshot_bytes();
        let restored = PostDrafts::restore_from_bytes(&bytes).expect("restore");
        assert!(restored.is_empty());
        assert_eq!(restored.snapshot_bytes(), bytes);
    }

    #[test]
    fn a_draft_with_text_is_not_empty() {
        assert!(!PostDrafts::from_compose(&sample_compose()).is_empty());
    }
}
