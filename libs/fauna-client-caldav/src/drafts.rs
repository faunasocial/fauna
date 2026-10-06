//! The **events rail**'s at-rest draft shape — the event composer's third of
//! draft-persistence v2 (`docs/goal/behavior/reserved-folders.md` § Drafts
//! Sync; `docs/goal/ui/events.md` § Persistence).
//!
//! The twin of `fauna_feed::drafts::PostDrafts` and
//! `fauna_conversations::DraftStore` for the `"events"` rail of
//! `fauna_protocol::drafts::DRAFT_RAILS` — the third and last constant of that
//! closed vocabulary, which the plane accepted from the start and no app wrote
//! until this module. It serialises the `event-form` composer's in-progress
//! state to canonical bytes the client glue seals under the owner's `BackupKey`
//! and PUTs to `__drafts`, and restores them on launch. The seal, the
//! `fauna.drafts.{get,put}` calls, the launch gate and the last-saved baseline
//! are **not** here — they live once in `fauna_client_drafts`.
//!
//! **Single-slot, no draft ids** — the same answer the posts rail reached. The
//! event composer is one modal surface (tui's `Mode::CreateEvent`, and the
//! `event-form` component in `ui.yaml`), not a per-thread map like the
//! conversations rail, so the blob is one record and needs no ordering rule to
//! be byte-stable.
//!
//! **Only user-authored input rests, and the fields are enumerated** rather
//! than a compose struct embedded wholesale — so a field added to some app's
//! event form later cannot become at-rest data by default; the author has to
//! choose. That is the rule the posts rail ratified and the conversations rail
//! was retrofitted to in 2026-08-16 (`ui/feed.md` § Persistence).
//!
//! ⚠ **The calendar is deliberately NOT at rest.** `ui.yaml`'s `event-form`
//! carries no calendar picker: the target calendar comes from the page's
//! selected calendar (with a first-calendar fallback) at submit time, and a
//! page's selected calendar is per-device view state — the same call
//! `ui/feed.md` § Persistence makes for the selected feed. Persisting one would
//! also let a draft name a calendar the restoring device does not have.
//!
//! **Datetimes rest raw as typed.** `dtstart`/`dtend` are the composer's text
//! exactly as the user left it, mid-edit and possibly not yet well-formed;
//! normalization to the wire's `…THH:MM:SS` shape stays where it already is, at
//! submit (`events.md` § Where logic lives, the A2 rule). A draft is what the
//! user wrote, not what the wire would accept.

use serde::{Deserialize, Serialize};

use fauna_client_drafts::{DraftsClientError, DraftsSync};
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_protocol::RpcRequester;

/// The serialised at-rest form of the event composer — the whole events-rail
/// draft set as one record, which for this rail is one compose slot. These are
/// the bytes sealed under the owner's `BackupKey` and stored in `__drafts` at
/// `path = "events"` (`reserved-folders.md` § Drafts Sync step 1).
///
/// Every field is user-authored `event-form` input. `#[serde(default)]`
/// throughout so a blob written by an older client (or a newer one that grew a
/// field) still restores — additive-everywhere evolution, per
/// `version-compatibility.md`.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
pub struct EventDrafts {
    /// `event-summary` — the VEVENT `SUMMARY`.
    #[serde(default)]
    pub summary: String,
    /// `event-form-start-datetime`, raw as typed (see the module docs).
    #[serde(default)]
    pub dtstart: String,
    /// `event-form-end-datetime`, raw as typed.
    #[serde(default)]
    pub dtend: String,
    /// `event-form-description` — the VEVENT `DESCRIPTION`.
    #[serde(default)]
    pub description: String,
    /// `event-form-location` — the VEVENT `LOCATION`.
    #[serde(default)]
    pub location: String,
}

/// A persisted events-rail blob could not be restored. The caller (the app's
/// trigger glue) treats this as "no drafts" and keeps the empty composer rather
/// than failing the surface — a corrupt or newer-shape blob must never break
/// composing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventDraftRestoreError {
    /// The bytes were not a canonical encoding of this record.
    Decode(String),
}

impl core::fmt::Display for EventDraftRestoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EventDraftRestoreError::Decode(m) => write!(f, "decode events drafts snapshot: {m}"),
        }
    }
}

impl std::error::Error for EventDraftRestoreError {}

impl EventDrafts {
    /// Serialise to the canonical at-rest bytes. Byte-stable for equal logical
    /// state — the property that makes `DraftsSync`'s unchanged-set dedup work
    /// and "byte-equal drafts" a meaningful cross-device assertion. This rail
    /// needs no sort to get there (single slot; see the module docs).
    pub fn snapshot_bytes(&self) -> Vec<u8> {
        // Encoding our own owned types is infallible in practice; surface a
        // clear panic rather than threading a Result through every caller
        // (the same call the other two rails' `snapshot_bytes` make).
        canonical_encode(self).expect("canonical_encode EventDrafts")
    }

    /// Restore from bytes produced by [`Self::snapshot_bytes`] (after the client
    /// glue unseals them with the owner's `BackupKey`).
    pub fn restore_from_bytes(bytes: &[u8]) -> Result<Self, EventDraftRestoreError> {
        canonical_decode(bytes).map_err(|e| EventDraftRestoreError::Decode(e.to_string()))
    }

    /// Whether this draft holds anything worth restoring. A blob that decodes to
    /// an all-empty record is indistinguishable from no draft at all, and the
    /// composer's own default is already that — so the glue skips the restore.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Build the canonical events-rail record from the composer's five inputs and
/// hand it to [`DraftsSync::save_if_changed`] — the boundary-independent half
/// of `save_drafts`, identical between `fauna-ffi`'s and `fauna-wasm`'s copies
/// before this lift (each boundary now only maps its own error type).
///
/// Passing five empty strings is how a caller clears the rail after a
/// successful create or a day-cell fresh start.
pub async fn save_event_draft<R: RpcRequester>(
    sync: &DraftsSync<R>,
    summary: String,
    dtstart: String,
    dtend: String,
    description: String,
    location: String,
) -> Result<(), DraftsClientError<R::Error>> {
    let snapshot = EventDrafts {
        summary,
        dtstart,
        dtend,
        description,
        location,
    }
    .snapshot_bytes();
    sync.save_if_changed(&snapshot).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::crypto::{BackupKey, decrypt_backup_chunk, encrypt_backup_chunk};

    fn sample() -> EventDrafts {
        EventDrafts {
            summary: "Quarterly walrus review".into(),
            dtstart: "2026-09-01T09:00".into(),
            dtend: "2026-09-01T10:30".into(),
            description: "bring the herring numbers".into(),
            location: "Room 3 / the ice floe".into(),
        }
    }

    #[test]
    fn snapshot_restore_round_trips_every_authored_field() {
        let bytes = sample().snapshot_bytes();

        let restored = EventDrafts::restore_from_bytes(&bytes).expect("restore");

        assert_eq!(restored.summary, "Quarterly walrus review");
        assert_eq!(restored.dtstart, "2026-09-01T09:00");
        assert_eq!(restored.dtend, "2026-09-01T10:30");
        assert_eq!(restored.description, "bring the herring numbers");
        assert_eq!(restored.location, "Room 3 / the ice floe");
    }

    /// The enumeration guard. This literal is deliberately **exhaustive** (no
    /// `..Default::default()`): a field added to the record fails to compile
    /// here, so at-rest data can never grow by accident — the author has to
    /// come to this test and choose. The rest of the fixtures in this module
    /// may use struct-update form; this one may not.
    #[test]
    fn the_at_rest_record_is_exactly_the_five_authored_event_form_fields() {
        let exhaustive = EventDrafts {
            summary: "Quarterly walrus review".into(),
            dtstart: "2026-09-01T09:00".into(),
            dtend: "2026-09-01T10:30".into(),
            description: "bring the herring numbers".into(),
            location: "Room 3 / the ice floe".into(),
        };
        assert_eq!(exhaustive, sample());
    }

    /// A draft holds the user's text mid-edit, including a datetime that is not
    /// yet well-formed. Normalization is a submit-time concern (`events.md`
    /// § Where logic lives, the A2 rule) — a rail that normalized on save would
    /// rewrite the user's half-typed input under the cursor.
    #[test]
    fn a_half_typed_datetime_rests_exactly_as_typed() {
        let mid_edit = EventDrafts {
            dtstart: "2026-09-0".into(),
            dtend: String::new(),
            ..sample()
        };
        let restored =
            EventDrafts::restore_from_bytes(&mid_edit.snapshot_bytes()).expect("restore");
        assert_eq!(restored.dtstart, "2026-09-0");
        assert_eq!(restored.dtend, "");
    }

    /// Byte-stability is what lets `DraftsSync` skip an unchanged upload.
    #[test]
    fn snapshot_bytes_is_stable_for_equal_logical_state() {
        assert_eq!(sample().snapshot_bytes(), sample().snapshot_bytes());

        // ...and equal after a round trip, which is the form the dedup baseline
        // actually compares (loaded bytes vs. freshly-serialised state).
        let a = sample().snapshot_bytes();
        let back = EventDrafts::restore_from_bytes(&a).expect("restore");
        assert_eq!(back.snapshot_bytes(), a);
    }

    /// The full at-rest path, mirroring both older rails' proof:
    /// serialise → seal under `BackupKey` → unseal → restore → byte-equal.
    #[test]
    fn seal_unseal_round_trips_byte_equal() {
        let key = BackupKey::derive(&[7u8; 32]);
        let plain = sample().snapshot_bytes();

        let sealed = encrypt_backup_chunk(&key, &plain).unwrap();
        // The nest stores exactly these, opaque: ciphertext carrying the
        // ChaCha20 `0x01` version byte, never the plaintext.
        assert_ne!(sealed, plain);
        assert_eq!(sealed[0], 0x01);

        let unsealed = decrypt_backup_chunk(&key, &sealed).unwrap();
        assert_eq!(unsealed, plain);
        assert_eq!(
            EventDrafts::restore_from_bytes(&unsealed)
                .expect("restore")
                .snapshot_bytes(),
            plain,
        );
    }

    #[test]
    fn restore_rejects_garbage() {
        let err = EventDrafts::restore_from_bytes(&[0xFF, 0xFF, 0xFF]).unwrap_err();
        assert!(matches!(err, EventDraftRestoreError::Decode(_)));
    }

    #[test]
    fn an_empty_composer_round_trips_and_reports_empty() {
        let bytes = EventDrafts::default().snapshot_bytes();
        let restored = EventDrafts::restore_from_bytes(&bytes).expect("restore");
        assert!(restored.is_empty());
        assert_eq!(restored.snapshot_bytes(), bytes);
    }

    #[test]
    fn a_draft_with_a_summary_is_not_empty() {
        assert!(!sample().is_empty());
    }

    /// Additive-everywhere evolution (`version-compatibility.md`): a blob a
    /// client wrote before this record grew its later fields must still restore
    /// on a newer client, with the absent fields at their defaults — never an
    /// error that would present to the user as a lost draft.
    #[test]
    fn a_blob_missing_later_fields_restores_with_defaults() {
        #[derive(Serialize)]
        struct OlderShape {
            summary: String,
            dtstart: String,
        }

        let older = canonical_encode(&OlderShape {
            summary: "just the title".into(),
            dtstart: "2026-09-01T09:00".into(),
        })
        .expect("encode older shape");

        let restored = EventDrafts::restore_from_bytes(&older).expect("an older blob still opens");
        assert_eq!(restored.summary, "just the title");
        assert_eq!(restored.dtstart, "2026-09-01T09:00");
        assert_eq!(restored.dtend, "");
        assert_eq!(restored.description, "");
        assert_eq!(restored.location, "");
    }
}
