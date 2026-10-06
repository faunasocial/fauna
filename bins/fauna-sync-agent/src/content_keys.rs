//! **The agent resolves its own content keys** — every set's engine key material
//! comes from the account's folder-key custody (`fauna.state.folder-keys`), read
//! in-process through the account store [`crate::account_host`] mounts
//! ([`SyncServiceState::folder_keys`]), never from the app
//! (`on-demand-files.md` § Shared sets on a capability host → *One mechanism —
//! the agent converges*, decision 1′; `sync-agent-credentials.md` § Credential
//! model, *Content keys are not provisioned*). With no store mounted custody is
//! unreadable, and every bound set builds keyless.
//!
//! **Edges.** The resolution is re-read — and the engines re-reconciled when it
//! changed — at every one of the agent's edges:
//! - **every reconcile** ([`crate::engine_driver::reconcile_engines`] wakes this
//!   task): provision, restore, bind/unbind, pause/resume;
//! - **the `state-fleet` nudge**: every custody change (a rotation, a share, a
//!   serve or paywall flip, a member's ingest) is a `fauna.account.state.put` on
//!   the fleet scope, and the nest fans a `SyncChanged` tagged with that scope to
//!   the writer's connected devices — this process among them; the edge waits
//!   for the mounted runtime's walk to land the rows before it re-reads;
//! - **the resident loop's per-tick row read**: each engine carries a
//!   [`fauna_sync_engine::binding_edge::BindingEdge`] whose basis is the row its
//!   keys were resolved from, and a tick that finds the row moved — or the floor
//!   still ahead of the generation held — wakes this task;
//! - **a periodic backstop**, because the nudge is best-effort by construction.
//!
//! **A re-read that still leaves a set pending walks first.** Custody is read
//! from the mounted store's local rows, which only the runtime's walk brings in
//! — and a lost nudge means no walk came. So when a fresh resolution still has a
//! bound set behind its floor or keyless, the task has the mounted runtime run
//! a pass and reads once more ([`refresh_through_a_walk`]): the row read that
//! found the floor ahead is the evidence that custody on the nest is ahead of
//! the replica, and the edge that noticed is the one that fetches it.
//!
//! The edges are the retry: there is no attempt budget. Between them a write to a
//! set behind its floor is held by the engine itself (the pre-seal hold,
//! `SyncEngine::seal_hold`), never sealed under the older generation.
//!
//! **Fail closed.** A list that cannot be read keeps the resolution the agent
//! holds (the engines' own floor hold guards their seals); custody that cannot be
//! read resolves against empty custody, so every bound set builds **keyless** and
//! fails closed per operation, every served-but-keyless set is withheld, and no
//! cross-nest set is named — until an edge re-reads it. A set the resolution does
//! not name gets no engine. The only thing ever read as "owner-only" is a row the
//! nest itself lists as unbound.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fauna_core::folder_keys::{FolderEngineKeys, FolderRef};
use fauna_protocol::PushEvent;
use fauna_sync_engine::binding_edge::{BindingBasis, floor_ahead};
use tokio::sync::watch;

use crate::state::SyncServiceState;

/// How long a quiet agent goes between re-resolves with no other edge firing —
/// the backstop for a missed nudge. Custody changes are rare and each one is
/// nudged; this bounds the worst case, not the common one.
const BACKSTOP_INTERVAL: Duration = Duration::from_secs(300);

/// How long one resolve may take (a list plus a custody load) before the edge
/// gives up and keeps what it holds; the next edge tries again.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the task waits before retrying a nest connection it could not open —
/// the agent's shared idle cadence, so a dead nest is not re-dialled faster here
/// than by the renewal loop beside it. A provision wakes the task at once.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(crate::state::IDLE_RECHECK_SECS);

/// One resolution of every set's content keys.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResolvedContentKeys {
    /// The account whose custody this resolution read. Every reader checks it
    /// against the capability it serves ([`Self::is_for`]), so an account switch
    /// never keys the incoming account's engines off the outgoing account's
    /// resolution in the moment before the next resolve lands. `None` only in
    /// the unit tests' fixtures, where it matches every account.
    actor_id: Option<[u8; 32]>,
    /// Every set this holder can bind, told apart by `FolderEngineKeys::folder_id`.
    keys: Vec<FolderEngineKeys>,
    /// The binding basis of every row the resolution's list read carried, by row
    /// id — what each engine's refresh edge compares its tick's read against.
    bases: HashMap<i64, BindingBasis>,
    /// Whether custody was actually read. `false` = it could not be, so every
    /// bound set resolved keyless.
    custody_read: bool,
    /// Test-only: answer the owner-only default for a set the resolution does
    /// not name — the unit tests' stand-in for a nest that lists every bound
    /// location as unbound. Never set in production.
    #[cfg(test)]
    owner_only_for_unnamed: bool,
}

impl ResolvedContentKeys {
    /// Was this resolution read from `actor`'s custody?
    #[must_use]
    pub fn is_for(&self, actor: [u8; 32]) -> bool {
        self.actor_id.is_none_or(|a| a == actor)
    }

    /// The engine key material for `folder_ref`, or `None` when the resolution
    /// does not name the set — no engine for it (see the module docs).
    #[must_use]
    pub fn keys_for(&self, folder_ref: FolderRef) -> Option<FolderEngineKeys> {
        let found = self
            .keys
            .iter()
            .find(|k| FolderRef::parse(&k.folder_id) == Some(folder_ref))
            .cloned();
        #[cfg(test)]
        if found.is_none() && self.owner_only_for_unnamed {
            return Some(FolderEngineKeys::default());
        }
        found
    }

    /// Every named set's nonce, by set name — the snapshot a recorder that
    /// addresses sets by name (the re-seed delivery) signs under. A name two
    /// sets share with different nonces (an owned set and a same-named one the
    /// holder belongs to) is dropped rather than guessed: its records go out
    /// unsigned, never under the wrong set's binding.
    #[must_use]
    pub fn set_nonces_by_name(&self) -> HashMap<String, [u8; 32]> {
        let mut out: HashMap<String, [u8; 32]> = HashMap::new();
        let mut ambiguous: Vec<String> = Vec::new();
        for k in &self.keys {
            let Some(nonce) = k.set_nonce else { continue };
            match out.get(&k.folder) {
                Some(held) if *held != nonce => ambiguous.push(k.folder.clone()),
                _ => {
                    out.insert(k.folder.clone(), nonce);
                }
            }
        }
        for name in ambiguous {
            out.remove(&name);
        }
        out
    }

    /// Every named set's nonce **lineage** (`writer-signed-change-records.md`
    /// ruling (11)(b)), by set name — [`Self::set_nonces_by_name`] with the
    /// live nonce's minter and the retired nonces the custody resolution
    /// carried, so a version reader judges a predecessor's row under a retired
    /// nonce as history (ruling (11)(c)) rather than as a row that does not
    /// verify — and the custody entry's serve window (ruling (7)(b)(ii)
    /// rule (2)), so the reader's served exemption is custody's word. The
    /// same ambiguity rule: a name two sets share is dropped.
    #[must_use]
    pub fn set_lineages_by_name(
        &self,
    ) -> HashMap<String, fauna_core::folder_keys::SetNonceLineage> {
        let live = self.set_nonces_by_name();
        self.keys
            .iter()
            .filter(|k| k.set_nonce.is_some() && live.get(&k.folder) == k.set_nonce.as_ref())
            .map(|k| {
                (
                    k.folder.clone(),
                    fauna_core::folder_keys::SetNonceLineage {
                        live: k.set_nonce,
                        live_minted_by: k.set_nonce_minted_by,
                        retired: k.retired_lineage.clone(),
                        // The serve window (ruling (7)(b)(ii) rule (2)): the
                        // agent's projection readers exempt a served set's
                        // pseudo-device rows on custody's word alone.
                        served_at: k.served_at,
                        unserved_at: k.unserved_at,
                    },
                )
            })
            .collect()
    }

    /// The basis `folder_ref`'s keys were resolved from — its row's, or the
    /// default for a cross-nest set (no row on this nest).
    #[must_use]
    pub fn basis_for(&self, folder_ref: FolderRef) -> BindingBasis {
        match folder_ref {
            FolderRef::Local(id) => self.bases.get(&id).cloned().unwrap_or_default(),
            FolderRef::Foreign(_) => BindingBasis::default(),
        }
    }

    /// *Keys pending* (`sync-agent.md` § Local agent health): a set bound here
    /// whose keys hold a generation behind its row's `content_key_floor`, or
    /// that resolved keyless (custody unread, or the generation not in custody).
    #[must_use]
    pub fn keys_pending_for(&self, folder_ref: FolderRef) -> bool {
        let Some(keys) = self.keys_for(folder_ref) else {
            return false;
        };
        if keys.mls_group_id.is_none() {
            // Owner-only: sealed under the BackupKey, no generation to be behind.
            return false;
        }
        let held = keys.content_keys.as_ref().map(|k| k.current_version());
        held.is_none() || floor_ahead(held, self.basis_for(folder_ref).content_key_floor())
    }

    /// Test-only: a resolution that keys every set owner-only, standing in for
    /// the unit tests' fake nest (see `owner_only_for_unnamed`).
    #[cfg(test)]
    #[must_use]
    pub fn owner_only_for_tests() -> Self {
        Self {
            custody_read: true,
            owner_only_for_unnamed: true,
            ..Self::default()
        }
    }

    /// Test-only: a resolution naming exactly `keys`, with `rows` as its list.
    #[cfg(test)]
    #[must_use]
    pub fn for_tests(
        keys: Vec<FolderEngineKeys>,
        rows: &[fauna_protocol::folders::FolderSummary],
    ) -> Self {
        Self {
            actor_id: None,
            keys,
            bases: rows.iter().map(|r| (r.id, BindingBasis::of(r))).collect(),
            custody_read: true,
            owner_only_for_unnamed: false,
        }
    }
}

/// Resolve every set's keys now: the folder list over the agent's own bearer
/// connection, custody through the mounted store. `None` = nothing could be resolved (no
/// capability, no connection, or the folder list failed) — the caller keeps
/// what it holds.
async fn resolve(state: &SyncServiceState) -> Option<ResolvedContentKeys> {
    let actor_id = {
        let cap = state.capability.read().await;
        cap.as_ref()?.actor_id_array()?
    };
    let nest = match state.nest_rpc_client().await {
        Ok(nest) => nest,
        Err(e) => {
            tracing::warn!(error = %e, "content keys: no nest connection to resolve over");
            return None;
        }
    };
    let rows = match tokio::time::timeout(
        RESOLVE_TIMEOUT,
        fauna_client_folders::FoldersClient::new(Arc::clone(&nest)).list_owned_and_shared_wire(),
    )
    .await
    {
        Ok(Ok(reply)) => reply.folders,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "content keys: folder list failed; keeping what is held");
            return None;
        }
        Err(_) => {
            tracing::warn!("content keys: folder list timed out; keeping what is held");
            return None;
        }
    };
    let (cfg, custody_read) =
        match tokio::time::timeout(RESOLVE_TIMEOUT, state.folder_keys.load()).await {
            Ok(Ok(cfg)) => (cfg, true),
            Ok(Err(e)) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "content keys: custody unreadable — every bound set builds keyless (fail \
                     closed) until an edge re-reads it"
                );
                (fauna_core::data::FoldersConfig::default(), false)
            }
            Err(_) => {
                tracing::warn!(
                    "content keys: custody load timed out — every bound set builds keyless (fail \
                     closed) until an edge re-reads it"
                );
                (fauna_core::data::FoldersConfig::default(), false)
            }
        };
    // Named here, from the custody just read: a sealed set's row rests no
    // plaintext name (`path-sealing.md` § the set-name plane), and every set
    // below is keyed and labelled by it. A sealed row custody cannot name yet is
    // left out — no engine for it until an edge re-reads.
    let rows = fauna_client_folders::engine_binding::named_for_engine_host(rows, &cfg);
    // This device's adoption markers ride the same blob to the engines
    // (`writer-signed-change-records.md` ruling (11)(d)) — the replica-local
    // row the identity app's re-mint wrote, read through the same mount.
    let markers = if custody_read {
        fauna_client_folders::adoption_markers_or_none(&*state.folder_keys).await
    } else {
        Vec::new()
    };
    let keys = match fauna_client_folders::engine_keys_from(
        &cfg,
        fauna_core::identity::ActorId(actor_id),
        &rows,
        &markers,
    ) {
        Ok(keys) => keys,
        Err(e) => {
            // A malformed nest projection: nothing in this list can vouch for any
            // set's binding, so no set is named — no engine runs until an edge
            // reads a well-formed one.
            tracing::error!(error = %e, "content keys: malformed folder projection; withholding every set");
            Vec::new()
        }
    };
    Some(ResolvedContentKeys {
        actor_id: Some(actor_id),
        keys,
        bases: rows.iter().map(|r| (r.id, BindingBasis::of(r))).collect(),
        custody_read,
        #[cfg(test)]
        owner_only_for_unnamed: false,
    })
}

/// Re-resolve and publish. `true` when the published resolution changed — the
/// caller then re-reconciles, and every engine whose stamp moved rebuilds.
async fn refresh(state: &SyncServiceState) -> bool {
    if state.capability.read().await.is_none() {
        // Nothing provisioned: nothing can be resolved, and nothing held is any
        // account's any more.
        return state.content_keys.write().await.take().is_some();
    }
    let Some(resolved) = resolve(state).await else {
        return false;
    };
    let mut slot = state.content_keys.write().await;
    if slot.as_ref() == Some(&resolved) {
        return false;
    }
    *slot = Some(resolved);
    true
}

/// [`refresh`], and — when the fresh resolution still leaves a bound set
/// pending — one pass of the mounted runtime and a second read (module docs,
/// *A re-read that still leaves a set pending walks first*). `true` when the
/// published resolution changed at either read.
async fn refresh_through_a_walk(state: &SyncServiceState) -> bool {
    let changed = refresh(state).await;
    if !keys_pending(state).await || !walk_mounted_store(state).await {
        return changed;
    }
    refresh(state).await || changed
}

/// Have the mounted runtime run one pass now, bounded by [`RESOLVE_TIMEOUT`].
/// `false` when there is nothing to ask: no store mounted, a pass that failed
/// or timed out, or a reader beside an app's runtime — the app's runtime holds
/// the engine role and walks on its own edges.
async fn walk_mounted_store(state: &SyncServiceState) -> bool {
    let Some(handle) = state
        .mounted_store
        .lock()
        .ok()
        .and_then(|held| held.clone())
    else {
        return false;
    };
    if !handle.is_engine_holder() {
        return false;
    }
    match tokio::time::timeout(RESOLVE_TIMEOUT, handle.reconcile_now()).await {
        Ok(Ok(_)) => true,
        Ok(Err(e)) => {
            tracing::warn!(
                error = %format!("{e:#}"),
                "content keys: the mounted store's pass failed; keeping what custody reads"
            );
            false
        }
        Err(_) => {
            tracing::warn!("content keys: the mounted store's pass timed out");
            false
        }
    }
}

/// Re-resolve now, on behalf of a job about to sign under the resolution's
/// nonces — the re-seed, whose seed-holding app created the target sets moments
/// before starting it (`writer-signed-change-records.md` ruling (7)(a)(i)). The
/// custody nudge for those creates may not have reached this task yet, so the
/// job does not wait for it: an engine-holding runtime walks first (a reader
/// beside an app's runtime already sees the app's custody write in the shared
/// store's rows), then the resolution is re-read and, when it moved, the
/// engines re-reconciled exactly as the edge task would.
pub(crate) async fn refresh_now(state: &Arc<SyncServiceState>) {
    walk_mounted_store(state).await;
    if refresh(state).await
        && let Err(e) = crate::engine_driver::reconcile_resolved(state).await
    {
        tracing::warn!(error = %e, "content keys: reconcile after an on-demand re-resolve failed");
    }
}

/// *Keys pending* for the agent's status reply: some set bound here is behind
/// its floor or keyless — or nothing is resolved yet while sets are bound.
pub async fn keys_pending(state: &SyncServiceState) -> bool {
    let bound: Vec<FolderRef> = {
        let config = state.config.read().await;
        crate::engine_driver::plan_engines(&config)
            .into_iter()
            .map(|m| m.folder_ref)
            .collect()
    };
    let Some(actor) = state
        .capability
        .read()
        .await
        .as_ref()
        .and_then(fauna_ipc::sync::SyncCapability::actor_id_array)
    else {
        return false;
    };
    if bound.is_empty() {
        return false;
    }
    match state.content_keys.read().await.as_ref() {
        Some(resolved) if resolved.is_for(actor) => {
            bound.iter().any(|r| resolved.keys_pending_for(*r))
        }
        // Nothing resolved for this account yet while sets are bound.
        _ => true,
    }
}

/// Is `event` the custody nudge — the `SyncChanged` tagged with the fleet
/// scope that every `fauna.account.state.put` there fans out (a custody write
/// among them)?
fn is_custody_nudge(event: &PushEvent) -> bool {
    matches!(event, PushEvent::SyncChanged(p)
        if p.scope.as_deref() == Some(fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE))
}

/// After a custody nudge, give the mounted runtime its walk before re-reading:
/// the nudge reaches the runtime's push arm at the same moment it reaches this
/// task. An engine-holding runtime completes a pass; a reader beside an app's
/// runtime sees the store's data version move when that app's pass lands the
/// rows. Bounded by [`RESOLVE_TIMEOUT`] — a walk that does not come keeps the
/// re-read, and the backstop is the correctness path either way.
async fn await_runtime_walk(state: &SyncServiceState) {
    let Some(handle) = state
        .mounted_store
        .lock()
        .ok()
        .and_then(|held| held.clone())
    else {
        return;
    };
    let walked = async {
        if handle.is_engine_holder() {
            let (_, baseline) = handle.pump_cycles();
            handle.pass_completed_after(baseline).await;
        } else {
            let before = handle.data_version().await.ok().flatten();
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if handle.data_version().await.ok().flatten() != before {
                    return;
                }
            }
        }
    };
    let _ = tokio::time::timeout(RESOLVE_TIMEOUT, walked).await;
}

/// The edge task: re-resolve on every wake (a reconcile, an engine's refresh
/// edge, the mount coming up or down), on every custody nudge once the runtime
/// has walked, and on the backstop; re-reconcile when the resolution changed.
/// Runs for the process lifetime.
pub async fn run(state: Arc<SyncServiceState>, mut shutdown: watch::Receiver<bool>) {
    // The connection whose pushes are subscribed, so a re-provision onto another
    // nest or actor re-subscribes on the client that replaced it.
    let mut subscribed: Option<(
        Arc<fauna_client::NestClient>,
        tokio::sync::broadcast::Receiver<PushEvent>,
    )> = None;
    loop {
        let changed = tokio::select! {
            _ = shutdown.changed() => return,
            changed = refresh_through_a_walk(&state) => changed,
        };
        if changed && let Err(e) = crate::engine_driver::reconcile_resolved(&state).await {
            tracing::warn!(error = %e, "content keys: reconcile after a re-resolve failed");
        }

        // (Re)subscribe to the current connection's pushes.
        let provisioned = state.capability.read().await.is_some();
        if provisioned {
            match state.nest_rpc_client().await {
                Ok(nest) => {
                    if !subscribed
                        .as_ref()
                        .is_some_and(|(held, _)| Arc::ptr_eq(held, &nest))
                    {
                        let rx = nest.subscribe_pushes();
                        subscribed = Some((nest, rx));
                    }
                }
                Err(_) => subscribed = None,
            }
        } else {
            subscribed = None;
        }

        // Wait for the next edge.
        // Nothing provisioned: the provision's reconcile wakes the task.
        let backstop = tokio::time::sleep(if subscribed.is_none() && provisioned {
            RECONNECT_BACKOFF
        } else {
            BACKSTOP_INTERVAL
        });
        tokio::pin!(backstop);
        loop {
            tokio::select! {
                _ = shutdown.changed() => return,
                () = state.content_keys_wake.notified() => break,
                () = &mut backstop => break,
                event = async {
                    match subscribed.as_mut() {
                        Some((_, rx)) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => match event {
                    Ok(event) if is_custody_nudge(&event) => {
                        await_runtime_walk(&state).await;
                        break;
                    }
                    Ok(_) => {}
                    // Missed pushes may have included the nudge: re-resolve.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        subscribed = None;
                        break;
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::folders::FolderSummary;

    fn row(id: i64, gid: Option<&str>, floor: Option<u64>) -> FolderSummary {
        FolderSummary {
            id,
            name: format!("set-{id}"),
            mls_group_id: gid.map(str::to_owned),
            content_key_floor: floor,
            ..Default::default()
        }
    }

    /// The by-name nonce snapshot the re-seed signs under: every named set's
    /// nonce, except a name two sets share with different nonces — dropped,
    /// never guessed.
    #[test]
    fn set_nonces_by_name_drops_an_ambiguous_name() {
        let named = |id: i64, name: &str, nonce: Option<[u8; 32]>| FolderEngineKeys {
            folder: name.into(),
            folder_id: FolderRef::Local(id).to_wire(),
            set_nonce: nonce,
            ..Default::default()
        };
        let resolved = ResolvedContentKeys::for_tests(
            vec![
                named(1, "docs", Some([1; 32])),
                named(2, "photos", Some([2; 32])),
                named(3, "photos", Some([3; 32])), // a same-named set it belongs to
                named(4, "bare", None),
            ],
            &[],
        );
        let map = resolved.set_nonces_by_name();
        assert_eq!(map.get("docs"), Some(&[1; 32]));
        assert!(
            !map.contains_key("photos"),
            "ambiguous → unsigned, never guessed"
        );
        assert!(!map.contains_key("bare"), "no nonce in custody");
    }

    /// Ruling (7)(b)(ii) rule (2): the lineage snapshot the agent's projection
    /// readers judge under carries custody's serve window, so a served set's
    /// pseudo-device rows are exempt there exactly as on the engine — and a
    /// set custody never served stays unexempt.
    #[test]
    fn set_lineages_by_name_carry_the_serve_window() {
        let named = |id: i64, name: &str, served_at: Option<u64>| FolderEngineKeys {
            folder: name.into(),
            folder_id: FolderRef::Local(id).to_wire(),
            set_nonce: Some([id as u8; 32]),
            served_at,
            ..Default::default()
        };
        let resolved = ResolvedContentKeys::for_tests(
            vec![named(1, "served", Some(2_000)), named(2, "plain", None)],
            &[],
        );
        let lineages = resolved.set_lineages_by_name();
        assert!(lineages["served"].webdav_served());
        assert_eq!(lineages["served"].served_at, Some(2_000));
        assert!(!lineages["plain"].webdav_served());
    }

    fn bound(id: i64, version: Option<u64>) -> FolderEngineKeys {
        let content_keys = version.map(|v| {
            let mut keys = fauna_core::folder_keys::FolderContentKeys::genesis([1; 32], 1);
            for n in 2..=v {
                keys.rotate([n as u8; 32], n);
            }
            keys
        });
        FolderEngineKeys {
            folder_id: FolderRef::Local(id).to_wire(),
            mls_group_id: Some(b"gid".to_vec()),
            content_keys,
            ..Default::default()
        }
    }

    #[test]
    fn a_bound_set_behind_its_floor_or_keyless_is_pending_an_owner_only_one_never() {
        let resolved = ResolvedContentKeys::for_tests(
            vec![
                bound(1, Some(1)),
                bound(2, Some(2)),
                bound(3, None),
                FolderEngineKeys {
                    folder_id: FolderRef::Local(4).to_wire(),
                    ..Default::default()
                },
            ],
            &[
                row(1, Some("aa"), Some(2)),
                row(2, Some("aa"), Some(2)),
                row(3, Some("aa"), None),
                row(4, None, None),
            ],
        );
        assert!(
            resolved.keys_pending_for(FolderRef::Local(1)),
            "behind the floor"
        );
        assert!(
            !resolved.keys_pending_for(FolderRef::Local(2)),
            "at the floor"
        );
        assert!(resolved.keys_pending_for(FolderRef::Local(3)), "keyless");
        assert!(
            !resolved.keys_pending_for(FolderRef::Local(4)),
            "owner-only"
        );
        assert!(
            !resolved.keys_pending_for(FolderRef::Local(5)),
            "a set the resolution does not name has no engine to be behind"
        );
    }

    #[test]
    fn a_set_the_resolution_does_not_name_gets_no_keys() {
        let resolved = ResolvedContentKeys::for_tests(vec![bound(1, Some(1))], &[]);
        assert!(resolved.keys_for(FolderRef::Local(1)).is_some());
        assert!(
            resolved.keys_for(FolderRef::Local(2)).is_none(),
            "absent is 'not keyed', never owner-only"
        );
    }

    /// **Same-named sets are told apart by ref.** Names are unique only per owner, so a holder who owns
    /// "docs" and is a member of someone else's "docs" resolves two same-named
    /// entries — in the producer's order, owned first. Each set's engine must be
    /// keyed off its own entry, reached by ref past the same-named one; a name
    /// match would key the shared set's engine off the owned set's (owner-only)
    /// material and seal under a key its members cannot open.
    #[test]
    fn keys_are_found_by_ref_past_a_same_named_entry_never_by_name() {
        let owned = FolderRef::Local(7);
        let foreign = FolderRef::Foreign([0xab; 32]);
        let resolved = ResolvedContentKeys::for_tests(
            vec![
                FolderEngineKeys {
                    folder: "docs".into(),
                    folder_id: owned.to_wire(),
                    ..Default::default()
                },
                FolderEngineKeys {
                    folder: "docs".into(),
                    folder_id: foreign.to_wire(),
                    mls_group_id: Some(b"foreign-gid".to_vec()),
                    home_nest_url: Some("https://home.example".into()),
                    channel_id_hex: Some("ab".repeat(32)),
                    ..Default::default()
                },
                FolderEngineKeys {
                    folder: "docs".into(),
                    folder_id: "not-a-ref".into(),
                    mls_group_id: Some(b"gid".to_vec()),
                    ..Default::default()
                },
            ],
            &[],
        );
        assert_eq!(
            resolved.keys_for(foreign).and_then(|k| k.mls_group_id),
            Some(b"foreign-gid".to_vec()),
            "a ref must out-rank a colliding name"
        );
        assert_eq!(
            resolved.keys_for(owned).and_then(|k| k.mls_group_id),
            None,
            "the owned twin resolves to itself"
        );
        assert!(
            resolved.keys_for(FolderRef::Local(999)).is_none(),
            "an entry whose ref does not parse matches nothing, whatever its name"
        );
    }

    #[test]
    fn only_the_fleet_scope_nudge_is_a_custody_edge() {
        let nudge = |folder: &str, scope: Option<&str>| {
            PushEvent::SyncChanged(fauna_protocol::push_events::SyncChangedPayload {
                folder: folder.into(),
                scope: scope.map(str::to_owned),
                ..Default::default()
            })
        };
        assert!(is_custody_nudge(&nudge("__account", Some("state-fleet"))));
        assert!(!is_custody_nudge(&nudge("__config", None)));
        assert!(!is_custody_nudge(&nudge(
            "__account",
            Some("account-state")
        )));
        assert!(!is_custody_nudge(&nudge("docs", None)));
    }
}
