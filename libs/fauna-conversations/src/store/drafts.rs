use crate::compose::{ComposeState, RecipientPickerState, ResolveState, SendState};
use crate::index_sink::{IndexableDraft, MessageIndexObserver, NEW_THREAD_DRAFT_ID};
use crate::keying::ThreadKey;
use crate::thread::ThreadId;
use fauna_core::encoding::{canonical_decode, canonical_encode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Default)]
pub struct DraftStore {
    per_thread: RwLock<HashMap<ThreadId, ComposeState>>,
    new_thread: RwLock<Option<ComposeState>>,
    /// Restored drafts whose thread is not on this device yet, under the
    /// thread's stable key. They live off `per_thread` because that map is
    /// keyed by the device-local [`ThreadId`], which such a thread does not
    /// have yet. Each is adopted into `per_thread` by [`Self::adopt_parked`]
    /// once its thread materializes, and until then every
    /// [`Self::snapshot_bytes`] re-emits it — otherwise this device's first
    /// autosave would erase another device's draft from the plane.
    parked: RwLock<HashMap<ThreadKey, ComposeState>>,
    /// The content-index sink, when this seat builds an index.
    ///
    /// **Registered on the store rather than driven from the manager's compose
    /// mutators, because the mutators are not a chokepoint** — better than
    /// twenty of them call `set`/`clear`, and a kind whose corpus must be exact
    /// cannot afford the one that gets added later and forgets. Every path that
    /// can change what the user would search is inside this type, so hooking it
    /// here makes "the index matches the composer" structural instead of a
    /// convention.
    index_observer: RwLock<Option<Arc<dyn MessageIndexObserver>>>,
}

/// The serialised at-rest form of a [`DraftStore`] — the whole conversations-rail
/// draft set as one record. This is the byte shape sealed under the owner's
/// `BackupKey` and stored in the `__drafts` reserved folder
/// (`docs/goal/behavior/file-sync.md` § Drafts Sync). Layout is editorial per the
/// design (`2026-05-13-drafts-at-rest-design.md` §146); this picks the simplest
/// layout: one blob holding the entire in-memory state.
#[derive(Serialize, Deserialize, Default)]
struct DraftsSnapshot {
    /// Per-thread compose states, **sorted by the canonical bytes of their
    /// key**. `ThreadKey` is not `Ord` and `HashMap` iteration order is
    /// nondeterministic, so we sort the vec before encoding — that is what
    /// makes [`DraftStore::snapshot_bytes`] byte-stable for equal logical
    /// state, and "byte-equal drafts" a meaningful cross-device assertion.
    threads: Vec<DraftEntry>,
    /// The single-slot new-thread compose (`new_thread_compose`).
    new_thread: Option<ComposeState>,
}

/// One per-thread draft at rest, under the thread's **stable key, never its
/// [`ThreadId`]** (`conversation-drafts.md` § Persistence). A `ThreadId` is
/// the device-local counter `t-N`, numbered in whatever order threads
/// materialize on that device, so a draft rested under it lands in whichever
/// conversation *another* device happened to number the same — the user's
/// words to one person in another person's composer. The key is what two
/// devices agree on ([`ThreadKey::names_the_same_thread_everywhere`]).
#[derive(Serialize, Deserialize)]
struct DraftEntry {
    thread: ThreadKey,
    compose: ComposeState,
}

/// A persisted-draft blob could not be restored. The caller (client glue) treats
/// this as "no drafts" and starts fresh rather than failing the surface — a
/// corrupt or newer-shape blob must never crash the composer.
#[derive(Debug, thiserror::Error)]
pub enum DraftRestoreError {
    #[error("decode drafts snapshot: {0}")]
    Decode(String),
}

impl DraftStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: &ThreadId) -> ComposeState {
        self.per_thread
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set(&self, id: ThreadId, state: ComposeState) {
        self.per_thread.write().unwrap().insert(id, state);
        self.notify_index();
    }

    pub fn clear(&self, id: &ThreadId) {
        self.per_thread.write().unwrap().remove(id);
        self.notify_index();
    }

    pub fn new_thread(&self) -> Option<ComposeState> {
        self.new_thread.read().unwrap().clone()
    }

    pub fn set_new_thread(&self, state: Option<ComposeState>) {
        *self.new_thread.write().unwrap() = state;
        self.notify_index();
    }

    pub fn clear_all(&self) {
        self.per_thread.write().unwrap().clear();
        self.parked.write().unwrap().clear();
        *self.new_thread.write().unwrap() = None;
        self.notify_index();
    }

    /// The `blob_hash` of every attachment any compose draft references — the
    /// per-thread drafts and the new-thread compose alike. The manager pins
    /// these in its attachment store: `send` re-resolves a draft's bytes from
    /// there, so they must outlive the store's budget while staged
    /// (`store::attachments`). The store reads this at eviction time, under its
    /// lock, so a draft must be set here before its bytes are inserted
    /// (`ConversationsManager::stage_attachment`).
    pub fn staged_attachment_hashes(&self) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        for compose in self.per_thread.read().unwrap().values() {
            out.extend(compose.attachments.iter().map(|d| d.blob_hash.clone()));
        }
        if let Some(compose) = self.new_thread.read().unwrap().as_ref() {
            out.extend(compose.attachments.iter().map(|d| d.blob_hash.clone()));
        }
        // A parked draft is still the user's draft: it is adopted, attachments
        // and all, the moment its thread lands.
        for compose in self.parked.read().unwrap().values() {
            out.extend(compose.attachments.iter().map(|d| d.blob_hash.clone()));
        }
        out
    }

    /// Register the content-index sink. Called by
    /// [`ConversationsManager::set_index_observer`] so registration stays a
    /// single act from app glue's point of view.
    ///
    /// [`ConversationsManager::set_index_observer`]: crate::manager::ConversationsManager::set_index_observer
    pub fn set_index_observer(&self, observer: Arc<dyn MessageIndexObserver>) {
        *self.index_observer.write().unwrap() = Some(observer);
        // The corpus that already exists is this kind's catch-up backlog: a
        // restore normally lands *before* an observer is registered (the same
        // ordering that made the Conversation restore leg a no-op in
        // production — `content-index.md` § Ingest triggers, v1). Offering it
        // at registration is what makes the drafts arm's catch-up leg fire at
        // all, rather than waiting for the user's next keystroke.
        self.notify_index();
    }

    /// Every draft with searchable text, as the index sees them.
    ///
    /// **Empty composes are excluded.** `get` hands back a default
    /// `ComposeState` for an unknown thread and the compose mutators write it
    /// straight back, so the store legitimately holds blank entries for threads
    /// the user merely opened. Indexing those would put rows in the corpus that
    /// can never match a query and can never be navigated to usefully.
    pub fn indexable_corpus(&self) -> Vec<IndexableDraft> {
        let mut out: Vec<IndexableDraft> = self
            .per_thread
            .read()
            .unwrap()
            .iter()
            .filter(|(_, compose)| has_searchable_text(compose))
            .map(|(id, compose)| IndexableDraft {
                content_id: id.0.clone(),
                thread_id: Some(id.clone()),
                subject: compose.subject_draft.clone().filter(|s| !s.is_empty()),
                body: compose.body_draft.clone(),
            })
            .collect();
        // Deterministic order, for the same reason `snapshot_bytes` sorts: the
        // corpus is sealed into a segment, and a stable order keeps equal
        // logical state from producing different bytes on different devices.
        out.sort_by(|a, b| a.content_id.cmp(&b.content_id));

        if let Some(compose) = self
            .new_thread
            .read()
            .unwrap()
            .as_ref()
            .filter(|c| has_searchable_text(c))
        {
            out.push(IndexableDraft {
                content_id: NEW_THREAD_DRAFT_ID.to_string(),
                thread_id: None,
                subject: compose.subject_draft.clone().filter(|s| !s.is_empty()),
                body: compose.body_draft.clone(),
            });
        }
        out
    }

    /// Hand the whole corpus to the index sink, if one is registered.
    ///
    /// Called after **every** mutation, and always with the write lock already
    /// released — the seam's contract is that the sink does not block, but a
    /// sink that re-entered this store while we held its lock would deadlock,
    /// and that is not a hazard worth leaving to an implementor's discipline.
    fn notify_index(&self) {
        let observer = self.index_observer.read().unwrap().clone();
        if let Some(observer) = observer {
            observer.observe_draft_corpus(&self.indexable_corpus());
        }
    }

    /// Serialise the entire draft set to its canonical at-rest bytes (v2
    /// persistence). The client seals these under the owner's `BackupKey`
    /// before uploading to the `__drafts` reserved folder. Byte-stable for
    /// equal logical state (see [`DraftsSnapshot::threads`]).
    ///
    /// `key_of` maps a local thread to its routing key in the thread store. A
    /// thread with no portable key — gone from the store, or a FaunaMls thread
    /// whose group has not bound its channel yet — stays off the blob, because
    /// no other device could find it. Every parked entry is re-emitted; should
    /// a live entry carry the same key, the live one wins.
    pub fn snapshot_bytes(&self, key_of: impl Fn(&ThreadId) -> Option<ThreadKey>) -> Vec<u8> {
        // Copied out first, so `key_of` (which reads the thread store) runs
        // under no lock of ours.
        let live: Vec<(ThreadId, ComposeState)> = self
            .per_thread
            .read()
            .unwrap()
            .iter()
            .map(|(id, compose)| (id.clone(), persistable(compose)))
            .collect();
        let mut by_key: HashMap<ThreadKey, ComposeState> = self
            .parked
            .read()
            .unwrap()
            .iter()
            .map(|(key, compose)| (key.clone(), persistable(compose)))
            .collect();
        for (id, compose) in live {
            if let Some(key) = key_of(&id).filter(ThreadKey::names_the_same_thread_everywhere) {
                by_key.insert(key, compose);
            }
        }
        let mut threads: Vec<(Vec<u8>, DraftEntry)> = by_key
            .into_iter()
            .map(|(thread, compose)| {
                let order = canonical_encode(&thread).expect("canonical_encode ThreadKey");
                (order, DraftEntry { thread, compose })
            })
            .collect();
        threads.sort_by(|a, b| a.0.cmp(&b.0));
        let threads = threads.into_iter().map(|(_, entry)| entry).collect();
        let snapshot = DraftsSnapshot {
            threads,
            new_thread: self.new_thread.read().unwrap().as_ref().map(persistable),
        };
        // Encoding our own owned types is infallible in practice; surface a clear
        // panic message rather than threading a Result through every caller.
        canonical_encode(&snapshot).expect("canonical_encode DraftsSnapshot")
    }

    /// Restore the draft set from bytes produced by [`Self::snapshot_bytes`]
    /// (after the client unseals them with the owner's `BackupKey`) — the
    /// load-on-launch / cross-device catch-up path.
    ///
    /// **A restore FILLS. It never clears a slot and never overwrites live
    /// user input** ([`has_user_input`] is the predicate). It used to replace
    /// the whole store wholesale, and that made it a third way to lose a
    /// draft, which `conversations.md` § Persistence does not allow: "**only**
    /// an explicit cancel/discard (`cancel_new_conversation`) or calling send
    /// (`send_new_thread`) clears the new-thread draft".
    ///
    /// Two properties follow, and the tests pin both:
    ///
    /// * **A blob whose slot is absent is not an instruction to close a live
    ///   composer.** The restore lands whenever the `__drafts` fetch completes
    ///   — the shells fire it at login and await it off the UI thread — so it
    ///   routinely arrives *after* the user opened `+` and started typing. A
    ///   wholesale replace then cleared the slot underneath them, and the
    ///   damage was silent in a specific way: the composer's *visibility* is a
    ///   separate flag (`new_thread_active`) the restore never touched, while
    ///   every mutator behind it (`set_new_thread_recipient_input`,
    ///   `accept_new_thread_chip`) is an `if let Some(..)` no-op once the slot
    ///   is gone. The user goes on typing into a picker that can no longer
    ///   record a keystroke or start a resolve.
    /// * **On a collision the live compose wins.** The blob is at best as old
    ///   as the last autosave, so a draft being typed is strictly the newer of
    ///   the two.
    ///
    /// Nothing is given up: a slot the user has not authored into is still
    /// adopted, which is what carries a draft written on another device onto
    /// this one. The posts rail reaches the same place from the other side —
    /// it declines to apply an all-empty record at all
    /// ([`fauna_feed::drafts::PostDrafts::is_empty`], consulted by
    /// `FeedManager::restore_drafts`).
    ///
    /// `id_of` maps a stable key to the thread this device holds under it.
    /// An entry whose thread is not here yet is **parked, not dropped**, and
    /// [`Self::adopt_parked`] fills it, under the same FILL rule, once the
    /// thread lands.
    pub fn restore_from_bytes(
        &self,
        bytes: &[u8],
        id_of: impl Fn(&ThreadKey) -> Option<ThreadId>,
    ) -> Result<(), DraftRestoreError> {
        let snapshot: DraftsSnapshot =
            canonical_decode(bytes).map_err(|e| DraftRestoreError::Decode(e.to_string()))?;
        // Resolved before any lock of ours is taken: `id_of` reads the thread
        // store.
        let resolved: Vec<(Option<ThreadId>, DraftEntry)> = snapshot
            .threads
            .into_iter()
            .map(|entry| (id_of(&entry.thread), entry))
            .collect();
        {
            let mut per_thread = self.per_thread.write().unwrap();
            let mut parked = self.parked.write().unwrap();
            for (id, entry) in resolved {
                match id {
                    // Taken as it rests: every writer projects through
                    // [`persistable`], so a blob carries no transient state
                    // to strip.
                    Some(id) => {
                        fill(&mut per_thread, id, entry.compose);
                    }
                    // A blank entry carries nothing worth waiting for.
                    None if has_user_input(&entry.compose) => {
                        parked.insert(entry.thread, entry.compose);
                    }
                    None => {}
                }
            }
        }
        {
            let mut new_thread = self.new_thread.write().unwrap();
            if !new_thread.as_ref().is_some_and(has_user_input) {
                // `None` from the blob leaves the live slot alone rather than
                // clearing it — the first property above.
                if let Some(restored) = snapshot.new_thread {
                    *new_thread = Some(restored);
                }
            }
        }
        // The restored corpus is exactly what a returning device must be able to
        // search, and it arrives through a path no compose mutator touches.
        self.notify_index();
        Ok(())
    }

    /// Whether any restored draft is waiting for its thread — the cheap check
    /// the manager makes before paying for [`Self::adopt_parked`].
    pub fn has_parked(&self) -> bool {
        !self.parked.read().unwrap().is_empty()
    }

    /// Fill every parked draft whose thread this device now holds, by
    /// [`Self::restore_from_bytes`]'s FILL rule: a live compose with user
    /// input wins, and the parked draft is then spent. Returns whether any
    /// thread's compose changed.
    pub fn adopt_parked(&self, id_of: impl Fn(&ThreadKey) -> Option<ThreadId>) -> bool {
        let keys: Vec<ThreadKey> = self.parked.read().unwrap().keys().cloned().collect();
        let arrived: Vec<(ThreadKey, ThreadId)> = keys
            .into_iter()
            .filter_map(|key| id_of(&key).map(|id| (key, id)))
            .collect();
        if arrived.is_empty() {
            return false;
        }
        let mut changed = false;
        {
            let mut per_thread = self.per_thread.write().unwrap();
            let mut parked = self.parked.write().unwrap();
            for (key, id) in arrived {
                // Taken under the lock: a concurrent restore or adoption may
                // have moved it since the keys were copied.
                if let Some(compose) = parked.remove(&key) {
                    changed |= fill(&mut per_thread, id, compose);
                }
            }
        }
        if changed {
            self.notify_index();
        }
        changed
    }
}

/// The per-thread half of the FILL rule: a live compose for this thread is
/// what the user is typing right now, so only an untouched one yields to the
/// resting draft. Returns whether the slot took it. Nothing on this path
/// clears: a thread the blob omits keeps its local draft.
fn fill(
    per_thread: &mut HashMap<ThreadId, ComposeState>,
    id: ThreadId,
    compose: ComposeState,
) -> bool {
    if per_thread.get(&id).is_some_and(has_user_input) {
        return false;
    }
    per_thread.insert(id, compose);
    true
}

/// The at-rest projection of a live compose: **only user-authored input rests.**
///
/// `docs/goal/ui/feed.md` § Persistence ratifies that rule for both draft rails
/// and names this rail's wholesale embedding of `ComposeState` the defect to
/// fix. Restoring a transient field is not cosmetic here: every shell builds
/// `dm-send-button` as `enabled = !sending`, and `send_state` is stamped only by
/// [`ConversationsManager::send`], so a restored `Sending` disables the button
/// with no gesture that clears it. `send` also stamps `Failed` *without*
/// clearing the draft, so a failed send rests indefinitely.
///
/// **Why this is a constructor and not a separate at-rest record.** The posts
/// rail enumerates its own [`fauna_feed::drafts::PostDrafts`]; this rail cannot
/// copy that, because its blob shipped. `ComposeState` carries no
/// `#[serde(default)]`, so a blob missing a key fails to decode on an app that
/// has not been updated — and the shared launch gate stays closed after a failed
/// restore, showing that app no drafts at all. At-rest evolution is
/// additive-everywhere (`version-compatibility.md`), so the bytes keep every
/// field and the transient ones are simply written at their idle default. The
/// **explicit struct literal is the gate**: no `..Default::default()`, so a
/// field added to `ComposeState` later fails to compile here until its author
/// classifies it — the same "cannot silently become at-rest data" property the
/// posts rail gets from enumerating a record.
///
/// Applied on **write**, so no blob carries transient state. (The read-side
/// heal of blobs written before this projection existed was retired by the
/// compat-remnant sweep — `version-compatibility.md` § Dimension 2, program 4.)
///
/// [`ConversationsManager::send`]: crate::manager::ConversationsManager::send
fn persistable(compose: &ComposeState) -> ComposeState {
    ComposeState {
        // ---- user-authored input: rests ----
        body_draft: compose.body_draft.clone(),
        subject_draft: compose.subject_draft.clone(),
        attachments: compose.attachments.clone(),
        reply_to: compose.reply_to.clone(),
        reply_recipients: compose.reply_recipients.clone(),
        recipient_picker: compose.recipient_picker.as_ref().map(persistable_picker),
        // ---- transient: never rests ----
        send_state: SendState::Idle,
        // Derived from the nest's figures; re-derived after a restore.
        list_send: None,
    }
}

/// The new-thread recipient picker's at-rest projection, under the same rule.
///
/// What the user typed (`raw_input`) and committed (`chips`) is draft content
/// and rests. The rest is one run's probe output: `suggestions` is a dropdown,
/// and `resolve_state`/`resolved` describe an async probe that is not running
/// after a restart — restoring `Resolving` would paint a spinner nothing ever
/// resolves.
fn persistable_picker(picker: &RecipientPickerState) -> RecipientPickerState {
    RecipientPickerState {
        // ---- user-authored input: rests ----
        raw_input: picker.raw_input.clone(),
        chips: picker.chips.clone(),
        include_home_nest: picker.include_home_nest,
        // ---- transient probe output: never rests ----
        suggestions: Vec::new(),
        suggestion_displays: Vec::new(),
        resolve_state: ResolveState::Idle,
        resolved: None,
    }
}

/// Whether a compose holds anything a query could match.
///
/// Attachments deliberately do not count on their own: a draft with a file and
/// no text has nothing tokenizable, and media is indexed by classifiers on its
/// own kind (`content-index.md` § What's indexed), not as draft text.
fn has_searchable_text(compose: &ComposeState) -> bool {
    !compose.body_draft.trim().is_empty()
        || compose
            .subject_draft
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
}

/// Whether a compose holds anything the **user** authored — the predicate
/// [`DraftStore::restore_from_bytes`] consults before it would overwrite a
/// live draft with a resting one.
///
/// Deliberately wider than [`has_searchable_text`], which answers a different
/// question (is there anything for a query to match). A staged attachment, a
/// reply target, an edited To-line and a typed-but-uncommitted recipient are
/// each user input a restore must not destroy, and none of them is text.
///
/// Transient fields are excluded by construction: `send_state` is stamped only
/// by `ConversationsManager::send`, and the picker's `suggestions` /
/// `resolve_state` / `resolved` are one run's probe output, not something the
/// user wrote (see [`persistable`] / [`persistable_picker`], which is where
/// that same split is ratified for the at-rest side). An expanded-but-blank
/// "+ topic" (`subject_draft: Some("")`) is an affordance rather than
/// authorship, so it does not hold a synced draft off either.
fn has_user_input(compose: &ComposeState) -> bool {
    has_searchable_text(compose)
        || !compose.attachments.is_empty()
        || compose.reply_to.is_some()
        || !compose.reply_recipients.is_empty()
        || compose
            .recipient_picker
            .as_ref()
            .is_some_and(|p| !p.raw_input.trim().is_empty() || !p.chips.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{AttachmentDraft, SendState};
    use fauna_core::crypto::{BackupKey, decrypt_backup_chunk, encrypt_backup_chunk};

    fn sample_compose(body: &str) -> ComposeState {
        ComposeState {
            body_draft: body.to_string(),
            subject_draft: Some("topic".to_string()),
            attachments: vec![AttachmentDraft {
                blob_hash: "abc123".to_string(),
                filename: "a.png".to_string(),
                mime_type: "image/png".to_string(),
                size_bytes: 42,
                is_image: true,
            }],
            ..Default::default()
        }
    }

    /// A channel key named after the conversation — what every device of the
    /// user materializes a FaunaMls thread under.
    fn key(channel: &str) -> ThreadKey {
        ThreadKey::Channel {
            rail: crate::address::Rail::FaunaMls,
            channel_id_hex: channel.into(),
        }
    }

    /// The thread store of a device whose local ids happen to equal the
    /// channel names — the world the tests below other than the cross-device
    /// ones live in, where the key mapping is not the property under test.
    fn same_ids(id: &ThreadId) -> Option<ThreadKey> {
        Some(key(&id.0))
    }

    fn same_keys(k: &ThreadKey) -> Option<ThreadId> {
        match k {
            ThreadKey::Channel { channel_id_hex, .. } => Some(ThreadId(channel_id_hex.clone())),
            _ => None,
        }
    }

    /// A device's thread store as a key ↔ local-id table.
    struct Device(Vec<(ThreadKey, ThreadId)>);

    impl Device {
        fn new(pairs: &[(&str, &str)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|(channel, id)| (key(channel), ThreadId((*id).into())))
                    .collect(),
            )
        }
        fn key_of(&self, id: &ThreadId) -> Option<ThreadKey> {
            self.0.iter().find(|(_, i)| i == id).map(|(k, _)| k.clone())
        }
        fn id_of(&self, k: &ThreadKey) -> Option<ThreadId> {
            self.0
                .iter()
                .find(|(kk, _)| kk == k)
                .map(|(_, i)| i.clone())
        }
    }

    /// The defect this keying exists to close: device B numbered its 1:1 with
    /// bob `t-4`, device A numbered it `t-7` and has a *different*
    /// conversation, with carol, at `t-4`. Keyed by local id, B's words to
    /// bob landed in A's composer for carol — one tap from a wrong-recipient
    /// send. Keyed by the channel, they land in bob's thread and only there.
    #[test]
    fn a_restore_fills_the_same_conversation_under_a_different_local_id() {
        let b = Device::new(&[("bob", "t-4")]);
        let b_store = DraftStore::new();
        b_store.set(ThreadId("t-4".into()), sample_compose("hi bob"));
        let bytes = b_store.snapshot_bytes(|id| b.key_of(id));

        let a = Device::new(&[("carol", "t-4"), ("bob", "t-7")]);
        let a_store = DraftStore::new();
        a_store.restore_from_bytes(&bytes, |k| a.id_of(k)).unwrap();

        assert_eq!(
            a_store.get(&ThreadId("t-7".into())).body_draft,
            "hi bob",
            "the draft reaches bob's thread on this device"
        );
        assert_eq!(
            a_store.get(&ThreadId("t-4".into())).body_draft,
            "",
            "and never the conversation that happens to share B's local id"
        );
        assert!(!a_store.has_parked());
    }

    /// A draft whose thread this device has not materialized yet waits for it
    /// instead of being dropped, and is filled — by the same FILL rule — when
    /// it arrives.
    #[test]
    fn a_parked_draft_fills_when_its_thread_lands() {
        let b = Device::new(&[("bob", "t-1"), ("dave", "t-2")]);
        let b_store = DraftStore::new();
        b_store.set(ThreadId("t-1".into()), sample_compose("hi bob"));
        b_store.set(ThreadId("t-2".into()), sample_compose("hi dave"));
        let bytes = b_store.snapshot_bytes(|id| b.key_of(id));

        let a_store = DraftStore::new();
        // Nothing has arrived on A yet.
        a_store.restore_from_bytes(&bytes, |_| None).unwrap();
        assert!(a_store.has_parked());

        // Bob's thread lands (welcome-ingest), and A is already typing in
        // dave's, which it holds too by now.
        let a = Device::new(&[("bob", "t-3"), ("dave", "t-5")]);
        a_store.set(ThreadId("t-5".into()), sample_compose("typed here first"));
        assert!(a_store.adopt_parked(|k| a.id_of(k)));

        assert_eq!(a_store.get(&ThreadId("t-3".into())).body_draft, "hi bob");
        assert_eq!(
            a_store.get(&ThreadId("t-5".into())).body_draft,
            "typed here first",
            "a live compose still wins over a parked one"
        );
        assert!(!a_store.has_parked(), "both parked drafts are spent");
        assert!(
            !a_store.adopt_parked(|k| a.id_of(k)),
            "and a second pass has nothing to do"
        );
    }

    /// A snapshot taken while a draft is parked still carries it: otherwise
    /// this device's first autosave, before the thread arrives, would erase
    /// the other device's draft from the plane.
    #[test]
    fn a_snapshot_re_emits_a_parked_draft() {
        let b = Device::new(&[("bob", "t-1")]);
        let b_store = DraftStore::new();
        b_store.set(ThreadId("t-1".into()), sample_compose("hi bob"));
        let bytes = b_store.snapshot_bytes(|id| b.key_of(id));

        let a = Device::new(&[("carol", "t-1")]);
        let a_store = DraftStore::new();
        a_store.restore_from_bytes(&bytes, |k| a.id_of(k)).unwrap();
        a_store.set(ThreadId("t-1".into()), sample_compose("hi carol"));
        let a_bytes = a_store.snapshot_bytes(|id| a.key_of(id));

        // A third device holding both conversations sees both drafts.
        let c = Device::new(&[("bob", "t-9"), ("carol", "t-8")]);
        let c_store = DraftStore::new();
        c_store
            .restore_from_bytes(&a_bytes, |k| c.id_of(k))
            .unwrap();
        assert_eq!(c_store.get(&ThreadId("t-9".into())).body_draft, "hi bob");
        assert_eq!(c_store.get(&ThreadId("t-8".into())).body_draft, "hi carol");
    }

    /// A thread no other device can find — a FaunaMls thread whose group has
    /// not bound its channel yet carries only a local participant key — stays
    /// off the blob rather than resting under a key that matches nothing.
    #[test]
    fn a_thread_without_a_portable_key_stays_off_the_blob() {
        let store = DraftStore::new();
        store.set(ThreadId("unbound".into()), sample_compose("hi"));
        let bytes = store.snapshot_bytes(|_| {
            Some(ThreadKey::Participants {
                rail: crate::address::Rail::FaunaMls,
                participants: Vec::new(),
            })
        });
        let decoded: DraftsSnapshot = canonical_decode(&bytes).unwrap();
        assert!(decoded.threads.is_empty());
    }

    /// A blob a newer build wrote can key a draft under a thread key this
    /// build does not name. The restore still lands every draft it can place,
    /// parks the unknown one (no thread here matches it), and re-emits it
    /// byte-for-byte on the next autosave — the blob is rewritten by every
    /// device, so dropping the entry would delete another device's words
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*, the carrying answer).
    #[test]
    fn a_draft_under_a_thread_key_from_a_newer_build_is_carried_through() {
        #[derive(Serialize)]
        enum NewerKey {
            Channel {
                rail: crate::address::Rail,
                channel_id_hex: String,
            },
            Lobby {
                room: String,
            },
        }
        #[derive(Serialize)]
        struct NewerEntry {
            thread: NewerKey,
            compose: ComposeState,
        }
        #[derive(Serialize)]
        struct NewerSnapshot {
            threads: Vec<NewerEntry>,
            new_thread: Option<ComposeState>,
        }
        let mut threads = vec![
            NewerEntry {
                thread: NewerKey::Lobby {
                    room: "plaza".into(),
                },
                compose: sample_compose("hi plaza"),
            },
            NewerEntry {
                thread: NewerKey::Channel {
                    rail: crate::address::Rail::FaunaMls,
                    channel_id_hex: "bob".into(),
                },
                compose: sample_compose("hi bob"),
            },
        ];
        threads.sort_by_key(|e| canonical_encode(&e.thread).unwrap());
        let bytes = canonical_encode(&NewerSnapshot {
            threads,
            new_thread: None,
        })
        .unwrap();

        let store = DraftStore::new();
        store.restore_from_bytes(&bytes, same_keys).unwrap();
        assert_eq!(store.get(&ThreadId("bob".into())).body_draft, "hi bob");
        assert!(store.has_parked(), "the unknown entry waits, never dropped");
        assert_eq!(store.snapshot_bytes(same_ids), bytes);
    }

    #[test]
    fn snapshot_restore_round_trips_state() {
        let a = DraftStore::new();
        a.set(ThreadId("t-2".into()), sample_compose("second"));
        a.set(ThreadId("t-1".into()), sample_compose("first"));
        a.set_new_thread(Some(sample_compose("new")));

        let bytes = a.snapshot_bytes(same_ids);

        let b = DraftStore::new();
        b.restore_from_bytes(&bytes, same_keys).unwrap();

        assert_eq!(b.get(&ThreadId("t-1".into())).body_draft, "first");
        assert_eq!(b.get(&ThreadId("t-2".into())).body_draft, "second");
        assert_eq!(b.new_thread().unwrap().body_draft, "new");
        // Byte-stable: B re-serialises to the exact same bytes as A.
        assert_eq!(b.snapshot_bytes(same_ids), bytes);
    }

    /// The twin of `fauna_feed::drafts::tests::transient_ui_state_does_not_rest`
    /// — `feed.md` § Persistence ratifies "only user-authored input rests" for
    /// both rails and names this rail's embedding of the whole `ComposeState`
    /// the defect to fix, not the pattern to copy.
    ///
    /// A restored `Sending` is user-stuck rather than cosmetic: every shell
    /// builds `dm-send-button` as `enabled = !sending` (tui
    /// `conversations/mod.rs`, android `ConversationsComposeBar.kt`), and
    /// `send_state` is only ever stamped by `ConversationsManager::send`, so
    /// nothing a user can do afterwards clears it.
    #[test]
    fn transient_send_state_does_not_rest() {
        let store = DraftStore::new();
        store.set(
            ThreadId("mid-send".into()),
            ComposeState {
                send_state: SendState::Sending,
                ..sample_compose("half a thought")
            },
        );
        store.set_new_thread(Some(ComposeState {
            send_state: SendState::failed("nest refused it"),
            ..sample_compose("a new thread")
        }));

        let restored = DraftStore::new();
        restored
            .restore_from_bytes(&store.snapshot_bytes(same_ids), same_keys)
            .unwrap();

        let thread = restored.get(&ThreadId("mid-send".into()));
        assert!(
            matches!(thread.send_state, SendState::Idle),
            "a draft captured mid-send must not come back with the send button disabled, got {:?}",
            thread.send_state,
        );
        assert_eq!(
            thread.body_draft, "half a thought",
            "the writing still survives",
        );
        let new_thread = restored.new_thread().expect("new-thread slot");
        assert!(
            matches!(new_thread.send_state, SendState::Idle),
            "a stale failure from a previous run is not a draft, got {:?}",
            new_thread.send_state,
        );
        assert_eq!(new_thread.body_draft, "a new thread");
    }

    /// A restore takes a blob as it rests — the read-side heal of transient
    /// send state, which served only blobs written before [`persistable`]
    /// existed, was retired by the compat-remnant sweep
    /// (`version-compatibility.md` § Dimension 2, program 4). No current
    /// writer produces such a blob; one that carries `Sending` is not healed.
    #[test]
    fn a_blob_carrying_send_state_is_restored_verbatim_not_healed() {
        let blob = canonical_encode(&DraftsSnapshot {
            threads: vec![DraftEntry {
                thread: key("sending"),
                compose: ComposeState {
                    send_state: SendState::Sending,
                    ..sample_compose("mid-send")
                },
            }],
            new_thread: None,
        })
        .expect("encode");

        let restored = DraftStore::new();
        restored.restore_from_bytes(&blob, same_keys).unwrap();

        let compose = restored.get(&ThreadId("sending".into()));
        assert!(
            matches!(compose.send_state, SendState::Sending),
            "the pre-fix heal is retired — no read-side rewrite, got {:?}",
            compose.send_state,
        );
        assert_eq!(compose.body_draft, "mid-send");
    }

    /// `ComposeState` carries no `#[serde(default)]`, so a *missing*
    /// `send_state` key would fail to decode — and the shared launch gate
    /// stays closed after a failed restore, which would show **no drafts at
    /// all**. A successful decode into the field-complete shape is therefore
    /// the proof the key is still written.
    #[test]
    fn a_snapshot_blob_carries_every_compose_field() {
        let store = DraftStore::new();
        store.set(
            ThreadId("t".into()),
            ComposeState {
                send_state: SendState::Sending,
                ..sample_compose("body")
            },
        );

        let decoded: DraftsSnapshot = canonical_decode(&store.snapshot_bytes(same_ids))
            .expect("the decoder requires every ComposeState field to be present");

        assert_eq!(decoded.threads.len(), 1);
        assert!(
            matches!(decoded.threads[0].compose.send_state, SendState::Idle),
            "the key is still written, at its idle default",
        );
    }

    #[test]
    fn snapshot_bytes_is_deterministic_regardless_of_insert_order() {
        let a = DraftStore::new();
        a.set(ThreadId("z".into()), sample_compose("z"));
        a.set(ThreadId("a".into()), sample_compose("a"));
        a.set(ThreadId("m".into()), sample_compose("m"));

        let b = DraftStore::new();
        b.set(ThreadId("a".into()), sample_compose("a"));
        b.set(ThreadId("m".into()), sample_compose("m"));
        b.set(ThreadId("z".into()), sample_compose("z"));

        assert_eq!(a.snapshot_bytes(same_ids), b.snapshot_bytes(same_ids));
    }

    /// The full at-rest path: serialise → seal under `BackupKey` → unseal →
    /// restore → byte-equal. Proves the "one shared-Rust impl sealed under the
    /// owner's `BackupKey`" success property end-to-end in shared Rust.
    #[test]
    fn seal_unseal_round_trips_byte_equal() {
        let key = BackupKey::derive(&[7u8; 32]);

        let a = DraftStore::new();
        a.set(ThreadId("thread".into()), sample_compose("draft body"));
        let plain = a.snapshot_bytes(same_ids);

        let sealed = encrypt_backup_chunk(&key, &plain).unwrap();
        // Sealed bytes carry the ChaCha20 `0x01` version byte and are not the
        // plaintext — the nest stores exactly these, opaque.
        assert_ne!(sealed, plain);
        assert_eq!(sealed[0], 0x01);

        let unsealed = decrypt_backup_chunk(&key, &sealed).unwrap();
        assert_eq!(unsealed, plain);

        let b = DraftStore::new();
        b.restore_from_bytes(&unsealed, same_keys).unwrap();
        assert_eq!(b.snapshot_bytes(same_ids), plain);
    }

    #[test]
    fn restore_rejects_garbage() {
        let store = DraftStore::new();
        let err = store
            .restore_from_bytes(&[0xFF, 0xFF, 0xFF], same_keys)
            .unwrap_err();
        assert!(matches!(err, DraftRestoreError::Decode(_)));
    }

    #[test]
    fn empty_store_round_trips() {
        let a = DraftStore::new();
        let bytes = a.snapshot_bytes(same_ids);
        let b = DraftStore::new();
        b.restore_from_bytes(&bytes, same_keys).unwrap();
        assert_eq!(b.snapshot_bytes(same_ids), bytes);
        assert!(b.new_thread().is_none());
    }

    /// `conversations.md` § Persistence pins the new-thread slot's lifetime:
    /// "**Only** an explicit **cancel/discard** (`cancel_new_conversation`) or
    /// **calling send** (`send_new_thread`) clears the new-thread draft."
    /// A launch restore was a silent third clearer.
    ///
    /// The harm is not cosmetic, and it is the failure this test was written
    /// from. `set_new_thread_recipient_input` / `accept_new_thread_chip` are
    /// `if let Some(..)` no-ops when the slot is absent, while the composer's
    /// *visibility* is a separate flag (`new_thread_active`) the restore never
    /// touches — so a slot cleared underneath an open composer leaves the user
    /// typing into a live-looking picker that can no longer record a keystroke
    /// or start a resolve. Observed on macOS as `recipient-resolve-status did
    /// not reach terminal state (last state read: 'idle'; element visible:
    /// True)` with no probe in the app log at all.
    ///
    /// The window is real on every app: the restore is a `__drafts` fetch the
    /// shells fire at login and await off the UI thread, so it lands whenever
    /// the network says, including after the user has opened `+` and started
    /// typing.
    #[test]
    fn a_restore_never_clears_a_live_new_thread_draft() {
        // What rests on the nest: no new-thread slot — the ordinary shape once
        // the last device cancelled or sent.
        let resting = DraftStore::new();
        let bytes = resting.snapshot_bytes(same_ids);

        // What this device is doing meanwhile: composer open, recipient typed.
        let live = DraftStore::new();
        live.set_new_thread(Some(ComposeState {
            recipient_picker: Some(RecipientPickerState {
                raw_input: "alice@self-nest.test".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }));

        // The launch restore lands late (a slow `__drafts` fetch under load).
        live.restore_from_bytes(&bytes, same_keys).unwrap();

        let slot = live
            .new_thread()
            .expect("a restore must never clear a live new-thread draft");
        assert_eq!(
            slot.recipient_picker.unwrap_or_default().raw_input,
            "alice@self-nest.test",
            "the half-typed recipient survives the restore",
        );
    }

    /// The same rule one slot over: a per-thread reply being typed is user
    /// input the restore did not author and may not overwrite. The blob is at
    /// best as old as the last autosave, so on a collision the live compose is
    /// strictly the newer of the two.
    #[test]
    fn a_restore_never_overwrites_a_live_per_thread_draft() {
        let resting = DraftStore::new();
        resting.set(ThreadId("t".into()), sample_compose("stale, from the blob"));
        let bytes = resting.snapshot_bytes(same_ids);

        let live = DraftStore::new();
        live.set(
            ThreadId("t".into()),
            sample_compose("what the user is typing"),
        );
        // A thread the blob knows nothing about is still the user's.
        live.set(ThreadId("local-only".into()), sample_compose("untouched"));

        live.restore_from_bytes(&bytes, same_keys).unwrap();

        assert_eq!(
            live.get(&ThreadId("t".into())).body_draft,
            "what the user is typing",
            "the live compose is newer than the blob and wins",
        );
        assert_eq!(
            live.get(&ThreadId("local-only".into())).body_draft,
            "untouched",
            "a restore fills; it never removes a draft the blob omits",
        );
    }

    /// The other half of the rule, so the fix above cannot be mistaken for
    /// "a restore stops restoring": with nothing live to protect, every slot is
    /// adopted exactly as before — including into a composer the user has
    /// merely *opened*, which is how a draft written on another device reaches
    /// this one (`conversations.md` § Persistence — drafts are synced across
    /// the user's devices).
    #[test]
    fn a_restore_still_fills_an_open_but_untouched_composer() {
        let resting = DraftStore::new();
        resting.set_new_thread(Some(sample_compose("written on the other device")));
        let bytes = resting.snapshot_bytes(same_ids);

        let live = DraftStore::new();
        // Exactly what `start_new_conversation` seeds: the composer is open and
        // holds an empty picker, and the user has typed nothing.
        live.set_new_thread(Some(ComposeState {
            recipient_picker: Some(RecipientPickerState::default()),
            ..Default::default()
        }));

        live.restore_from_bytes(&bytes, same_keys).unwrap();

        assert_eq!(
            live.new_thread().expect("the slot is present").body_draft,
            "written on the other device",
            "an untouched composer takes the synced draft",
        );
    }

    // ── The content-index corpus seam ─────────────────────────────────────────

    /// Records every corpus the store hands the index.
    #[derive(Default)]
    struct CorpusSpy(std::sync::Mutex<Vec<Vec<IndexableDraft>>>);

    impl crate::index_sink::MessageIndexObserver for CorpusSpy {
        fn observe_indexable_message(&self, _msg: crate::index_sink::IndexableMessage<'_>) {}
        fn observe_draft_corpus(&self, drafts: &[IndexableDraft]) {
            self.0.lock().unwrap().push(drafts.to_vec());
        }
    }

    impl CorpusSpy {
        fn latest(&self) -> Vec<IndexableDraft> {
            self.0.lock().unwrap().last().cloned().unwrap_or_default()
        }
        fn count(&self) -> usize {
            self.0.lock().unwrap().len()
        }
    }

    fn spied_store() -> (DraftStore, Arc<CorpusSpy>) {
        let store = DraftStore::new();
        let spy = Arc::new(CorpusSpy::default());
        store.set_index_observer(
            Arc::clone(&spy) as Arc<dyn crate::index_sink::MessageIndexObserver>
        );
        (store, spy)
    }

    /// **Every** mutation path notifies — which is why the observer lives on the
    /// store rather than on the twenty-odd compose mutators that call into it.
    #[test]
    fn every_mutation_hands_the_index_the_whole_corpus() {
        let (store, spy) = spied_store();
        let at_registration = spy.count();

        store.set(ThreadId("t1".into()), sample_compose("alpha"));
        store.set(ThreadId("t2".into()), sample_compose("bravo"));
        assert_eq!(
            spy.latest().len(),
            2,
            "the corpus is the payload, not the one draft that changed"
        );

        store.clear(&ThreadId("t1".into()));
        assert_eq!(
            spy.latest().len(),
            1,
            "a discard reaches the index as a smaller corpus"
        );

        store.clear_all();
        assert!(
            spy.latest().is_empty(),
            "clearing everything is an empty corpus, not a missing notification"
        );
        assert_eq!(
            spy.count() - at_registration,
            4,
            "set, set, clear, clear_all — every one of them notified"
        );
    }

    /// Registration itself offers the corpus that already exists: a restore
    /// normally lands before app glue registers the sink, so waiting for the
    /// next keystroke would leave restored drafts unsearchable for the session.
    #[test]
    fn registering_the_sink_offers_the_existing_corpus() {
        let store = DraftStore::new();
        store.set(ThreadId("t1".into()), sample_compose("alpha"));

        let spy = Arc::new(CorpusSpy::default());
        store.set_index_observer(
            Arc::clone(&spy) as Arc<dyn crate::index_sink::MessageIndexObserver>
        );

        assert_eq!(spy.latest().len(), 1, "the pre-existing draft was offered");
    }

    /// A restore is the drafts arm's catch-up leg, and it reaches the seam.
    #[test]
    fn a_restore_notifies_the_index() {
        let source = DraftStore::new();
        source.set(ThreadId("t1".into()), sample_compose("alpha"));
        let bytes = source.snapshot_bytes(same_ids);

        let (store, spy) = spied_store();
        store
            .restore_from_bytes(&bytes, same_keys)
            .expect("restore");

        assert_eq!(
            spy.latest().len(),
            1,
            "restored drafts must reach the builder — no compose mutator runs on \
             this path"
        );
    }

    /// Blank composes are excluded: `get` synthesizes a default for any thread
    /// the user merely opened, and the mutators write it straight back.
    #[test]
    fn empty_composes_are_not_part_of_the_corpus() {
        let (store, spy) = spied_store();

        store.set(
            ThreadId("opened-but-untyped".into()),
            ComposeState::default(),
        );
        assert!(
            spy.latest().is_empty(),
            "an empty compose can never match a query"
        );

        store.set(
            ThreadId("whitespace".into()),
            ComposeState {
                body_draft: "   \n ".into(),
                ..Default::default()
            },
        );
        assert!(spy.latest().is_empty(), "nor can whitespace");

        // A subject with no body still counts — it is searchable text.
        store.set(
            ThreadId("subject-only".into()),
            ComposeState {
                subject_draft: Some("quarterly walrus report".into()),
                ..Default::default()
            },
        );
        assert_eq!(spy.latest().len(), 1);
    }

    /// The thread-less new-thread compose is in the corpus under its reserved
    /// id — it is the draft a user is most likely to be actively writing.
    #[test]
    fn the_new_thread_compose_is_indexed_under_its_reserved_id() {
        let (store, spy) = spied_store();
        store.set_new_thread(Some(sample_compose("unsent thoughts")));

        let corpus = spy.latest();
        assert_eq!(corpus.len(), 1);
        assert_eq!(corpus[0].content_id, NEW_THREAD_DRAFT_ID);
        assert_eq!(
            corpus[0].thread_id, None,
            "it belongs to no thread, and its nav target says so"
        );
    }

    /// Two devices holding equal drafts must produce equal corpora — the corpus
    /// is sealed into a segment, so iteration order would otherwise make the
    /// bytes differ for identical state (the reason `snapshot_bytes` sorts too).
    #[test]
    fn the_corpus_order_is_deterministic() {
        let (a, spy_a) = spied_store();
        let (b, spy_b) = spied_store();

        for id in ["t3", "t1", "t2"] {
            a.set(ThreadId(id.into()), sample_compose(id));
        }
        for id in ["t2", "t3", "t1"] {
            b.set(ThreadId(id.into()), sample_compose(id));
        }

        let ids: Vec<String> = spy_a.latest().into_iter().map(|d| d.content_id).collect();
        assert_eq!(ids, vec!["t1", "t2", "t3"], "sorted by content id");
        assert_eq!(
            ids,
            spy_b
                .latest()
                .into_iter()
                .map(|d| d.content_id)
                .collect::<Vec<_>>()
        );
    }
}
