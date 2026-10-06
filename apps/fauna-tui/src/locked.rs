//! The locked launch surface and its one action (`devices.md` § The locked
//! state; `ui/sessions.md` § Layout & flow → *The locked surface (leg 2)*).
//!
//! Two screens, both launch surfaces — a locked account has no session, so
//! neither can live in the signed-in shell:
//!
//! - **`launch_account_locked`** ([`LaunchSurface::AccountLocked`]): the
//!   standing `launch-account-locked-notice` — the unlock time and the sentence
//!   that a lock the owner did not set means somebody holds the secret key —
//!   and `launch-account-locked-stolen-button`. No retry and no fallthrough:
//!   nothing clears the lock before its time, and the launch machine runs the
//!   one refresh at that time itself ([`follow_snapshot`]).
//! - **`identity_stolen_entry`** ([`LaunchSurface::IdentityStolenEntry`]): the
//!   stolen-identity ceremony from outside a session, painted with the Settings
//!   Recovery kit section's own ids. It runs the shared
//!   `fauna_client_recovery::ceremony::succeed_stolen_identity` — the same
//!   composition Settings calls — over an anonymous connection with the seed
//!   this device still holds, and adopts the successor through
//!   [`crate::session::adopt_landed_succession`], the existing account switch.
//!
//! The buffers live on `App` ([`LockedState`]) for the reason the unlock
//! surface's do: the inputs paint them, and the surface enum cannot reach them.

use std::sync::Arc;

use fauna_client_recovery::ceremony::{LandedSuccession, STOLEN_CONFIRM_WORD, StolenOutcome};
use fauna_core::secret::SecretString;
use fauna_i18n::strings::onboarding::launch as l;
use fauna_i18n::strings::settings::recovery_kit as rk;
use fauna_launch_machine::{LaunchPhase, LaunchSnapshot};
use fauna_ui_ids as ids;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, UiMessage};
use crate::element::{Element, Field, Gesture};
use crate::launch::{LaunchAction, LaunchSurface};

/// `identity_stolen_entry`'s local state.
#[derive(Default)]
pub struct LockedState {
    /// `recovery-entry-phrase-field` — the pasted kit. A secret that outranks
    /// the seed, so it zeroizes on drop and is cleared the moment the ceremony
    /// has consumed it.
    pub phrase: SecretString,
    /// `identity-stolen-confirm-field` — the literal [`STOLEN_CONFIRM_WORD`],
    /// never a secret.
    pub confirm: String,
    /// The ceremony is in flight: the button is disabled, and a second press
    /// must not mint a second successor.
    pub busy: bool,
    /// `error-message`. Kept across Back on purpose — one arm of the ceremony
    /// puts the successor seed in this sentence as the only way back in, and
    /// leaving the step must not take it off the screen
    /// ([`StolenOutcome::carries_the_only_seed`]).
    pub error: Option<String>,
}

/// An editable field on `identity_stolen_entry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockedField {
    Phrase,
    Confirm,
}

pub fn field(state: &LockedState, field: &LockedField) -> String {
    match field {
        LockedField::Phrase => state.phrase.as_str().to_string(),
        LockedField::Confirm => state.confirm.clone(),
    }
}

pub fn set_field(state: &mut LockedState, field: LockedField, value: String) {
    match field {
        LockedField::Phrase => state.phrase = value.into(),
        LockedField::Confirm => state.confirm = value,
    }
}

/// The standing notice: the shared lock sentence, the unlock time through the
/// shared `format_unix_local`, and the REQUIRED sentence that a lock the owner
/// did not set means somebody holds the secret key (`devices.md` § The two
/// panic buttons, point 5).
pub fn notice_text(locked_until_secs: u64) -> String {
    let time =
        fauna_core::format::format_unix_local(i64::try_from(locked_until_secs).unwrap_or(i64::MAX));
    format!(
        "{} {} {}",
        l::ACCOUNT_LOCKED,
        l::account_locked_until(&time),
        l::ACCOUNT_LOCKED_NOT_YOURS
    )
}

/// `launch_account_locked` (ui.yaml): the notice and its one action.
pub fn notice_elements(locked_until_secs: u64) -> Vec<Element> {
    vec![
        Element::label(
            ids::LAUNCH_ACCOUNT_LOCKED_NOTICE,
            notice_text(locked_until_secs),
        ),
        // The Settings section's own label: one name for the action wherever
        // it is offered.
        Element::launch_button(
            ids::LAUNCH_ACCOUNT_LOCKED_STOLEN_BUTTON,
            rk::STOLEN,
            LaunchAction::OpenStolenEntry,
        ),
    ]
}

/// `identity_stolen_entry` (ui.yaml) — existing elements only.
pub fn entry_elements(app: &App) -> Vec<Element> {
    let state = &app.locked;
    vec![
        Element::input(
            ids::RECOVERY_ENTRY_PHRASE_FIELD,
            state.phrase.as_str().to_string(),
            Field::Locked(LockedField::Phrase),
        )
        .labelled(rk::KIT_PHRASE_PLACEHOLDER),
        Element::input(
            ids::IDENTITY_STOLEN_CONFIRM_FIELD,
            state.confirm.clone(),
            Field::Locked(LockedField::Confirm),
        )
        .labelled(rk::STOLEN_CONFIRM_PLACEHOLDER),
        Element::gesture_button(
            ids::IDENTITY_STOLEN_BUTTON,
            rk::STOLEN,
            !state.busy && state.confirm == STOLEN_CONFIRM_WORD,
            Gesture::Launch(LaunchAction::SubmitStolen),
        ),
        Element::gesture_button(
            ids::RECOVERY_ENTRY_BACK_BUTTON,
            fauna_i18n::strings::common::BACK,
            !state.busy,
            Gesture::Launch(LaunchAction::BackToLocked),
        ),
    ]
}

/// Pane title for the entry step (chrome).
pub fn entry_title() -> String {
    rk::STOLEN.to_string()
}

/// The entry step's help text (chrome): the Settings section's own warning, so
/// the irreversible act is described by one sentence everywhere.
pub fn entry_description() -> Vec<String> {
    vec![rk::STOLEN_WARNING.to_string()]
}

/// `launch-account-locked-stolen-button`: open the entry step.
pub fn open_entry(app: &mut App) {
    if let LaunchSurface::AccountLocked { locked_until_secs } = app.launch {
        app.launch = LaunchSurface::IdentityStolenEntry { locked_until_secs };
        app.focus = 0;
    }
}

/// `recovery-entry-back-button`: back to wherever the machine now stands — the
/// notice while the lock holds, and straight on if it lapsed while the user
/// was on the step ([`follow_snapshot`] deliberately leaves the step alone).
pub fn back(app: &mut App, tx: &UnboundedSender<UiMessage>) {
    let LaunchSurface::IdentityStolenEntry { locked_until_secs } = app.launch else {
        return;
    };
    if app.locked.busy {
        return;
    }
    app.launch = LaunchSurface::AccountLocked { locked_until_secs };
    app.focus = 0;
    follow_snapshot(app, tx);
}

/// The launch machine changed state on its own. The locked surface follows the
/// snapshot back out (`devices.md` § The locked state — "the refresh is the
/// machine's own"): no app-side timer, and nothing here decides when a lock
/// ends.
///
/// Acts only while the **notice** is showing. Every other launch surface is
/// routed by the gesture that drove the machine; the entry step is left alone
/// because the user is mid-ceremony there, and its Back re-reads the machine.
pub fn follow_snapshot(app: &mut App, tx: &UnboundedSender<UiMessage>) {
    let LaunchSurface::AccountLocked { locked_until_secs } = app.launch else {
        return;
    };
    let Some(machine) = app.launch_machine.clone() else {
        return;
    };
    let snapshot = machine.snapshot();
    if still_parked(&snapshot, locked_until_secs) {
        return;
    }
    crate::launch::route(app, tx, snapshot);
    app.focus = 0;
}

/// Whether `snapshot` gives the notice nothing to do: the machine is still on
/// this lock, or it is mid-flight (its refresh is running and the settled
/// state arrives as a later change).
fn still_parked(snapshot: &LaunchSnapshot, shown: u64) -> bool {
    match snapshot.phase {
        LaunchPhase::Boot
        | LaunchPhase::Hydrating
        | LaunchPhase::SilentChallenge { .. }
        | LaunchPhase::Refreshing { .. } => true,
        _ => snapshot.locked_until_secs == Some(shown),
    }
}

/// What `identity-stolen-button` needs off the render thread: everything the
/// shared composition takes, captured while `App` is in hand.
pub struct StolenJob {
    nest_url: String,
    secret_hex: SecretString,
    phrase: SecretString,
    credentials: Arc<fauna_credential_store::CredentialStore>,
}

impl StolenJob {
    /// The ceremony — the shared composition, with no engine: a locked app has
    /// no session, so the group sweep is the successor's to finish from
    /// Settings.
    pub async fn run(self) -> StolenOutcome {
        let identity = match crate::session::session_actor_id(&self.secret_hex) {
            Ok(identity) => identity,
            // The ceremony never started — nothing moved.
            Err(e) => return StolenOutcome::not_landed(e),
        };
        let accounts = fauna_client_accounts::AccountRegistry::new(
            self.credentials as Arc<dyn fauna_client_accounts::SecretStore>,
        );
        fauna_client_recovery::ceremony::succeed_stolen_identity(
            &self.nest_url,
            &identity,
            self.phrase.as_str(),
            None,
            &accounts,
            crate::settings::successor_db_path,
        )
        .await
    }
}

/// Validate the entry step and claim the ceremony. `None` means it was refused
/// in words on `error-message` (or is already running) — re-checked here and
/// not only in the render, because an agent driving the id directly reaches
/// this with the button disabled (`testing.md` point 11).
pub fn begin_stolen(app: &mut App) -> Option<StolenJob> {
    if !matches!(app.launch, LaunchSurface::IdentityStolenEntry { .. }) || app.locked.busy {
        return None;
    }
    if app.locked.confirm != STOLEN_CONFIRM_WORD {
        app.locked.error = Some(rk::STOLEN_CONFIRM_PLACEHOLDER.to_string());
        return None;
    }
    if app.locked.phrase.as_str().trim().is_empty() {
        app.locked.error = Some(rk::KIT_PHRASE_REQUIRED.to_string());
        return None;
    }
    let Some((Some(nest_url), secret_hex, _handle)) = crate::session::stored_account(app) else {
        // Unreachable from the notice (the machine parked on a stored
        // identity's refusal); answered rather than dropped.
        app.locked.error = Some(fauna_i18n::strings::errors::recovery_identity_unreadable(
            "no stored account",
        ));
        return None;
    };
    app.locked.error = None;
    app.locked.busy = true;
    Some(StolenJob {
        nest_url,
        secret_hex,
        phrase: app.locked.phrase.clone(),
        credentials: Arc::clone(&app.credentials),
    })
}

/// Fold the ceremony's outcome: adopt the successor, or say what happened.
pub fn fold_stolen(app: &mut App, outcome: StolenOutcome) {
    app.locked.busy = false;
    match outcome {
        StolenOutcome::Landed(LandedSuccession {
            successor_secret_hex,
            new_actor_id: _,
            sweep,
            succeeded_at,
        }) => {
            let Some((Some(nest_url), old_secret_hex, _)) = crate::session::stored_account(app)
            else {
                // The store emptied under the ceremony. The account has moved,
                // so the seed goes on screen: it is the only way back in.
                app.locked.error = Some(rk::stolen_persist_failed(&successor_secret_hex));
                return;
            };
            let predecessor = crate::session::session_actor_id(&old_secret_hex)
                .map(|identity| identity.actor_id().to_hex())
                .ok();
            // Both secrets die with this fold: the pasted kit outranks the
            // seed, and the successor seed is in the registry.
            app.locked.phrase = SecretString::default();
            app.locked.confirm.clear();
            match crate::session::adopt_landed_succession(
                app,
                &nest_url,
                predecessor,
                sweep,
                &successor_secret_hex,
                succeeded_at,
            ) {
                // The switch relaunched as the successor; the launch flow owns
                // the screen from here.
                Ok(()) => app.locked.error = None,
                Err(msg) => {
                    // The succession LANDED even though the adoption did not,
                    // so this must never read as "nothing happened".
                    tracing::error!(
                        "[locked] adopting the successor identity after a landed succession: \
                         {msg}"
                    );
                    app.locked.error = Some(rk::stolen_persist_failed(&successor_secret_hex));
                }
            }
        }
        // Every other arm paints the shared sentence verbatim and wraps
        // nothing (`settings.md` § Recovery kit → *The ceremony's outcome is
        // headlined by its arm*).
        unlanded => {
            app.locked.error = Some(
                unlanded
                    .message()
                    .map(|m| crate::wizard::localized(&m))
                    .unwrap_or_default(),
            );
        }
    }
}

/// `identity-stolen-button`, awaited (the agent's click path).
pub async fn submit(app: &mut App) {
    if let Some(job) = begin_stolen(app) {
        let outcome = job.run().await;
        fold_stolen(app, outcome);
    }
}

/// `identity-stolen-button`, spawned (the keyboard path): the outcome rides
/// the UI channel back to [`fold_stolen`].
pub fn spawn_submit(app: &mut App, tx: &UnboundedSender<UiMessage>) {
    if let Some(job) = begin_stolen(app) {
        let tx = tx.clone();
        tokio::spawn(async move {
            let outcome = job.run().await;
            let _ = tx.send(UiMessage::Data(DataMessage::LockedCeremony(Box::new(
                outcome,
            ))));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNTIL: u64 = 1_800_000_000;

    fn locked_snapshot() -> LaunchSnapshot {
        LaunchSnapshot {
            phase: LaunchPhase::Offline { transient: false },
            last_error: Some(l::ACCOUNT_LOCKED.to_string()),
            locked_until_secs: Some(UNTIL),
            ..LaunchSnapshot::initial()
        }
    }

    fn ids(elements: &[Element]) -> Vec<String> {
        elements.iter().map(|e| e.id.clone()).collect()
    }

    /// `devices.md` § The locked state. The machine parks a lock on the same
    /// `Offline { transient: false }` an outdated nest gets, so only the
    /// snapshot's `locked_until_secs` tells the two apart: with it set the
    /// surface is the notice — unlock time, the sentence about a lock the
    /// owner did not set — and ONE action; without it the same snapshot is
    /// `NeedsUpdate`, so the arm order is load-bearing.
    ///
    /// Red-verify by moving the locked arm below the generic `Offline` arm in
    /// `launch::route`.
    #[test]
    fn a_locked_snapshot_paints_the_notice_with_one_action_never_needs_update() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();

        crate::launch::route(&mut app, &tx, locked_snapshot());

        assert!(
            matches!(
                app.launch,
                LaunchSurface::AccountLocked {
                    locked_until_secs: UNTIL
                }
            ),
            "its own surface — not NeedsUpdate, whose title and CTA misstate the problem: \
             got {:?}",
            app.launch
        );
        let elements = app.page_elements();
        assert_eq!(
            ids(&elements),
            vec![
                "launch-account-locked-notice",
                "launch-account-locked-stolen-button"
            ],
            "one action only — no retry (nothing clears a lock before its time) and no \
             fallthrough"
        );
        let notice = &elements[0].text;
        let time = fauna_core::format::format_unix_local(UNTIL as i64);
        assert!(notice.contains(&time), "the unlock time: {notice}");
        assert!(notice.contains(l::ACCOUNT_LOCKED), "{notice}");
        assert!(
            notice.contains(l::ACCOUNT_LOCKED_NOT_YOURS),
            "REQUIRED copy — a lock the owner did not set means somebody holds the key: {notice}"
        );
        // Its own element, not the generic `error-message`.
        assert_eq!(app.screen_error_text(), None);
        assert_eq!(app.screen_title(), l::ACCOUNT_LOCKED_TITLE);

        // The control: the same snapshot without the side channel is the
        // outdated-nest surface — the field is what routes.
        crate::launch::route(
            &mut app,
            &tx,
            LaunchSnapshot {
                locked_until_secs: None,
                ..locked_snapshot()
            },
        );
        assert!(matches!(app.launch, LaunchSurface::NeedsUpdate { .. }));
    }

    /// `ui/sessions.md` § Layout & flow: the button opens `identity_stolen_entry`,
    /// composed entirely of existing elements, and Back returns to the notice.
    #[test]
    fn the_stolen_button_opens_the_entry_step_and_back_returns_to_the_notice() {
        let mut app = crate::app::tests::test_app();
        let tx = app.tx.clone();
        crate::launch::route(&mut app, &tx, locked_snapshot());

        open_entry(&mut app);

        assert!(matches!(
            app.launch,
            LaunchSurface::IdentityStolenEntry {
                locked_until_secs: UNTIL
            }
        ));
        let elements = app.page_elements();
        assert_eq!(
            ids(&elements),
            vec![
                "recovery-entry-phrase-field",
                "identity-stolen-confirm-field",
                "identity-stolen-button",
                "recovery-entry-back-button",
            ],
            "existing elements only — no new id on this step (rule A)"
        );
        assert!(
            !elements[2].enabled,
            "the type-to-confirm gate starts unarmed"
        );
        let _ = app.set_field(
            Field::Locked(LockedField::Confirm),
            STOLEN_CONFIRM_WORD.to_string(),
        );
        assert!(app.page_elements()[2].enabled, "armed by the literal word");

        // No machine on this test app, so Back has nothing to re-read and
        // lands on the notice it came from.
        back(&mut app, &tx);
        assert!(matches!(
            app.launch,
            LaunchSurface::AccountLocked {
                locked_until_secs: UNTIL
            }
        ));
    }

    /// An agent driving `identity-stolen-button` directly reaches the action
    /// with the button disabled; both refusals answer in words on
    /// `error-message` and send nothing (`testing.md` point 11).
    #[test]
    fn the_ceremony_is_refused_in_words_before_anything_is_sent() {
        let mut app = crate::app::tests::test_app();
        app.launch = LaunchSurface::IdentityStolenEntry {
            locked_until_secs: UNTIL,
        };

        assert!(begin_stolen(&mut app).is_none());
        assert_eq!(
            app.screen_error_text().as_deref(),
            Some(rk::STOLEN_CONFIRM_PLACEHOLDER),
            "the wrong confirm word is refused out loud"
        );

        app.locked.confirm = STOLEN_CONFIRM_WORD.to_string();
        assert!(begin_stolen(&mut app).is_none());
        assert_eq!(
            app.screen_error_text().as_deref(),
            Some(rk::KIT_PHRASE_REQUIRED),
            "an empty phrase is refused out loud"
        );
        assert!(!app.locked.busy, "a refused press claims nothing");

        // Off the step entirely the action is inert — it cannot become a
        // second way to start a ceremony from another launch surface.
        app.launch = LaunchSurface::AccountLocked {
            locked_until_secs: UNTIL,
        };
        app.locked.phrase = "ab".repeat(32).into();
        assert!(begin_stolen(&mut app).is_none());
    }

    /// The notice holds while the machine is on this lock or mid-flight, and
    /// gives way to any settled snapshot that is not this lock.
    #[test]
    fn the_notice_holds_on_this_lock_and_yields_to_any_other_settled_state() {
        assert!(still_parked(&locked_snapshot(), UNTIL), "the same lock");
        for phase in [
            LaunchPhase::Boot,
            LaunchPhase::Hydrating,
            LaunchPhase::SilentChallenge { attempt: 2 },
        ] {
            assert!(
                still_parked(
                    &LaunchSnapshot {
                        phase,
                        locked_until_secs: None,
                        ..LaunchSnapshot::initial()
                    },
                    UNTIL
                ),
                "mid-flight: the settled state arrives as a later change"
            );
        }
        assert!(
            !still_parked(
                &LaunchSnapshot {
                    phase: LaunchPhase::Online,
                    ..LaunchSnapshot::initial()
                },
                UNTIL
            ),
            "the lock lapsed and the refresh signed in"
        );
        assert!(
            !still_parked(
                &LaunchSnapshot {
                    locked_until_secs: Some(UNTIL + 86_400),
                    ..locked_snapshot()
                },
                UNTIL
            ),
            "a different unlock time is a new lock — the notice repaints"
        );
        assert!(
            !still_parked(
                &LaunchSnapshot {
                    phase: LaunchPhase::Offline { transient: true },
                    locked_until_secs: None,
                    ..LaunchSnapshot::initial()
                },
                UNTIL
            ),
            "the refresh failed transiently — the ordinary retry surface"
        );
    }

    /// The surface follows the machine out with no timer of its own
    /// (`devices.md` § The locked state): a tick while the notice shows
    /// re-reads the machine and routes its settled snapshot. Here the machine
    /// settled on the wizard (an empty store), standing in for any non-locked
    /// outcome of the refresh.
    ///
    /// Red-verify by making `follow_snapshot` return early.
    #[tokio::test]
    async fn a_machine_tick_moves_the_notice_on_once_the_machine_left_the_lock() {
        let mut app = crate::app::tests::test_app();
        let machine = fauna_launch_machine::LaunchMachine::new(
            Arc::new(fauna_launch_machine::NullObserver),
            Arc::new(crate::session::tests::NoPersistence),
        );
        app.launch_machine = Some(Arc::clone(&machine));
        app.launch = LaunchSurface::AccountLocked {
            locked_until_secs: UNTIL,
        };

        // Still at `Boot` — mid-flight, so the tick moves nothing.
        app.handle_message(UiMessage::Data(DataMessage::LaunchMachineChanged));
        assert!(matches!(app.launch, LaunchSurface::AccountLocked { .. }));

        machine.start().await;
        app.handle_message(UiMessage::Data(DataMessage::LaunchMachineChanged));
        assert!(
            matches!(app.launch, LaunchSurface::Wizard),
            "the machine settled off the lock, so the notice must not stand: {:?}",
            app.launch
        );

        // And only the notice follows: on the entry step the user is
        // mid-ceremony, and a tick leaves them there.
        app.launch = LaunchSurface::IdentityStolenEntry {
            locked_until_secs: UNTIL,
        };
        app.handle_message(UiMessage::Data(DataMessage::LaunchMachineChanged));
        assert!(matches!(
            app.launch,
            LaunchSurface::IdentityStolenEntry { .. }
        ));
    }
}
