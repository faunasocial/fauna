//! UniFFI façade for the user-global **sync preferences**
//! (`fauna.state.sync-prefs`) — today the default conflict policy for newly
//! created folders, the `sync-default-conflict-policy-select` control on the
//! Folders page's "Sync defaults" section (`docs/goal/behavior/file-sync.md`
//! § Conflicts, policy).
//!
//! Delegates to the shared
//! `fauna_sync_engine::preference_surfaces::{load,save}_sync_prefs` — the same
//! calls tui and linux make — which read and write the account store of this
//! process's runtime (`crate::account_runtime::handle_source()`), waiting for
//! it when a call arrives before the assembly has landed
//! (`config-dissolution.md` § The `__config` dissolution schedule → *The
//! closure order*, steps (1) and (5)). The preference rests sealed on the
//! account-state plane and the nest never receives or can decrypt it.
//!
//! Exported fns take/return only built-in types (`Vec<u8>` /
//! `Option<String>`), like the sibling façades.

use fauna_sync_engine::preference_surfaces;

use crate::{FfiError, general_err};

/// Read the owner's default conflict policy for new folders, as its canonical
/// wire string (`"auto"` | `"latest_wins_always"`); `None` = no preference (new
/// sets take the nest column default, `auto`). Renders the
/// `sync-default-conflict-policy-select` current value.
#[fauna_uniffi_async::export]
pub async fn load_sync_prefs() -> Result<Option<String>, FfiError> {
    preference_surfaces::load_sync_prefs(&crate::account_runtime::handle_source())
        .await
        .map_err(preference_surfaces::plane_failure)
        .map_err(general_err)
}

/// Set (or clear, with `None`) the owner's default conflict policy for new file
/// sets and persist. The input is normalized to a canonical wire string
/// (unknown → `"auto"`); the stored value is returned so the UI shows exactly
/// what was saved. Existing sets are untouched — each set's nest row (the
/// per-set `folder-conflict-policy-select`) stays authoritative.
#[fauna_uniffi_async::export]
pub async fn save_sync_prefs(policy: Option<String>) -> Result<Option<String>, FfiError> {
    preference_surfaces::save_sync_prefs(
        &crate::account_runtime::handle_source(),
        policy.as_deref(),
    )
    .await
    .map_err(preference_surfaces::plane_failure)
    .map_err(general_err)
}
