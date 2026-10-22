//! UniFFI façade for the `mail-settings` page's account controls
//! — the receive and send switches and the
//! automatic reply (`docs/goal/behavior/mail-policy-config.md` § Tier 3), and
//! the mailbox quota readout (`docs/goal/behavior/imap-server.md`
//! § Composition with submission quota). The draft check, the quota wording
//! and the "Delivered to" resolution are exported straight from
//! `fauna_client_mail_settings` and `fauna_mail::aliases::delivered`; this
//! file carries the RPCs. The alias extend rides `MailAliasesAction::Extend`
//! on the exported `MailAliasesMachine`, which fills the row's pattern,
//! controls and new expiry itself.
//!
//! Plain `fauna.bridges.*` RPCs over the caller's already-authenticated nest
//! connection, the [`crate::mail_forwarding`] shape.

use std::sync::Arc;

use fauna_client_bridges::MailAccountClient;
use fauna_client_email::AutoRespond;
use fauna_client_mail_settings::MailQuotas;
use fauna_protocol::bridge_routing::GetMailSwitchesReply;

use crate::email_client::FfiEmailClient;
use crate::nest_client::FfiNestClient;
use crate::{FfiError, general_err, stringify};

/// The caller's own receive, send and scan-result-row switches.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MailSwitches {
    pub receive_enabled: bool,
    pub send_enabled: bool,
    /// Whether received messages show the scan-result row (default on).
    pub scan_result_visible: bool,
}

impl From<GetMailSwitchesReply> for MailSwitches {
    fn from(r: GetMailSwitchesReply) -> Self {
        Self {
            receive_enabled: r.receive_enabled,
            send_enabled: r.send_enabled,
            scan_result_visible: r.scan_result_visible,
        }
    }
}

/// Read the caller's receive, send and scan-result-row switches (all on by
/// default).
#[fauna_uniffi_async::export]
pub async fn mail_switches_get(nest: Arc<FfiNestClient>) -> Result<MailSwitches, FfiError> {
    MailAccountClient::new(nest.nest_arc())
        .get_mail_switches()
        .await
        .map(Into::into)
        .map_err(general_err)
}

/// Set either switch (`None` keeps its stored value); the nest confirms the
/// write, then this re-reads so the UI shows the persisted values.
#[fauna_uniffi_async::export]
pub async fn mail_switches_set(
    nest: Arc<FfiNestClient>,
    receive_enabled: Option<bool>,
    send_enabled: Option<bool>,
) -> Result<MailSwitches, FfiError> {
    MailAccountClient::new(nest.nest_arc())
        .set_mail_switches_and_reload(receive_enabled, send_enabled)
        .await
        .map(Into::into)
        .map_err(general_err)
}

/// Set the scan-result-row switch; the nest confirms the write, then this
/// re-reads so the UI shows the persisted values.
#[fauna_uniffi_async::export]
pub async fn mail_scan_result_visible_set(
    nest: Arc<FfiNestClient>,
    scan_result_visible: bool,
) -> Result<MailSwitches, FfiError> {
    MailAccountClient::new(nest.nest_arc())
        .set_scan_result_visible_and_reload(scan_result_visible)
        .await
        .map(Into::into)
        .map_err(general_err)
}

/// Read the caller's mailbox quota and daily sending allowance — the input of
/// `mail_quota_display_of` and `mail_over_quota_notice_of`.
#[fauna_uniffi_async::export]
pub async fn mail_quotas_get(nest: Arc<FfiNestClient>) -> Result<MailQuotas, FfiError> {
    MailAccountClient::new(nest.nest_arc())
        .get_mail_quotas()
        .await
        .map(|reply| MailQuotas::from(&reply))
        .map_err(general_err)
}

/// What the automatic-reply toggle and message show.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiAutoRespond {
    pub enabled: bool,
    /// The reply body; empty when `enabled` is false.
    pub message: String,
}

impl From<AutoRespond> for FfiAutoRespond {
    fn from(a: AutoRespond) -> Self {
        Self {
            enabled: a.enabled,
            message: a.message,
        }
    }
}

// A second export block on the `fauna.email.*` handle, gated with this file so
// the Go mail-bridge build (no `mail-admin`) keeps its bindings unchanged.
#[fauna_uniffi_async::export]
impl FfiEmailClient {
    /// Read the automatic-reply setting from the caller's filter rules.
    pub async fn autorespond_get(&self) -> Result<FfiAutoRespond, FfiError> {
        self.client()
            .autorespond_get()
            .await
            .map(Into::into)
            .map_err(stringify)
    }

    /// Turn the automatic reply on with `message`, or change it. Check the
    /// draft with `autorespond_message_draft_outcome` first.
    pub async fn autorespond_set(&self, message: String) -> Result<(), FfiError> {
        self.client()
            .autorespond_set(&message)
            .await
            .map_err(stringify)
    }

    /// Turn the automatic reply off.
    pub async fn autorespond_clear(&self) -> Result<(), FfiError> {
        self.client().autorespond_clear().await.map_err(stringify)
    }
}
