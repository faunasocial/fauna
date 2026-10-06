//! KeyPackage replenish logic.
//!
//! Pure function: check count, generate if below threshold.
//! Transport (HTTP/WASM) is provided by the consumer.

use openmls::prelude::KeyPackage;

use crate::engine::MlsEngine;
use crate::error::Result;

/// Default threshold below which we generate more key packages.
pub const DEFAULT_THRESHOLD: u64 = 5;

/// Default number of key packages to generate per batch.
pub const DEFAULT_BATCH_SIZE: usize = 10;

/// Check if replenishment is needed and generate key packages if so.
/// Returns the generated key packages (empty vec if no replenishment needed).
pub fn replenish_if_needed(
    engine: &MlsEngine,
    current_count: u64,
    threshold: u64,
    batch_size: usize,
) -> Result<Vec<KeyPackage>> {
    if current_count >= threshold {
        return Ok(Vec::new());
    }
    engine.generate_key_packages(batch_size)
}
