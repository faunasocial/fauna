//! UniFFI face of the **Sessions page** (`docs/goal/ui/sessions.md` § Where
//! logic lives — "UniFFI `libs/fauna-ffi/src/sessions.rs` and a wasm twin") —
//! the boundary the android, macOS, iOS and windows apps render the page from
//! and act through. tui and linux call the shared crate directly.
//!
//! **Nothing is decided here.** The act-then-refold pass is
//! `fauna_client_account::SessionsClient::pass`, the rows are
//! `sessions_view::fold`'s, the roster reduction is `sessions_view::roster_from`,
//! the lock's word gate is `sessions_view::lockout_word_refusal` over
//! `fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD`, and this machine's
//! row is the Devices face's own rule ([`crate::devices_this_device_row`]). What
//! this module adds is the crossing: one Record for the page, one export per
//! act, each returning the re-folded page beside the act's error (the custody
//! face's shape) — so an app holds no fold, no sort and no keep-the-painted-rows
//! rule of its own.
//!
//! The signed-out door's lock (leg 3) does not cross here: it is
//! `OnboardingMachine::submit_emergency_lockout`, already exported with the
//! onboarding machine.
//!
//! Gated behind the default-on `sessions` feature, dropped from the Go
//! mail-bridge `--no-default-features` build (no settings UI on a server),
//! which keeps the checked-in Go binding byte-identical.

use std::sync::Arc;

use fauna_client_account::sessions_view::{self, SessionRowView};
use fauna_client_account::{SessionsAct, SessionsClient, SessionsPass};
use fauna_core::localized::LocalizedText;
use fauna_devices_machine::DevicesMachine;

use crate::FfiNestClient;

/// One `session-card` (`ui/sessions.md` § State & data shape), plus its
/// `session-detail` line already composed — times through the shared
/// `format_unix_local`, the address or plainly that none was recorded — so no
/// app formats a session time of its own.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSessionRow {
    /// Row identity and the `sessions_revoke` argument.
    pub token_id: String,
    /// `session-kind`.
    pub kind: LocalizedText,
    /// `session-this-mark-badge` — "This app" / "This device", else absent.
    pub mark: Option<LocalizedText>,
    /// `session-detail`.
    pub detail: LocalizedText,
    pub created_at: u64,
    pub last_used_at: u64,
    pub expires_at: u64,
    pub ip_address: Option<String>,
    /// False exactly on this app's own row: `session-revoke-button` is absent.
    pub can_revoke: bool,
}

/// The whole page: the rows and the page-level `error-message`.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq)]
pub struct FfiSessionsSnapshot {
    pub rows: Vec<FfiSessionRow>,
    pub error: Option<LocalizedText>,
}

/// What a load or an act brings back: the page to paint, and whether the lock
/// landed — on `locked` the app leaves the shell for its launch surface, which
/// meets `fauna.auth.account_locked` and paints the locked notice (leg 2).
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq)]
pub struct FfiSessionsPage {
    pub snapshot: FfiSessionsSnapshot,
    pub locked: bool,
}

/// The lock's confirm word, `LOCK` — never localized. For the render's enable
/// check only: [`sessions_lockout`] re-checks what was typed.
#[uniffi::export]
pub fn sessions_lockout_confirm_word() -> String {
    fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD.to_string()
}

/// `sessions-revoke-others-button`'s answer — the FFI face of
/// [`sessions_view::revoke_others_possibility`]: still loading before the
/// first read (`snapshot` absent), the read's failure while it holds no rows,
/// `sessions.no_other_sessions` when only this app's own row is listed, and in
/// flight while an act is on its way (`busy`) (`ui/sessions.md` § Errors &
/// edge cases). The arm and the confirm both ask it again.
#[uniffi::export]
pub fn sessions_revoke_others_possibility(
    snapshot: Option<FfiSessionsSnapshot>,
    busy: bool,
) -> fauna_core::possibility::Possibility {
    let shared = snapshot.map(|s| sessions_view::SessionsSnapshot {
        rows: s.rows.into_iter().map(row_view).collect(),
        error: s.error,
    });
    sessions_view::revoke_others_possibility(shared.as_ref(), busy)
}

/// The nav-edge hydrate: read and fold. `devices` is the app's Devices machine
/// (it names device-minted sessions and owns the sealed-label render), absent
/// before it is built; `own_device_id` is the app's locally-stored device id,
/// the This-device rule's fallback. `previous` is the page the app painted
/// last: a failed read keeps its rows under the error.
#[fauna_uniffi_async::export]
pub async fn sessions_load(
    nest: Arc<FfiNestClient>,
    devices: Option<Arc<DevicesMachine>>,
    own_device_id: Option<String>,
    previous: Option<FfiSessionsSnapshot>,
) -> FfiSessionsPage {
    run(nest, devices, own_device_id, previous, None).await
}

/// `session-revoke-button[i]` — no confirm (recoverable by re-auth); revoke,
/// then re-fold.
#[fauna_uniffi_async::export]
pub async fn sessions_revoke(
    nest: Arc<FfiNestClient>,
    devices: Option<Arc<DevicesMachine>>,
    own_device_id: Option<String>,
    previous: Option<FfiSessionsSnapshot>,
    token_id: String,
) -> FfiSessionsPage {
    let act = Some(SessionsAct::Revoke(token_id));
    run(nest, devices, own_device_id, previous, act).await
}

/// `sessions-revoke-others-confirm-button` — the second press of the inline
/// confirm (the arming is the app's render state); end every session but this
/// app's, then re-fold.
#[fauna_uniffi_async::export]
pub async fn sessions_revoke_others(
    nest: Arc<FfiNestClient>,
    devices: Option<Arc<DevicesMachine>>,
    own_device_id: Option<String>,
    previous: Option<FfiSessionsSnapshot>,
) -> FfiSessionsPage {
    let act = Some(SessionsAct::RevokeOthers);
    run(nest, devices, own_device_id, previous, act).await
}

/// `sessions-lockout-button` — `confirm_word` is
/// `sessions-lockout-confirm-field` verbatim, checked HERE (the action arm),
/// not only in the render: a wrong word refuses loudly on `error-message` and
/// reaches no nest. On a landed lock the page comes back `locked`.
#[fauna_uniffi_async::export]
pub async fn sessions_lockout(
    nest: Arc<FfiNestClient>,
    devices: Option<Arc<DevicesMachine>>,
    own_device_id: Option<String>,
    previous: Option<FfiSessionsSnapshot>,
    confirm_word: String,
) -> FfiSessionsPage {
    if let Some(refusal) = sessions_view::lockout_word_refusal(
        &confirm_word,
        fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD,
    ) {
        return FfiSessionsPage {
            snapshot: FfiSessionsSnapshot {
                rows: previous.map(|p| p.rows).unwrap_or_default(),
                error: Some(refusal),
            },
            locked: false,
        };
    }
    run(
        nest,
        devices,
        own_device_id,
        previous,
        Some(SessionsAct::Lockout),
    )
    .await
}

async fn run(
    nest: Arc<FfiNestClient>,
    devices: Option<Arc<DevicesMachine>>,
    own_device_id: Option<String>,
    previous: Option<FfiSessionsSnapshot>,
    act: Option<SessionsAct>,
) -> FfiSessionsPage {
    let client = SessionsClient::new(nest.nest_arc(), nest.nest_arc());
    let pass = client
        .pass(act, roster(devices, own_device_id), || {
            fauna_launch_machine::launch_clock::now_secs_or_zero().max(0) as u64
        })
        .await;
    page(pass, previous)
}

/// The roster the fold names device-minted sessions from, and this machine's
/// principal — the Devices machine's rendered rows, and the Devices face's own
/// This-device rule.
async fn roster(
    devices: Option<Arc<DevicesMachine>>,
    own_device_id: Option<String>,
) -> sessions_view::SessionsRoster {
    let rows = match &devices {
        Some(machine) => {
            machine.refresh().await;
            machine.snapshot().devices
        }
        None => Vec::new(),
    };
    let this_row = crate::devices::devices_this_device_row(own_device_id).await;
    sessions_view::roster_from(
        rows.into_iter()
            .map(|d| (d.device_id, d.principal, d.label)),
        this_row.as_deref(),
    )
}

/// [`SessionsPass::onto`] across the boundary: the pass's rows, or the painted
/// ones when the re-read failed, under the pass's error.
fn page(pass: SessionsPass, previous: Option<FfiSessionsSnapshot>) -> FfiSessionsPage {
    let locked = pass.locked;
    let rows = match pass.rows {
        Some(rows) => rows.iter().map(row).collect(),
        None => previous.map(|p| p.rows).unwrap_or_default(),
    };
    FfiSessionsPage {
        snapshot: FfiSessionsSnapshot {
            rows,
            error: pass.error,
        },
        locked,
    }
}

fn row(r: &SessionRowView) -> FfiSessionRow {
    FfiSessionRow {
        token_id: r.token_id.clone(),
        kind: r.kind.clone(),
        mark: r.mark.clone(),
        detail: sessions_view::session_detail(
            r,
            |secs| fauna_core::format::format_unix_local(i64::try_from(secs).unwrap_or(i64::MAX)),
            fauna_i18n::strings::lookup,
        ),
        created_at: r.created_at,
        last_used_at: r.last_used_at,
        expires_at: r.expires_at,
        ip_address: r.ip_address.clone(),
        can_revoke: r.can_revoke,
    }
}

/// The fold's row back from its FFI mirror — the answer reads only the row
/// count and `can_revoke`, but every field crosses so the mirror cannot drift.
fn row_view(r: FfiSessionRow) -> SessionRowView {
    SessionRowView {
        token_id: r.token_id,
        kind: r.kind,
        mark: r.mark,
        created_at: r.created_at,
        last_used_at: r.last_used_at,
        expires_at: r.expires_at,
        ip_address: r.ip_address,
        can_revoke: r.can_revoke,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revoke_others_waits_for_a_read_with_another_sign_in() {
        let key = |p: fauna_core::possibility::Possibility| p.reason().map(|r| r.key.clone());
        assert_eq!(
            key(sessions_revoke_others_possibility(None, false)),
            Some("common.still_loading".into())
        );
        let mut own = row(&view("own"));
        own.can_revoke = false;
        let only_own = FfiSessionsSnapshot {
            rows: vec![own.clone()],
            error: None,
        };
        assert_eq!(
            key(sessions_revoke_others_possibility(Some(only_own), false)),
            Some(sessions_view::keys::NO_OTHER_SESSIONS.into())
        );
        let two = FfiSessionsSnapshot {
            rows: vec![own, row(&view("other"))],
            error: None,
        };
        assert!(sessions_revoke_others_possibility(Some(two.clone()), false).is_possible());
        assert_eq!(
            key(sessions_revoke_others_possibility(Some(two), true)),
            Some("common.in_flight".into())
        );
    }

    fn view(id: &str) -> SessionRowView {
        SessionRowView {
            token_id: id.into(),
            kind: LocalizedText::key(sessions_view::keys::KIND_APP),
            mark: None,
            created_at: 0,
            last_used_at: 0,
            expires_at: 0,
            ip_address: None,
            can_revoke: true,
        }
    }

    #[test]
    fn a_failed_reread_keeps_the_painted_rows() {
        let painted = page(
            SessionsPass {
                rows: Some(vec![view("kept")]),
                ..Default::default()
            },
            None,
        )
        .snapshot;
        let err = sessions_view::load_failed("offline");
        let next = page(
            SessionsPass {
                rows: None,
                error: Some(err.clone()),
                locked: false,
            },
            Some(painted),
        );
        assert_eq!(next.snapshot.rows[0].token_id, "kept");
        assert_eq!(next.snapshot.error, Some(err));
    }

    #[test]
    fn the_detail_line_says_the_address_was_not_recorded() {
        let r = row(&view("x"));
        assert_eq!(r.detail.key, sessions_view::keys::DETAIL);
        assert_eq!(
            r.detail.args["address"],
            fauna_i18n::strings::lookup(sessions_view::keys::ADDRESS_NOT_RECORDED).unwrap()
        );
    }

    #[test]
    fn the_confirm_word_is_the_shared_constant() {
        assert_eq!(sessions_lockout_confirm_word(), "LOCK");
    }
}
