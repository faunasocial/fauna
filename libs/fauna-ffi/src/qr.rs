//! UniFFI face of [`fauna_core::qr_matrix`] — the shared QR **module matrix** every
//! native app draws with its own toolkit (Compose `Canvas` / XAML / SwiftUI `Path`)
//! rather than linking a platform QR library. One encoder, six apps (priorities #1/#2).
//!
//! Deliberately **generic over the payload**, not identity-specific: the identity export
//! composes [`identity_qr_encode`](crate::identity_qr_encode) → [`qr_matrix`]; any future
//! payload codec composes onto this same face (the peer-invite candidate is gone — the
//! pairing surface is retired, `p2p.md` § No pairing step, ever).

use crate::FfiError;
use fauna_core::qr_matrix::{QUIET_ZONE_MODULES, QrMatrix};

/// Encode `data` as a QR module matrix at error-correction level M.
///
/// The returned [`QrMatrix`] carries **no quiet zone** — a renderer pads itself by
/// [`qr_quiet_zone_modules`] on all four sides, or scanners refuse the code.
///
/// Errors when `data` exceeds the largest QR version's capacity at level M.
#[uniffi::export]
pub fn qr_matrix(data: String) -> Result<QrMatrix, FfiError> {
    fauna_core::qr_matrix::qr_matrix(&data).map_err(|msg| FfiError::General { msg })
}

/// The quiet-zone margin, in modules, a renderer must leave around [`qr_matrix`]'s grid.
///
/// A const in Rust; exported as a function because UniFFI has no constant surface — this
/// is what stops each app hard-coding its own `4`.
#[uniffi::export]
pub fn qr_quiet_zone_modules() -> u32 {
    QUIET_ZONE_MODULES
}
