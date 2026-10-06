//! The **local arm**, registered: `fauna_client_search`'s [`LocalSearchIndex`]
//! seam implemented over this crate's [`MailIndexReader`].
//!
//! This module is the one place the two halves of backend 2 meet. Everything on
//! either side already existed — the manager fires a local arm when one is
//! registered (`ui/search.md` § State & data shape), and the reader opens the
//! sealed mail/calendar slice — but nothing connected them, so
//! `SearchManager::has_local_index()` answered `false` on every app and the
//! Search page was nest-only.
//!
//! # Why the projection lives here and not in the shared crate
//!
//! The seam yields **already-projected** hits, because a local row's snippet
//! must render from locally-held content (`content-index.md` § Don't do these:
//! "no Tantivy stored fields; snippets render from fetched content") and its
//! navigation target must be resolved from the content id. A [`QueryHit`] car‑
//! ries no text at all — `(kind, content_id, timestamp_ns, sender_actor_id,
//! score)`. Only the native side has the content stores that answer both, so
//! resolving them is this crate's job and the merge stays engine-free.
//!
//! # The unresolvable hit is DROPPED, not rendered inert
//!
//! `ui/search.md` § State & data shape states that **local rows always carry
//! `Some(navigation)`**. The sealed index outlives the client's in-memory
//! conversations store (segments persist across launches; the store is rebuilt
//! by each launch's mailbox re-walk), so the index can legitimately know a
//! message id the store cannot currently resolve. Such a hit has neither a
//! snippet nor a thread to open — a row with an empty body and a dead click,
//! which is worse than no row. Dropping it is what keeps that ratified claim
//! true by construction rather than by convention, and nothing is *lost*: the
//! hit **re-resolves on a later query** once the store's re-walk catches up.
//!
//! ⚠ Not "backend 1 covers the same content from the nest side" — that was the
//! rule's original rationale and it is **retired** (`ui/search.md` § State &
//! data shape, corrected 2026-08-05). It holds only for the public floor
//! corpus; a sealed kind (conversations, drafts, contacts) has no nest-side
//! twin to fall back to. Re-resolution is the general guarantee.

use std::sync::Arc;

use fauna_client_search::{LocalSearchHit, LocalSearchIndex, SearchKindClass, SearchNav};
use fauna_index::{ContentKind, QueryHit};
use tokio::sync::Mutex;

use crate::index_builder::file_content_id;
use crate::rail_publisher::{
    IndexRailPublisher, MailIndexReader, MailcalKeyRing, MasterIndexReader, open_mail_reader,
    open_master_reader,
};

/// One resolved message: what the projection needs that the index cannot hold.
#[derive(Clone, Debug, PartialEq)]
pub struct LocatedMessage {
    /// The thread holding the message — `SearchNav::Mail`'s open target.
    pub thread_id: String,
    /// The message's plaintext body, which the snippet renders from.
    pub body: String,
}

/// Resolves a mail message id against content this device holds locally.
///
/// A seam rather than a direct `ConversationsManager` dependency for two
/// reasons: it makes the projection unit-testable without standing up a whole
/// conversations session, and rollout S4's master-key kinds (conversations,
/// posts, contacts, drafts) each need their own resolver against the same shape.
/// The production implementation over the manager is below.
pub trait MailContentLookup: Send + Sync {
    /// `None` when this device's store does not hold the message — a normal
    /// state (see the module docs), never an error.
    fn locate(&self, message_id: &str) -> Option<LocatedMessage>;

    /// Resolve a **draft** by the content id the builder indexed it under: a
    /// thread id, or `NEW_THREAD_DRAFT_ID` for the new-thread compose slot.
    ///
    /// A separate method rather than a reuse of [`Self::locate`] because a draft
    /// is not message-shaped — it has no `MessageId` to look up and no thread to
    /// select a message inside — which is exactly the extension this seam's docs
    /// anticipated for non-message kinds.
    ///
    /// `None` when the draft is gone (discarded on this device, or the corpus
    /// moved on), which drops the hit under the same rule an unresolvable mail
    /// hit takes.
    fn locate_draft(&self, content_id: &str) -> Option<LocatedDraft>;
}

/// One resolved address-book card: the current text to render, keyed by the
/// identity the index holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedContact {
    /// The card's `uid_hash`, hex-lowercase — matches the indexed `content_id`.
    pub uid_hash: String,
    /// The card's **current** display text, which the snippet renders from.
    pub body: String,
}

/// Reads the live address-book corpus at query time — the third ingest class's
/// resolver (`content-index.md` § Where queries run → *The third ingest class
/// resolves the same way, one step further out*).
///
/// **Async and whole-corpus, where [`MailContentLookup`] is sync and per-id, and
/// the difference is the whole point of the class.** A contact hit has no
/// client-resident store to resolve against — the corpus lives on the nest and
/// is reachable only by an explicit network read — so resolution is a wire read
/// rather than a store lookup. It is taken **once per query for the entire
/// result set**, never once per hit, which is what keeps a page of contact hits
/// to a single round trip.
///
/// A failed read (offline, nest unreachable) yields *no contact rows* rather
/// than an error row — the existing no-reader posture, stated for this class in
/// § Where queries run.
#[async_trait::async_trait]
pub trait ContactCorpusRead: Send + Sync {
    /// Every card in every one of the actor's address books.
    async fn read_corpus(&self) -> Result<Vec<LocatedContact>, String>;
}

/// One resolved draft: the current body to render, and where to open it.
#[derive(Clone, Debug, PartialEq)]
pub struct LocatedDraft {
    /// The composer to open — `None` for the new-thread slot.
    pub thread_id: Option<String>,
    /// The draft's **current** body text.
    pub body: String,
}

/// Reads the current text of the user's own posts at query time — the third
/// ingest class's resolver in its **per-hit** shape (`content-index.md`
/// § Where queries run: *one `fauna.posts.get` per post hit, bounded by the
/// page size*), where [`ContactCorpusRead`] is whole-corpus — a post corpus has
/// no one-call whole read short of re-paging the enumeration, and a page of
/// hits is already the bound.
///
/// Infallible by shape: an id absent from the returned map — deleted
/// (`fauna.posts.not_found`), unreadable, or offline — DROPS its hit under the
/// ratified unresolvable-hit rule, which is exactly what makes deletion
/// display-correct with no index mutation. A wholesale transport failure is an
/// empty map: *no local post rows*, never an error row.
#[async_trait::async_trait]
pub trait PostCorpusRead: Send + Sync {
    /// `post_id → current body text` for every id that still resolves.
    async fn read_posts(&self, post_ids: &[String]) -> std::collections::HashMap<String, String>;
}

/// One resolved file: what the row renders, and whether its bytes are reachable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedFile {
    /// The owning set's stable `FolderSummary.id` — the identity half the
    /// navigation target carries.
    pub folder_id: i64,
    /// `path_hash`, hex-lowercase — the other identity half.
    pub path_hash_hex: String,
    /// The file's **current** rendered path (unsealed client-side).
    pub path: String,
    /// Whether the backing set's source device is currently reachable — the
    /// Media page's availability affordance (`MediaItem.source_online`).
    pub source_online: bool,
}

/// Reads the user's live file corpus at query time — the third ingest class's
/// resolver in its **whole-corpus** shape, like [`ContactCorpusRead`] and unlike
/// the per-hit [`PostCorpusRead`] (`content-index.md` § Where queries run).
///
/// One `fauna.media.list` drain resolves every File hit in a result set at once:
/// the wire has no by-identity file read to do per-hit lookups with, and the
/// drain is the same call the walk already makes.
///
/// Three things come out of that one read, which is why it returns rows rather
/// than a name map: **liveness** (a hit absent from the listing was deleted *or
/// renamed* → DROPPED), the **current name** for the snippet, and
/// **`source_online`** for the availability affordance. A failed read is an
/// empty listing — *no file rows*, never an error row.
#[async_trait::async_trait]
pub trait FileCorpusRead: Send + Sync {
    /// Every file in every set this reader can render, keyed by identity.
    async fn read_files(&self) -> Result<Vec<LocatedFile>, String>;
}

impl MailContentLookup for fauna_conversations::ConversationsManager {
    fn locate(&self, message_id: &str) -> Option<LocatedMessage> {
        self.locate_message(message_id)
            .map(|(thread_id, body)| LocatedMessage {
                thread_id: thread_id.0,
                body,
            })
    }

    fn locate_draft(&self, content_id: &str) -> Option<LocatedDraft> {
        self.locate_draft(content_id)
            .map(|(thread_id, body)| LocatedDraft {
                thread_id: thread_id.map(|t| t.0),
                body,
            })
    }
}

/// The mail/calendar slice of backend 2, as the shared `SearchManager` sees it.
///
/// Holds no **root** key: the MSEK stays inside the launcher that built this
/// (`fauna_client_conversations::NestMailIndexLauncher`), which hands app glue
/// this opaque object and never a key (`conversations.md` § Architectural rules
/// #2). What this holds is the already-derived [`MailcalKeyRing`] — a carrier of
/// [`crate::IndexSegmentKey`]s, each zeroize-on-drop.
///
/// **Why the ring rather than the MSEK** (`key-material-hierarchy.md` § Plaintext
/// key lifetime on bridges → *Carrier shape*). This used to be `msek: [u8; 32]`,
/// minted by copying the raw key out of the launcher's cache — `msek: *msek`,
/// from `&keys.msek`. That is the third instance of one shape: [`MailKeys`]
/// itself and `NestContactCorpus` were
/// each fixed when a finding named them, leaving this one — a bare `[u8; 32]` is
/// `Copy`, so it duplicated silently on every assignment and pass-by-value, none
/// of the duplicates were ever zeroized, and this object is held behind an `Arc`
/// for the whole login session. The doc comment above it *claimed* "Holds no
/// key" from the day it was written, which is how it survived.
///
/// Deriving the ring at construction — where the launcher already has the MSEK
/// in hand — rather than per query from a held copy makes that claim true by
/// construction, and makes this arm symmetric with its sibling
/// [`MasterLocalSearch`], which has always taken a custody type
/// ([`crate::IndexMasterKey`]) rather than seed bytes (priority #4: the richest
/// pattern was one call site away). The ring is the current generation only,
/// exactly as the per-query `from_msek` derivation it replaces — same freshness,
/// one derivation instead of one per query.
///
/// No separate compile-time pin is needed to keep this non-`Copy`: every
/// `IndexSegmentKey` in the ring has a destructor, which makes `Copy`
/// uncompilable (`E0184`) for the ring and therefore for this type.
pub struct MailLocalSearch {
    keys: MailcalKeyRing,
    publisher: Arc<IndexRailPublisher>,
    lookup: Arc<dyn MailContentLookup>,
    /// The reader opened at some past query, kept so a query whose index has not
    /// moved pays one `list()` instead of refetching and unsealing every live
    /// segment. See [`Self::reader`] for why the manifest hash is the right
    /// staleness key.
    cached: Mutex<Option<Arc<MailIndexReader>>>,
}

impl MailLocalSearch {
    /// Takes the **derived ring**, not the MSEK — see the type's docs for why
    /// (`key-material-hierarchy.md` § Carrier shape). The caller holds the root
    /// and derives once; this object never sees it.
    pub fn new(
        keys: MailcalKeyRing,
        publisher: Arc<IndexRailPublisher>,
        lookup: Arc<dyn MailContentLookup>,
    ) -> Self {
        Self {
            keys,
            publisher,
            lookup,
            cached: Mutex::new(None),
        }
    }

    /// The reader to answer this query with — the cached one if the published
    /// index has not moved, a freshly opened one otherwise.
    ///
    /// **Why the manifest hash is the staleness key.** The mail/calendar
    /// manifest names every live segment, so it changes on exactly the events
    /// that change what a query can find (a flush appends a segment; compaction
    /// folds and tombstones some) and on no others. Checking it costs one
    /// `list()`; opening blind would refetch and unseal every live segment on
    /// every keystroke-submitted query, which for a real mailbox is megabytes a
    /// query. A never-published actor caches an empty reader against `None` and
    /// re-opens as soon as the first manifest appears.
    async fn reader(&self) -> Result<Arc<MailIndexReader>, String> {
        let published = self
            .publisher
            .mailcal_manifest_hash()
            .await
            .map_err(|e| e.to_string())?;

        let mut cached = self.cached.lock().await;
        if let Some(reader) = cached.as_ref()
            && reader.manifest_hash() == published.as_deref()
        {
            return Ok(Arc::clone(reader));
        }

        let reader = Arc::new(
            open_mail_reader(&self.keys, self.publisher.as_ref())
                .await
                .map_err(|e| e.to_string())?,
        );
        *cached = Some(Arc::clone(&reader));
        Ok(reader)
    }
}

#[async_trait::async_trait]
impl LocalSearchIndex for MailLocalSearch {
    async fn query(
        &self,
        query: &str,
        kinds: &[SearchKindClass],
        limit: usize,
    ) -> Result<Vec<LocalSearchHit>, String> {
        let selected = local_kinds(kinds);
        if selected.is_empty() {
            // The manager already skips the arm when the filter selects no local
            // class; this covers a filter that selects only classes another
            // key's reader serves (rollout S4's master-key kinds), which this
            // reader would answer empty anyway. Answering without a rail round
            // trip is the honest version of that empty answer.
            return Ok(Vec::new());
        }

        let reader = self.reader().await?;
        let owned_query = query.to_string();
        // Tantivy's search is blocking, and the seam is async precisely so an
        // implementation can keep it off the reactor.
        let hits =
            tokio::task::spawn_blocking(move || reader.query(&owned_query, &selected, None, limit))
                .await
                .map_err(|e| format!("local index query panicked: {e}"))?
                .map_err(|e| e.to_string())?;

        Ok(hits
            .iter()
            .filter_map(|hit| project_hit(hit, self.lookup.as_ref()))
            .collect())
    }
}

/// The **master-class** slice of backend 2 — the counterpart of
/// [`MailLocalSearch`] over the other key class.
///
/// Holds no key for the same reason: the seed-derived master key stays inside
/// the launcher that built this.
///
/// # Why this type had to exist before any master kind was searchable
///
/// The manager holds exactly **one** local arm slot, and app glue filled it with
/// a [`MailLocalSearch`], whose class map answers `None` for every master kind.
/// So the Conversation kind — which has had a producer since 2026-08-05 and sits
/// in `MASTER_KINDS_BUILT` — was sealing segments onto the rail that no query
/// path could ever read back: indexed, replicated, and invisible. Adding a kind
/// to the build list without its query half is the exact failure
/// `content-index.md` § Ingest triggers, v1 → *Registration happens at BOTH ends
/// of the pipeline* records, and the one the constant's own doc comment forbids.
/// [`CompositeLocalSearch`](fauna_client_search::CompositeLocalSearch) is what
/// lets both readers occupy the single slot.
pub struct MasterLocalSearch {
    key: fauna_index::IndexMasterKey,
    publisher: Arc<IndexRailPublisher>,
    lookup: Arc<dyn MailContentLookup>,
    /// The contacts resolver, when this seat can read the address book.
    ///
    /// `Option` because it is **not** gated on the same thing the rest of the
    /// class is: every master kind needs only the identity seed, but a card
    /// body is sealed to the actor's MSEK-derived recipient keypair
    /// (`fauna_client_carddav::unseal_card_body`), so contacts cannot be read —
    /// or indexed — before mail is provisioned. `None` is the ordinary state of
    /// an actor without mail, and it removes `Contact` from the kinds this
    /// reader claims rather than leaving hits nothing can project.
    contacts: Option<Arc<dyn ContactCorpusRead>>,
    /// The posts resolver. `Option` for uniformity with `contacts` and for
    /// testability, but its precondition is only the authed connection — the
    /// launcher attaches it unconditionally, so a `None` in production is a
    /// wiring bug the `master_kinds` narrowing turns into *no post rows*
    /// rather than undropped hits.
    posts: Option<Arc<dyn PostCorpusRead>>,
    /// The files resolver. Like `posts`, its only precondition is the authed
    /// connection — a file's *name* is sealed to the set's label audience, but
    /// this actor is that audience for every set they can list, so there is no
    /// second key gate the way `contacts` has one. `None` narrows `File` out of
    /// the claimed kinds rather than leaving hits nothing can project.
    files: Option<Arc<dyn FileCorpusRead>>,
    cached: Mutex<Option<Arc<MasterIndexReader>>>,
}

impl MasterLocalSearch {
    /// The `lookup` is the same seam the mail arm uses, and that is not a
    /// shortcut: a Conversation doc's content id is its `MessageId` (
    /// `index_builder::doc_for`), and `ConversationsManager::locate_message`
    /// resolves against the `ThreadStore`, which holds threads of **every** rail
    /// — mail and fauna-MLS conversations alike. A kind whose content is not
    /// message-shaped (drafts, posts, contacts) needs its own resolution and
    /// must extend this seam rather than reuse `locate`.
    pub fn new(
        key: fauna_index::IndexMasterKey,
        publisher: Arc<IndexRailPublisher>,
        lookup: Arc<dyn MailContentLookup>,
    ) -> Self {
        Self {
            key,
            publisher,
            lookup,
            contacts: None,
            posts: None,
            files: None,
            cached: Mutex::new(None),
        }
    }

    /// Attach the posts resolver, making `Post` a kind this reader claims.
    pub fn with_posts(mut self, posts: Arc<dyn PostCorpusRead>) -> Self {
        self.posts = Some(posts);
        self
    }

    /// Attach the files resolver, making `File` a kind this reader claims.
    pub fn with_files(mut self, files: Arc<dyn FileCorpusRead>) -> Self {
        self.files = Some(files);
        self
    }

    /// Attach the contacts resolver, making `Contact` a kind this reader claims.
    ///
    /// Separate from [`Self::new`] rather than a fourth parameter because the
    /// two have different preconditions and different lifetimes: the master key
    /// exists for any logged-in actor, while the address book needs the MSEK
    /// (see the field's docs). A seat that never provisions mail simply never
    /// calls this.
    pub fn with_contacts(mut self, contacts: Arc<dyn ContactCorpusRead>) -> Self {
        self.contacts = Some(contacts);
        self
    }

    /// The reader to answer this query with — cached unless the published master
    /// manifest moved. Same staleness argument as [`MailLocalSearch::reader`].
    async fn reader(&self) -> Result<Arc<MasterIndexReader>, String> {
        let published = self
            .publisher
            .master_manifest_hash()
            .await
            .map_err(|e| e.to_string())?;

        let mut cached = self.cached.lock().await;
        if let Some(reader) = cached.as_ref()
            && reader.manifest_hash() == published.as_deref()
        {
            return Ok(Arc::clone(reader));
        }

        let reader = Arc::new(
            open_master_reader(self.key.clone(), self.publisher.as_ref())
                .await
                .map_err(|e| e.to_string())?,
        );
        *cached = Some(Arc::clone(&reader));
        Ok(reader)
    }
}

#[async_trait::async_trait]
impl LocalSearchIndex for MasterLocalSearch {
    async fn query(
        &self,
        query: &str,
        kinds: &[SearchKindClass],
        limit: usize,
    ) -> Result<Vec<LocalSearchHit>, String> {
        let selected = master_kinds(
            kinds,
            ResolverSet {
                contacts: self.contacts.is_some(),
                posts: self.posts.is_some(),
                files: self.files.is_some(),
            },
        );
        if selected.is_empty() {
            return Ok(Vec::new());
        }

        let reader = self.reader().await?;
        let owned_query = query.to_string();
        let hits =
            tokio::task::spawn_blocking(move || reader.query(&owned_query, &selected, None, limit))
                .await
                .map_err(|e| format!("local index query panicked: {e}"))?
                .map_err(|e| e.to_string())?;

        // One wire read for **every** contact hit in the result set, taken only
        // when the set actually holds one — the third ingest class's resolver
        // (`content-index.md` § Where queries run). A per-hit read would turn a
        // page of contact hits into a page of round trips; a read taken
        // unconditionally would put the address book on the critical path of
        // every mail query.
        let cards = match self.contacts.as_ref() {
            Some(contacts) if hits.iter().any(|h| h.kind == ContentKind::Contact) => {
                match contacts.read_corpus().await {
                    Ok(cards) => cards
                        .into_iter()
                        .map(|c| (c.uid_hash, c.body))
                        .collect::<std::collections::HashMap<_, _>>(),
                    // No local rows for this kind, never an error row: the read
                    // is the resolver, so a failed read is an unresolvable hit
                    // (§ Where queries run — *an offline or failed resolve
                    // yields no local rows for these kinds*).
                    Err(e) => {
                        tracing::debug!(error = %e, "index: contact resolve failed, dropping contact rows");
                        std::collections::HashMap::new()
                    }
                }
            }
            _ => std::collections::HashMap::new(),
        };

        // The per-hit half of the class resolver: one `fauna.posts.get` per
        // post hit in the set, taken only when the set holds one — bounded by
        // the page size the caller already capped `limit` at (`content-index.md`
        // § Where queries run).
        let posts = match self.posts.as_ref() {
            Some(posts) if hits.iter().any(|h| h.kind == ContentKind::Post) => {
                let ids: Vec<String> = hits
                    .iter()
                    .filter(|h| h.kind == ContentKind::Post)
                    .filter_map(|h| String::from_utf8(h.content_id.0.clone()).ok())
                    .collect();
                posts.read_posts(&ids).await
            }
            _ => std::collections::HashMap::new(),
        };

        // The whole-corpus half of the class resolver, taken once for every File
        // hit in the set — one `fauna.media.list` drain, and only when the set
        // actually holds a file hit. The wire has no by-identity file read, so
        // per-hit resolution is not even available here: the drain IS the read.
        let files = match self.files.as_ref() {
            Some(files) if hits.iter().any(|h| h.kind == ContentKind::File) => {
                match files.read_files().await {
                    Ok(rows) => rows
                        .into_iter()
                        .map(|f| (file_content_id(f.folder_id, &f.path_hash_hex), f))
                        .collect::<std::collections::HashMap<_, _>>(),
                    // Same no-local-rows posture as a failed contacts read.
                    Err(e) => {
                        tracing::debug!(error = %e, "index: file resolve failed, dropping file rows");
                        std::collections::HashMap::new()
                    }
                }
            }
            _ => std::collections::HashMap::new(),
        };

        Ok(hits
            .iter()
            .filter_map(|hit| match hit.kind {
                ContentKind::Contact => project_contact_hit(hit, &cards),
                ContentKind::Post => project_post_hit(hit, &posts),
                ContentKind::File => project_file_hit(hit, &files),
                _ => project_hit(hit, self.lookup.as_ref()),
            })
            .collect())
    }
}

/// Project one contact hit against the corpus this query read.
///
/// A hit no card answers for is **DROPPED** — the card was deleted since it was
/// indexed, and dropping is what makes deletion display-correct with no index
/// mutation (`ui/search.md` § State & data shape → *An unresolvable local hit is
/// DROPPED*; the at-rest posting residue purges at the next versioned re-index).
fn project_contact_hit(
    hit: &QueryHit,
    cards: &std::collections::HashMap<String, String>,
) -> Option<LocalSearchHit> {
    let uid_hash = String::from_utf8(hit.content_id.0.clone()).ok()?;
    let body = cards.get(&uid_hash)?.clone();
    Some(LocalSearchHit {
        content_type: content_type_for(hit.kind).to_string(),
        snippet: body,
        timestamp: hit.timestamp_ns / 1_000_000,
        score: hit.score,
        navigation: Some(SearchNav::Contact {
            uid_hash: uid_hash.clone(),
        }),
        content_id: uid_hash,
    })
}

/// Project one post hit against the reads this query took.
///
/// A hit `fauna.posts.get` no longer answers for is **DROPPED** — the post was
/// deleted since it was indexed, and dropping is what makes deletion
/// display-correct with no index mutation (`content-index.md` § Ingest
/// triggers, v1: deletion is display-healed; the at-rest posting residue
/// purges at the next versioned re-index).
fn project_post_hit(
    hit: &QueryHit,
    posts: &std::collections::HashMap<String, String>,
) -> Option<LocalSearchHit> {
    let post_id = String::from_utf8(hit.content_id.0.clone()).ok()?;
    let body = posts.get(&post_id)?.clone();
    Some(LocalSearchHit {
        content_type: content_type_for(hit.kind).to_string(),
        snippet: body,
        timestamp: hit.timestamp_ns / 1_000_000,
        score: hit.score,
        navigation: Some(SearchNav::Post {
            post_id: post_id.clone(),
        }),
        content_id: post_id,
    })
}

/// Project one file hit against the listing this query drained.
///
/// A hit the listing no longer holds is **DROPPED** — the file was deleted *or
/// renamed* since it was indexed, and a rename is structurally delete + create,
/// so both verbs heal the same way with no index mutation (`content-index.md`
/// § Ingest triggers, v1 → *The files/media arms are SCOPED*; the at-rest
/// posting residue purges at the next versioned re-index).
///
/// The snippet is the **current** path from the listing rather than the indexed
/// one, which is what keeps a stale doc honest: the doc's text is frozen at
/// first stage by the append guard, and rendering from the resolver is how the
/// row still shows what the file is called now.
fn project_file_hit(
    hit: &QueryHit,
    files: &std::collections::HashMap<String, LocatedFile>,
) -> Option<LocalSearchHit> {
    let content_id = String::from_utf8(hit.content_id.0.clone()).ok()?;
    let file = files.get(&content_id)?;
    Some(LocalSearchHit {
        content_type: content_type_for(hit.kind).to_string(),
        snippet: file.path.clone(),
        timestamp: hit.timestamp_ns / 1_000_000,
        score: hit.score,
        navigation: Some(SearchNav::File {
            folder_id: file.folder_id,
            path_hash: file.path_hash_hex.clone(),
        }),
        content_id,
    })
}

/// The caller's classes, narrowed to the ones this key class actually serves.
fn local_kinds(kinds: &[SearchKindClass]) -> Vec<ContentKind> {
    kinds.iter().filter_map(content_kind).collect()
}

/// The master reader's half of the same narrowing.
///
/// Only the kinds that actually have a producer **and** resolve through the
/// lookup appear here. A kind whose docs this device could not project is worse
/// than absent — `project_hit` would drop every hit, so the page would show
/// nothing while the rail paid to carry the segments.
/// Which of the master reader's optional resolvers are attached — the input to
/// [`master_kinds`]'s claim-nothing-you-cannot-project narrowing.
///
/// A struct rather than the positional `bool`s it replaced (there were three,
/// and the File arm made four): same-typed positional flags are the shape that
/// lets a caller transpose two of them silently, and the failure here is
/// invisible — claiming a kind whose resolver is absent drops every one of its
/// hits at projection, so the page shows nothing while the rail pays to carry
/// the segments. The same reasoning `FolderWelcomeFields` was grouped under.
#[derive(Clone, Copy, Debug)]
struct ResolverSet {
    contacts: bool,
    posts: bool,
    files: bool,
}

#[cfg(test)]
impl ResolverSet {
    /// Every optional resolver attached — for a test asserting something other
    /// than the narrowing itself.
    fn all() -> Self {
        Self {
            contacts: true,
            posts: true,
            files: true,
        }
    }

    /// No optional resolver attached — the bare master reader.
    fn none() -> Self {
        Self {
            contacts: false,
            posts: false,
            files: false,
        }
    }
}

fn master_kinds(kinds: &[SearchKindClass], resolvers: ResolverSet) -> Vec<ContentKind> {
    kinds
        .iter()
        .filter_map(|class| match class {
            SearchKindClass::Conversation => Some(ContentKind::Conversation),
            // Drafts: producer (the `DraftStore` corpus seam) and resolver
            // (`locate_draft`) both landed with the snapshot-kind arm.
            SearchKindClass::Draft => Some(ContentKind::Draft),
            // Contacts: producer (the launcher's reconcile walk) and resolver
            // (`ContactCorpusRead`) landed with the third-ingest-class arm — but
            // the resolver needs the MSEK, so it is conditional where the others
            // are not. Claiming the kind without it is the exact failure this
            // function's docs name: every hit would be dropped at projection.
            SearchKindClass::Contact if resolvers.contacts => Some(ContentKind::Contact),
            SearchKindClass::Contact => None,
            // Posts: producer (walk + create-time trickle) and resolver
            // (`PostCorpusRead`) landed with the append-shaped third-class arm.
            // The launcher attaches the resolver unconditionally (it needs only
            // the connection), so the guard here is the same
            // claim-nothing-you-cannot-project rule, not a precondition.
            SearchKindClass::Post if resolvers.posts => Some(ContentKind::Post),
            SearchKindClass::Post => None,
            // Files: producer (the `fauna.media.list` reconcile walk) and
            // resolver (`FileCorpusRead`) landed with the File arm. Same
            // claim-nothing-you-cannot-project guard as posts — the launcher
            // attaches the resolver unconditionally, so `false` here is a wiring
            // bug that costs *no file rows* rather than undropped hits.
            SearchKindClass::File if resolvers.files => Some(ContentKind::File),
            SearchKindClass::File => None,
            // Media is REFUTED for v1, not merely unbuilt: its only present text
            // is a filename, which IS the File arm's row, so a Media arm today
            // would double-index File and add nothing. It activates with the
            // classifier plans (7/8), whose tokens are its real corpus.
            SearchKindClass::Media => None,
            // The other key class's kinds, and the nest-only ones.
            SearchKindClass::Mail
            | SearchKindClass::Calendar
            | SearchKindClass::Profile
            | SearchKindClass::Other => None,
        })
        .collect()
}

/// The seam's engine-free vocabulary → the index engine's.
///
/// `None` for a class the local mail/calendar index cannot hold. Deliberately
/// exhaustive rather than a wildcard: rollout S4 adds the master-key kinds, and
/// a new class must be a compile error here rather than silently unsearchable.
fn content_kind(class: &SearchKindClass) -> Option<ContentKind> {
    match class {
        SearchKindClass::Mail => Some(ContentKind::Mail),
        SearchKindClass::Calendar => Some(ContentKind::Calendar),
        // Served by the master-key reader (rollout S4), not this one.
        SearchKindClass::Conversation
        | SearchKindClass::Post
        | SearchKindClass::Contact
        | SearchKindClass::File
        | SearchKindClass::Draft
        | SearchKindClass::Media => None,
        // Nest-only, and an unknown class cannot be asked for.
        SearchKindClass::Profile | SearchKindClass::Other => None,
    }
}

/// Project one index hit into a display-ready row, or drop it.
///
/// Pure, so the two rules that matter — render from locally-held content, and
/// never emit a row without navigation — are provable without a rail, an index
/// or a conversations session. See the module docs for why an unresolvable hit
/// is dropped rather than rendered inert.
fn project_hit(hit: &QueryHit, lookup: &dyn MailContentLookup) -> Option<LocalSearchHit> {
    // Every content id this reader can address is UTF-8 by construction — an RFC
    // `Message-ID` for the message-shaped kinds, a thread id or the reserved
    // new-thread id for a draft. Anything else is a doc this reader cannot
    // address, the same "not renderable" case as an unresolvable lookup.
    let content_id = String::from_utf8(hit.content_id.0.clone()).ok()?;

    // Drafts resolve through their own seam method and navigate to a composer;
    // see `LocatedDraft`. The remaining kinds are message-shaped and share one
    // resolution.
    let (snippet, navigation) = if hit.kind == ContentKind::Draft {
        let located = lookup.locate_draft(&content_id)?;
        (
            located.body,
            SearchNav::Draft {
                thread_id: located.thread_id,
            },
        )
    } else {
        let located = lookup.locate(&content_id)?;
        (
            located.body,
            SearchNav::Mail {
                thread_id: located.thread_id,
                message_id: content_id.clone(),
            },
        )
    };

    Some(LocalSearchHit {
        content_type: content_type_for(hit.kind).to_string(),
        snippet,
        // The index stores nanos; the snapshot row carries millis.
        timestamp: hit.timestamp_ns / 1_000_000,
        score: hit.score,
        navigation: Some(navigation),
        content_id,
    })
}

/// The raw type string a local row carries, in the **same vocabulary the nest
/// uses** — that identity is what lets a local row and its nest twin classify
/// into one kind class and dedup (`fauna_client_search::kind`).
///
/// [`ContentKind::as_str`] IS that vocabulary, so this delegates rather than
/// re-spelling it. Both matches were exhaustive, so a *new* variant was never
/// the risk here — a wrong string was, and a second hand-written copy could
/// have drifted into one without breaking any build.
fn content_type_for(kind: ContentKind) -> &'static str {
    kind.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_index::ContentId;
    use std::collections::HashMap;

    /// A lookup holding exactly the messages and drafts a device's store would.
    #[derive(Default)]
    struct FakeLookup(
        HashMap<String, LocatedMessage>,
        HashMap<String, LocatedDraft>,
    );

    impl FakeLookup {
        fn with(id: &str, thread: &str, body: &str) -> Self {
            let mut map = HashMap::new();
            map.insert(
                id.to_string(),
                LocatedMessage {
                    thread_id: thread.to_string(),
                    body: body.to_string(),
                },
            );
            Self(map, HashMap::new())
        }

        /// A store holding one draft under `id`, with `thread` as its composer
        /// target (`None` = the new-thread slot).
        fn with_draft(id: &str, thread: Option<&str>, body: &str) -> Self {
            let mut map = HashMap::new();
            map.insert(
                id.to_string(),
                LocatedDraft {
                    thread_id: thread.map(|t| t.to_string()),
                    body: body.to_string(),
                },
            );
            Self(HashMap::new(), map)
        }
    }

    impl MailContentLookup for FakeLookup {
        fn locate(&self, message_id: &str) -> Option<LocatedMessage> {
            self.0.get(message_id).cloned()
        }

        fn locate_draft(&self, content_id: &str) -> Option<LocatedDraft> {
            self.1.get(content_id).cloned()
        }
    }

    fn hit(id: &str) -> QueryHit {
        QueryHit {
            kind: ContentKind::Mail,
            content_id: ContentId(id.as_bytes().to_vec()),
            timestamp_ns: 1_700_000_000_000_000_000,
            sender_actor_id: None,
            score: 4.5,
        }
    }

    /// **The projection contract.** The snippet comes from locally-held content
    /// (the index carries none) and the row is navigable to the thread that
    /// holds the message.
    #[test]
    fn a_hit_renders_its_snippet_and_navigation_from_locally_held_content() {
        let lookup = FakeLookup::with("<a@example.com>", "thread-7", "the numbers are in");

        let row = project_hit(&hit("<a@example.com>"), &lookup).expect("the hit resolves");

        assert_eq!(row.snippet, "the numbers are in");
        assert_eq!(
            row.navigation,
            Some(SearchNav::Mail {
                thread_id: "thread-7".into(),
                message_id: "<a@example.com>".into(),
            })
        );
        assert_eq!(row.content_id, "<a@example.com>");
        assert_eq!(row.content_type, "mail");
        // nanos → millis, the snapshot row's unit.
        assert_eq!(row.timestamp, 1_700_000_000_000);
    }

    /// **The regression this slice exists for.** The Conversation kind had a
    /// producer and a place in `MASTER_KINDS_BUILT` since 2026-08-05, and no
    /// reader anywhere would answer for it: the manager's single arm held a
    /// mail/calendar reader, whose class map returns `None` for every master
    /// kind. Segments were sealed, published and replicated that no query path
    /// could read — indistinguishable from never having been built.
    ///
    /// Asserting on both readers is what makes this a pin rather than a
    /// tautology: it fails if a future edit drops Conversation from the master
    /// reader **or** quietly widens the mail reader to answer for it (which
    /// would open the wrong manifest with the wrong key).
    #[test]
    fn the_master_reader_answers_for_conversation_and_the_mail_reader_does_not() {
        assert_eq!(
            master_kinds(&[SearchKindClass::Conversation], ResolverSet::none()),
            vec![ContentKind::Conversation],
            "a Conversation query must reach the master reader, or the kind is \
             indexed and permanently unsearchable"
        );
        assert!(
            local_kinds(&[SearchKindClass::Conversation]).is_empty(),
            "the mail/calendar reader must not claim a master kind — its key \
             opens a different manifest"
        );
    }

    /// The two readers partition the classes: neither answers for the other's,
    /// and a nest-only class reaches neither.
    #[test]
    fn the_two_readers_partition_the_kind_classes() {
        for class in [SearchKindClass::Mail, SearchKindClass::Calendar] {
            assert!(
                !local_kinds(&[class]).is_empty(),
                "{class:?} is the mail reader's own class"
            );
            assert!(
                master_kinds(&[class], ResolverSet::all()).is_empty(),
                "{class:?} must not reach the master reader"
            );
        }
        for class in [SearchKindClass::Profile, SearchKindClass::Other] {
            assert!(local_kinds(&[class]).is_empty());
            assert!(
                master_kinds(&[class], ResolverSet::all()).is_empty(),
                "{class:?} is served by the nest arm, not either local reader"
            );
        }
    }

    /// A Conversation hit projects through the same resolver mail uses, and
    /// that is sound rather than a shortcut: a Conversation doc's content id is
    /// its `MessageId`, and the store the lookup reads holds threads of every
    /// rail. The row carries the nest's own type string so a local row and its
    /// nest twin dedup into one.
    #[test]
    fn a_conversation_hit_projects_through_the_shared_message_resolver() {
        let lookup = FakeLookup::with("m-42", "thread-9", "see you at the harbour");
        let mut h = hit("m-42");
        h.kind = ContentKind::Conversation;

        let row = project_hit(&h, &lookup).expect("the hit resolves");

        assert_eq!(row.content_type, "conversation");
        assert_eq!(row.snippet, "see you at the harbour");
        assert_eq!(
            row.navigation,
            Some(SearchNav::Mail {
                thread_id: "thread-9".into(),
                message_id: "m-42".into(),
            }),
            "a conversation row opens its thread and selects the message — the \
             same target shape, since the same conversations UI renders both"
        );
    }

    /// **`search.md` § State & data shape: local rows always carry `Some`.** A
    /// hit whose message this device cannot resolve has no snippet and no thread
    /// to open, so it is dropped rather than rendered as an empty inert row.
    #[test]
    fn a_hit_this_device_cannot_resolve_is_dropped_not_rendered_inert() {
        let lookup = FakeLookup::default();

        assert_eq!(project_hit(&hit("<gone@example.com>"), &lookup), None);
    }

    /// The type filter must reach the engine: a filter set to mail asks for mail
    /// only, or the page would show calendar rows the user filtered out.
    #[test]
    fn the_type_filter_narrows_the_kinds_the_engine_is_asked_for() {
        assert_eq!(
            local_kinds(&[SearchKindClass::Mail]),
            vec![ContentKind::Mail]
        );
        assert_eq!(
            local_kinds(&[SearchKindClass::Calendar]),
            vec![ContentKind::Calendar]
        );
        assert_eq!(
            local_kinds(&[SearchKindClass::Mail, SearchKindClass::Calendar]),
            vec![ContentKind::Mail, ContentKind::Calendar]
        );
    }

    /// A class this key's reader cannot serve (a master-key kind, or the
    /// nest-only profile class) never becomes a query.
    #[test]
    fn classes_this_key_cannot_serve_are_not_asked_for() {
        assert!(local_kinds(&[SearchKindClass::Profile]).is_empty());
        assert!(local_kinds(&[SearchKindClass::Post]).is_empty());
        assert!(local_kinds(&[SearchKindClass::Other]).is_empty());
        // The mail half of a mixed filter still gets through.
        assert_eq!(
            local_kinds(&[SearchKindClass::Post, SearchKindClass::Mail]),
            vec![ContentKind::Mail]
        );
    }

    /// A draft hit resolves through its own seam method and navigates to the
    /// **composer**, not to a message inside a thread.
    #[test]
    fn a_draft_hit_navigates_to_its_composer() {
        let lookup = FakeLookup::with_draft("thread-7", Some("thread-7"), "penguin notes");
        let mut h = hit("thread-7");
        h.kind = ContentKind::Draft;

        let row = project_hit(&h, &lookup).expect("a draft resolves");
        assert_eq!(
            row.navigation,
            Some(SearchNav::Draft {
                thread_id: Some("thread-7".into())
            })
        );
        assert_eq!(
            row.snippet, "penguin notes",
            "the snippet renders from the live draft, so it can never show text \
             the user has already replaced"
        );
        assert_eq!(row.content_type, "draft");
    }

    /// The new-thread compose has no thread to open — its nav target carries
    /// `None` rather than a synthetic id.
    #[test]
    fn the_new_thread_draft_navigates_without_a_thread() {
        let lookup = FakeLookup::with_draft(
            fauna_conversations::index_sink::NEW_THREAD_DRAFT_ID,
            None,
            "unsent thoughts",
        );
        let mut h = hit(fauna_conversations::index_sink::NEW_THREAD_DRAFT_ID);
        h.kind = ContentKind::Draft;

        let row = project_hit(&h, &lookup).expect("the new-thread draft resolves");
        assert_eq!(row.navigation, Some(SearchNav::Draft { thread_id: None }));
    }

    /// A draft the user discarded since the segment was sealed is **dropped**,
    /// under the same rule an unresolvable mail hit takes — never rendered as an
    /// empty row that opens an empty composer.
    #[test]
    fn a_discarded_draft_hit_is_dropped() {
        let lookup = FakeLookup::default();
        let mut h = hit("thread-gone");
        h.kind = ContentKind::Draft;

        assert!(project_hit(&h, &lookup).is_none());
    }

    /// The drafts class reaches the master reader now that both its halves
    /// exist; asking for it must not silently return nothing.
    #[test]
    fn the_master_reader_serves_the_draft_class() {
        assert_eq!(
            master_kinds(&[SearchKindClass::Draft], ResolverSet::none()),
            vec![ContentKind::Draft]
        );
        assert!(
            local_kinds(&[SearchKindClass::Draft]).is_empty(),
            "drafts are master-class; the mail/calendar reader must not claim them"
        );
    }

    // ── The contacts arm's query side ────────────────────────────────────────

    fn contact_hit(uid_hash: &str) -> QueryHit {
        let mut h = hit(uid_hash);
        h.kind = ContentKind::Contact;
        h
    }

    fn corpus(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(u, b)| (u.to_string(), b.to_string()))
            .collect()
    }

    /// A contact hit renders from the card's **current** wire text and
    /// navigates by `uid_hash` — the identity that survives an in-place edit.
    #[test]
    fn a_contact_hit_renders_from_the_live_card_and_navigates_by_uid_hash() {
        let cards = corpus(&[("aa", "Alice Flamingo\nalice@example.com")]);

        let row = project_contact_hit(&contact_hit("aa"), &cards).expect("the hit resolves");

        assert_eq!(row.snippet, "Alice Flamingo\nalice@example.com");
        assert_eq!(
            row.navigation,
            Some(SearchNav::Contact {
                uid_hash: "aa".into()
            })
        );
        assert_eq!(row.content_type, "contact");
        assert_eq!(row.content_id, "aa");
    }

    /// **Deletion is display-healed, not index-healed.** The card is gone from
    /// the live corpus, so the hit is DROPPED — no index mutation, no inert row
    /// (`ui/search.md` § State & data shape).
    #[test]
    fn a_deleted_cards_hit_is_dropped() {
        assert!(project_contact_hit(&contact_hit("gone"), &corpus(&[])).is_none());
    }

    /// An **offline or failed** corpus read reaches the projection as an empty
    /// corpus, and must yield no contact rows rather than an error row — the
    /// no-reader posture, stated for this class in § Where queries run. Same
    /// code path as a deletion by construction, which is why the resolver can
    /// swallow the read error.
    #[test]
    fn a_failed_corpus_read_yields_no_contact_rows() {
        let empty = corpus(&[]);
        assert!(
            [contact_hit("aa"), contact_hit("bb")]
                .iter()
                .all(|h| project_contact_hit(h, &empty).is_none())
        );
    }

    /// **The reader claims `Contact` only when it can resolve one**, because a
    /// kind whose hits all drop at projection is worse than an absent kind: the
    /// page shows nothing while the rail pays to carry the segments. Contacts
    /// are the first kind where this is conditional at runtime rather than
    /// settled at compile time — the resolver needs the MSEK, which the rest of
    /// the master class does not.
    #[test]
    fn the_master_reader_claims_contacts_only_with_a_resolver_attached() {
        assert_eq!(
            master_kinds(
                &[SearchKindClass::Contact],
                ResolverSet {
                    contacts: true,
                    ..ResolverSet::none()
                }
            ),
            vec![ContentKind::Contact],
            "with the address book readable, a contact query must reach the reader"
        );
        assert!(
            master_kinds(&[SearchKindClass::Contact], ResolverSet::none()).is_empty(),
            "without the MSEK there is no resolver, so claiming the kind would \
             produce hits nothing can project"
        );
        assert!(
            local_kinds(&[SearchKindClass::Contact]).is_empty(),
            "contacts are master-class; the mail/calendar reader must not claim them"
        );
    }

    // ── the posts projection ───────────────────────────────────────────────

    fn post_hit(post_id: &str) -> QueryHit {
        let mut h = hit(post_id);
        h.kind = ContentKind::Post;
        h
    }

    fn posts_read(entries: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        entries
            .iter()
            .map(|(id, body)| (id.to_string(), body.to_string()))
            .collect()
    }

    /// A post hit renders its **current** text and navigates by its real id —
    /// `SearchNav::Post` carries the same hex id backend 1's rows mint, which
    /// is what lets the `(kind class, id)` dedup collapse the twins with local
    /// winning (`fauna_client_search::manager`).
    #[test]
    fn a_post_hit_projects_its_current_text_and_navigates_by_id() {
        let read = posts_read(&[("aa11", "the albatross crossed the meridian")]);
        let row = project_post_hit(&post_hit("aa11"), &read).expect("the hit resolves");

        assert_eq!(row.content_type, "post");
        assert_eq!(row.snippet, "the albatross crossed the meridian");
        assert_eq!(
            row.navigation,
            Some(SearchNav::Post {
                post_id: "aa11".into()
            })
        );
        assert_eq!(row.content_id, "aa11");
    }

    /// **Deletion is display-healed**: a hit whose `fauna.posts.get` found
    /// nothing is DROPPED — an append index cannot remove the doc, so the drop
    /// at projection is the deletion the user observes (`content-index.md`
    /// § Ingest triggers, v1 — the posts ruling). A failed or offline read
    /// lands in the same map-miss by construction.
    #[test]
    fn a_deleted_posts_hit_is_dropped_at_projection() {
        let read = posts_read(&[("aa11", "still here")]);
        assert!(project_post_hit(&post_hit("gone"), &read).is_none());
        assert!(project_post_hit(&post_hit("aa11"), &read).is_some());
    }

    /// The reader claims `Post` only with the resolver attached — same
    /// claim-nothing-you-cannot-project rule as contacts. In production the
    /// launcher attaches it unconditionally, so the `false` arm is the wiring-
    /// bug netting, not a runtime state.
    #[test]
    fn the_master_reader_claims_posts_only_with_a_resolver_attached() {
        assert_eq!(
            master_kinds(
                &[SearchKindClass::Post],
                ResolverSet {
                    posts: true,
                    ..ResolverSet::none()
                }
            ),
            vec![ContentKind::Post],
        );
        assert!(master_kinds(&[SearchKindClass::Post], ResolverSet::none()).is_empty());
        assert!(
            local_kinds(&[SearchKindClass::Post]).is_empty(),
            "posts are master-class; the mail/calendar reader must not claim them"
        );
    }

    // ── the file projection ────────────────────────────────────────────────

    fn file_hit(folder_id: i64, path_hash: &str) -> QueryHit {
        let mut h = hit(&file_content_id(folder_id, path_hash));
        h.kind = ContentKind::File;
        h
    }

    fn files_read(rows: &[(i64, &str, &str)]) -> std::collections::HashMap<String, LocatedFile> {
        rows.iter()
            .map(|(set, hash, path)| {
                (
                    file_content_id(*set, hash),
                    LocatedFile {
                        folder_id: *set,
                        path_hash_hex: hash.to_string(),
                        path: path.to_string(),
                        source_online: true,
                    },
                )
            })
            .collect()
    }

    /// A file hit renders its **current** path and navigates by the durable
    /// identity pair — the set's stable row id plus `path_hash`, never the
    /// renameable set name (`content-index.md` § Ingest triggers, v1 → *The
    /// files/media arms are SCOPED*).
    #[test]
    fn a_file_hit_projects_its_current_path_and_navigates_by_the_identity_pair() {
        let read = files_read(&[(42, "aa11", "holidays/albatross.jpg")]);
        let row = project_file_hit(&file_hit(42, "aa11"), &read).expect("the hit resolves");

        assert_eq!(row.content_type, "file");
        assert_eq!(
            row.snippet, "holidays/albatross.jpg",
            "the snippet is the resolver's live name, not the one frozen into the \
             doc at first stage — which is what keeps a renamed-then-restored or \
             a stale doc honest"
        );
        assert_eq!(
            row.navigation,
            Some(SearchNav::File {
                folder_id: 42,
                path_hash: "aa11".into()
            })
        );
        assert_eq!(row.content_id, "42:aa11");
    }

    /// **Deletion _and rename_ are display-healed.** An append index cannot
    /// remove the doc, so the drop at projection is the disappearance the user
    /// observes — and because a rename is structurally delete + create, the old
    /// name's doc drops by exactly the same route while the new name's doc is
    /// staged by the next walk. A failed or offline drain is the same map-miss.
    #[test]
    fn a_deleted_or_renamed_file_hit_is_dropped_at_projection() {
        let read = files_read(&[(42, "aa11", "still/here.txt")]);
        assert!(
            project_file_hit(&file_hit(42, "bb22"), &read).is_none(),
            "the old name's path_hash is absent from the listing after a rename"
        );
        assert!(project_file_hit(&file_hit(42, "aa11"), &read).is_some());
    }

    /// The identity is the **pair**: a hit in one set must not resolve against a
    /// same-named file in another. Keying the resolver map on `path_hash` alone
    /// would silently render the wrong set's file — a cross-set leak into the
    /// row, not merely a wrong name.
    #[test]
    fn a_file_hit_does_not_resolve_against_the_same_path_in_another_set() {
        let read = files_read(&[(1, "aa11", "notes.txt")]);
        assert!(project_file_hit(&file_hit(2, "aa11"), &read).is_none());
    }

    /// The reader claims `File` only with the resolver attached — the same
    /// claim-nothing-you-cannot-project rule as contacts and posts.
    #[test]
    fn the_master_reader_claims_files_only_with_a_resolver_attached() {
        assert_eq!(
            master_kinds(
                &[SearchKindClass::File],
                ResolverSet {
                    files: true,
                    ..ResolverSet::none()
                }
            ),
            vec![ContentKind::File],
        );
        assert!(master_kinds(&[SearchKindClass::File], ResolverSet::none()).is_empty());
        assert!(
            local_kinds(&[SearchKindClass::File]).is_empty(),
            "files are master-class; the mail/calendar reader must not claim them"
        );
    }

    /// Media stays **refuted** for v1 — out of the claimed kinds no matter which
    /// resolvers are attached. Its only present text is a filename, which IS the
    /// File arm's row, so claiming it would double-index File and add nothing.
    #[test]
    fn media_is_never_claimed_even_with_every_resolver_attached() {
        assert!(master_kinds(&[SearchKindClass::Media], ResolverSet::all()).is_empty());
        assert!(local_kinds(&[SearchKindClass::Media]).is_empty());
    }
}
