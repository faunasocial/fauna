//! Phase two — linking and merging (`docs/goal/behavior/archive-import.md`
//! § Linking and merging (phase two) and its twelve rulings).
//!
//! Two contacts who both imported the same platform's archive find each
//! other ([`ArchiveSignal::Announce`] → a question on the contact's row →
//! offer → confirm), exchange origin indices of their imported posts, and
//! each claims its OWN archived comments and reactions on the other's posts
//! as backdated reply and reaction posts it signs itself, resolved by a
//! ladder that never guesses (rulings 5–7).
//!
//! [`ArchiveLinks`] is the whole of it, over the crate's two seams: the
//! nest-ward [`ArchiveNest`] (folders, the roster, posts, the followers
//! tier) and the channel-ward [`ArchiveSignalPoster`] (the session's
//! `archive_channel_for` + `send_archive_signal`, which `fauna-conversations`
//! owns). With the `conversations-sink` feature it is also the session's
//! `ArchiveSignalSink` (ruling 2) — see `sink_glue`. The state lives in the
//! archive folders' `state/links.cbor` ([`LINKS_PATH`]), read as the union
//! over every folder of the platform (ruling 3), so every device of the
//! account sees what the one that decrypted a signal captured.

mod types;

#[cfg(feature = "conversations-sink")]
mod sink_glue;
#[cfg(feature = "conversations-sink")]
pub use sink_glue::{BackendPoster, SessionPoster};

#[cfg(test)]
mod tests;

/// The platform every link call names — re-exported so an app parses an
/// `ArchiveLinkView::platform` token (`Platform::parse_token`) without
/// depending on the parser crate itself.
pub use fauna_archive::Platform;
pub use types::*;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use fauna_archive::model::{ArchiveComment, ArchiveReaction, ArchiveTargetRef, ReactionKind};
use fauna_archive::{Category, Entity, ExternalActorRef, ExternalId};
use fauna_client_core::post::{
    PostAuthoring, ReferenceKind, ReferencedAudience, SealedAudience, build_reaction_post_at,
    build_referencing_post_at, build_sealed_referencing_post_at, decode_post_fetched_as,
    mint_seal_id,
};
use fauna_core::MaybeSendSync;
use fauna_core::data::{Reference, Timestamp};
use fauna_core::identity::{ActorId, ActorKeypair};

use crate::audience::{PostAudience, map_audience};
use crate::nest::{ArchiveFolderRef, ArchiveNest, ArchiveNestError};
use crate::state::{ImportState, ImportTarget, STATE_PATH, model_path};

/// The fingerprint's domain string (§ Linking and merging, *Discovery*).
pub const FINGERPRINT_DOMAIN: &str = "fauna-archive-link/v1";

/// The ledger reason for a record no ladder rung placed (ruling 5).
pub const UNMATCHED_NO_RUNG: &str = "no post in the contact's index matches its target";
/// The ledger reason for a link several indexed posts shared (ruling 5,
/// rung (iii)).
pub const UNMATCHED_AMBIGUOUS_LINK: &str = "several posts share the link it names";
/// The ledger reason for a reaction kind with no fixed emoji (ruling 7).
pub const UNMATCHED_REACTION_KIND: &str = "a reaction kind with no mapping";

/// BLAKE3 over the domain string, the platform token and `key` — each
/// followed by a zero byte, so no two inputs share an encoding.
fn fingerprint_key(platform: &Platform, key: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(FINGERPRINT_DOMAIN.as_bytes());
    hasher.update(&[0]);
    hasher.update(platform.token().as_bytes());
    hasher.update(&[0]);
    hasher.update(key.as_bytes());
    *hasher.finalize().as_bytes()
}

/// The `Announce` fingerprint of a platform identity: keyed on the platform
/// user ID when the export has one, else on the normalized display name.
pub fn fingerprint(platform: &Platform, actor: &ExternalActorRef) -> [u8; 32] {
    fingerprint_key(platform, actor.id.as_deref().unwrap_or(&actor.name_key))
}

/// The name-keyed twin an `Announce` carries beside an id-keyed
/// [`fingerprint`]: `None` when the fingerprint is already name-keyed. A
/// Facebook export carries its owner's `username` but names every friend by
/// display name alone, so without it an id-bearing announcer could never be
/// found in a friends list (`archive-import.md` § Linking and merging,
/// *Discovery*).
pub fn name_fingerprint(platform: &Platform, actor: &ExternalActorRef) -> Option<[u8; 32]> {
    actor
        .id
        .as_ref()
        .map(|_| fingerprint_key(platform, &actor.name_key))
}

/// Whether `friend` could be the person an `Announce` names: a friend ref
/// carrying an ID is tried both by it and by its name, since the announcer's
/// export may carry only one of the two.
fn fingerprint_matches(platform: &Platform, friend: &ExternalActorRef, fp: &[u8; 32]) -> bool {
    friend
        .id
        .as_deref()
        .is_some_and(|id| &fingerprint_key(platform, id) == fp)
        || &fingerprint_key(platform, &friend.name_key) == fp
}

/// [`fingerprint_matches`] over both of an `Announce`'s fingerprints.
fn announce_matches(
    platform: &Platform,
    friend: &ExternalActorRef,
    fp: &[u8; 32],
    name_fp: Option<&[u8; 32]>,
) -> bool {
    fingerprint_matches(platform, friend, fp)
        || name_fp.is_some_and(|n| fingerprint_matches(platform, friend, n))
}

/// Two platform refs name one person: by ID when both carry one, else by
/// the normalized name.
pub fn same_person(a: &ExternalActorRef, b: &ExternalActorRef) -> bool {
    match (&a.id, &b.id) {
        (Some(x), Some(y)) => x == y,
        _ => a.name_key == b.name_key,
    }
}

/// What a claimed reaction becomes (ruling 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappedReaction {
    /// A plain like: `Reference::Upvote`.
    Upvote,
    /// `Reference::React { emoji }`.
    React(&'static str),
}

/// Ruling 7's fixed map; `None` is a kind held unmatched, never guessed.
pub fn reaction_emoji(kind: &ReactionKind) -> Option<MappedReaction> {
    Some(match kind {
        ReactionKind::Like => MappedReaction::Upvote,
        ReactionKind::Love => MappedReaction::React("❤️"),
        ReactionKind::Care => MappedReaction::React("🤗"),
        ReactionKind::Haha => MappedReaction::React("😆"),
        ReactionKind::Wow => MappedReaction::React("😮"),
        ReactionKind::Sad => MappedReaction::React("😢"),
        ReactionKind::Angry => MappedReaction::React("😡"),
        ReactionKind::Other { .. } => return None,
    })
}

/// What ruling 5's ladder answers for one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved<'a> {
    /// A rung fired on exactly this entry.
    Hit(&'a OriginEntry),
    /// Rung (iii) found several posts sharing the link — never a guess.
    Ambiguous,
    /// No rung fired.
    None,
}

impl<'a> Resolved<'a> {
    pub fn hit(self) -> Option<&'a OriginEntry> {
        match self {
            Resolved::Hit(entry) => Some(entry),
            _ => None,
        }
    }

    /// The unmatched-ledger reason, when no entry was placed.
    pub fn unmatched_reason(self) -> Option<&'static str> {
        match self {
            Resolved::Hit(_) => None,
            Resolved::Ambiguous => Some(UNMATCHED_AMBIGUOUS_LINK),
            Resolved::None => Some(UNMATCHED_NO_RUNG),
        }
    }
}

/// Ruling 5's ladder — one ladder for the owner's own fold and the phase-two
/// merge: (i) the id rung, (ii) the permalink rung, (iii) the shared-link
/// rung over the entries not newer than the item (`at`), firing only on
/// exactly one survivor. Verbatim string compares, no normalisation.
pub fn resolve<'a>(
    target: &ArchiveTargetRef,
    at: Timestamp,
    index: &'a [OriginEntry],
) -> Resolved<'a> {
    if let Some(id) = &target.id
        && let Some(hit) = index.iter().find(|e| &e.external_id == id)
    {
        return Resolved::Hit(hit);
    }
    let Some(url) = target.url.as_deref() else {
        return Resolved::None;
    };
    if let Some(hit) = index.iter().find(|e| e.url.as_deref() == Some(url)) {
        return Resolved::Hit(hit);
    }
    let mut sharing = index
        .iter()
        .filter(|e| e.created_at <= at && e.links.iter().any(|l| l == url));
    match (sharing.next(), sharing.next()) {
        (Some(hit), None) => Resolved::Hit(hit),
        (Some(_), Some(_)) => Resolved::Ambiguous,
        (None, _) => Resolved::None,
    }
}

/// The channel-ward seam: the session's `archive_channel_for(contact)` then
/// `send_archive_signal` (`fauna-conversations`, the carrier), kept
/// behind a trait so the module is testable without MLS.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ArchiveSignalPoster: MaybeSendSync {
    /// Post the canonical `ArchiveSignal` bytes to `contact_hex`'s pair channel.
    async fn post(&self, contact_hex: &str, bytes: Vec<u8>) -> Result<(), ArchiveNestError>;
}

/// `flat_error` crosses UniFFI as the `Display` string — the row shows the
/// page's `error-message` either way.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
#[cfg_attr(feature = "uniffi", uniffi(flat_error))]
pub enum LinkError {
    #[error(transparent)]
    Nest(#[from] ArchiveNestError),
    #[error("no archive folder of this platform")]
    NoArchive,
    #[error("no question is open for this contact")]
    NotAsked,
    #[error("{0}")]
    Build(String),
}

/// What the sink dropped, by reason (the carrier's own tally counts what it
/// handed over; this one counts what the link module refused).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkTally {
    /// A sender whose roster row is not `accepted` or `confirmed`.
    pub not_a_contact: u64,
    /// Bytes that do not decode as an `ArchiveSignal`.
    pub undecodable: u64,
    /// A newer build's signal (`ArchiveSignal::Unknown`), ignored.
    pub unknown: u64,
    /// A `LinkOffer` whose ref is not in this user's friends (ruling 9).
    pub offer_not_a_friend: u64,
    /// A `Claimed` item that failed verification (ruling 9).
    pub claim_refused: u64,
}

/// The state a pair's row on the Contacts page renders (ruling 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ArchiveLinkState {
    Asked,
    Offered,
    Linked,
    Declined,
    Unlinked,
}

/// One pair, flat for the apps (ruling 8): derived from the folder union, so
/// no app computes it. Serde for web, whose binding hands it to the SPA as
/// JSON and takes it back for the two composers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ArchiveLinkView {
    /// The contact's actor id, hex.
    pub contact: String,
    pub state: ArchiveLinkState,
    /// The platform token (`facebook`, `instagram`).
    pub platform: String,
    /// The name the contact had on the platform, as this user's archive
    /// recorded it; empty when unknown.
    pub archived_name: String,
    pub claimed: u64,
    pub unmatched: u64,
}

/// Claimed and unmatched totals across every pair — the finished-import
/// step's line (ruling 11).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkCounts {
    pub claimed: u64,
    pub unmatched: u64,
}

/// The folders of one platform (newest first) and their union.
struct Loaded {
    folders: Vec<ArchiveFolderRef>,
    state: LinksState,
}

impl Loaded {
    fn owner(&self) -> &ExternalActorRef {
        &self.folders[0].marker.owner
    }
}

/// Phase two over the two seams. One per signed-in account.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct ArchiveLinks {
    nest: Arc<dyn ArchiveNest>,
    poster: Arc<dyn ArchiveSignalPoster>,
    keypair: ActorKeypair,
    tally: Mutex<LinkTally>,
    /// Serializes every read-modify-write of the folders' link state, so a
    /// signal the sink captures never races a round's write.
    write: WriteLock,
    /// Set when the sink captured a change that leaves work owed (a pair
    /// just linked owes its index; a stored index owes a claim run; a
    /// re-driven offer owes its answer). The sink never sends — it runs
    /// inside the receive loop — so the app's wiring polls [`Self::work_due`]
    /// and calls [`Self::drive`].
    due: std::sync::atomic::AtomicBool,
    /// Bumped on every state change the sink captures, so an app can repaint
    /// the Contacts rows when a question arrives without re-reading the
    /// folders on a timer ([`Self::captures`]).
    captures: std::sync::atomic::AtomicU64,
}

/// The apps' face (ruling 8), the same six calls on every app: tui and linux
/// call the inherent methods inline; the four FFI legs reach these through
/// UniFFI (built by `fauna-ffi`'s conversations factory, read back with
/// `FfiNestClient::archive_links`), and web through `fauna-wasm`'s
/// `WasmArchiveLinks`. Each forwards to the inherent method its doc names,
/// taking the platform as the view's token. Kept in its own block because
/// UniFFI exports every method of an exported block, private ones included.
#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl ArchiveLinks {
    /// [`Self::views`] — one view per pair, read with the roster.
    pub async fn link_views(&self) -> Result<Vec<ArchiveLinkView>, LinkError> {
        self.views().await
    }

    /// [`Self::answer_link`] for a view's `platform` token
    /// (`contact-archive-link-yes` / `-no`).
    pub async fn answer_link_token(
        &self,
        contact: String,
        platform: String,
        yes: bool,
    ) -> Result<(), LinkError> {
        self.answer_link(&contact, &parse_platform(&platform)?, yes)
            .await
    }

    /// [`Self::unlink`] for a view's `platform` token
    /// (`contact-archive-unlink`).
    pub async fn unlink_token(&self, contact: String, platform: String) -> Result<(), LinkError> {
        self.unlink(&contact, &parse_platform(&platform)?).await
    }

    /// [`Self::drive`] — once at login, and on the app's tick whenever
    /// [`Self::link_work_due`] reads true.
    pub async fn drive_links(&self) -> Result<(), LinkError> {
        self.drive().await
    }

    /// [`Self::work_due`] — an atomic load, cheap on a one-second tick.
    pub fn link_work_due(&self) -> bool {
        self.work_due()
    }

    /// [`Self::captures`] — moved since the app's last look → re-read
    /// [`Self::link_views`] and repaint the Contacts rows.
    pub fn link_captures(&self) -> u64 {
        self.captures()
    }
}

/// A view's platform token back to the [`Platform`] the link calls name.
fn parse_platform(token: &str) -> Result<Platform, LinkError> {
    Platform::parse_token(token)
        .ok_or_else(|| LinkError::Build(format!("unknown platform {token}")))
}

impl ArchiveLinks {
    pub fn new(
        nest: Arc<dyn ArchiveNest>,
        poster: Arc<dyn ArchiveSignalPoster>,
        keypair: &ActorKeypair,
    ) -> Self {
        Self {
            nest,
            poster,
            keypair: ActorKeypair::from_secret(*keypair.secret_bytes()),
            tally: Mutex::new(LinkTally::default()),
            write: WriteLock::default(),
            due: std::sync::atomic::AtomicBool::new(false),
            captures: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// How many state changes the sink has captured since this was built: a
    /// change since an app's last look means a row may now ask, or stop
    /// asking, its question.
    pub fn captures(&self) -> u64 {
        self.captures.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether a captured signal left work owed since the last [`Self::drive`].
    pub fn work_due(&self) -> bool {
        self.due.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn tally(&self) -> LinkTally {
        self.tally.lock().expect("link tally").clone()
    }

    fn bump(&self, f: impl FnOnce(&mut LinkTally)) {
        f(&mut self.tally.lock().expect("link tally"));
    }

    /// A stamp strictly after everything `state` already holds, so two
    /// writes inside one clock tick still order.
    fn stamp(state: &LinksState) -> Timestamp {
        let newest = state
            .pairs
            .iter()
            .map(|p| p.updated_at)
            .chain([state.updated_at])
            .max()
            .unwrap_or_default();
        Timestamp(Timestamp::now().0.max(newest.0 + 1))
    }

    // ── the folders ────────────────────────────────────────────────────────

    async fn platforms(&self) -> Result<Vec<Platform>, ArchiveNestError> {
        let mut out: Vec<Platform> = Vec::new();
        for folder in self.nest.list_archive_folders().await? {
            if folder.marker.platform.is_known() && !out.contains(&folder.marker.platform) {
                out.push(folder.marker.platform);
            }
        }
        Ok(out)
    }

    /// Every archive folder of `platform`, newest first, and the union of
    /// their link states; `None` when there is none (ruling 3).
    async fn load(&self, platform: &Platform) -> Result<Option<Loaded>, ArchiveNestError> {
        let mut folders: Vec<ArchiveFolderRef> = self
            .nest
            .list_archive_folders()
            .await?
            .into_iter()
            .filter(|f| &f.marker.platform == platform)
            .collect();
        if folders.is_empty() {
            return Ok(None);
        }
        folders.sort_by_key(|f| std::cmp::Reverse(f.marker.created_at));
        let mut states = Vec::new();
        for folder in &folders {
            let Some(bytes) = self.nest.read_file(&folder.folder, LINKS_PATH).await? else {
                continue;
            };
            match fauna_cbor::decode_strict::<LinksState>(&bytes) {
                Ok(state) => states.push(state),
                Err(e) => tracing::warn!(folder = %folder.folder, "unreadable {LINKS_PATH}: {e}"),
            }
        }
        Ok(Some(Loaded {
            folders,
            state: LinksState::union(states),
        }))
    }

    /// Writes the union whole into the newest folder of the platform.
    async fn save(&self, loaded: &mut Loaded) -> Result<(), ArchiveNestError> {
        loaded.state.updated_at = Self::stamp(&loaded.state);
        let bytes = fauna_cbor::encode_canonical(&loaded.state)
            .map_err(|e| ArchiveNestError::Transport(format!("encode {LINKS_PATH}: {e}")))?;
        self.nest
            .write_file(&loaded.folders[0].folder, LINKS_PATH, bytes)
            .await
    }

    /// Every entity of `category` across the platform's folders, each
    /// external id once (a re-import's folder repeats the older one's).
    async fn entities(
        &self,
        folders: &[ArchiveFolderRef],
        category: &Category,
    ) -> Result<Vec<Entity>, ArchiveNestError> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for folder in folders {
            let Some(bytes) = self
                .nest
                .read_file(&folder.folder, &model_path(category))
                .await?
            else {
                continue;
            };
            let Ok(entities) = fauna_cbor::decode_strict::<Vec<Entity>>(&bytes) else {
                continue;
            };
            for entity in entities {
                let key = match &entity {
                    Entity::Comment(c) => Some(c.external_id.clone()),
                    Entity::Reaction(r) => Some(r.external_id.clone()),
                    Entity::Post(p) => Some(p.external_id.clone()),
                    Entity::Album(a) => Some(a.external_id.clone()),
                    _ => None,
                };
                if key.is_none_or(|k| seen.insert(k)) {
                    out.push(entity);
                }
            }
        }
        Ok(out)
    }

    async fn friends(
        &self,
        folders: &[ArchiveFolderRef],
    ) -> Result<Vec<ExternalActorRef>, ArchiveNestError> {
        Ok(self
            .entities(folders, &Category::Friends)
            .await?
            .into_iter()
            .filter_map(|e| match e {
                Entity::Friendship(f) => Some(f.actor),
                _ => None,
            })
            .collect())
    }

    /// Every import state of the platform's folders (a newer build's state
    /// still names what it imported).
    async fn import_states(
        &self,
        folders: &[ArchiveFolderRef],
    ) -> Result<Vec<ImportState>, ArchiveNestError> {
        let mut out = Vec::new();
        for folder in folders {
            if let Some(bytes) = self.nest.read_file(&folder.folder, STATE_PATH).await?
                && let Ok(state) = fauna_cbor::decode_strict::<ImportState>(&bytes)
            {
                out.push(state);
            }
        }
        Ok(out)
    }

    /// This user's own imported post ids (hex) on the platform.
    async fn own_post_ids(
        &self,
        folders: &[ArchiveFolderRef],
    ) -> Result<BTreeSet<String>, ArchiveNestError> {
        Ok(self
            .import_states(folders)
            .await?
            .into_iter()
            .flat_map(|s| s.imported)
            .filter_map(|r| match r.target {
                ImportTarget::Post { post_id } => Some(post_id.to_ascii_lowercase()),
                _ => None,
            })
            .collect())
    }

    // ── the sink (ruling 2) ────────────────────────────────────────────────

    /// Ingest one received signal: `sender_hex` is the MLS-authenticated
    /// author. Admits only a sender whose roster row is `accepted` or
    /// `confirmed`, applies the transition, and writes the folder **before**
    /// answering `true`. `false` covers "refused" and "could not write".
    pub async fn ingest(&self, sender_hex: &str, bytes: &[u8]) -> bool {
        let Ok(signal) = fauna_cbor::decode_strict::<ArchiveSignal>(bytes) else {
            self.bump(|t| t.undecodable += 1);
            return false;
        };
        let Some(platform) = signal.platform().cloned() else {
            self.bump(|t| t.unknown += 1);
            return true;
        };
        match self.nest.contact_status(sender_hex).await {
            Ok(Some(status)) if matches!(status.as_str(), "accepted" | "confirmed") => {}
            Ok(_) => {
                self.bump(|t| t.not_a_contact += 1);
                return false;
            }
            Err(e) => {
                tracing::warn!("archive signal: roster read failed: {e}");
                return false;
            }
        }
        let _guard = self.write.lock().await;
        let mut loaded = match self.load(&platform).await {
            Ok(Some(loaded)) => loaded,
            // Ruling 3: nothing to compare against and nothing to store.
            Ok(None) => return true,
            Err(e) => {
                tracing::warn!("archive signal: folder read failed: {e}");
                return false;
            }
        };
        match self.apply(&mut loaded, sender_hex, &platform, signal).await {
            Ok(false) => true,
            Ok(true) => match self.save(&mut loaded).await {
                Ok(()) => {
                    self.captures
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    if !loaded.state.pending.is_empty()
                        || loaded
                            .state
                            .pairs
                            .iter()
                            .any(|p| p.state == PairState::Linked)
                    {
                        self.due.store(true, std::sync::atomic::Ordering::Release);
                    }
                    true
                }
                Err(e) => {
                    tracing::warn!("archive signal: capture failed: {e}");
                    false
                }
            },
            Err(e) => {
                tracing::warn!("archive signal: {e}");
                false
            }
        }
    }

    /// One transition; `Ok(true)` when the state changed.
    async fn apply(
        &self,
        loaded: &mut Loaded,
        sender: &str,
        platform: &Platform,
        signal: ArchiveSignal,
    ) -> Result<bool, ArchiveNestError> {
        let now = Self::stamp(&loaded.state);
        let current = loaded.state.pair(sender, platform).map(|p| p.state.clone());
        match signal {
            ArchiveSignal::Announce {
                fingerprint,
                name_fingerprint,
                ..
            } => {
                // Asked once, whichever way it went: any pair at all is final.
                if current.is_some() {
                    return Ok(false);
                }
                let friends = self.friends(&loaded.folders).await?;
                let Some(hit) = friends.into_iter().find(|f| {
                    announce_matches(platform, f, &fingerprint, name_fingerprint.as_ref())
                }) else {
                    return Ok(false);
                };
                loaded.state.pairs.push(new_pair(
                    sender,
                    platform,
                    PairState::Asked,
                    Some(hit),
                    false,
                    now,
                ));
                Ok(true)
            }
            ArchiveSignal::LinkOffer { me, .. } => {
                let friends = self.friends(&loaded.folders).await?;
                if !friends.iter().any(|f| same_person(f, &me)) {
                    self.bump(|t| t.offer_not_a_friend += 1);
                    return Ok(false);
                }
                match current {
                    None | Some(PairState::Asked) => {
                        upsert_pair(&mut loaded.state, sender, platform, now, |p| {
                            p.state = PairState::Asked;
                            p.asked_by_offer = true;
                            p.their_ref = Some(me);
                        });
                    }
                    // Crossed offers: both said yes — linked, and the
                    // contact's offer is answered.
                    Some(PairState::Offered) => {
                        upsert_pair(&mut loaded.state, sender, platform, now, |p| {
                            p.state = PairState::Linked;
                            p.their_ref = Some(me);
                            p.index_sent_at = None;
                        });
                        clear_awaiting(&mut loaded.state, sender, platform);
                        let me = loaded.owner().clone();
                        queue(
                            &mut loaded.state,
                            sender,
                            ArchiveSignal::LinkConfirm {
                                platform: platform.clone(),
                                me,
                            },
                            Awaits::Nothing,
                            now,
                        );
                    }
                    // A re-driven offer whose answer was lost: answer again.
                    Some(PairState::Linked) => {
                        let me = loaded.owner().clone();
                        queue(
                            &mut loaded.state,
                            sender,
                            ArchiveSignal::LinkConfirm {
                                platform: platform.clone(),
                                me,
                            },
                            Awaits::Nothing,
                            now,
                        );
                    }
                    Some(PairState::Declined) => {
                        queue(
                            &mut loaded.state,
                            sender,
                            ArchiveSignal::LinkDecline {
                                platform: platform.clone(),
                            },
                            Awaits::Nothing,
                            now,
                        );
                    }
                    Some(PairState::Unlinked | PairState::Unknown(_)) => return Ok(false),
                }
                Ok(true)
            }
            ArchiveSignal::LinkConfirm { me, .. } => {
                if current != Some(PairState::Offered) {
                    return Ok(false);
                }
                let friends = self.friends(&loaded.folders).await?;
                upsert_pair(&mut loaded.state, sender, platform, now, |p| {
                    p.state = PairState::Linked;
                    p.index_sent_at = None;
                    if friends.iter().any(|f| same_person(f, &me)) {
                        p.their_ref = Some(me);
                    }
                });
                clear_awaiting(&mut loaded.state, sender, platform);
                Ok(true)
            }
            ArchiveSignal::LinkDecline { .. } => {
                if !matches!(current, Some(PairState::Offered | PairState::Asked)) {
                    return Ok(false);
                }
                upsert_pair(&mut loaded.state, sender, platform, now, |p| {
                    p.state = PairState::Declined;
                });
                clear_awaiting(&mut loaded.state, sender, platform);
                Ok(true)
            }
            ArchiveSignal::Unlink { .. } => {
                if !matches!(
                    current,
                    Some(
                        PairState::Asked
                            | PairState::Offered
                            | PairState::Linked
                            | PairState::Declined
                    )
                ) {
                    return Ok(false);
                }
                upsert_pair(&mut loaded.state, sender, platform, now, |p| {
                    p.state = PairState::Unlinked;
                    p.origin_index = None;
                    p.index_received_at = None;
                });
                clear_awaiting(&mut loaded.state, sender, platform);
                Ok(true)
            }
            ArchiveSignal::OriginIndex { entries, .. } => {
                if current != Some(PairState::Linked) {
                    return Ok(false);
                }
                upsert_pair(&mut loaded.state, sender, platform, now, |p| {
                    p.origin_index = Some(entries);
                    p.index_received_at = Some(now);
                });
                Ok(true)
            }
            ArchiveSignal::Claimed { items, .. } => {
                if current != Some(PairState::Linked) {
                    return Ok(false);
                }
                let own = self.own_post_ids(&loaded.folders).await?;
                let mut changed = false;
                for item in items {
                    if loaded.state.received.iter().any(|r| {
                        r.contact == sender
                            && &r.platform == platform
                            && r.item.external_id == item.external_id
                    }) {
                        continue;
                    }
                    if !self.verify_claim(sender, &item, &own).await? {
                        self.bump(|t| t.claim_refused += 1);
                        continue;
                    }
                    loaded.state.received.push(ReceivedClaim {
                        contact: sender.to_string(),
                        platform: platform.clone(),
                        item,
                        at: now,
                    });
                    changed = true;
                }
                Ok(changed)
            }
            ArchiveSignal::Unknown(_) => Ok(false),
        }
    }

    /// Ruling 9: the named post exists, is signed by the sender, and
    /// references one of this user's own imported posts — the one named.
    async fn verify_claim(
        &self,
        sender: &str,
        item: &ClaimedItem,
        own: &BTreeSet<String>,
    ) -> Result<bool, ArchiveNestError> {
        let target = item.target_post_id.to_ascii_lowercase();
        if !own.contains(&target) {
            return Ok(false);
        }
        let Ok(sender_id) = ActorId::from_hex(sender) else {
            return Ok(false);
        };
        let Ok(target_digest) = fauna_core::hex32::decode(&target) else {
            return Ok(false);
        };
        let Some(bytes) = self.nest.fetch_post(&item.post_id, sender).await? else {
            return Ok(false);
        };
        let Ok((post, origin)) = decode_post_fetched_as(&item.post_id, &bytes) else {
            return Ok(false);
        };
        if origin.is_none() || post.author != sender_id {
            return Ok(false);
        }
        Ok(post.references.iter().any(|r| match r {
            Reference::Reply { post_id }
            | Reference::React { post_id, .. }
            | Reference::Upvote { post_id } => post_id.digest() == target_digest,
            _ => false,
        }))
    }

    // ── the user's answers (ruling 8) ──────────────────────────────────────

    /// The user's answer to the contact row's question. Yes to an
    /// announce's question offers; yes to an offer confirms (and the pair is
    /// linked); no declines, for good.
    pub async fn answer_link(
        &self,
        contact: &str,
        platform: &Platform,
        yes: bool,
    ) -> Result<(), LinkError> {
        {
            let _guard = self.write.lock().await;
            let mut loaded = self.load(platform).await?.ok_or(LinkError::NoArchive)?;
            let pair = loaded
                .state
                .pair(contact, platform)
                .ok_or(LinkError::NotAsked)?;
            if pair.state != PairState::Asked {
                return Err(LinkError::NotAsked);
            }
            let by_offer = pair.asked_by_offer;
            let now = Self::stamp(&loaded.state);
            let me = loaded.owner().clone();
            let p = platform.clone();
            let (state, signal, awaits) = match (yes, by_offer) {
                (true, false) => (
                    PairState::Offered,
                    ArchiveSignal::LinkOffer { platform: p, me },
                    Awaits::LinkAnswer,
                ),
                (true, true) => (
                    PairState::Linked,
                    ArchiveSignal::LinkConfirm { platform: p, me },
                    Awaits::Nothing,
                ),
                (false, _) => (
                    PairState::Declined,
                    ArchiveSignal::LinkDecline { platform: p },
                    Awaits::Nothing,
                ),
            };
            upsert_pair(&mut loaded.state, contact, platform, now, |pair| {
                pair.state = state;
                pair.index_sent_at = None;
            });
            queue(&mut loaded.state, contact, signal, awaits, now);
            self.save(&mut loaded).await?;
        }
        self.drive_platform(platform).await
    }

    /// Unlink from the contact's row: future merges stop, the stored index
    /// is dropped, claimed content stays (outcome 19).
    pub async fn unlink(&self, contact: &str, platform: &Platform) -> Result<(), LinkError> {
        {
            let _guard = self.write.lock().await;
            let mut loaded = self.load(platform).await?.ok_or(LinkError::NoArchive)?;
            if loaded.state.pair(contact, platform).map(|p| &p.state) != Some(&PairState::Linked) {
                return Err(LinkError::NotAsked);
            }
            let now = Self::stamp(&loaded.state);
            upsert_pair(&mut loaded.state, contact, platform, now, |p| {
                p.state = PairState::Unlinked;
                p.origin_index = None;
                p.index_received_at = None;
            });
            queue(
                &mut loaded.state,
                contact,
                ArchiveSignal::Unlink {
                    platform: platform.clone(),
                },
                Awaits::Nothing,
                now,
            );
            self.save(&mut loaded).await?;
        }
        self.drive_platform(platform).await
    }

    // ── the rounds (ruling 4) ──────────────────────────────────────────────

    /// After a finished import of `platform`: `Announce` to every accepted
    /// contact, a fresh index and the claim run to every linked one.
    pub async fn announce_round(&self, platform: &Platform) -> Result<(), LinkError> {
        {
            let _guard = self.write.lock().await;
            let mut loaded = self.load(platform).await?.ok_or(LinkError::NoArchive)?;
            let now = Self::stamp(&loaded.state);
            let fp = fingerprint(platform, loaded.owner());
            let name_fp = name_fingerprint(platform, loaded.owner());
            for contact in self.nest.accepted_contacts().await? {
                queue(
                    &mut loaded.state,
                    &contact,
                    ArchiveSignal::Announce {
                        platform: platform.clone(),
                        fingerprint: fp,
                        name_fingerprint: name_fp,
                    },
                    Awaits::Nothing,
                    now,
                );
            }
            let linked: Vec<String> = loaded
                .state
                .pairs
                .iter()
                .filter(|p| &p.platform == platform && p.state == PairState::Linked)
                .map(|p| p.contact.clone())
                .collect();
            for contact in linked {
                upsert_pair(&mut loaded.state, &contact, platform, now, |p| {
                    p.index_sent_at = None;
                    p.claims_run_at = None;
                });
            }
            self.save(&mut loaded).await?;
        }
        self.drive_platform(platform).await
    }

    /// The session-start re-drive, every platform: each linked pair's due
    /// index and due claim run, then every pending send.
    pub async fn drive(&self) -> Result<(), LinkError> {
        self.due.store(false, std::sync::atomic::Ordering::Release);
        for platform in self.platforms().await? {
            self.drive_platform(&platform).await?;
        }
        Ok(())
    }

    async fn drive_platform(&self, platform: &Platform) -> Result<(), LinkError> {
        {
            let _guard = self.write.lock().await;
            let Some(mut loaded) = self.load(platform).await? else {
                return Ok(());
            };
            // Due indices and claim runs queue their signals, so the one
            // pending pass below sends everything.
            let due_index: Vec<String> = loaded
                .state
                .pairs
                .iter()
                .filter(|p| {
                    &p.platform == platform
                        && p.state == PairState::Linked
                        && p.index_sent_at.is_none()
                })
                .map(|p| p.contact.clone())
                .collect();
            for contact in &due_index {
                let entries = self.origin_index_for(&loaded, contact).await?;
                let now = Self::stamp(&loaded.state);
                queue(
                    &mut loaded.state,
                    contact,
                    ArchiveSignal::OriginIndex {
                        platform: platform.clone(),
                        entries,
                    },
                    Awaits::Nothing,
                    now,
                );
                upsert_pair(&mut loaded.state, contact, platform, now, |p| {
                    p.index_sent_at = Some(now)
                });
            }
            if !due_index.is_empty() {
                self.save(&mut loaded).await?;
            }
            let due_claims: Vec<String> = loaded
                .state
                .pairs
                .iter()
                .filter(|p| {
                    &p.platform == platform
                        && p.state == PairState::Linked
                        && p.origin_index.is_some()
                        && p.claims_run_at
                            .is_none_or(|run| p.index_received_at.is_some_and(|got| got > run))
                })
                .map(|p| p.contact.clone())
                .collect();
            for contact in due_claims {
                self.claim_run(&mut loaded, &contact).await?;
            }
        }
        self.send_pending(platform).await
    }

    /// Sends every pending signal of `platform`'s folders. The posts leave
    /// outside the write lock (the receive loop that calls the sink must
    /// never wait on a send), then each outcome is folded into the state as
    /// it now stands: a sent signal that awaits nothing leaves the list, one
    /// awaiting an answer stays, marked sent, to be re-driven; a failed send
    /// stays as it was.
    async fn send_pending(&self, platform: &Platform) -> Result<(), LinkError> {
        let to_send = {
            let _guard = self.write.lock().await;
            match self.load(platform).await? {
                Some(loaded) => loaded.state.pending,
                None => return Ok(()),
            }
        };
        let mut delivered = Vec::new();
        for pending in to_send {
            let bytes = fauna_cbor::encode_canonical(&pending.signal)
                .map_err(|e| LinkError::Build(e.to_string()))?;
            match self.poster.post(&pending.contact, bytes).await {
                Ok(()) => delivered.push(pending),
                Err(e) => {
                    tracing::warn!(contact = %pending.contact, "archive signal send failed: {e}")
                }
            }
        }
        if delivered.is_empty() {
            return Ok(());
        }
        let _guard = self.write.lock().await;
        let mut loaded = self.load(platform).await?.ok_or(LinkError::NoArchive)?;
        for sent in delivered {
            let Some(at) = loaded.state.pending.iter().position(|p| *p == sent) else {
                continue;
            };
            if sent.awaits == Awaits::Nothing {
                loaded.state.pending.remove(at);
            } else {
                loaded.state.pending[at].sent = true;
            }
        }
        self.save(&mut loaded).await?;
        Ok(())
    }

    /// The index this user owes `contact`: public posts always, followers
    /// posts only when the contact is an approved subscriber, owner-only
    /// never (§ Linking and merging, *The origin index*).
    async fn origin_index_for(
        &self,
        loaded: &Loaded,
        contact: &str,
    ) -> Result<Vec<OriginEntry>, LinkError> {
        let subscriber = self.nest.is_followers_subscriber(contact).await?;
        let states = self.import_states(&loaded.folders).await?;
        let posts = self.entities(&loaded.folders, &Category::Posts).await?;
        let mut out = Vec::new();
        for state in &states {
            for record in &state.imported {
                let ImportTarget::Post { post_id } = &record.target else {
                    continue;
                };
                let Some(Entity::Post(post)) = posts
                    .iter()
                    .find(|e| matches!(e, Entity::Post(p) if p.external_id == record.external_id))
                else {
                    continue;
                };
                let audience = map_audience(post.audience, &state.scope.audience_mode);
                let public = match audience {
                    PostAudience::Public => true,
                    PostAudience::Followers if subscriber => false,
                    _ => continue,
                };
                if out
                    .iter()
                    .any(|e: &OriginEntry| e.external_id == record.external_id)
                {
                    continue;
                }
                out.push(OriginEntry {
                    external_id: record.external_id.clone(),
                    url: post.url.clone(),
                    links: post.links.clone(),
                    created_at: Timestamp(post.created_at.0),
                    post_id: post_id.clone(),
                    public,
                });
            }
        }
        Ok(out)
    }

    // ── the claim run (rulings 5–7) ────────────────────────────────────────

    async fn claim_run(&self, loaded: &mut Loaded, contact: &str) -> Result<(), LinkError> {
        let platform = loaded.folders[0].marker.platform.clone();
        let owner = loaded.owner().clone();
        let Some(pair) = loaded.state.pair(contact, &platform).cloned() else {
            return Ok(());
        };
        let (Some(their_ref), Some(index)) = (pair.their_ref.clone(), pair.origin_index.clone())
        else {
            return Ok(());
        };
        self.replay_in_flight(loaded).await?;

        let mut comments: Vec<ArchiveComment> = Vec::new();
        let mut reactions: Vec<ArchiveReaction> = Vec::new();
        for entity in self.entities(&loaded.folders, &Category::Comments).await? {
            if let Entity::Comment(c) = entity {
                comments.push(c);
            }
        }
        for entity in self.entities(&loaded.folders, &Category::Reactions).await? {
            if let Entity::Reaction(r) = entity {
                reactions.push(r);
            }
        }
        let theirs = |target: &ArchiveTargetRef| {
            target
                .owner
                .as_ref()
                .is_some_and(|o| same_person(o, &their_ref))
        };
        comments.retain(|c| same_person(&c.author, &owner) && theirs(&c.target));
        reactions.retain(|r| same_person(&r.author, &owner) && theirs(&r.target));
        comments.sort_by_key(|c| c.created_at);
        reactions.sort_by_key(|r| r.created_at);

        let subscriber = self.nest.is_followers_subscriber(contact).await?;
        for comment in &comments {
            if loaded
                .state
                .claim(contact, &platform, &comment.external_id)
                .is_some_and(ClaimEntry::is_final)
            {
                continue;
            }
            let entry = self
                .claim_comment(loaded, contact, &platform, comment, &index, subscriber)
                .await?;
            let now = Self::stamp(&loaded.state);
            loaded
                .state
                .record_claim(contact, &platform, &comment.external_id, entry, now);
            self.save(loaded).await?;
        }
        for reaction in &reactions {
            if loaded
                .state
                .claim(contact, &platform, &reaction.external_id)
                .is_some_and(ClaimEntry::is_final)
            {
                continue;
            }
            let entry = self.claim_reaction(&platform, reaction, &index).await?;
            let now = Self::stamp(&loaded.state);
            loaded
                .state
                .record_claim(contact, &platform, &reaction.external_id, entry, now);
            self.save(loaded).await?;
        }

        let items: Vec<ClaimedItem> = loaded
            .state
            .claims
            .iter()
            .filter(|c| c.contact == contact && c.platform == platform)
            .filter_map(|c| match &c.entry {
                ClaimEntry::Claimed {
                    post_id,
                    target_post_id,
                    ..
                } => Some(ClaimedItem {
                    external_id: c.external_id.clone(),
                    post_id: post_id.clone(),
                    target_post_id: target_post_id.clone(),
                }),
                _ => None,
            })
            .collect();
        let now = Self::stamp(&loaded.state);
        if !items.is_empty() {
            queue(
                &mut loaded.state,
                contact,
                ArchiveSignal::Claimed {
                    platform: platform.clone(),
                    items,
                },
                Awaits::Nothing,
                now,
            );
        }
        upsert_pair(&mut loaded.state, contact, &platform, now, |p| {
            p.claims_run_at = Some(now)
        });
        self.save(loaded).await?;
        Ok(())
    }

    async fn claim_comment(
        &self,
        loaded: &mut Loaded,
        contact: &str,
        platform: &Platform,
        comment: &ArchiveComment,
        index: &[OriginEntry],
        subscriber: bool,
    ) -> Result<ClaimEntry, LinkError> {
        let resolved = resolve(&comment.target, Timestamp(comment.created_at.0), index);
        let Some(target) = resolved.hit() else {
            return Ok(ClaimEntry::Unmatched {
                reason: resolved.unmatched_reason().unwrap_or_default().to_string(),
            });
        };
        let words = comment.text.as_deref().unwrap_or("").trim();
        if words.is_empty() {
            return Ok(ClaimEntry::HeldMedia);
        }
        let media_held = comment.media.len() as u64;
        let cid = target_cid(&target.post_id)?;
        let authoring = PostAuthoring::imported(Timestamp(comment.created_at.0), platform.token());
        if target.public {
            let bytes = build_referencing_post_at(
                &self.keypair,
                words,
                &[],
                cid,
                ReferencedAudience::Public,
                ReferenceKind::Reply,
                &authoring,
            )
            .map_err(|e| LinkError::Build(e.to_string()))?;
            let post_id = self.nest.create_post(bytes).await?;
            return Ok(ClaimEntry::Claimed {
                post_id,
                target_post_id: target.post_id.clone(),
                media_held,
            });
        }
        // Ruling 6: sealed to this user's own followers tier, and only when
        // the contact can open it.
        if !subscriber {
            return Ok(ClaimEntry::HeldUnreadable);
        }
        let gate = self.nest.provision_followers_tier().await?;
        let seal_id = mint_seal_id().map_err(|e| LinkError::Build(e.to_string()))?;
        let build = build_sealed_referencing_post_at(
            &self.keypair,
            words,
            cid,
            ReferenceKind::Reply,
            SealedAudience::Tier {
                tier: &gate.tier,
                tier_rank: gate.rank,
                key_blob_ref: gate.key_blob_ref,
                period_key: &gate.period_key,
            },
            seal_id,
            &authoring,
        )
        .map_err(|e| LinkError::Build(e.to_string()))?;
        loaded.state.in_flight = Some(InFlightClaim {
            contact: contact.to_string(),
            platform: platform.clone(),
            external_id: comment.external_id.clone(),
            target_post_id: target.post_id.clone(),
            post_bytes: build.post_bytes,
            sealed_body: build.encrypted_blob,
            media_held,
        });
        self.save(loaded).await?;
        let post_id = self.create_in_flight(loaded).await?;
        Ok(ClaimEntry::Claimed {
            post_id,
            target_post_id: target.post_id.clone(),
            media_held,
        })
    }

    /// Lands the written-ahead sealed reply: its body blob, then its create
    /// — both content-addressed, so a replay of a create that did land is a
    /// no-op. Clears the write-ahead and answers the post id.
    async fn create_in_flight(&self, loaded: &mut Loaded) -> Result<String, LinkError> {
        let flight = loaded
            .state
            .in_flight
            .clone()
            .expect("a written-ahead claim");
        let echoed = self
            .nest
            .upload_gated_body(flight.sealed_body.clone())
            .await?;
        if echoed != blake3::hash(&flight.sealed_body).to_hex().as_str() {
            return Err(LinkError::Build(
                "the nest's sealed-body reply does not name the uploaded blob".into(),
            ));
        }
        let post_id = self.nest.create_post(flight.post_bytes).await?;
        loaded.state.in_flight = None;
        Ok(post_id)
    }

    async fn replay_in_flight(&self, loaded: &mut Loaded) -> Result<(), LinkError> {
        let Some(flight) = loaded.state.in_flight.clone() else {
            return Ok(());
        };
        let post_id = self.create_in_flight(loaded).await?;
        let now = Self::stamp(&loaded.state);
        loaded.state.record_claim(
            &flight.contact,
            &flight.platform,
            &flight.external_id,
            ClaimEntry::Claimed {
                post_id,
                target_post_id: flight.target_post_id,
                media_held: flight.media_held,
            },
            now,
        );
        self.save(loaded).await?;
        Ok(())
    }

    async fn claim_reaction(
        &self,
        platform: &Platform,
        reaction: &ArchiveReaction,
        index: &[OriginEntry],
    ) -> Result<ClaimEntry, LinkError> {
        let Some(mapped) = reaction_emoji(&reaction.kind) else {
            return Ok(ClaimEntry::Unmatched {
                reason: UNMATCHED_REACTION_KIND.to_string(),
            });
        };
        let resolved = resolve(&reaction.target, Timestamp(reaction.created_at.0), index);
        let Some(target) = resolved.hit() else {
            return Ok(ClaimEntry::Unmatched {
                reason: resolved.unmatched_reason().unwrap_or_default().to_string(),
            });
        };
        let authoring = PostAuthoring::imported(Timestamp(reaction.created_at.0), platform.token());
        let emoji = match mapped {
            MappedReaction::Upvote => None,
            MappedReaction::React(emoji) => Some(emoji),
        };
        let bytes = build_reaction_post_at(
            &self.keypair,
            target_cid(&target.post_id)?,
            emoji,
            &authoring,
        )
        .map_err(|e| LinkError::Build(e.to_string()))?;
        let post_id = self.nest.create_post(bytes).await?;
        Ok(ClaimEntry::Claimed {
            post_id,
            target_post_id: target.post_id.clone(),
            media_held: 0,
        })
    }

    // ── what the apps read ─────────────────────────────────────────────────

    /// One view per pair the apps render (ruling 8), every platform.
    pub async fn views(&self) -> Result<Vec<ArchiveLinkView>, LinkError> {
        let mut out = Vec::new();
        for platform in self.platforms().await? {
            let Some(loaded) = self.load(&platform).await? else {
                continue;
            };
            for pair in &loaded.state.pairs {
                let state = match pair.state {
                    PairState::Asked => ArchiveLinkState::Asked,
                    PairState::Offered => ArchiveLinkState::Offered,
                    PairState::Linked => ArchiveLinkState::Linked,
                    PairState::Declined => ArchiveLinkState::Declined,
                    PairState::Unlinked => ArchiveLinkState::Unlinked,
                    PairState::Unknown(_) => continue,
                };
                let counts = counts_of(&loaded.state, Some(&pair.contact));
                out.push(ArchiveLinkView {
                    contact: pair.contact.clone(),
                    state,
                    platform: pair.platform.token().to_string(),
                    archived_name: pair
                        .their_ref
                        .as_ref()
                        .map(|r| r.display_name.clone())
                        .unwrap_or_default(),
                    claimed: counts.claimed,
                    unmatched: counts.unmatched,
                });
            }
        }
        Ok(out)
    }

    /// Claimed and unmatched totals across every pair (ruling 11).
    pub async fn counts(&self) -> Result<LinkCounts, LinkError> {
        let mut total = LinkCounts::default();
        for platform in self.platforms().await? {
            if let Some(loaded) = self.load(&platform).await? {
                let c = counts_of(&loaded.state, None);
                total.claimed += c.claimed;
                total.unmatched += c.unmatched;
            }
        }
        Ok(total)
    }

    /// The external ids of this user's private comment copies a contact has
    /// claimed — hidden from the collapsed-comment view from here on.
    pub async fn hidden_comment_ids(
        &self,
        platform: &Platform,
    ) -> Result<BTreeSet<ExternalId>, LinkError> {
        Ok(self
            .load(platform)
            .await?
            .map(|l| {
                l.state
                    .received
                    .into_iter()
                    .map(|r| r.item.external_id)
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// A minimal async lock over `std` alone — the crate's tokio is native-only,
/// and the module stays wasm-clean (§ Architectural rules). Held across
/// awaits; a waiter parks its waker and every release wakes them all.
#[derive(Default)]
struct WriteLock {
    state: Mutex<(bool, Vec<std::task::Waker>)>,
}

struct WriteGuard<'a>(&'a WriteLock);

impl WriteLock {
    async fn lock(&self) -> WriteGuard<'_> {
        std::future::poll_fn(|cx| {
            let mut state = self.state.lock().expect("write lock");
            if state.0 {
                state.1.push(cx.waker().clone());
                std::task::Poll::Pending
            } else {
                state.0 = true;
                std::task::Poll::Ready(WriteGuard(self))
            }
        })
        .await
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        let waiters = {
            let mut state = self.0.state.lock().expect("write lock");
            state.0 = false;
            std::mem::take(&mut state.1)
        };
        for waker in waiters {
            waker.wake();
        }
    }
}

fn counts_of(state: &LinksState, contact: Option<&str>) -> LinkCounts {
    let mut out = LinkCounts::default();
    for record in state
        .claims
        .iter()
        .filter(|c| contact.is_none_or(|k| c.contact == k))
    {
        match record.entry {
            ClaimEntry::Claimed { .. } => out.claimed += 1,
            ClaimEntry::Unmatched { .. } => out.unmatched += 1,
            _ => {}
        }
    }
    out
}

/// A post id (hex) as the 36-byte CID a reference names — the digest the
/// nest files the post under (the feed's own reply path).
fn target_cid(post_id: &str) -> Result<[u8; 36], LinkError> {
    let digest = fauna_core::hex32::decode(post_id).map_err(|e| LinkError::Build(e.to_string()))?;
    Ok(*fauna_cbor::Cid::from_digest_dag_cbor(digest).as_bytes())
}

fn new_pair(
    contact: &str,
    platform: &Platform,
    state: PairState,
    their_ref: Option<ExternalActorRef>,
    asked_by_offer: bool,
    now: Timestamp,
) -> PairLink {
    PairLink {
        contact: contact.to_string(),
        platform: platform.clone(),
        state,
        asked_by_offer,
        their_ref,
        origin_index: None,
        index_received_at: None,
        claims_run_at: None,
        index_sent_at: None,
        updated_at: now,
        extra: Default::default(),
    }
}

fn upsert_pair(
    state: &mut LinksState,
    contact: &str,
    platform: &Platform,
    now: Timestamp,
    f: impl FnOnce(&mut PairLink),
) {
    if state.pair(contact, platform).is_none() {
        state.pairs.push(new_pair(
            contact,
            platform,
            PairState::Asked,
            None,
            false,
            now,
        ));
    }
    let pair = state.pair_mut(contact, platform).expect("just ensured");
    f(pair);
    pair.updated_at = now;
}

/// Queues `signal` for `contact`, once: an identical unsent signal already
/// queued is not repeated.
fn queue(
    state: &mut LinksState,
    contact: &str,
    signal: ArchiveSignal,
    awaits: Awaits,
    now: Timestamp,
) {
    if state
        .pending
        .iter()
        .any(|p| p.contact == contact && p.signal == signal && !p.sent)
    {
        return;
    }
    state.pending.push(PendingSend {
        contact: contact.to_string(),
        signal,
        awaits,
        sent: false,
        at: now,
    });
}

/// The contact answered: this side's `LinkOffer` to them is no longer owed.
fn clear_awaiting(state: &mut LinksState, contact: &str, platform: &Platform) {
    state.pending.retain(|p| {
        !(p.contact == contact
            && p.awaits == Awaits::LinkAnswer
            && p.signal.platform() == Some(platform))
    });
}
