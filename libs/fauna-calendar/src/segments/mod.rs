//! Calendar at-rest segment shapes.

pub mod envelope;
pub mod floor;
pub mod placement;

pub use envelope::{CAL_ENVELOPE_FORMAT_VERSION, CalRecordEnvelope};
pub use floor::{CAL_FLOOR_FORMAT_VERSION, CalFloorMetadata};
