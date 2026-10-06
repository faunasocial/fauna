//! The launch-time folder removal resume as a [`PostRestoreHook`] — the ONE
//! shared body every tokio leg hands to
//! `fauna_client_mls_sync::launcher::tokio_launcher` (the native FFI factory +
//! tui), and linux drives from its own restore task. It lives HERE, not
//! beside the launcher, because building a [`FoldersAuthor`] needs this crate —
//! and this crate depends on `fauna-client-conversations`, which depends on
//! `fauna-client-mls-sync`: the hook seam is what breaks that cycle (the
//! launcher's module doc tells the same story from its side).
//!
//! **Why the resume rides the MLS-sync launch** (and not the folders rail):
//! the removal gate **is** the conversations backend, and `restore_and_wire` is
//! what injects its `CommitGate` — this is the first moment a gated sentinel can
//! be discharged rather than deferred (`mls-group-key-material.md` § M2
//! Rotate-on-removal: "every real plane wires the gate, so the launch-time
//! resume discharges a poisoned sentinel with no user step"). Resuming before
//! the restore would see `GateNotEngaged` and defer every gated sentinel to the
//! next launch.
//!
//! **Why running on the restore's `Failed` path is safe** (the hook contract
//! requires it): the `GateNotEngaged` guard in `drive_removal` — a byted sentinel
//! resumes through the ungated leg *without consulting the gate* (durable bytes outrank the gate), so the single-device fallback still heals
//! what it can; a **byte-less** one defers — unless its durable
//! `gated_attempted` stamp proves no engaged gated attempt ever started for it
//! (then nothing can have been distributed and the ungated rebuild is safe;
//! without that carve-out a plane that never engages — the `Failed` path of a
//! replica restore — would defer the sentinel forever and the removal could never
//! complete). A sentinel with
//! `gated_attempted` set may carry a commit an earlier gated
//! attempt already distributed (the gated route records no bytes), and
//! rebuilding it with no walk-to-head could fork the group — those defer.
//!
//! **The pass needs TWO edges, and runs at whichever comes last.** The restore
//! (above) is one; the other is the seat's account runtime, which is where the
//! folder-key custody rests and which assembles on its own task with no
//! ordering against the conversations launch — on every seat. So when the hook
//! fires:
//!
//! - **custody readable** → the pass runs in line, before the first poll, as it
//!   always has;
//! - **custody not readable yet** (the runtime is still assembling) → the pass
//!   is *detached*: it waits the assembly out as a custody write does
//!   (`FolderKeyStore::load_for_write`, ~30 s, then gives up until the next
//!   launch) and runs then, beside the receive loop. The hook returns at once,
//!   so the receive loop never waits on the runtime. Running beside the loop is
//!   the posture a foreground `remove_member` has always had; the staged
//!   removal's own guards (above) are what make it safe at any moment.
//!
//! A plain read here used to answer "not running" and skip the recovery until
//! a launch that happened to win the race. The detached leg holds the backend
//! weakly across its wait, so a session torn down meanwhile ends it.
//!
//! **The same pass re-runs the foreign-home seed**
//! (`FaunaMlsBackend::seed_channel_homes_from_custody`). The restore runs it
//! once itself, but against the same custody and so in the same race: over an
//! unreadable custody it fills nothing, and a cross-nest folder channel then
//! drains its own nest for the whole session. The seed fills only holes, so a
//! second run after a first that did read is a no-op.
//!
//! **The same pass starts the served-blob follower**
//! ([`crate::ServedBlobFollower`], `writer-signed-change-records.md` ruling
//! (7)(b)(ii) rule (3)): the pass reconciles the `WebdavKeysBlob` once, and
//! the follower re-runs that reconcile whenever the custody nudge's walk lands
//! a sibling's serve flip, for the rest of the session.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use fauna_client::NestClient;
use fauna_client_mls_sync::launcher::PostRestoreHook;
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_core::identity::ActorKeypair;
use zeroize::Zeroizing;

use crate::FoldersClient;
use crate::key_reader::FolderKeyStore;
use crate::orchestration::FoldersAuthor;

/// Re-drive every crash-staged folder member removal
/// ([`FoldersAuthor::resume_pending_removals`]) at launch — the recovery that
/// makes rotate-on-removal's forward-secrecy guarantee actually complete after
/// a crash between the staged rotation and its publish. With a recording
/// device named ([`Self::with_recording_device`]) the same pass also resumes an
/// interrupted served-set walk. Handed to the shared
/// tokio launcher by the FFI factory + tui (so apple / android / windows / tui
/// all inherit it with no per-app glue); linux calls [`Self::run`] from its
/// own restore trigger.
///
/// **Never fatal.** A stuck sentinel must not cost the user their session: the
/// resume already isolates per-entry failures internally,
/// and a transport failure here just means the next launch retries. The count
/// is logged because a non-zero one means a removal the user asked for had
/// been silently incomplete until now.
pub struct FolderRemovalResume {
    /// The leg's native WS-RPC transport — [`Self::run`] rebuilds its
    /// [`FoldersAuthor`] from it per call. Concrete (not `R: RpcRequester`)
    /// on purpose: the `PostRestoreHook` future must be `Send`, which the
    /// seam's per-impl auto-trait inference (`requester.rs` module doc) can
    /// only prove for a concrete transport — and every consumer of this hook
    /// is a native tokio leg holding an `Arc<NestClient>` (the wasm leg drives
    /// its resume through its own `folders_resume_pending_removals` face).
    nest: Arc<NestClient>,
    /// `ActorKeypair` is not `Clone` (its secret zeroizes on drop) and
    /// Consumers take one by value, so the hook keeps the seed and
    /// re-derives per call.
    seed: Zeroizing<[u8; 32]>,
    /// The account's folder-key custody — the seat's `PlaneFolderKeys`.
    custody: Arc<dyn FolderKeyStore>,
    /// The account's mail custody — a removal rotation of a served set
    /// re-provisions its `WebdavKeysBlob` under the MSEK.
    mail: Arc<dyn fauna_client_config::MailStore>,
    /// The owner's grant log — the pass's paywall keep-alive records each
    /// grant's renewal (or the re-mint healing one the log never held) there
    /// ([`FoldersAuthor::with_grant_log`]).
    grant_log: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    /// The app's recording device (hex) — the one its Media gestures and its
    /// serve face's walk record under. `Some` wires the served-set walk, so
    /// the same launch resume also finishes an interrupted pre-serve re-seal
    /// ([`FoldersAuthor::converge_served_sets`], `webdav-server.md` § Key
    /// model (c)); `None` runs the removal resume alone.
    recording_device: Option<String>,
    /// The account's attested predecessor ids, handed to the served-set walk
    /// ([`crate::served_set_converge`]); empty falls back on the statement walk.
    predecessors: Vec<[u8; 32]>,
    /// Set once this hook has started the served-blob follower — shared
    /// with the detached leg's copy, so a seat follows custody once.
    following: Arc<std::sync::atomic::AtomicBool>,
}

impl FolderRemovalResume {
    /// Build over the leg's nest transport, the owner's 32-byte identity seed
    /// (the same seed the launcher's `self_secret` came from), the
    /// account's folder-key and mail custodies, and its grant log.
    pub fn new(
        nest: Arc<NestClient>,
        seed: [u8; 32],
        custody: Arc<dyn FolderKeyStore>,
        mail: Arc<dyn fauna_client_config::MailStore>,
        grant_log: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    ) -> Self {
        Self {
            nest,
            seed: Zeroizing::new(seed),
            custody,
            mail,
            grant_log,
            recording_device: None,
            predecessors: Vec::new(),
            following: Arc::default(),
        }
    }

    /// Hand the account's attested predecessor ids
    /// (`AccountRegistry::attested_predecessor_actor_ids`) to the served-set
    /// walk, so it admits a head a retired identity signed without a succession
    /// lookup. Empty (the default) keeps the statement-walk fallback.
    pub fn with_predecessors(mut self, predecessors: Vec<[u8; 32]>) -> Self {
        self.predecessors = predecessors;
        self
    }

    /// Name the app's recording device (hex), wiring the served-set walk into
    /// the launch resume — the same device the face passes its serve walk.
    pub fn with_recording_device(mut self, device_id: Option<String>) -> Self {
        self.recording_device = device_id;
        self
    }

    /// Run the launch folder pass at the later of its two edges (the module
    /// doc): in line when custody is readable now, else detached to run once
    /// the account runtime has assembled. Returns without waiting for the
    /// runtime either way. Must be called inside a tokio runtime.
    pub async fn run(&self, backend: Arc<FaunaMlsBackend>) {
        if self.custody.load().await.is_ok() {
            self.pass(backend).await;
            return;
        }
        let waiter = Self {
            nest: Arc::clone(&self.nest),
            seed: self.seed.clone(),
            custody: Arc::clone(&self.custody),
            mail: Arc::clone(&self.mail),
            grant_log: Arc::clone(&self.grant_log),
            recording_device: self.recording_device.clone(),
            predecessors: self.predecessors.clone(),
            following: Arc::clone(&self.following),
        };
        let backend = Arc::downgrade(&backend);
        // Detached on purpose: a dropped tokio `JoinHandle` never cancels.
        drop(tokio::spawn(async move {
            waiter.pass_once_readable(backend).await;
        }));
    }

    /// The detached leg: wait the runtime's assembly out, then run the pass
    /// if the session is still alive.
    async fn pass_once_readable(&self, backend: Weak<FaunaMlsBackend>) {
        if let Err(e) = self.custody.load_for_write().await {
            tracing::warn!(
                "folders: launch pass skipped — custody stayed unreadable, the next launch \
                 retries: {e}"
            );
            return;
        }
        let Some(backend) = backend.upgrade() else {
            return;
        };
        self.pass(backend).await;
    }

    /// The pass itself: the foreign-home seed, then the resume — the same
    /// recipe as `folders_author.rs::author` and linux's
    /// `build_folders_author`: the thin folders client, the owner's
    /// identity, the account's folder-key custody, the conversations rail's one per-actor
    /// `MlsEngine` as the group-crypto seam, and the session backend as the
    /// `FolderCommitGate` (with the plane wired it reports a live gate rather
    /// than `GateNotEngaged`).
    async fn pass(&self, backend: Arc<FaunaMlsBackend>) {
        let seeded = backend.seed_channel_homes_from_custody().await;
        if seeded > 0 {
            tracing::info!(
                "folders: seeded {seeded} channel home(s) from this member's foreign-set \
                 records once custody was readable"
            );
        }
        let keypair = ActorKeypair::from_secret(*self.seed);
        let engine = backend.engine();
        let mut author = FoldersAuthor::new(
            FoldersClient::new(Arc::clone(&self.nest)),
            keypair,
            Arc::clone(&self.custody),
            Arc::clone(&self.mail),
            engine,
        )
        .with_commit_gate(Arc::new(backend))
        .with_grant_log(Arc::clone(&self.grant_log));
        if let Some(device_id) = self.recording_device.clone() {
            author = author.with_served_set_converge(crate::served_set_converge(
                Arc::clone(&self.nest),
                &ActorKeypair::from_secret(*self.seed),
                Arc::clone(&self.custody) as Arc<dyn crate::FolderKeyReader>,
                device_id,
                self.predecessors.clone(),
            ));
        }

        // Seeded before the pass's own blob reconcile, so a sibling's serve
        // flip landing between that reconcile and the follower's first wait
        // is still a change; spawned after it, so the two never provision at
        // once.
        let follower = crate::ServedBlobFollower::new(
            FoldersClient::new(Arc::clone(&self.nest)),
            ActorKeypair::from_secret(*self.seed).actor_id(),
            Arc::clone(&self.custody),
            Arc::clone(&self.mail),
        )
        .await
        .with_principal_grants(crate::PrincipalFolderGrants::new(
            Arc::clone(&self.grant_log),
            &ActorKeypair::from_secret(*self.seed),
        ));

        match author.resume_pending_removals().await {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!("folders: resumed {n} crash-staged member removal(s) at launch")
            }
            Err(e) => {
                tracing::warn!("folders: resume_pending_removals failed at launch: {e}")
            }
        }

        self.follow_served_custody(follower).await;
    }

    /// Start the blob reconcile's re-run on the custody nudge
    /// ([`crate::ServedBlobFollower`]) — one at a time per hook: a second
    /// launch pass on the same seat (a restore re-run) finds it already
    /// following. It ends by itself when the seat's runtime does, and the
    /// next pass then starts another.
    async fn follow_served_custody(&self, follower: crate::ServedBlobFollower<Arc<NestClient>>) {
        use std::sync::atomic::Ordering;
        if self.following.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(notices) = self.custody.change_notices().await else {
            self.following.store(false, Ordering::SeqCst);
            return;
        };
        let following = Arc::clone(&self.following);
        // Detached on purpose: a dropped tokio `JoinHandle` never cancels.
        drop(tokio::spawn(async move {
            follower.run(notices).await;
            following.store(false, Ordering::SeqCst);
        }));
    }
}

#[async_trait]
impl PostRestoreHook for FolderRemovalResume {
    async fn run(&self, backend: Arc<FaunaMlsBackend>) {
        FolderRemovalResume::run(self, backend).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use fauna_conversations::backend::SelfAddress;
    use fauna_conversations::backends::mock::InertConversationsRpc;
    use fauna_core::data::{FolderPendingRemoval, FoldersConfig, ForeignFolder};
    use fauna_mls::engine::MlsEngine;
    use fauna_mls::types::ChannelId;

    use super::*;
    use crate::key_reader::{FolderKeyReader, MemoryFolderKeyStore};

    const SEED: [u8; 32] = [0x31; 32];
    const FOREIGN: [u8; 32] = [0x6f; 32];
    const HOME: &str = "https://home.example";

    /// A seat's custody over a runtime that is still assembling until `up`:
    /// a plain read refuses at once, the read a write starts from waits.
    struct AssemblingRuntime {
        up: AtomicBool,
        custody: MemoryFolderKeyStore,
    }

    #[async_trait]
    impl FolderKeyReader for AssemblingRuntime {
        async fn load(&self) -> anyhow::Result<FoldersConfig> {
            if !self.up.load(Ordering::SeqCst) {
                anyhow::bail!("the account runtime is not running");
            }
            self.custody.load().await
        }
    }

    #[async_trait]
    impl FolderKeyStore for AssemblingRuntime {
        async fn load_for_write(&self) -> anyhow::Result<FoldersConfig> {
            while !self.up.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            self.custody.load().await
        }
        async fn merge(&self, replica: FoldersConfig) -> anyhow::Result<FoldersConfig> {
            self.custody.merge(replica).await
        }
        async fn settle_removal(
            &self,
            removal: FolderPendingRemoval,
        ) -> anyhow::Result<FoldersConfig> {
            self.custody.settle_removal(removal).await
        }
    }

    /// The hook over a custody holding one foreign-set record, and the
    /// backend whose custody sink reads that same custody — a seat at launch.
    /// The nest is never reached: nothing here asserts past the seed.
    fn seat(
        up: bool,
    ) -> (
        FolderRemovalResume,
        Arc<AssemblingRuntime>,
        Arc<FaunaMlsBackend>,
    ) {
        let mut cfg = FoldersConfig::default();
        crate::custody::record_foreign_set(
            &mut cfg,
            ForeignFolder {
                channel_id: FOREIGN,
                home_nest_url: HOME.into(),
                ..Default::default()
            },
            1_000,
        );
        let custody = Arc::new(AssemblingRuntime {
            up: AtomicBool::new(up),
            custody: MemoryFolderKeyStore::with(cfg),
        });
        let nest = NestClient::new(
            "ws://127.0.0.1:1/ws".into(),
            ActorKeypair::from_secret(SEED),
        );
        let keypair = ActorKeypair::from_secret(SEED);
        let backend = Arc::new(FaunaMlsBackend::new_shared(
            Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(SEED)).expect("engine")),
            Arc::new(InertConversationsRpc),
            SelfAddress::new(""),
            keypair.actor_id(),
        ));
        backend.set_folder_custody_sink(Arc::new(crate::NestFolderCustodySink::new(
            Arc::clone(&nest),
            Arc::clone(&custody) as Arc<dyn FolderKeyStore>,
        )));
        let hook = FolderRemovalResume::new(
            nest,
            SEED,
            Arc::clone(&custody) as Arc<dyn FolderKeyStore>,
            Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            Arc::new(
                fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(
                    keypair.actor_id(),
                ),
            ),
        );
        (hook, custody, backend)
    }

    /// A launch whose runtime is still assembling: the hook returns without
    /// waiting for it — the receive loop is behind this call — and the pass
    /// runs once custody is readable, where it used to be skipped until a
    /// launch that won the race.
    #[tokio::test]
    async fn a_pass_due_before_the_runtime_assembles_runs_once_it_has_without_holding_the_launch() {
        let (hook, custody, backend) = seat(false);
        // The restore's own seed, over the unreadable custody: nothing.
        assert_eq!(backend.seed_channel_homes_from_custody().await, 0);

        tokio::time::timeout(Duration::from_secs(5), hook.run(Arc::clone(&backend)))
            .await
            .expect("the hook returns while the runtime is still assembling");
        assert_eq!(
            backend.channel_home_url(&ChannelId(FOREIGN)),
            None,
            "nothing ran over the unreadable custody"
        );

        custody.up.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), async {
            while backend.channel_home_url(&ChannelId(FOREIGN)).is_none() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the detached pass runs once the runtime is up");
        assert_eq!(
            backend.channel_home_url(&ChannelId(FOREIGN)).as_deref(),
            Some(HOME)
        );
    }

    /// The session torn down while the pass waits for the runtime: the
    /// detached leg ends rather than run over a dead session's engine.
    #[tokio::test]
    async fn a_session_torn_down_during_the_wait_ends_the_detached_pass() {
        let (hook, custody, backend) = seat(false);
        hook.run(Arc::clone(&backend)).await;
        let weak = Arc::downgrade(&backend);
        drop(backend);
        assert!(
            weak.upgrade().is_none(),
            "the detached leg holds the backend weakly across its wait"
        );
        custody.up.store(true, Ordering::SeqCst);
    }
}
