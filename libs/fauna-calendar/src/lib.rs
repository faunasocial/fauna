//! `fauna-calendar` — shared at-rest shapes for CalDAV calendars + events.
//!
//! Two independent layers, mirroring `fauna-mail`:
//!
//! - **Placement journal** (`segments::placement`) — the compacted-state
//!   manifest consumed by the nest-side `CalPlacementSegmentManager`; the
//!   DR/restore index.
//! - **Content record** (`segments::{envelope, floor}`) — the per-event
//!   payload + metadata that ride one `__calendar` segment-record slot, as
//!   calendar event bodies migrate out of `bridge_caldav_events.encrypted_body`
//!   into the content segment store.

#[cfg(feature = "nest-segments")]
pub mod segments;
