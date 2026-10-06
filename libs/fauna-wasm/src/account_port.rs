//! The core chunk's half of the **account port** — how a wasm chunk other
//! than this one reaches the tab's account runtime
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*).
//!
//! A page machine in another chunk holds a forwarder for its seam (the
//! Devices page's `fauna_devices_machine::port::PortFleetRemoval`), whose
//! every call arrives here through the SPA's `sharedAccountPort` as
//! [`account_port_call`]: the account the port was minted for, a door name,
//! and the call's arguments as canonical bytes. This module hands the door to
//! its seam's `serve` over **the object the native seats wire** (decision
//! (d)) — the fleet seam over `RuntimeFleetRemoval` and the ATProto
//! identity seam over `RuntimeAtprotoIdentity`, the folder-key custody seam
//! (`fauna_client_folders::port`) over `PlaneFolderKeys`, each of whose sources reads the
//! runtime fresh on every call and answers `None` when no runtime runs *or*
//! the running one serves another account ([`crate::account_runtime::handle_for`],
//! decision (e)); the adapter's own rule then refuses, so an absent or
//! foreign runtime deletes nothing on web exactly as on the six native apps.
//! The ATProto chunk's credential seam (`fauna_atproto_settings_machine::port`)
//! is served the same way over `RuntimeAtprotoCredentials`. The
//! custody-ceremony seam (`fauna_client_config::custody_port`), the
//! followed-folders seam (`fauna_client_config::follows_port`), the mail
//! seam (`fauna_client_config::mail_port`, the labeler catalog's grant mint)
//! and the succession-ledger seam
//! (`fauna_client_config::succession_ledger_port`, the grant log the labeler
//! catalog and the custody facet record through) are served by the handle's
//! own implementations, for the port's account only — the ledger seam's
//! through `fauna_client_config::ResolvingLedgerStore`, so a runtime still
//! starting is waited out exactly as the core chunk's own grant machines wait
//! for it. A door no seam knows is refused by name.
//!
//! The crossing set is a positive list (decision (h)): a seam is added here
//! by one arm in [`dispatch`], beside its `port` module in the crate that
//! declares it. The runtime's lifecycle, the pump controls and the ceremony
//! authority never cross.

use fauna_account_port::PortFault;
use fauna_account_seams::atproto_credentials::RuntimeAtprotoCredentials;
use fauna_account_seams::atproto_identity::RuntimeAtprotoIdentity;
use fauna_account_seams::fleet_removal::RuntimeFleetRemoval;
use fauna_account_seams::folder_keys::PlaneFolderKeys;
use wasm_bindgen::prelude::*;

use crate::account_runtime;

/// Answer one door for the account `actor_id_hex`.
pub(crate) async fn dispatch(
    actor_id_hex: String,
    door: &str,
    payload: &[u8],
) -> Result<Vec<u8>, PortFault> {
    // The custody-ceremony and followed-folders seams, served by the handle's
    // own implementations — but only for the account the port was minted for
    // (decision (e)).
    let custody = fauna_client_config::custody_port::doors::ALL.contains(&door);
    let follows = fauna_client_config::follows_port::doors::ALL.contains(&door);
    if custody || follows {
        let Some(handle) = account_runtime::handle_for(&actor_id_hex) else {
            return Err(PortFault::NoRuntime(format!(
                "no account runtime serves {actor_id_hex} in this tab"
            )));
        };
        let answered = if custody {
            fauna_client_config::custody_port::serve(&handle, door, payload).await
        } else {
            fauna_client_config::follows_port::serve(&handle, door, payload).await
        };
        if let Some(answered) = answered {
            return answered;
        }
    }
    // The mail seam likewise — the labeler catalog chunk's grant mint reads
    // the MSEK through it (decision (h): only the READ fold crosses).
    if fauna_client_config::mail_port::doors::ALL.contains(&door) {
        let Some(handle) = account_runtime::handle_for(&actor_id_hex) else {
            return Err(PortFault::NoRuntime(format!(
                "no account runtime serves {actor_id_hex} in this tab"
            )));
        };
        if let Some(answered) = fauna_client_config::mail_port::serve(&handle, door, payload).await
        {
            return answered;
        }
    }
    // The succession-ledger seam — the labeler catalog's and the folders
    // page's custody facet's grant events record through it (decision (h):
    // only the read and the join cross). Resolved per call for the port's
    // account and waited for while that runtime is still starting, like the
    // core chunk's own `ledger_seam`: a grant write fired in the start window
    // lands instead of failing, and a runtime that never comes for this
    // account answers the seam's own `LEDGER_NOT_READY` refusal (decision (f)).
    if fauna_client_config::succession_ledger_port::doors::ALL.contains(&door) {
        let ledger = {
            let actor_id_hex = actor_id_hex.clone();
            fauna_client_config::ResolvingLedgerStore::new(move || {
                account_runtime::handle_for(&actor_id_hex)
            })
        };
        if let Some(answered) =
            fauna_client_config::succession_ledger_port::serve(&ledger, door, payload).await
        {
            return answered;
        }
    }
    let fleet = {
        let actor_id_hex = actor_id_hex.clone();
        RuntimeFleetRemoval::new(move || account_runtime::handle_for(&actor_id_hex))
    };
    if let Some(answered) = fauna_devices_machine::port::serve(&fleet, door, payload).await {
        return answered;
    }
    // The folder-key custody seam — the folders and media chunks' content-key
    // reads and writes (`fauna.state.folder-keys`).
    let folder_keys = {
        let actor_id_hex = actor_id_hex.clone();
        PlaneFolderKeys::new(move || account_runtime::handle_for(&actor_id_hex))
    };
    if let Some(answered) = fauna_client_folders::port::serve(&folder_keys, door, payload).await {
        return answered;
    }
    // The ATProto identity seam — the ATProto settings chunk's custody reads
    // and joins (`fauna.state.atproto-identity`).
    let identity = {
        let actor_id_hex = actor_id_hex.clone();
        RuntimeAtprotoIdentity::new(move || account_runtime::handle_for(&actor_id_hex))
    };
    if let Some(answered) = fauna_client_atproto::port::serve(&identity, door, payload).await {
        return answered;
    }
    // The ATProto credential seam — the ATProto settings chunk's minted
    // app-credential secrets (`fauna.state.atproto`).
    let credentials =
        RuntimeAtprotoCredentials::new(move || account_runtime::handle_for(&actor_id_hex));
    if let Some(answered) =
        fauna_atproto_settings_machine::port::serve(&credentials, door, payload).await
    {
        return answered;
    }
    Err(PortFault::UnknownDoor(door.to_string()))
}

/// Run one door of a consumer seam for the account `actorIdHex` — the one
/// export `sharedAccountPort` (`$lib/account-runtime`) forwards every call
/// to. Resolves with the answer's canonical bytes (a seam method's refusal
/// included, which is data); rejects with the tagged object
/// `PortFault::to_js` writes when the crossing itself fails.
#[wasm_bindgen(js_name = accountPortCall, unchecked_return_type = "Promise<Uint8Array>")]
pub fn account_port_call(actor_id_hex: String, door: String, payload: Vec<u8>) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        match dispatch(actor_id_hex, &door, &payload).await {
            Ok(bytes) => Ok(js_sys::Uint8Array::from(&bytes[..]).into()),
            Err(fault) => Err(fault.to_js()),
        }
    })
}
