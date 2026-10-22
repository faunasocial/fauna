//! The approving half of the **consent-time grant** to a third-party app over
//! its `ext.*` record kinds (`docs/goal/architecture/third-party-kinds.md`
//! § The record doors → *Who mints the consent-time grant*), shared by every
//! machine that answers a consent request — the AT Protocol page's card and
//! the Connected apps tray — so neither re-implements a step.
//!
//! [`answer_consent`] runs, for an approve naming a `fauna:records:rw:` scope:
//! the shared entry point `fauna_client_capabilities::ext_consent::
//! prepare_ext_consent_grant` (verify the manifest, publish the
//! `fauna.state.kind-manifest` row, mint, record the `Mint`), then the
//! deposit (`fauna.capabilities.mint`), then the resolve — mint before
//! resolve, because the nest mints the app's row at `/oauth/token` with no
//! client in the loop. A resolve that does not land after the deposit
//! (answered elsewhere, expired, refused) revokes what the approve began
//! best-effort, so it never outlives the consent it was minted for; the nest's
//! unredeemed-grant sweep is the backstop when even that fails.
//!
//! A `fauna:folder:read:<id>` scope's twin is its own grant beside the
//! records grant (`docs/goal/behavior/webdav-server.md` § Key model → *A
//! principal's read* rule (1)): the machine resolves each named folder through
//! the owner's folder custody ([`ConsentFolderSeam`], [`CustodyConsentFolders`]
//! the shared impl), the entry point plans one `content.read{folder, set}`
//! grant per folder under its derived id, and every prepared grant is
//! deposited in order before the resolve. The same seam names the folder on
//! the card ([`consent_folder_names`]).
//!
//! A `fauna:folder:deposit:<id>` scope's keyless `deposit` tuple rides the
//! consent's one grant (`docs/goal/behavior/file-sync.md` § Third-party
//! deposit ingress) and needs no seam. A row carrying a BARE folder-plane
//! scope is approved with the card's chosen folder
//! (`docs/goal/behavior/authorization-server.md` § Consent → *The card
//! chooses the folder*): the approve plans over the row's scopes qualified
//! by it (`ext_consent::approved_scopes`) and resolves with it; a bare row
//! with no choice, or a choice on a row that takes none, is refused before
//! anything is minted.
//!
//! A request naming no twin-bearing scope, and every decline, resolve exactly
//! as they always have. A twin-bearing approve on a machine built without
//! [`ConsentGrantSeams`] — or a folder read on one built without the folder
//! seam — is refused rather than resolved keyless: the app would be connected
//! with no access to what the card promised.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

pub use fauna_client_capabilities::ext_consent::ConsentFolder;
use fauna_client_capabilities::ext_consent::{
    ExtConsentError, ExtConsentOwner, PreparedExtGrant, approved_scopes, folder_read_ids,
    names_twin_scope, prepare_ext_consent_grant,
};
use fauna_client_capabilities::grant_log::{self, GrantEventSigner, KeypairGrantEventSigner};
use fauna_core::crypto::{BackupKey, DelegableSchedule};
use fauna_core::identity::ActorKeypair;

use crate::nest_api::NestConsentRow;

/// The owner-side seams the consent-time grant needs beyond the nest API —
/// the `fauna-labeler-catalog-machine` `LabelerGrantSeams` shape.
pub struct ConsentGrantSeams {
    /// The owner's identity pubkey (the blob's index, the log's chain).
    pub actor_id: [u8; 32],
    /// The owner's identity keypair — its secret derives each folder twin's
    /// grant id (`fauna_client_capabilities::folder_principal_grant_id`).
    pub owner_keypair: Arc<ActorKeypair>,
    /// The owner's delegable branch — the only typed path a grant pair leaves.
    pub delegable: Arc<DelegableSchedule>,
    /// The account's kind-manifest rows (the registry overlay's source).
    pub manifests: Arc<dyn fauna_client_config::KindManifestStore>,
    /// The account's grant-event log.
    pub ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    /// The owner's grant-event signer.
    pub signer: Arc<dyn GrantEventSigner>,
    /// The owner's folder custody, for a `folder:read` scope's twin and the
    /// card's folder name — `None` refuses a folder read (`Unwired`).
    pub folders: Option<Arc<dyn ConsentFolderSeam>>,
    /// The account's D10 authoring-delegation doors, for a
    /// `fauna:identity:op:post.author` or `profile.author` scope — approving
    /// either class provisions the delegation when the account has none
    /// ([`ensure_authoring_delegation`]). `None` refuses those classes
    /// (`Unwired`).
    pub authoring: Option<Arc<dyn crate::nest_api::AtprotoSettingsNestApi>>,
}

/// The owner's folder custody as the consent machine asks it — the folder
/// read twin's seam (`webdav-server.md` § Key model → *A principal's read*
/// (1)): a folder id's set name, its served state and its keys.
///
/// Its [`fauna_client_capabilities::OwnedSetNames`] half is what a
/// principal's folder grant id is matched over ([`principal_folder_names`]).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ConsentFolderSeam: fauna_client_capabilities::OwnedSetNames {
    /// The owned folder with row id `id`, resolved from custody — `None` when
    /// it is not one of the owner's folders or cannot be read now (the
    /// approve then refuses: fail closed).
    async fn folder_for_consent(&self, id: i64) -> Option<ConsentFolder>;
}

/// The custody fold behind [`ConsentFolderSeam`]: the owned row `id` among
/// `rows` (owner-scoped `fauna.folders.list` rows, named from `custody` by
/// hash where the nest rests no name), its served state from custody —
/// never the nest's `webdav_enabled` (ruling (7)(b)(ii)) — and the channel
/// its content keys rest at. A member row is never the reader's own folder.
#[must_use]
pub fn consent_folder_from(
    rows: Vec<fauna_protocol::folders::FolderSummary>,
    custody: fauna_core::data::FoldersConfig,
    id: i64,
) -> Option<ConsentFolder> {
    let row = fauna_client_folders::custody::named_from_custody(rows, &custody)
        .into_iter()
        .find(|r| r.id == id && r.role.as_deref() != Some("member"))?;
    Some(ConsentFolder {
        served_by_custody: fauna_client_folders::custody_served(&row, &custody),
        custody_channel_id: fauna_client_folders::owned_custody_channel(&row).ok(),
        set_name: row.name,
        custody,
    })
}

/// The shared [`ConsentFolderSeam`]: the owner's folder-key custody plus the
/// owner-scoped `fauna.folders.list`, read fresh per ask — what the Media
/// page and the engine binding resolve a set from. `R` is the host's nest
/// transport (native `Arc<NestClient>`, wasm `WsRpcClient`).
pub struct CustodyConsentFolders<R> {
    /// The account's folder-key custody.
    pub keys: Arc<dyn fauna_client_folders::FolderKeyStore>,
    /// The nest the owner's folder rows are listed from.
    pub nest: R,
}

/// [`ConsentFolderSeam::folder_for_consent`] over any host transport `R`:
/// custody, then the owner-scoped folder list, folded by
/// [`consent_folder_from`]. Both targets' impls call it.
#[cfg(feature = "rpc-glue")]
async fn folder_for_consent_over<R: fauna_protocol::RpcRequester>(
    keys: &dyn fauna_client_folders::FolderKeyStore,
    nest: R,
    id: i64,
) -> Option<ConsentFolder> {
    let custody = keys
        .load()
        .await
        .inspect_err(|e| tracing::warn!(target: "fauna_atproto_settings", error = %e, "consent folder: custody unreadable"))
        .ok()?;
    let rows = fauna_client_folders::FoldersClient::new(nest)
        .list_wire()
        .await
        .inspect_err(|e| tracing::warn!(target: "fauna_atproto_settings", error = %e, "consent folder: folder list unreadable"))
        .ok()?
        .folders;
    consent_folder_from(rows, custody, id)
}

#[cfg(all(feature = "rpc-glue", not(target_arch = "wasm32")))]
#[async_trait::async_trait]
impl ConsentFolderSeam for CustodyConsentFolders<Arc<fauna_client::NestClient>> {
    async fn folder_for_consent(&self, id: i64) -> Option<ConsentFolder> {
        folder_for_consent_over(&*self.keys, self.nest.clone(), id).await
    }
}

/// The SPA's twin: the same custody fold over the browser session
/// (`WsRpcClient`), the shape `CustodyOwnedSetNames` has on both targets.
#[cfg(all(feature = "rpc-glue", target_arch = "wasm32"))]
#[async_trait::async_trait(?Send)]
impl ConsentFolderSeam for CustodyConsentFolders<fauna_rpc_wasm::WsRpcClient> {
    async fn folder_for_consent(&self, id: i64) -> Option<ConsentFolder> {
        folder_for_consent_over(&*self.keys, self.nest.clone(), id).await
    }
}

#[cfg(all(feature = "rpc-glue", target_arch = "wasm32"))]
#[async_trait::async_trait(?Send)]
impl fauna_client_capabilities::OwnedSetNames
    for CustodyConsentFolders<fauna_rpc_wasm::WsRpcClient>
{
    async fn owned_set_names(&self) -> Option<Vec<String>> {
        fauna_client_folders::read_owned_set_names(&*self.keys, self.nest.clone()).await
    }
}

#[cfg(all(feature = "rpc-glue", not(target_arch = "wasm32")))]
#[async_trait::async_trait]
impl fauna_client_capabilities::OwnedSetNames
    for CustodyConsentFolders<Arc<fauna_client::NestClient>>
{
    async fn owned_set_names(&self) -> Option<Vec<String>> {
        fauna_client_folders::read_owned_set_names(&*self.keys, self.nest.clone()).await
    }
}

impl ConsentGrantSeams {
    /// The seams from the owner's identity `keypair` over the host's stores:
    /// the delegable branch is derived from the keypair's seed (the backup
    /// key's root), the seams and the signer each hold a copy of the keypair.
    pub fn from_keypair(
        keypair: &ActorKeypair,
        manifests: Arc<dyn fauna_client_config::KindManifestStore>,
        ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    ) -> Self {
        Self {
            actor_id: keypair.actor_id().0,
            owner_keypair: Arc::new(ActorKeypair::from_secret(*keypair.secret_bytes())),
            delegable: Arc::new(DelegableSchedule::derive(&BackupKey::derive(
                keypair.secret_bytes(),
            ))),
            manifests,
            ledger,
            signer: Arc::new(KeypairGrantEventSigner::new(keypair)),
            folders: None,
            authoring: None,
        }
    }

    /// These seams with the owner's folder custody wired — what lets an
    /// approve mint a `folder:read` twin and the card name the folder.
    #[must_use]
    pub fn with_folders(mut self, folders: Arc<dyn ConsentFolderSeam>) -> Self {
        self.folders = Some(folders);
        self
    }

    /// These seams with the account's D10 delegation doors wired — what lets
    /// an approve of the `post.author` or `profile.author` class provision
    /// the delegation.
    #[must_use]
    pub fn with_authoring(mut self, api: Arc<dyn crate::nest_api::AtprotoSettingsNestApi>) -> Self {
        self.authoring = Some(api);
        self
    }

    /// These seams with both nest-backed doors wired over a native session:
    /// the owner's folder custody (`keys`) plus `nest`'s folder list, and the
    /// D10 delegation doors. The one call tui, linux and the four UniFFI apps
    /// make, so none carries its own copy of the pair — the native twin of
    /// [`consent_grant_seams_over_port`].
    #[cfg(all(feature = "rpc-glue", not(target_arch = "wasm32")))]
    #[must_use]
    pub fn with_nest_custody(
        self,
        keys: Arc<dyn fauna_client_folders::FolderKeyStore>,
        nest: Arc<fauna_client::NestClient>,
    ) -> Self {
        let authoring = crate::nest_api::ws_rpc::WsRpcAtprotoSettingsNest::new(Arc::clone(&nest));
        self.with_folders(Arc::new(CustodyConsentFolders { keys, nest }))
            .with_authoring(Arc::new(authoring))
    }
}

/// web's consent seams, wired over the tab's account port: the seams both
/// wasm consent machines (the AT Protocol page's and the Connected apps
/// tray's) take, each in its own chunk with no reach into the core chunk's
/// runtime but through `port` (`account-client-lifecycle.md` § The account
/// port). The grant log and the kind-manifest rows ride the port's forwarders
/// (`succession_ledger_port`, `kind_manifest_port`), the folder custody its
/// `PortFolderKeys`, and the folder list and the D10 delegation doors the
/// chunk's own `client` — the seams `apps/fauna-tui` and `apps/fauna-linux`
/// wire natively, over the one set of forwarders every web consumer shares.
///
/// # Errors
/// `port` is not a `SharedAccountPort` — refused by name, where it was wired.
#[cfg(all(feature = "rpc-glue", feature = "account-port", target_arch = "wasm32"))]
pub fn consent_grant_seams_over_port(
    keypair: &ActorKeypair,
    client: fauna_rpc_wasm::WsRpcClient,
    port: &fauna_account_port::JsAccountPort,
) -> Result<ConsentGrantSeams, fauna_account_port::PortFault> {
    // `JsAccountPort` is not `Clone`: each forwarder wraps its own handle on
    // the one JS port object.
    let again = || fauna_account_port::JsAccountPort::from((**port).clone());
    let ledger =
        fauna_client_config::succession_ledger_port::from_js_port(again(), keypair.actor_id())?;
    let manifests = fauna_client_config::kind_manifest_port::from_js_port(again())?;
    let keys = fauna_client_folders::port::PortFolderKeys::new(
        fauna_account_port::JsAccountTransport::new(again().into())?,
    );
    let authoring = crate::nest_api::ws_rpc::WsRpcAtprotoSettingsNest::new(client.clone());
    let folders = CustodyConsentFolders {
        keys: Arc::new(keys),
        nest: client,
    };
    Ok(ConsentGrantSeams::from_keypair(keypair, manifests, ledger)
        .with_folders(Arc::new(folders))
        .with_authoring(Arc::new(authoring)))
}

/// Why the D10 delegation could not be provisioned.
#[derive(Debug, thiserror::Error)]
pub enum ProvisionDelegationError {
    /// A nest call of the ceremony failed.
    #[error("{}", .0.detail())]
    Nest(crate::nest_api::AtprotoSettingsApiError),
    /// The cert could not be built.
    #[error("{0}")]
    Cert(String),
}

/// The D10 mint ceremony (`atproto-pds-full.md` § D10 → *Mint ceremony*):
/// fetch `K_pub` (minting `K`), sign the cert over THAT key under the
/// account's `identity`, upload it — three nest calls in a fixed order, since
/// the nest refuses a cert naming any key but the one it minted. The cert
/// grants the enumerated authoring set for `DELEGATION_WINDOW_SECS` from
/// `now` (time-bounded by construction). The one ceremony both the AT
/// Protocol page's authorize gesture and the consent of the `post.author` and
/// `profile.author` classes run; re-running it renews a lapsed delegation in
/// place.
pub async fn provision_delegation(
    api: &dyn crate::nest_api::AtprotoSettingsNestApi,
    identity: &ActorKeypair,
    now: fauna_core::data::Timestamp,
) -> Result<(), ProvisionDelegationError> {
    use fauna_client_bridges::atproto_delegation::{
        AUTHORING_CAPABILITIES, DELEGATION_WINDOW_SECS, build_authoring_delegation_cert,
    };
    let k_pub = api
        .fetch_authoring_key()
        .await
        .map_err(ProvisionDelegationError::Nest)?;
    // MICROSECONDS on both sides: `Timestamp` is micros and the window
    // constant is seconds.
    let expires_at = fauna_core::data::Timestamp(now.0 + DELEGATION_WINDOW_SECS * 1_000_000);
    let cert = build_authoring_delegation_cert(
        identity,
        k_pub,
        &AUTHORING_CAPABILITIES,
        now,
        Some(expires_at),
    )
    .map_err(|e| ProvisionDelegationError::Cert(e.to_string()))?;
    api.provision_authoring_delegation(cert)
        .await
        .map_err(ProvisionDelegationError::Nest)
}

/// Make sure the account holds a live delegation carrying every capability
/// in `required` before a grant of an authoring-key class is minted
/// (`authorization-server.md` § Scope grammar → *The planes ruled
/// 2026-10-02* and *The profile arm*: approving `post.author` or
/// `profile.author` provisions the delegation when the account has none —
/// [`authoring_capabilities`] names what each class needs). The stored cert
/// is verified under the account's OWN identity key — a cert it did not sign
/// is no delegation — and a missing, foreign, lapsed one, or one lacking a
/// required capability, is replaced by [`provision_delegation`], whose cert
/// carries both. Returns whether it provisioned.
pub async fn ensure_authoring_delegation(
    api: &dyn crate::nest_api::AtprotoSettingsNestApi,
    identity: &ActorKeypair,
    required: &[fauna_core::data::Capability],
    now: fauna_core::data::Timestamp,
) -> Result<bool, ProvisionDelegationError> {
    use fauna_client_bridges::atproto_delegation::{
        DelegationLiveness, delegation_liveness, parse_delegation_cert,
    };
    let state = api
        .fetch_authoring_delegation()
        .await
        .map_err(ProvisionDelegationError::Nest)?;
    let live = state.cert.as_deref().is_some_and(|cert| {
        parse_delegation_cert(cert, &identity.actor_id()).is_ok_and(|summary| {
            required.iter().all(|c| summary.capabilities.contains(c))
                && delegation_liveness(summary.expires_at, now) != DelegationLiveness::Expired
        })
    });
    if live {
        return Ok(false);
    }
    provision_delegation(api, identity, now).await?;
    Ok(true)
}

/// The capabilities the authoring delegation must carry for the oracle
/// classes `row` names — `Post` for `post.author`, `UpdateProfile` for
/// `profile.author` (each class's custodian checks the same one per
/// request); empty when it names neither.
fn authoring_capabilities(row: &NestConsentRow) -> Vec<fauna_core::data::Capability> {
    use fauna_core::data::Capability;
    use fauna_core::identity_op::IdentityOpClass;
    let mut required = Vec::new();
    for s in &row.scopes {
        let cap = match fauna_bridge_atproto::fauna_scope::identity_op_qualifier(s) {
            Some(IdentityOpClass::PostAuthor) => Capability::Post,
            Some(IdentityOpClass::ProfileAuthor) => Capability::UpdateProfile,
            _ => continue,
        };
        if !required.contains(&cap) {
            required.push(cap);
        }
    }
    required
}

/// Whether answering `row` with an approve mints a consent-time grant — it
/// names a twin-bearing scope (`records`, `folder:read`, `folder:deposit` or
/// `personalization:rw`; a bare folder-plane scope once the card's choice
/// qualifies it). **An admin's install card never does**: its scopes
/// are the ceiling each user may later grant the plugin, and installing
/// grants nothing over anyone's data — each user's own bind card mints
/// theirs (`third-party.md` § The runner contract → *The install-approval
/// leg*). A bind card carries the install section too, flagged `bind`, and
/// mints like every consent.
pub fn names_twin(row: &NestConsentRow) -> bool {
    !is_install_card(row) && row.scopes.iter().any(|s| names_twin_scope(s))
}

/// An admin's install card: an install section that does not ride a user's
/// bind card.
fn is_install_card(row: &NestConsentRow) -> bool {
    row.install.as_ref().is_some_and(|i| !i.bind)
}

/// The owner's own name for every folder `rows`' `folder:read` scopes name,
/// through the folder seam — what the card words the read row with
/// (`fauna_scope::folder_read_card_row`). Empty without the seam; a folder
/// it cannot resolve is left out, and its row reads unnamed.
pub async fn consent_folder_names(
    seams: Option<&ConsentGrantSeams>,
    rows: &[NestConsentRow],
) -> BTreeMap<i64, String> {
    let mut names = BTreeMap::new();
    let Some(folders) = seams.and_then(|s| s.folders.as_ref()) else {
        return names;
    };
    for row in rows {
        for id in folder_read_ids(&row.scopes) {
            if names.contains_key(&id) {
                continue;
            }
            if let Some(folder) = folders.folder_for_consent(id).await {
                names.insert(id, folder.set_name);
            }
        }
    }
    names
}

/// Which of the owner's folders each of `holder`'s principal folder grants
/// covers — grant id → set name over every generation the log holds
/// (`fauna_client_capabilities::folder_principal_set_names`), read here where
/// the owner secret is held so the principal folds stay pure; the facet row
/// and history line name the folder from it
/// (`view_model::principal_grant_folder`). Empty without the folder seam or
/// when the log or the owner's set names cannot be read — every folder grant
/// then reads unnamed.
pub async fn principal_folder_names(
    seams: Option<&ConsentGrantSeams>,
    holder: &[u8; 32],
) -> BTreeMap<[u8; 16], String> {
    let Some((seams, folders)) = seams.and_then(|s| Some((s, s.folders.as_ref()?))) else {
        return BTreeMap::new();
    };
    let Some(set_names) = folders.owned_set_names().await else {
        return BTreeMap::new();
    };
    let Ok(ledger) = seams
        .ledger
        .load()
        .await
        .inspect_err(|e| tracing::warn!(target: "fauna_atproto_settings", error = %e, "principal folders: grant log unreadable"))
    else {
        return BTreeMap::new();
    };
    fauna_client_capabilities::folder_principal_set_names(
        &ledger.grant_events,
        seams.owner_keypair.secret_bytes(),
        holder,
        &set_names,
    )
}

/// Why the consent-time grant could not be minted — every arm leaves the
/// request unanswered, so the user may still decline it.
#[derive(Debug, thiserror::Error)]
pub enum ConsentGrantError {
    /// The machine was built without [`ConsentGrantSeams`], or — for a
    /// `folder:read` scope — without its folder seam.
    #[error("this app cannot grant that access yet")]
    Unwired,
    /// The shared entry point refused (manifest, kinds, keys, store, sign).
    #[error(transparent)]
    Refused(#[from] ExtConsentError),
    /// An authoring-key class's delegation could not be provisioned.
    #[error(transparent)]
    Delegation(#[from] ProvisionDelegationError),
}

/// Why [`answer_consent`] did not resolve the request.
#[derive(Debug)]
pub enum AnswerError<E> {
    /// The grant could not be prepared; nothing was deposited or resolved.
    Grant(ConsentGrantError),
    /// A nest call failed — the deposit (nothing resolved) or the resolve
    /// (the deposited grant was revoked again best-effort).
    Nest(E),
}

/// A fresh 16-byte grant id — random, never a counter (the nest keys on
/// `(owner, grant_id)`).
fn new_grant_id() -> [u8; 16] {
    use rand::RngCore;
    let mut id = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut id);
    id
}

fn now_epoch_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs().max(0) as u64
}

/// Answer `row`: for an approve naming a twin-bearing scope, prepare its
/// grants, `deposit` each in order, then `resolve`; otherwise just `resolve`.
/// `folder` is the card's choice for a row carrying a bare folder-plane
/// scope — the approve's grants plan over the row qualified by it, and
/// `resolve` is sent it; a decline sends none. Returns `resolve`'s own answer
/// (`false` = nothing live was resolved). Grants an approve began — every
/// deposited one it did not re-mint in place — are withdrawn through `revoke`
/// and signed `Revoke` events, best-effort, when a deposit fails partway or
/// the resolve does not answer `Ok(true)`.
pub async fn answer_consent<E, D, DF, R, RF, V, VF>(
    seams: Option<&ConsentGrantSeams>,
    row: &NestConsentRow,
    approved: bool,
    folder: Option<i64>,
    mut deposit: D,
    resolve: R,
    revoke: V,
) -> Result<bool, AnswerError<E>>
where
    E: std::fmt::Display,
    D: FnMut(Vec<u8>) -> DF,
    DF: Future<Output = Result<(), E>>,
    R: FnOnce(Vec<u8>, bool, Option<i64>) -> RF,
    RF: Future<Output = Result<bool, E>>,
    V: FnMut([u8; 16]) -> VF,
    VF: Future<Output = Result<(), E>>,
{
    // An install card's ceiling scopes are not the user's grant: they pass to
    // the nest as handed.
    let folder = folder.filter(|_| approved);
    let approved_row;
    let row = if approved && !is_install_card(row) {
        approved_row = NestConsentRow {
            scopes: approved_scopes(&row.scopes, folder)
                .map_err(|e| AnswerError::Grant(e.into()))?,
            ..row.clone()
        };
        &approved_row
    } else {
        row
    };
    let (seams, prepared) = if approved && names_twin(row) {
        let seams = seams.ok_or(AnswerError::Grant(ConsentGrantError::Unwired))?;
        let mut folders = BTreeMap::new();
        let folder_ids = folder_read_ids(&row.scopes);
        if !folder_ids.is_empty() {
            let seam = seams
                .folders
                .as_ref()
                .ok_or(AnswerError::Grant(ConsentGrantError::Unwired))?;
            for id in folder_ids {
                if let Some(folder) = seam.folder_for_consent(id).await {
                    folders.insert(id, folder);
                }
            }
        }
        // The posting and profile classes' custodian signs with the account's
        // delegated authoring sub-key: provision it first, so the grant is
        // never minted over a class nothing can perform (mint before resolve
        // still holds — this precedes both).
        let required = authoring_capabilities(row);
        if !required.is_empty() {
            let api = seams
                .authoring
                .as_ref()
                .ok_or(AnswerError::Grant(ConsentGrantError::Unwired))?;
            ensure_authoring_delegation(
                api.as_ref(),
                &seams.owner_keypair,
                &required,
                fauna_core::data::Timestamp::now(),
            )
            .await
            .map_err(|e| AnswerError::Grant(e.into()))?;
        }
        let owner = ExtConsentOwner {
            actor_id: seams.actor_id,
            owner_secret: seams.owner_keypair.secret_bytes(),
            delegable: &seams.delegable,
            manifests: seams.manifests.as_ref(),
            ledger: seams.ledger.as_ref(),
            signer: seams.signer.as_ref(),
        };
        let prepared = prepare_ext_consent_grant(
            &owner,
            &row.to_pending(),
            &folders,
            new_grant_id(),
            now_epoch_secs(),
        )
        .await
        .map_err(|e| AnswerError::Grant(e.into()))?;
        (Some(seams), prepared)
    } else {
        (None, Vec::new())
    };
    let mut deposited = Vec::with_capacity(prepared.len());
    for grant in &prepared {
        if let Err(e) = deposit(grant.blob_bytes.clone()).await {
            if let Some(seams) = seams {
                withdraw(seams, row, &deposited, revoke).await;
            }
            return Err(AnswerError::Nest(e));
        }
        deposited.push(grant);
    }
    let resolved = resolve(row.consent_id.clone(), approved, folder).await;
    if let Some(seams) = seams
        && !matches!(resolved, Ok(true))
    {
        withdraw(seams, row, &deposited, revoke).await;
    }
    resolved.map_err(AnswerError::Nest)
}

/// Revoke the deposited grants of a consent that did not resolve — those the
/// approve began, and only those: the records grant and every folder twin it
/// minted fresh. A twin it re-minted in place over a live generation stays
/// standing as re-minted, since the principal's earlier access is the owner's
/// earlier approve and a revoke here would end it (`webdav-server.md` § Key
/// model → *A principal's read* rule (1), *A withdrawn approve ends only what
/// it began*). The nest first (revoke narrows, so the nest leads), then one
/// signed `Revoke` per grant the nest let go of. Failures are logged, never
/// surfaced: the resolve's own answer is what the user sees, and a grant
/// left standing stays visible and revocable either way.
async fn withdraw<E, V, VF>(
    seams: &ConsentGrantSeams,
    row: &NestConsentRow,
    deposited: &[&PreparedExtGrant],
    mut revoke: V,
) where
    E: std::fmt::Display,
    V: FnMut([u8; 16]) -> VF,
    VF: Future<Output = Result<(), E>>,
{
    let Some(holder) = row
        .holder_x25519
        .as_deref()
        .and_then(|h| <[u8; 32]>::try_from(h).ok())
    else {
        return;
    };
    let mut ended = Vec::new();
    for grant in deposited.iter().filter(|g| !g.replaced) {
        match revoke(grant.grant_id).await {
            Ok(()) => ended.push((grant.grant_id, holder)),
            Err(e) => tracing::warn!(
                target: "fauna_atproto_settings",
                error = %e,
                "consent did not resolve after its grant was deposited; the nest sweeps it at expiry"
            ),
        }
    }
    if let Err(e) = grant_log::record_revokes(
        seams.ledger.as_ref(),
        seams.signer.as_ref(),
        seams.actor_id,
        &ended,
        now_epoch_secs(),
    )
    .await
    {
        tracing::warn!(
            target: "fauna_atproto_settings",
            error = %e,
            "withdrawn consent grant: the Revoke event was not recorded"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_config::test_helpers::{FakeKindManifestStore, FakeSuccessionLedgerStore};
    use fauna_core::data::{FolderKeyCustody, FoldersConfig};
    use fauna_core::folder_keys::{FolderContentKeys, serve_custody_channel_id};
    use fauna_mls::wrapped_blob::{GrantBlob, ScopeTuple};
    use fauna_protocol::folders::FolderSummary;
    use std::sync::Mutex;

    /// The owner's custody for one unshared set named `name`, its keys at the
    /// serve pseudo-channel, serve-stamped when `served`.
    fn custody(name: &str, served: bool) -> FoldersConfig {
        let mut entry = FolderKeyCustody {
            channel_id: Some(serve_custody_channel_id(name)),
            keys: Some(FolderContentKeys::genesis([0xC1; 32], 1_000)),
            name: Some(name.into()),
            ..Default::default()
        };
        if served {
            entry.serve_on(5_000);
        }
        let mut cfg = FoldersConfig::default();
        cfg.sets.push(entry);
        cfg
    }

    fn row(id: i64, name: &str) -> FolderSummary {
        FolderSummary {
            id,
            name: name.into(),
            ..Default::default()
        }
    }

    /// Served is custody's word: the stamped entry reads served, the
    /// unstamped one does not, whatever the nest's flag says; a member row
    /// and an unknown id resolve to nothing.
    #[test]
    fn the_custody_fold_reads_served_from_custody() {
        let served = consent_folder_from(vec![row(42, "photos")], custody("photos", true), 42)
            .expect("an owned row resolves");
        assert_eq!(served.set_name, "photos");
        assert!(served.served_by_custody);
        assert_eq!(
            served.custody_channel_id,
            Some(serve_custody_channel_id("photos"))
        );

        let mut flagged = row(42, "photos");
        flagged.webdav_enabled = true;
        let unserved = consent_folder_from(vec![flagged], custody("photos", false), 42).unwrap();
        assert!(
            !unserved.served_by_custody,
            "the nest's webdav_enabled selects nothing"
        );

        let mut member = row(42, "photos");
        member.role = Some("member".into());
        assert!(consent_folder_from(vec![member], custody("photos", true), 42).is_none());
        assert!(consent_folder_from(vec![row(42, "photos")], custody("photos", true), 7).is_none());
    }

    /// A seam answering "photos" (id 42) and "docs" (id 7) from fixed rows
    /// and custody.
    struct FixedFolders(FoldersConfig);

    #[async_trait::async_trait]
    impl ConsentFolderSeam for FixedFolders {
        async fn folder_for_consent(&self, id: i64) -> Option<ConsentFolder> {
            consent_folder_from(vec![row(42, "photos"), row(7, "docs")], self.0.clone(), id)
        }
    }

    #[async_trait::async_trait]
    impl fauna_client_capabilities::OwnedSetNames for FixedFolders {
        async fn owned_set_names(&self) -> Option<Vec<String>> {
            Some(fauna_client_folders::custody::owned_set_names_from(
                vec![row(42, "photos"), row(7, "docs")],
                &self.0,
            ))
        }
    }

    /// Served custody for both "photos" (id 42) and "docs" (id 7).
    fn two_served_sets() -> FoldersConfig {
        let mut cfg = custody("photos", true);
        cfg.sets.extend(custody("docs", true).sets);
        cfg
    }

    fn seams(folders: Option<FoldersConfig>) -> ConsentGrantSeams {
        let kp = ActorKeypair::generate();
        let seams = ConsentGrantSeams::from_keypair(
            &kp,
            Arc::new(FakeKindManifestStore::empty()),
            Arc::new(FakeSuccessionLedgerStore::empty(kp.actor_id())),
        );
        match folders {
            Some(cfg) => seams.with_folders(Arc::new(FixedFolders(cfg))),
            None => seams,
        }
    }

    fn consent(scopes: &[&str]) -> NestConsentRow {
        NestConsentRow {
            consent_id: vec![1; 16],
            client_id: "https://app.example/client-metadata.json".into(),
            scopes: scopes.iter().map(ToString::to_string).collect(),
            holder_x25519: Some(
                fauna_mls::wrapped_blob::generate_x25519_keypair()
                    .1
                    .to_vec(),
            ),
            ..Default::default()
        }
    }

    /// What the approve path called, in order, and every blob it deposited.
    #[derive(Default)]
    struct Calls(Mutex<Vec<String>>, Mutex<Vec<Vec<u8>>>);

    impl Calls {
        fn log(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }

        fn blobs(&self) -> Vec<GrantBlob> {
            self.1
                .lock()
                .unwrap()
                .iter()
                .map(|b| GrantBlob::from_canonical_bytes(b).unwrap())
                .collect()
        }
    }

    /// Approve `row`: the resolve answers `resolved`, and the deposit numbered
    /// `fail_deposit` (from 0), if any, fails.
    async fn answer(
        seams: Option<&ConsentGrantSeams>,
        row: &NestConsentRow,
        calls: &Calls,
        resolved: bool,
        fail_deposit: Option<usize>,
    ) -> Result<bool, AnswerError<String>> {
        answer_in(seams, row, true, None, calls, resolved, fail_deposit).await
    }

    /// Answer `row` with `approved` and the card's `folder` choice; the
    /// resolve logs the folder it was sent.
    async fn answer_in(
        seams: Option<&ConsentGrantSeams>,
        row: &NestConsentRow,
        approved: bool,
        folder: Option<i64>,
        calls: &Calls,
        resolved: bool,
        fail_deposit: Option<usize>,
    ) -> Result<bool, AnswerError<String>> {
        answer_consent(
            seams,
            row,
            approved,
            folder,
            |blob| async move {
                let n = calls.1.lock().unwrap().len();
                if fail_deposit == Some(n) {
                    calls.0.lock().unwrap().push("deposit failed".into());
                    return Err("refused".to_string());
                }
                calls.0.lock().unwrap().push("deposit".into());
                calls.1.lock().unwrap().push(blob);
                Ok(())
            },
            |_, _, folder| async move {
                calls.0.lock().unwrap().push(match folder {
                    None => "resolve".into(),
                    Some(id) => format!("resolve in {id}"),
                });
                Ok(resolved)
            },
            |grant_id| async move {
                calls
                    .0
                    .lock()
                    .unwrap()
                    .push(format!("revoke {}", hex::encode(grant_id)));
                Ok(())
            },
        )
        .await
    }

    async fn approve(
        seams: Option<&ConsentGrantSeams>,
        row: &NestConsentRow,
        calls: &Calls,
    ) -> Result<bool, AnswerError<String>> {
        answer(seams, row, calls, true, None).await
    }

    /// The `Revoke` events the seams' log holds, by grant id.
    async fn revoked(seams: &ConsentGrantSeams) -> Vec<Vec<u8>> {
        seams
            .ledger
            .load()
            .await
            .unwrap()
            .grant_events
            .into_iter()
            .filter(|e| e.kind == fauna_core::grant_event::GrantEventKind::Revoke)
            .map(|e| e.grant_id)
            .collect()
    }

    /// The production approving path deposits the folder twin BEFORE it
    /// resolves — the principal row is minted at `/oauth/token`, after the
    /// consent, with no client in the loop.
    #[tokio::test]
    async fn approving_a_folder_read_deposits_the_twin_before_resolving() {
        let seams = seams(Some(custody("photos", true)));
        let calls = Calls::default();
        let row = consent(&["fauna:folder:read:42"]);
        assert!(approve(Some(&seams), &row, &calls).await.unwrap());
        assert_eq!(calls.log(), ["deposit", "resolve"]);
        let hash = fauna_core::path_crypto::set_name_hash("photos");
        assert_eq!(calls.blobs()[0].scope, [ScopeTuple::folder_read(&hash)]);
    }

    /// A bare deposit row approved with the card's folder deposits the keyless
    /// `deposit` tuple over THAT folder before resolving, and resolves with
    /// the choice (`folder-deposit.md`;
    /// `authorization-server.md` § Consent → *The card chooses the folder*).
    #[tokio::test]
    async fn approving_a_bare_deposit_mints_over_the_chosen_folder() {
        let seams = seams(None);
        let calls = Calls::default();
        let row = consent(&["fauna:folder:deposit"]);
        assert!(
            answer_in(Some(&seams), &row, true, Some(7), &calls, true, None)
                .await
                .unwrap()
        );
        assert_eq!(calls.log(), ["deposit", "resolve in 7"]);
        let blob = &calls.blobs()[0];
        assert_eq!(blob.scope, [ScopeTuple::folder_deposit(7)]);
        assert!(blob.wrapped_keys.is_empty(), "deposit is keyless");
    }

    /// A qualified deposit row (the re-consent path) takes no choice and
    /// deposits over the folder it names.
    #[tokio::test]
    async fn approving_a_qualified_deposit_mints_over_its_folder() {
        let seams = seams(None);
        let calls = Calls::default();
        let row = consent(&["fauna:folder:deposit:9"]);
        assert!(approve(Some(&seams), &row, &calls).await.unwrap());
        assert_eq!(calls.log(), ["deposit", "resolve"]);
        assert_eq!(calls.blobs()[0].scope, [ScopeTuple::folder_deposit(9)]);
    }

    /// A bare row approved with no folder, and a qualified row approved with
    /// one, are refused before anything is minted or resolved; a decline
    /// sends no folder whatever it was handed.
    #[tokio::test]
    async fn a_mismatched_folder_choice_is_refused_before_anything() {
        let seams = seams(None);
        for (scope, folder) in [
            ("fauna:folder:deposit", None),
            ("fauna:folder:deposit:9", Some(9)),
        ] {
            let calls = Calls::default();
            let err = answer_in(
                Some(&seams),
                &consent(&[scope]),
                true,
                folder,
                &calls,
                true,
                None,
            )
            .await
            .unwrap_err();
            assert!(
                matches!(err, AnswerError::Grant(ConsentGrantError::Refused(_))),
                "{scope}"
            );
            assert!(calls.log().is_empty(), "{scope}: {:?}", calls.log());
        }
        assert!(seams.ledger.load().await.unwrap().grant_events.is_empty());

        let calls = Calls::default();
        let row = consent(&["fauna:folder:deposit"]);
        assert!(
            answer_in(Some(&seams), &row, false, Some(7), &calls, true, None)
                .await
                .unwrap()
        );
        assert_eq!(calls.log(), ["resolve"]);
    }

    /// Approving the `post.author` class on an account with no delegation
    /// runs the D10 ceremony FIRST — fetch, then provision a cert signed by
    /// the owner over the minted key, carrying `Post` — and only then
    /// deposits the keyless `identity.op` grant and resolves.
    #[tokio::test]
    async fn approving_post_author_provisions_the_delegation_before_the_grant() {
        use crate::nest_api::{FakeAtprotoSettingsNestApi, FakeCall};
        use fauna_core::identity_op::IdentityOpClass;
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let seams = seams(None).with_authoring(api.clone());
        let calls = Calls::default();
        let row = consent(&["fauna:identity:op:post.author"]);

        assert!(approve(Some(&seams), &row, &calls).await.unwrap());

        let d10: Vec<FakeCall> = api
            .calls()
            .into_iter()
            .filter(|c| {
                matches!(
                    c,
                    FakeCall::FetchAuthoringDelegation
                        | FakeCall::FetchAuthoringKey
                        | FakeCall::ProvisionAuthoringDelegation { .. }
                )
            })
            .collect();
        assert!(matches!(
            d10.as_slice(),
            [
                FakeCall::FetchAuthoringDelegation,
                FakeCall::FetchAuthoringKey,
                FakeCall::ProvisionAuthoringDelegation { .. }
            ]
        ));
        let cert = api.delegation().cert.expect("provisioned");
        let summary = fauna_client_bridges::atproto_delegation::parse_delegation_cert(
            &cert,
            &seams.owner_keypair.actor_id(),
        )
        .expect("signed by the owner");
        assert_eq!(
            summary.device_key, [0xA5; 32],
            "over the key the nest minted"
        );
        assert!(
            summary
                .capabilities
                .contains(&fauna_core::data::Capability::Post)
        );
        assert_eq!(calls.log(), ["deposit", "resolve"]);
        assert_eq!(
            calls.blobs()[0].scope,
            [fauna_client_capabilities::identity_op_scope(
                IdentityOpClass::PostAuthor
            )]
        );

        // A second consent finds the live delegation and provisions nothing.
        let again = Calls::default();
        assert!(approve(Some(&seams), &row, &again).await.unwrap());
        let provisions = api
            .calls()
            .into_iter()
            .filter(|c| matches!(c, FakeCall::ProvisionAuthoringDelegation { .. }))
            .count();
        assert_eq!(provisions, 1);
    }

    /// Without the delegation doors wired, approving the class is refused
    /// before anything is deposited or resolved — never a grant over a class
    /// nothing can perform.
    #[tokio::test]
    async fn approving_post_author_unwired_is_refused() {
        let seams = seams(None);
        let calls = Calls::default();
        let row = consent(&["fauna:identity:op:post.author"]);
        assert!(matches!(
            approve(Some(&seams), &row, &calls).await,
            Err(AnswerError::Grant(ConsentGrantError::Unwired))
        ));
        assert!(calls.log().is_empty());
    }

    /// Approving `profile.author` on an account whose delegation carries
    /// `Post` alone re-provisions it — the profile custodian checks
    /// `UpdateProfile` — and the new cert carries both; a second consent
    /// finds it live and provisions nothing.
    #[tokio::test]
    async fn approving_profile_author_provisions_an_update_profile_delegation() {
        use crate::nest_api::{AuthoringDelegationState, FakeAtprotoSettingsNestApi, FakeCall};
        use fauna_core::data::Capability;
        use fauna_core::identity_op::IdentityOpClass;
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let seams = seams(None).with_authoring(api.clone());
        let post_only = fauna_client_bridges::atproto_delegation::build_authoring_delegation_cert(
            &seams.owner_keypair,
            [0xA5; 32],
            &[Capability::Post],
            fauna_core::data::Timestamp::now(),
            None,
        )
        .unwrap();
        api.set_delegation(AuthoringDelegationState {
            k_pub: Some([0xA5; 32]),
            cert: Some(post_only),
            last_used_at: None,
        });
        let calls = Calls::default();
        let row = consent(&["fauna:identity:op:profile.author"]);

        assert!(approve(Some(&seams), &row, &calls).await.unwrap());

        let provisions = |api: &FakeAtprotoSettingsNestApi| {
            api.calls()
                .into_iter()
                .filter(|c| matches!(c, FakeCall::ProvisionAuthoringDelegation { .. }))
                .count()
        };
        assert_eq!(provisions(&api), 1, "a Post-only delegation is replaced");
        let summary = fauna_client_bridges::atproto_delegation::parse_delegation_cert(
            &api.delegation().cert.expect("provisioned"),
            &seams.owner_keypair.actor_id(),
        )
        .expect("signed by the owner");
        assert!(summary.capabilities.contains(&Capability::UpdateProfile));
        assert!(summary.capabilities.contains(&Capability::Post));
        assert_eq!(calls.log(), ["deposit", "resolve"]);
        assert_eq!(
            calls.blobs()[0].scope,
            [fauna_client_capabilities::identity_op_scope(
                IdentityOpClass::ProfileAuthor
            )]
        );

        let again = Calls::default();
        assert!(approve(Some(&seams), &row, &again).await.unwrap());
        assert_eq!(provisions(&api), 1);
    }

    /// An oracle class with no custodian prerequisite mints its keyless
    /// grant and provisions nothing.
    #[tokio::test]
    async fn approving_another_oracle_class_mints_without_a_ceremony() {
        let seams = seams(None);
        let calls = Calls::default();
        let row = consent(&["fauna:identity:op:nostr.sign_event"]);
        assert!(approve(Some(&seams), &row, &calls).await.unwrap());
        assert_eq!(calls.log(), ["deposit", "resolve"]);
    }

    /// An admin's install card names the plugin's `records` ceiling but
    /// approving it mints nothing: the install grants nothing over the
    /// admin's data, and it resolves even with no seams wired.
    #[tokio::test]
    async fn approving_an_install_card_mints_no_grant() {
        let calls = Calls::default();
        let mut row = consent(&["atproto", "fauna:records:rw:ext.plugins.example.*"]);
        row.holder_x25519 = None;
        row.install = Some(crate::NestConsentInstall {
            execution_form: "wasm".into(),
            ..Default::default()
        });
        assert!(!names_twin(&row));
        assert!(approve(None, &row, &calls).await.unwrap());
        assert_eq!(calls.log(), ["resolve"]);
    }

    /// A user's bind card carries the install section too (rebuilt from the
    /// installed rows), but it is a consent like any other: its approve mints
    /// the account's grant to the plugin's holder key before it resolves
    /// (`third-party.md` § The principal model → *Hosted principals*, the
    /// bind leg). The section's `bind` flag is what tells the two cards apart.
    #[tokio::test]
    async fn approving_a_bind_card_mints_its_grant_before_resolving() {
        let records = consent(&["atproto", "fauna:records:rw:ext.plugins.example.*"]);
        let mut bind = records.clone();
        bind.install = Some(crate::NestConsentInstall {
            execution_form: "wasm".into(),
            bind: true,
            ..Default::default()
        });
        assert!(
            names_twin(&bind),
            "a bind card's records scope plans a grant"
        );

        let seams = seams(None);
        let calls = Calls::default();
        let mut row = consent(&["fauna:identity:op:nostr.sign_event"]);
        row.install = bind.install.clone();
        assert!(approve(Some(&seams), &row, &calls).await.unwrap());
        assert_eq!(calls.log(), ["deposit", "resolve"]);
    }

    /// Two folders are two grants, each deposited in order before the
    /// resolve, each under its own set's derived id.
    #[tokio::test]
    async fn approving_two_folders_deposits_every_grant_before_resolving() {
        let seams = seams(Some(two_served_sets()));
        let calls = Calls::default();
        let row = consent(&["fauna:folder:read:42", "fauna:folder:read:7"]);
        assert!(approve(Some(&seams), &row, &calls).await.unwrap());
        assert_eq!(calls.log(), ["deposit", "deposit", "resolve"]);
        let holder: [u8; 32] = row.holder_x25519.clone().unwrap().try_into().unwrap();
        let ids: Vec<Vec<u8>> = calls.blobs().iter().map(|b| b.index.1.clone()).collect();
        let derive = |set| {
            fauna_client_capabilities::folder_principal_grant_id(
                seams.owner_keypair.secret_bytes(),
                &holder,
                set,
                0,
            )
            .to_vec()
        };
        assert_eq!(ids, [derive("docs"), derive("photos")]);
    }

    /// A deposit failing partway withdraws the grants already deposited, and
    /// nothing is resolved.
    #[tokio::test]
    async fn a_deposit_failing_partway_withdraws_what_was_deposited() {
        let seams = seams(Some(two_served_sets()));
        let calls = Calls::default();
        let row = consent(&["fauna:folder:read:42", "fauna:folder:read:7"]);
        let err = answer(Some(&seams), &row, &calls, true, Some(1))
            .await
            .unwrap_err();
        assert!(matches!(err, AnswerError::Nest(_)));
        let first = calls.blobs()[0].index.1.clone();
        assert_eq!(
            calls.log(),
            [
                "deposit".to_string(),
                "deposit failed".to_string(),
                format!("revoke {}", hex::encode(&first)),
            ]
        );
        assert_eq!(revoked(&seams).await, [first]);
    }

    /// A resolve that does not land withdraws a twin the approve minted
    /// fresh — but leaves standing one it re-minted in place over a live
    /// generation, since that access is the owner's earlier approve.
    #[tokio::test]
    async fn a_withdrawn_approve_ends_only_what_it_began() {
        let seams = seams(Some(custody("photos", true)));
        let row = consent(&["fauna:folder:read:42"]);

        let fresh = Calls::default();
        assert!(
            !answer(Some(&seams), &row, &fresh, false, None)
                .await
                .unwrap()
        );
        let fresh_id = fresh.blobs()[0].index.1.clone();
        assert_eq!(
            fresh.log(),
            [
                "deposit".to_string(),
                "resolve".to_string(),
                format!("revoke {}", hex::encode(&fresh_id)),
            ]
        );
        assert_eq!(revoked(&seams).await, std::slice::from_ref(&fresh_id));

        // The next approve mints the next generation, and lands.
        let live = Calls::default();
        assert!(approve(Some(&seams), &row, &live).await.unwrap());
        let live_id = live.blobs()[0].index.1.clone();
        assert_ne!(live_id, fresh_id, "a spent generation is never re-minted");

        // A re-approve over the live generation replaces it in place; its
        // resolve failing revokes nothing.
        let replaced = Calls::default();
        assert!(
            !answer(Some(&seams), &row, &replaced, false, None)
                .await
                .unwrap()
        );
        assert_eq!(replaced.blobs()[0].index.1, live_id);
        assert_eq!(replaced.log(), ["deposit", "resolve"]);
        assert_eq!(revoked(&seams).await, [fresh_id]);
    }

    /// No folder seam → `Unwired`, and nothing is deposited or resolved; an
    /// unserved folder refuses the same way, with the card's reason.
    #[tokio::test]
    async fn a_folder_read_is_refused_unwired_or_unserved() {
        let row = consent(&["fauna:folder:read:42"]);
        let calls = Calls::default();
        let err = approve(Some(&seams(None)), &row, &calls).await.unwrap_err();
        assert!(matches!(
            err,
            AnswerError::Grant(ConsentGrantError::Unwired)
        ));

        let err = approve(Some(&seams(Some(custody("photos", false)))), &row, &calls)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AnswerError::Grant(ConsentGrantError::Refused(
                ExtConsentError::FolderNotServed(_)
            ))
        ));
        assert!(calls.0.lock().unwrap().is_empty());
    }

    /// The card names the folder the seam resolves, and words an unresolved
    /// one generically.
    #[tokio::test]
    async fn the_card_names_the_folder_through_the_seam() {
        let row = consent(&["fauna:folder:read:42"]);
        let seams = seams(Some(custody("photos", true)));
        let names = consent_folder_names(Some(&seams), std::slice::from_ref(&row)).await;
        assert_eq!(names.get(&42).map(String::as_str), Some("photos"));
        let card = crate::machine::consent_card_row(row.clone(), &names);
        assert!(card.scope_descriptions[0].contains("\"photos\""));

        assert!(
            consent_folder_names(None, std::slice::from_ref(&row))
                .await
                .is_empty()
        );
        let card = crate::machine::consent_card_row(row, &BTreeMap::new());
        assert!(card.scope_descriptions[0].contains("one of your folders"));
    }

    /// The seam lists the owner's own sets — a member row is never one — and
    /// the principal's folder grant the approve minted resolves to its set
    /// through them, the id → name map the principal row's facet names it by.
    #[tokio::test]
    async fn a_principals_folder_grant_resolves_to_its_set_through_the_seam() {
        let mut member = row(9, "shared");
        member.role = Some("member".into());
        assert_eq!(
            fauna_client_folders::custody::owned_set_names_from(
                vec![row(42, "photos"), member],
                &custody("photos", true)
            ),
            ["photos"]
        );

        let seams = seams(Some(two_served_sets()));
        let row = consent(&["fauna:folder:read:42"]);
        assert!(
            approve(Some(&seams), &row, &Calls::default())
                .await
                .unwrap()
        );
        let holder: [u8; 32] = row.holder_x25519.clone().unwrap().try_into().unwrap();
        let names = principal_folder_names(Some(&seams), &holder).await;
        let id = fauna_client_capabilities::folder_principal_grant_id(
            seams.owner_keypair.secret_bytes(),
            &holder,
            "photos",
            0,
        );
        assert_eq!(names.get(&id).map(String::as_str), Some("photos"));
        assert_eq!(names.len(), 1, "docs holds no grant to this holder");
        assert!(principal_folder_names(None, &holder).await.is_empty());
    }
}
