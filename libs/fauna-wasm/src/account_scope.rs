//! Web's **account-scope erase** — the wasm twin of the native
//! `account_scope` (`apps/fauna-linux/src/account_scope.rs`, tui's): what a
//! sign-out and a remove-account erase for an account beyond the registry's own
//! `fauna/{actor}/…` slots (`apps/account-scoping.md` § The scoping taxonomy →
//! *Erasure follows scope*, the paragraph "Web's account store is in the erase
//! too").
//!
//! One per-actor erase, [`erase_actor_scope`], with one arm today: the
//! account store — the IndexedDB database and the OPFS segment directory named
//! `StoreRoot::store_name(actor)`. The actor-keyed `localStorage` state web's
//! wasm modules mint beside the registry namespace joins it as a second
//! arm.
//!
//! **Every erase runs behind the sign-out record**
//! (`fauna_client_accounts::SignOutRecord`): the gesture names the accounts it
//! reaches before its first await, and [`finish_recorded`] is the one routine
//! that does what a record owes — the credential wipe if it has not run, then
//! each recorded account's erase, each account leaving the record as its store
//! goes. The sign-out's own tail and the next page load call the same routine,
//! so a tab closed mid-sign-out is finished by whichever tab loads next. The
//! load retires no enrollment: it has no session to do it over.
//!
//! **The store half is a residue class** (the same paragraph, decision 4). A
//! database delete waits behind a connection somebody else holds and an OPFS
//! removal can be refused, so the erase is bounded ([`ERASE_BUDGET`]) and an
//! account whose store is still here stays in the record. [`finish_recorded`]
//! answers with how many are left, as the shared
//! `fauna_client_accounts::EraseResidueView` line the onboarding page's
//! `sign-out-residue` view paints, and the next load sweeps again. A sweep at a
//! load or at the view's Remove Again is asked the sign-out's own question
//! first — does another tab serve one of these accounts — and erases nothing
//! when one does; the probe is the caller's (Web Locks, `$lib/webLocks`), its
//! answer the `erase_refused` argument.

#![cfg(target_arch = "wasm32")]

use std::sync::Arc;
use std::time::Duration;

use fauna_account_store::indexeddb::IndexedDbBackend;
use fauna_account_store::root::StoreRoot;
use fauna_client_accounts::{
    AccountRegistry, EraseResidueView, LocalStorageSecretStore, SignOutRecord,
    sign_out_residue_retry_blocked_copy, with_web_mutation_lock,
};
use fauna_core::localized::LocalizedText;
use futures_util::future::{Either, join_all, select};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

/// How long one sweep waits for its account stores to go. A delete a foreign
/// connection blocks answers at once (the browser says `blocked`); the budget
/// is for the request that says nothing — one queued behind an earlier delete
/// of the same store, or a large store still being removed. Every planned
/// account is erased under the same budget at the same time, so it bounds the
/// sweep, not each account. A store that outlives it stays in the record and
/// is counted in the line; the next load settles it.
pub(crate) const ERASE_BUDGET: Duration = Duration::from_secs(5);

/// Erase everything this origin holds for `actor_hex` outside the registry
/// namespace, within [`ERASE_BUDGET`]. Idempotent: an account with no store
/// erases cleanly. `Err` is "its store may still be here".
pub(crate) async fn erase_actor_scope(actor_hex: &str) -> anyhow::Result<()> {
    erase_actor_scope_under(&StoreRoot::platform(), actor_hex).await
}

/// [`erase_actor_scope`] under an explicit store root (tests).
pub(crate) async fn erase_actor_scope_under(
    root: &StoreRoot,
    actor_hex: &str,
) -> anyhow::Result<()> {
    let name = root.store_name(actor_hex)?;
    let erase = std::pin::pin!(IndexedDbBackend::delete(&name));
    let budget = std::pin::pin!(fauna_sleep::sleep(ERASE_BUDGET));
    match select(erase, budget).await {
        Either::Left((erased, _)) => erased,
        Either::Right(_) => {
            anyhow::bail!("delete account store {name:?}: not finished within {ERASE_BUDGET:?}")
        }
    }
}

/// The accounts the registry names right now.
fn registry_accounts() -> Vec<String> {
    AccountRegistry::new(Arc::new(LocalStorageSecretStore))
        .list()
        .into_iter()
        .map(|a| a.actor_id)
        .collect()
}

/// Erase one planned account and take it out of the record. Answers whether
/// the account owes nothing more. A failed erase leaves it recorded for the
/// next load and answers `false`; an id that is no actor's can never be erased
/// and is dropped. The name goes to the log, never to the user.
async fn erase_and_settle(actor: &str) -> bool {
    if StoreRoot::platform().store_name(actor).is_err() {
        tracing::warn!("[account-scope] sign-out record names {actor:?}, no actor id — dropped");
        SignOutRecord::settle(&LocalStorageSecretStore, actor);
        return true;
    }
    match erase_actor_scope(actor).await {
        Ok(()) => {
            tracing::info!("[account-scope] erase: removed account scope {actor}");
            SignOutRecord::settle(&LocalStorageSecretStore, actor);
            true
        }
        Err(e) => {
            tracing::warn!(
                "[account-scope] erase: account scope {actor} not removed, retried at the \
                 next load: {e:#}"
            );
            false
        }
    }
}

/// What [`finish_recorded`] did, as the SPA reads it (`signOutFinish`).
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct SignOutFinish {
    /// A record was found: the caller drops a tab pin the wipe orphaned.
    pub(crate) found: bool,
    /// The line the user is owed when account stores are still here —
    /// `EraseResidueView`'s, or the retry's refusal when another tab serves a
    /// planned account — as a `LocalizedText` `{ key, args }`. Absent after a
    /// clean sweep, which says nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) residue: Option<LocalizedText>,
}

/// The residue line for `survivors` account stores still in the origin. Web
/// is out of the credential class (`removeItem` has no failure a read-back
/// could disagree with), so the view never carries credentials; and the
/// onboarding page paints the Remove Again control beside the line, so it is
/// the `Rendered` one.
fn residue_line(survivors: usize) -> Option<LocalizedText> {
    EraseResidueView::from_survivor_count(survivors)
        .copy()
        .warning
}

/// The line for a sweep another tab refused: the retry's own refusal, which
/// names the remedy (close it, press Remove Again), in the count line's place —
/// the native re-sweep's `ResidueRetry::Blocked` paint. Nothing when no account
/// was planned, so a refusal over nothing never paints a view.
fn refused_line(planned: usize) -> Option<LocalizedText> {
    (planned > 0).then(sign_out_residue_retry_blocked_copy)
}

/// The accounts whose store a [`finish_recorded`] run now would erase — what
/// the caller puts to the other-tab probe before a load's sweep. Empty with no
/// record.
fn planned_erase() -> Vec<String> {
    SignOutRecord::load(&LocalStorageSecretStore)
        .map(|record| record.plan(&registry_accounts()).erase)
        .unwrap_or_default()
}

/// Do what the sign-out record owes, and answer what is left.
///
/// With the wipe owed: every account the registry names joins the record, the
/// all-accounts credential wipe runs under the cross-tab mutation lock, and
/// every recorded store is erased. With the wipe done: a recorded account the
/// registry names again is signed in here once more — left untouched, dropped
/// from the record — and the others are erased.
///
/// `erase_refused` is the other-tab probe's answer over [`planned_erase`]:
/// another tab serves one of those accounts, so this run erases no store and
/// every planned account stays recorded (the native re-sweep's rule — any one
/// served refuses the whole sweep). The wipe and the keep rule still run: the
/// wipe is the sign-out's own decision, already made, and neither touches a
/// store. A refused sweep keeps every planned account and answers the
/// refusal's line.
pub(crate) async fn finish_recorded(erase_refused: bool) -> SignOutFinish {
    let store = LocalStorageSecretStore;
    let Some(record) = SignOutRecord::load(&store) else {
        return SignOutFinish::default();
    };
    let plan = record.plan(&registry_accounts());
    if plan.wipe {
        SignOutRecord::before_wipe(&store, plan.erase.iter().cloned());
        with_web_mutation_lock(|| {
            let _ = AccountRegistry::new(Arc::new(LocalStorageSecretStore)).clear_all();
        })
        .await;
        SignOutRecord::wipe_done(&store);
        tracing::info!(
            "[account-scope] sign-out: credentials wiped, {} account store(s) to erase",
            plan.erase.len()
        );
    }
    for actor in &plan.keep {
        tracing::info!(
            "[account-scope] {actor} is signed in here again — its store is kept and leaves \
             the sign-out record"
        );
        SignOutRecord::settle(&store, actor);
    }
    let residue = if erase_refused {
        tracing::warn!(
            "[account-scope] another tab serves a recorded account — {} account store(s) left \
             for a later sweep",
            plan.erase.len()
        );
        refused_line(plan.erase.len())
    } else {
        let erased = join_all(plan.erase.iter().map(|actor| erase_and_settle(actor))).await;
        residue_line(erased.iter().filter(|gone| !**gone).count())
    };
    SignOutFinish {
        found: true,
        residue,
    }
}

/// A remove-account's erase of `actor`, after the registry removal was tried:
/// erased when the registry no longer names it, left untouched (and dropped
/// from the record) when the removal was refused. Never runs another record's
/// wipe — a sign-out in flight in a sibling tab finishes its own.
pub(crate) async fn finish_removed_account(actor: &str) {
    let named = registry_accounts()
        .iter()
        .any(|a| a.eq_ignore_ascii_case(actor));
    if named {
        SignOutRecord::settle(&LocalStorageSecretStore, actor);
    } else {
        // A store that would not go stays recorded; the next load's sweep
        // takes it, and tells the user if it lands on onboarding.
        erase_and_settle(actor).await;
    }
}

/// **The sign-out's decision**, written before the gesture's first await: the
/// record names every account the registry lists plus `signed_in_actor` (the
/// tab's own identity, which an unreadable registry would not name), and owes
/// the credential wipe.
#[wasm_bindgen(js_name = signOutRecordBegin)]
pub fn sign_out_record_begin(signed_in_actor: Option<String>) {
    let reach = registry_accounts().into_iter().chain(signed_in_actor);
    let record = SignOutRecord::record_sign_out(&LocalStorageSecretStore, reach);
    tracing::info!(
        "[account-scope] sign-out recorded for {} account(s)",
        record.accounts.len()
    );
}

/// Whether a confirmed sign-out's credential wipe is still owed. The
/// identity read fails closed on it: a load that finds the record shows no
/// account as signed in while it finishes the sign-out.
#[wasm_bindgen(js_name = signOutWipeOwed)]
pub fn sign_out_wipe_owed() -> bool {
    SignOutRecord::wipe_owed_in(&LocalStorageSecretStore)
}

/// The accounts whose store [`sign_out_finish`] would erase right now — what a
/// load puts to the other-tab probe before it sweeps.
#[wasm_bindgen(js_name = signOutPlannedErase)]
pub fn sign_out_planned_erase() -> Vec<String> {
    planned_erase()
}

/// Finish whatever the sign-out record owes ([`finish_recorded`]). Resolves to
/// `{ found, residue? }`: whether a record was found — the caller then drops
/// this tab's account pin, which names an account the wipe removed — and the
/// residue line as a `LocalizedText` when account stores are still here.
/// `erase_refused` is the caller's other-tab probe over
/// [`sign_out_planned_erase`].
#[wasm_bindgen(
    js_name = signOutFinish,
    unchecked_return_type = "Promise<{ found: boolean; residue?: { key: string; args?: Record<string, string> } }>"
)]
pub fn sign_out_finish(erase_refused: bool) -> js_sys::Promise {
    future_to_promise(async move { crate::rpc::to_js(&finish_recorded(erase_refused).await) })
}
