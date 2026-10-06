//! What a sign-out's erase could **not** remove, as an app must paint it.
//!
//! The sweeps themselves answer *what survived* — the filesystem half
//! (`fauna_account_store::db::EraseSweep`, re-exported as
//! `fauna_sync_engine::db`) and the credential half ([`crate::CredentialSweep`]);
//! this module answers *what the user is told about it*, once, for every seat —
//! so windows, apple, android, linux and tui say the same thing about the same
//! outcome rather than each phrasing it in C#, Swift, Kotlin and two Rusts.
//!
//! **Why this exists at all.** A sign-out whose erase fails still completes —
//! deliberately, and that must not be reversed (`account-scoping.md` § Erasure
//! follows scope: sign-out must finish even on an I/O error). The defect is that
//! *proceeding* was indistinguishable from *succeeding*: every seat turned the
//! failure into a log line and painted a clean "Signed out" over a device that
//! still held the user's readable data. `principles.md` § The user always
//! controls their data puts the delete affordance in the app, and a log is not
//! an affordance.
//!
//! **Shape lifted from `fauna_client_recovery::ceremony::SweepView`** — the
//! house pattern for "a partial outcome an app may only localize": an
//! arm-derived render gate and a copy selector that owns every line. Its
//! affordance enum retired once every seat painted the residue surface's retry
//! control: the line always names Remove Again.

use fauna_core::localized::LocalizedText;

use crate::CredentialSweep;

/// Some of the user's data could not be removed from this device.
const KEY_RESIDUE: &str = "settings.sign_out_residue";
/// The user's sign-in credentials could not be removed from this device.
const KEY_RESIDUE_CREDENTIALS: &str = "settings.sign_out_residue_credentials";
/// Both at once — ONE line naming both, never two lines on a surface that
/// holds one.
const KEY_RESIDUE_WITH_CREDENTIALS: &str = "settings.sign_out_residue_with_credentials";

/// A second window of this app is still serving one of the accounts this
/// sign-out would erase, so nothing was erased and nothing was signed out
/// (`account-scoping.md` § Concurrent instances → *An erase refuses while a
/// sibling serves the account*).
const KEY_BLOCKED_BY_INSTANCE: &str = "settings.sign_out_blocked_other_window";

/// The line a seat paints when a sign-out is refused because another window of
/// this app still serves one of its accounts.
///
/// It lives here, with the rest of the sign-out copy, for the reason that copy
/// is shared at all: seven seats phrasing the same refusal seven ways is how a
/// user learns that Fauna means different things in different windows. The
/// remedy — close the other window, then sign out again — is named in the line,
/// because the refusal is otherwise indistinguishable from a sign-out that
/// silently did nothing.
pub fn sign_out_blocked_copy() -> LocalizedText {
    LocalizedText::key(KEY_BLOCKED_BY_INSTANCE)
}

/// Remove-account refused for the same reason: another window is serving the
/// account being removed ([`crate::remove_account_blocked`]).
const KEY_REMOVE_BLOCKED_BY_INSTANCE: &str = "settings.remove_account_blocked_other_window";

/// The unreadable-index floor refused for the same reason: another window is
/// serving an account its erase would reach ([`crate::sign_out_blocked`]).
const KEY_START_OVER_BLOCKED_BY_INSTANCE: &str =
    "onboarding.launch.index_malformed_reset_blocked_other_window";

/// The line a seat paints when a remove-account is refused because another
/// window still serves that account.
///
/// Not [`sign_out_blocked_copy`]'s line, because a refusal's line is mostly its
/// remedy, and the remedy names the gesture: a user who pressed *remove* and is
/// told to *sign out again* is told to do something else. One line per erasing
/// gesture, all three here — the same answer, phrased for the button pressed.
pub fn remove_account_blocked_copy() -> LocalizedText {
    LocalizedText::key(KEY_REMOVE_BLOCKED_BY_INSTANCE)
}

/// Remove-account refused because **this** window serves the account
/// ([`crate::RemoveAccountBlocked::ServedHere`]) — reachable from a bound
/// window, whose account is not the registry's active one.
const KEY_REMOVE_BLOCKED_SERVED_HERE: &str = "settings.remove_account_blocked_this_window";

/// The line a seat paints when a remove-account is refused because this very
/// window is using the account. Not [`remove_account_blocked_copy`]'s line: its
/// remedy (close the *other* window) would do nothing here.
pub fn remove_account_served_here_copy() -> LocalizedText {
    LocalizedText::key(KEY_REMOVE_BLOCKED_SERVED_HERE)
}

/// The line a seat paints when the unreadable-index floor ("start over on this
/// device") is refused because another window still serves an account its
/// erase would reach — see [`remove_account_blocked_copy`] for why it is its
/// own line. It says nothing was removed rather than "still signed in": on
/// that screen the user is not signed in to anything they can see.
pub fn start_over_blocked_copy() -> LocalizedText {
    LocalizedText::key(KEY_START_OVER_BLOCKED_BY_INSTANCE)
}

/// The residue's re-sweep refused for the same reason: another window serves
/// an account the residue belongs to.
const KEY_RESIDUE_RETRY_BLOCKED_BY_INSTANCE: &str =
    "settings.sign_out_residue_retry_blocked_other_window";

/// The line a seat paints on the residue surface when its re-sweep is refused
/// because another window still serves one of the accounts the residue belongs
/// to — the native `retry_sign_out_residue`'s `Blocked`, and web's refused
/// sweep over its sign-out record. Its own line, like the other erasing
/// gestures' refusals: the remedy names the button pressed (Remove Again).
pub fn sign_out_residue_retry_blocked_copy() -> LocalizedText {
    LocalizedText::key(KEY_RESIDUE_RETRY_BLOCKED_BY_INSTANCE)
}

/// The erase's outcome, reduced to what a surface needs: one number for the
/// filesystem half, one flag for the credential half.
///
/// Deliberately **not** the survivor paths or key names. They are in the log,
/// where whoever debugs the next one needs them (`account-scoping.md` § Erasure
/// follows scope → the ⚠ *the erase must SAY what it did* corollary); a user is
/// owed the fact and the remedy, and `%LOCALAPPDATA%\fauna\<64-hex>\mls.db` or
/// `fauna/<64-hex>/secret` is neither. It also keeps the type free of `PathBuf`,
/// so the FFI and wasm faces that carry it across a boundary carry a `u32` and a
/// `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EraseResidueView {
    /// How many paths the sweep tried to remove and could not — the length of
    /// `EraseSweep::survivors`.
    pub survivors: u32,
    /// Whether the credential erase left the user's sign-in credentials behind —
    /// a [`CredentialSweep`] that is not clean. A flag and not a count: *"3
    /// credentials"* is a number no user can act on, and the keys behind it
    /// (the identity seed among them) are one fact about the device — the
    /// account can still be signed into from here.
    pub credentials_survived: bool,
}

/// The residue's own line, as [`EraseResidueView::copy`] selects it.
///
/// `None` is a real value — the line a surface must **not** paint — never an
/// absence for the app to fill in. A clean sweep says nothing: *"all 0 items
/// were left behind"* is reassurance by vacuity, which the house copy refuses
/// here for the same reason the group sweep refuses it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EraseResidueCopy {
    /// What survived, and what to do about it.
    pub warning: Option<LocalizedText>,
}

impl EraseResidueView {
    /// Build the view from a sweep's survivor count, saturating rather than
    /// truncating: a survivor count past `u32::MAX` is not a real case, and a
    /// wrapped count would be a *smaller* number — the one direction that
    /// under-reports the user's exposure.
    #[must_use]
    pub fn from_survivor_count(survivors: usize) -> Self {
        Self {
            survivors: u32::try_from(survivors).unwrap_or(u32::MAX),
            credentials_survived: false,
        }
    }

    /// Fold the credential half of the erase in. Never talks an already-set
    /// flag back down: a clean sweep folded after a dirty one leaves the device
    /// dirty, the same one-way rule the filesystem fold keeps.
    ///
    /// ⚠ **Fold it in AFTER the credential wipe has run, not where the
    /// filesystem erase runs.** The filesystem erase must run first (it reads
    /// the registry the credential wipe destroys), and a line built at that
    /// point answers for half the erase — which is how the credential half
    /// went unasked on linux and tui until 2026-09-13.
    #[must_use]
    pub fn with_credentials(mut self, sweep: &CredentialSweep) -> Self {
        self.credentials_survived |= !sweep.is_clean();
        self
    }

    /// Whether the erase left the user's data on the device — the render gate
    /// for any retry control, and the one place that question is answered.
    ///
    /// Two owing arms, one per half of the erase: a filesystem sweep with
    /// survivors (`EraseSweep::is_clean` false), and a credential erase that is
    /// not clean ([`CredentialSweep::is_clean`] false). Only both clean means
    /// the device is clean.
    #[must_use]
    pub fn owes_work(&self) -> bool {
        self.survivors > 0 || self.credentials_survived
    }

    /// Select the residue's line. Every line names Remove Again
    /// (`sign-out-residue-retry-button`, driving
    /// [`crate::retry_sign_out_residue`]), the control every seat paints beside
    /// it on the `sign-out-residue` view.
    #[must_use]
    pub fn copy(&self) -> EraseResidueCopy {
        let count = || self.survivors.to_string();
        let warning = match (self.survivors > 0, self.credentials_survived) {
            (false, false) => None,
            (true, false) => Some(LocalizedText::key_arg(KEY_RESIDUE, "count", count())),
            (false, true) => Some(LocalizedText::key(KEY_RESIDUE_CREDENTIALS)),
            (true, true) => Some(LocalizedText::key_arg(
                KEY_RESIDUE_WITH_CREDENTIALS,
                "count",
                count(),
            )),
        };
        EraseResidueCopy { warning }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirty_credentials() -> CredentialSweep {
        CredentialSweep {
            survivors: vec!["fauna/aa11/secret".to_string()],
            wipe_failed: false,
        }
    }

    #[test]
    fn a_clean_sweep_says_nothing() {
        let view =
            EraseResidueView::from_survivor_count(0).with_credentials(&CredentialSweep::default());
        assert!(!view.owes_work());
        assert_eq!(view.copy().warning, None);
    }

    /// The defect the row exists for: a sign-out that left data behind must not
    /// be indistinguishable from one that did not.
    #[test]
    fn one_survivor_owes_work_and_carries_a_line() {
        let view = EraseResidueView::from_survivor_count(1);
        assert!(view.owes_work());
        let copy = view.copy();
        let line = copy.warning.expect("a survivor must produce a line");
        assert_eq!(line.key, KEY_RESIDUE);
        assert_eq!(
            line.args.get("count").map(String::as_str),
            Some("1"),
            "the count the user is shown is the survivor count"
        );
    }

    /// The credential half on its own: a filesystem sweep that removed
    /// everything must not talk a surviving identity seed back down to a clean
    /// "Signed out". The line names the credentials and carries NO count — the
    /// keys behind it are one fact about the device, and never reach the screen.
    #[test]
    fn surviving_credentials_alone_owe_work_and_name_the_credentials() {
        let view = EraseResidueView::from_survivor_count(0).with_credentials(&dirty_credentials());
        assert!(
            view.owes_work(),
            "a clean filesystem sweep is not a clean sign-out"
        );
        let line = view
            .copy()
            .warning
            .expect("surviving credentials owe the user a line");
        assert_eq!(line.key, KEY_RESIDUE_CREDENTIALS);
        assert!(
            line.args.is_empty(),
            "no count and no key name reach the user: {:?}",
            line.args
        );
    }

    /// The locked-keyring shape: nothing reads back (the store refuses reads
    /// too), but the wholesale wipe said it failed. That is still a residue.
    #[test]
    fn a_failed_wipe_with_nothing_readable_still_owes_work() {
        let mut sweep = CredentialSweep::default();
        sweep.record_wipe_failure();
        let view = EraseResidueView::from_survivor_count(0).with_credentials(&sweep);
        assert!(view.credentials_survived);
        assert!(view.owes_work());
    }

    /// Both halves failing is ONE line naming both, with the path count — the
    /// `sign-out-residue` view holds one line.
    #[test]
    fn both_halves_failing_is_one_line_naming_both() {
        let view = EraseResidueView::from_survivor_count(2).with_credentials(&dirty_credentials());
        let line = view.copy().warning.expect("owing");
        assert_eq!(line.key, KEY_RESIDUE_WITH_CREDENTIALS);
        assert_eq!(line.args.get("count").map(String::as_str), Some("2"));
    }

    /// The fold is one-way: a clean credential sweep folded after a dirty one
    /// leaves the device dirty.
    #[test]
    fn a_clean_fold_never_talks_a_dirty_flag_back_down() {
        let view = EraseResidueView::from_survivor_count(0)
            .with_credentials(&dirty_credentials())
            .with_credentials(&CredentialSweep::default());
        assert!(view.credentials_survived);
    }

    /// Saturating, not wrapping — the one direction that could under-report the
    /// user's exposure is the one direction this must never take.
    #[test]
    fn an_absurd_survivor_count_saturates_rather_than_wrapping_to_a_smaller_one() {
        let view = EraseResidueView::from_survivor_count(usize::MAX);
        assert_eq!(view.survivors, u32::MAX);
        assert!(view.owes_work());
    }
}
