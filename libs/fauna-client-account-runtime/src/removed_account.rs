//! A remove-account's nest half: retire this machine from the account it
//! just removed, as a sign-out does (`apps/account-scoping.md` § Erasure
//! follows scope → *Removing an account retires this machine from it, as a
//! sign-out does*, decisions 2 and 3).
//!
//! The removed account has no runtime in this process — the gesture refuses
//! the account this instance serves — so the retirement needs a session and a
//! runtime of its own: a sign-in as the removed account with the secret the
//! removal took into memory, the ordinary account runtime assembled over that
//! snapshot and the account's still-present store, and that runtime's
//! sign-out stop (`AccountStoreHandle::shutdown_for_sign_out`) as soon as it
//! is ready. No retire-only mode and no second retirement path: what a
//! sign-out retires is what this retires.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_accounts::RemovedAccountCredentials;
use fauna_client_core::succession_delivery::SignIn;
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_sync_engine::account_runtime::{AccountStoreHandle, production_credential_store};

pub use fauna_account_plane::account_driver::{
    EnrollmentRetirement, REMOVED_ACCOUNT_RETIRE_BUDGET,
};

use crate::{
    ACCOUNT_RUNTIME_STOP_BUDGET, AppRuntimeInputs, ResolveContext, SandboxedStoreContainer,
    assemble, build_params, sign_in_with_seed,
};

/// What the app brings to a removal's nest half — the two per-app facts the
/// live assembly's [`AppRuntimeInputs`] and [`ResolveContext`] also carry.
pub struct RemovedAccountInputs {
    /// Resolves this machine's own device id for the removed account, hex —
    /// the named row the retirement clears. Read on a blocking task, as the
    /// live assembly reads it; the account's scope is still on disk, since
    /// the erase follows the nest half.
    pub own_device_id_hex: Box<dyn FnOnce() -> Option<String> + Send>,
    /// The sandboxed shell's store container, exactly as the live assembly
    /// passes it (`AppRuntimeInputs::store_container`) — `None` on every
    /// desktop.
    pub store_container: Option<SandboxedStoreContainer>,
}

/// Retire this machine from the account `removed` holds the slots of, within
/// [`REMOVED_ACCOUNT_RETIRE_BUDGET`], and say how it went — the answer is
/// logged here and worth asserting, never worth branching on: the removal's
/// erase follows regardless.
///
/// * No device authorization in the snapshot → [`EnrollmentRetirement::NotEnrolled`]
///   at once: there is no grant, so nothing is signed in to or assembled.
/// * A nest that cannot be reached, refuses the sign-in, or answers nothing in
///   time → [`EnrollmentRetirement::Deferred`] inside the budget; the
///   enrollment stays until the user removes the device from another device's
///   Devices page (decision 3).
///
/// Every slot the assembly reads or writes is the snapshot's, through
/// [`CredentialStore::detached_like`] over the production account-store
/// namespace — so nothing reaches the registry the removal just left
/// (decision 1), and the replica opens under the posture the real slot would
/// have opened it under.
pub async fn retire_removed_account(
    removed: Arc<RemovedAccountCredentials>,
    inputs: RemovedAccountInputs,
) -> EnrollmentRetirement {
    let actor = removed.actor_id().to_owned();
    let retirement = retire(removed, inputs).await;
    tracing::info!(
        actor = %actor,
        ?retirement,
        "[account-runtime] remove-account: the machine's enrollment retirement"
    );
    retirement
}

async fn retire(
    removed: Arc<RemovedAccountCredentials>,
    inputs: RemovedAccountInputs,
) -> EnrollmentRetirement {
    if !removed.holds_device_authorization() {
        return EnrollmentRetirement::NotEnrolled;
    }
    // The stop is a sign-out's and takes a sign-out's budget; the sign-in and
    // the assembly share what is left of the whole.
    let assembly_budget = REMOVED_ACCOUNT_RETIRE_BUDGET - ACCOUNT_RUNTIME_STOP_BUDGET;
    let (client, handle) = match tokio::time::timeout(
        assembly_budget,
        sign_in_and_assemble(Arc::clone(&removed), inputs),
    )
    .await
    {
        Ok(Ok(assembled)) => assembled,
        Ok(Err(e)) => return EnrollmentRetirement::Deferred(format!("{e:#}")),
        Err(_) => {
            return EnrollmentRetirement::Deferred(format!(
                "the nest did not answer within {}s",
                assembly_budget.as_secs()
            ));
        }
    };
    let retirement =
        match tokio::time::timeout(ACCOUNT_RUNTIME_STOP_BUDGET, handle.shutdown_for_sign_out())
            .await
        {
            Ok(retirement) => retirement,
            // Dropping the handle below ends the runtime's host all the same.
            Err(_) => EnrollmentRetirement::Deferred(format!(
                "the stop did not finish within {}s",
                ACCOUNT_RUNTIME_STOP_BUDGET.as_secs()
            )),
        };
    drop(handle);
    client.disconnect().await;
    retirement
}

/// The sign-in as the removed account and the runtime over its snapshot.
async fn sign_in_and_assemble(
    removed: Arc<RemovedAccountCredentials>,
    inputs: RemovedAccountInputs,
) -> anyhow::Result<(Arc<NestClient>, AccountStoreHandle)> {
    let actor_id_hex = removed.actor_id().to_owned();
    let secret = removed
        .secret_hex()
        .ok_or_else(|| anyhow::anyhow!("the removal took no identity secret to sign in with"))?;
    let nest_url = removed
        .nest_url()
        .ok_or_else(|| anyhow::anyhow!("the removed account was bound to no nest"))?;
    let keypair = ActorKeypair::from_secret_hex(&secret)
        .map_err(|e| anyhow::anyhow!("the removed account's secret: {e}"))?;
    let principal = ActorKeypair::from_secret(*keypair.secret_bytes());
    let client = match sign_in_with_seed(&nest_url, keypair).await {
        SignIn::Connected(client) => client,
        SignIn::NoAccount => anyhow::bail!("the nest holds no account for the removed identity"),
        SignIn::Failed(e) => anyhow::bail!("sign-in: {e}"),
        SignIn::NoSeed => anyhow::bail!("sign-in: no seed"),
    };
    let snapshot_slot =
        || CredentialStore::detached_like(&production_credential_store(), removed.clone());
    let mut params = build_params(
        AppRuntimeInputs {
            actor_id_hex,
            principal: principal.into(),
            // Nothing this runtime does reads them: it is stopped as soon as
            // it is ready, and its stop runs no pass to walk channels or
            // carry predecessors.
            memberships: None,
            attested_predecessors: Default::default(),
            peer_transport: None,
            store_container: inputs.store_container,
        },
        Arc::clone(&client),
        &nest_url,
    )?;
    params.credentials = snapshot_slot();
    let assembled = assemble(
        params,
        ResolveContext {
            nest_url,
            own_device_id_hex: inputs.own_device_id_hex,
        },
        snapshot_slot(),
    )
    .await;
    match assembled {
        Ok(handle) => Ok((client, handle)),
        Err(e) => {
            client.disconnect().await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use fauna_client_accounts::{AccountRegistry, InMemorySecretStore, SecretStore};

    use super::*;

    // Built rather than spelled, so no literal reads as a leaked key.
    fn secret() -> String {
        "33".repeat(32)
    }

    /// A registry holding one account bound to `nest_url`, with this
    /// machine's enrollment — a device authorization and a writer key — in the
    /// account store's namespace, removed with its snapshot.
    fn removed_account(
        nest_url: &str,
        enrolled: bool,
    ) -> (
        AccountRegistry,
        Arc<InMemorySecretStore>,
        Arc<RemovedAccountCredentials>,
    ) {
        let aux = Arc::new(InMemorySecretStore::new());
        let registry = AccountRegistry::new(Arc::new(InMemorySecretStore::new()))
            .also_erasing(vec![aux.clone() as Arc<dyn SecretStore>]);
        let actor = registry.add_account(&secret(), None, None).unwrap();
        registry.set_nest_url(&actor, nest_url);
        aux.set(&actor, &"44".repeat(32));
        if enrolled {
            aux.set(&format!("{actor}/device-auth"), "an authorization");
        }
        let snapshot = registry.remove_with_snapshot(&actor).unwrap();
        (registry, aux, Arc::new(snapshot))
    }

    fn inputs(store: &std::path::Path) -> RemovedAccountInputs {
        RemovedAccountInputs {
            own_device_id_hex: Box::new(|| Some("55".repeat(32))),
            store_container: Some(SandboxedStoreContainer {
                dir: store.to_path_buf(),
                exclusion:
                    fauna_sync_engine::account_runtime::CloudBackupExclusion::platform_desktop(),
            }),
        }
    }

    /// A removal of an account this machine never enrolled on has no grant
    /// to retire: it answers at once and dials nothing — the nest URL here
    /// would hang any dial for the whole budget.
    #[tokio::test]
    async fn an_unenrolled_removal_retires_nothing_and_dials_nothing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (_registry, _aux, snapshot) = removed_account(&url, false);
        let dir = tempfile::tempdir().unwrap();

        let started = Instant::now();
        let retirement = retire_removed_account(snapshot, inputs(dir.path())).await;

        assert_eq!(retirement, EnrollmentRetirement::NotEnrolled);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    /// Decision 3: a nest that accepts the connection and answers nothing
    /// leaves the retirement `Deferred` inside the budget — the gesture's
    /// erase is never held past it — and the attempt writes nothing to the
    /// registry the removal left (decision 1): the removed account's slots
    /// are still absent from every namespace.
    #[tokio::test]
    async fn a_silent_nest_defers_inside_the_budget_and_the_registry_stays_clean() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        // Accept and hold every connection, answering nothing.
        let held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                open.push(socket);
            }
        });
        let (registry, aux, snapshot) = removed_account(&url, true);
        let actor = snapshot.actor_id().to_owned();
        let dir = tempfile::tempdir().unwrap();

        let started = Instant::now();
        let retirement = retire_removed_account(snapshot, inputs(dir.path())).await;
        let took = started.elapsed();
        held.abort();

        assert!(
            matches!(retirement, EnrollmentRetirement::Deferred(_)),
            "{retirement:?}"
        );
        assert!(
            took <= REMOVED_ACCOUNT_RETIRE_BUDGET + std::time::Duration::from_secs(1),
            "the nest half held the gesture for {took:?}"
        );
        assert!(registry.list().is_empty());
        for key in [
            actor.clone(),
            format!("{actor}/device-auth"),
            format!("{actor}/grant-registered"),
            format!("fauna/{actor}/secret"),
        ] {
            assert_eq!(aux.get(&key), None, "{key} came back");
        }
    }

    /// A nest that refuses the connection outright defers too.
    #[tokio::test]
    async fn an_unreachable_nest_defers() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let (_registry, _aux, snapshot) =
            removed_account(&format!("http://127.0.0.1:{port}"), true);
        let dir = tempfile::tempdir().unwrap();

        let retirement = retire_removed_account(snapshot, inputs(dir.path())).await;

        assert!(
            matches!(retirement, EnrollmentRetirement::Deferred(_)),
            "{retirement:?}"
        );
    }
}
