//! QR **module matrix** generation — the one shared primitive every app renders itself.
//!
//! Clients draw the boolean grid with their own toolkit (canvas / GTK `DrawingArea` /
//! Compose `Canvas` / XAML / SwiftUI `Path`) rather than each pulling a platform QR
//! library, which is what keeps the six apps on one code path (priorities #1/#2).
//!
//! Payload *codecs* live next door and compose onto this: [`crate::identity_qr`] writes the
//! `(identity, handle)` URI. This module knows nothing about any payload — it turns an
//! arbitrary payload string into modules.

use qrcode::{EcLevel, QrCode};

/// A QR code as a square grid of modules, row-major, **without a quiet zone**.
///
/// `modules[y * size + x]` is `true` when the module is dark. The 4-module quiet zone the
/// spec requires is deliberately *not* baked in: a renderer knows its own background and
/// scales the payload area exactly, so it pads itself. A renderer that forgets the quiet
/// zone produces a code many scanners refuse — see [`QUIET_ZONE_MODULES`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct QrMatrix {
    /// Modules per side (21 for version 1, growing by 4 per version).
    pub size: u32,
    /// Row-major dark/light flags, `size * size` entries.
    pub modules: Vec<bool>,
}

/// The quiet-zone margin, in modules, every renderer must leave around [`QrMatrix`].
///
/// ISO/IEC 18004 requires 4 light modules on all four sides. It is not part of the matrix
/// (see [`QrMatrix`]), so each app adds it when drawing.
pub const QUIET_ZONE_MODULES: u32 = 4;

impl QrMatrix {
    /// Dark/light flag at `(x, y)`, or `None` when out of bounds.
    pub fn module_at(&self, x: u32, y: u32) -> Option<bool> {
        if x >= self.size || y >= self.size {
            return None;
        }
        self.modules.get((y * self.size + x) as usize).copied()
    }
}

/// Encode `data` as a QR code at error-correction level **M** (~15% recovery) — the level
/// Apple's `IdentityExportSection` already renders at, kept identical so a code scanned off
/// one app's screen behaves the same as off another's.
///
/// Returns `Err` when `data` exceeds what the largest QR version holds at level M.
pub fn qr_matrix(data: &str) -> Result<QrMatrix, String> {
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M)
        .map_err(|e| format!("QR encoding failed: {e}"))?;
    let size = code.width() as u32;
    // `to_colors()` is row-major, matching `modules[y * size + x]`. The orientation is
    // pinned by the dark-module test, not by this comment — a mirrored grid still shows
    // three finder patterns and still decodes on lenient scanners.
    let modules = code
        .to_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    Ok(QrMatrix { size, modules })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity_qr::IdentityQr;

    const SECRET: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";

    /// A finder pattern is the 7x7 concentric square in three corners: a dark 7x7 ring, a
    /// light 5x5 ring inside it, a dark 3x3 core. Every conformant QR has exactly these,
    /// so asserting them proves we emitted a real QR grid and not a transposed/garbled one.
    fn assert_finder_pattern_at(m: &QrMatrix, ox: u32, oy: u32) {
        for dy in 0..7 {
            for dx in 0..7 {
                let ring = dx == 0 || dx == 6 || dy == 0 || dy == 6;
                let core = (2..=4).contains(&dx) && (2..=4).contains(&dy);
                let expected = ring || core;
                assert_eq!(
                    m.module_at(ox + dx, oy + dy),
                    Some(expected),
                    "finder pattern mismatch at ({}, {}) of the block at ({ox}, {oy})",
                    dx,
                    dy
                );
            }
        }
    }

    #[test]
    fn matrix_is_square_and_versioned() {
        let m = qr_matrix("hello").unwrap();
        // Version N has side 21 + 4*(N-1); versions run 1..=40.
        assert!(m.size >= 21 && m.size <= 177, "unexpected side {}", m.size);
        assert_eq!((m.size - 21) % 4, 0, "side {} is not a QR version", m.size);
        assert_eq!(m.modules.len(), (m.size * m.size) as usize);
    }

    #[test]
    fn matrix_has_the_three_finder_patterns() {
        let m = qr_matrix("hello").unwrap();
        assert_finder_pattern_at(&m, 0, 0); // top-left
        assert_finder_pattern_at(&m, m.size - 7, 0); // top-right
        assert_finder_pattern_at(&m, 0, m.size - 7); // bottom-left
        // The fourth corner is NOT a finder pattern — that asymmetry is what lets a
        // scanner recover rotation. Guard against emitting a symmetric (wrong) grid.
        let bottom_right_is_finder = (0..7).all(|dy| {
            (0..7).all(|dx| {
                let ring = dx == 0 || dx == 6 || dy == 0 || dy == 6;
                let core = (2..=4).contains(&dx) && (2..=4).contains(&dy);
                m.module_at(m.size - 7 + dx, m.size - 7 + dy) == Some(ring || core)
            })
        });
        assert!(
            !bottom_right_is_finder,
            "bottom-right corner must not be a finder pattern"
        );
    }

    #[test]
    fn module_at_maps_xy_into_the_flat_vec_and_is_bounds_checked() {
        let m = qr_matrix("hello").unwrap();
        // Pins the accessor's convention against `modules` — `(x, y)` → `y * size + x`.
        // This says nothing about whether the *grid* is mirrored (both halves would flip
        // together); `dark_module_pins_the_orientation` is what covers that.
        assert_eq!(m.module_at(3, 0), Some(m.modules[3]));
        assert_eq!(m.module_at(0, 1), Some(m.modules[m.size as usize]));
        assert_eq!(m.module_at(m.size, 0), None);
        assert_eq!(m.module_at(0, m.size), None);
    }

    #[test]
    fn encoding_is_deterministic_and_payload_sensitive() {
        assert_eq!(qr_matrix("hello").unwrap(), qr_matrix("hello").unwrap());
        assert_ne!(qr_matrix("hello").unwrap(), qr_matrix("hell0").unwrap());
    }

    #[test]
    fn encodes_the_longest_realistic_identity_payload() {
        // Worst case the export surface can hand us: 64-hex secret + a long handle.
        let handle = format!("{}@{}.example.com", "a".repeat(64), "b".repeat(63));
        let uri = IdentityQr::to_uri(SECRET, Some(&handle));
        let m = qr_matrix(&uri).expect("realistic identity payload must fit a QR code");
        assert!(m.size >= 21);
    }

    #[test]
    fn identity_uri_round_trips_from_the_encoded_payload() {
        // The matrix is generated from exactly the URI the import parser accepts — the
        // export→scan→import loop is closed end-to-end (onboarding.md §1.identity_import).
        let uri = IdentityQr::to_uri(SECRET, Some("alice@fauna.social"));
        assert!(qr_matrix(&uri).is_ok());
        assert_eq!(IdentityQr::from_uri(&uri).unwrap(), SECRET);
    }

    /// Render the matrix the way a client must — quiet zone included, scaled up — and hand
    /// it to a real QR decoder. Greyscale: dark module → 0, light → 255.
    fn decode_roundtrip(data: &str) -> String {
        const SCALE: usize = 4;
        let m = qr_matrix(data).unwrap();
        let side = (m.size + 2 * QUIET_ZONE_MODULES) as usize * SCALE;
        let mut img = rqrr::PreparedImage::prepare_from_greyscale(side, side, |px, py| {
            let mx = px / SCALE;
            let my = py / SCALE;
            let q = QUIET_ZONE_MODULES as usize;
            let (Some(x), Some(y)) = (mx.checked_sub(q), my.checked_sub(q)) else {
                return 255; // quiet zone
            };
            match m.module_at(x as u32, y as u32) {
                Some(true) => 0,
                _ => 255,
            }
        });
        let grids = img.detect_grids();
        assert_eq!(
            grids.len(),
            1,
            "decoder found {} grids, want 1",
            grids.len()
        );
        let (_meta, content) = grids[0].decode().expect("grid must decode");
        content
    }

    #[test]
    fn dark_module_pins_the_orientation() {
        // ISO/IEC 18004 mandates one always-dark module at (x=8, y=4*version+9), i.e.
        // (8, size-8). It is the code's only asymmetric fixed module, so it — not the
        // finder patterns, which are transpose-symmetric across the three corners — is
        // what proves we emit row-major and not a mirrored grid. Several payloads, since
        // each selects a different mask.
        for data in [
            "hello",
            "a",
            "fauna",
            "0123456789",
            "the quick brown fox jumps over the lazy dog",
            &IdentityQr::to_uri(SECRET, None),
            &IdentityQr::to_uri(SECRET, Some("alice@fauna.social")),
        ] {
            let m = qr_matrix(data).unwrap();
            assert_eq!(
                m.module_at(8, m.size - 8),
                Some(true),
                "dark module missing at (8, {}) for payload {data:?} — matrix is mirrored",
                m.size - 8
            );
        }
    }

    #[test]
    fn matrix_decodes_back_to_its_payload() {
        // The load-bearing test: a scanner reading what we emit must recover the exact
        // payload. Orientation, masking and EC level are all pinned by this one assertion.
        assert_eq!(decode_roundtrip("hello"), "hello");
    }

    #[test]
    fn identity_qr_decodes_back_to_the_importable_uri() {
        // Closes export→scan→import: the decoded string is exactly what the import parser
        // accepts (onboarding.md §1.identity_import).
        let uri = IdentityQr::to_uri(SECRET, Some("alice@fauna.social"));
        let scanned = decode_roundtrip(&uri);
        assert_eq!(scanned, uri);
        assert_eq!(
            crate::identity_qr::parse_import_input(&scanned),
            Some(crate::identity_qr::ImportedIdentity {
                secret: SECRET.into(),
                handle: Some("alice@fauna.social".into()),
            })
        );
    }

    #[test]
    fn oversized_payload_errors_rather_than_panicking() {
        // Past version 40's capacity at level M. Must be a clean Err — a client that hands
        // us a huge string gets an error string, never a panic across the FFI boundary.
        let huge = "x".repeat(10_000);
        let err = qr_matrix(&huge).unwrap_err();
        assert!(
            err.contains("QR encoding failed"),
            "unexpected error: {err}"
        );
    }
}
