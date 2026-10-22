//! The store-consent gate's native face (`account-age-band.md` § The account
//! age band → the store-law developer duties bullet; § App surface →
//! *Store-consent surfaces*) — android and iOS run
//! `fauna_client_family::store_consent` through this one call at launch and on
//! every foreground.
//!
//! **Nothing here decides anything.** The platform glue reads its seams (the
//! store signal, today's UTC date, the device-local state it stored last round)
//! into [`FfiStoreConsentInput`]; [`store_consent_round`] returns the verdict,
//! the state to write back, and the `store-consent-lock-message` lines — the
//! decision and its text in one call, the `screen_lock_message` precedent, so
//! the two legs cannot drift on when a child is locked out or what the lock
//! says. No web face: the five other apps declare the absence.
//!
//! `value-format`-gated: the message crosses as `LocalizedText`, a bare
//! `fauna_core` type (the uniffi-bindgen-go footgun `family.rs`'s screen-time
//! faces document) — and the Go mail-bridge has no lock to render.

use fauna_client_family::store_consent::{
    self as sc, AskAnswer, PlayChangeStatus, SignificantChange, StoreConsentInput,
    StoreConsentLockState, StoreConsentState, StoreConsentVerdict, StoreSignal, UtcDate,
};
use fauna_core::localized::LocalizedText;

/// FFI mirror of [`UtcDate`] — a calendar date in UTC.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfiUtcDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl From<FfiUtcDate> for UtcDate {
    fn from(d: FfiUtcDate) -> Self {
        UtcDate::new(d.year, d.month, d.day)
    }
}

impl From<UtcDate> for FfiUtcDate {
    fn from(d: UtcDate) -> Self {
        Self {
            year: d.year,
            month: d.month,
            day: d.day,
        }
    }
}

/// FFI mirror of [`PlayChangeStatus`] — Play Age Signals'
/// `significantChangeStatus`.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiPlayChangeStatus {
    Approved,
    Pending,
    Declined,
}

impl From<FfiPlayChangeStatus> for PlayChangeStatus {
    fn from(s: FfiPlayChangeStatus) -> Self {
        match s {
            FfiPlayChangeStatus::Approved => Self::Approved,
            FfiPlayChangeStatus::Pending => Self::Pending,
            FfiPlayChangeStatus::Declined => Self::Declined,
        }
    }
}

/// FFI mirror of [`AskAnswer`] — the PermissionKit answer last received (iOS).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiAskAnswer {
    Approved,
    Declined,
}

impl From<FfiAskAnswer> for AskAnswer {
    fn from(a: FfiAskAnswer) -> Self {
        match a {
            FfiAskAnswer::Approved => Self::Approved,
            FfiAskAnswer::Declined => Self::Declined,
        }
    }
}

/// FFI mirror of [`StoreSignal`] — the store's own signal for the person on
/// this device.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiStoreSignal {
    /// iOS: `requiredRegulatoryFeatures`' two significant-change members, the
    /// PermissionKit answer, `AppStore.ageRatingCode` read now.
    Apple {
        parental_consent_required: bool,
        adult_notification_required: bool,
        ask_answer: Option<FfiAskAnswer>,
        current_rating: Option<i64>,
    },
    /// android: Play Age Signals' `significantChangeStatus` and
    /// `significantChangeApprovalDate`.
    Play {
        status: Option<FfiPlayChangeStatus>,
        approval_date: Option<FfiUtcDate>,
    },
}

impl From<FfiStoreSignal> for StoreSignal {
    fn from(s: FfiStoreSignal) -> Self {
        match s {
            FfiStoreSignal::Apple {
                parental_consent_required,
                adult_notification_required,
                ask_answer,
                current_rating,
            } => Self::Apple {
                parental_consent_required,
                adult_notification_required,
                ask_answer: ask_answer.map(Into::into),
                current_rating,
            },
            FfiStoreSignal::Play {
                status,
                approval_date,
            } => Self::Play {
                status: status.map(Into::into),
                approval_date: approval_date.map(Into::into),
            },
        }
    }
}

/// FFI mirror of [`StoreConsentState`] — what the glue stores on the device
/// between rounds. Both `None` on first run.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FfiStoreConsentState {
    pub consented_through: Option<u32>,
    pub last_rating: Option<i64>,
}

impl From<FfiStoreConsentState> for StoreConsentState {
    fn from(s: FfiStoreConsentState) -> Self {
        Self {
            consented_through: s.consented_through,
            last_rating: s.last_rating,
        }
    }
}

impl From<StoreConsentState> for FfiStoreConsentState {
    fn from(s: StoreConsentState) -> Self {
        Self {
            consented_through: s.consented_through,
            last_rating: s.last_rating,
        }
    }
}

/// FFI mirror of [`StoreConsentInput`] — every seam the glue supplies.
/// `signal: None` (no signal, or a `foss` android build) never gates.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiStoreConsentInput {
    pub signal: Option<FfiStoreSignal>,
    pub stored: FfiStoreConsentState,
    pub today: FfiUtcDate,
}

/// FFI mirror of [`SignificantChange`] — one pending change; `description` is
/// its `en.yaml` key (the iOS ask and acknowledgment sheet carry it).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSignificantChange {
    pub id: u32,
    pub effective_from: FfiUtcDate,
    pub description: String,
}

impl From<&SignificantChange> for FfiSignificantChange {
    fn from(c: &SignificantChange) -> Self {
        Self {
            id: c.id,
            effective_from: c.effective_from.into(),
            description: c.description.to_owned(),
        }
    }
}

/// FFI mirror of [`StoreConsentLockState`].
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiStoreConsentLockState {
    Pending,
    Declined,
}

/// FFI mirror of [`StoreConsentVerdict`].
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiStoreConsentVerdict {
    NoGate,
    Clear,
    /// Render `store-consent-lock`; `can_ask` = render
    /// `store-consent-lock-ask-button` (iOS).
    Locked {
        entries: Vec<FfiSignificantChange>,
        state: FfiStoreConsentLockState,
        can_ask: bool,
    },
    /// iOS: show `showSignificantUpdateAcknowledgment`, then write `persist`.
    AdultAcknowledgment {
        entries: Vec<FfiSignificantChange>,
    },
}

impl From<&StoreConsentVerdict> for FfiStoreConsentVerdict {
    fn from(v: &StoreConsentVerdict) -> Self {
        let entries = |e: &[SignificantChange]| e.iter().map(Into::into).collect();
        match v {
            StoreConsentVerdict::NoGate => Self::NoGate,
            StoreConsentVerdict::Clear => Self::Clear,
            StoreConsentVerdict::Locked {
                entries: e,
                state,
                can_ask,
            } => Self::Locked {
                entries: entries(e),
                state: match state {
                    StoreConsentLockState::Pending => FfiStoreConsentLockState::Pending,
                    StoreConsentLockState::Declined => FfiStoreConsentLockState::Declined,
                },
                can_ask: *can_ask,
            },
            StoreConsentVerdict::AdultAcknowledgment { entries: e } => Self::AdultAcknowledgment {
                entries: entries(e),
            },
        }
    }
}

/// One round's answer: the verdict, the device-local state to write back
/// (always — it carries the first-run baseline), and the lock's message lines
/// (`Some` exactly when the verdict is `Locked`; resolve each through the
/// app's own localization pipeline and render them in order).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiStoreConsentRound {
    pub verdict: FfiStoreConsentVerdict,
    pub persist: FfiStoreConsentState,
    pub message: Option<Vec<LocalizedText>>,
}

/// `fauna_client_family::store_consent::store_consent_round` over the shipped
/// significant-change ledger, plus `store_consent_lock_message` over its
/// verdict — the whole store-consent decision in one call.
#[uniffi::export]
pub fn store_consent_round(input: FfiStoreConsentInput) -> FfiStoreConsentRound {
    let round = sc::store_consent_round(&StoreConsentInput {
        signal: input.signal.map(Into::into),
        stored: input.stored.into(),
        today: input.today.into(),
    });
    FfiStoreConsentRound {
        verdict: (&round.verdict).into(),
        persist: round.persist.into(),
        message: sc::store_consent_lock_message(&round.verdict),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(signal: Option<FfiStoreSignal>) -> FfiStoreConsentInput {
        FfiStoreConsentInput {
            signal,
            stored: FfiStoreConsentState::default(),
            today: FfiUtcDate {
                year: 2027,
                month: 1,
                day: 1,
            },
        }
    }

    /// The shipped ledger is empty: no signal never gates, a covered person
    /// is clear, and the first run's baseline crosses back to persist.
    #[test]
    fn the_face_carries_the_shared_round() {
        let r = store_consent_round(input(None));
        assert_eq!(r.verdict, FfiStoreConsentVerdict::NoGate);
        assert_eq!(r.persist.consented_through, Some(0));
        assert!(r.message.is_none());

        let r = store_consent_round(input(Some(FfiStoreSignal::Apple {
            parental_consent_required: true,
            adult_notification_required: false,
            ask_answer: None,
            current_rating: Some(4),
        })));
        assert_eq!(r.verdict, FfiStoreConsentVerdict::Clear);
        assert_eq!(r.persist.last_rating, Some(4));
    }

    /// A Play PENDING locks with the message, no ask, and the play hint.
    #[test]
    fn a_play_pending_crosses_with_its_message() {
        let r = store_consent_round(input(Some(FfiStoreSignal::Play {
            status: Some(FfiPlayChangeStatus::Pending),
            approval_date: None,
        })));
        assert_eq!(
            r.verdict,
            FfiStoreConsentVerdict::Locked {
                entries: vec![],
                state: FfiStoreConsentLockState::Pending,
                can_ask: false,
            }
        );
        let keys: Vec<String> = r.message.unwrap().into_iter().map(|l| l.key).collect();
        assert_eq!(
            keys.last().map(String::as_str),
            Some("family.store_consent_lock.play_hint")
        );
    }
}
