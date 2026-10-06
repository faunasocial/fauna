//! Re-exports the user-settings Linked-nests machine so its UniFFI exports
//! surface in the generated Swift / Kotlin / C# bindings, plus a free-fn
//! constructor that builds it over an [`FfiNestClient`]'s **bearer** WS-RPC
//! connection (`fauna.pair.{list,add,revoke}` are `User`-gated, owner-scoped).
//!
//! The machine lives in its home crate `fauna-client-pair`
//! ([`LinkedNestsMachine`], the `linked-nests` page). This file is a thin glue
//! layer mirroring `src/mail_admin.rs`: it re-exports the machine + snapshot +
//! action + error types and wraps the crate's native `build_linked_nests_machine`
//! seam constructor in a `#[uniffi::export]` free fn.
//!
//! Per priority #2, the snapshot projection + the link/list/unlink sequencing
//! live in shared Rust; the per-app (windows/macos/ios/android) UI only
//! constructs a machine here and renders its snapshot / dispatches its actions.
//! Linux builds the same machine natively via the same crate. See
//! `docs/goal/behavior/linked-nests.md`.

use std::sync::Arc;

pub use fauna_client_pair::{
    LinkInput, LinkedNestRow, LinkedNestStatus, LinkedNestsAction, LinkedNestsMachine,
    LinkedNestsSnapshot, PairDispatchError, PairNestError,
};

use crate::FfiNestClient;

/// Build a [`LinkedNestsMachine`] for the user-settings `linked-nests` page over
/// `nest`'s bearer WS-RPC connection.
#[uniffi::export]
pub fn build_linked_nests_machine(nest: Arc<FfiNestClient>) -> Arc<LinkedNestsMachine> {
    Arc::new(fauna_client_pair::build_linked_nests_machine(
        nest.nest_arc(),
    ))
}

/// Build a [`LinkedNestsMachine`] wired with the **mail relay-provisioning
/// post-link hook** (the home-with-public-relay one-action flow): a both-ends
/// `LinkBoth` auto-provisions the just-linked home box's mailbox reusing the fleet
/// MSEK, so the user links once and relayed mail is immediately readable there
/// (`docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Pairing).
/// Unlike [`build_linked_nests_machine`] this needs the actor's 32-byte ed25519
/// `secret` (to seal/read the primary's mail MSEK + provision the peer).
/// The shared substrate every native app lifts onto (linux leads natively via
/// the same `rpc_glue` constructor); requires the `mail-admin` feature (the mail
/// crate provides the hook). Returns `Result` because the keypair derivation is
/// fallible.
#[cfg(feature = "mail-admin")]
#[uniffi::export]
pub fn build_linked_nests_machine_with_mail_relay(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
) -> Result<Arc<LinkedNestsMachine>, crate::FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    Ok(Arc::new(
        fauna_client_mail_settings::rpc_glue::build_linked_nests_machine_with_mail_relay(
            nest.nest_arc(),
            keypair,
            crate::ledger_seam(),
            crate::account_runtime::mail_store(),
        ),
    ))
}

/// Build a [`LinkedNestsMachine`] wired with the **Nests-page trust facet**: the
/// machine hydrates the connected nest's trust facet — its content-processor
/// holders + the signed grant-event log's Now/History folds — and drives
/// Mint/Renew/Revoke of content-processing grants (`docs/goal/ui/nests.md`
/// § Where logic lives). Like [`build_linked_nests_machine_with_mail_relay`] it
/// needs the actor's 32-byte ed25519 `secret`: the shared trust seams sign the
/// grant-event log (recorded through this process's account store —
/// `crate::ledger_seam`) with the identity key,
/// which **never crosses back over the FFI boundary** (the seam signs; the
/// per-app shell only renders the snapshot + dispatches Mint/Renew/Revoke).
/// The shared substrate every native app (windows/macos/ios/android; linux
/// natively) lifts onto for the Nests page — no per-app trust logic
/// (priority #1/#2). `Result` because the keypair derivation is fallible.
#[uniffi::export]
pub fn build_linked_nests_machine_with_trust(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
) -> Result<Arc<LinkedNestsMachine>, crate::FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    let names = folder_names(&nest, &keypair);
    Ok(Arc::new(
        fauna_client_pair::build_linked_nests_machine_with_trust(
            nest.nest_arc(),
            keypair,
            crate::ledger_seam(),
            crate::backup_seam(),
            blessing_door(),
            crate::account_runtime::period_key_store(),
            crate::account_runtime::mail_store(),
        )
        .with_folder_names(names),
    ))
}

/// Build a [`LinkedNestsMachine`] wired with **both** the mail relay-provisioning
/// post-link hook AND the Nests-page trust facet — the combined constructor a
/// native app's trust-enabled Nests page uses when it also wires the
/// home-with-public-relay one-action mail flow (linux builds this natively via
/// the same `rpc_glue` constructor; `nests.md` § Where logic lives + Implementation
/// status). It is the union of [`build_linked_nests_machine_with_mail_relay`] and
/// [`build_linked_nests_machine_with_trust`]: the `LinkBoth` hook auto-provisions
/// the just-linked home box's mailbox, and the machine hydrates the trust facet +
/// drives Mint/Renew/Revoke/SetLens. Needs the actor's 32-byte ed25519 `secret`
/// for the same two reasons the siblings do (seal/read the primary's MSEK for the
/// hook; sign the grant-event log for the trust seams — the
/// raw identity key never crosses back over FFI). One shared shape for all four
/// native apps (priority #1/#2). `Result` because keypair derivation is
/// fallible; requires the `mail-admin` feature (the mail crate provides the hook).
#[cfg(feature = "mail-admin")]
#[uniffi::export]
pub fn build_linked_nests_machine_with_mail_relay_and_trust(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
) -> Result<Arc<LinkedNestsMachine>, crate::FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    let names = folder_names(&nest, &keypair);
    Ok(Arc::new(
        fauna_client_mail_settings::rpc_glue::build_linked_nests_machine_with_mail_relay_and_trust(
            nest.nest_arc(),
            keypair,
            crate::ledger_seam(),
            crate::backup_seam(),
            blessing_door(),
            crate::account_runtime::period_key_store(),
            crate::account_runtime::mail_store(),
        )
        .with_folder_names(names),
    ))
}

/// The seam that names a web-serve paywall grant's folder on the trust facet —
/// the seat's folder-key custody plus the owner's folder list over `nest`
/// (`nests.md` § Trust facet — grants). One call site for both trust builders;
/// the secret stays inside the seam and never crosses back over FFI.
fn folder_names(
    nest: &FfiNestClient,
    keypair: &fauna_core::identity::ActorKeypair,
) -> fauna_client_pair::TrustFolderNames {
    fauna_client_pair::TrustFolderNames::new(
        keypair,
        Arc::new(fauna_client_folders::CustodyOwnedSetNames {
            keys: crate::account_runtime::folder_key_store(),
            nest: nest.nest_arc(),
        }),
    )
}

/// The Nests page's blessing door — the account plane's
/// `fauna.state.blessed-nests`, over this seat's runtime handle (the shared
/// impl, `fauna_account_seams::blessed_nests`). One call site for both trust
/// builders, so all four UniFFI apps wire it identically.
#[cfg(feature = "account-runtime")]
fn blessing_door() -> Arc<dyn fauna_client_pair::BlessedNestsStore> {
    Arc::new(
        fauna_client_account_runtime::blessed_nests::PlaneBlessedNests::new(
            crate::account_runtime::handle,
        ),
    )
}

/// A build without the account runtime has no plane to hold a blessing: every
/// verdict reads un-blessed and every toggle is refused, never kept elsewhere
/// (the kind is plane-only).
#[cfg(not(feature = "account-runtime"))]
fn blessing_door() -> Arc<dyn fauna_client_pair::BlessedNestsStore> {
    Arc::new(fauna_client_pair::NoAccountRuntime)
}

/// Classify a user-entered link-form value: a 64-hex Ed25519 identity routes to
/// the single-end [`LinkedNestsAction::Link`]; any other value is a nest address
/// routing to the both-ends [`LinkedNestsAction::LinkBoth`]. Shared so every
/// app (linux native calls the crate fn directly; the page-based clients call
/// this) routes the same input to the same action (priority #2).
#[uniffi::export]
pub fn classify_link_input(raw: String) -> LinkInput {
    fauna_client_pair::classify_link_input(&raw)
}
