//! UniFFI façade for the Nostr succession-aftermath **npub confirm banner**
//! — every non-tui/non-linux native app (android,
//! windows, macos, ios) reaches
//! [`fauna_client_config::npub_confirmation_owed_for`] and the account-store
//! handle's `fauna.state.nostr-confirmation` doors through this one module.
//!
//! The stamp lives on the account plane (`fauna.state.nostr-confirmation`,
//! `config-dissolution.md` § The `__config` dissolution schedule) and is read
//! and written through the seat's account-store handle
//! (`crate::account_runtime::handle()`) — `None` before the assembly lands,
//! which the read degrades to "not owed" and the write refuses. Gated behind
//! its own `nostr-npub-confirm` feature (implies `account-runtime`),
//! default-on and in `store-safe`: the Go mail-bridge
//! `--no-default-features` build has no Nostr settings UI, so it drops this
//! module and keeps the checked-in Go bindings byte-identical.

use std::sync::Arc;

use fauna_client_config::npub_confirmation_owed_for;

use crate::nest_client::FfiNestClient;
use crate::{FfiError, general_err};

/// Is the caller owed an npub confirmation right now — the Nostr page's
/// nav-enter read (tui reference: `apps/fauna-tui/src/nostr.rs`'s
/// `refresh_and_check_npub`). Best-effort like the shared function itself:
/// any unhappy answer degrades to `false` rather than an error, so a bad read
/// never turns into a page error — it is just re-asked on the next load.
///
/// `owner_secret` is no longer read (the stamp is on the account plane) and stays only so the exported signature every
/// app calls is unchanged.
#[fauna_uniffi_async::export]
pub async fn npub_confirmation_owed(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<bool, FfiError> {
    let _ = owner_secret;
    let account = crate::account_runtime::handle();
    Ok(npub_confirmation_owed_for(
        nest.nest_arc().as_ref(),
        account.as_ref().map(|a| a.npub_confirmed_at()),
    )
    .await)
}

/// Record the owner's "yes, that's my npub" confirmation — also the
/// best-effort call a fresh (re-)link makes on its own, so the banner never
/// resurrects itself right after a deliberate re-link (tui's `Op::Link`,
/// linux's link-success arm are the reference).
///
/// `now` is the caller's own clock (epoch seconds) — passed in rather than
/// read here, same as the shared function. Refused while the seat has no
/// account runtime; `nest` and `owner_secret` stay only so the exported
/// signature is unchanged.
#[fauna_uniffi_async::export]
pub async fn confirm_npub(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    now: i64,
) -> Result<(), FfiError> {
    let _ = (nest, owner_secret);
    let account = crate::account_runtime::handle()
        .ok_or_else(|| general_err("npub confirm: the account store is not ready yet"))?;
    account
        .confirm_nostr_npub(now)
        .await
        .map(|_| ())
        .map_err(|e| general_err(format!("{e:#}")))
}
