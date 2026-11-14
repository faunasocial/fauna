//! Stateful per-rail draft autosync: [`DraftsSync`] wraps a [`DraftsClient`]
//! with the small amount of state every app leg needs *identically* — a
//! "have we loaded yet?" gate plus the last-saved baseline — so the six
//! per-app legs stay pure trigger glue (a debounce timer + the manager's
//! `drafts_snapshot_bytes()` / `restore_drafts`) and never re-implement the two
//! safety properties below. Writing them once, here, is the point: copy-pasting
//! the load gate across six apps is exactly where a subtle break would cost
//! user data.
//!
//! 1. **Never save before the launch load completes.** The conversations
//!    manager fires `notify()` for non-draft reasons during startup too, so an
//!    autosave observer can tick before the launch GET returns. If that stray
//!    tick PUT the (empty) snapshot, it would clobber the user's nest-stored
//!    drafts *before* the GET ever read them — a "No user-data loss" violation
//!    (`docs/goal/architecture/version-compatibility.md`).
//!    [`DraftsSync::save_if_changed`] is therefore a no-op until
//!    [`DraftsSync::load`] has run.
//! 2. **Don't re-upload an unchanged draft set.** `DraftStore::snapshot_bytes`
//!    is byte-stable for equal logical state, so an unchanged set compares equal
//!    to the baseline and is skipped — no `sync_changes` churn pushed to the
//!    user's other devices for a no-op edit, nor for the redundant tick that the
//!    post-restore `notify()` produces.
//!
//! Deliberately manager-agnostic (no `fauna-conversations` dependency): [`load`]
//! returns the snapshot bytes for the caller to hand to
//! `ConversationsManager::restore_drafts`, and [`save_if_changed`] takes the
//! caller's `drafts_snapshot_bytes()`. The rail `path` (e.g. `"conversations"`)
//! is fixed at construction.
//!
//! [`load`]: DraftsSync::load
//! [`save_if_changed`]: DraftsSync::save_if_changed

use std::sync::Mutex;
use std::time::Duration;

use fauna_core::identity::ActorKeypair;
use fauna_protocol::RpcRequester;

use crate::store::{DraftsClient, DraftsClientError};

/// How long the composer must be quiescent before an edited draft set is
/// persisted — long enough that ordinary typing coalesces into one upload,
/// short enough that a draft is safe within a couple of seconds of the user
/// pausing.
///
/// The *timer* stays per-app glue, as the module doc says — GTK's
/// `timeout_add_local_once` plus a generation counter on linux, a `select!`
/// against a sleep on tui. What lives here is the **window**, which is one
/// product decision rather than one per rail per app.
///
/// It was five copies in Rust alone — linux ×2 and tui ×3, one per rail, each
/// under a comment promising it matched the other legs. That promise is worth
/// what a promise no build checks is ever worth: across the whole fleet the
/// knob had already reached **three different values** (1500 ms on the Rust
/// legs, android and apple; 1200 ms on web; ~600 ms on windows), which
/// `reserved-folders.md` § Drafts Sync records as a fact rather than as a
/// ratified decision. Apple's copy is the sharpest illustration — its comment
/// says it matches the others and then calls web's 1.2 s "the same order",
/// noticing the divergence and waving it through. The rail with the longer
/// window quietly loses more work when an app dies mid-burst, so a spread
/// nobody chose is a real difference in how much typing survives a crash.
///
/// **Scope of this constant: the single source of truth for every shell.**
/// The Rust-native shells (linux, tui) reach it directly; the door-crossing
/// apps (android, apple, web, windows) read the same value through
/// `fauna-ffi::autosave_debounce_ms` / `fauna-wasm`'s `autosaveDebounceMs` and
/// hold no literal of their own (`docs/goal/behavior/reserved-folders.md` §
/// Drafts Sync).
///
/// Not a *safety* bound: properties 1 and 2 above are what protect the user's
/// drafts, and they hold at any window. This is the coalescing knob.
pub const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// How long one rail's leave-flush may take before the door goes on without it
/// (`docs/goal/behavior/reserved-folders.md` § The leave-flush promise): a slow
/// or unreachable nest must not hang quitting or signing out. Each rail is
/// bounded on its own, so one rail's timeout never costs another its save.
/// tui, web and linux read it here; windows' teardown flush still carries its own
/// 5 s budget.
pub const LEAVE_FLUSH_BUDGET: Duration = Duration::from_secs(3);

/// The env seam the e2e harness uses to neutralise the debounce for a test, in
/// whole milliseconds. Gated, so a `strings` sweep of a release artifact finds
/// not even the name (convention 15's verification method reads the built
/// artifact, not the source).
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub const E2E_AUTOSAVE_DEBOUNCE_ENV: &str = "FAUNA_E2E_DRAFTS_AUTOSAVE_DEBOUNCE_MS";

/// **The one door every shell reads for the autosave window.** The production
/// answer is [`AUTOSAVE_DEBOUNCE`] verbatim; a test-capable build additionally
/// honours [`E2E_AUTOSAVE_DEBOUNCE_ENV`].
///
/// Read this rather than the constant. The Rust-native shells (linux, tui) call
/// it directly; the door-crossing apps (android, apple, web, windows) reach the
/// same answer through `fauna-ffi`'s `autosave_debounce_ms` / `fauna-wasm`'s
/// `autosaveDebounceMs`. All seven therefore read one *window*, and the seam
/// reaches six of them at once without a line of app-side plumbing — which is
/// the point of putting it here rather than at a call site (priority #2;
/// `docs/goal/architecture/e2e-automation-surface-gating.md` § The source-IMAP
/// trust seed states the general shape).
///
/// **web takes the seam through another door:** `wasm32-unknown-unknown` has
/// no process environment, so `std::env::var` is always `Err(NotPresent)`
/// there and this answers [`AUTOSAVE_DEBOUNCE`]; the harness writes the same
/// name into the page origin's `localStorage` instead (the escrow-trust seed's
/// shape), which `fauna-wasm`'s `e2eDraftsAutosaveWindowMs` reads through
/// [`parse_e2e_autosave_debounce`]. Without it web keeps its own
/// `__faunaTestAgent` 150 ms cadence, which shortens where this lengthens.
///
/// **What the seam is FOR is lengthening, not shortening.** A leave-flush
/// witness has to prove the draft reached the nest via the app's leave door and
/// *not* via the debounce landing on its own. Every such leg but one is sound
/// because the process is gone, so the debounce provably cannot have fired;
/// iOS's door leaves the app running, so it needs the window pushed out past
/// the test instead — then any save at all is necessarily the flush. Shortening
/// the window to race it is what convention 14 forbids: a bound under the
/// production 1.5 s trades a false green for a load-dependent false red.
///
/// Cheap enough to call per schedule (one `getenv` in a test build, nothing in
/// a shipped one), which is what keeps it a live read rather than a value an
/// app latches at launch.
pub fn autosave_debounce() -> Duration {
    e2e_autosave_debounce_override().unwrap_or(AUTOSAVE_DEBOUNCE)
}

/// Test-capable builds only — see [`autosave_debounce`].
///
/// The value grammar is [`parse_e2e_autosave_debounce`]'s.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
fn e2e_autosave_debounce_override() -> Option<Duration> {
    parse_e2e_autosave_debounce(&std::env::var(E2E_AUTOSAVE_DEBOUNCE_ENV).ok()?)
}

/// The seam's value grammar, shared by both places the harness puts it: the
/// process environment ([`autosave_debounce`]) and web's page-origin
/// `localStorage` under the same name (`fauna-wasm`'s
/// `e2eDraftsAutosaveWindowMs`), since a page reads no environment. Empty or
/// whitespace is "not set".
///
/// A malformed value **panics** rather than falling back: only the e2e harness
/// sets this, so a typo is a harness bug, and silently serving the production
/// 1.5 s would turn it into a flaky witness that reads like a product gap.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub fn parse_e2e_autosave_debounce(raw: &str) -> Option<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let ms: u64 = trimmed.parse().unwrap_or_else(|e| {
        panic!(
            "{E2E_AUTOSAVE_DEBOUNCE_ENV} is set to {trimmed:?}, which is not a \
             whole number of milliseconds: {e}. Only the e2e harness sets this \
             variable, so this is a harness bug — failing loudly beats serving \
             the production window to a test that asked not to have it."
        )
    });
    Some(Duration::from_millis(ms))
}

/// The production twin: a shipped build has no autosave-window seam, so the
/// constant is the whole answer.
///
/// Convention 15's "same-signature no-op twin wherever the caller is plumbing
/// the app compiles unconditionally" — [`autosave_debounce`] is exactly that
/// plumbing. Two bodies rather than a `#[cfg]` inside one keeps the shipped
/// path free of a branch that exists only to be compiled away, and makes the
/// absence explicit where a reader finds the presence.
#[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
fn e2e_autosave_debounce_override() -> Option<Duration> {
    None
}

/// The waits between a launch load's attempts after a transport failure —
/// about three minutes in all, then the failure is returned and the next
/// launch tries again.
///
/// Why a retry at all: the launch `fauna.drafts.get` carries the kind's 5 s
/// deadline, and the save gate stays shut until a load succeeds. Before this, a
/// single slow answer at launch — a loaded box, a reconnect, the first boot
/// after a reload — left the session with no synced drafts and no autosave
/// until the user restarted the app (found on web, where a reload's first read
/// routinely outlived the deadline).
///
/// Bounded, not endless, because the loop holds the session's transport: a load
/// begun for an account the user has since left would otherwise keep that
/// client alive. Late delivery itself is harmless — the caller's identity-epoch
/// check drops a reply for a departed account.
pub const LAUNCH_LOAD_RETRY_DELAYS: [Duration; 10] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(30),
];

/// Per-rail load-gate + last-saved baseline guarding a [`DraftsClient`]. One
/// instance per actor per rail on each Fauna app, held for the app's
/// lifetime (cheap: a `DraftsClient` + a small mutex).
pub struct DraftsSync<R: RpcRequester> {
    client: DraftsClient<R>,
    rail: String,
    state: Mutex<State>,
    /// Retired-identity `BackupKey`s this device holds, offered on the load
    /// path only. Empty for every identity that never succeeded, which is the
    /// overwhelmingly common case and pays nothing.
    predecessors: Vec<crate::BackupKey>,
}

#[derive(Default)]
struct State {
    /// Set true once [`DraftsSync::load`] has run. Until then every
    /// `save_if_changed` is a no-op (safety property 1).
    loaded: bool,
    /// The bytes most recently persisted (or loaded). `None` means "first run,
    /// nothing persisted yet" — distinct from `Some(empty_snapshot)`.
    last_saved: Option<Vec<u8>>,
}

impl<R: RpcRequester> DraftsSync<R> {
    /// Build over a transport handle, the user's identity keypair, and the rail
    /// key (`"conversations"`, later `"posts"` / `"events"`). The at-rest
    /// `BackupKey` is derived inside the wrapped [`DraftsClient`].
    pub fn new(nest: R, keypair: &ActorKeypair, rail: impl Into<String>) -> Self {
        Self {
            client: DraftsClient::new(nest, keypair),
            rail: rail.into(),
            state: Mutex::new(State::default()),
            predecessors: Vec::new(),
        }
    }

    /// Offer retired-identity `BackupKey`s to [`load`](Self::load), so a
    /// successor opens a rail its predecessor sealed instead of hard-erroring
    /// on it.
    ///
    /// ⚠ **Read-only, and deliberately so — this is not a seal root.** The
    /// re-seal pass
    /// ([`DraftsClient::rekey_rail_from_predecessors`](crate::DraftsClient::rekey_rail_from_predecessors))
    /// stays the single writer; a new seal under a retired key is
    /// unrepresentable here rather than merely discouraged, exactly as
    /// `LabelCustody::with_predecessors` keeps it on the label plane.
    ///
    /// ⚠ **Resolve the walk ONCE per session and pass it in**, as the app hook
    /// already does for the media, label, byte and `__mls` planes — two
    /// resolutions of one fact is the silent divergence
    /// `AccountRegistry::predecessor_backup_keys` exists to prevent.
    #[must_use]
    pub fn with_predecessors(mut self, keys: Vec<crate::BackupKey>) -> Self {
        self.predecessors = keys;
        self
    }

    /// How many retired keys this rail will offer on the read path.
    ///
    /// Exists for the **per-app call-site pin** and nothing else: this crate's
    /// own tests cannot see an app stop passing the walk, which is precisely
    /// the vacuity that let `FileDownloadKeys::predecessor_backup_keys` ship
    /// with zero production writers while five tier_1 tests stayed green. The
    /// twin of `LabelCustody::predecessor_count`, and asserted the same way.
    #[must_use]
    pub fn predecessor_count(&self) -> usize {
        self.predecessors.len()
    }

    /// Fetch + unseal this rail's drafts for the launch / cross-device catch-up.
    /// Records the loaded bytes as the baseline (so the immediate post-restore
    /// `save_if_changed` tick is a no-op) and lifts the save gate. The caller
    /// hands the returned bytes to `ConversationsManager::restore_drafts_at`,
    /// with the identity epoch it read before calling this;
    /// `Ok(None)` is first run (keep the empty store).
    ///
    /// **A transport failure is retried** on [`LAUNCH_LOAD_RETRY_DELAYS`]
    /// before it is returned, so a nest that is slow or unreachable for a moment at launch
    /// does not cost the user their synced drafts for the whole session
    /// (`conversation-drafts.md` § Persistence, *A launch load lost to the
    /// transport is retried*). A seal failure is never retried: the same bytes
    /// would fail the same way. The future can therefore run for minutes —
    /// spawn it, never await it on a path that holds a page or a build back.
    ///
    /// A late load is safe because every rail's restore FILLS — it never
    /// overwrites a draft the user is composing into: the conversations rail's
    /// `DraftStore::restore_from_bytes`, the posts rail's `PostDrafts::fill`
    /// and the events rail's `EventDrafts::fill`. A new rail owes the same
    /// before it may load here.
    pub async fn load(&self) -> Result<Option<Vec<u8>>, DraftsClientError<R::Error>> {
        self.load_retrying(&LAUNCH_LOAD_RETRY_DELAYS, fauna_sleep::sleep)
            .await
    }

    /// [`load`](Self::load) over an explicit schedule and sleep — the seam the
    /// tests drive without a timer.
    async fn load_retrying<S, F>(
        &self,
        delays: &[Duration],
        mut sleep: S,
    ) -> Result<Option<Vec<u8>>, DraftsClientError<R::Error>>
    where
        S: FnMut(Duration) -> F,
        F: core::future::Future<Output = ()>,
    {
        let mut delays = delays.iter();
        let loaded = loop {
            match self
                .client
                .load_with_predecessors(&self.rail, &self.predecessors)
                .await
            {
                Ok(loaded) => break loaded,
                Err(DraftsClientError::Transport(e)) => match delays.next() {
                    Some(delay) => sleep(*delay).await,
                    None => return Err(DraftsClientError::Transport(e)),
                },
                Err(seal) => return Err(seal),
            }
        };
        let mut st = self.state.lock().unwrap();
        st.loaded = true;
        st.last_saved = loaded.clone();
        Ok(loaded)
    }

    /// **The leave door's own read, when the launch load has not landed yet**
    /// (`docs/goal/behavior/reserved-folders.md` § The leave-flush promise). A
    /// leave door that finds the save gate shut would otherwise save nothing:
    /// the launch load is retried on a backoff of minutes, and a door taken in
    /// the first seconds of a session — on a loaded box, or the first boot
    /// after a web reload — would drop the edit still inside the debounce.
    ///
    /// One attempt, never the retry schedule: a leave door is bounded
    /// (`LEAVE_FLUSH_BUDGET`) and must not wait on a backoff. A load that
    /// lands lifts the gate and records the baseline exactly as
    /// [`load`](Self::load) does, so safety property 1 holds unchanged — the
    /// door saves only over drafts it has read. The caller FILLS its rail from
    /// the returned bytes (the same fill every rail's launch restore applies),
    /// then snapshots and calls [`save_if_changed`](Self::save_if_changed).
    ///
    /// `Ok(None)` means there is nothing to fill: the gate was already open (no
    /// read is made) or the rail is empty on the nest. An error leaves the gate
    /// shut, and the caller goes on without the save.
    pub async fn load_for_leave(&self) -> Result<Option<Vec<u8>>, DraftsClientError<R::Error>> {
        if self.state.lock().unwrap().loaded {
            return Ok(None);
        }
        let loaded = self
            .client
            .load_with_predecessors(&self.rail, &self.predecessors)
            .await?;
        let mut st = self.state.lock().unwrap();
        // A launch load that landed while this read was in flight already set
        // the baseline; either answer is the nest's state before any save.
        st.loaded = true;
        st.last_saved = loaded.clone();
        Ok(loaded)
    }

    /// **The leave door's save, one body for every rail and every app**
    /// (`docs/goal/behavior/reserved-folders.md` § The leave-flush promise):
    /// [`load_for_leave`](Self::load_for_leave), then
    /// [`save_if_changed`](Self::save_if_changed).
    ///
    /// `captured` is the rail's snapshot as the door found it. When the read
    /// hands back a resting draft, `fill` gets it and returns the snapshot to
    /// save instead: the rail's own FILL rule applied to the live compose (the
    /// one its launch restore applies), then re-snapshotted. An untouched
    /// composer takes the resting draft, so the snapshot equals what rests and
    /// nothing is written; a composer the user typed into keeps their text and
    /// is saved. With the gate already open, or nothing resting, `fill` is
    /// never called and `captured` is saved as it is.
    ///
    /// The caller bounds the whole call by
    /// [`LEAVE_FLUSH_BUDGET`](crate::LEAVE_FLUSH_BUDGET); a failed read is
    /// returned and nothing is saved (the gate stays shut).
    pub async fn save_on_leave(
        &self,
        captured: Vec<u8>,
        fill: impl FnOnce(Vec<u8>) -> Vec<u8>,
    ) -> Result<bool, DraftsClientError<R::Error>> {
        let snapshot = match self.load_for_leave().await? {
            Some(resting) => fill(resting),
            None => captured,
        };
        self.save_if_changed(&snapshot).await
    }

    /// Seal + persist `snapshot` for this rail **iff** a launch [`load`](Self::load)
    /// has completed *and* `snapshot` differs from the last persisted bytes.
    /// Returns `Ok(true)` when it wrote, `Ok(false)` when skipped — the pre-load
    /// gate (safety property 1) or an unchanged snapshot (property 2). The
    /// per-app debounce calls this with `manager.drafts_snapshot_bytes()`.
    pub async fn save_if_changed(
        &self,
        snapshot: &[u8],
    ) -> Result<bool, DraftsClientError<R::Error>> {
        // Decide under the lock, then release it before the await — never hold a
        // std Mutex across `.await`. Saves are serialized by the caller's
        // debounce, so the brief unlocked window can't race a second save.
        {
            let st = self.state.lock().unwrap();
            if !st.loaded {
                return Ok(false);
            }
            if st.last_saved.as_deref() == Some(snapshot) {
                return Ok(false);
            }
        }
        self.client.save(&self.rail, snapshot).await?;
        self.state.lock().unwrap().last_saved = Some(snapshot.to_vec());
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MemoryDraftsNest as FakeNest, SharedDraftsNest as SharedNest};
    use fauna_client_testkit::block_on;
    use fauna_protocol::drafts::KIND_GET;
    use std::sync::Arc;

    fn keypair() -> ActorKeypair {
        ActorKeypair::from_secret([7u8; 32])
    }

    fn sync(nest: Arc<FakeNest>) -> DraftsSync<SharedNest> {
        DraftsSync::new(SharedNest(nest), &keypair(), "conversations")
    }

    /// Safety property 1: a save before the launch load is a no-op and writes
    /// nothing — the gate that keeps a stray startup tick from clobbering the
    /// user's nest drafts with an empty snapshot.
    #[test]
    fn save_before_load_is_gated() {
        let nest = Arc::new(FakeNest::default());
        let s = sync(nest.clone());
        assert!(!block_on(s.save_if_changed(b"draft")).unwrap());
        assert_eq!(*nest.puts.lock().unwrap(), 0, "must not PUT before load");
        assert!(nest.stored.lock().unwrap().is_empty());
    }

    /// First run: load returns None, the gate lifts, and the first genuine draft
    /// is persisted.
    #[test]
    fn first_run_then_first_draft_saves() {
        let nest = Arc::new(FakeNest::default());
        let s = sync(nest.clone());
        assert_eq!(block_on(s.load()).unwrap(), None);
        assert!(block_on(s.save_if_changed(b"hello")).unwrap());
        assert_eq!(*nest.puts.lock().unwrap(), 1);
    }

    /// Safety property 2: an unchanged snapshot is not re-uploaded.
    #[test]
    fn unchanged_snapshot_skips_save() {
        let nest = Arc::new(FakeNest::default());
        let s = sync(nest.clone());
        block_on(s.load()).unwrap();
        assert!(block_on(s.save_if_changed(b"a")).unwrap());
        assert!(!block_on(s.save_if_changed(b"a")).unwrap());
        assert!(block_on(s.save_if_changed(b"ab")).unwrap());
        assert_eq!(*nest.puts.lock().unwrap(), 2, "only the two distinct sets");
    }

    /// Loading an existing blob sets the baseline, so the redundant
    /// post-restore tick (same bytes) does not re-upload.
    #[test]
    fn load_sets_baseline_so_post_restore_tick_is_noop() {
        let nest = Arc::new(FakeNest::default());
        // Device A persists a draft.
        let a = sync(nest.clone());
        block_on(a.load()).unwrap();
        block_on(a.save_if_changed(b"from A")).unwrap();
        let puts_after_a = *nest.puts.lock().unwrap();

        // Device B (same identity) loads it, then its composer re-renders and
        // the autosave ticks with the just-restored bytes — must be a no-op.
        let b = sync(nest.clone());
        assert_eq!(
            block_on(b.load()).unwrap().as_deref(),
            Some(b"from A".as_slice())
        );
        assert!(!block_on(b.save_if_changed(b"from A")).unwrap());
        assert_eq!(*nest.puts.lock().unwrap(), puts_after_a, "no churn from B");
    }

    /// ⚠ The successor's FIRST session, and the whole reason the read fallback
    /// exists. The re-seal pass is a post-auth hook while this load runs at
    /// launch, and nothing orders the two — so the read usually wins. Without
    /// the offered predecessor key it hard-errors, the gate never lifts, and
    /// the user's composers stay empty for the session.
    #[test]
    fn a_successor_opens_a_rail_its_predecessor_sealed() {
        let nest = Arc::new(FakeNest::default());

        // The predecessor persists a draft under its own identity.
        let predecessor_kp = fauna_core::identity::ActorKeypair::from_secret([21u8; 32]);
        let predecessor =
            DraftsSync::new(SharedNest(nest.clone()), &predecessor_kp, "conversations");
        block_on(predecessor.load()).unwrap();
        block_on(predecessor.save_if_changed(b"the reply the theft interrupted")).unwrap();

        // The successor launches. Its own key does not open that blob.
        let bare = sync(nest.clone());
        assert!(
            block_on(bare.load()).is_err(),
            "without the fallback a successor cannot read its own inherited rail"
        );

        let with_key = DraftsSync::new(SharedNest(nest.clone()), &keypair(), "conversations")
            .with_predecessors(vec![crate::backup_key_from_seed(
                predecessor_kp.secret_bytes(),
            )]);
        assert_eq!(
            block_on(with_key.load()).unwrap().as_deref(),
            Some(b"the reply the theft interrupted".as_slice()),
            "the offered retired key must open the inherited rail"
        );
    }

    /// ⚠ The fallback must not become a seal root. Having *read* through a
    /// predecessor key, the very next autosave must still write under the
    /// successor's own — otherwise the read fallback would quietly re-seal the
    /// rail to a retired identity and the re-seal pass could never finish.
    #[test]
    fn reading_through_a_predecessor_key_never_seals_under_it() {
        let nest = Arc::new(FakeNest::default());
        let predecessor_kp = fauna_core::identity::ActorKeypair::from_secret([21u8; 32]);
        let predecessor =
            DraftsSync::new(SharedNest(nest.clone()), &predecessor_kp, "conversations");
        block_on(predecessor.load()).unwrap();
        block_on(predecessor.save_if_changed(b"inherited")).unwrap();

        let successor = DraftsSync::new(SharedNest(nest.clone()), &keypair(), "conversations")
            .with_predecessors(vec![crate::backup_key_from_seed(
                predecessor_kp.secret_bytes(),
            )]);
        block_on(successor.load()).unwrap();
        block_on(successor.save_if_changed(b"edited by the successor")).unwrap();

        // The rail now opens under the successor's key and NOT the predecessor's.
        let blob = nest
            .stored
            .lock()
            .unwrap()
            .get("conversations")
            .cloned()
            .expect("the rail must still exist");
        let successor_key = crate::backup_key_from_seed(keypair().secret_bytes());
        assert_eq!(
            crate::unseal_drafts(&blob, &successor_key).unwrap(),
            b"edited by the successor",
        );
        assert!(
            crate::unseal_drafts(
                &blob,
                &crate::backup_key_from_seed(predecessor_kp.secret_bytes())
            )
            .is_err(),
            "a save after a fallback read must retire the predecessor's seal"
        );
    }

    /// An identity that never succeeded offers no keys and is unaffected — the
    /// overwhelmingly common path, pinned so the fallback cannot change it.
    #[test]
    fn an_unopenable_rail_still_hard_errors_with_no_predecessors() {
        let nest = Arc::new(FakeNest::default());
        let stranger = fauna_core::identity::ActorKeypair::from_secret([77u8; 32]);
        let other = DraftsSync::new(SharedNest(nest.clone()), &stranger, "conversations");
        block_on(other.load()).unwrap();
        block_on(other.save_if_changed(b"not yours")).unwrap();

        let s = sync(nest.clone());
        assert!(
            block_on(s.load()).is_err(),
            "an unreadable rail must stay a hard error, never masked as 'no drafts' — \
             masking it would lift the save gate and clobber it"
        );
        assert!(
            !block_on(s.save_if_changed(b"empty view")).unwrap(),
            "and the gate must stay shut, so nothing overwrites the unread rail"
        );
    }

    /// A nest that fails the first `fail_gets` drafts reads with a transport
    /// error — the launch read outliving its deadline — then answers from the
    /// shared [`FakeNest`].
    struct FlakyNest {
        inner: SharedNest,
        fail_gets: std::sync::atomic::AtomicU32,
        gets: std::sync::atomic::AtomicU32,
    }

    impl FlakyNest {
        fn new(nest: Arc<FakeNest>, fail_gets: u32) -> Self {
            Self {
                inner: SharedNest(nest),
                fail_gets: std::sync::atomic::AtomicU32::new(fail_gets),
                gets: std::sync::atomic::AtomicU32::new(0),
            }
        }
    }

    impl RpcRequester for &FlakyNest {
        type Error = String;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use std::sync::atomic::Ordering;
            if kind == KIND_GET {
                self.gets.fetch_add(1, Ordering::SeqCst);
                let failing = self
                    .fail_gets
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok();
                if failing {
                    return Err("the nest took too long to respond".to_string());
                }
            }
            let Ok(reply) = self.inner.request(kind, payload).await;
            Ok(reply)
        }
    }

    /// A retrying load's outcome beside the waits it asked for.
    type RecordedLoad<E> = (Result<Option<Vec<u8>>, DraftsClientError<E>>, Vec<Duration>);

    /// Run `sync`'s retrying load over [`LAUNCH_LOAD_RETRY_DELAYS`] with a sleep
    /// that records each wait instead of taking it.
    fn load_recording_waits<R: RpcRequester>(
        sync: &DraftsSync<R>,
        delays: &[Duration],
    ) -> RecordedLoad<R::Error> {
        let waits = std::cell::RefCell::new(Vec::new());
        let result = block_on(sync.load_retrying(delays, |d| {
            waits.borrow_mut().push(d);
            core::future::ready(())
        }));
        (result, waits.into_inner())
    }

    /// **A launch load lost to the transport is retried until it lands** — the
    /// defect this pins: one slow answer at launch used to leave the save gate
    /// shut, so the session neither showed the drafts another device wrote nor
    /// saved any of its own until the app was restarted.
    #[test]
    fn a_launch_load_lost_to_the_transport_is_retried_until_it_lands() {
        let nest = Arc::new(FakeNest::default());
        // Another device's drafts rest on the nest.
        let other = sync(nest.clone());
        block_on(other.load()).unwrap();
        block_on(other.save_if_changed(b"from device B")).unwrap();

        let flaky = FlakyNest::new(nest.clone(), 2);
        let s = DraftsSync::new(&flaky, &keypair(), "conversations");
        let (result, waits) = load_recording_waits(&s, &LAUNCH_LOAD_RETRY_DELAYS);

        assert_eq!(
            result.unwrap().as_deref(),
            Some(b"from device B".as_slice()),
            "the third attempt must deliver what the first two lost"
        );
        assert_eq!(waits, LAUNCH_LOAD_RETRY_DELAYS[..2].to_vec());
        assert!(
            block_on(s.save_if_changed(b"typed after the late load")).unwrap(),
            "the late success must lift the save gate"
        );
    }

    /// The retry is bounded: after the schedule the transport error is returned
    /// and the gate stays shut, so nothing is saved over drafts this session
    /// never read.
    #[test]
    fn a_launch_load_gives_up_after_the_schedule_with_the_gate_still_shut() {
        let nest = Arc::new(FakeNest::default());
        let flaky = FlakyNest::new(nest.clone(), u32::MAX);
        let s = DraftsSync::new(&flaky, &keypair(), "conversations");
        let (result, waits) = load_recording_waits(&s, &LAUNCH_LOAD_RETRY_DELAYS);

        assert!(matches!(result, Err(DraftsClientError::Transport(_))));
        assert_eq!(waits, LAUNCH_LOAD_RETRY_DELAYS.to_vec());
        assert_eq!(
            flaky.gets.load(std::sync::atomic::Ordering::SeqCst) as usize,
            LAUNCH_LOAD_RETRY_DELAYS.len() + 1,
            "one first try plus one per scheduled wait"
        );
        assert!(!block_on(s.save_if_changed(b"would clobber")).unwrap());
        assert_eq!(*nest.puts.lock().unwrap(), 0);
    }

    /// A seal failure is not retried: the same bytes fail the same way, and
    /// waiting minutes to say so would only delay the error.
    #[test]
    fn a_seal_failure_is_returned_at_once() {
        let nest = Arc::new(FakeNest::default());
        let stranger = ActorKeypair::from_secret([77u8; 32]);
        let other = DraftsSync::new(SharedNest(nest.clone()), &stranger, "conversations");
        block_on(other.load()).unwrap();
        block_on(other.save_if_changed(b"not yours")).unwrap();

        let s = sync(nest);
        let (result, waits) = load_recording_waits(&s, &LAUNCH_LOAD_RETRY_DELAYS);
        assert!(matches!(result, Err(DraftsClientError::Seal(_))));
        assert!(waits.is_empty(), "a seal failure must not wait: {waits:?}");
    }

    /// **A leave door taken before the launch load landed reads once, then
    /// saves** — the defect this pins: web's account switch, taken seconds
    /// after the feed opened, found the gate shut and saved nothing.
    #[test]
    fn a_leave_load_opens_a_shut_gate_and_hands_back_what_rests() {
        let nest = Arc::new(FakeNest::default());
        let other = sync(nest.clone());
        block_on(other.load()).unwrap();
        block_on(other.save_if_changed(b"from device B")).unwrap();

        let s = sync(nest.clone());
        assert_eq!(
            block_on(s.load_for_leave()).unwrap().as_deref(),
            Some(b"from device B".as_slice()),
            "the caller fills its rail from what rests before it snapshots"
        );
        assert!(block_on(s.save_if_changed(b"typed before the launch load")).unwrap());
        assert!(nest.stored.lock().unwrap().contains_key("conversations"));
    }

    /// The leave read is one attempt: a door is bounded, so a transport
    /// failure is returned at once and the gate stays shut — nothing is saved
    /// over drafts this session never read.
    #[test]
    fn a_leave_load_lost_to_the_transport_is_one_attempt_and_keeps_the_gate_shut() {
        let nest = Arc::new(FakeNest::default());
        let flaky = FlakyNest::new(nest.clone(), u32::MAX);
        let s = DraftsSync::new(&flaky, &keypair(), "conversations");
        assert!(matches!(
            block_on(s.load_for_leave()),
            Err(DraftsClientError::Transport(_))
        ));
        assert_eq!(flaky.gets.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!block_on(s.save_if_changed(b"would clobber")).unwrap());
        assert_eq!(*nest.puts.lock().unwrap(), 0);
    }

    /// An open gate needs no read: the leave door goes straight to the save.
    #[test]
    fn a_leave_load_on_an_open_gate_reads_nothing() {
        let nest = Arc::new(FakeNest::default());
        let flaky = FlakyNest::new(nest.clone(), 0);
        let s = DraftsSync::new(&flaky, &keypair(), "conversations");
        block_on(s.load()).unwrap();
        assert_eq!(block_on(s.load_for_leave()).unwrap(), None);
        assert_eq!(flaky.gets.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Seed `nest` with `bytes` resting on the conversations rail, as another
    /// device of the same identity would leave them.
    fn seed_resting(nest: &Arc<FakeNest>, bytes: &[u8]) {
        let other = sync(nest.clone());
        block_on(other.load()).unwrap();
        assert!(block_on(other.save_if_changed(bytes)).unwrap());
    }

    /// A shut gate, a draft resting on the nest and an untouched composer: the
    /// fill takes the resting draft, so nothing is written over it.
    #[test]
    fn a_leave_save_over_a_shut_gate_never_overwrites_a_resting_draft_with_an_untouched_one() {
        let nest = Arc::new(FakeNest::default());
        seed_resting(&nest, b"resting");
        let puts_before = *nest.puts.lock().unwrap();

        let s = sync(nest.clone());
        let wrote = block_on(s.save_on_leave(b"".to_vec(), |resting| resting)).unwrap();
        assert!(
            !wrote,
            "an untouched composer that took the resting draft saves nothing"
        );
        assert_eq!(*nest.puts.lock().unwrap(), puts_before);
    }

    /// A shut gate and a typed edit: one read, then the fill keeps the user's
    /// text and it is saved.
    #[test]
    fn a_leave_save_over_a_shut_gate_saves_a_typed_edit_after_one_read() {
        let nest = Arc::new(FakeNest::default());
        seed_resting(&nest, b"resting");
        let flaky = FlakyNest::new(nest.clone(), 0);
        let s = DraftsSync::new(&flaky, &keypair(), "conversations");

        let mut filled_from = None;
        let wrote = block_on(s.save_on_leave(b"typed".to_vec(), |resting| {
            filled_from = Some(resting);
            b"typed".to_vec()
        }))
        .unwrap();
        assert!(wrote);
        assert_eq!(filled_from.as_deref(), Some(b"resting".as_slice()));
        assert_eq!(flaky.gets.load(std::sync::atomic::Ordering::SeqCst), 1);
        let reader = sync(nest.clone());
        assert_eq!(
            block_on(reader.load()).unwrap().as_deref(),
            Some(b"typed".as_slice())
        );
    }

    /// An open gate saves the captured snapshot and never calls the fill.
    #[test]
    fn a_leave_save_over_an_open_gate_saves_the_capture_without_a_fill() {
        let nest = Arc::new(FakeNest::default());
        seed_resting(&nest, b"resting");
        let s = sync(nest.clone());
        block_on(s.load()).unwrap();
        let wrote = block_on(s.save_on_leave(b"typed".to_vec(), |_| {
            panic!("an open gate has nothing to fill from")
        }))
        .unwrap();
        assert!(wrote);
    }

    /// A read lost to the transport saves nothing: the gate stays shut.
    #[test]
    fn a_leave_save_whose_read_fails_saves_nothing() {
        let nest = Arc::new(FakeNest::default());
        let flaky = FlakyNest::new(nest.clone(), u32::MAX);
        let s = DraftsSync::new(&flaky, &keypair(), "conversations");
        assert!(block_on(s.save_on_leave(b"typed".to_vec(), |r| r)).is_err());
        assert_eq!(*nest.puts.lock().unwrap(), 0);
    }
}
