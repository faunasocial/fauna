//! The content-index **sink seam** — how a client-side index builder learns
//! that a message it should index has just been ingested.
//!
//! Ratified shape (`docs/goal/behavior/content-index.md` § Ingest triggers, v1
//! → *The receive hook*): an **observer seam, not a hard dependency**. This
//! crate stays tantivy-free — it hands the sink already-decrypted, already-
//! parsed text and identifiers; the builder (`libs/fauna-client-index`) owns
//! tokenizing, sealing, and publishing. App glue only registers the sink.
//!
//! Modelled on the two existing observer precedents: the post-decrypt
//! [`ConversationsManager::observe_local_detection`] hook (same call sites) and
//! `fauna_client_folders::FolderCustodyObserver` (the "narrowest possible
//! edge" rule — no key material, no index handle crosses this boundary).
//!
//! **Where it fires.** Both inbound chokepoints named by the goal doc, one
//! frame inside them: [`ConversationsManager::ingest_inbound`] (the mail rail's
//! `backends::smtp::ingest_inbound_record` funnels here, as does every other
//! participant-keyed rail) and [`ConversationsManager::ingest_inbound_to_thread`]
//! (the MLS conversations rail). That frame — rather than
//! `ingest_inbound_record` itself — is where the message-id dedup has already
//! run and the **thread id exists**, so the sink never re-indexes a duplicate
//! the manager just dropped and every emitted doc carries a real navigation
//! target (`docs/goal/ui/search.md` § State & data shape: local rows always
//! carry `Some(navigation)`).
//!
//! **Contract for implementors:** called synchronously from the ingest path
//! while the manager holds no lock the sink can reach, and it **must not
//! block** — batch or notify, never do I/O inline.

use crate::message::MessageId;
use crate::thread::ThreadId;

/// Which content kind an ingested message belongs to. Deliberately a small
/// closed set rather than a re-export of `fauna_index::ContentKind`: that type
/// lives behind tantivy, which this crate must not depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexableKind {
    /// SMTP-rail mail — the mail-kind slice (sealed under the MSEK-derived
    /// index-segment key; rollout slice S3).
    Mail,
    /// A fauna-native (MLS) conversation message — the master-key slice
    /// (rollout slice S4).
    Conversation,
}

/// One just-ingested message, in exactly the shape a content-index builder
/// needs: the searchable text, the timestamp, and the identifiers a result row
/// navigates by. Borrowed throughout — the sink copies what it keeps.
#[derive(Debug, Clone, Copy)]
pub struct IndexableMessage<'a> {
    pub kind: IndexableKind,
    /// Navigation target part 1 — the thread the message landed in.
    pub thread_id: &'a ThreadId,
    /// Navigation target part 2, and the index's `content_id`: the RFC
    /// `Message-ID` (or the manager's stable synthetic fallback for a message
    /// that carried none). Producer-owned and stable across devices, which is
    /// what makes an independently built segment dedup against another's.
    pub message_id: &'a MessageId,
    /// Subject line, when the message has one → the index's `Title` field.
    pub subject: Option<&'a str>,
    /// The canonical body text (markdown / plaintext source, never the render
    /// projection) → the index's `Body` field.
    pub body: &'a str,
    /// Present only for actor-addressed rails; mail senders are email
    /// addresses, not actors, so the mail rail passes `None`.
    pub sender_actor_id: Option<&'a [u8; 32]>,
    /// The nest's segment-record id for this message, when the ingest frame
    /// holds one — the mail rail's `InboundMailRecord::message_id` (raw 32
    /// bytes). Becomes the doc's stored *secondary* identity, the spelling the
    /// MDA's `SEARCH` coverage asks in (`content-index.md` § Where the index
    /// is built → the 2026-08-10 carrier ruling). `None` on rails that have no
    /// nest record id (MLS conversations, drafts corpus, walks) — never a
    /// substitute for [`Self::message_id`], which stays the content id.
    pub nest_message_id: Option<&'a [u8]>,
    pub timestamp_ms: i64,
    /// Whether this copy is the **account's own** — for mail, that it was
    /// served by the `Sent` mailbox, which is the only proof of authorship
    /// (`../../docs/goal/ui/conversations.md` § Receiving into the
    /// conversations view: "`Sent` is proof of authorship and `INBOX` never
    /// is, regardless of what `From:` claims").
    ///
    /// **What the index does with it: only an own copy may supersede a doc
    /// already indexed under its content id.** The Message-ID collision rule
    /// settles a collision one way — the `Sent` copy is the message, and an
    /// `INBOX` record never displaces a held copy — and the index has to apply
    /// the *same* asymmetry or it oscillates. A squat keeps re-paging from
    /// `INBOX` every launch and the thread store starts empty every launch, so
    /// on every launch the squat reaches this seam **before** the `Sent` copy
    /// does; a rule that superseded on any content change would let the squat
    /// take the id back each time and the `Sent` copy take it again, rewriting
    /// a segment twice per launch for ever. Ownership is what breaks the tie,
    /// and it breaks it in the direction the collision rule already chose.
    ///
    /// Rails with no ownership notion pass `false`; they also carry no nest
    /// record id, so they never reach the supersession arm at all.
    pub is_own: bool,
}

/// One draft, in the shape the index builder needs — the **snapshot kind's**
/// unit (`content-index.md` § Ingest triggers, v1 → *Drafts are a snapshot
/// kind*).
///
/// Owned rather than borrowed, unlike [`IndexableMessage`]: a draft corpus is
/// assembled from the store under its own read lock and handed over as a whole,
/// so there is no ingest frame whose borrows could outlive the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexableDraft {
    /// The draft's **stable identity**, and the index's `content_id`: the thread
    /// it belongs to, or [`NEW_THREAD_DRAFT_ID`] for the single-slot new-thread
    /// compose. Stable across edits on purpose — an id that changed with the
    /// body would leave every past version live in the index forever.
    pub content_id: String,
    /// The composer to open — `None` for the new-thread slot, which belongs to
    /// no thread yet.
    pub thread_id: Option<ThreadId>,
    /// The draft subject, when one is being composed → the index's `Title`.
    pub subject: Option<String>,
    /// The draft body → the index's `Body`.
    pub body: String,
}

/// The reserved `content_id` of the new-thread compose slot.
///
/// `DraftStore` holds per-thread drafts *and* one thread-less new-thread
/// compose (`store/drafts.rs`), and the latter has no natural id. It gets a
/// reserved one rather than being left out of the corpus: it is the draft a user
/// is most likely to be actively writing, and dropping it would make "search
/// finds what I am typing" quietly untrue for the main compose surface. The
/// prefix cannot collide with a `ThreadId`, which is hex.
pub const NEW_THREAD_DRAFT_ID: &str = "draft:new-thread";

/// The seam itself. Registered by app glue via
/// [`ConversationsManager::set_index_observer`]; a client with no local index
/// builder (web — no browser tantivy, `content-index.md` § Where queries run)
/// registers none and is structurally unaffected.
///
/// [`ConversationsManager::set_index_observer`]: crate::manager::ConversationsManager::set_index_observer
pub trait MessageIndexObserver: Send + Sync {
    /// A message was just ingested and is now visible in the thread store.
    /// Must not block.
    fn observe_indexable_message(&self, msg: IndexableMessage<'_>);

    /// This session's launch catch-up for `kind` has **completed**: everything
    /// the seam has delivered for that kind so far was the launch backlog, and
    /// everything it delivers from here is the receive-path trickle.
    ///
    /// **Per kind, because the two kinds' backlogs finish at different moments
    /// and for different reasons** — mail's is the first clean mailbox sweep,
    /// Conversation's is the restore + first clean conversation refold
    /// (`content-index.md` § Ingest triggers, v1 → *The Conversation kind's
    /// catch-up*). A single kind-less signal would have to pick one of them,
    /// and either choice is wrong for the other builder: fired on mail's
    /// boundary it closes the Conversation window before the refold has walked
    /// the channels (reclassifying the rest of that backlog as trickle, which
    /// the lease never gates — the exact N× republish the boundary exists to
    /// prevent), and a mail-enabled actor whose feed errors every sweep would
    /// never close the Conversation window at all. Implementors take their own
    /// kind and ignore the rest, exactly as [`Self::observe_indexable_message`]
    /// already does.
    ///
    /// The two shapes are governed differently — `content-index.md` § Where the
    /// index is built → *The builder and the advisory task lease*: an advisory
    /// stand-down may bind only build work whose queue **re-presents**, which
    /// the backlog does (every launch re-pages the mailbox from UID 0 and the
    /// stage-time guard recomputes the missing set) and the trickle does not (a
    /// doc flows past this seam once per session, so a skipped one leaves live,
    /// *visible* mail unsearchable on that seat). A builder under a lease
    /// therefore consults its gate before this call and ignores it after.
    ///
    /// **Fired at most once per window, and only after a walk that completed
    /// without error.** Both boundaries are structural facts of the receive loop
    /// rather than new machinery — the first `poll_mail_feeds` pages from UID 0
    /// to the live tip inside one call, and the first `poll_bound` folds every
    /// bound channel from its restored watermark. A walk that *errored* does not
    /// fire it: its cursor did not advance, so the next sweep re-pages the same
    /// backlog and gets another chance to close the boundary honestly. A window
    /// is normally the session's one; [`Self::observe_catch_up_reopened`] is the
    /// only way a kind gets another.
    ///
    /// Default no-op: an observer that does no pass-shaped work (a test
    /// recorder, a future kind whose builder is purely incremental) is
    /// structurally unaffected, exactly as a client with no builder registers no
    /// observer at all.
    fn observe_catch_up_complete(&self, kind: IndexableKind) {
        let _ = kind;
    }

    /// `kind`'s catch-up window **opens again**: backlog that was not walkable
    /// when the boundary closed is about to reach the seam, and must count as
    /// backlog — not as the trickle the closed boundary would make of it.
    /// [`Self::observe_catch_up_complete`] follows once that walk has folded
    /// cleanly.
    ///
    /// One producer today: a **community room's key-in** landing after the
    /// Conversation boundary closed without it. A room waiting for its key-in
    /// folds for now (`content-index-ingest.md` § Ingest triggers, v1 → *A
    /// community room waiting for its key-in*), so everything behind its wait —
    /// the room's whole history, for a newcomer — is walked only after the key
    /// arrives. The receive loop fires this before that walk ingests a record.
    ///
    /// Per kind, for the reason the boundary is. Default no-op, like the
    /// boundary's: a dropped reopen leaves the backlog staging as trickle —
    /// what it did before this signal existed — so a forwarding wrapper that
    /// forgot it would lose no content, only the lease's withholding of it.
    fn observe_catch_up_reopened(&self, kind: IndexableKind) {
        let _ = kind;
    }

    /// The user's **entire draft corpus**, as it stands after a change.
    ///
    /// A snapshot, not a delta, and that is the whole design of the drafts arm
    /// (`content-index.md` § Ingest triggers, v1 → *Drafts are a snapshot
    /// kind*). Drafts differ from every kind indexed before them on two axes at
    /// once — a draft is **edited**, and a draft is **discarded** — and neither
    /// is expressible as an append:
    ///
    /// - An edit cannot be superseded by re-staging. `Index::add_doc` upserts by
    ///   content id, but only within one un-flushed batch: once a segment is
    ///   sealed and published, no later `delete_term` can reach into it, so the
    ///   old text stays queryable from the old segment and a fold does not
    ///   collapse it either. Both facts are pinned by
    ///   `fauna-index/tests/cross_segment_supersession.rs`.
    /// - A discard has no representation at all in an append stream.
    ///
    /// Delivering the corpus makes both exact: the builder publishes one segment
    /// holding it and tombstones every earlier draft segment, so what is at rest
    /// is always precisely the current drafts — edits superseded, discards gone,
    /// and the at-rest size bounded by the corpus rather than by the user's
    /// keystroke count.
    ///
    /// Fired from every `DraftStore` mutation (set, discard, clear, restore), so
    /// the corpus reaching the builder cannot drift from the one the user sees.
    /// Same non-blocking contract as [`Self::observe_indexable_message`].
    ///
    /// Default no-op, for the same reason the boundary signal has one.
    fn observe_draft_corpus(&self, drafts: &[IndexableDraft]) {
        let _ = drafts;
    }
}
