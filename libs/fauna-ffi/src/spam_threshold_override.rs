//! UniFFI façade for the per-account spam-folder threshold override
//! — the `mail-spam` page's
//! `mail-spam-threshold-override-input`.
//!
//! Thinner than the sibling settings façades (`muted_keywords.rs`,
//! `sync_prefs.rs`): `MailAccountClient::{get,set}_spam_threshold_override`
//! are plain `fauna.bridges.*` RPCs over the caller's already-authenticated
//! nest connection — User-class, caller-scoped, no account-store seal/unseal, so
//! no owner secret/keypair is needed here.

use std::sync::Arc;

use fauna_client_bridges::MailAccountClient;

use crate::nest_client::FfiNestClient;
use crate::{FfiError, general_err};

/// Read the caller's per-account spam-folder threshold override, in whole
/// points, or `None` when the account follows the admin default.
#[fauna_uniffi_async::export]
pub async fn spam_threshold_override_get(
    nest: Arc<FfiNestClient>,
) -> Result<Option<u32>, FfiError> {
    MailAccountClient::new(nest.nest_arc())
        .get_spam_threshold_override()
        .await
        .map_err(general_err)
}

/// Set (or clear, with `None`) the caller's per-account spam-folder threshold
/// override; the nest confirms the write, then this re-reads so the UI
/// reflects the persisted value, never the local edit. `Some(0)` is a real
/// setting — it turns automatic Junk filing off for this account, distinct
/// from `None` (follow the admin default).
#[fauna_uniffi_async::export]
pub async fn spam_threshold_override_set(
    nest: Arc<FfiNestClient>,
    value: Option<u32>,
) -> Result<Option<u32>, FfiError> {
    MailAccountClient::new(nest.nest_arc())
        .set_spam_threshold_override_and_reload(value)
        .await
        .map_err(general_err)
}
