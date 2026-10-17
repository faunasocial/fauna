//! UniFFI façade for the `mail-settings` page's Forwarding section
//! — the
//! `mail-settings-forward-all-to-input` and `mail-settings-forward-per-hour-input`
//! fields. The draft parsers and the hint are exported straight from
//! `fauna_client_mail_settings::forwarding`; this file carries the four RPCs.
//!
//! Plain `fauna.bridges.*` RPCs over the caller's already-authenticated nest
//! connection — User-class, caller-scoped — the `spam_threshold_override.rs`
//! shape: no account-store seal/unseal, so no owner secret/keypair is needed.

use std::sync::Arc;

use fauna_client_bridges::MailAccountClient;

use crate::nest_client::FfiNestClient;
use crate::{FfiError, general_err};

/// The caller's hourly forwarding limit and the admin ceiling it may not pass.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ForwardPerHour {
    pub value: u32,
    pub ceiling: u32,
}

/// Read the caller's per-account "forward all incoming mail to" address, or
/// `None` when forwarding is off.
#[fauna_uniffi_async::export]
pub async fn forward_all_to_get(nest: Arc<FfiNestClient>) -> Result<Option<String>, FfiError> {
    MailAccountClient::new(nest.nest_arc())
        .get_forward_all_to()
        .await
        .map_err(general_err)
}

/// Set (or clear, with `None`) the forward-all address; the nest confirms the
/// write, then this re-reads so the UI shows the persisted value, never the
/// keystrokes.
#[fauna_uniffi_async::export]
pub async fn forward_all_to_set(
    nest: Arc<FfiNestClient>,
    address: Option<String>,
) -> Result<Option<String>, FfiError> {
    MailAccountClient::new(nest.nest_arc())
        .set_forward_all_to_and_reload(address)
        .await
        .map_err(general_err)
}

/// Read the caller's hourly forwarding limit and its ceiling.
#[fauna_uniffi_async::export]
pub async fn forward_per_hour_get(nest: Arc<FfiNestClient>) -> Result<ForwardPerHour, FfiError> {
    let reply = MailAccountClient::new(nest.nest_arc())
        .get_forward_per_hour()
        .await
        .map_err(general_err)?;
    Ok(ForwardPerHour {
        value: reply.forward_per_hour,
        ceiling: reply.forward_per_hour_ceiling,
    })
}

/// Set the hourly forwarding limit; the nest confirms the write, then this
/// re-reads (value + ceiling).
#[fauna_uniffi_async::export]
pub async fn forward_per_hour_set(
    nest: Arc<FfiNestClient>,
    value: u32,
) -> Result<ForwardPerHour, FfiError> {
    let reply = MailAccountClient::new(nest.nest_arc())
        .set_forward_per_hour_and_reload(value)
        .await
        .map_err(general_err)?;
    Ok(ForwardPerHour {
        value: reply.forward_per_hour,
        ceiling: reply.forward_per_hour_ceiling,
    })
}
