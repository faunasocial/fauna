//! The nest-backed **shared-folder content-key resolver** — the
//! [`FolderKeyResolver`] every custody-holding surface consults so a shared set
//! opens under content keys from custody rather than the owner `BackupKey`.
//!
//! Split out of the `mls`-gated `custody_ingest` seam because nothing here
//! touches MLS: it reads the member-visible roster (`fauna.folders.list`) and
//! the caller's own custody (a [`FolderKeyReader`]) and decides. Gated on
//! `nest-resolver` alone (which `mls` turns on), so a consumer that must not
//! link the MLS graph — the shipped sync agent, bearer-only — still renders
//! sealed set names (`docs/goal/behavior/file-sync.md` § Sealed names & paths).

use std::sync::Arc;

use async_trait::async_trait;
use fauna_core::folder_keys::{FolderKeyResolver, FolderRef, ResolvedCustody, ResolvedFolderKeys};
use fauna_protocol::RpcRequester;

use crate::FoldersClient;
use crate::custody;
use crate::key_reader::FolderKeyReader;

/// The nest-backed **shared-folder content-key resolver** (Phase 0 — the read
/// leg; `folders.md` § Sharing): the read counterpart every custody-holding
/// surface consults so a **shared** set opens under content keys from custody
/// rather than the owner `BackupKey`. It resolves a set **name** → its bound
/// `mls_group_id` (from the member-visible roster) + the caller's own
/// content-key custody (through a [`FolderKeyReader`]), returning `None` for an
/// owner-only (unbound) set.
///
/// Two classes of consumer, one resolver: the Media machine's **byte** download
/// (`MediaMachine::download_file`) and the **sealed-label** renders on media
/// list / snapshot browse+diff / conflicts, which reach it through
/// [`fauna_core::label_custody::LabelCustody`]. That sharing is the ruling's own
/// logic — whoever can open a set's bytes renders its names
/// (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
///
/// Sibling of [`NestFolderCustodySink`] (same `FoldersClient` + custody
/// pattern), kept a distinct type because it serves a different seam
/// ([`FolderKeyResolver`] on the Media machine, not `FolderCustodySink` on the
/// conversations session) and resolves in the opposite direction (name → keys,
/// not channel → envelope). Built by the client's Media glue.
pub struct NestFolderKeyResolver<R: RpcRequester> {
    folders: FoldersClient<R>,
    custody: Arc<dyn FolderKeyReader>,
}

impl<R: RpcRequester + Clone> NestFolderKeyResolver<R> {
    /// `nest` backs the roster read (`fauna.folders.list`); `custody` reads the
    /// caller's own content-key custody — the seat's `PlaneFolderKeys` (web:
    /// the account port's forwarder).
    pub fn new(nest: R, custody: Arc<dyn FolderKeyReader>) -> Self {
        Self {
            folders: FoldersClient::new(nest),
            custody,
        }
    }
}

/// The pure custody decision: one roster read + one custody read → the
/// three-valued [`FolderKeyResolver`] answer. Split from the async shell so
/// the contract is unit-testable without a nest.
///
/// **A roster row addressed by `name_hash` — owned or member — is an authoritative
/// identity answer, and the foreign lookup is only reachable when NO such row
/// exists.** The previous shape collapsed "an owned row exists and is
/// UNBOUND" (a positive answer: the owner key IS its custody) with "no row at
/// all" into one miss and fell through to the foreign lookup
/// ([`custody::find_foreign_set_by_hash`] today)
/// — so a foreign record whose freely-chosen display name collided with the
/// user's own unbound set (`photos`, `documents`, …) resolved the user's own
/// set to a **stranger's roster content root**: labels sealed to the wrong
/// audience, chunk fetches routed to the stranger's nest, and the S9 scrub
/// primed to destroy plaintext under a seal nobody could open.
///
/// Corollary (deliberate): an owned/member row
/// **shadows** a same-named foreign record — the foreign set is unaddressable
/// by this name until the collision dissolves. A reader holding the set's row
/// addresses it by identity instead — [`resolve_custody_by_ref`].
pub(crate) fn resolve_custody_from(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &fauna_core::data::FoldersConfig,
    name_hash: &[u8; 32],
) -> anyhow::Result<ResolvedCustody> {
    if let Some(row) = custody::find_named_roster_row(roster, cfg, name_hash) {
        return custody_of_roster_row(&row, cfg);
    }
    // No roster row — a FOREIGN (cross-nest) set resolves from the member's
    // own custody record instead (written at accept time; Phase 2 client
    // read-side): identity + the home-nest base URL the byte fetch must use. A
    // name-less foreign record (an unopenable seal) is unaddressable by
    // name and stays owner-only — it resolves by channel through
    // [`resolve_custody_by_ref`].
    let Some(foreign) = custody::find_foreign_set_by_hash(cfg, name_hash) else {
        return Ok(ResolvedCustody::owner_only());
    };
    Ok(custody_of_foreign_set(foreign, cfg))
}

/// The custody of a set **by its identity** — the
/// [`FolderKeyResolver::resolve_ref`] twin of [`resolve_custody_from`]
/// (`mls-group-key-material.md` § M2 → *Resolving custody — by the set's
/// identity, never by its name*). [`FolderRef::Local`] is the roster row with
/// that `id` — no owner-first search, so a member row is reachable beside the
/// reader's own same-named set — and [`FolderRef::Foreign`] the live foreign
/// record at that channel. A ref naming neither is `Err`: a caller holding a
/// set's identity asked about THAT set, and "no such set" is not the positive
/// owner-only answer a by-name miss is. Everything after the pick is
/// [`resolve_custody_from`]'s own arm, shared.
pub(crate) fn resolve_custody_by_ref(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &fauna_core::data::FoldersConfig,
    folder_ref: &FolderRef,
) -> anyhow::Result<ResolvedCustody> {
    match folder_ref {
        FolderRef::Local(id) => {
            // (No set name or id in the message — S7.)
            let row = roster
                .iter()
                .find(|s| s.id == *id)
                .ok_or_else(|| anyhow::anyhow!("no roster row with that id"))?;
            custody_of_roster_row(&custody::named_roster_row(row.clone(), cfg), cfg)
        }
        FolderRef::Foreign(channel) => custody::find_foreign_set(cfg, channel)
            .map(|foreign| custody_of_foreign_set(foreign, cfg))
            .ok_or_else(|| anyhow::anyhow!("no live foreign set at that channel")),
        // A group set's keys are its scope's generation ring
        // (`fauna_account_plane::group_set_keys`), never an M2 custody entry.
        FolderRef::Group(_) => Err(anyhow::anyhow!(
            "a group set has no folder custody — its keys are the scope's generation ring"
        )),
    }
}

/// The custody answer for a roster row already picked and named from custody
/// ([`custody::named_roster_row`]) — the arm both resolution directions share.
fn custody_of_roster_row(
    row: &fauna_protocol::folders::FolderSummary,
    cfg: &fauna_core::data::FoldersConfig,
) -> anyhow::Result<ResolvedCustody> {
    // The custody identity through the ONE shared resolution every
    // consumer uses (`webdav-server.md` § Key model, the custody note):
    // the derived `ChannelId` for a shared set, the serve pseudo-channel
    // for a served-unshared one, `None` for a plain owner-only set. A
    // bound row whose group id cannot be decoded is corrupt state, not
    // "unbound": answering owner-only would license an owner-root seal
    // for a set that IS bound. (No set name in the message — S7.)
    let channel = crate::engine_binding::custody_channel_for(row, cfg)
        .map_err(|_| anyhow::anyhow!("bound roster row carries an undecodable mls_group_id"))?;
    let Some(channel) = channel else {
        // Owned, unbound, unserved: positively owner-only — plus, for a
        // set a since-unflagged serve window once sealed, that window's
        // generations as READ candidates (`retired_serve_custody`, the
        // read-side twin of the engine's `retired_content_keys`).
        return Ok(ResolvedCustody::OwnerOnly {
            retired_content_keys: crate::engine_binding::retired_serve_custody(cfg, row),
        });
    };
    // The raw group id rides only when there IS a group: a served-unshared
    // set is content-keyed with no group, and never fakes one.
    let mls_group_id = row
        .mls_group_id
        .as_deref()
        .map(hex::decode)
        .transpose()
        .map_err(|_| anyhow::anyhow!("bound roster row carries an undecodable mls_group_id"))?;
    // Custody: the caller's content-key generations (member custody
    // ingested at join/rotation, or the owner's own — for a served set, the
    // serve-enable genesis at the pseudo-channel). Absent →
    // content-keyed-but-unresolvable: the content-keyed-ness still rides,
    // so seals fail closed and opens fail on their own — never the
    // owner-key path (`EngineKeyBinding::{BoundKeysMissing,ServedKeysMissing}`'s
    // read-side twins).
    Ok(ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
        content_keys: custody::content_keys(cfg, &channel),
        mls_group_id,
        home_nest_url: None,
        home_nest_actor_id: None,
    }))
}

/// The custody answer for a live foreign record already picked — the arm both
/// resolution directions share.
fn custody_of_foreign_set(
    foreign: &fauna_core::data::ForeignFolder,
    cfg: &fauna_core::data::FoldersConfig,
) -> ResolvedCustody {
    ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
        mls_group_id: Some(foreign.mls_group_id.clone()),
        content_keys: custody::content_keys(cfg, &foreign.channel_id),
        home_nest_url: Some(foreign.home_nest_url.clone()),
        // The grant-delivered trust root for the byte-plane dial
        // (`refresh_foreign_set_from_reply` keeps it current from every
        // federated read reply).
        home_nest_actor_id: foreign.home_nest_actor_id.clone(),
    })
}

/// The set nonce for `folder` by the same name resolution
/// [`resolve_custody_from`] uses: a roster row resolves by its custody channel
/// (a member's received copy or the owner's keyed entry), else — for the
/// caller's OWN row — the owner's live pick by name; a foreign record by its
/// channel.
pub(crate) fn resolve_set_nonce_from(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &fauna_core::data::FoldersConfig,
    folder: &str,
) -> Option<[u8; 32]> {
    custody::set_nonce_by_name(roster, cfg, folder)
}

/// [`resolve_set_nonce_from`] by the set's identity
/// ([`FolderKeyResolver::set_nonce_ref`]): the roster row with that id —
/// never the owner-first name search, so a member row answers its own nonce
/// beside the caller's same-named set — or the foreign record at a channel.
pub(crate) fn resolve_set_nonce_by_ref(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &fauna_core::data::FoldersConfig,
    folder_ref: &FolderRef,
) -> Option<[u8; 32]> {
    match folder_ref {
        FolderRef::Local(id) => {
            let row = roster.iter().find(|s| s.id == *id)?;
            custody::set_nonce_of_row(&custody::named_roster_row(row.clone(), cfg), cfg)
        }
        FolderRef::Foreign(channel) => custody::find_foreign_set(cfg, channel)
            .and_then(|f| custody::set_nonce_for_channel(cfg, &f.channel_id)),
        // No set nonce on the group family: a group set's rows are plane
        // entries bound to the scope by the plane's own writer signature.
        FolderRef::Group(_) => None,
    }
}

// Same shared-logic-once / per-target-trait-impl split as
// `NestFolderCustodySink` above (see that module doc for why).
impl<R: RpcRequester + Clone> NestFolderKeyResolver<R> {
    /// The [`FolderKeyResolver::read_scope`] read: the roster and custody,
    /// once, pinned into a [`PinnedFolderKeyResolver`] every per-set answer of
    /// one page read comes from. Either read failing is `None` — the caller
    /// keeps this resolver, whose per-call reads keep the must-succeed rule.
    pub(crate) async fn do_read_scope(&self) -> Option<Arc<dyn FolderKeyResolver>> {
        let reply = match self.folders.list_owned_and_shared_wire().await {
            Ok(reply) => reply,
            Err(e) => {
                tracing::debug!("read scope: roster read failed; resolving per call: {e}");
                return None;
            }
        };
        let cfg = match self.custody.load().await {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::debug!("read scope: custody read failed; resolving per call: {e:#}");
                return None;
            }
        };
        Some(Arc::new(PinnedFolderKeyResolver {
            roster: reply.folders,
            cfg,
        }))
    }

    pub(crate) async fn do_set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving a set nonce: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving a set nonce"))?;
        Ok(resolve_set_nonce_from(&reply.folders, &cfg, folder))
    }

    pub(crate) async fn do_set_nonce_ref(
        &self,
        folder_ref: &FolderRef,
    ) -> anyhow::Result<Option<[u8; 32]>> {
        // The same two reads as `do_set_nonce`, with the same must-succeed rule.
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving a set nonce: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving a set nonce"))?;
        Ok(resolve_set_nonce_by_ref(&reply.folders, &cfg, folder_ref))
    }

    pub(crate) async fn do_set_lineage(
        &self,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        // The wire rows, matched by hash: this resolver's `FoldersClient`
        // holds no label custody, so the rendered list drops every sealed set.
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving a set lineage: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving a set lineage"))?;
        Ok(custody::set_lineage_by_hash(
            &reply.folders,
            &cfg,
            name_hash,
        ))
    }

    pub(crate) async fn do_set_lineage_ref(
        &self,
        folder_ref: &FolderRef,
    ) -> anyhow::Result<Option<fauna_core::folder_keys::SetNonceLineage>> {
        // The same two reads as `do_set_lineage`, with the same must-succeed rule.
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving a set lineage: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving a set lineage"))?;
        Ok(custody::set_lineage_by_ref(
            &reply.folders,
            &cfg,
            folder_ref,
        ))
    }

    pub(crate) async fn do_foreign_sets(
        &self,
    ) -> anyhow::Result<Vec<fauna_core::data::ForeignFolder>> {
        // Custody alone — a foreign set has no roster row on this nest.
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed listing foreign sets"))?;
        Ok(custody::live_foreign_sets(&cfg).cloned().collect())
    }

    pub(crate) async fn do_resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        // Both reads must SUCCEED for any answer at all — a transport failure
        // is `Err` ("could not determine"), never `Ok(None)`: the old
        // `.ok()?`-to-`None` collapse read a transient roster/config failure
        // as "unbound", and every seal site downstream took that as license
        // for the owner root. (Messages carry no set name — S7.)
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving set custody: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving set custody"))?;
        resolve_custody_from(&reply.folders, &cfg, name_hash)
    }

    pub(crate) async fn do_resolve_ref(
        &self,
        folder_ref: &FolderRef,
    ) -> anyhow::Result<ResolvedCustody> {
        // The same two reads as `do_resolve`, with the same must-succeed rule.
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving set custody: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving set custody"))?;
        resolve_custody_by_ref(&reply.folders, &cfg, folder_ref)
    }
}

/// One page read's custody view ([`FolderKeyResolver::read_scope`]): the
/// roster and custody [`NestFolderKeyResolver`] read once, answered from by the
/// same pure decisions its per-call methods make — so a scope answers exactly
/// what the resolver would have answered at the moment it was made, without
/// one roster read per set. Reads only, and for one read only (the trait
/// method's ⚠).
pub struct PinnedFolderKeyResolver {
    pub(crate) roster: Vec<fauna_protocol::folders::FolderSummary>,
    pub(crate) cfg: fauna_core::data::FoldersConfig,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl FolderKeyResolver for PinnedFolderKeyResolver {
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        resolve_custody_from(&self.roster, &self.cfg, name_hash)
    }

    async fn resolve_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<ResolvedCustody> {
        resolve_custody_by_ref(&self.roster, &self.cfg, folder_ref)
    }

    async fn set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        Ok(resolve_set_nonce_from(&self.roster, &self.cfg, folder))
    }

    async fn set_nonce_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<Option<[u8; 32]>> {
        Ok(resolve_set_nonce_by_ref(
            &self.roster,
            &self.cfg,
            folder_ref,
        ))
    }

    async fn set_lineage(
        &self,
        _folder: &str,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        Ok(custody::set_lineage_by_hash(
            &self.roster,
            &self.cfg,
            name_hash,
        ))
    }

    async fn set_lineage_ref(
        &self,
        folder_ref: &FolderRef,
    ) -> anyhow::Result<Option<fauna_core::folder_keys::SetNonceLineage>> {
        Ok(custody::set_lineage_by_ref(
            &self.roster,
            &self.cfg,
            folder_ref,
        ))
    }

    async fn foreign_sets(&self) -> anyhow::Result<Vec<fauna_core::data::ForeignFolder>> {
        Ok(custody::live_foreign_sets(&self.cfg).cloned().collect())
    }

    // `read_scope` keeps its `None` default: a scope is already one read.
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl FolderKeyResolver for NestFolderKeyResolver<Arc<fauna_client::NestClient>> {
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve(name_hash).await
    }

    async fn resolve_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve_ref(folder_ref).await
    }

    async fn set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce(folder).await
    }

    async fn set_nonce_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce_ref(folder_ref).await
    }

    async fn set_lineage(
        &self,
        _folder: &str,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        self.do_set_lineage(name_hash).await
    }

    async fn set_lineage_ref(
        &self,
        folder_ref: &FolderRef,
    ) -> anyhow::Result<Option<fauna_core::folder_keys::SetNonceLineage>> {
        self.do_set_lineage_ref(folder_ref).await
    }

    async fn read_scope(&self) -> Option<Arc<dyn FolderKeyResolver>> {
        self.do_read_scope().await
    }

    async fn foreign_sets(&self) -> anyhow::Result<Vec<fauna_core::data::ForeignFolder>> {
        self.do_foreign_sets().await
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl FolderKeyResolver for NestFolderKeyResolver<fauna_rpc_wasm::WsRpcClient> {
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve(name_hash).await
    }

    async fn resolve_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve_ref(folder_ref).await
    }

    async fn set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce(folder).await
    }

    async fn set_nonce_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce_ref(folder_ref).await
    }

    async fn set_lineage(
        &self,
        _folder: &str,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        self.do_set_lineage(name_hash).await
    }

    async fn set_lineage_ref(
        &self,
        folder_ref: &FolderRef,
    ) -> anyhow::Result<Option<fauna_core::folder_keys::SetNonceLineage>> {
        self.do_set_lineage_ref(folder_ref).await
    }

    async fn read_scope(&self) -> Option<Arc<dyn FolderKeyResolver>> {
        self.do_read_scope().await
    }

    async fn foreign_sets(&self) -> anyhow::Result<Vec<fauna_core::data::ForeignFolder>> {
        self.do_foreign_sets().await
    }
}
