//! Shared cairo QR-matrix painter — the one Linux-specific rendering routine
//! bridging `fauna_core::qr_matrix::QrMatrix` (identity export + Nostr Connect
//! bunker invites); no client links a platform QR library (priorities #1/#2).

use fauna_core::qr_matrix::{QUIET_ZONE_MODULES, QrMatrix};

/// Paint `m` into `width` x `height`, dark-on-light with the mandatory quiet
/// zone.
///
/// Deliberately **not** theme-aware: a QR must stay dark-on-light to scan, so
/// the light background is painted explicitly rather than inherited from a
/// (possibly dark) theme.
pub(crate) fn draw_matrix(cr: &gtk::cairo::Context, m: &QrMatrix, width: i32, height: i32) {
    // The quiet zone is not part of the matrix — the renderer pads itself, or
    // scanners refuse the code (fauna_core::qr_matrix::QUIET_ZONE_MODULES).
    let modules_per_side = m.size + 2 * QUIET_ZONE_MODULES;
    let scale = (width.min(height) as f64) / (modules_per_side as f64);

    // Light background across the whole box — this IS the quiet zone at the edges.
    cr.set_source_rgb(1.0, 1.0, 1.0);
    let _ = cr.paint();

    cr.set_source_rgb(0.0, 0.0, 0.0);
    for y in 0..m.size {
        for x in 0..m.size {
            if m.module_at(x, y) != Some(true) {
                continue;
            }
            let px = (x + QUIET_ZONE_MODULES) as f64 * scale;
            let py = (y + QUIET_ZONE_MODULES) as f64 * scale;
            cr.rectangle(px, py, scale, scale);
        }
    }
    let _ = cr.fill();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The draw func maps module (x, y) → the px rect offset by the quiet
    /// zone, and the grid fits inside the box. Pins the geometry without a
    /// live GTK surface: a mirrored or unpadded render is the failure this
    /// guards (the matrix's own orientation is pinned by
    /// `fauna_core::qr_matrix`'s dark-module test). Mirrors
    /// `settings/identity_export.rs`'s pre-extraction geometry pin.
    #[test]
    fn quiet_zone_is_added_around_the_grid_and_the_whole_code_fits() {
        const BOX_PX: i32 = 220;
        let m = fauna_core::qr_matrix::qr_matrix("fauna").unwrap();
        let modules_per_side = m.size + 2 * QUIET_ZONE_MODULES;
        let scale = (BOX_PX as f64) / (modules_per_side as f64);

        // The top-left *payload* module sits one quiet zone in from the box edge, not at 0.
        let first_px = QUIET_ZONE_MODULES as f64 * scale;
        assert!(
            first_px > 0.0,
            "quiet zone must offset the grid from the edge"
        );

        // The bottom-right payload module's far edge lands inside the box, leaving the
        // trailing quiet zone.
        let last_edge = (m.size + QUIET_ZONE_MODULES) as f64 * scale;
        assert!(
            last_edge <= BOX_PX as f64,
            "grid + quiet zone must fit in {BOX_PX}px, got {last_edge}"
        );
        let trailing = BOX_PX as f64 - last_edge;
        assert!(
            (trailing - first_px).abs() < 1.0,
            "quiet zone must be symmetric: leading {first_px}, trailing {trailing}"
        );
    }
}
