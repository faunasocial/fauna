//! `Passage<O>` — the shared shape behind every "re-key/re-seal/re-grant"
//! recovery-kit leg's progress surface (`succession-aftermath.md` § Re-key
//! scope: each leg is *"started at first successor sign-in, **surfaced with
//! progress**, resumed until complete"*).
//!
//! `ConfigResealProgress`, `BackupRegrantProgress`, `ReplicaResealProgress`,
//! `DraftsResealProgress`, `GrantRemintProgress`, and `MailBurnProgress` each
//! independently defined the identical three-arm `Running`/`Settled(O)`/
//! `Failed(String)` enum, and the identical `still_owed()` fallback
//! (`Running | Failed(_) => true`, `Settled` delegates to the outcome) —
//! found as six near-duplicate pairs by the dev-fleet near-duplicate-function
//! scanner.
//! Only the `Settled` arm's rendering and still-owed predicate are genuinely
//! per-leg (a different outcome enum, different i18n keys); everything else
//! is now here, once.

use crate::localized::LocalizedText;

/// What a [`Passage`]'s `Settled` outcome supplies: how to render it, and
/// whether the pass is still owed by someone given this settled state.
pub trait ProgressOutcome {
    /// The i18n key for the `Running` line.
    const RUNNING_KEY: &'static str;
    /// The i18n key for the `Failed(reason)` line — rendered with a `"reason"`
    /// arg, the one substitution every leg's failure line carries.
    const FAILED_KEY: &'static str;

    /// What to show once settled, or `None` when nothing is owed and nothing
    /// happened — every leg reports two or more of its outcome's arms this
    /// way so a no-op pass doesn't train the user to ignore the line that
    /// matters.
    fn settled_line(&self) -> Option<LocalizedText>;

    /// Whether the pass is still owed by *someone*, given this settled
    /// outcome — the leg's own resume condition, folded per-arm.
    fn still_owed(&self) -> bool;
}

/// The pass as a progress surface: in flight, settled with a domain outcome,
/// or unable to reach the nest at all. `O` supplies everything domain-
/// specific via [`ProgressOutcome`]; `Running`/`Failed` render and
/// still-owed identically for every leg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Passage<O> {
    /// The pass is in flight.
    Running,
    /// The pass returned; the outcome decides whether anything renders.
    Settled(O),
    /// The pass could not reach the nest (or the write failed). Carries the
    /// display string the caller already has, since transport errors are not
    /// enumerable enough to key i18n off.
    Failed(String),
}

impl<O: ProgressOutcome> Passage<O> {
    /// What to show, or `None` when nothing is owed and nothing happened.
    ///
    /// Pure, so every app renders identical copy and every platform can test
    /// it without a nest.
    pub fn status_line(&self) -> Option<LocalizedText> {
        match self {
            Self::Running => Some(LocalizedText::key(O::RUNNING_KEY)),
            Self::Settled(outcome) => outcome.settled_line(),
            Self::Failed(reason) => {
                let mut text = LocalizedText::key(O::FAILED_KEY);
                text.args.insert("reason".into(), reason.clone());
                Some(text)
            }
        }
    }

    /// Whether the pass is still owed by *someone* — this device on a retry,
    /// or another device that holds the retired key/seed.
    ///
    /// The caller's resume condition: § Re-key scope's "resumed until
    /// complete" is exactly "keep calling at every sign-in while this is
    /// true". `Failed` counts as owed — a pass that never reached the nest
    /// settled nothing.
    pub fn still_owed(&self) -> bool {
        match self {
            Self::Running | Self::Failed(_) => true,
            Self::Settled(outcome) => outcome.still_owed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum TestOutcome {
        Done,
        StillOwed,
        Nothing,
    }

    impl ProgressOutcome for TestOutcome {
        const RUNNING_KEY: &'static str = "test.running";
        const FAILED_KEY: &'static str = "test.failed";

        fn settled_line(&self) -> Option<LocalizedText> {
            match self {
                Self::Done => Some(LocalizedText::key("test.done")),
                Self::StillOwed => Some(LocalizedText::key("test.owed_elsewhere")),
                Self::Nothing => None,
            }
        }

        fn still_owed(&self) -> bool {
            matches!(self, Self::StillOwed)
        }
    }

    #[test]
    fn running_renders_the_running_key_and_is_owed() {
        let p = Passage::<TestOutcome>::Running;
        assert_eq!(p.status_line().unwrap().key, "test.running");
        assert!(p.still_owed());
    }

    #[test]
    fn failed_renders_the_failed_key_with_a_reason_arg_and_is_owed() {
        let p = Passage::<TestOutcome>::Failed("boom".to_string());
        let line = p.status_line().unwrap();
        assert_eq!(line.key, "test.failed");
        assert_eq!(line.args.get("reason").map(String::as_str), Some("boom"));
        assert!(p.still_owed());
    }

    #[test]
    fn settled_nothing_renders_none_and_is_not_owed() {
        let p = Passage::Settled(TestOutcome::Nothing);
        assert_eq!(p.status_line(), None);
        assert!(!p.still_owed());
    }

    #[test]
    fn settled_still_owed_renders_and_is_owed() {
        let p = Passage::Settled(TestOutcome::StillOwed);
        assert_eq!(p.status_line().unwrap().key, "test.owed_elsewhere");
        assert!(p.still_owed());
    }

    #[test]
    fn settled_done_renders_and_is_not_owed() {
        let p = Passage::Settled(TestOutcome::Done);
        assert_eq!(p.status_line().unwrap().key, "test.done");
        assert!(!p.still_owed());
    }
}
