//! The Settings → "Mail & Calendar" sub-page (`ui/mail-settings.md`).
//!
//! A paint shell over the shared `MailSettingsMachine`, per that doc's
//! § Architectural rules 3 ("One state machine drives every flow. Per-app UI
//! is dumb rendering of `MailSettingsSnapshot` + dispatch of
//! `MailSettingsAction`"). Every decision the page renders is a shared-Rust
//! helper resolved against the tui's string table — the status wording
//! (`settings_status_label`) above all. None of them is re-derived here; each
//! was the last un-lifted copy on some other app, and a seventh copy is exactly
//! what priority #2/#4 forbid.
//!
//! **The app-password rows are not here.** On tui they are rows of the
//! Connected apps roster (`connected_apps.rs`; `connected-apps.md`
//! § Layout & flow): this page keeps Add password and Rotate keys.
//!
//! **The error bridge is load-bearing.** `machine.dispatch(..)`'s `Result` is
//! deliberately ignored at the call site (linux does the same) because a
//! dispatch failure arrives on the *snapshot* (`MailSettingsSnapshot.error`),
//! not the return value. `apply_outcome` copies it onto `App::errors` — without
//! that fold a failed enable would paint no error and have no effect, which is
//! the dropped-command shape (`architecture/testing.md` point 10) in disguise.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_mail_settings::{
    CredentialKind, MailSettingsMachine, MailSettingsSnapshot, SettingsStatus,
    settings_status_label,
};
use fauna_core::secret::SecretString;
use fauna_i18n::strings::mail_settings as ms;
use fauna_i18n::strings::settings::mail as t;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture};

/// The Mail sub-page's state.
///
/// The machine is built once at the post-auth hook (`attach_session`) rather
/// than per gesture: `MailSettingsMachine::new` is sync and cheap (the RPC is
/// `hydrate()`), it carries interior mutability (`Mutex<Inner>`, so every method
/// takes `&self`), and holding it as an `Arc` is what lets an `Op` own only
/// `Arc`s and cross a `tokio::spawn` — the page-module contract's requirement.
/// It is session-scoped: `clear_session` drops it, so a stale machine can never
/// outlive a sign-out.
#[derive(Default)]
pub struct MailState {
    /// The shared machine. `None` pre-login.
    pub machine: Option<Arc<MailSettingsMachine>>,
    /// The last snapshot the page painted. `None` until the nav-edge hydrate
    /// folds one — the toggle then paints its honest pre-hydrate "off" (the
    /// shared `settings_status_label` gates `Idle` on `enabled`, so an
    /// un-hydrated page reads "Mail is disabled", never "All up to date").
    pub snapshot: Option<MailSettingsSnapshot>,
    /// Whether the `mail-add-credential` inline reveal is open. The toggle opens
    /// it; there is no separate dialog surface — `mail-settings.md` § User
    /// actions makes Enable "Toggle → add-credential dialog → submit", and the
    /// inline reveal is the shape web/linux/windows/android already render.
    pub show_add_form: bool,
    /// The `mail-add-credential-name-input` buffer — a local draft committed
    /// only on submit, like every other tui form buffer.
    pub name_input: String,
    /// Whether the destructive disable dialog is armed (`mail-settings.md`
    /// § User actions: "Flipping the toggle off keeps it rendering 'Mail
    /// enabled' — the dialog is the real decision point"), so the toggle's own
    /// `state` attr stays "on" until `DisableMail` actually lands.
    pub show_disable_confirm: bool,
    /// Whether the `mail-rotate-keys-confirm` inline reveal is open. The
    /// rotate-keys button opens it; confirm dispatches `StartRotation`, cancel
    /// closes it (`mail-settings.md` § User actions → "Rotate mail keys"). A
    /// page-local flag like the add-credential and disable reveals — the shared
    /// machine owns the rotation itself.
    pub show_rotate_form: bool,
    /// `Some(credentials to re-wrap)` while a confirmed rotation is still
    /// running — set on confirm, cleared by the rotation's own outcome
    /// (`Outcome::MailRotated`). While set, the rotate form stays open with its
    /// progress line painted and its controls disabled, so the multi-step
    /// rotation is visible while it runs and cannot be started twice
    /// (`mail-settings.md` § Element visibility). The count is taken at confirm,
    /// from the credentials the rotation re-wraps, because the page's snapshot
    /// only catches up when the dispatch returns. Not touched by
    /// [`Self::reset_form`]: navigating away does not stop the rotation.
    pub rotation_in_flight: Option<u64>,
    /// Credential ids excluded from the next rotation (the "this one is
    /// compromised, don't re-wrap it" case) — one `mail-rotate-keys-exclude-item`
    /// checkbox per credential toggles membership. Cleared whenever the form is
    /// (re)opened (`open_rotate_form`, the `open_add_form` precedent) so a stale
    /// exclusion from an earlier, cancelled visit can never silently carry into a
    /// later rotation.
    pub rotate_excluded: std::collections::HashSet<String>,
    /// The add-credential form's selected kind: `false` = OAUTHBEARER (the
    /// default, `mail-add-credential-type-selector` inactive), `true` = PLAIN
    /// (active). The type-selector toggles it; PLAIN reveals the password family
    /// (`mail-settings.md` § Add credential). The default matches the doc's
    /// "defaulting to OAUTHBEARER per `mail-credentials.md` § KDF choice".
    pub add_plain: bool,
    /// PLAIN-only: whether the bridge password is auto-generated (default **on**,
    /// `mail-settings.md` § Add credential — "The PLAIN form defaults
    /// auto-generate ON"). When on, [`Self::password_input`] holds a client-minted
    /// ~143-bit secret shown read-only for the user to copy; turning it off makes
    /// the field editable. `open_add_form` seeds it `true`.
    pub autogenerate: bool,
    /// The `mail-add-credential-password-input` value (PLAIN). When
    /// [`Self::autogenerate`] is on this is the client-minted secret, minted ONCE
    /// (at the PLAIN-select / autogen-on edge) and shown read-only — never
    /// re-minted at submit, so the value the user copies is exactly the value
    /// stored (the apple 2026-07-13 bug was re-minting a different one). When off
    /// it is the user's manual entry, served to the type path by
    /// `SettingsField::MailPassword`.
    pub password_input: String,
    /// PLAIN manual entry: whether the password field is unmasked
    /// (`mail-add-credential-password-show-toggle`; default masked, the unlock
    /// passphrase precedent). Paint-only — the raw buffer is always
    /// [`Self::password_input`].
    pub password_shown: bool,
    /// The OAUTHBEARER one-time token to reveal after a successful mint. `Some`
    /// switches the form into **token-reveal mode**: the input fields hide and
    /// `mail-add-credential-token-display` + `-token-copy-button` show, and the
    /// form stays open until the user closes it (`mail-settings.md`
    /// § Implementation status — closing on success destroyed the token at the
    /// instant it was minted; the apple 2026-07-13 twin bug). Held as
    /// [`SecretString`] so it stays zeroizing/redacted up to the renderer.
    pub shown_token: Option<SecretString>,
    /// Result of the most recent `enable_caldav_mailbox` **test command**, echoed
    /// to the driver as the top-level `caldav_mailbox_reply` state key — the tui
    /// twin of linux's `test_agent::SharedState::caldav_mailbox_reply`
    /// (`apps/fauna-linux/src/test_agent.rs:47`). `None` until a run completes;
    /// the handler clears it first so the driver's poll detects *this* run.
    /// Test-agent only: nothing in the product path reads or writes it.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    pub caldav_mailbox_reply: Option<Result<(), String>>,
    /// Result of the most recent `serve_enable_folder` **test command** — the
    /// number of served sets the reconciled `WebdavKeysBlob` carries — echoed as
    /// the top-level `webdav_serve_reply` state key, the tui twin of linux's
    /// `test_agent::SharedState::webdav_serve_reply`. Held here beside the
    /// CalDAV reply because it is the same kind of arrangement: it seals under
    /// the MSEK, so it needs mail enabled first. Same lifecycle: `None` until a
    /// run completes, cleared first, never touched by the product path.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    pub webdav_serve_reply: Option<Result<usize, String>>,
    /// The `mail-settings-forward-all-to-input` draft (`mail-forwarding.md`
    /// § Per-account "forward all"). Seeded only from a nest-confirmed read
    /// ([`Self::apply_account_read`]), edited by keystroke, committed on
    /// Enter/click (`Action::MailCommitForwardAllTo`) — the
    /// `mail-spam-threshold-override-input` shape, so a gesture elsewhere on
    /// the page can never clobber an uncommitted edit.
    pub forward_all_to_input: String,
    /// The `mail-settings-forward-per-hour-input` draft (`mail-forwarding.md`
    /// § Per-account forward rate-limit), same lifecycle as
    /// [`Self::forward_all_to_input`].
    pub forward_per_hour_input: String,
    /// The admin ceiling the limit may not pass, as the nest returned it beside
    /// the value — the field's bound, never hard-coded here. `None` until the
    /// first read resolves; the limit commit is inert until then.
    pub forward_per_hour_ceiling: Option<u32>,
    /// The receive / send / scan-result switches as the nest last confirmed
    /// them (`mail-settings-receive-toggle` / `-send-toggle` /
    /// `-scan-result-visible-toggle`; `mail-policy-config.md` § Tier 3). `None`
    /// until the first read resolves — every toggle is inert until then, so a
    /// click can never write a guessed value.
    pub switches: Option<fauna_protocol::bridge_routing::GetMailSwitchesReply>,
    /// The automatic reply as the filter rules last read it
    /// (`mail-settings-autorespond-toggle`; `email-filters.md` § The
    /// auto-respond setting). `None` until the first read; the toggle is inert
    /// until then.
    pub autorespond: Option<fauna_client_email::AutoRespond>,
    /// The `mail-settings-autorespond-message-input` draft, the
    /// [`Self::forward_all_to_input`] lifecycle: seeded only from a confirmed
    /// read, committed on Enter (`Action::MailCommitAutorespondMessage`).
    pub autorespond_message_input: String,
    /// The last `get_mail_quotas` read — the `mail-settings-mailbox-quota`
    /// readout and the `mail-settings-over-quota-notice`. `None` until it
    /// resolves (nothing painted).
    pub quotas: Option<fauna_protocol::bridge_routing::GetMailQuotasReply>,
}

/// One nest-confirmed read of the page's per-account mail settings — the
/// Forwarding pair, the receive / send switches, the automatic reply and the
/// quotas. Each part is `None` when the op did not touch it, so committing one
/// field never repaints another's uncommitted draft; each carries its own
/// failure, bridged onto `error-message` in the single fold.
#[derive(Debug)]
pub(crate) struct MailAccountRead {
    pub(super) forward_all_to: Option<Result<Option<String>, String>>,
    pub(super) forward_per_hour:
        Option<Result<fauna_protocol::bridge_routing::GetForwardPerHourReply, String>>,
    pub(super) switches:
        Option<Result<fauna_protocol::bridge_routing::GetMailSwitchesReply, String>>,
    pub(super) autorespond: Option<Result<fauna_client_email::AutoRespond, String>>,
    pub(super) quotas: Option<Result<fauna_protocol::bridge_routing::GetMailQuotasReply, String>>,
}

impl MailAccountRead {
    /// A read that touched nothing — the base each single-part op fills in.
    pub(super) fn none() -> Self {
        Self {
            forward_all_to: None,
            forward_per_hour: None,
            switches: None,
            autorespond: None,
            quotas: None,
        }
    }
}

impl MailAccountRead {
    /// The first failure any part carries, for the page's `error-message`.
    pub(super) fn error(&self) -> Option<String> {
        fn err<T>(part: &Option<Result<T, String>>) -> Option<String> {
            part.as_ref().and_then(|r| r.as_ref().err().cloned())
        }
        err(&self.forward_all_to)
            .or_else(|| err(&self.forward_per_hour))
            .or_else(|| err(&self.switches))
            .or_else(|| err(&self.autorespond))
            .or_else(|| err(&self.quotas))
    }
}

/// Read every per-account value the page shows — the page hydrate's second
/// half. Replay-safe pure reads over the shared `MailAccountClient` and the
/// `fauna-client-email` auto-respond facade.
pub(super) async fn read_account(nest: Arc<fauna_client::NestClient>) -> MailAccountRead {
    let email = fauna_client_email::EmailClient::new(nest.clone());
    let client = fauna_client_bridges::MailAccountClient::new(nest);
    MailAccountRead {
        forward_all_to: Some(client.get_forward_all_to().await.map_err(|e| e.to_string())),
        forward_per_hour: Some(
            client
                .get_forward_per_hour()
                .await
                .map_err(|e| e.to_string()),
        ),
        switches: Some(client.get_mail_switches().await.map_err(|e| e.to_string())),
        autorespond: Some(email.autorespond_get().await.map_err(|e| e.to_string())),
        quotas: Some(client.get_mail_quotas().await.map_err(|e| e.to_string())),
    }
}

/// Write one receive / send switch (`None` keeps the other's stored value),
/// then re-read both so the toggles reflect the **persisted** values.
pub(super) async fn set_mail_switches(
    nest: Arc<fauna_client::NestClient>,
    receive_enabled: Option<bool>,
    send_enabled: Option<bool>,
) -> MailAccountRead {
    MailAccountRead {
        switches: Some(
            fauna_client_bridges::MailAccountClient::new(nest)
                .set_mail_switches_and_reload(receive_enabled, send_enabled)
                .await
                .map_err(|e| e.to_string()),
        ),
        ..MailAccountRead::none()
    }
}

/// Write the scan-result switch (`mail-content-scanning.md` § Per-account
/// opt-out for visible scan details), then re-read every switch.
pub(super) async fn set_scan_result_visible(
    nest: Arc<fauna_client::NestClient>,
    visible: bool,
) -> MailAccountRead {
    MailAccountRead {
        switches: Some(
            fauna_client_bridges::MailAccountClient::new(nest)
                .set_scan_result_visible_and_reload(visible)
                .await
                .map_err(|e| e.to_string()),
        ),
        ..MailAccountRead::none()
    }
}

/// Turn the automatic reply on with `message` (or change it), or off when
/// `None`, through the shared facade; then re-read the setting.
pub(super) async fn set_autorespond(
    nest: Arc<fauna_client::NestClient>,
    message: Option<String>,
) -> MailAccountRead {
    let email = fauna_client_email::EmailClient::new(nest);
    let written = match &message {
        Some(message) => email.autorespond_set(message).await,
        None => email.autorespond_clear().await,
    };
    let autorespond = match written {
        Ok(()) => email.autorespond_get().await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    MailAccountRead {
        autorespond: Some(autorespond),
        ..MailAccountRead::none()
    }
}

/// Commit the forward-all address (`None` stops forwarding), then re-read so
/// the field reflects the **persisted** value.
pub(super) async fn set_forward_all_to(
    nest: Arc<fauna_client::NestClient>,
    value: Option<String>,
) -> MailAccountRead {
    MailAccountRead {
        forward_all_to: Some(
            fauna_client_bridges::MailAccountClient::new(nest)
                .set_forward_all_to_and_reload(value)
                .await
                .map_err(|e| e.to_string()),
        ),
        ..MailAccountRead::none()
    }
}

/// Commit the hourly limit, then re-read value + ceiling.
pub(super) async fn set_forward_per_hour(
    nest: Arc<fauna_client::NestClient>,
    value: u32,
) -> MailAccountRead {
    MailAccountRead {
        forward_per_hour: Some(
            fauna_client_bridges::MailAccountClient::new(nest)
                .set_forward_per_hour_and_reload(value)
                .await
                .map_err(|e| e.to_string()),
        ),
        ..MailAccountRead::none()
    }
}

impl MailState {
    /// Reset the sub-page-local form state. Called on every fresh nav to the
    /// page, so neither a half-typed credential name nor an armed destructive
    /// confirm survives a revisit (the `sign_out_pending` precedent).
    pub fn reset_form(&mut self) {
        self.show_add_form = false;
        self.name_input.clear();
        self.show_disable_confirm = false;
        self.show_rotate_form = false;
        self.rotate_excluded.clear();
        self.reset_add_form_fields();
    }

    /// Reset the add-credential form's kind/password/token state to its fresh
    /// defaults (OAUTHBEARER, auto-generate on, no password, masked, no revealed
    /// token). Shared by [`Self::reset_form`] and [`Self::open_add_form`] so a
    /// re-opened form is never seeded from a prior visit's PLAIN entry or a
    /// shown-once token still sitting on screen.
    fn reset_add_form_fields(&mut self) {
        self.add_plain = false;
        self.autogenerate = true;
        self.password_input.clear();
        self.password_shown = false;
        self.shown_token = None;
    }

    /// Fold a nest-confirmed per-account read. The only writer of the drafts,
    /// the ceiling, the switches, the automatic reply and the quotas — each part
    /// reseeds its field from the persisted value only when the op touched it
    /// and it succeeded; a failed part keeps what it had (its error already
    /// rides `error-message`).
    pub(super) fn apply_account_read(&mut self, read: MailAccountRead) {
        if let Some(Ok(address)) = read.forward_all_to {
            self.forward_all_to_input = address.unwrap_or_default();
        }
        if let Some(Ok(reply)) = read.forward_per_hour {
            self.forward_per_hour_input = reply.forward_per_hour.to_string();
            self.forward_per_hour_ceiling = Some(reply.forward_per_hour_ceiling);
        }
        if let Some(Ok(switches)) = read.switches {
            self.switches = Some(switches);
        }
        if let Some(Ok(autorespond)) = read.autorespond {
            // An off reply has no body; keep the user's draft rather than
            // blanking it, so turning the reply back on sends the last words.
            if autorespond.enabled {
                self.autorespond_message_input = autorespond.message.clone();
            }
            self.autorespond = Some(autorespond);
        }
        if let Some(Ok(quotas)) = read.quotas {
            self.quotas = Some(quotas);
        }
    }

    /// Open the add-credential inline reveal at its fresh defaults. Used by both
    /// the enable toggle (first credential) and the add-credential button
    /// (subsequent), so re-opening after an OAUTHBEARER token reveal or a
    /// cancelled PLAIN entry always starts clean.
    pub fn open_add_form(&mut self) {
        self.reset_add_form_fields();
        self.show_add_form = true;
        self.show_disable_confirm = false;
        self.show_rotate_form = false;
    }

    /// Open the rotate-keys confirm reveal, closing any other open reveal so only
    /// one destructive surface is visible at a time (the [`Self::open_add_form`]
    /// shape).
    pub fn open_rotate_form(&mut self) {
        self.show_rotate_form = true;
        self.show_add_form = false;
        self.show_disable_confirm = false;
        self.rotate_excluded.clear();
    }

    /// Mint (or re-mint) the auto-generated bridge password into
    /// [`Self::password_input`] when PLAIN + auto-generate are both on; clear it
    /// for manual entry. Called at every edge that can turn the pair on/off (the
    /// type-selector → PLAIN, the auto-generate toggle) so the shown secret is
    /// minted exactly ONCE per settled state and is the same value submit stores
    /// — the shared `resolve_autogenerated_password` decision (mail-credentials.md
    /// § Auto-generated bridge password) rather than a hand-rolled `if`.
    pub fn apply_autogen_state(&mut self) {
        let kind = if self.add_plain {
            CredentialKind::Plain
        } else {
            CredentialKind::OAuthBearer
        };
        let resolved = fauna_client_mail_settings::password_gen::resolve_autogenerated_password(
            kind,
            self.autogenerate,
        );
        if let Some(pw) = resolved {
            self.password_input = String::from(pw);
            self.password_shown = false;
        } else {
            // Manual entry (or OAUTHBEARER) — no generated secret sits in the
            // field; the user types their own.
            self.password_input.clear();
        }
    }
}

/// The Mail sub-page's ordered element list (`mail-settings.md` § Layout & flow).
///
/// Built: the always-visible top (heading + toggle + status), the pending-rotation
/// banner, both halves of the toggle (the add-credential inline reveal and the
/// destructive disable confirm), the rotate-keys inline confirm form
/// ([`rotate_keys_form`]), the credential-management controls (add, keys info,
/// rotate), the serve-here toggle, and the MUA-instructions block
/// ([`mua_elements`]). The app-password rows (`connected-apps-item*`) are not painted
/// here: on tui they render on Connected apps.
///
/// `settings-nav-back` returns to the Settings hub, the same rail-sub-page
/// pattern `account.rs`/`folders.rs`/etc. carry — a live user report found
/// Esc was the only way out on Folders, undiscoverable, and the same gap
/// applied here (user-approved 2026-08-03). Earlier reasoning tried to draw
/// an analogy to the feed's `post_detail` dialog's Esc-only dismissal, but
/// that surface is a transient overlay reached from the Feed page, not a
/// Settings rail destination like this one — the analogy doesn't hold.
///
/// The page's `error-message` is registered globally by
/// [`crate::ui::register_frame`] off `App::errors` (the `tui-settings` / `logs` /
/// `account` / `privacy` precedent), so it is not painted here.
pub(super) fn mail_elements(state: &SettingsState) -> Vec<Element> {
    let m = &state.mail;
    let snap = m.snapshot.as_ref();
    let enabled = snap.is_some_and(|s| s.enabled);
    // The status line reads the machine's LIVE status once the page has
    // hydrated, not the copy the last fold painted: the machine moves to
    // `Syncing` / `RotationInProgress` the moment a dispatch starts, and the
    // fold only lands when it returns — so the painted copy alone would never
    // say "syncing" (§ Status indicator). Pre-hydrate it stays `Idle`, which
    // the shared label reads as "Mail is disabled".
    let status = snap
        .and(m.machine.as_ref())
        .map(|machine| machine.snapshot().status)
        .or_else(|| snap.map(|s| s.status.clone()))
        .unwrap_or(SettingsStatus::Idle);

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, ms::TITLE),
        Element::chrome(t::SECTION_DESCRIPTION),
    ];

    // `mail-settings-enabled-toggle` — always visible (§ Layout & flow, "Top of
    // page (always visible)"). The `state` attr carries the toggle's OWN on/off
    // answer, which is what the e2e reads (`get_attr(id, "state")`): the derived
    // status text is not a proxy for it, because a `Syncing` snapshot of an
    // already-enabled mailbox still reports "syncing" while `enabled` is true.
    //
    // On a box that cannot enable mail (the shared machine's
    // `mail_unavailable_here`, read — never re-derived) the toggle reads
    // `unavailable` and is disabled, and the notice stands in for the enable
    // explanation (§ Layout & flow). A mailbox already on keeps its toggle, so
    // the user can still turn it off.
    let unavailable = !enabled && snap.is_some_and(|s| s.mail_unavailable_here);
    let state = match (enabled, unavailable) {
        (true, _) => "on",
        (false, true) => "unavailable",
        (false, false) => "off",
    };
    els.push(
        Element::checkbox_gesture(
            ids::MAIL_SETTINGS_ENABLED_TOGGLE,
            t::ENABLE_TITLE,
            enabled,
            Gesture::Settings(Action::ToggleMailEnabled),
        )
        .attr("state", state)
        .enabled(!unavailable),
    );
    if unavailable {
        els.push(Element::label(
            ids::MAIL_SETTINGS_UNAVAILABLE_NOTICE,
            fauna_i18n::strings::mail_settings::UNAVAILABLE_NOTICE,
        ));
    } else {
        els.push(Element::chrome(t::ENABLE_SUBTITLE));
    }

    // `mail-settings-status-indicator` — the shared label (§ Status indicator:
    // "The decision lives in shared Rust … one source of truth all apps
    // resolve through their own i18n runtime"). Never a local match arm.
    els.push(Element::label(
        ids::MAIL_SETTINGS_STATUS_INDICATOR,
        crate::wizard::localized(&settings_status_label(status, enabled)),
    ));

    // The pending-rotation banner — shown only when a prior rotation didn't finish
    // (§ Pending-rotation banner: "If `mail.pending_rotation` is set … the page
    // surfaces a banner … with a 'Resume' button"). Gated on
    // `pending_rotation.is_some()`, independent of the credential-management gate
    // below — a set sentinel already implies a provisioned mailbox.
    if snap.is_some_and(|s| s.pending_rotation.is_some()) {
        els.push(Element::label(
            ids::MAIL_SETTINGS_PENDING_ROTATION_BANNER,
            t::BANNER_TITLE,
        ));
        els.push(Element::chrome(t::BANNER_SUBTITLE));
        els.push(Element::gesture_button(
            ids::MAIL_SETTINGS_PENDING_ROTATION_RESUME_BUTTON,
            t::RESUME,
            true,
            Gesture::Settings(Action::MailResumeRotation),
        ));
    }

    // The destructive disable dialog — present only while open (§ Element IDs:
    // "`mail-settings-disable-confirm` + `-button`: present only while the
    // destructive disable dialog is open"). The confirm is CalDAV-aware in the
    // shared machine, not here: `DisableMail` preserves the shared MSEK when
    // CalDAV is on and does the full teardown when it is off.
    if m.show_disable_confirm {
        els.push(Element::label(
            ids::MAIL_SETTINGS_DISABLE_CONFIRM,
            t::DISABLE_TITLE,
        ));
        els.push(Element::chrome(t::DISABLE_WARNING));
        els.push(Element::gesture_button(
            ids::MAIL_SETTINGS_DISABLE_CONFIRM_BUTTON,
            t::DISABLE_CONFIRM,
            true,
            Gesture::Settings(Action::MailDisableConfirm),
        ));
    }

    // The add-credential inline reveal (ui.yaml page `mail-add-credential`).
    if m.show_add_form {
        els.extend(add_credential_form(m, enabled));
    }

    // The rotate-keys inline confirm reveal (ui.yaml page `mail-rotate-keys-confirm`),
    // opened by the rotate-keys button below — an inline reveal like the
    // add-credential form, not a separate route.
    if m.show_rotate_form {
        els.extend(rotate_keys_form(m, snap));
    }

    // The credential-management section. Gated on the shared
    // `credential_management_reachable` field — read, never re-derived
    // (§ Credential-management reachability: "every app reads the field
    // rather than re-deriving the disjunction, so a future DAV sibling widens it
    // in one place"). A CalDAV-only actor (email off) still manages credentials.
    if let Some(s) = snap.filter(|s| s.credential_management_reachable) {
        els.push(Element::chrome(t::CREDENTIALS_TITLE));
        // `mail-settings-add-credential-button` — the ONLY way to reach the
        // submit's `AddCredential` branch: the enabled toggle's job is the
        // disable decision, so without this button a second credential would be
        // unreachable and that branch would be dead code.
        els.push(Element::gesture_button(
            ids::MAIL_SETTINGS_ADD_CREDENTIAL_BUTTON,
            t::ADD_CREDENTIAL,
            true,
            Gesture::Settings(Action::MailOpenAddCredential),
        ));
        // `mail-settings-keys-info` — the (i) explainer (§ Keys explainer).
        els.push(Element::label(ids::MAIL_SETTINGS_KEYS_INFO, ms::KEYS_INFO));
        // `mail-settings-rotate-keys-button` — opens the rotate-keys confirm form.
        // Visible only with ≥1 credential (§ Element visibility, "rotate: ≥1
        // credential"): a mailbox with no credential has no MSEK to re-wrap.
        // Disabled while a rotation is pending or running (§ Architectural
        // rules 5: no second rotation racing the first) — the banner's Resume
        // is the way on; the shared machine refuses a second StartRotation too.
        if !s.credentials.is_empty() {
            els.push(Element::gesture_button(
                ids::MAIL_SETTINGS_ROTATE_KEYS_BUTTON,
                t::ROTATE_KEYS,
                s.pending_rotation.is_none() && m.rotation_in_flight.is_none(),
                Gesture::Settings(Action::MailOpenRotateForm),
            ));
        }

        // The app passwords themselves are rows of the Connected apps roster
        // (`mail-settings.md` § Credentials list): this page keeps Add password
        // and Rotate keys, and points at where each password is listed, copied,
        // revealed and disconnected.
        els.push(Element::chrome(if s.credentials.is_empty() {
            t::CREDENTIALS_EMPTY
        } else {
            ms::CREDENTIALS_ON_CONNECTED_APPS
        }));

        els.extend(mua_elements(s));

        // `mail-settings-serve-here-toggle` — user-set, default on (§ Local
        // IMAP/CalDAV-serving toggle). Rides `credential_management_reachable`
        // like the rest of this block (§ Credential-management reachability lists
        // it). Non-optimistic: the `state` attr flips only after the nest write
        // returns (the click path awaits the dispatch), which is what the e2e
        // asserts — the toggle reflects the nest, not a local guess. `state`
        // carries the toggle's own on/off, the uniform cross-app read idiom.
        els.push(
            Element::checkbox_gesture(
                ids::MAIL_SETTINGS_SERVE_HERE_TOGGLE,
                ms::SERVE_HERE_LABEL,
                s.serving_enabled,
                Gesture::Settings(Action::MailToggleServeHere),
            )
            .attr("state", if s.serving_enabled { "on" } else { "off" }),
        );
        els.push(Element::chrome(ms::SERVE_HERE_SUBTITLE));
    }

    // The Forwarding section (`mail-forwarding.md` § Per-account "forward all",
    // § Per-account forward rate-limit) — a mail feature, so shown while mail
    // is enabled. Both fields commit on Enter/click, the Spam page's threshold
    // shape: the draft is checked by the shared `forwarding` parsers first, and
    // a committed value repaints from the nest's re-read.
    if enabled {
        els.extend(account_switch_elements(m));
        els.extend(autorespond_elements(m));
        els.extend(quota_elements(m));
        els.extend(forwarding_elements(m));
    }

    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );

    els
}

/// The receive / send / scan-result switches (`mail-policy-config.md` § Tier
/// 3). Each is non-optimistic, the `mail-settings-serve-here-toggle` shape:
/// its `state` attr flips only once the nest's re-read lands. Absent until the
/// first read.
fn account_switch_elements(m: &MailState) -> Vec<Element> {
    let Some(switches) = m.switches.as_ref() else {
        return Vec::new();
    };
    let toggle = |id, label, on: bool, action| {
        Element::checkbox_gesture(id, label, on, Gesture::Settings(action))
            .attr("state", if on { "on" } else { "off" })
    };
    vec![
        toggle(
            ids::MAIL_SETTINGS_RECEIVE_TOGGLE,
            ms::RECEIVE_LABEL,
            switches.receive_enabled,
            Action::MailToggleReceive,
        ),
        Element::chrome(ms::RECEIVE_SUBTITLE),
        toggle(
            ids::MAIL_SETTINGS_SEND_TOGGLE,
            ms::SEND_LABEL,
            switches.send_enabled,
            Action::MailToggleSend,
        ),
        Element::chrome(ms::SEND_SUBTITLE),
        toggle(
            ids::MAIL_SETTINGS_SCAN_RESULT_VISIBLE_TOGGLE,
            ms::SCAN_RESULT_VISIBLE_LABEL,
            switches.scan_result_visible,
            Action::MailToggleScanResultVisible,
        ),
        Element::chrome(ms::SCAN_RESULT_VISIBLE_SUBTITLE),
    ]
}

/// The automatic reply: its toggle (absent until the first read) and the
/// message input, committed on Enter — which also turns the reply on.
fn autorespond_elements(m: &MailState) -> Vec<Element> {
    let mut els = Vec::new();
    if let Some(autorespond) = m.autorespond.as_ref() {
        els.push(
            Element::checkbox_gesture(
                ids::MAIL_SETTINGS_AUTORESPOND_TOGGLE,
                ms::AUTORESPOND_LABEL,
                autorespond.enabled,
                Gesture::Settings(Action::MailToggleAutorespond),
            )
            .attr("state", if autorespond.enabled { "on" } else { "off" }),
        );
        els.push(Element::chrome(ms::AUTORESPOND_SUBTITLE));
    }
    els.push(
        Element::input_commit(
            ids::MAIL_SETTINGS_AUTORESPOND_MESSAGE_INPUT,
            m.autorespond_message_input.clone(),
            Field::Settings(SettingsField::MailAutorespondMessage),
            Gesture::Settings(Action::MailCommitAutorespondMessage),
        )
        .labelled(ms::AUTORESPOND_MESSAGE_LABEL),
    );
    els
}

/// The mailbox quota readout and, only when a limit has run out, the notice —
/// both decided and worded by the shared `quota` helpers.
fn quota_elements(m: &MailState) -> Vec<Element> {
    let Some(quotas) = m.quotas.as_ref() else {
        return Vec::new();
    };
    let mut els = vec![Element::label(
        ids::MAIL_SETTINGS_MAILBOX_QUOTA,
        fauna_client_mail_settings::mail_quota_text(quotas, fauna_i18n::strings::lookup),
    )];
    els.push(Element::label(
        ids::MAIL_SETTINGS_SUBMISSION_QUOTA,
        fauna_client_mail_settings::mail_submission_quota_text(quotas, fauna_i18n::strings::lookup),
    ));
    if let Some(notice) = fauna_client_mail_settings::mail_over_quota_notice(quotas) {
        els.push(Element::label(
            ids::MAIL_SETTINGS_OVER_QUOTA_NOTICE,
            crate::wizard::localized(&notice),
        ));
    }
    els
}

/// The Forwarding section's elements: the forward-all address and the hourly
/// limit, each an `input_commit` with its explainer. The limit's explainer
/// names the ceiling once the first read has returned it.
fn forwarding_elements(m: &MailState) -> Vec<Element> {
    let mut els = vec![
        Element::chrome(ms::FORWARDING_TITLE),
        Element::input_commit(
            ids::MAIL_SETTINGS_FORWARD_ALL_TO_INPUT,
            m.forward_all_to_input.clone(),
            Field::Settings(SettingsField::MailForwardAllTo),
            Gesture::Settings(Action::MailCommitForwardAllTo),
        )
        .labelled(ms::FORWARD_ALL_TO_LABEL),
        Element::chrome(ms::FORWARD_ALL_TO_SUBTITLE),
        Element::input_commit(
            ids::MAIL_SETTINGS_FORWARD_PER_HOUR_INPUT,
            m.forward_per_hour_input.clone(),
            Field::Settings(SettingsField::MailForwardPerHour),
            Gesture::Settings(Action::MailCommitForwardPerHour),
        )
        .labelled(ms::FORWARD_PER_HOUR_LABEL),
    ];
    if let Some(ceiling) = m.forward_per_hour_ceiling {
        els.push(Element::chrome(crate::wizard::localized(
            &fauna_client_mail_settings::forward_per_hour_hint(ceiling),
        )));
    }
    els
}

/// Mask a password buffer for paint: same length, no content (the unlock
/// passphrase precedent). The raw buffer stays `MailState::password_input`; only
/// the painted/registered `text` is masked, so the type path still edits the real
/// value.
fn mask(value: &str) -> String {
    "•".repeat(value.chars().count())
}

/// The `mail-add-credential` inline reveal (ui.yaml page `mail-add-credential`),
/// rendered inside the mail-settings page rather than a separate route.
///
/// Two modes, mirroring linux (`apps/fauna-linux/src/settings/mail.rs`):
///
/// - **input mode** (`shown_token` is `None`): the name, the kind selector, and —
///   when PLAIN is selected — the auto-generate toggle + password field family;
///   plus submit + cancel.
/// - **token-reveal mode** (`shown_token` is `Some`): an OAUTHBEARER mint
///   succeeded, so the input fields give way to the one-time token display + copy,
///   and cancel becomes "Done". The form STAYS open here — closing it on the mint
///   would destroy the shown-once token at the instant it was minted
///   (`mail-settings.md` § Implementation status, the apple 2026-07-13 fix).
fn add_credential_form(m: &MailState, enabled: bool) -> Vec<Element> {
    // Token-reveal mode: an OAUTHBEARER mint landed; show only the token + copy +
    // Done, so the shown-once secret is reachable and nothing re-fires the submit.
    if let Some(token) = m.shown_token.as_ref() {
        return vec![
            // Un-ID'd warning line (no ui.yaml id for it), like the title.
            Element::chrome(t::TOKEN_WARNING),
            Element::label(
                ids::MAIL_ADD_CREDENTIAL_TOKEN_DISPLAY,
                token.as_str().to_owned(),
            ),
            Element::gesture_button(
                ids::MAIL_ADD_CREDENTIAL_TOKEN_COPY_BUTTON,
                t::COPY_TOKEN,
                true,
                Gesture::Settings(Action::MailAddCredentialCopyToken),
            ),
            // The cancel button relabels to "Done": the mint already succeeded, so
            // this only dismisses the token — same id, so the driver's
            // `close_add_credential_form` drives it uniformly across both modes.
            Element::gesture_button(
                ids::MAIL_ADD_CREDENTIAL_CANCEL_BUTTON,
                t::DONE,
                true,
                Gesture::Settings(Action::MailAddCredentialCancel),
            ),
        ];
    }

    // Input mode: name + kind selector, then the PLAIN family, then submit/cancel.
    let mut els = vec![
        Element::chrome(t::ADD_TITLE),
        Element::input(
            ids::MAIL_ADD_CREDENTIAL_NAME_INPUT,
            m.name_input.clone(),
            Field::Settings(SettingsField::MailCredentialName),
        )
        .labelled(t::NAME_PLACEHOLDER),
        // The kind selector — PLAIN (checked) | OAUTHBEARER (unchecked, the
        // default per `mail-credentials.md` § KDF choice). ui.yaml: "Selection is
        // exposed via attribute, not separate sub-IDs" — the `kind` attr carries
        // it; a single click toggles (the cross-app single-toggle contract the
        // driver drives: OAUTHBEARER → PLAIN).
        Element::checkbox_gesture(
            ids::MAIL_ADD_CREDENTIAL_TYPE_SELECTOR,
            t::TYPE_SELECTOR,
            m.add_plain,
            Gesture::Settings(Action::MailAddCredentialToggleKind),
        )
        .attr("kind", if m.add_plain { "plain" } else { "oauthbearer" }),
    ];

    // The PLAIN password family — shown only when PLAIN is selected.
    if m.add_plain {
        els.push(Element::checkbox_gesture(
            ids::MAIL_ADD_CREDENTIAL_AUTOGENERATE_TOGGLE,
            t::AUTOGENERATE,
            m.autogenerate,
            Gesture::Settings(Action::MailAddCredentialToggleAutogenerate),
        ));
        if m.autogenerate {
            // Auto-generate ON: the client-minted secret, shown read-only for the
            // user to copy. The registered `text` is the REAL password (never a
            // mask) — the driver reads it back via `get_text` and the row's later
            // reveal must match it, which is the apple secret-integrity proof. No
            // show-toggle / strength / weak-warning: the value is already shown and
            // always strong (ui.yaml: strength "Hidden while auto-generate is on").
            els.push(
                Element::label(
                    ids::MAIL_ADD_CREDENTIAL_PASSWORD_INPUT,
                    m.password_input.clone(),
                )
                .labelled(t::PASSWORD_PLACEHOLDER),
            );
        } else {
            // Manual entry: an editable, masked-by-default input + show-toggle.
            els.push(
                Element::input(
                    ids::MAIL_ADD_CREDENTIAL_PASSWORD_INPUT,
                    if m.password_shown {
                        m.password_input.clone()
                    } else {
                        mask(&m.password_input)
                    },
                    Field::Settings(SettingsField::MailPassword),
                )
                .labelled(t::PASSWORD_PLACEHOLDER),
            );
            els.push(Element::gesture_button(
                ids::MAIL_ADD_CREDENTIAL_PASSWORD_SHOW_TOGGLE,
                if m.password_shown { t::HIDE } else { t::SHOW },
                true,
                Gesture::Settings(Action::MailAddCredentialTogglePasswordShow),
            ));
            // Advisory strength readout (shared Rust; never gates submit), painted
            // only when there is something to rate — the shared fn returns `None`
            // for an empty password.
            if let Some(label) =
                fauna_client_mail_settings::password_gen::password_strength_label(&m.password_input)
            {
                els.push(Element::label(
                    ids::MAIL_ADD_CREDENTIAL_PASSWORD_STRENGTH_METER,
                    crate::wizard::localized(&label),
                ));
            }
            // The weak-password warning — shown whenever manual entry is active on
            // an encrypted nest, which every nest now is (no-modes), so the shared
            // `warn_manual_password(auto_generate=false, nest_encrypted=true)` is
            // unconditionally true here (`mail-settings.md` § Add credential).
            if fauna_client_mail_settings::password_gen::warn_manual_password(false, true) {
                els.push(Element::label(
                    ids::MAIL_ADD_CREDENTIAL_WEAK_PASSWORD_WARNING,
                    t::WEAK_PASSWORD_WARNING,
                ));
            }
        }
    }

    els.push(Element::gesture_button(
        ids::MAIL_ADD_CREDENTIAL_SUBMIT_BUTTON,
        if enabled {
            t::SUBMIT_ADD
        } else {
            t::SUBMIT_ENABLE
        },
        true,
        Gesture::Settings(Action::MailAddCredentialSubmit),
    ));
    els.push(Element::gesture_button(
        ids::MAIL_ADD_CREDENTIAL_CANCEL_BUTTON,
        t::CANCEL,
        true,
        Gesture::Settings(Action::MailAddCredentialCancel),
    ));
    els
}

/// The `mail-rotate-keys-confirm` inline reveal (ui.yaml page
/// `mail-rotate-keys-confirm`), rendered inside the mail-settings page like the
/// add-credential form. Confirm dispatches `StartRotation`; the shared machine
/// re-wraps the MSEK under every surviving credential (`mail-settings.md`
/// § User actions → "Rotate mail keys"; `mail-credentials.md` § Hard revoke).
///
/// The `mail-rotate-keys-exclude-list` container renders one
/// `mail-rotate-keys-exclude-item` checkbox per credential (flat indexed, same
/// one per credential in the mail machine's order — read by `[i]`, not a
/// `within()` scope): checking it toggles that credential's
/// membership in [`MailState::rotate_excluded`], which [`Action::MailRotateConfirm`]
/// reads at confirm time. A credential the user excludes here is dropped from
/// the `fauna.state.mail` credentials and never re-wrapped under the new MSEK — the
/// "this one is compromised" case (mail-credentials.md § Hard revoke).
fn rotate_keys_form(m: &MailState, snap: Option<&MailSettingsSnapshot>) -> Vec<Element> {
    // The progress line reflects a mid-flight rotation. It always registers (the
    // form's completeness), painting the shared rotation label only while
    // `RotationInProgress`, else empty — the click path awaits the whole rotation,
    // so by the time the page re-renders the status is usually back to `Idle`.
    //
    // While this page's own rotation runs, the page still holds the pre-rotation
    // snapshot (the machine's snapshot reaches the page only when the dispatch
    // returns), so the line is painted from what the rotation starts with — the
    // count `start_rotation` itself reports first.
    let progress = match (snap.map(|s| s.status.clone()), m.rotation_in_flight) {
        (Some(status @ SettingsStatus::RotationInProgress { .. }), _) => {
            crate::wizard::localized(&settings_status_label(status, true))
        }
        (_, Some(credentials_remaining)) => crate::wizard::localized(&settings_status_label(
            SettingsStatus::RotationInProgress {
                credentials_remaining,
            },
            true,
        )),
        _ => String::new(),
    };
    let mut els = vec![
        Element::chrome(t::ROTATE_TITLE),
        Element::label(ids::MAIL_ROTATE_KEYS_WARNING_TEXT, t::ROTATE_WARNING),
        // The exclude-list container marker (the indexed items below are its
        // children in registration order).
        Element::label(ids::MAIL_ROTATE_KEYS_EXCLUDE_LIST, String::new()),
    ];
    for c in snap.map(|s| s.credentials.as_slice()).unwrap_or_default() {
        els.push(Element::checkbox_gesture(
            ids::MAIL_ROTATE_KEYS_EXCLUDE_ITEM,
            c.display_name.clone(),
            m.rotate_excluded.contains(&c.credential_id),
            Gesture::Settings(Action::MailToggleRotateExclude(c.credential_id.clone())),
        ));
    }
    els.push(Element::label(
        ids::MAIL_ROTATE_KEYS_PROGRESS_INDICATOR,
        progress,
    ));
    els.push(Element::gesture_button(
        ids::MAIL_ROTATE_KEYS_CONFIRM_BUTTON,
        t::ROTATE_CONFIRM,
        m.rotation_in_flight.is_none(),
        Gesture::Settings(Action::MailRotateConfirm),
    ));
    els.push(Element::gesture_button(
        ids::MAIL_ROTATE_KEYS_CANCEL_BUTTON,
        t::CANCEL,
        m.rotation_in_flight.is_none(),
        Gesture::Settings(Action::MailRotateCancel),
    ));
    els
}

/// The `mail-settings-mua-instructions` block — the connection details the user
/// copies into Thunderbird / Apple Calendar / a file manager.
///
/// Read-only, and **per-protocol** (`mail-settings.md` § MUA setup instructions).
/// The one shared MSEK + `default` credential AUTHs IMAP + SMTP + CalDAV +
/// CardDAV + WebDAV, but a *row* describes one protocol, and the protocols
/// enable independently — so each row gates on its own protocol's state while
/// the block itself rides `credential_management_reachable` (its caller's gate).
/// Getting that split wrong is the failure this block is shaped to avoid: gating
/// the CalDAV row on `enabled` would hide the calendar server URL from exactly
/// the CalDAV-only actor who needs it, and gating the WebDAV row on the
/// deployment-wide `webdav_enabled` (which defaults ON for a real-domain box)
/// would print a dead mount URL for every actor on every box — the § WebDAV
/// files rationale, which is why the snapshot carries the per-actor
/// `serves_webdav_set` instead.
///
/// Every value is shared Rust (`MuaInstructions`, already resolved for the
/// local-target any-locator carve-out); nothing is assembled here. The labels
/// are paint-only — each element's registered `text` stays the bare value, the
/// cross-app `get_text` contract (`mua_webdav_url()` must read as a URL).
fn mua_elements(s: &MailSettingsSnapshot) -> Vec<Element> {
    let mua = &s.mua;
    let mut els = vec![
        Element::label(ids::MAIL_SETTINGS_MUA_INSTRUCTIONS, t::MUA_TITLE),
        Element::chrome(t::MUA_DESCRIPTION),
    ];

    // IMAP + SMTP describe the EMAIL protocol → gated on `enabled`.
    if s.enabled {
        els.push(
            Element::label(ids::MAIL_SETTINGS_MUA_IMAP_HOST, mua.imap_host.clone())
                .labelled(t::MUA_IMAP_HOST),
        );
        els.push(
            Element::label(ids::MAIL_SETTINGS_MUA_IMAP_PORT, mua.imap_port.to_string())
                .labelled(t::MUA_IMAP_PORT),
        );
        els.push(
            Element::label(ids::MAIL_SETTINGS_MUA_SMTP_HOST, mua.smtp_host.clone())
                .labelled(t::MUA_SMTP_HOST),
        );
        els.push(
            Element::label(ids::MAIL_SETTINGS_MUA_SMTP_PORT, mua.smtp_port.to_string())
                .labelled(t::MUA_SMTP_PORT),
        );
    }

    // CalDAV describes the CALENDAR protocol → gated on `caldav_enabled`,
    // independent of email being on.
    if s.caldav_enabled {
        els.push(
            Element::label(ids::MAIL_SETTINGS_MUA_CALDAV_HOST, mua.caldav_host.clone())
                .labelled(t::MUA_CALDAV_HOST),
        );
        els.push(
            Element::label(
                ids::MAIL_SETTINGS_MUA_CALDAV_PORT,
                mua.caldav_port.to_string(),
            )
            .labelled(t::MUA_CALDAV_PORT),
        );
    }

    // The WebDAV collection root describes the FILES protocol → gated on the
    // per-actor serve state. One full URL, not a host+port pair: no SRV
    // autodiscovery exists for WebDAV, so this line is the primary setup surface.
    if s.serves_webdav_set {
        els.push(
            Element::label(ids::MAIL_SETTINGS_MUA_WEBDAV_URL, mua.webdav_url.clone())
                .labelled(t::MUA_WEBDAV_URL),
        );
    }

    // Username + AUTH are shared by every protocol (one credential AUTHs them
    // all), so they show whenever the block does.
    //
    // `username_format` is deliberately painted RAW — it is the generic
    // *template* (`{handle}+{credential_id}@{domain}`) teaching the user the
    // address convention, which is why its placeholders survive to the screen.
    // It is NOT the concrete login: that is the per-password login on the
    // Connected apps roster (`connected-apps-item-username`), the only row
    // `resolve_mua_username` belongs on. Substituting here would silently
    // collapse the template into one credential's address and lose the rule the
    // row exists to teach (linux paints it raw for the same reason).
    els.push(
        Element::label(
            ids::MAIL_SETTINGS_MUA_USERNAME_FORMAT,
            mua.username_format.clone(),
        )
        .labelled(t::MUA_USERNAME),
    );
    els.push(
        Element::label(
            ids::MAIL_SETTINGS_MUA_AUTH_MECHANISM,
            mua.auth_mechanism.clone(),
        )
        .labelled(t::MUA_AUTH),
    );

    els
}
