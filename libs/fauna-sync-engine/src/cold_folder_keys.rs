//! [`ColdReplicaFolderKeys`] — a capability host's [`FolderKeyReader`]: the
//! account's folder-key custody read through the host's throwaway fleet
//! replica ([`ColdFleetReplica`] on its device arm), as the enrolled device the
//! host is a process of (`on-demand-files.md` § Shared sets on a capability
//! host, decision 1′).
//!
//! A load walks the replica first — incremental over its stored frontier, so
//! a rotation minted since the last load reaches the host at its next build
//! or edge — then folds `fauna.state.folder-keys` through the plane's own
//! door. A row the replica cannot open (a generation this device holds no wrap
//! and no custody for) is not in the fold: the set it keys builds keyless and
//! fails closed, never plaintext.
//!
//! **The replica lives on a thread of its own.** Its store futures are not
//! `Send` (the store backend's async trait), so — like the account runtime's
//! store task — one worker thread owns the replica and answers loads over a
//! channel, and the reader handle is a `Send + Sync` sender any runtime may
//! hold. Loads are served one at a time, in order. The worker ends when the
//! last handle drops.

use anyhow::{Context, Result};
use fauna_account_plane::cold_replica::{ColdFleetReplica, ColdKeySource};
use fauna_client_folders::FolderKeyReader;
use fauna_core::crypto::BackupKey;
use fauna_core::data::FoldersConfig;
use fauna_core::identity::ActorId;
use fauna_protocol::RpcRequester;
use tokio::sync::{mpsc, oneshot};

/// One load, answered by the replica's worker.
type LoadReply = oneshot::Sender<Result<FoldersConfig>>;

/// The folder-key reader over one throwaway replica — every per-set host of
/// one account in a process shares one (clone the handle).
#[derive(Clone)]
pub struct ColdReplicaFolderKeys {
    loads: mpsc::UnboundedSender<LoadReply>,
}

impl ColdReplicaFolderKeys {
    /// Start the replica's worker thread for `actor` over `rpc` — the host's
    /// own connection, authenticated as the account — keyed by `source`, and
    /// return the reader. Nothing is fetched until the first load.
    ///
    /// # Errors
    /// The worker thread could not be spawned. A replica that fails to open
    /// on the worker fails every load with the reason instead.
    pub fn spawn<R>(
        rpc: R,
        actor: ActorId,
        backup_key: BackupKey,
        source: ColdKeySource,
    ) -> Result<Self>
    where
        R: RpcRequester + Send + 'static,
    {
        let (loads, mut requests) = mpsc::unbounded_channel::<LoadReply>();
        std::thread::Builder::new()
            .name("fauna-cold-replica".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let why = format!("the throwaway replica's runtime failed to start: {e}");
                        while let Some(reply) = requests.blocking_recv() {
                            let _ = reply.send(Err(anyhow::anyhow!("{why}")));
                        }
                        return;
                    }
                };
                runtime.block_on(async move {
                    let replica = ColdFleetReplica::open(rpc, actor, &backup_key, source)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    while let Some(reply) = requests.recv().await {
                        let answer = match &replica {
                            Ok(replica) => load(replica).await,
                            Err(why) => Err(anyhow::anyhow!("{why}")),
                        };
                        let _ = reply.send(answer);
                    }
                });
            })
            .context("spawn the throwaway replica's worker thread")?;
        Ok(Self { loads })
    }

    /// Whether `other` reads through the same replica as this handle.
    pub fn shares_replica_with(&self, other: &Self) -> bool {
        self.loads.same_channel(&other.loads)
    }
}

/// The account's folder-key custody as a process that holds the identity seed
/// and hosts no account runtime reads it — the e2e harness's app-less
/// exports: a throwaway fleet replica on the escrow arm
/// ([`ColdKeySource::Seed`]), keyed by the account generations the seed
/// recovers, over `rpc` (authenticated as the account). A replica that cannot
/// start leaves custody unreadable — a bound set records unsigned and seals
/// fail closed.
pub fn seed_holder_folder_keys<R>(
    rpc: R,
    identity: &fauna_core::identity::ActorKeypair,
) -> std::sync::Arc<dyn FolderKeyReader>
where
    R: RpcRequester + Send + 'static,
{
    let seed = zeroize::Zeroizing::new(*identity.secret_bytes());
    match ColdReplicaFolderKeys::spawn(
        rpc,
        identity.actor_id(),
        BackupKey::derive(&seed),
        ColdKeySource::Seed(seed),
    ) {
        Ok(reader) => std::sync::Arc::new(reader),
        Err(e) => std::sync::Arc::new(fauna_client_folders::UnreadableFolderKeys(format!("{e:#}"))),
    }
}

/// Walk, then fold.
async fn load<R: RpcRequester>(replica: &ColdFleetReplica<R>) -> Result<FoldersConfig> {
    replica.walk().await?;
    replica.read_folder_keys().await
}

#[async_trait::async_trait]
impl FolderKeyReader for ColdReplicaFolderKeys {
    async fn load(&self) -> Result<FoldersConfig> {
        let (reply, answer) = oneshot::channel();
        self.loads
            .send(reply)
            .map_err(|_| anyhow::anyhow!("the throwaway replica's worker has stopped"))?;
        answer
            .await
            .map_err(|_| anyhow::anyhow!("the throwaway replica's worker stopped mid-load"))?
    }
}
