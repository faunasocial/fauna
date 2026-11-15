//! UniFFI façade for the `fauna.drafts.{get,put}` kinds — the native-client
//! `__drafts` persistence seam the draft-persistence v2 client legs drive. Two
//! handles over the shared `fauna-client-drafts` crate (both seal under the
//! owner's `BackupKey` and call `fauna.drafts.{get,put}` over the shared
//! `NestClient`); the native twins of the Rust-native Linux app reaching the
//! crate directly:
//!
//! * [`FfiDraftsClient`] — the thin, stateless per-call `load(path)` / `save(path,
//!   bytes)` over `DraftsClient`. Used by the windows leg (a C#-side launch-gate +
//!   debounce wraps it).
//! * [`FfiDraftsSync`] — the **canonical** stateful wrapper over `DraftsSync`
//!   (launch gate + last-saved baseline), built once per session and held, so the
//!   per-app leg stays pure trigger glue (restore on launch + a debounced
//!   save). Used by the android leg; macos/ios consume it next, and the windows
//!   convergence follow-up moves windows onto it too (reserved-folders.md § Drafts Sync).
//!
//! Construct via [`crate::nest_client::FfiNestClient::drafts`] /
//! [`crate::nest_client::FfiNestClient::drafts_sync`]; the methods are exposed to
//! Swift as `async throws` and Kotlin as `suspend fun`.
//!
//! Per-app glue is just the trigger; the seal + the WS call are NOT
//! re-implemented per client (`docs/goal/behavior/reserved-folders.md` § Drafts Sync).
//! The behavioural round-trip (seal → `fauna.drafts.put` → `fauna.drafts.get` →
//! unseal, byte-equal; `None` on an empty rail; independent paths) and the
//! `DraftsSync` gate/baseline are owned + tested by the shared
//! `fauna-client-drafts` crate (`store.rs` / `sync.rs` tests) and exercised
//! end-to-end by the tier_3 e2e; these wrappers are thin transport-bound
//! delegations with no logic of their own beyond the `DraftsClientError` →
//! `FfiError` mapping.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_drafts::{DraftsClient, DraftsClientError, DraftsSync};
use fauna_conversations::ConversationsManager;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::RpcRequester;

use crate::{FfiError, general_err};

/// `fauna_client_drafts::autosave_debounce` → the shared draft-autosave
/// debounce window in whole milliseconds, so every door-crossing app's
/// composer coalesces edits on the same cadence as the Rust-native shells
/// (priority #2 — one shared window, no per-app drift;
/// `docs/goal/behavior/reserved-folders.md` § Drafts Sync). `u32`, matching
/// the web `autosaveDebounceMs` face's return shape.
///
/// It reads the shared accessor rather than the constant behind it, so the
/// harness's window seam reaches apple, android and windows through the one
/// door they already call — see that function for why the seam lengthens the
/// window rather than shortening it. Called per schedule, so the answer is
/// live rather than latched at launch.
#[uniffi::export]
pub fn autosave_debounce_ms() -> u32 {
    fauna_client_drafts::autosave_debounce().as_millis() as u32
}

/// UniFFI handle for the `fauna.drafts.{get,put}` kinds. Thin wrapper over the
/// shared `fauna_client_drafts::DraftsClient` bound to this connection's
/// `NestClient` and the owner's derived `BackupKey`. Cheap to hold (a transport
/// handle + the derived key); construct one per actor via
/// [`FfiNestClient::drafts`](crate::nest_client::FfiNestClient::drafts).
#[derive(uniffi::Object)]
pub struct FfiDraftsClient {
    inner: DraftsClient<Arc<NestClient>>,
}

impl FfiDraftsClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>, keypair: &ActorKeypair) -> Arc<Self> {
        Arc::new(Self {
            inner: DraftsClient::new(nest, keypair),
        })
    }
}

#[fauna_uniffi_async::export]
impl FfiDraftsClient {
    /// `fauna.drafts.get` — fetch + unseal the calling actor's draft blob for
    /// `path` (e.g. `"conversations"`). `None` when the actor has never persisted
    /// drafts for this rail (first run) — the caller keeps its empty store. A
    /// present-but-undecryptable blob is a hard error, never masked as "no drafts".
    /// Hand the returned bytes to `ConversationsManager.restore_drafts_at`, with
    /// the identity epoch read before this call.
    pub async fn load(&self, path: String) -> Result<Option<Vec<u8>>, FfiError> {
        self.inner.load(&path).await.map_err(general_err)
    }

    /// `fauna.drafts.put` — seal + persist `snapshot_bytes` (from
    /// `ConversationsManager.drafts_snapshot_bytes`) for the calling actor's
    /// `path` rail, overwriting any prior blob. Idempotent overwrite, so an
    /// unchanged draft set re-uploads with no effect.
    pub async fn save(&self, path: String, snapshot_bytes: Vec<u8>) -> Result<(), FfiError> {
        self.inner
            .save(&path, &snapshot_bytes)
            .await
            .map_err(general_err)
    }
}

/// UniFFI handle for one actor's per-rail draft *autosync* — the **canonical**
/// stateful layer above [`FfiDraftsClient`]. Wraps the shared
/// `fauna_client_drafts::DraftsSync` (the launch gate + last-saved baseline), so
/// it must be built once at login and held for the session: a fresh instance per
/// call would re-close the gate and lose the dedup baseline. Where
/// [`FfiDraftsClient`] is a thin per-call `load`/`save`, this owns the two
/// no-data-loss safety properties (never PUT before the launch GET succeeds;
/// don't re-upload an unchanged set), so the per-app leg stays pure trigger
/// glue. Construct via
/// [`FfiNestClient::drafts_sync`](crate::nest_client::FfiNestClient::drafts_sync).
#[derive(uniffi::Object)]
pub struct FfiDraftsSync {
    sync: DraftsSync<Arc<NestClient>>,
}

impl FfiDraftsSync {
    /// Build over the live connection's transport + identity, for one rail
    /// (`"conversations"`; later `"posts"` / `"events"`). The at-rest `BackupKey`
    /// is derived inside the wrapped `DraftsClient` from the connection secret —
    /// the same secret the WS connection authenticated with (mirrors
    /// `FfiCaldavClient`'s keypair rebuild from `nest.auth()`).
    pub(crate) fn from_nest(nest: Arc<NestClient>, rail: String) -> Arc<Self> {
        Arc::new(Self {
            sync: drafts_sync_from_nest(nest, rail),
        })
    }
}

/// The `DraftsSync` half of [`FfiDraftsSync::from_nest`] / [`crate::event_drafts::
/// FfiEventDraftsSync::from_nest`] — both rebuild the same connection-derived
/// keypair the same way, differing only in which rail they hand `DraftsSync`.
/// Infallible: called only on an already-authenticated connection, where the
/// keypair is always present.
pub(crate) fn drafts_sync_from_nest(
    nest: Arc<NestClient>,
    rail: String,
) -> DraftsSync<Arc<NestClient>> {
    let secret = *nest
        .auth()
        .keypair()
        .expect("identity keypair required for drafts")
        .secret_bytes();
    let keypair = ActorKeypair::from_secret(secret);
    DraftsSync::new(nest, &keypair, rail)
}

#[fauna_uniffi_async::export]
impl FfiDraftsSync {
    /// Fetch + unseal this rail's persisted drafts for the launch / cross-device
    /// catch-up (`fauna.drafts.get`). Returns the canonical snapshot bytes the
    /// caller hands to `ConversationsManager.restoreDraftsAt` (with the
    /// `identityEpoch()` it read before this call), or `None` on first
    /// run. Records the baseline + lifts the save gate; a transport/seal failure
    /// leaves the gate closed (so a later `save_if_changed` can't clobber the
    /// unread blob) and surfaces the error string.
    pub async fn load(&self) -> Result<Option<Vec<u8>>, FfiError> {
        self.sync.load().await.map_err(general_err)
    }

    /// Seal + persist `snapshot` for this rail (`fauna.drafts.put`) **iff** a
    /// launch [`load`](Self::load) has succeeded *and* `snapshot` differs from the
    /// last-saved baseline. Returns `true` when it wrote, `false` when skipped (the
    /// pre-load gate or an unchanged set). The client glue calls this from a
    /// debounce with `ConversationsManager.draftsSnapshotBytes()`.
    pub async fn save_if_changed(&self, snapshot: Vec<u8>) -> Result<bool, FfiError> {
        self.sync
            .save_if_changed(&snapshot)
            .await
            .map_err(general_err)
    }

    /// The conversations rail's **leave-door save**
    /// (`docs/goal/behavior/reserved-folders.md` § The leave-flush promise):
    /// the shared `DraftsSync::save_on_leave` with this rail's FILL rule,
    /// `ConversationsManager::fill_drafts_for_leave` — so a door taken before
    /// the launch load has lifted the save gate still saves, by one read of
    /// its own, a fill of `manager` as the launch restore would, then the
    /// save. With the gate already open it is `save_if_changed(captured)`.
    ///
    /// The door hands over what it captured AT the door, before it tore
    /// anything down: `captured` is `manager.draftsSnapshotBytes()` and
    /// `epoch` is `manager.identityEpoch()`, both read then — a teardown
    /// that clears the manager before this runs moves its epoch, and the
    /// fill then fills nothing, so `captured` is what is saved. The caller
    /// bounds the call (the shared leave budget); a failed read saves
    /// nothing. Returns `true` when it wrote.
    pub async fn save_conversations_on_leave(
        &self,
        manager: Arc<ConversationsManager>,
        epoch: u64,
        captured: Vec<u8>,
    ) -> Result<bool, FfiError> {
        conversations_leave_save(&self.sync, &manager, epoch, captured)
            .await
            .map_err(general_err)
    }
}

/// The body of [`FfiDraftsSync::save_conversations_on_leave`], over any
/// transport — the seam its tests drive over an in-memory nest.
async fn conversations_leave_save<R: RpcRequester>(
    sync: &DraftsSync<R>,
    manager: &ConversationsManager,
    epoch: u64,
    captured: Vec<u8>,
) -> Result<bool, DraftsClientError<R::Error>> {
    sync.save_on_leave(captured, |resting| {
        manager.fill_drafts_for_leave(epoch, &resting)
    })
    .await
}

impl FfiDraftsSync {
    /// The wrapped shared sync — for a sibling face in this crate that drives a
    /// shared body over it (the posts rail's leave save,
    /// `FfiFeedManager::save_drafts_on_leave`). Outside the export block.
    pub(crate) fn drafts_sync(&self) -> &DraftsSync<Arc<NestClient>> {
        &self.sync
    }
}

/// The body of the posts rail's leave face, `FfiFeedManager::save_drafts_on_leave`,
/// over any transport — the seam its tests drive over an in-memory nest. The
/// face itself lives on the feed manager, not on [`FfiDraftsSync`], so this
/// type's foreign interface is the same in every build flavor (a flavor that
/// excises the feed plane keeps `drafts`).
#[cfg(feature = "feed-manager")]
pub(crate) async fn posts_leave_save<R: RpcRequester, N: RpcRequester + Clone>(
    sync: &DraftsSync<R>,
    manager: &fauna_feed::FeedManager<N>,
    captured: Vec<u8>,
) -> Result<bool, DraftsClientError<R::Error>> {
    sync.save_on_leave(captured, |resting| manager.fill_drafts_for_leave(&resting))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exported window IS the shared constant — a door-crossing app that
    /// re-declared its own literal would silently decouple from the value every
    /// other app reads (the failure mode `reserved-folders.md` § Drafts Sync
    /// names this face to end).
    #[test]
    fn autosave_debounce_mirrors_the_shared_constant() {
        assert_eq!(
            autosave_debounce_ms() as u128,
            fauna_client_drafts::AUTOSAVE_DEBOUNCE.as_millis()
        );
    }

    use fauna_client_drafts::testing::{MemoryDraftsNest, SharedDraftsNest};
    use fauna_client_testkit::block_on;

    fn keypair() -> ActorKeypair {
        ActorKeypair::from_secret([7u8; 32])
    }

    fn sync(nest: &Arc<MemoryDraftsNest>, rail: &str) -> DraftsSync<SharedDraftsNest> {
        DraftsSync::new(SharedDraftsNest(Arc::clone(nest)), &keypair(), rail)
    }

    /// `bytes` resting on `nest` for `rail`, as another device of the same
    /// identity would have left them.
    fn rest(nest: &Arc<MemoryDraftsNest>, rail: &str, bytes: &[u8]) {
        let other = sync(nest, rail);
        block_on(other.load()).unwrap();
        assert!(block_on(other.save_if_changed(bytes)).unwrap());
    }

    fn resting(nest: &Arc<MemoryDraftsNest>, rail: &str) -> Option<Vec<u8>> {
        block_on(sync(nest, rail).load()).unwrap()
    }

    fn puts(nest: &MemoryDraftsNest) -> u32 {
        *nest.puts.lock().unwrap()
    }

    fn conversations_with_new_thread(body: &str) -> Arc<ConversationsManager> {
        let manager = ConversationsManager::new();
        manager.start_new_conversation();
        manager.set_new_thread_body(body.to_string());
        manager
    }

    /// A conversations door taken before the launch load lifted the gate
    /// still saves what the user typed — the gap the face exists to close
    /// (`save_if_changed` alone writes nothing behind a shut gate).
    #[test]
    fn a_conversations_door_before_the_launch_load_saves_the_typed_draft() {
        let nest = Arc::new(MemoryDraftsNest::default());
        let rail = fauna_protocol::drafts::RAIL_CONVERSATIONS;
        rest(
            &nest,
            rail,
            &conversations_with_new_thread("resting").drafts_snapshot_bytes(),
        );

        let typed = conversations_with_new_thread("typed before the gate opened");
        let captured = typed.drafts_snapshot_bytes();
        let leaving = sync(&nest, rail);
        let wrote = block_on(conversations_leave_save(
            &leaving,
            &typed,
            typed.identity_epoch(),
            captured.clone(),
        ))
        .unwrap();

        assert!(wrote);
        assert_eq!(resting(&nest, rail), Some(captured));
    }

    /// An untouched composer takes the resting draft, so the door writes
    /// nothing over it — the fill rule, not the captured empty snapshot.
    #[test]
    fn a_conversations_door_with_nothing_typed_never_overwrites_the_resting_draft() {
        let nest = Arc::new(MemoryDraftsNest::default());
        let rail = fauna_protocol::drafts::RAIL_CONVERSATIONS;
        let resting_bytes = conversations_with_new_thread("resting").drafts_snapshot_bytes();
        rest(&nest, rail, &resting_bytes);
        let before = puts(&nest);

        let untouched = ConversationsManager::new();
        let wrote = block_on(conversations_leave_save(
            &sync(&nest, rail),
            &untouched,
            untouched.identity_epoch(),
            untouched.drafts_snapshot_bytes(),
        ))
        .unwrap();

        assert!(!wrote);
        assert_eq!(puts(&nest), before);
        assert_eq!(resting(&nest, rail), Some(resting_bytes));
    }

    #[cfg(feature = "feed-manager")]
    fn feed_with(text: &str) -> fauna_feed::FeedManager<Arc<SharedDraftsNest>> {
        let manager = fauna_feed::FeedManager::new(
            Arc::new(SharedDraftsNest(Arc::new(MemoryDraftsNest::default()))),
            [9u8; 32],
        );
        manager.update_compose(text.to_string(), String::new(), None);
        manager
    }

    /// The posts rail's twin of the conversations door test.
    #[cfg(feature = "feed-manager")]
    #[test]
    fn a_posts_door_before_the_launch_load_saves_the_typed_draft() {
        let nest = Arc::new(MemoryDraftsNest::default());
        let rail = fauna_protocol::drafts::RAIL_POSTS;
        rest(&nest, rail, &feed_with("resting").drafts_snapshot_bytes());

        let typed = feed_with("typed before the gate opened");
        let captured = typed.drafts_snapshot_bytes();
        let wrote = block_on(posts_leave_save(
            &sync(&nest, rail),
            &typed,
            captured.clone(),
        ))
        .unwrap();

        assert!(wrote);
        assert_eq!(resting(&nest, rail), Some(captured));
    }

    #[cfg(feature = "feed-manager")]
    #[test]
    fn a_posts_door_with_nothing_typed_never_overwrites_the_resting_draft() {
        let nest = Arc::new(MemoryDraftsNest::default());
        let rail = fauna_protocol::drafts::RAIL_POSTS;
        let resting_bytes = feed_with("resting").drafts_snapshot_bytes();
        rest(&nest, rail, &resting_bytes);
        let before = puts(&nest);

        let untouched = feed_with("");
        let captured = untouched.drafts_snapshot_bytes();
        let wrote = block_on(posts_leave_save(&sync(&nest, rail), &untouched, captured)).unwrap();

        assert!(!wrote);
        assert_eq!(puts(&nest), before);
        assert_eq!(resting(&nest, rail), Some(resting_bytes));
    }
}
