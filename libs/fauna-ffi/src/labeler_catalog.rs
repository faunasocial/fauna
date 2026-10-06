//! Re-exports the page-level Labeler-Catalog state machine so its UniFFI
//! exports surface in the generated Swift / Kotlin / C# bindings, plus a
//! free-fn constructor that builds the machine over an [`FfiNestClient`]'s
//! WS-RPC connection. The machine itself lives in
//! libs/fauna-labeler-catalog-machine; this file is a thin glue layer
//! (mirrors src/devices.rs).

use std::sync::Arc;

pub use fauna_labeler_catalog_machine::{
    LabelerCatalogEntry, LabelerCatalogMachine, LabelerCatalogObserver, LabelerCatalogSnapshot,
    LabelerInspectView,
};

use crate::{FfiError, FfiNestClient};

/// Build a [`LabelerCatalogMachine`] for the labeler-catalog page (and the
/// personalization home's subscribed-labelers facet, which reads the same
/// snapshot) over `nest`'s authenticated WS-RPC connection. `observer` ticks
/// on every snapshot change. The machine owns the catalog read (`refresh()`),
/// the inspect-before-subscribe trust gate (`inspect()` / `close_inspect()`),
/// and the (un)subscribe gestures (`subscribe()` / `unsubscribe()`) — all over
/// the same connection.
///
/// Built **with its grant seams** from the actor's 32-byte ed25519 `secret`:
/// subscribing a `wasm` mail labeler mints the per-labeler grant to this
/// nest's mail service and unsubscribing revokes it
/// (`content-moderation-and-ranking.md` § Tier-3 → *Subscribing = minting a
/// capability*) — the keypair signs the grant-log events the account
/// plane's succession ledger keeps. Returns `Result` because the keypair
/// derivation is fallible (the `build_mail_settings_machine` shape).
#[uniffi::export]
pub fn build_labeler_catalog_machine_with_grants(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    observer: Arc<dyn LabelerCatalogObserver>,
) -> Result<Arc<LabelerCatalogMachine>, FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    Ok(
        fauna_labeler_catalog_machine::build_labeler_catalog_machine_with_grants(
            nest.nest_arc(),
            keypair,
            crate::ledger_seam(),
            crate::account_runtime::mail_store(),
            observer,
        ),
    )
}
