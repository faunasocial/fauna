//! Box-recovery's tui glue over the account plane's deployment-seed custody
//! (`box-recovery.md` § The plane-era recovery floor). Every decision lives in
//! the shared native seam, `fauna_client_account_runtime::deployment_seeds`;
//! this module owns only tui's secret decoding, its store root, its spawns and
//! the message each answer rides back on.
//!
//! - **The reads** (*(b) The reads*) — [`load_recoverable_boxes`] and
//!   [`load_selfhosted_command`] run where no account runtime exists (the
//!   onboarding wizard's `nest_recovery` hub and self-hosted page, the
//!   launch-retry surface's recover entry). Each is ONE resolver call: this
//!   device's own account store joined with a cold read from the nest at the
//!   given URL when there is one — never either-or on whether a URL is stored,
//!   because a surviving device's saved nest is often the dead box itself.
//!   The projections (`recoverable_box_ids_in` /
//!   `selfhosted_recovery_command_in`) are the shared ones, which is what keeps
//!   the box list and the installer line byte-identical across the fleet.
//! - **The custody leg** (*(c) The writes*) — [`spawn_custody_leg`] is the
//!   only capture: run whenever the session holds both an authenticated
//!   connection and the account-store handle, at whichever of the post-auth
//!   edge (`session::establish` / `session::reconverge_post_auth`) and the
//!   store-ready edge (`app.rs`'s `AccountStoreReady` arm) lands second, and
//!   at every later post-auth edge. A run that leaves an admin's custody
//!   unconfirmed lands on `warning-message`.
//!
//! Best-effort throughout: a malformed secret, an unreachable nest or an absent
//! store all collapse to "nothing to show" (`recover-box-empty-message` / the
//! pending command placeholder). Recovery is reached *because* a box died — an
//! unreachable nest is the expected case here, never an error to shout about.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_core::identity::ActorKeypair;
use fauna_sync_engine::account_runtime::AccountStoreHandle;
use fauna_sync_engine::root::StoreRoot;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{DataMessage, UiMessage};

/// The identity secret behind `secret_hex`, or `None` (logged) when it is
/// malformed — the reads then answer "nothing to show".
fn secret_bytes(secret_hex: &str) -> Option<[u8; 32]> {
    match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(kp) => Some(*kp.secret_bytes()),
        Err(e) => {
            tracing::warn!("recovery: bad secret hex: {e}");
            None
        }
    }
}

/// The per-user account-store root the reads open this device's own store
/// under — the SAME root tui's account runtime assembles on
/// (`fauna_client_account_runtime::build_params` resolves
/// [`StoreRoot::platform`] for a desktop host), so the local read sees the
/// rows the runtime wrote.
fn store_root() -> StoreRoot {
    StoreRoot::platform()
}

/// The custodied boxes the `nest_recovery` hub (and the launch-retry recover
/// entry) lists: this device's own store joined with a cold read from the nest
/// at `nest_url` when one is given. Only public `nest_actor_id`s cross out; the
/// seed stays in Rust.
pub async fn load_recoverable_boxes(nest_url: Option<&str>, secret_hex: &str) -> Vec<String> {
    let Some(secret) = secret_bytes(secret_hex) else {
        return Vec::new();
    };
    fauna_client_account_runtime::deployment_seeds::recoverable_box_ids(
        nest_url.map(str::to_owned),
        secret,
        store_root(),
    )
    .await
}

/// The `recover-selfhosted-command` for the selected box — the installer `.env`
/// line carrying that box's custodied `FAUNA_DEPLOYMENT_SEED`, so the rebuilt box
/// re-presents the **same** `nest_actor_id` and every TOFU-pinned client
/// reconnects without a trust break. Same two sources as
/// [`load_recoverable_boxes`].
///
/// `None` when no source custodies that box — the page then shows the pending
/// placeholder rather than a command that would boot a box with the *wrong*
/// identity.
pub async fn load_selfhosted_command(
    nest_url: Option<&str>,
    secret_hex: &str,
    nest_actor_id_hex: &str,
) -> Option<String> {
    let secret = secret_bytes(secret_hex)?;
    fauna_client_account_runtime::deployment_seeds::selfhosted_command(
        nest_url.map(str::to_owned),
        secret,
        store_root(),
        nest_actor_id_hex,
    )
    .await
}

/// Spawn **the custody leg** over `nest` with the account-store handle
/// (`fauna_client_account_runtime::deployment_seeds::run_custody_leg`), and
/// bridge the warning it answers — custody unconfirmed for an admin — onto
/// `warning-message` ([`DataMessage::RecoveryCustodyWarning`] →
/// `app.injected_warning`). The steady state (the fold already holds this box)
/// makes no round trip. A dropped `tx` (UI gone) is ignored.
pub fn spawn_custody_leg(
    nest: Arc<NestClient>,
    store: AccountStoreHandle,
    tx: UnboundedSender<UiMessage>,
) {
    tokio::spawn(async move {
        if let Some(msg) =
            fauna_client_account_runtime::deployment_seeds::run_custody_leg(&nest, &store).await
        {
            let _ = tx.send(UiMessage::Data(DataMessage::RecoveryCustodyWarning(msg)));
        }
    });
}
