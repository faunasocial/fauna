//! UniFFI door for the **whole** remove-account gesture — ask, snapshot and
//! remove, retire, erase — so the FFI seats make one call where they made
//! three (`account-scoping.md` § Erasure follows scope → *Removing an account
//! retires this machine from it, as a sign-out does*).
//!
//! Every step is shared code already: the question is [`crate::remove_account_blocked`]'s,
//! the registry step `AccountRegistry::remove_with_snapshot`, the nest half
//! `fauna_client_account_runtime::retire_removed_account`, and the erase
//! [`crate::account_state_erase_scope`]'s pair. What this door adds is the
//! ORDER, which is the part a shell could get wrong: the question first,
//! because every later step destroys something a refusal cannot give back; the
//! registry removal as the one decision point, landing before the first await;
//! the nest half over the slots that removal took into memory; and the erase
//! only after the nest half has closed the store it assembled over.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cloud_backup::FfiStoreContainer;
use crate::erase_guard::FfiEraseRemoveBlocked;
use crate::{FfiAccountInstanceLock, FfiAccountRegistry, FfiError};

/// Remove `actor_id_hex` from this install — the `account-remove-button`
/// gesture, whole.
///
/// * `Ok(Some(blocked))` — refused before anything changed: paint the case's
///   line on the Settings page's `error-message` (the arguments that decide it
///   are [`crate::remove_account_blocked`]'s, with `base_dirs[0]` as its
///   `base_dir`).
/// * `Err` — the registry refused the removal; nothing else ran.
/// * `Ok(None)` — removed: the entry and its slots are gone, this machine's
///   enrollment on the account's nest was retired within
///   `REMOVED_ACCOUNT_RETIRE_BUDGET` (or deferred — best-effort, logged by
///   the shared routine, never a reason to stop), and the account's scopes
///   are erased under every base.
///
/// `base_dirs` are the app's own state bases the erase sweeps — the first is
/// the install base a sibling instance of this app declares itself in; android
/// passes its two roots. `store_container` is exactly what the seat passes to
/// `FfiNestClient::start_account_runtime` (`None` on desktops): its `dir` is
/// the shared store root the erase and the question resolve, and the whole of
/// it is what the nest half assembles under. `own_lock` and `serving_here`
/// are [`crate::remove_account_blocked`]'s.
#[fauna_uniffi_async::export]
pub async fn remove_account_gesture(
    registry: Arc<FfiAccountRegistry>,
    actor_id_hex: String,
    base_dirs: Vec<String>,
    store_container: Option<FfiStoreContainer>,
    own_lock: Option<Arc<FfiAccountInstanceLock>>,
    serving_here: Option<String>,
) -> Result<Option<FfiEraseRemoveBlocked>, FfiError> {
    let container_dir = store_container.as_ref().map(|c| c.dir.clone());
    let ask_base = base_dirs.first().cloned().unwrap_or_default();
    if let Some(blocked) = crate::remove_account_blocked(
        ask_base,
        actor_id_hex.clone(),
        container_dir.clone(),
        own_lock,
        serving_here,
    ) {
        tracing::warn!("[settings/account] remove-account REFUSED ({blocked:?}) — {actor_id_hex}");
        return Ok(Some(blocked));
    }
    let removed = registry
        .registry()
        .remove_with_snapshot(&actor_id_hex)
        .map_err(|e| {
            tracing::error!("[settings/account] remove account failed: {e:#}");
            crate::general_err(e)
        })?;
    let removed = Arc::new(removed);
    if removed.holds_device_authorization() {
        let own_device_id = removed.device_id();
        // The outcome's log line is the shared routine's own.
        let _ = fauna_client_account_runtime::retire_removed_account(
            Arc::clone(&removed),
            fauna_client_account_runtime::RemovedAccountInputs {
                // The FFI seats keep the id in the registry's per-account
                // slot, which the removal took with the rest.
                own_device_id_hex: Box::new(move || own_device_id),
                store_container: store_container.map(|c| {
                    fauna_client_account_runtime::SandboxedStoreContainer {
                        dir: PathBuf::from(c.dir),
                        exclusion: c.exclusion.into(),
                    }
                }),
            },
        )
        .await;
    }
    let store_root = crate::account_state::store_root_for(container_dir.map(PathBuf::from));
    for base in &base_dirs {
        if let Err(e) = crate::account_state::erase_scope_under(
            Path::new(base),
            store_root.as_deref(),
            &actor_id_hex,
        ) {
            tracing::warn!("[settings/account] remove-account erase under {base} failed: {e}");
        }
    }
    Ok(None)
}
