//! The **one create and one delete** every production set lifecycle gesture
//! routes through — the custody-first half of the set nonce
//! (`docs/goal/architecture/mls-group-key-material.md` § M2 → *Writer-signed
//! change records* → *Custody shape of the set nonce*, rulings (d) and (e)).
//!
//! Every writer-signed change record binds its set by a client-minted 32-byte
//! nonce, so the nonce must exist in the owner's custody (the account plane's
//! `fauna.state.folder-keys`, through [`FolderKeyStore`]) **before**
//! the nest ever sees the set: [`create_set`] mints it, writes the live custody
//! entry, and only then sends `fauna.folders.create` carrying it. The nest keeps
//! its copy opaque and never selects which nonce a set verifies under.
//! [`delete_set`] is the twin: it retires every live entry for the name (the
//! standing delete intent the owner's reconcile re-drives), then sends
//! `fauna.folders.delete`.
//!
//! **What a failure leaves behind.** The helpers split a failed nest call by
//! whether the nest *answered* ([`RpcErrorClass::is_rejection`]):
//!
//! - A **refusal** (a name conflict at create; the non-cascading delete refused
//!   while snapshots remain) is definitive: the set's state did not move, so the
//!   custody write is rolled back — the create's added entry is retired, the
//!   delete's retirements are lifted.
//! - A **transport fault** is ambiguous — the nest may have applied the call —
//!   so the custody write stands. A live entry for a create that never landed
//!   selects nothing the nest lists; a tombstone for a delete that never landed
//!   is the delete intent the reconcile re-issues. Rolling either back on a
//!   fault could turn a landed create into a reconcile-driven delete.
//!
//! Transport-generic like every seam in this crate: native `Arc<NestClient>`,
//! wasm `WsRpcClient`.

use fauna_core::data::{FoldersConfig, Timestamp};
use fauna_protocol::folders::{FolderCreateReply, FolderCreateRequest, FolderDeleteReply};
use fauna_protocol::{ByteBuf, RpcErrorClass, RpcRequester};

use crate::FoldersClient;
use crate::custody;
use crate::key_reader::{FolderKeyStore, update};

/// A set lifecycle gesture that did not complete.
#[derive(Debug)]
pub enum SetLifecycleError<E> {
    /// The custody read or write that must precede the nest call failed; the
    /// nest was never asked.
    Custody(anyhow::Error),
    /// The nest call failed (its own error, refusal or fault).
    Nest(E),
}

impl<E: core::fmt::Display> core::fmt::Display for SetLifecycleError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Custody(e) => write!(f, "folder custody write failed: {e:#}"),
            Self::Nest(e) => write!(f, "{e}"),
        }
    }
}

impl<E> SetLifecycleError<E> {
    /// The nest's own error, when the nest call was the one that failed —
    /// callers that classify a refusal (a name conflict, a delete refused while
    /// snapshots remain) read it from here.
    pub fn nest_error(&self) -> Option<&E> {
        match self {
            Self::Nest(e) => Some(e),
            Self::Custody(_) => None,
        }
    }
}

/// Create a set with its nonce minted into custody first (ruling (d)): mint 32
/// random bytes → join the live custody entry into the store → `fauna.folders.create`
/// with `set_nonce` set → on a nest refusal retire the added entry.
///
/// **Sealed from birth.** The same request carries the set's `name_hash` and,
/// when `files` holds an owner key ([`FoldersClient::with_label_custody`]), its
/// `name_sealed` and `retention_policy_sealed` — both salted by the name's own
/// hash, so derivable before the row exists ([`seal_at_create`]).
///
/// `req.set_nonce`, `req.name_hash`, `req.name_sealed` and
/// `req.retention_policy_sealed` are overwritten — they are this helper's to
/// mint.
pub async fn create_set<R>(
    files: &FoldersClient<R>,
    custody_store: &dyn FolderKeyStore,
    req: FolderCreateRequest,
) -> Result<FolderCreateReply, SetLifecycleError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let owner = files.label_custody().owner_key();
    create_set_with_owner_root(files, custody_store, req, owner).await
}

/// [`create_set`] with the owner's seal root named explicitly — for a seam
/// whose `FoldersClient` deliberately carries no label custody (the folder
/// wizard: an owner-only custody on a client that could later `update` a
/// bound set would seal that set's labels under the wrong root). A new set is
/// owner-only, so the owner key is the whole of what a create seals under.
pub async fn create_set_with_owner_root<R>(
    files: &FoldersClient<R>,
    custody_store: &dyn FolderKeyStore,
    mut req: FolderCreateRequest,
    owner: Option<fauna_core::crypto::BackupKey>,
) -> Result<FolderCreateReply, SetLifecycleError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let nonce: [u8; 32] = *fauna_core::secret::fresh_secret_32();
    let name = req.name.clone();
    // The creating identity is the nonce's minter (ruling (11)(a)); a store
    // that cannot name it writes none, and the owner's reconcile re-mints.
    let minted_by = custody_store.minting_identity().await.ok().flatten();
    update(custody_store, |cfg| {
        custody::record_created_set(cfg, &name, nonce, minted_by, Timestamp::now().0);
    })
    .await
    .map_err(SetLifecycleError::Custody)?;
    req.set_nonce = Some(ByteBuf::from(nonce.to_vec()));
    seal_at_create(owner, &mut req);
    match files.create(req).await {
        Ok(reply) => Ok(reply),
        Err(e) => {
            if e.is_rejection() {
                // Best-effort: a failed rollback leaves a live entry for a set
                // the nest never listed, which selects nothing.
                let _ = update(custody_store, |cfg| {
                    custody::retire_set_nonce(cfg, &nonce, Timestamp::now().0)
                })
                .await;
            }
            Err(SetLifecycleError::Nest(e))
        }
    }
}

/// Stamp the create's name-salted carriers: `name_hash` always, and — under the
/// owner's root, since a brand-new set is owner-only (no content-key generation
/// exists yet) — `name_sealed` and, when a policy rides the request,
/// `retention_policy_sealed`.
///
/// Best-effort like every seal seam: a custody-less client or a derivation
/// failure leaves the seals `None`, and the update-time backfill stamps them
/// later. The path lists are deliberately absent — they seal under the row id
/// the nest mints at INSERT, so they arrive on the first keyed update.
fn seal_at_create(owner: Option<fauna_core::crypto::BackupKey>, req: &mut FolderCreateRequest) {
    req.name_hash = Some(ByteBuf::from(
        fauna_core::path_crypto::set_name_hash(&req.name).to_vec(),
    ));
    let root = owner.and_then(|key| {
        fauna_core::file_download::FileDownloadKeys::owner(key)
            .label_seal_root()
            .ok()
            .flatten()
    });
    let Some(root) = root else {
        req.name_sealed = None;
        req.retention_policy_sealed = None;
        return;
    };
    req.name_sealed = fauna_core::label_custody::seal_set_name(&root, &req.name)
        .ok()
        .flatten()
        .map(ByteBuf::from);
    req.retention_policy_sealed = req.retention_policy.as_deref().and_then(|policy| {
        fauna_core::label_custody::seal_retention_policy(&root, &req.name, policy)
            .ok()
            .map(ByteBuf::from)
    });
}

/// Delete a set, retiring its custody first (ruling (e)): retire every live
/// entry for `name` → `fauna.folders.delete` → on a nest refusal lift the
/// retirements this call made (a lift stamp, ruling (l)(v)).
pub async fn delete_set<R>(
    files: &FoldersClient<R>,
    custody_store: &dyn FolderKeyStore,
    name: &str,
) -> Result<FolderDeleteReply, SetLifecycleError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let now = Timestamp::now().0;
    let (_, retired) = update(custody_store, |cfg| retire_live_entries(cfg, name, now))
        .await
        .map_err(SetLifecycleError::Custody)?;
    match files.delete(name).await {
        Ok(reply) => Ok(reply),
        Err(e) => {
            if e.is_rejection() && !retired.is_empty() {
                let _ = update(custody_store, |cfg| {
                    custody::unretire_set_nonces(cfg, &retired, now)
                })
                .await;
            }
            Err(SetLifecycleError::Nest(e))
        }
    }
}

/// The set nonce a change record into `folder` binds to, read fresh — the
/// member-visible roster plus the holder's custody
/// ([`custody::set_nonce_by_name`]). For a client that records into a set it
/// holds no engine for (a version restore, an archive import, the re-seed
/// delivery): resolve just before the record, then sign with a fixed nonce.
/// `Ok(None)`: custody holds none for the set — the record goes out unsigned,
/// and the nest refuses it `signature_required`.
pub async fn record_nonce<R>(
    files: &FoldersClient<R>,
    custody_store: &dyn crate::key_reader::FolderKeyReader,
    folder: &str,
) -> Result<Option<[u8; 32]>, SetLifecycleError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let roster = files
        .list_owned_and_shared_wire()
        .await
        .map_err(SetLifecycleError::Nest)?;
    let cfg = custody_store
        .load()
        .await
        .map_err(SetLifecycleError::Custody)?;
    Ok(custody::set_nonce_by_name(&roster.folders, &cfg, folder))
}

/// What [`prepare_reseed_targets`] found or did for one restored folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReseedTargetPrep {
    /// Created now through [`create_set`] — custody first, nonce minted.
    Created,
    /// Re-created now under the live nonce custody already holds for the name
    /// — the lost set's own, so the target IS that set re-homed and custody
    /// keeps one entry for it (see [`prepare_reseed_targets`]).
    Recreated,
    /// Already prepared: the nest lists an owned set by the name and custody
    /// holds a live nonce for it — an earlier attempt's target (or a fresh
    /// folder the owner created by hand), never a second create.
    Resumed,
    /// The nest lists the name but custody holds no live nonce for it: left
    /// as it is — the owner's set-custody reconcile mints one, and the
    /// materialize arm refuses a nonce-less target until then.
    ListedWithoutNonce,
    /// The create did not land; the reason. The ceremony still runs, and the
    /// arm refuses this folder `target_missing`.
    Failed(String),
}

/// The re-seed ceremony's **target pre-create** (`writer-signed-change-
/// records.md` ruling (7)(a)(i)): before the ceremony materializes, the
/// seed-holding process creates each delivered covered folder's target under
/// its display name through [`create_set`], so the set has a custody entry at
/// least as old as itself and a nonce the delivery leg signs under — the nest
/// never creates one inside materialize.
///
/// **A name custody still holds live is the same set, re-homed.** After a box
/// loss the owner's custody keeps the lost set's live entry (nothing deleted
/// it), so the target is re-created at the nest under THAT nonce
/// ([`ReseedTargetPrep::Recreated`]) rather than a second one minted: two live
/// entries for one name resolve to the earliest (custody (c)'s pick), so a
/// fresh mint would leave the device signing under the lost set's nonce while
/// the nest holds the new one, and every re-homed row would be refused
/// `signature_invalid`. Only a name with no live entry — never created here,
/// or deleted, its entry retired — mints through [`create_set`].
///
/// `names` are the display names the custodian's store learned for its
/// covered-folder sets (a set it never learned a name for is not here: the
/// driver reports it `folder_unnamed`). Idempotent across retries: an owned
/// set the nest lists with a live custody nonce is [`ReseedTargetPrep::Resumed`].
/// Whether a resumed target is *fresh* is not decided here — the arm's
/// freshness rule and torn-run classifier decide that, inside its transaction.
pub async fn prepare_reseed_targets<R>(
    files: &FoldersClient<R>,
    custody_store: &dyn FolderKeyStore,
    names: &[String],
) -> Result<Vec<(String, ReseedTargetPrep)>, SetLifecycleError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let listed = files
        .list_wire()
        .await
        .map_err(SetLifecycleError::Nest)?
        .folders;
    let cfg = custody_store
        .load()
        .await
        .map_err(SetLifecycleError::Custody)?;
    let mut out: Vec<(String, ReseedTargetPrep)> = Vec::with_capacity(names.len());
    for name in names {
        if out.iter().any(|(done, _)| done == name) {
            continue;
        }
        // By address: a sealed set's listed row rests no plaintext name.
        let name_hash = fauna_core::path_crypto::set_name_hash(name);
        let owned_listed = listed.iter().any(|s| {
            s.role.as_deref() != Some("member")
                && fauna_core::label_custody::set_name_label_salt(
                    s.name_hash.as_deref().map(|b| &b[..]),
                    &s.name,
                ) == name_hash
        });
        let live = custody::live_set_nonce(&cfg, name);
        let prep = match (owned_listed, live) {
            (true, Some(_)) => ReseedTargetPrep::Resumed,
            (true, None) => ReseedTargetPrep::ListedWithoutNonce,
            (false, Some(nonce)) => {
                // Sealed from birth like every keyed create, so the re-created
                // target rests no plaintext name either.
                let mut req = FolderCreateRequest {
                    name: name.clone(),
                    set_nonce: Some(ByteBuf::from(nonce.to_vec())),
                    ..Default::default()
                };
                seal_at_create(files.label_custody().owner_key(), &mut req);
                match files.create(req).await {
                    Ok(_) => ReseedTargetPrep::Recreated,
                    Err(e) => ReseedTargetPrep::Failed(e.to_string()),
                }
            }
            (false, None) => {
                let req = FolderCreateRequest {
                    name: name.clone(),
                    ..Default::default()
                };
                match create_set(files, custody_store, req).await {
                    Ok(_) => ReseedTargetPrep::Created,
                    Err(e) => ReseedTargetPrep::Failed(e.to_string()),
                }
            }
        };
        out.push((name.clone(), prep));
    }
    Ok(out)
}

/// [`prepare_reseed_targets`] as every host runs it ahead of the ceremony: each
/// target's preparation logged, a failure to prepare logged and never fatal.
/// The ceremony runs regardless — a folder whose target could not be prepared
/// is the driver's typed per-set answer (`target_missing`, `rehome_unsigned`),
/// which the owner reads on the result, so stopping here would only hide it.
/// The phone host calls it before its in-process `run_reseed`, every desktop
/// shell through `fauna_client_sync::reseed_wire::await_agent_reseed`.
pub async fn prepare_reseed_targets_logged<R>(
    files: &FoldersClient<R>,
    custody_store: &dyn FolderKeyStore,
    names: &[String],
) where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    if names.is_empty() {
        return;
    }
    match prepare_reseed_targets(files, custody_store, names).await {
        Ok(prepared) => {
            for (name, prep) in prepared {
                tracing::info!(folder = %name, ?prep, "re-seed: target set");
            }
        }
        Err(e) => tracing::warn!("re-seed: preparing the target sets: {e}"),
    }
}

/// What one [`reconcile_set_custody`] pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SetCustodyReconcile {
    /// Listed sets whose every custody entry is retired — the delete re-issued.
    pub redeleted: usize,
    /// Listed sets with no nonce-bearing entry — a nonce minted (or a
    /// nonce-less entry repaired in place).
    pub minted: usize,
    /// Surplus live same-name entries retired in favour of the pick.
    pub retired_duplicates: usize,
    /// Sets whose nest echo differed from the pick — the pick pushed.
    pub echoes_pushed: usize,
    /// Owned sets whose live nonce another identity minted (or none recorded)
    /// — re-minted under the current identity (the succession cut,
    /// `writer-signed-change-records.md` ruling (11)(a)).
    pub reminted: usize,
    /// Re-minted sets whose adoption marker could not be written — left
    /// un-cut for the next pass (the marker comes first, always).
    pub remint_deferred: usize,
}

/// The owner's launch-time **set-custody reconcile** (ruling (g)).
///
/// **First, over custody — never over the nest's listing — the succession
/// cut** (`writer-signed-change-records.md` ruling (11)(a), [`remint_owned_sets`]):
/// every live owned entry whose nonce `identity` did not mint is re-minted, its
/// adoption marker written first, and the new pick pushed to the nest whether
/// or not the nest lists the set. Then, per set the owner-scoped
/// `fauna.folders.list` returns:
///
/// - every entry for the name retired → re-issue `fauna.folders.delete` (the
///   standing delete intent: a crash between the retire and the nest call, or a
///   nest that withheld it, gets the delete again — never a re-mint);
/// - no nonce-bearing entry, and the nest's echo carries none either → mint
///   one (a nonce-less entry keyed at the set's custody channel is repaired in
///   place) — a path that never fires after the baseline, present so no
///   client-reachable state is unrecoverable;
/// - no nonce-bearing entry but a nest echo → nothing: custody is behind (a
///   fresh device the plane has not yet carried the minting device's row),
///   never minted over;
/// - several live entries → retire all but the pick
///   ([`custody::live_set_by_name`]);
/// - the nest's echo ≠ the pick → push the pick (`FolderUpdateRequest::set_nonce`).
///
/// The listing's custody edits land in ONE store write before any nest call.
/// The engine's re-record leg (re-signing this device's rows onto the pick) is
/// not here — it reads the retired nonces the engine keys carry; nor is the
/// envelope re-publish of a bound set, which the author converges after this
/// pass (`FoldersAuthor::converge_envelopes`).
pub async fn reconcile_set_custody<R>(
    files: &FoldersClient<R>,
    custody_store: &dyn FolderKeyStore,
    identity: fauna_core::identity::ActorId,
) -> Result<SetCustodyReconcile, SetLifecycleError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let mut report = SetCustodyReconcile::default();
    let pushed = remint_owned_sets(files, custody_store, identity, &mut report).await?;
    // Unrendered: the author's client holds no label custody, and since schema
    // 114 a sealed set's row rests no plaintext name. The name every step below
    // keys custody by is recovered from custody itself, by the row's hash; a
    // sealed row custody holds no entry for is left alone this pass (nothing
    // to reconcile against — the same as a row the rendered list omitted).
    let listed = files
        .list_wire()
        .await
        .map_err(SetLifecycleError::Nest)?
        .folders;
    let held = custody_store
        .load()
        .await
        .map_err(SetLifecycleError::Custody)?;
    let owned: Vec<_> = custody::named_from_custody(
        listed
            .into_iter()
            .filter(|s| s.role.as_deref() != Some("member") && !s.name.starts_with("__"))
            .collect(),
        &held,
    );
    let now = Timestamp::now().0;
    let mut redelete: Vec<String> = Vec::new();
    let (cfg, ()) = update(custody_store, |cfg| {
        for summary in &owned {
            let named: Vec<_> = cfg
                .sets
                .iter()
                .filter(|s| s.name.as_deref() == Some(summary.name.as_str()))
                .collect();
            let any_live = named.iter().any(|s| s.is_live());
            if !named.is_empty() && !any_live {
                redelete.push(summary.name.clone());
                continue;
            }
            if custody::live_set_nonce(cfg, &summary.name).is_none() {
                // The nest holds a nonce custody does not: only a custody
                // holder of this owner pushes one, so this replica is BEHIND —
                // a fresh device whose plane walk has not carried it the row
                // yet. Minting would push a second nonce over the nest's, and
                // the synced entry, being older, would win the pick: signer
                // and nest disagree. Left alone until the row lands.
                if summary.set_nonce.is_some() {
                    continue;
                }
                let nonce: [u8; 32] = *fauna_core::secret::fresh_secret_32();
                repair_or_mint(cfg, summary, nonce, identity, now);
                report.minted += 1;
            }
            let pick = custody::live_set_nonce(cfg, &summary.name);
            for s in cfg.sets.iter_mut() {
                // A nonce-less live entry is a nonce-less key holder (`key_named_set`'s no-create-entry shape), never
                // a competing binding — left alone.
                if s.is_live()
                    && s.name.as_deref() == Some(summary.name.as_str())
                    && s.set_nonce.is_some()
                    && s.set_nonce != pick
                {
                    s.retire(now);
                    // The pick's edge (ruling (11)(b)): what places this
                    // loser in the lineage of THIS incarnation.
                    s.retired_by_pick = pick;
                    report.retired_duplicates += 1;
                }
            }
        }
    })
    .await
    .map_err(SetLifecycleError::Custody)?;

    for name in redelete {
        // Idempotent: the nest refusing (already gone, snapshots remain) is
        // logged by the caller's report, never fatal to the pass.
        if files.delete(name.clone()).await.is_ok() {
            report.redeleted += 1;
        }
    }
    for summary in &owned {
        let Some(pick) = custody::live_set_nonce(&cfg, &summary.name) else {
            continue;
        };
        if summary.set_nonce.as_ref().map(|b| b.as_slice()) == Some(&pick[..])
            || pushed.contains(&(summary.name.clone(), pick))
        {
            continue;
        }
        files
            .update(fauna_protocol::folders::FolderUpdateRequest {
                name: summary.name.clone(),
                set_nonce: Some(ByteBuf::from(pick.to_vec())),
                ..Default::default()
            })
            .await
            .map_err(SetLifecycleError::Nest)?;
        report.echoes_pushed += 1;
    }
    Ok(report)
}

/// **The succession cut's custody arm** (`writer-signed-change-records.md`
/// ruling (11)(a)): *an owned set's live nonce is one the current identity
/// minted.* Over custody's live **owned** entries — those carrying a `name`; a
/// member's received copy carries none — whatever the nest lists, every entry
/// whose `minted_by` is not `identity` (a predecessor's, or none recorded) is
/// re-minted:
///
/// 1. the device-local **adoption marker** naming the old nonce is written
///    ([`FolderKeyStore::record_adoption_marker`]) — a store that cannot write
///    one leaves the set un-cut for the next pass;
/// 2. ONE custody update retires the entry — a re-mint's retirement, never
///    lifted — and adds a live entry under 32 fresh bytes with the same
///    channel, keys and name, `minted_by = identity`, `replaces = the old
///    nonce` (load, join, write — two devices re-minting at once leave two
///    live entries that the pick settles, the loser a sibling in the lineage);
/// 3. the new pick is pushed to the nest (`FolderUpdateRequest::set_nonce`).
///
/// Returns the `(name, pick)` pairs it pushed.
/// The envelope of a bound set is re-published after it, by the author's
/// convergence pass; records come last. Idempotent and crash-safe by what
/// custody is: a crash after the write leaves the push to the listing arm's
/// echo check and the re-publish to the convergence pass; one before it
/// leaves the set un-cut, the marker standing.
pub async fn remint_owned_sets<R>(
    files: &FoldersClient<R>,
    custody_store: &dyn FolderKeyStore,
    identity: fauna_core::identity::ActorId,
    report: &mut SetCustodyReconcile,
) -> Result<Vec<(String, [u8; 32])>, SetLifecycleError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let cfg = custody_store
        .load_for_write()
        .await
        .map_err(SetLifecycleError::Custody)?;
    let mut marked: Vec<[u8; 32]> = Vec::new();
    for old in custody::uncut_owned_nonces(&cfg, &identity) {
        match custody_store.record_adoption_marker(old).await {
            Ok(()) => marked.push(old),
            Err(e) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "folders: the succession cut's adoption marker could not be written; \
                     the set stays un-cut until the next pass"
                );
                report.remint_deferred += 1;
            }
        }
    }
    if marked.is_empty() {
        return Ok(Vec::new());
    }
    let now = Timestamp::now().0;
    let (cfg, reminted) = update(custody_store, |cfg| {
        custody::remint_owned_entries(cfg, &identity, &marked, now, || {
            *fauna_core::secret::fresh_secret_32()
        })
    })
    .await
    .map_err(SetLifecycleError::Custody)?;
    report.reminted += reminted.len();
    let mut pushed = Vec::new();
    for name in reminted {
        let Some(pick) = custody::live_set_nonce(&cfg, &name) else {
            continue;
        };
        files
            .update(fauna_protocol::folders::FolderUpdateRequest {
                name: name.clone(),
                set_nonce: Some(ByteBuf::from(pick.to_vec())),
                ..Default::default()
            })
            .await
            .map_err(SetLifecycleError::Nest)?;
        report.echoes_pushed += 1;
        pushed.push((name, pick));
    }
    Ok(pushed)
}

/// [`remint_owned_sets`] as the succession aftermath's post-store-ready pass
/// runs it (`fauna_client_config::FolderCustodyCut`, the seam the recovery
/// crate declares): the folders client and the account's custody store a host
/// already holds, cut as the identity the pass serves.
// The fields are read only by `do_cut`, whose two callers (the `FolderCustodyCut`
// impls below) are `mls`-gated; the struct itself stays ungated for the hosts
// that build it without `mls`, so the unread fields are allowed there.
#[cfg_attr(not(feature = "mls"), allow(dead_code))]
pub struct SetCustodyCut<R: RpcRequester> {
    files: FoldersClient<R>,
    custody: std::sync::Arc<dyn FolderKeyStore>,
}

impl<R: RpcRequester> SetCustodyCut<R> {
    pub fn new(files: FoldersClient<R>, custody: std::sync::Arc<dyn FolderKeyStore>) -> Self {
        Self { files, custody }
    }
}

#[cfg(feature = "mls")]
impl<R: RpcRequester> SetCustodyCut<R>
where
    R::Error: RpcErrorClass + core::fmt::Display,
{
    async fn do_cut(
        &self,
        identity: fauna_core::identity::ActorId,
    ) -> Result<usize, fauna_client_config::StoreError> {
        let mut report = SetCustodyReconcile::default();
        remint_owned_sets(&self.files, &*self.custody, identity, &mut report)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(e.to_string()))?;
        Ok(report.reminted)
    }
}

// One impl per transport, as every `Nest*` seam in this crate: the shared
// logic once (`do_cut`), the `Send`-ness each transport's futures carry.
#[cfg(all(feature = "mls", not(target_arch = "wasm32")))]
#[async_trait::async_trait]
impl fauna_client_config::FolderCustodyCut
    for SetCustodyCut<std::sync::Arc<fauna_client::NestClient>>
{
    async fn cut(
        &self,
        identity: fauna_core::identity::ActorId,
    ) -> Result<usize, fauna_client_config::StoreError> {
        self.do_cut(identity).await
    }
}

#[cfg(all(feature = "mls", target_arch = "wasm32"))]
#[async_trait::async_trait(?Send)]
impl fauna_client_config::FolderCustodyCut for SetCustodyCut<fauna_rpc_wasm::WsRpcClient> {
    async fn cut(
        &self,
        identity: fauna_core::identity::ActorId,
    ) -> Result<usize, fauna_client_config::StoreError> {
        self.do_cut(identity).await
    }
}

/// Give a listed set with no nonce-bearing entry its nonce: in place on a
/// nonce-less entry already keyed at the set's custody channel (`key_named_set`'s
/// no-create-entry branch), else a fresh live entry.
fn repair_or_mint(
    cfg: &mut FoldersConfig,
    summary: &fauna_protocol::folders::FolderSummary,
    nonce: [u8; 32],
    identity: fauna_core::identity::ActorId,
    now_micros: u64,
) {
    // Where a keyed entry rests whether or not the set is served.
    let channel = crate::engine_binding::owned_custody_channel(summary).ok();
    if let Some(entry) = cfg.sets.iter_mut().find(|s| {
        s.is_live() && s.set_nonce.is_none() && channel.is_some() && s.channel_id == channel
    }) {
        entry.set_nonce = Some(nonce);
        entry.name = Some(summary.name.clone());
        entry.minted_by = Some(identity);
        if entry.created_at == 0 {
            entry.created_at = now_micros;
        }
        return;
    }
    custody::record_created_set(cfg, &summary.name, nonce, Some(identity), now_micros);
}

/// [`custody::retire_set`], reporting the nonces it retired so a refused delete
/// can lift exactly those.
fn retire_live_entries(cfg: &mut FoldersConfig, name: &str, now_micros: u64) -> Vec<[u8; 32]> {
    let nonces: Vec<[u8; 32]> = cfg
        .sets
        .iter()
        .filter(|s| s.is_live() && s.name.as_deref() == Some(name))
        .filter_map(|s| s.set_nonce)
        .collect();
    custody::retire_set(cfg, name, now_micros);
    nonces
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_reader::MemoryFolderKeyStore;
    use fauna_client_testkit::block_on;
    use fauna_protocol::RpcError;
    use fauna_protocol::folders::{
        FolderSummary, FolderUpdateReply, FolderUpdateRequest, FoldersListReply,
        KIND_FOLDERS_CREATE, KIND_FOLDERS_DELETE, KIND_FOLDERS_LIST, KIND_FOLDERS_UPDATE,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    enum FakeErr {
        Fault,
        Rejected(RpcError),
    }
    impl core::fmt::Display for FakeErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl RpcErrorClass for FakeErr {
        fn is_rejection(&self) -> bool {
            matches!(self, Self::Rejected(_))
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            match self {
                Self::Rejected(e) => Some(e),
                Self::Fault => None,
            }
        }
    }

    enum Outcome {
        Ok,
        Refuse,
        Fault,
    }

    /// `(kind, the create request's nonce, the custody at call time)`.
    type RecordedCall = (&'static str, Option<Vec<u8>>, FoldersConfig);

    /// A folders arm that records every call in order, together with the
    /// custody the store held at that moment.
    struct FakeNest {
        custody: Arc<MemoryFolderKeyStore>,
        outcome: Mutex<Outcome>,
        calls: Mutex<Vec<RecordedCall>>,
        /// What `fauna.folders.list` answers.
        listed: Mutex<Vec<FolderSummary>>,
        /// Recorded `fauna.folders.update` requests.
        updates: Mutex<Vec<FolderUpdateRequest>>,
        /// Recorded `fauna.folders.create` requests.
        creates: Mutex<Vec<FolderCreateRequest>>,
    }

    impl FakeNest {
        fn stored_config(&self) -> FoldersConfig {
            self.custody.snapshot()
        }
    }

    impl RpcRequester for FakeNest {
        type Error = FakeErr;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, FakeErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode");
            if kind == KIND_FOLDERS_LIST {
                let reply = FoldersListReply {
                    folders: self.listed.lock().unwrap().clone(),
                    ..Default::default()
                };
                let out = fauna_protocol::encode_canonical(&reply).unwrap();
                return Ok(fauna_protocol::decode_strict(&out).expect("decode"));
            }
            if kind == KIND_FOLDERS_UPDATE {
                let req: FolderUpdateRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                self.updates.lock().unwrap().push(req);
                let out = fauna_protocol::encode_canonical(&FolderUpdateReply {
                    ok: true,
                    extra: Default::default(),
                })
                .unwrap();
                return Ok(fauna_protocol::decode_strict(&out).expect("decode"));
            }
            let nonce = (kind == KIND_FOLDERS_CREATE).then(|| {
                let req: FolderCreateRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                self.creates.lock().unwrap().push(req.clone());
                req.set_nonce.map(|b| b.into_vec()).unwrap_or_default()
            });
            self.calls
                .lock()
                .unwrap()
                .push((kind, nonce, self.stored_config()));
            match *self.outcome.lock().unwrap() {
                Outcome::Refuse => Err(FakeErr::Rejected(RpcError::new(
                    "fauna.folders.conflict",
                    "error.folders.conflict",
                ))),
                Outcome::Fault => Err(FakeErr::Fault),
                Outcome::Ok => {
                    let out = match kind {
                        KIND_FOLDERS_CREATE => {
                            fauna_protocol::encode_canonical(&FolderCreateReply {
                                id: 1,
                                name: "docs".into(),
                                retention_policy: None,
                                extra: Default::default(),
                            })
                        }
                        KIND_FOLDERS_DELETE => {
                            fauna_protocol::encode_canonical(&FolderDeleteReply {
                                ok: true,
                                extra: Default::default(),
                            })
                        }
                        other => panic!("unexpected kind {other}"),
                    }
                    .unwrap();
                    Ok(fauna_protocol::decode_strict(&out).expect("decode"))
                }
            }
        }
    }

    fn harness(
        outcome: Outcome,
    ) -> (
        Arc<FakeNest>,
        FoldersClient<Arc<FakeNest>>,
        Arc<MemoryFolderKeyStore>,
    ) {
        let custody = Arc::new(MemoryFolderKeyStore::default());
        let nest = Arc::new(FakeNest {
            custody: Arc::clone(&custody),
            outcome: Mutex::new(outcome),
            calls: Mutex::new(Vec::new()),
            listed: Mutex::new(Vec::new()),
            updates: Mutex::new(Vec::new()),
            creates: Mutex::new(Vec::new()),
        });
        let files = FoldersClient::new(nest.clone());
        (nest, files, custody)
    }

    /// A set is sealed from birth: the keyed
    /// create carries `name_hash`, and `name_sealed` + `retention_policy_sealed`
    /// that open under the owner's root with that hash as salt — so the seal
    /// backfill finds nothing to stamp on a fresh set.
    #[test]
    fn a_keyed_create_carries_the_name_hash_and_both_owner_root_seals() {
        let (nest, files, config) = harness(Outcome::Ok);
        let owner = fauna_core::crypto::BackupKey::from_bytes([7u8; 32]);
        let files = files.with_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
            owner.clone(),
        ));
        let policy = r#"{"max_snapshots":7,"max_age_days":30}"#;
        let req = FolderCreateRequest {
            retention_policy: Some(policy.into()),
            ..create_req("Family photos")
        };
        block_on(create_set(&files, &*config, req)).unwrap();

        let sent = nest.creates.lock().unwrap()[0].clone();
        let hash = fauna_core::path_crypto::set_name_hash("Family photos");
        assert_eq!(sent.name_hash.as_deref().map(|b| &b[..]), Some(&hash[..]));
        assert!(sent.name.is_empty(), "a sealed create leaves by hash alone");
        let keys = fauna_core::file_download::FileDownloadKeys::owner(owner);
        assert_eq!(
            fauna_core::label_custody::render_set_name(
                &keys,
                sent.name_sealed.as_deref().map(|b| &b[..]),
                "",
                sent.name_hash.as_deref().map(|b| &b[..]),
            ),
            fauna_core::path_crypto::SealedLabelRender::Sealed("Family photos".into()),
            "the name opens under the owner root without its plaintext"
        );
        assert!(sent.retention_policy_sealed.is_some());
        assert_eq!(
            fauna_core::label_custody::render_retention_policy(
                &keys,
                sent.retention_policy_sealed.as_deref().map(|b| &b[..]),
                None,
                "Family photos",
                sent.name_hash.as_deref().map(|b| &b[..]),
            )
            .as_deref(),
            Some(policy),
        );
    }

    /// A custody-less client still sends the address, and no seal: the
    /// update-time backfill stamps it later. No policy → no policy seal.
    #[test]
    fn a_keyless_create_sends_the_name_hash_and_no_seal() {
        let (nest, files, config) = harness(Outcome::Ok);
        block_on(create_set(&files, &*config, create_req("docs"))).unwrap();
        let sent = nest.creates.lock().unwrap()[0].clone();
        assert_eq!(
            sent.name_hash.as_deref().map(|b| &b[..]),
            Some(&fauna_core::path_crypto::set_name_hash("docs")[..])
        );
        assert_eq!(sent.name_sealed, None);
        assert_eq!(sent.retention_policy_sealed, None);
    }

    fn create_req(name: &str) -> FolderCreateRequest {
        FolderCreateRequest {
            name: name.into(),
            ..Default::default()
        }
    }

    /// The re-seed pre-create creates each missing target through the shared
    /// create helper (custody first), resumes one already prepared, and never
    /// creates a name twice (ruling (7)(a)(i)).
    #[test]
    fn reseed_targets_are_created_once_and_resumed_after() {
        let (nest, files, config) = harness(Outcome::Ok);
        let names = vec!["docs".to_string(), "docs".to_string()];
        let first = block_on(prepare_reseed_targets(&files, &*config, &names)).unwrap();
        assert_eq!(first, vec![("docs".to_string(), ReseedTargetPrep::Created)]);
        let calls = nest.calls.lock().unwrap().len();
        assert_eq!(calls, 1, "one create for a name named twice");
        let nonce = custody::live_set_nonce(&nest.stored_config(), "docs").expect("custody");

        // The retry: the nest now lists the set, custody holds its nonce.
        *nest.listed.lock().unwrap() = vec![listed("docs", Some(nonce))];
        let again = block_on(prepare_reseed_targets(&files, &*config, &names)).unwrap();
        assert_eq!(again, vec![("docs".to_string(), ReseedTargetPrep::Resumed)]);
        assert_eq!(
            nest.calls.lock().unwrap().len(),
            calls,
            "never a second create"
        );
    }

    /// After a box loss the owner's custody still holds the lost set's live
    /// entry under the name. The target is that set re-homed: re-created at the
    /// nest under the SAME nonce, never a second mint — a second live entry
    /// would lose the earliest-wins pick to the lost set's, so the device would
    /// sign under a nonce the nest does not hold (`signature_invalid`).
    #[test]
    fn a_lost_sets_live_nonce_is_reused_for_its_reseed_target() {
        let (nest, files, config) = harness(Outcome::Ok);
        block_on(update(&*config, |cfg| {
            custody::record_created_set(cfg, "docs", [0x42; 32], None, 7);
        }))
        .unwrap();
        let names = vec!["docs".to_string()];
        let out = block_on(prepare_reseed_targets(&files, &*config, &names)).unwrap();
        assert_eq!(out, vec![("docs".to_string(), ReseedTargetPrep::Recreated)]);
        let calls = nest.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (kind, sent, _) = &calls[0];
        assert_eq!(*kind, KIND_FOLDERS_CREATE);
        assert_eq!(
            sent.as_deref(),
            Some(&[0x42u8; 32][..]),
            "the lost set's nonce"
        );
        let cfg = nest.stored_config();
        assert_eq!(
            cfg.sets.iter().filter(|s| s.is_live()).count(),
            1,
            "no second custody entry: {cfg:?}"
        );
    }

    /// A re-created target is sealed from birth like every keyed create, so it
    /// rests no plaintext name; and once it rests blank, the retry still finds
    /// it by its address and resumes rather than creating it twice.
    #[test]
    fn a_recreated_reseed_target_is_sealed_and_found_again_by_its_hash() {
        let (nest, files, config) = harness(Outcome::Ok);
        let owner = fauna_core::crypto::BackupKey::from_bytes([7u8; 32]);
        let files = files.with_label_custody(fauna_core::label_custody::LabelCustody::owner_only(
            owner.clone(),
        ));
        block_on(update(&*config, |cfg| {
            custody::record_created_set(cfg, "docs", [0x42; 32], None, 7);
        }))
        .unwrap();
        let names = vec!["docs".to_string()];
        let out = block_on(prepare_reseed_targets(&files, &*config, &names)).unwrap();
        assert_eq!(out, vec![("docs".to_string(), ReseedTargetPrep::Recreated)]);
        let sent = nest.creates.lock().unwrap()[0].clone();
        let hash = fauna_core::path_crypto::set_name_hash("docs");
        assert_eq!(sent.name_hash.as_deref().map(|b| &b[..]), Some(&hash[..]));
        assert_eq!(
            fauna_core::label_custody::render_set_name(
                &fauna_core::file_download::FileDownloadKeys::owner(owner),
                sent.name_sealed.as_deref().map(|b| &b[..]),
                "",
                Some(&hash[..]),
            ),
            fauna_core::path_crypto::SealedLabelRender::Sealed("docs".into()),
        );

        // The retry against the scrubbed row: no plaintext, only the address
        // and the seal the create sent.
        let mut row = listed("", Some([0x42; 32]));
        row.name_hash = Some(ByteBuf::from(hash.to_vec()));
        row.name_sealed = sent.name_sealed.clone();
        *nest.listed.lock().unwrap() = vec![row];
        let creates = nest.creates.lock().unwrap().len();
        let again = block_on(prepare_reseed_targets(&files, &*config, &names)).unwrap();
        assert_eq!(again, vec![("docs".to_string(), ReseedTargetPrep::Resumed)]);
        assert_eq!(
            nest.creates.lock().unwrap().len(),
            creates,
            "never a second create"
        );
    }

    /// A refused create is reported, not fatal; a listed set without a custody
    /// nonce is left for the reconcile.
    #[test]
    fn reseed_target_refusals_and_nonceless_sets_are_reported() {
        let (nest, files, config) = harness(Outcome::Refuse);
        *nest.listed.lock().unwrap() = vec![listed("hand-made", None)];
        let names = vec!["docs".to_string(), "hand-made".to_string()];
        let out = block_on(prepare_reseed_targets(&files, &*config, &names)).unwrap();
        assert!(matches!(&out[0], (n, ReseedTargetPrep::Failed(_)) if n == "docs"));
        assert_eq!(
            out[1],
            (
                "hand-made".to_string(),
                ReseedTargetPrep::ListedWithoutNonce
            )
        );
    }

    #[test]
    fn create_writes_custody_before_the_nest_create_and_sends_that_nonce() {
        let (nest, files, config) = harness(Outcome::Ok);
        block_on(create_set(&files, &*config, create_req("docs"))).expect("creates");
        let calls = nest.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (kind, sent, at_call) = &calls[0];
        assert_eq!(*kind, KIND_FOLDERS_CREATE);
        let held = custody::live_set_nonce(at_call, "docs").expect("custody precedes the create");
        assert_eq!(
            sent.as_deref(),
            Some(&held[..]),
            "the nest gets the custody nonce"
        );
    }

    #[test]
    fn a_refused_create_retires_the_added_entry() {
        let (nest, files, config) = harness(Outcome::Refuse);
        assert!(block_on(create_set(&files, &*config, create_req("docs"))).is_err());
        let cfg = nest.stored_config();
        assert_eq!(cfg.sets.len(), 1);
        assert!(!cfg.sets[0].is_live(), "rolled back");
        assert_eq!(custody::live_set_nonce(&cfg, "docs"), None);
    }

    #[test]
    fn a_faulted_create_leaves_the_entry_live() {
        let (nest, files, config) = harness(Outcome::Fault);
        assert!(block_on(create_set(&files, &*config, create_req("docs"))).is_err());
        let cfg = nest.stored_config();
        assert!(
            custody::live_set_nonce(&cfg, "docs").is_some(),
            "the nest may have created it — never roll back on ambiguity"
        );
    }

    #[test]
    fn delete_retires_custody_before_the_nest_delete() {
        let (nest, files, config) = harness(Outcome::Ok);
        block_on(create_set(&files, &*config, create_req("docs"))).unwrap();
        block_on(delete_set(&files, &*config, "docs")).expect("deletes");
        let calls = nest.calls.lock().unwrap();
        let (kind, _, at_call) = &calls[1];
        assert_eq!(*kind, KIND_FOLDERS_DELETE);
        assert_eq!(
            custody::live_set_nonce(at_call, "docs"),
            None,
            "retired before the nest delete"
        );
        assert_eq!(at_call.sets.len(), 1, "a tombstone, never a removal");
    }

    #[test]
    fn a_refused_delete_lifts_the_retirement_and_a_faulted_one_keeps_it() {
        let (nest, files, config) = harness(Outcome::Ok);
        block_on(create_set(&files, &*config, create_req("docs"))).unwrap();
        let nonce = custody::live_set_nonce(&nest.stored_config(), "docs").unwrap();

        *nest.outcome.lock().unwrap() = Outcome::Refuse;
        assert!(block_on(delete_set(&files, &*config, "docs")).is_err());
        assert_eq!(
            custody::live_set_nonce(&nest.stored_config(), "docs"),
            Some(nonce),
            "the set still exists — the nonce stays live"
        );

        *nest.outcome.lock().unwrap() = Outcome::Fault;
        assert!(block_on(delete_set(&files, &*config, "docs")).is_err());
        assert_eq!(
            custody::live_set_nonce(&nest.stored_config(), "docs"),
            None,
            "an ambiguous delete stands as the delete intent"
        );
    }

    fn listed(name: &str, echo: Option<[u8; 32]>) -> FolderSummary {
        FolderSummary {
            id: 1,
            name: name.into(),
            role: Some("owner".into()),
            set_nonce: echo.map(|n| ByteBuf::from(n.to_vec())),
            ..Default::default()
        }
    }

    /// The identity the reconciling owner runs as.
    const OWN: fauna_core::identity::ActorId = fauna_core::identity::ActorId([0xA1; 32]);

    fn seed(config: &MemoryFolderKeyStore, edit: impl FnOnce(&mut FoldersConfig)) {
        block_on(update(config, edit)).expect("seed custody");
    }

    #[test]
    fn reconcile_redeletes_an_all_retired_listed_set_exactly_once() {
        let (nest, files, config) = harness(Outcome::Ok);
        seed(&config, |cfg| {
            custody::record_created_set(cfg, "docs", [1; 32], Some(OWN), 10);
            custody::retire_set(cfg, "docs", 20);
        });
        *nest.listed.lock().unwrap() = vec![listed("docs", Some([1; 32]))];
        let report = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(report.redeleted, 1);
        assert_eq!(report.minted, 0, "never a re-mint");
        let kinds: Vec<_> = nest.calls.lock().unwrap().iter().map(|c| c.0).collect();
        assert_eq!(kinds, vec![KIND_FOLDERS_DELETE]);
        assert!(nest.updates.lock().unwrap().is_empty());
    }

    #[test]
    fn reconcile_pushes_a_differing_echo_once_and_never_changes_custody() {
        let (nest, files, config) = harness(Outcome::Ok);
        seed(&config, |cfg| {
            custody::record_created_set(cfg, "docs", [1; 32], Some(OWN), 10);
        });
        let before = nest.stored_config();
        *nest.listed.lock().unwrap() = vec![listed("docs", Some([9; 32]))];
        let report = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(report.echoes_pushed, 1);
        let updates = nest.updates.lock().unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates[0].set_nonce.as_ref().map(|b| b.to_vec()),
            Some(vec![1; 32])
        );
        assert_eq!(nest.stored_config(), before, "the echo selects nothing");
    }

    #[test]
    fn reconcile_leaves_a_matching_echo_alone() {
        let (nest, files, config) = harness(Outcome::Ok);
        seed(&config, |cfg| {
            custody::record_created_set(cfg, "docs", [1; 32], Some(OWN), 10);
        });
        *nest.listed.lock().unwrap() = vec![listed("docs", Some([1; 32]))];
        let report = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(report, SetCustodyReconcile::default());
        assert!(nest.updates.lock().unwrap().is_empty());
    }

    #[test]
    fn reconcile_retires_all_but_the_pick_of_several_live_entries() {
        let (nest, files, config) = harness(Outcome::Ok);
        seed(&config, |cfg| {
            custody::record_created_set(cfg, "docs", [2; 32], Some(OWN), 20);
            custody::record_created_set(cfg, "docs", [1; 32], Some(OWN), 10);
        });
        *nest.listed.lock().unwrap() = vec![listed("docs", Some([1; 32]))];
        let report = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(report.retired_duplicates, 1);
        let cfg = nest.stored_config();
        assert_eq!(custody::live_set_nonce(&cfg, "docs"), Some([1; 32]));
        assert_eq!(
            cfg.sets.iter().filter(|s| s.is_live()).count(),
            1,
            "the race loser is retired, never removed"
        );
    }

    // ── Ruling (11)(a): the cut's custody arm ──────────────────────────────

    const PRED: fauna_core::identity::ActorId = fauna_core::identity::ActorId([0xA0; 32]);

    /// An owned set whose live nonce a predecessor minted is re-minted by one
    /// pass — the adoption marker first, then custody, then the nest's copy —
    /// and left alone by the next.
    #[test]
    fn reconcile_remints_a_predecessor_minted_set_once_marker_first() {
        let (nest, files, config) = harness(Outcome::Ok);
        seed(&config, |cfg| {
            custody::record_created_set(cfg, "docs", [1; 32], Some(PRED), 10);
        });
        *nest.listed.lock().unwrap() = vec![listed("docs", Some([1; 32]))];
        let report = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(report.reminted, 1);
        let cfg = nest.stored_config();
        let live = custody::live_set_by_name(&cfg, "docs").expect("live");
        assert_eq!(live.minted_by, Some(OWN));
        assert_eq!(live.replaces, Some([1; 32]));
        let fresh = live.set_nonce.expect("nonce");
        assert_ne!(fresh, [1; 32]);
        assert_eq!(
            block_on(crate::key_reader::FolderKeyReader::adoption_markers(
                &*config
            ))
            .unwrap(),
            vec![[1; 32]],
            "the marker names the nonce the cut replaced"
        );
        let updates = nest.updates.lock().unwrap().clone();
        assert_eq!(
            updates
                .iter()
                .map(|u| u.set_nonce.as_ref().map(|b| b.to_vec()))
                .collect::<Vec<_>>(),
            vec![Some(fresh.to_vec())],
            "the new pick pushed once"
        );
        *nest.listed.lock().unwrap() = vec![listed("docs", Some(fresh))];
        let again = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(again, SetCustodyReconcile::default(), "idempotent");
    }

    /// The cut runs over custody, never the nest's listing: a set the nest
    /// hides is cut and pushed all the same.
    #[test]
    fn reconcile_cuts_a_set_the_nest_does_not_list() {
        let (nest, files, config) = harness(Outcome::Ok);
        seed(&config, |cfg| {
            custody::record_created_set(cfg, "hidden", [1; 32], None, 10);
        });
        let report = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(report.reminted, 1);
        let updates = nest.updates.lock().unwrap();
        assert_eq!(updates.len(), 1);
        // Addressed by hash: the push rests no plaintext name.
        assert_eq!(
            updates[0].name_hash.as_deref().map(|b| b.to_vec()),
            Some(fauna_core::path_crypto::set_name_hash("hidden").to_vec())
        );
    }

    /// A store that keeps no device-local marker leaves the set un-cut — the
    /// marker comes first, always.
    #[test]
    fn reconcile_defers_the_cut_when_no_marker_can_be_written() {
        struct NoMarkers(MemoryFolderKeyStore);
        #[async_trait::async_trait]
        impl crate::key_reader::FolderKeyReader for NoMarkers {
            async fn load(&self) -> anyhow::Result<FoldersConfig> {
                self.0.load().await
            }
        }
        #[async_trait::async_trait]
        impl FolderKeyStore for NoMarkers {
            async fn merge(&self, replica: FoldersConfig) -> anyhow::Result<FoldersConfig> {
                self.0.merge(replica).await
            }
            async fn settle_removal(
                &self,
                removal: fauna_core::data::FolderPendingRemoval,
            ) -> anyhow::Result<FoldersConfig> {
                self.0.settle_removal(removal).await
            }
        }
        let (nest, files, _config) = harness(Outcome::Ok);
        let store = NoMarkers(MemoryFolderKeyStore::default());
        seed(&store.0, |cfg| {
            custody::record_created_set(cfg, "docs", [1; 32], Some(PRED), 10);
        });
        *nest.listed.lock().unwrap() = vec![listed("docs", Some([1; 32]))];
        let report = block_on(reconcile_set_custody(&files, &store, OWN)).expect("reconciles");
        assert_eq!((report.reminted, report.remint_deferred), (0, 1));
        assert_eq!(
            custody::live_set_nonce(&store.0.snapshot(), "docs"),
            Some([1; 32])
        );
    }

    /// The create helper records the creating identity as the minter, so the
    /// reconcile never cuts a set its own identity created.
    #[test]
    fn create_records_the_store_identity_as_the_minter() {
        let (nest, files, _config) = harness(Outcome::Ok);
        let store = MemoryFolderKeyStore::default().serving(OWN);
        block_on(create_set(&files, &store, create_req("docs"))).expect("creates");
        let cfg = store.snapshot();
        assert_eq!(
            custody::live_set_by_name(&cfg, "docs").and_then(|s| s.minted_by),
            Some(OWN)
        );
        assert!(custody::uncut_owned_nonces(&cfg, &OWN).is_empty());
        drop(nest);
    }

    #[test]
    fn reconcile_mints_for_a_listed_set_custody_has_never_seen_and_pushes_it() {
        let (nest, files, config) = harness(Outcome::Ok);
        *nest.listed.lock().unwrap() = vec![listed("docs", None)];
        let report = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(report.minted, 1);
        let minted = custody::live_set_nonce(&nest.stored_config(), "docs").expect("minted");
        let updates = nest.updates.lock().unwrap();
        assert_eq!(
            updates[0].set_nonce.as_ref().map(|b| b.to_vec()),
            Some(minted.to_vec())
        );
    }

    /// A fresh device of the owner reconciles before the account plane has
    /// carried it the custody row another device minted the set under: the
    /// nest's echo is a nonce custody does not hold YET. Minting over it would
    /// push a second nonce onto the nest, and once the synced row lands its
    /// older entry wins the pick — the signer and the nest then disagree and
    /// every record fails `signature_invalid`. So the pass leaves the set
    /// alone, and the relaunch after the row arrives finds the two agreeing.
    #[test]
    fn reconcile_never_mints_over_a_nest_nonce_custody_has_not_received_yet() {
        const SEEDED: [u8; 32] = [0x5E; 32];
        let (nest, files, config) = harness(Outcome::Ok);
        *nest.listed.lock().unwrap() = vec![listed("docs", Some(SEEDED))];

        let report = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(report.minted, 0, "custody is behind, not empty");
        assert!(
            nest.updates.lock().unwrap().is_empty(),
            "nothing pushed over the nest"
        );
        assert_eq!(custody::live_set_nonce(&nest.stored_config(), "docs"), None);

        // The plane's row arrives (the seeding device's create entry), then the
        // app relaunches.
        seed(&config, |cfg| {
            custody::record_created_set(cfg, "docs", SEEDED, Some(OWN), 1);
        });
        let report = block_on(reconcile_set_custody(&files, &*config, OWN)).expect("reconciles");
        assert_eq!(
            report,
            SetCustodyReconcile::default(),
            "nothing left to settle"
        );
        assert_eq!(
            custody::live_set_nonce(&nest.stored_config(), "docs"),
            Some(SEEDED),
            "the signer's nonce is the nest's"
        );
        assert!(nest.updates.lock().unwrap().is_empty());
    }
}
