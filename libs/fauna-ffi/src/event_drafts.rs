//! The **events rail** of draft-persistence v2, as a UniFFI native sees it
//! (`docs/goal/behavior/reserved-folders.md` § Drafts Sync;
//! `docs/goal/ui/events.md` § Persistence) — the android/apple/windows twin of
//! `fauna-wasm`'s `WasmEventDrafts`, which this mirrors field-for-field and
//! method-for-method (priority #3).
//!
//! The third rail's typed native face, and deliberately **not** shaped like
//! [`crate::FfiDraftsSync`] (the rail-agnostic bytes-in/bytes-out wrapper the
//! conversations and feed legs use). Those two legs each hang their
//! `DraftsSync` off a shared *manager* that already owns the compose state and
//! the canonical encoding, so a generic `load()`/`save_if_changed(bytes)` is
//! all their trigger glue needs. The Events page has no manager on any app
//! (`reserved-folders.md` § Drafts Sync's 2026-08-17 ruling: the three trigger
//! shapes stay three) and no native-side encoder either, so this face
//! **carries the typed record across the boundary** instead — the caller hands
//! over five strings and gets five strings back, never a sealed blob, never
//! the rail name, never the decision whether a save is safe (priority #2: the
//! canonical encoding, the seal, the `fauna.drafts.{get,put}` calls, the
//! launch gate and the last-saved baseline all stay in Rust, owned once by
//! `fauna_client_caldav::drafts::EventDrafts` + `fauna_client_drafts::DraftsSync`).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_caldav::drafts::EventDrafts;
use fauna_client_drafts::DraftsSync;

use crate::{FfiError, general_err};

/// The event composer's rail key within `__drafts` — one of the three frozen
/// constants on the wire (`fauna_protocol::drafts::DRAFT_RAILS`), never an
/// app's choice: a leg that minted its own rail name would round-trip only
/// with itself and silently lose every draft the user's other devices wrote
/// (`reserved-folders.md` § Drafts Sync step 1). Mirrors `fauna-wasm::
/// event_drafts::EVENTS_RAIL`.
const EVENTS_RAIL: &str = fauna_protocol::drafts::RAIL_EVENTS;

/// The at-rest events-rail draft, crossing the UniFFI boundary as five plain
/// strings — [`fauna_client_caldav::drafts::EventDrafts`] flattened. The
/// datetimes rest **raw as typed**; normalization to the wire's
/// `…THH:MM:SS` shape stays at submit (`events.md` § Where logic lives, the
/// A2 rule) — a draft is what the user wrote, not what the wire would accept.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiEventDrafts {
    pub summary: String,
    pub dtstart: String,
    pub dtend: String,
    pub description: String,
    pub location: String,
}

impl From<EventDrafts> for FfiEventDrafts {
    fn from(d: EventDrafts) -> Self {
        Self {
            summary: d.summary,
            dtstart: d.dtstart,
            dtend: d.dtend,
            description: d.description,
            location: d.location,
        }
    }
}

impl From<FfiEventDrafts> for EventDrafts {
    fn from(d: FfiEventDrafts) -> Self {
        Self {
            summary: d.summary,
            dtstart: d.dtstart,
            dtend: d.dtend,
            description: d.description,
            location: d.location,
        }
    }
}

/// UniFFI handle for one actor's events-rail draft autosync — the events twin
/// of [`crate::FfiDraftsSync`], pre-bound to `EVENTS_RAIL` and typed rather
/// than raw bytes. Wraps the shared `DraftsSync` (launch gate + last-saved
/// baseline), so it must be built once at login and held for the session: a
/// fresh instance per call would re-close the gate and lose the dedup
/// baseline. Construct via
/// [`FfiNestClient::event_drafts`](crate::nest_client::FfiNestClient::event_drafts).
#[derive(uniffi::Object)]
pub struct FfiEventDraftsSync {
    sync: DraftsSync<Arc<NestClient>>,
}

impl FfiEventDraftsSync {
    /// Build over the live connection's transport + identity — the events
    /// rail of [`crate::drafts::drafts_sync_from_nest`], which
    /// [`crate::FfiDraftsSync::from_nest`] shares.
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self {
            sync: crate::drafts::drafts_sync_from_nest(nest, EVENTS_RAIL.to_string()),
        })
    }
}

#[fauna_uniffi_async::export]
impl FfiEventDraftsSync {
    /// Restore the owner's persisted event-composer draft on launch: fetch +
    /// unseal the `__drafts` blob at `path = "events"` (`fauna.drafts.get`)
    /// and decode it into the five `event-form` inputs.
    ///
    /// `None` for a first-run empty rail, and for a blob that decodes to an
    /// all-empty record — indistinguishable from no draft, and the form's
    /// default already is it. A blob that will not decode is `None` too
    /// rather than an error: a corrupt or newer-shape record must never break
    /// composing (the shared record's own contract), and the load itself
    /// succeeded, so the save gate is correctly lifted. Mirrors
    /// `WasmEventDrafts::restoreDrafts`'s "undefined" cases exactly.
    ///
    /// A transport/seal *failure* errors WITHOUT lifting the `DraftsSync` save
    /// gate, so a later `save_drafts` stays a no-op for the session and can
    /// never clobber the user's unread draft; the next launch retries.
    pub async fn restore_drafts(&self) -> Result<Option<FfiEventDrafts>, FfiError> {
        let bytes = match self.sync.load().await.map_err(general_err)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        match EventDrafts::restore_from_bytes(&bytes) {
            Ok(draft) if draft.is_empty() => Ok(None),
            Ok(draft) => Ok(Some(draft.into())),
            Err(_) => Ok(None),
        }
    }

    /// Persist the owner's current event-composer draft after a compose
    /// change (the caller debounces on the shared
    /// [`crate::autosave_debounce_ms`] window): build the canonical
    /// record from the five inputs and hand it to `DraftsSync::save_if_changed`,
    /// which seals under the owner's `BackupKey` and overwrites the
    /// `__drafts` blob (`fauna.drafts.put`) **iff** a launch restore has
    /// succeeded *and* the record differs from the last-saved baseline.
    ///
    /// Passing five empty strings is how the caller clears the rail after a
    /// successful create or a day-cell fresh start.
    pub async fn save_drafts(
        &self,
        summary: String,
        dtstart: String,
        dtend: String,
        description: String,
        location: String,
    ) -> Result<(), FfiError> {
        fauna_client_caldav::drafts::save_event_draft(
            &self.sync,
            summary,
            dtstart,
            dtend,
            description,
            location,
        )
        .await
        .map_err(general_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rail name is the wire's closed enumeration, not this crate's
    /// choice — the property that makes an android-written draft restore in
    /// the user's tui.
    #[test]
    fn the_rail_is_the_ratified_events_constant() {
        assert_eq!(EVENTS_RAIL, "events");
        assert!(
            fauna_protocol::drafts::is_ratified_rail(EVENTS_RAIL),
            "the rail must be one of the nest-validated DRAFT_RAILS",
        );
    }

    /// The `From` conversions round-trip every field — a typo here would
    /// silently swap or drop a field crossing the FFI boundary.
    #[test]
    fn ffi_event_drafts_round_trips_through_the_shared_record() {
        let ffi = FfiEventDrafts {
            summary: "Quarterly walrus review".into(),
            dtstart: "2026-09-01T09:00".into(),
            dtend: "2026-09-01T10:30".into(),
            description: "bring the herring numbers".into(),
            location: "Room 3 / the ice floe".into(),
        };
        let core: EventDrafts = ffi.clone().into();
        let back: FfiEventDrafts = core.into();
        assert_eq!(ffi, back);
    }
}
