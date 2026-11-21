//! The terminal's indeterminate activity indicator: a cycling braille glyph.
//!
//! `onboarding-provisioning.md` § 6 *A running step shows activity, never an
//! attempt count* asks every app to paint a `Running` step's
//! `provisioning-step-checkbox` with the platform's indeterminate activity
//! indicator — "in the terminal an animated glyph". The GUI apps use their
//! toolkit's native spinner, so the frame list is tui's alone and lives here
//! rather than in shared Rust.
//!
//! The animation is paint-only: the page sets [`crate::element::Element::spinning`]
//! with the frame for *now*, the renderer paints [`glyph`] of it, and the
//! automation registry keeps the element's `text` — the shared
//! `progress::status_glyph` — so no e2e read depends on a frame. What makes a
//! frame advance while nothing else changes is the page-scoped redraw tick in
//! the main loop, firing every [`FRAME_MS`] while a spinner is on screen
//! (`spinner_showing` in the crate root).

/// One frame's time on screen. Eight frames a second reads as motion without
/// redrawing the terminal more than it needs.
pub const FRAME_MS: u64 = 125;

/// The frames, in order: one braille dot circling.
pub const FRAMES: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// The frame a paint at wall-clock `now_ms` shows. Derived from the clock, not
/// a counter, so every redraw — the tick's or any other — lands on the frame
/// for its moment, and two pages painting at once agree.
pub fn frame_at(now_ms: u64) -> usize {
    ((now_ms / FRAME_MS) % FRAMES.len() as u64) as usize
}

/// The glyph frame `frame` paints (wrapping, so any index is safe).
pub fn glyph(frame: usize) -> &'static str {
    FRAMES[frame % FRAMES.len()]
}

/// The `provisioning-step-checkbox` element for one step row, painted at
/// `now_ms`: its text is always the canonical `status_glyph` (the automation
/// read), and a `Running` step additionally paints the activity glyph. Shared
/// by the two pages that reuse the `provisioning-step-row` component —
/// provisioning and nest retire — so both animate the same way.
pub fn step_checkbox(
    status: fauna_provisioning::progress::StepStatus,
    now_ms: u64,
) -> crate::element::Element {
    use fauna_provisioning::progress::{StepStatus, status_glyph};
    let el = crate::element::Element::label(
        fauna_ui_ids::PROVISIONING_STEP_CHECKBOX,
        status_glyph(status),
    );
    if status == StepStatus::Running {
        el.spinning(frame_at(now_ms))
    } else {
        el
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_provisioning::progress::{StepStatus, status_glyph};

    #[test]
    fn frames_advance_one_tick_apart_and_cycle() {
        assert_ne!(frame_at(0), frame_at(FRAME_MS));
        assert_eq!(frame_at(0), frame_at(FRAME_MS * FRAMES.len() as u64));
        let distinct: std::collections::BTreeSet<_> = FRAMES.iter().collect();
        assert_eq!(distinct.len(), FRAMES.len(), "every frame differs");
    }

    #[test]
    fn checkbox_text_stays_the_shared_glyph_whatever_the_frame() {
        for status in [
            StepStatus::Pending,
            StepStatus::Running,
            StepStatus::Skipped,
            StepStatus::Succeeded,
            StepStatus::Failed,
        ] {
            for t in [0, FRAME_MS, 3 * FRAME_MS] {
                assert_eq!(step_checkbox(status, t).text, status_glyph(status));
            }
        }
    }
}
