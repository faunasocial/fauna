//! The **store-consent gate** (`account-age-band.md` § The account age band →
//! the store-law developer duties bullet, D-S1…D-S4 and D-S6): the one
//! shared-Rust answer both store-distributed mobile apps ask at launch and on
//! every return to the foreground — *must this device show the whole-app
//! store-consent lock?* — plus the significant-change ledger it reads and the
//! lock's message.
//!
//! The gate is a **store construct about one device**: it keys on the
//! platform's own signal for the person the store knows on this device (iOS
//! `AgeRangeService.requiredRegulatoryFeatures`, android Play Age Signals'
//! `significantChangeStatus`), never on `supervised_by` or the age band (D2,
//! D3). No nest state, no wire field — the only persisted input is the
//! device-local [`StoreConsentState`] the platform glue stores and hands back.
//!
//! Shape: the FaunaKit `StoreAgeClaimRound` precedent, in Rust so both legs
//! share it — the platform glue reads its seams (the store signal, today's
//! date, the stored state) and this module decides. Nothing here touches a
//! platform API, so every branch is pinned over fixtures below.
//!
//! The UniFFI faces are `fauna_ffi::store_consent_round` and
//! `fauna_ffi::store_consent_lock_message`; web has no face (the five other
//! apps declare the absence — a store construct, like the kids door).

use fauna_core::localized::LocalizedText;

/// A calendar date in UTC — the ledger's `effective_from`, Play's
/// `significantChangeApprovalDate` and "today". Field order makes the derived
/// `Ord` chronological.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl UtcDate {
    pub const fn new(year: u16, month: u8, day: u8) -> Self {
        Self { year, month, day }
    }
}

/// One entry of the significant-change ledger (D-S2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignificantChange {
    /// Monotonic; the first entry is 1 (0 means "no entry" — the empty
    /// ledger's head, and the synthetic rating-change entry's id).
    pub id: u32,
    /// The date entered for this change in Play Console's *Age signals* page —
    /// android compares it against Play's approval date.
    pub effective_from: UtcDate,
    /// The `en.yaml` key of the one sentence a parent reads (Apple's dialog,
    /// Play Console, the lock message).
    pub description: &'static str,
}

/// **The significant-change ledger — EMPTY, and so by design until the owner
/// admits the first entry at a release (D-S8 as decided 2026-10-21: the owner's
/// own significance call, no legal review).** The owner appends an entry at a
/// release against the four criteria (data, rating, monetization,
/// functionality — the goal doc's item (2)), with its description key in
/// `en.yaml` under `family.store_consent_lock.changes`, and submits the same
/// date + sentence in Play Console. The mechanism must ship at least one
/// release before the first entry, so every install has a `consented_through`
/// baseline before anything is significant.
pub const SIGNIFICANT_CHANGES: &[SignificantChange] = &[];

/// The description of the synthetic entry a changed `ageRatingCode` enters the
/// round as (D-S4).
pub const RATING_CHANGE_DESCRIPTION: &str = "family.store_consent_lock.rating_changed";

/// Play Age Signals' `significantChangeStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayChangeStatus {
    Approved,
    Pending,
    Declined,
}

/// The answer to this device's PermissionKit significant-update ask (iOS), as
/// the glue last received it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskAnswer {
    Approved,
    Declined,
}

/// The store's own signal for the person on this device. `None` in
/// [`StoreConsentInput::signal`] — no signal read, or a `foss` android build,
/// which links no Play code — is never a gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreSignal {
    /// iOS: the two significant-change members of
    /// `AgeRangeService.requiredRegulatoryFeatures`, the PermissionKit answer,
    /// and `AppStore.ageRatingCode` read now (`None` if the read failed).
    Apple {
        parental_consent_required: bool,
        adult_notification_required: bool,
        ask_answer: Option<AskAnswer>,
        current_rating: Option<i64>,
    },
    /// android: Play Age Signals' `significantChangeStatus` (`None` = Play
    /// returned none — the duty does not apply) and
    /// `significantChangeApprovalDate`.
    Play {
        status: Option<PlayChangeStatus>,
        approval_date: Option<UtcDate>,
    },
}

/// What this install keeps on the device between rounds. Both start `None`;
/// the round's [`StoreConsentRound::persist`] is written back after every
/// round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreConsentState {
    /// The ledger id this install has consent through (iOS). Written at first
    /// run as the then-current head — the store obtained consent to the
    /// download of this very version.
    pub consented_through: Option<u32>,
    /// The last `ageRatingCode` this install saw (iOS). The first read is a
    /// silent baseline.
    pub last_rating: Option<i64>,
}

/// The round's inputs — every seam the platform glue supplies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConsentInput {
    pub signal: Option<StoreSignal>,
    pub stored: StoreConsentState,
    pub today: UtcDate,
}

/// Whether a lock is waiting on the parent or was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreConsentLockState {
    Pending,
    Declined,
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreConsentVerdict {
    /// The duty does not apply to this person (or no signal, or `foss`).
    NoGate,
    /// The duty applies and nothing is pending.
    Clear,
    /// Render the `store-consent-lock`. `entries` are every pending change,
    /// coalesced into one ask; `can_ask` is true on iOS (the
    /// `store-consent-lock-ask-button`), false on android (Play asks itself).
    Locked {
        entries: Vec<SignificantChange>,
        state: StoreConsentLockState,
        can_ask: bool,
    },
    /// iOS adult notification: show the system
    /// `showSignificantUpdateAcknowledgment` sheet for `entries`, no Fauna
    /// surface and no lock. The round's `persist` already counts them
    /// acknowledged — write it back once the sheet was shown.
    AdultAcknowledgment { entries: Vec<SignificantChange> },
}

/// The verdict plus the device-local state to write back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConsentRound {
    pub verdict: StoreConsentVerdict,
    pub persist: StoreConsentState,
}

/// Run the round over the shipped [`SIGNIFICANT_CHANGES`].
pub fn store_consent_round(input: &StoreConsentInput) -> StoreConsentRound {
    store_consent_round_with(SIGNIFICANT_CHANGES, input)
}

/// [`store_consent_round`] over an injected ledger — the seam the tier_1 pins
/// drive, since the shipped ledger is empty.
pub fn store_consent_round_with(
    ledger: &[SignificantChange],
    input: &StoreConsentInput,
) -> StoreConsentRound {
    let head = ledger.last().map_or(0, |c| c.id);
    // The first-run baseline is written whatever the signal says: the duty
    // may start applying to this device later, and the install must already
    // know what it was downloaded at.
    let mut persist = StoreConsentState {
        consented_through: Some(input.stored.consented_through.unwrap_or(head)),
        last_rating: input.stored.last_rating,
    };
    let verdict = match &input.signal {
        None => StoreConsentVerdict::NoGate,
        Some(StoreSignal::Apple {
            parental_consent_required,
            adult_notification_required,
            ask_answer,
            current_rating,
        }) => {
            // A failed rating read keeps the stored value; a first read is a
            // silent baseline; only a change of a known value is a trigger.
            let rating_changed = matches!(
                (input.stored.last_rating, current_rating),
                (Some(last), Some(now)) if last != *now
            );
            if input.stored.last_rating.is_none() {
                persist.last_rating = *current_rating;
            }
            let through = persist.consented_through.unwrap_or(head);
            let mut entries: Vec<SignificantChange> =
                ledger.iter().filter(|c| c.id > through).cloned().collect();
            if rating_changed {
                entries.push(SignificantChange {
                    id: 0,
                    effective_from: input.today,
                    description: RATING_CHANGE_DESCRIPTION,
                });
            }
            let consented = StoreConsentState {
                consented_through: Some(head),
                last_rating: current_rating.or(input.stored.last_rating),
            };
            if *parental_consent_required {
                if entries.is_empty() {
                    StoreConsentVerdict::Clear
                } else {
                    match ask_answer {
                        Some(AskAnswer::Approved) => {
                            persist = consented;
                            StoreConsentVerdict::Clear
                        }
                        Some(AskAnswer::Declined) => StoreConsentVerdict::Locked {
                            entries,
                            state: StoreConsentLockState::Declined,
                            can_ask: true,
                        },
                        None => StoreConsentVerdict::Locked {
                            entries,
                            state: StoreConsentLockState::Pending,
                            can_ask: true,
                        },
                    }
                }
            } else if *adult_notification_required {
                if entries.is_empty() {
                    StoreConsentVerdict::Clear
                } else {
                    persist = consented;
                    StoreConsentVerdict::AdultAcknowledgment { entries }
                }
            } else {
                StoreConsentVerdict::NoGate
            }
        }
        // Play returned no status: the duty does not apply to this person.
        Some(StoreSignal::Play { status: None, .. }) => StoreConsentVerdict::NoGate,
        Some(StoreSignal::Play {
            status: Some(status),
            approval_date,
        }) => {
            // Pending = in effect by today and later than what the parent
            // approved through. No approval date: nothing approved yet.
            let entries: Vec<SignificantChange> = ledger
                .iter()
                .filter(|c| {
                    c.effective_from <= input.today
                        && approval_date.is_none_or(|approved| c.effective_from > approved)
                })
                .cloned()
                .collect();
            let lock = |state| StoreConsentVerdict::Locked {
                entries: entries.clone(),
                state,
                can_ask: false,
            };
            match status {
                PlayChangeStatus::Declined => lock(StoreConsentLockState::Declined),
                PlayChangeStatus::Pending => lock(StoreConsentLockState::Pending),
                // An APPROVED status with no date to compare trusts the
                // store; with a date, a later entry is still pending.
                PlayChangeStatus::Approved if approval_date.is_some() && !entries.is_empty() => {
                    lock(StoreConsentLockState::Pending)
                }
                PlayChangeStatus::Approved => StoreConsentVerdict::Clear,
            }
        }
    };
    StoreConsentRound { verdict, persist }
}

/// The `store-consent-lock-message`, derived once (the
/// `screen-time-lock-message` twin): `None` unless the verdict is
/// [`StoreConsentVerdict::Locked`], else its lines in order — the title, the
/// what-changed lead and one line per pending change, the state, the platform
/// hint. Lines, never a pre-joined string, so each shell resolves every key
/// through its own localization pipeline.
pub fn store_consent_lock_message(verdict: &StoreConsentVerdict) -> Option<Vec<LocalizedText>> {
    let StoreConsentVerdict::Locked {
        entries,
        state,
        can_ask,
    } = verdict
    else {
        return None;
    };
    let mut lines = vec![LocalizedText::key("family.store_consent_lock.title")];
    if !entries.is_empty() {
        lines.push(LocalizedText::key("family.store_consent_lock.what_changed"));
        lines.extend(entries.iter().map(|c| LocalizedText::key(c.description)));
    }
    lines.push(LocalizedText::key(match state {
        StoreConsentLockState::Pending => "family.store_consent_lock.state_pending",
        StoreConsentLockState::Declined => "family.store_consent_lock.state_declined",
    }));
    lines.push(LocalizedText::key(if *can_ask {
        "family.store_consent_lock.ask_hint"
    } else {
        "family.store_consent_lock.play_hint"
    }));
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: fn(u16, u8, u8) -> UtcDate = UtcDate::new;

    fn ledger() -> Vec<SignificantChange> {
        vec![
            SignificantChange {
                id: 1,
                effective_from: D(2027, 1, 10),
                description: "test.change_one",
            },
            SignificantChange {
                id: 2,
                effective_from: D(2027, 3, 1),
                description: "test.change_two",
            },
        ]
    }

    fn apple(parental: bool, adult: bool, answer: Option<AskAnswer>) -> Option<StoreSignal> {
        Some(StoreSignal::Apple {
            parental_consent_required: parental,
            adult_notification_required: adult,
            ask_answer: answer,
            current_rating: Some(4),
        })
    }

    fn input(signal: Option<StoreSignal>, through: Option<u32>) -> StoreConsentInput {
        StoreConsentInput {
            signal,
            stored: StoreConsentState {
                consented_through: through,
                last_rating: Some(4),
            },
            today: D(2027, 3, 5),
        }
    }

    fn round(i: &StoreConsentInput) -> StoreConsentVerdict {
        store_consent_round_with(&ledger(), i).verdict
    }

    fn ids(v: &StoreConsentVerdict) -> Vec<u32> {
        match v {
            StoreConsentVerdict::Locked { entries, .. }
            | StoreConsentVerdict::AdultAcknowledgment { entries } => {
                entries.iter().map(|c| c.id).collect()
            }
            _ => vec![],
        }
    }

    #[test]
    fn the_shipped_ledger_has_monotonic_ids_and_non_decreasing_dates() {
        for pair in SIGNIFICANT_CHANGES.windows(2) {
            assert!(pair[0].id < pair[1].id);
            assert!(pair[0].effective_from <= pair[1].effective_from);
        }
        assert!(SIGNIFICANT_CHANGES.first().is_none_or(|c| c.id >= 1));
    }

    /// Until the owner admits the first entry, nobody is asked.
    #[test]
    fn the_shipped_ledger_asks_nobody_anything() {
        let i = input(apple(true, false, None), Some(0));
        assert_eq!(store_consent_round(&i).verdict, StoreConsentVerdict::Clear);
        let play = input(
            Some(StoreSignal::Play {
                status: Some(PlayChangeStatus::Approved),
                approval_date: None,
            }),
            None,
        );
        assert_eq!(
            store_consent_round(&play).verdict,
            StoreConsentVerdict::Clear
        );
    }

    #[test]
    fn duty_absent_is_no_gate() {
        assert_eq!(round(&input(None, Some(0))), StoreConsentVerdict::NoGate);
        assert_eq!(
            round(&input(apple(false, false, None), Some(0))),
            StoreConsentVerdict::NoGate
        );
        let play = Some(StoreSignal::Play {
            status: None,
            approval_date: None,
        });
        assert_eq!(round(&input(play, None)), StoreConsentVerdict::NoGate);
    }

    /// First run persists the head as the download consent, whatever the
    /// signal — the baseline the build rule relies on.
    #[test]
    fn first_run_baselines_consent_at_the_head() {
        let r = store_consent_round_with(&ledger(), &input(None, None));
        assert_eq!(r.persist.consented_through, Some(2));
        let r = store_consent_round_with(&ledger(), &input(apple(true, false, None), None));
        assert_eq!(r.persist.consented_through, Some(2));
        assert_eq!(r.verdict, StoreConsentVerdict::Clear);
    }

    #[test]
    fn nothing_pending_is_clear() {
        assert_eq!(
            round(&input(apple(true, false, None), Some(2))),
            StoreConsentVerdict::Clear
        );
    }

    #[test]
    fn pending_entries_coalesce_into_one_lock() {
        let v = round(&input(apple(true, false, None), Some(0)));
        assert_eq!(ids(&v), vec![1, 2]);
        assert!(matches!(
            v,
            StoreConsentVerdict::Locked {
                state: StoreConsentLockState::Pending,
                can_ask: true,
                ..
            }
        ));
        assert_eq!(
            ids(&round(&input(apple(true, false, None), Some(1)))),
            vec![2]
        );
    }

    #[test]
    fn declined_keeps_the_lock_and_the_ask() {
        let r = store_consent_round_with(
            &ledger(),
            &input(apple(true, false, Some(AskAnswer::Declined)), Some(1)),
        );
        assert!(matches!(
            r.verdict,
            StoreConsentVerdict::Locked {
                state: StoreConsentLockState::Declined,
                can_ask: true,
                ..
            }
        ));
        assert_eq!(r.persist.consented_through, Some(1));
    }

    #[test]
    fn approval_consents_through_the_head() {
        let r = store_consent_round_with(
            &ledger(),
            &input(apple(true, false, Some(AskAnswer::Approved)), Some(0)),
        );
        assert_eq!(r.verdict, StoreConsentVerdict::Clear);
        assert_eq!(r.persist.consented_through, Some(2));
    }

    #[test]
    fn adult_notification_acknowledges_without_a_lock() {
        let r = store_consent_round_with(&ledger(), &input(apple(false, true, None), Some(1)));
        assert_eq!(
            r.verdict,
            StoreConsentVerdict::AdultAcknowledgment {
                entries: vec![ledger()[1].clone()]
            }
        );
        assert_eq!(r.persist.consented_through, Some(2));
        assert_eq!(
            round(&input(apple(false, true, None), Some(2))),
            StoreConsentVerdict::Clear
        );
    }

    #[test]
    fn rating_first_read_is_a_silent_baseline() {
        let mut i = input(apple(true, false, None), Some(2));
        i.stored.last_rating = None;
        let r = store_consent_round_with(&ledger(), &i);
        assert_eq!(r.verdict, StoreConsentVerdict::Clear);
        assert_eq!(r.persist.last_rating, Some(4));
    }

    #[test]
    fn rating_change_is_a_synthetic_pending_entry() {
        let mut i = input(apple(true, false, None), Some(2));
        i.stored.last_rating = Some(3);
        let r = store_consent_round_with(&ledger(), &i);
        let StoreConsentVerdict::Locked { entries, .. } = &r.verdict else {
            panic!("expected a lock, got {:?}", r.verdict);
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 0);
        assert_eq!(entries[0].description, RATING_CHANGE_DESCRIPTION);
        assert_eq!(entries[0].effective_from, i.today);
        // The stored rating moves only with consent.
        assert_eq!(r.persist.last_rating, Some(3));
        i.signal = apple(true, false, Some(AskAnswer::Approved));
        let r = store_consent_round_with(&ledger(), &i);
        assert_eq!(r.verdict, StoreConsentVerdict::Clear);
        assert_eq!(r.persist.last_rating, Some(4));
        // A failed read is no trigger and forgets nothing.
        i.signal = Some(StoreSignal::Apple {
            parental_consent_required: true,
            adult_notification_required: false,
            ask_answer: None,
            current_rating: None,
        });
        let r = store_consent_round_with(&ledger(), &i);
        assert_eq!(r.verdict, StoreConsentVerdict::Clear);
        assert_eq!(r.persist.last_rating, Some(3));
    }

    /// The rating trigger joins pending ledger entries in one ask.
    #[test]
    fn rating_change_coalesces_with_ledger_entries() {
        let mut i = input(apple(true, false, None), Some(1));
        i.stored.last_rating = Some(3);
        assert_eq!(ids(&round(&i)), vec![2, 0]);
    }

    fn play(
        status: PlayChangeStatus,
        approval: Option<UtcDate>,
        today: UtcDate,
    ) -> StoreConsentVerdict {
        let mut i = input(
            Some(StoreSignal::Play {
                status: Some(status),
                approval_date: approval,
            }),
            None,
        );
        i.today = today;
        round(&i)
    }

    #[test]
    fn play_pending_and_declined_lock_without_an_ask() {
        let v = play(
            PlayChangeStatus::Pending,
            Some(D(2027, 1, 10)),
            D(2027, 3, 5),
        );
        assert_eq!(ids(&v), vec![2]);
        assert!(matches!(
            v,
            StoreConsentVerdict::Locked {
                state: StoreConsentLockState::Pending,
                can_ask: false,
                ..
            }
        ));
        assert!(matches!(
            play(
                PlayChangeStatus::Declined,
                Some(D(2027, 1, 10)),
                D(2027, 3, 5)
            ),
            StoreConsentVerdict::Locked {
                state: StoreConsentLockState::Declined,
                can_ask: false,
                ..
            }
        ));
    }

    /// The date comparison at each boundary: `effective_from` against today
    /// (in effect on its own day) and against the approval date (approved
    /// through it, inclusive).
    #[test]
    fn play_date_boundaries() {
        use PlayChangeStatus::Approved;
        let eff = D(2027, 3, 1);
        // Against today: the day before it is not yet in effect.
        assert_eq!(
            play(Approved, Some(D(2027, 1, 10)), D(2027, 2, 28)),
            StoreConsentVerdict::Clear
        );
        assert_eq!(ids(&play(Approved, Some(D(2027, 1, 10)), eff)), vec![2]);
        assert_eq!(
            ids(&play(Approved, Some(D(2027, 1, 10)), D(2027, 3, 2))),
            vec![2]
        );
        // Against the approval date: approved through the day before leaves
        // it pending; the day itself or after clears it.
        assert_eq!(ids(&play(Approved, Some(D(2027, 2, 28)), eff)), vec![2]);
        assert_eq!(play(Approved, Some(eff), eff), StoreConsentVerdict::Clear);
        assert_eq!(
            play(Approved, Some(D(2027, 3, 2)), D(2027, 3, 5)),
            StoreConsentVerdict::Clear
        );
    }

    #[test]
    fn play_approved_without_a_date_trusts_the_store() {
        assert_eq!(
            play(PlayChangeStatus::Approved, None, D(2027, 3, 5)),
            StoreConsentVerdict::Clear
        );
    }

    #[test]
    fn the_lock_message_follows_the_verdict() {
        assert!(store_consent_lock_message(&StoreConsentVerdict::Clear).is_none());
        assert!(store_consent_lock_message(&StoreConsentVerdict::NoGate).is_none());
        let v = round(&input(
            apple(true, false, Some(AskAnswer::Declined)),
            Some(0),
        ));
        let keys: Vec<String> = store_consent_lock_message(&v)
            .unwrap()
            .into_iter()
            .map(|l| l.key)
            .collect();
        assert_eq!(
            keys,
            [
                "family.store_consent_lock.title",
                "family.store_consent_lock.what_changed",
                "test.change_one",
                "test.change_two",
                "family.store_consent_lock.state_declined",
                "family.store_consent_lock.ask_hint",
            ]
        );
        let v = play(PlayChangeStatus::Pending, None, D(2027, 1, 1));
        let keys: Vec<String> = store_consent_lock_message(&v)
            .unwrap()
            .into_iter()
            .map(|l| l.key)
            .collect();
        assert_eq!(
            keys,
            [
                "family.store_consent_lock.title",
                "family.store_consent_lock.state_pending",
                "family.store_consent_lock.play_hint",
            ]
        );
    }

    /// Every key the message can carry resolves in `en.yaml`, the shipped
    /// ledger's descriptions included, and so does the iOS ask button's label.
    /// The lock shows before sign-in, so these strings are public texts
    /// (`public-texts.md` § The boundary), landed in public-texts round 2.
    #[test]
    fn every_message_key_resolves() {
        let keys = [
            "family.store_consent_lock.title",
            "family.store_consent_lock.what_changed",
            "family.store_consent_lock.state_pending",
            "family.store_consent_lock.state_declined",
            "family.store_consent_lock.ask_hint",
            "family.store_consent_lock.ask_button",
            "family.store_consent_lock.play_hint",
            RATING_CHANGE_DESCRIPTION,
        ];
        for key in keys
            .into_iter()
            .chain(SIGNIFICANT_CHANGES.iter().map(|c| c.description))
        {
            assert!(fauna_i18n::strings::lookup(key).is_some(), "{key}");
        }
    }
}
