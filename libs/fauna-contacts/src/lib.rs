//! `fauna-contacts` — shared at-rest shapes for CardDAV address books + cards.
//!
//! The structural twin of `fauna-calendar` for the CardDAV bridge store
//! (`bridge_carddav_*` mirrors `bridge_caldav_*` — see
//! `docs/goal/behavior/carddav-server.md` § Storage model).
//!
//! Two independent layers, mirroring `fauna-calendar`:
//!
//! - **Placement journal** (`segments::placement`) — the compacted-state
//!   manifest consumed by the nest-side `CardPlacementSegmentManager`.
//! - **Content record** (`segments::{envelope, floor}`) — the per-card payload
//!   and metadata that ride one `__card` segment-record slot, as vCard bodies
//!   migrate out of `bridge_carddav_cards.encrypted_body` into the content
//!   segment store.

#[cfg(feature = "nest-segments")]
pub mod segments;
