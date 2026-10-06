//! The manager's **attachment store** — a bounded, content-addressed cache of
//! attachment plaintext, never the home of the bytes
//! (`docs/goal/ui/conversations.md` § Attachments → *Retention*).
//!
//! Every app renders an attachment off one handle, its `blob_hash`, and
//! resolves the bytes through `ConversationsManager::attachment_bytes`. The
//! per-record reader bound (`fauna_core::attachment_limits`) caps what one
//! message costs a member — at most 64 entries of at most 10 MiB — but not
//! what the app *holds*: before this store had a budget it kept every opened
//! attachment for the manager's lifetime, so any co-member could grow every
//! other member's process by 640 MiB per message until the OS killed it. The
//! bytes rest elsewhere — on the room's home nest (FaunaMls) or in the INBOX
//! record (SMTP) — so the device may hold a *cache* of them, and a cache has a
//! budget.
//!
//! Three rules, all of them here:
//!
//! 1. **The store holds at most [`ATTACHMENT_STORE_BUDGET_BYTES`]**, evicting
//!    the least recently *read* entry first — a render is a read, so what is on
//!    screen stays resident. A hard-coded constant, never a knob
//!    (`principles.md` § One configuration surface): no user or admin would
//!    ever want to choose it.
//! 2. **A staged outgoing draft is pinned.** `send` re-resolves a draft's bytes
//!    from this store by hash, so evicting them would lose the user's own
//!    attachment — the send refuses rather than going without it, but the user
//!    would have to attach it again. The manager hands [`AttachmentStore::insert`]
//!    the set of hashes any compose draft references and eviction skips them;
//!    the store may then exceed the budget by the user's own picks plus the
//!    newest entry, which is bounded by what the user chose, never by a remote
//!    member. The set is a closure the store calls at eviction time — after the
//!    new entry is resident, with the caller holding the store's lock across the
//!    whole insert — so it can never be a snapshot taken before a draft that is
//!    being staged was registered.
//! 3. **An evicted entry is not lost when it can be fetched again.** The
//!    receive paths — and the FaunaMls send, for the sender's own attachments,
//!    which it never walks back — remember, per handle, where the bytes rest
//!    ([`AttachmentCoordinates`] — one arm per rail); a render-time miss on a
//!    handle with coordinates marks it *wanted*, and the next receive cycle
//!    fetches it again, each rail taking only its own wants: the FaunaMls
//!    sweep re-GETs and re-opens the sealed blob
//!    (`backends::fauna_mls::refill_evicted_attachments`), the mail sweep
//!    re-reads the one mail record and re-parses its MIME
//!    (`backends::smtp::refill_evicted_mail_attachments`). A handle with no
//!    coordinates — or whose bytes turned out to be gone for good — stays
//!    declared: its filename and size render, the bytes do not.
//!
//! Content-addressing is enforced at the manager's door, not here: the store
//! trusts that `hash` is the BLAKE3 of `bytes` because
//! `ConversationsManager::cache_attachment_bytes_checked` verified it.

use crate::backend::MailFeed;
use fauna_mls::types::ChannelId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// The most attachment plaintext the store holds — 128 MiB, the same on all
/// 7 apps. Sized for the tightest platform: a dozen attachments at the 10 MiB
/// door ceiling, or some forty phone photographs, stay resident, which covers
/// what a screen shows and a short scroll back; anything older is re-fetched
/// from where it rests. A constant, not a knob — it follows from device memory
/// and the wire's per-attachment ceiling, not from any deployment choice.
pub const ATTACHMENT_STORE_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// How many handles the store remembers fetch coordinates for. A coordinate
/// record is ~150 bytes, so this is a few megabytes at most; the oldest
/// remembered handle is forgotten first, and a handle without coordinates is
/// simply declared when evicted.
pub const MAX_REMEMBERED_ATTACHMENT_COORDINATES: usize = 16 * 1024;

/// Which key opens a sealed attachment blob when it is fetched again — the one
/// point where the two room classes' receive paths differ
/// (`community-rooms.md` § The three classes → *Attachments — the second
/// content kind*).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentOpeningKey {
    /// End-to-end: the channel's MLS blob key at the attachment's `epoch`.
    MlsEpoch { epoch: u64 },
    /// Community: the room's attachment content kind off the generation the
    /// naming message was sealed under.
    RoomGeneration {
        #[serde(with = "serde_bytes")]
        generation: [u8; 32],
    },
    /// A key a newer build records and this one does not name, carried whole
    /// so a `history/<ch>` slice this build merges and re-uploads keeps it
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*). This build cannot open the blob it names: the attachment is
    /// never refilled here and renders declared, and its coordinates are kept,
    /// never forgotten, for a build that can. No build writes one.
    #[serde(untagged)]
    Unknown(fauna_core::carried::CarriedValue),
}

/// Where a FaunaMls attachment's sealed blob rests on its channel's home nest
/// and which key opens it — everything the receive loop knew when it first
/// cached the plaintext (or the send, when it sealed and uploaded it), except
/// the channel, which whoever holds this names.
///
/// Also the at-rest shape a `history/<ch>` slice carries per attachment
/// ([`crate::store::history::ChannelHistorySlice::attachment_coordinates`]),
/// where the channel is the slice's own — so a slice cannot point a restoring
/// device at another channel's blobs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedBlobCoordinates {
    /// The sealed blob's content address on the channel's home nest,
    /// lowercase hex.
    pub sealed_cid_hex: String,
    /// The author's declared plaintext size — re-checked on refill exactly as
    /// on first receive (`conversation-rooms.md` § The home nest → *The reader
    /// bounds what it fetches*).
    pub size_bytes: u64,
    pub key: AttachmentOpeningKey,
}

/// Where an SMTP attachment's bytes rest: inside the MIME of one mail record.
/// The mailbox rides beside the UID because UIDs are monotonic *per mailbox* —
/// INBOX and Sent both have a record 1, so a UID alone names no record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MailRecordCoordinates {
    pub mailbox: MailFeed,
    pub uid: u32,
}

/// Where an attachment's bytes rest and how to get them back, one arm per
/// rail whose bytes can be fetched again. Manager-internal: nothing here
/// crosses the FFI, so the `AttachmentSnapshot` every app renders is
/// unchanged (`conversations.md` § Attachments → *Retention*, weighed and
/// rejected: coordinates on the FFI record).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentCoordinates {
    /// A sealed blob on the home nest of `channel` — the channel the naming
    /// message arrived on or was sent to (`FaunaMlsBackend::channel_home_url`
    /// picks the nest).
    FaunaMls {
        channel: ChannelId,
        blob: SealedBlobCoordinates,
    },
    /// A MIME part of one mail record.
    Smtp(MailRecordCoordinates),
}

/// What a render-time read found.
#[derive(Debug, PartialEq, Eq)]
pub enum AttachmentRead {
    /// Resident; the read touched it.
    Hit(Vec<u8>),
    /// Not resident. `wanted_now` is `true` when this read is the one that
    /// marked the handle for refill — the caller pokes the receive loop once,
    /// not on every repaint of a missing attachment.
    Miss { wanted_now: bool },
}

struct Entry {
    bytes: Vec<u8>,
    /// The store clock's value at the last read or insert — the eviction
    /// order's key.
    last_read: u64,
}

/// The store itself. Single-threaded by construction; the manager wraps it in
/// its own lock.
pub struct AttachmentStore {
    budget: usize,
    resident_bytes: usize,
    entries: HashMap<String, Entry>,
    /// `last_read` → handle, so the least recently read entry is the first key.
    by_last_read: BTreeMap<u64, String>,
    /// Monotonic; every read and insert takes the next value, so stamps are
    /// unique and the map above is total.
    clock: u64,
    /// The one coordinate map, every rail's arm in it.
    coordinates: HashMap<String, AttachmentCoordinates>,
    /// Insertion order of `coordinates`, for the FIFO cap.
    coordinate_order: VecDeque<String>,
    /// Handles a render missed that have coordinates — what the next receive
    /// cycle refills. Insertion-ordered, deduplicated.
    wanted: Vec<String>,
    wanted_set: HashSet<String>,
}

impl Default for AttachmentStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AttachmentStore {
    pub fn new() -> Self {
        Self::with_budget(ATTACHMENT_STORE_BUDGET_BYTES)
    }

    pub fn with_budget(budget: usize) -> Self {
        Self {
            budget,
            resident_bytes: 0,
            entries: HashMap::new(),
            by_last_read: BTreeMap::new(),
            clock: 0,
            coordinates: HashMap::new(),
            coordinate_order: VecDeque::new(),
            wanted: Vec::new(),
            wanted_set: HashSet::new(),
        }
    }

    /// Change the budget. Existing residents are not evicted until the next
    /// insert (eviction needs the pinned set, which only the manager knows).
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// The plaintext bytes currently held.
    pub fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.entries.contains_key(hash)
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// A render-time read: touches a hit; on a miss with known coordinates,
    /// marks the handle wanted so the next receive cycle refills it.
    pub fn read(&mut self, hash: &str) -> AttachmentRead {
        let stamp = self.tick();
        if let Some(entry) = self.entries.get_mut(hash) {
            self.by_last_read.remove(&entry.last_read);
            entry.last_read = stamp;
            let bytes = entry.bytes.clone();
            self.by_last_read.insert(stamp, hash.to_string());
            return AttachmentRead::Hit(bytes);
        }
        let wanted_now =
            self.coordinates.contains_key(hash) && self.wanted_set.insert(hash.to_string());
        if wanted_now {
            self.wanted.push(hash.to_string());
        }
        AttachmentRead::Miss { wanted_now }
    }

    /// A read that neither touches nor wants — the send-time resolve, which
    /// is not a render and must not schedule a refill.
    pub fn peek(&self, hash: &str) -> Option<&[u8]> {
        self.entries.get(hash).map(|e| e.bytes.as_slice())
    }

    /// Insert `bytes` under `hash` (a verified pair — see the module doc), then
    /// evict least-recently-read entries that are neither `hash` nor pinned
    /// until the store is within budget or no such entry remains. Re-inserting
    /// a resident handle only touches it.
    ///
    /// `pinned` is called at most once, and only when eviction runs — after
    /// `hash` is resident, so the pin set it returns is read no earlier than
    /// the insert it guards (rule 2).
    pub fn insert(
        &mut self,
        hash: String,
        bytes: Vec<u8>,
        pinned: impl FnOnce() -> HashSet<String>,
    ) {
        let stamp = self.tick();
        if let Some(entry) = self.entries.get_mut(&hash) {
            self.by_last_read.remove(&entry.last_read);
            entry.last_read = stamp;
            self.by_last_read.insert(stamp, hash);
            return;
        }
        self.resident_bytes += bytes.len();
        self.by_last_read.insert(stamp, hash.clone());
        self.entries.insert(
            hash.clone(),
            Entry {
                bytes,
                last_read: stamp,
            },
        );
        self.wanted_set.remove(&hash);
        self.wanted.retain(|h| h != &hash);
        if self.resident_bytes <= self.budget {
            return;
        }
        let pinned = pinned();
        while self.resident_bytes > self.budget {
            let victim = self
                .by_last_read
                .values()
                .find(|h| **h != hash && !pinned.contains(*h))
                .cloned();
            match victim {
                Some(h) => self.remove(&h),
                None => break,
            }
        }
    }

    fn remove(&mut self, hash: &str) {
        if let Some(entry) = self.entries.remove(hash) {
            self.resident_bytes -= entry.bytes.len();
            self.by_last_read.remove(&entry.last_read);
        }
    }

    /// Test-only: drop `hash`'s bytes and keep its coordinates — exactly what
    /// the budget's eviction does to one entry. Returns whether the bytes were
    /// resident.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn evict(&mut self, hash: &str) -> bool {
        let resident = self.entries.contains_key(hash);
        self.remove(hash);
        resident
    }

    /// Remember where `hash`'s bytes rest, so a later miss can fetch them
    /// again. The oldest remembered handle is forgotten past the cap.
    pub fn remember(&mut self, hash: String, coordinates: AttachmentCoordinates) {
        if self.coordinates.insert(hash.clone(), coordinates).is_none() {
            self.coordinate_order.push_back(hash);
            while self.coordinate_order.len() > MAX_REMEMBERED_ATTACHMENT_COORDINATES {
                if let Some(oldest) = self.coordinate_order.pop_front() {
                    self.coordinates.remove(&oldest);
                    self.unwant(&oldest);
                }
            }
        }
    }

    /// [`Self::remember`], but only when `hash` has no coordinates yet — what
    /// a restored `history/<ch>` slice fills: what this device learned itself
    /// this launch (its receive loop, or its send) outranks a copy another
    /// device wrote earlier.
    pub fn remember_if_absent(&mut self, hash: String, coordinates: AttachmentCoordinates) {
        if !self.coordinates.contains_key(&hash) {
            self.remember(hash, coordinates);
        }
    }

    /// Stop remembering `hash` — a refill found the bytes gone for good
    /// (absent on the nest or from the mailbox, or opening/verification
    /// failed), so a later miss stays declared instead of asking every cycle.
    pub fn forget(&mut self, hash: &str) {
        if self.coordinates.remove(hash).is_some() {
            self.coordinate_order.retain(|h| h != hash);
        }
        self.unwant(hash);
    }

    fn unwant(&mut self, hash: &str) {
        if self.wanted_set.remove(hash) {
            self.wanted.retain(|h| h != hash);
        }
    }

    pub fn coordinates(&self, hash: &str) -> Option<&AttachmentCoordinates> {
        self.coordinates.get(hash)
    }

    /// Drain the wanted handles whose bytes rest in a sealed blob on a
    /// channel's home nest, in the order they were first missed — what the
    /// FaunaMls sweep refills. Every other rail's wants stay wanted for that
    /// rail's own sweep. A handle still missing after the caller's refill is
    /// marked wanted again by the next render that misses it.
    pub fn take_wanted_sealed_blobs(&mut self) -> Vec<(String, ChannelId, SealedBlobCoordinates)> {
        self.take_wanted_where(|c| match c {
            AttachmentCoordinates::FaunaMls { channel, blob } => Some((*channel, blob.clone())),
            AttachmentCoordinates::Smtp(_) => None,
        })
        .into_iter()
        .map(|(hash, (channel, blob))| (hash, channel, blob))
        .collect()
    }

    /// Drain the wanted handles whose bytes rest in a mail record — what the
    /// mail sweep refills — leaving every other rail's wants in place. Same
    /// ordering and re-want contract as [`Self::take_wanted_sealed_blobs`].
    pub fn take_wanted_mail_records(&mut self) -> Vec<(String, MailRecordCoordinates)> {
        self.take_wanted_where(|c| match c {
            AttachmentCoordinates::Smtp(record) => Some(*record),
            AttachmentCoordinates::FaunaMls { .. } => None,
        })
    }

    /// The one drain both rails' takes share: split the wanted list by what
    /// `pick` accepts, returning the accepted handles and keeping the rest
    /// wanted in their original order. Split rather than drained whole
    /// because the two sweeps run at different moments — a mail-only push
    /// wakes only the mail sweep — so a take that swallowed the other rail's
    /// wants would drop them until their next render.
    fn take_wanted_where<T>(
        &mut self,
        pick: impl Fn(&AttachmentCoordinates) -> Option<T>,
    ) -> Vec<(String, T)> {
        let mut taken = Vec::new();
        let mut kept = Vec::new();
        for hash in std::mem::take(&mut self.wanted) {
            match self.coordinates.get(&hash).map(&pick) {
                Some(Some(t)) => {
                    self.wanted_set.remove(&hash);
                    taken.push((hash, t));
                }
                Some(None) => kept.push(hash),
                // `forget` and the cap both unwant, so a wanted handle always
                // has coordinates; dropped rather than kept if that ever slips.
                None => {
                    self.wanted_set.remove(&hash);
                }
            }
        }
        self.wanted = kept;
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coords(n: u8) -> AttachmentCoordinates {
        AttachmentCoordinates::FaunaMls {
            channel: ChannelId([n; 32]),
            blob: SealedBlobCoordinates {
                sealed_cid_hex: format!("{n:02x}"),
                size_bytes: 1,
                key: AttachmentOpeningKey::MlsEpoch { epoch: 1 },
            },
        }
    }

    fn mail(mailbox: MailFeed, uid: u32) -> AttachmentCoordinates {
        AttachmentCoordinates::Smtp(MailRecordCoordinates { mailbox, uid })
    }

    #[test]
    fn a_miss_with_coordinates_is_wanted_once_until_taken() {
        let mut s = AttachmentStore::with_budget(10);
        assert_eq!(s.read("h"), AttachmentRead::Miss { wanted_now: false });
        s.remember("h".into(), coords(1));
        assert_eq!(s.read("h"), AttachmentRead::Miss { wanted_now: true });
        assert_eq!(s.read("h"), AttachmentRead::Miss { wanted_now: false });
        let wanted = s.take_wanted_sealed_blobs();
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].0, "h");
        assert!(s.take_wanted_sealed_blobs().is_empty());
        assert_eq!(s.read("h"), AttachmentRead::Miss { wanted_now: true });
    }

    #[test]
    fn a_forgotten_handle_is_declared_not_wanted() {
        let mut s = AttachmentStore::with_budget(10);
        s.remember("h".into(), coords(1));
        assert_eq!(s.read("h"), AttachmentRead::Miss { wanted_now: true });
        s.forget("h");
        assert!(s.take_wanted_sealed_blobs().is_empty());
        assert_eq!(s.read("h"), AttachmentRead::Miss { wanted_now: false });
    }

    #[test]
    fn an_insert_clears_a_pending_want_for_the_same_handle() {
        let mut s = AttachmentStore::with_budget(10);
        s.remember("h".into(), coords(1));
        s.read("h");
        s.insert("h".into(), vec![1, 2, 3], HashSet::new);
        assert!(s.take_wanted_sealed_blobs().is_empty());
        assert_eq!(s.resident_bytes(), 3);
    }

    /// Each rail's sweep takes only its own wants: the FaunaMls sweep runs on
    /// a conversation push the mail sweep never sees, and the reverse, so a
    /// take that drained every want would drop the other rail's until its next
    /// render.
    #[test]
    fn each_rails_take_leaves_the_other_rails_wants_wanted() {
        let mut s = AttachmentStore::with_budget(10);
        s.remember("m1".into(), mail(MailFeed::Inbox, 7));
        s.remember("c".into(), coords(1));
        s.remember("m2".into(), mail(MailFeed::Sent, 7));
        for h in ["m1", "c", "m2"] {
            assert_eq!(s.read(h), AttachmentRead::Miss { wanted_now: true });
        }

        let blobs = s.take_wanted_sealed_blobs();
        assert_eq!(
            blobs.iter().map(|(h, ..)| h.as_str()).collect::<Vec<_>>(),
            vec!["c"]
        );
        // Still wanted, so a repaint does not re-poke.
        assert_eq!(s.read("m1"), AttachmentRead::Miss { wanted_now: false });

        let records = s.take_wanted_mail_records();
        assert_eq!(
            records,
            vec![
                (
                    "m1".to_string(),
                    MailRecordCoordinates {
                        mailbox: MailFeed::Inbox,
                        uid: 7
                    }
                ),
                (
                    "m2".to_string(),
                    MailRecordCoordinates {
                        mailbox: MailFeed::Sent,
                        uid: 7
                    }
                ),
            ],
            "both mailboxes' wants, in first-miss order, the mailbox beside each uid"
        );
        assert!(s.take_wanted_mail_records().is_empty());
        assert!(s.take_wanted_sealed_blobs().is_empty());
    }

    #[test]
    fn remember_if_absent_never_overwrites_what_the_receive_loop_learned() {
        let mut s = AttachmentStore::with_budget(10);
        s.remember("h".into(), coords(1));
        s.remember_if_absent("h".into(), coords(2));
        assert_eq!(s.coordinates("h"), Some(&coords(1)));
        s.remember_if_absent("g".into(), coords(2));
        assert_eq!(s.coordinates("g"), Some(&coords(2)));
    }

    #[test]
    fn the_coordinate_cap_forgets_the_oldest_first() {
        let mut s = AttachmentStore::with_budget(10);
        for i in 0..=MAX_REMEMBERED_ATTACHMENT_COORDINATES {
            s.remember(format!("h{i}"), coords(1));
        }
        assert!(s.coordinates("h0").is_none());
        assert!(s.coordinates("h1").is_some());
        assert_eq!(
            s.coordinate_order.len(),
            MAX_REMEMBERED_ATTACHMENT_COORDINATES
        );
    }

    #[test]
    fn eviction_never_removes_the_entry_just_inserted_or_a_pinned_one() {
        let mut s = AttachmentStore::with_budget(5);
        let pinned: HashSet<String> = ["p".to_string()].into_iter().collect();
        s.insert("p".into(), vec![0; 4], || pinned.clone());
        s.insert("big".into(), vec![0; 4], || pinned.clone());
        // Over budget, but nothing evictable: the pinned entry and the newest
        // both stay.
        assert!(s.contains("p") && s.contains("big"));
        assert_eq!(s.resident_bytes(), 8);
        s.insert("next".into(), vec![0; 1], || pinned.clone());
        assert!(!s.contains("big"), "the unpinned older entry goes first");
        assert!(s.contains("p") && s.contains("next"));
    }

    /// Rule 2's pin set is read when eviction runs, not before the insert, so
    /// the manager's closure sees every draft registered before the bytes went
    /// in — and a store within budget never builds the set at all.
    #[test]
    fn the_pin_set_is_read_only_when_eviction_runs() {
        fn counting(reads: &std::cell::Cell<u32>) -> impl FnOnce() -> HashSet<String> + '_ {
            move || {
                reads.set(reads.get() + 1);
                HashSet::new()
            }
        }
        let reads = std::cell::Cell::new(0);
        let mut s = AttachmentStore::with_budget(5);
        s.insert("a".into(), vec![0; 4], counting(&reads));
        assert_eq!(
            reads.get(),
            0,
            "within budget: nothing to evict, no pin read"
        );
        s.insert("a".into(), vec![0; 4], counting(&reads));
        assert_eq!(reads.get(), 0, "a re-insert only touches the entry");
        s.insert("b".into(), vec![0; 4], counting(&reads));
        assert_eq!(reads.get(), 1, "over budget: read once, for this eviction");
        assert!(!s.contains("a") && s.contains("b"));
    }
}
