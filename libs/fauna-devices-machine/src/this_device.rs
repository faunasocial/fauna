//! Which roster row the **This-device** badge marks
//! (`docs/goal/behavior/devices.md` § This-device marker).
//!
//! The badge is a pure client-side string comparison — the nest's roster is
//! identical whoever asks — so the whole question is *which value* an app
//! compares. The answer is **the row it enrolled on**, and that is not always
//! the `device_id` the app holds in its own local store.

/// The `sync_devices` row this app's `device-this-mark-badge` marks.
///
/// `enrolled` is the row this machine's grant is actually registered on —
/// `AccountStoreHandle::enrolled_device_row`, read out of the principal
/// slot's registration latch. `own` is the app's own locally-generated
/// `device_id` (tui's `device.db`, linux's actor-scoped twin, web's
/// `localStorage`, android's `EncryptedSharedPreferences`).
///
/// **`enrolled` wins whenever there is one, and the fallback is load-bearing
/// rather than a formality.** Both halves are exactly right in their own case:
///
/// * **An enrolled row is known** → mark it. Where a **co-located sync agent**
///   provisioned by *another app on the same box* advertises its own id, the
///   enrollment converges onto that agent's row
///   (`sync-agent.md` § Credential model → the RULED 2026-08-15
///   block, decision 2) — deliberately, so two apps on one box hold one row
///   instead of two. The app's own id then names no row at all, and marking it
///   marks nothing. This is the case the badge got wrong until 2026-08-19.
/// * **No enrolled row is known** → the app's own id. The latch answers `None`
///   before the enrollment ceremony's nest legs have ever succeeded, and again
///   after a re-mint whose fresh grant encoding no longer matches the stored
///   latch. It is also per-actor, so it is `None` for an actor this machine has
///   not enrolled. In every one of those the app *does* own its own row —
///   decision 2's case 1 (no agent) and case 3 (an agent this gate refuses)
///   both enroll under the app's own id, and decision 5 accepts that
///   multiplicity outright: "a second co-located app that independently engages
///   sync keeps its own named row". So the fallback is not a degraded guess; it
///   is the correct answer for the two cases that were never broken.
///
/// `None` out means neither is known — the badge simply never matches, which
/// is the safe default a fresh install lands in.
///
/// A cheap held-value read, as the § This-device marker documents — never an
/// IPC round trip to the co-located agent.
pub fn this_device_row(enrolled: Option<&str>, own: Option<&str>) -> Option<String> {
    enrolled
        .filter(|id| !id.is_empty())
        .or(own.filter(|id| !id.is_empty()))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect this rule exists to close: an agent provisioned by a
    /// *different* app advertises its own id, the enrollment converges there,
    /// and the badge must follow — marking the app's own id marks no row.
    #[test]
    fn the_enrolled_row_wins_over_the_apps_own_id() {
        assert_eq!(
            this_device_row(Some("agentrow"), Some("ownid")),
            Some("agentrow".to_string())
        );
    }

    /// Decision 2's cases 1 and 3, and every pass before the ceremony's nest
    /// legs have succeeded: the app genuinely owns its own row.
    #[test]
    fn the_apps_own_id_stands_in_when_no_row_is_enrolled() {
        assert_eq!(
            this_device_row(None, Some("ownid")),
            Some("ownid".to_string())
        );
    }

    /// A fresh install has neither — no badge rather than a guessed one.
    #[test]
    fn neither_known_marks_nothing() {
        assert_eq!(this_device_row(None, None), None);
    }

    /// An empty string is a missing value, never a row to compare against —
    /// the roster's own ids are never empty, so an empty `enrolled` that fell
    /// through would blank the badge instead of falling back.
    #[test]
    fn an_empty_id_reads_as_absent_on_either_side() {
        assert_eq!(
            this_device_row(Some(""), Some("ownid")),
            Some("ownid".to_string())
        );
        assert_eq!(
            this_device_row(Some("agentrow"), Some("")),
            Some("agentrow".to_string())
        );
        assert_eq!(this_device_row(Some(""), Some("")), None);
    }
}
