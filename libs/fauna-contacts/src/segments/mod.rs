//! Address-book at-rest segment shapes.

pub mod envelope;
pub mod floor;
pub mod placement;

pub use envelope::{CARD_ENVELOPE_FORMAT_VERSION, CardRecordEnvelope};
pub use floor::{CARD_FLOOR_FORMAT_VERSION, CardFloorMetadata};
