//! The Settings → Sessions sub-page (`ui/sessions.md`; nav id `sessions`) —
//! where this account is signed in right now, as one escalation ladder: the
//! list (revoke one), sign out everywhere else, the 24-hour lock.
//!
//! A paint shell over shared Rust, like every sub-page here: the rows, their
//! order, kinds and marks are `fauna_client_account::sessions_view::fold`'s,
//! the acts are `SessionsClient`'s, the confirm word is
//! `fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD`. What stays here is
//! only what `ui/sessions.md` § Where logic lives leaves to app glue — the
//! render and the two confirm states. No page machine: the page hydrates on the
//! nav edge and re-snapshots after each act (the Devices/Mail/Privacy shape),
//! and it keeps no cache beyond the last painted snapshot.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_account::sessions_view::{
    self, RosterDevice, SessionRowView, SessionsFoldInput, SessionsSnapshot,
};
use fauna_client_account::{SessionsClient, SessionsError};
use fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD;
use fauna_core::localized::LocalizedText;
use fauna_devices_machine::DevicesMachine;
use fauna_i18n::strings::sessions as t;
use fauna_ui_ids as ids;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture};

/// The page's state — the last painted snapshot plus the two confirm states.
#[derive(Default)]
pub struct SessionsState {
    /// `None` until the first nav-edge hydrate folds one.
    pub snapshot: Option<SessionsSnapshot>,
    /// Sign out everywhere else is armed: the confirm/cancel pair shows.
    pub revoke_others_armed: bool,
    /// `sessions-lockout-confirm-field`'s buffer.
    pub lockout_confirm_input: String,
    /// An act is in flight — the act buttons stand disabled until it folds.
    pub busy: bool,
}

impl SessionsState {
    /// A fresh visit: disarm and clear the typed word, keep nothing armed
    /// across a navigation (the `sign_out_pending` precedent).
    pub(super) fn reset_form(&mut self) {
        self.revoke_others_armed = false;
        self.lockout_confirm_input.clear();
        self.busy = false;
    }
}

/// One act on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionsAct {
    Revoke(String),
    RevokeOthers,
    Lockout,
}

/// Everything the op reads, captured off `SettingsState` so it can cross a
/// `tokio::spawn`.
pub struct SessionsOp {
    nest: Arc<NestClient>,
    devices: Option<Arc<DevicesMachine>>,
    store: Option<fauna_sync_engine::account_runtime::AccountStoreHandle>,
    local_device_id: Option<String>,
    act: Option<SessionsAct>,
}

/// What the op brings back.
#[derive(Debug)]
pub struct SessionsOutcome {
    /// The re-folded rows, or `None` when the re-read failed — the previously
    /// painted rows then stay (`ui/sessions.md` § Errors & edge cases).
    rows: Option<Vec<SessionRowView>>,
    error: Option<LocalizedText>,
    /// The lock landed: the app leaves the shell for its launch surface.
    pub locked: bool,
}

/// The op for a nav-edge hydrate (`act: None`) or an act; `None` pre-auth.
pub(super) fn op(state: &SettingsState, act: Option<SessionsAct>) -> Option<SessionsOp> {
    Some(SessionsOp {
        nest: state.nest.clone()?,
        devices: state.devices.machine.clone(),
        store: state.account_store.clone(),
        local_device_id: state.devices.local_device_id.clone(),
        act,
    })
}

/// Act (if asked), then re-read and fold. A lock that lands returns at once:
/// the nest has just revoked every token, this app's included, so there is
/// nothing left to list.
pub(super) async fn run(op: SessionsOp) -> SessionsOutcome {
    let client = SessionsClient::new(Arc::clone(&op.nest), Arc::clone(&op.nest));
    let mut error = None;
    match op.act {
        None => {}
        Some(SessionsAct::Revoke(token_id)) => {
            if let Err(e) = client.revoke(token_id).await {
                error = Some(sessions_view::act_failed(e));
            }
        }
        Some(SessionsAct::RevokeOthers) => match client.revoke_others().await {
            Ok(_) => {}
            Err(SessionsError::NoOwnSession) => {
                error = Some(LocalizedText::key(sessions_view::keys::NO_OWN_SESSION));
            }
            Err(SessionsError::Rpc(e)) => error = Some(sessions_view::act_failed(e)),
        },
        Some(SessionsAct::Lockout) => match client.lockout().await {
            Ok(_) => {
                return SessionsOutcome {
                    rows: None,
                    error: None,
                    locked: true,
                };
            }
            Err(e) => error = Some(sessions_view::act_failed(e)),
        },
    }

    // The roster names device-minted sessions — rendered by the shared Devices
    // machine, which owns the sealed-label render; this page never touches keys.
    let roster: Vec<(String, RosterDevice)> = match &op.devices {
        Some(machine) => {
            machine.refresh().await;
            machine
                .snapshot()
                .devices
                .into_iter()
                .filter_map(|d| {
                    let principal = d.principal?;
                    Some((
                        d.device_id,
                        RosterDevice {
                            principal,
                            name: d.label,
                        },
                    ))
                })
                .collect()
        }
        None => Vec::new(),
    };
    // This machine's enrolled principal — the same read door as the Devices
    // page's This-device marker (`devices.md` § This-device marker).
    let enrolled = match &op.store {
        Some(store) => store.enrolled_device_row().await.ok().flatten(),
        None => None,
    };
    let this_row =
        fauna_devices_machine::this_device_row(enrolled.as_deref(), op.local_device_id.as_deref());
    let this_principal = this_row.and_then(|row| {
        roster
            .iter()
            .find(|(device_id, _)| *device_id == row)
            .map(|(_, d)| d.principal.clone())
    });
    let roster: Vec<RosterDevice> = roster.into_iter().map(|(_, d)| d).collect();

    match client.list().await {
        Ok(reply) => {
            let own = client.own_token_ids().await;
            let now = fauna_launch_machine::launch_clock::now_secs_or_zero().max(0) as u64;
            let snapshot = sessions_view::fold(
                &SessionsFoldInput {
                    sessions: &reply.sessions,
                    own_token_ids: &own,
                    roster: &roster,
                    this_device_principal: this_principal.as_deref(),
                    now,
                },
                error,
            );
            SessionsOutcome {
                rows: Some(snapshot.rows),
                error: snapshot.error,
                locked: false,
            }
        }
        Err(e) => SessionsOutcome {
            rows: None,
            // An act's error outranks the re-read's: it is the one the user
            // just asked about (e2e convention 11 — never drop it).
            error: Some(error.unwrap_or_else(|| sessions_view::load_failed(e))),
            locked: false,
        },
    }
}

/// Fold an outcome onto the page. A failed re-read keeps the painted rows.
pub(super) fn apply(state: &mut SessionsState, outcome: SessionsOutcome) {
    state.busy = false;
    let rows = match outcome.rows {
        Some(rows) => rows,
        None => state.snapshot.take().map(|s| s.rows).unwrap_or_default(),
    };
    state.snapshot = Some(SessionsSnapshot {
        rows,
        error: outcome.error,
    });
}

/// This sub-page's error — read by `App::page_snapshot_error` (the `web` /
/// `encryption` shape: a page never pushes its own `error-message`).
pub(super) fn page_error(state: &SettingsState) -> Option<String> {
    state
        .sessions
        .snapshot
        .as_ref()
        .and_then(|s| s.error.as_ref())
        .map(|e| e.resolve(fauna_i18n::strings::lookup))
        .filter(|e| !e.is_empty())
}

fn format_time(secs: u64) -> String {
    fauna_core::format::format_unix_local(i64::try_from(secs).unwrap_or(i64::MAX))
}

pub(super) fn sessions_elements(state: &SettingsState) -> Vec<Element> {
    let s = &state.sessions;
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    ];

    // Region 1 — the list. Un-hydrated paints nothing rather than a claim.
    if let Some(snapshot) = &s.snapshot {
        if snapshot.rows.is_empty() {
            els.push(Element::label(ids::SESSIONS_EMPTY, t::EMPTY));
        }
        for (i, row) in snapshot.rows.iter().enumerate() {
            els.push(Element::label(ids::SESSION_CARD, String::new()));
            els.push(
                Element::label(
                    ids::SESSION_KIND,
                    row.kind.resolve(fauna_i18n::strings::lookup),
                )
                .within(ids::SESSION_CARD, i),
            );
            if let Some(mark) = &row.mark {
                els.push(
                    Element::label(
                        ids::SESSION_THIS_MARK_BADGE,
                        mark.resolve(fauna_i18n::strings::lookup),
                    )
                    .within(ids::SESSION_CARD, i),
                );
            }
            els.push(
                Element::label(
                    ids::SESSION_DETAIL,
                    sessions_view::session_detail(row, format_time, fauna_i18n::strings::lookup)
                        .resolve(fauna_i18n::strings::lookup),
                )
                .within(ids::SESSION_CARD, i),
            );
            // Absent on this app's own row: leaving this app is Sign out.
            if row.can_revoke {
                els.push(
                    Element::gesture_button(
                        ids::SESSION_REVOKE_BUTTON,
                        t::REVOKE,
                        !s.busy,
                        Gesture::Settings(Action::SessionsRevoke(row.token_id.clone())),
                    )
                    .within(ids::SESSION_CARD, i),
                );
            }
        }
    }

    // Region 2 — sign out everywhere else, the two-press inline confirm.
    els.push(Element::gesture_button(
        ids::SESSIONS_REVOKE_OTHERS_BUTTON,
        t::REVOKE_OTHERS,
        !s.busy && !s.revoke_others_armed,
        Gesture::Settings(Action::SessionsRevokeOthersArm),
    ));
    if s.revoke_others_armed {
        els.push(Element::gesture_button(
            ids::SESSIONS_REVOKE_OTHERS_CONFIRM_BUTTON,
            t::REVOKE_OTHERS_CONFIRM,
            !s.busy,
            Gesture::Settings(Action::SessionsRevokeOthersConfirm),
        ));
        els.push(Element::gesture_button(
            ids::SESSIONS_REVOKE_OTHERS_CANCEL_BUTTON,
            t::REVOKE_OTHERS_CANCEL,
            true,
            Gesture::Settings(Action::SessionsRevokeOthersCancel),
        ));
    }
    els.push(Element::label(ids::SESSIONS_REVOKE_NOTE, t::REVOKE_NOTE));

    // Region 3 — the lock. The warning is always visible, never behind the
    // confirm; the word gates the button here AND in the action arm.
    els.push(Element::label(
        ids::SESSIONS_LOCKOUT_WARNING,
        t::LOCKOUT_WARNING,
    ));
    els.push(
        Element::input(
            ids::SESSIONS_LOCKOUT_CONFIRM_FIELD,
            s.lockout_confirm_input.clone(),
            Field::Settings(SettingsField::SessionsLockoutConfirm),
        )
        .labelled(t::LOCKOUT_CONFIRM_PLACEHOLDER),
    );
    els.push(Element::gesture_button(
        ids::SESSIONS_LOCKOUT_BUTTON,
        t::LOCKOUT_BUTTON,
        !s.busy && s.lockout_confirm_input == LOCKOUT_CONFIRM_WORD,
        Gesture::Settings(Action::SessionsLockout),
    ));
    els
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;

    fn row(id: &str, own: bool) -> SessionRowView {
        SessionRowView {
            token_id: id.into(),
            kind: LocalizedText::key(sessions_view::keys::KIND_APP),
            mark: own.then(|| LocalizedText::key(sessions_view::keys::MARK_THIS_APP)),
            created_at: 1_700_000_000,
            last_used_at: 1_700_000_100,
            expires_at: 1_700_003_600,
            ip_address: None,
            can_revoke: !own,
        }
    }

    fn app_with(rows: Option<Vec<SessionRowView>>) -> crate::app::App {
        let mut app = crate::app::tests::authed_app();
        app.page = crate::pages::Page::Settings;
        app.settings.sub = SubPage::Sessions;
        app.settings.sessions.snapshot = rows.map(|rows| SessionsSnapshot { rows, error: None });
        app
    }

    fn ids_of(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn own_row_has_no_revoke_button_and_others_do() {
        let app = app_with(Some(vec![row("own", true), row("other", false)]));
        let els = sessions_elements(&app.settings);
        let revokes: Vec<_> = els
            .iter()
            .filter(|e| e.id == ids::SESSION_REVOKE_BUTTON)
            .collect();
        assert_eq!(revokes.len(), 1, "only the other row is revocable");
        let marks = els
            .iter()
            .filter(|e| e.id == ids::SESSION_THIS_MARK_BADGE)
            .count();
        assert_eq!(marks, 1);
        assert_eq!(els.iter().filter(|e| e.id == ids::SESSION_CARD).count(), 2);
    }

    #[test]
    fn required_copy_is_always_painted_and_the_pair_only_when_armed() {
        let mut app = app_with(None);
        let els = sessions_elements(&app.settings);
        let painted = ids_of(&els);
        for required in [
            ids::PAGE_HEADING,
            ids::SESSIONS_REVOKE_OTHERS_BUTTON,
            ids::SESSIONS_REVOKE_NOTE,
            ids::SESSIONS_LOCKOUT_WARNING,
            ids::SESSIONS_LOCKOUT_CONFIRM_FIELD,
            ids::SESSIONS_LOCKOUT_BUTTON,
        ] {
            assert!(painted.contains(&required), "{required} missing");
        }
        assert!(!painted.contains(&ids::SESSIONS_REVOKE_OTHERS_CONFIRM_BUTTON));
        assert!(
            !painted.contains(&ids::SESSIONS_EMPTY),
            "un-hydrated is not empty"
        );

        app.settings.sessions.revoke_others_armed = true;
        let els = sessions_elements(&app.settings);
        let painted = ids_of(&els);
        assert!(painted.contains(&ids::SESSIONS_REVOKE_OTHERS_CONFIRM_BUTTON));
        assert!(painted.contains(&ids::SESSIONS_REVOKE_OTHERS_CANCEL_BUTTON));
    }

    #[test]
    fn lock_button_arms_only_on_the_literal_word() {
        let mut app = app_with(Some(vec![row("own", true)]));
        let enabled = |app: &crate::app::App| {
            sessions_elements(&app.settings)
                .into_iter()
                .find(|e| e.id == ids::SESSIONS_LOCKOUT_BUTTON)
                .unwrap()
                .enabled
        };
        assert!(!enabled(&app));
        app.settings.sessions.lockout_confirm_input = "lock".into();
        assert!(!enabled(&app), "the word is never case-folded");
        app.settings.sessions.lockout_confirm_input = LOCKOUT_CONFIRM_WORD.into();
        assert!(enabled(&app));
    }

    #[test]
    fn a_failed_reread_keeps_the_painted_rows() {
        let mut state = SessionsState {
            snapshot: Some(SessionsSnapshot {
                rows: vec![row("own", true)],
                error: None,
            }),
            busy: true,
            ..Default::default()
        };
        apply(
            &mut state,
            SessionsOutcome {
                rows: None,
                error: Some(sessions_view::load_failed("offline")),
                locked: false,
            },
        );
        let snap = state.snapshot.unwrap();
        assert_eq!(snap.rows.len(), 1);
        assert!(snap.error.is_some());
        assert!(!state.busy);
    }
}
