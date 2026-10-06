//! The ward's screen-time lock — tui's rendering of the cross-page "screen time
//! is off right now" surface (`docs/goal/behavior/family-safety.md` § Screen
//! time, Slice E; ui.yaml `global:` elements `screen-time-lock` /
//! `screen-time-lock-message`, user-approved 2026-07-15). The tui twin of
//! `apps/fauna-linux/src/screen_lock.rs` and web's `screenTime.svelte.ts`.
//!
//! **Client-enforced by construction.** The nest cannot see when a child's
//! device is in use and deliberately does not gate on it, so this surface *is*
//! the enforcement — a conforming-client render rule. § Don't do these is
//! explicit: *"Don't put screen-time or content enforcement on the nest."*
//!
//! **Every decision is shared Rust — including the orchestration, not just the
//! policy.** [`ScreenLock`] is [`fauna_core::screen_time::ScreenLockCore`]: the
//! clock, the window/budget wiring, the test-clock skew and the heartbeat glue
//! all used to be hand-rolled once per app (tui, linux) around the same
//! [`fauna_core::screen_time::screen_lock_message`]/
//! [`fauna_core::screen_time::UsageHeartbeat`] calls, byte-identically save for
//! how each app stores the instance — a seventh tui↔linux twin, and the first
//! whose duplication was a full orchestration type rather than a formatter or a
//! dispatch-and-log fn.
//!
//! **State lives on [`crate::app::App`], not in a `thread_local`.** linux hangs
//! its copy off thread-locals because GTK hands its callbacks no context; tui's
//! loop threads one `&mut App` through every handler, so the ordinary field is
//! both the idiomatic shape here and the testable one (drives a plain struct,
//! no process-global to reset between cases). *This* is the one genuinely
//! platform-divergent piece — where the instance lives — which is exactly why
//! the lift stopped at the storage boundary rather than trying to unify that
//! too.
//!
//! **The Family page stays reachable.** The goal-doc invariant is that a locked
//! ward can always see who supervises them and what the policy is, so the lock
//! replaces the *page pane* only: the sidebar keeps `supervised-indicator` and
//! the `family-tab` row live, and [`ScreenLock::lock_message`]'s caller exempts
//! the family page outright. A lock that could not be read from is a lock the
//! child cannot understand or appeal.

use fauna_ui_ids as ids;

/// How often the lock re-evaluates its own verdict against the local clock.
///
/// The policy only changes on a `fauna.family.status` read, but *time* passes
/// continuously, so the surface must re-ask on a tick or a ward already in the
/// app would sail past their bedtime. One minute is the resolution of the policy
/// itself (both bounds and the budget are whole minutes), so a finer tick could
/// not change an answer.
///
/// The same tick drives the usage heartbeat, which is why it must stay at or
/// under [`fauna_core::screen_time::MAX_ACCRUAL_STEP_SECS`]: the engine credits
/// at most one step per call, so a slower tick would silently under-count.
/// linux's `LOCK_TICK_SECS` and web's `lockTick` are the same 60 seconds.
pub const LOCK_TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// The ward's own screen-time state — the shared orchestration type both tui
/// (a plain `App` field) and linux (a `thread_local!` cell) hold an instance
/// of. `lock_message` takes tui's own i18n lookup explicitly, matching
/// `fauna_client_alerts::CriticalAlerts::active_lines`'s shape; every other
/// method's signature is unchanged from before the lift, so callers outside
/// this module needed no edits beyond `lock_message`'s new argument.
pub type ScreenLock = fauna_core::screen_time::ScreenLockCore;

/// [`ScreenLock::lock_message`], resolved through tui's own i18n table — the
/// one call site every other module in this app should reach for, so the
/// lookup fn is named exactly once.
pub fn lock_message(lock: &ScreenLock) -> Option<String> {
    lock.lock_message(fauna_i18n::strings::lookup)
}

/// The lock surface's elements — what [`crate::app::App::page_elements`]
/// returns *instead of* the page's own while the lock holds, so the pane paints
/// it, the automation registry registers it, and the focus ring sees only it.
///
/// `screen-time-lock` is the landmark (an e2e reads it with `is_visible`, so it
/// carries the title rather than nothing — the terminal has no empty container
/// to point at), and `screen-time-lock-message` is the explanation line, which
/// names when use resumes or the budget that ran out, plus the guardian, and
/// nothing about what the ward was doing.
///
/// The hint below them is untagged chrome: ui.yaml scopes no id to it, and it
/// is what tells a locked child the Family page is still open to them.
pub fn lock_elements(message: &str) -> Vec<crate::element::Element> {
    use crate::element::Element;
    vec![
        Element::label(
            ids::SCREEN_TIME_LOCK,
            fauna_i18n::strings::family::SCREEN_LOCK_TITLE,
        ),
        Element::label(ids::SCREEN_TIME_LOCK_MESSAGE, message.to_string()),
        Element::chrome(fauna_i18n::strings::family::SCREEN_LOCK_FAMILY_HINT),
    ]
}
