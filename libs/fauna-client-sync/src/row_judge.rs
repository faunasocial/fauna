//! **The projection readers' half of writer-signed change records** —
//! `docs/goal/architecture/mls-group-key-material.md` § M2 → *Multi-writer* →
//! *Writer-signed change records*, ruling (3): every client reader verifies a
//! row before it lists, downloads or restores it, and a row that fails is
//! skipped, warned, counted and treated as absent.
//!
//! The sync engine judges the `changes.list` rows it pulls; this module serves
//! the readers that never see a `changes.list` row, only a *projection* of one —
//! a `fauna.media.list` item ([`fauna_protocol::media::MediaItem::as_change_row`])
//! or a `fauna.files.versions.list` entry
//! ([`fauna_protocol::files::FileVersionInfo::as_change_row`]). The verdict is
//! the one shared judge's ([`RowReader`]); what lives here is only what a
//! projection reader must gather that an engine already holds — each set's
//! binding (its nonce, its owner, its serve flag) and, on demand, its writer
//! roster — across however many sets one listing spans.
//!
//! **The listing is judged, not its sets.** A projection listing is re-fetched
//! whole on every refresh, so a row that cannot be judged *yet* (its writer's
//! roster could not be read) is simply absent from this listing and judged
//! again on the next — there is no cursor to hold.
//!
//! **A row signed under a retired identity** (ruling (8)) verifies as its
//! successor's through the seat's own-account source: the predecessor ids the
//! app attested ([`ReaderSeat::predecessors`]), and — for a seat handed none,
//! or missing one — the same statement walk the roster runs, ending at the
//! seat's own id over `fauna.recovery.succession.lookup`
//! ([`LearnedPredecessors`]). A judged item is stamped with the verdict's
//! writer and with whether it was signed as the seat's *current* identity,
//! the fact every open of its bytes or label is keyed on.
//!
//! Scope: same-nest listings only. `fauna.media.list` and the version-history
//! kinds project the caller's own nest's rows, so every roster read here is
//! the own nest's `fauna.folders.members.list_actors`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex};

use fauna_core::encoding::EmbedAsBytes;
use fauna_protocol::folders::{
    ActorMembersListReply, ActorMembersListRequest, FoldersListReply, FoldersListRequest,
    KIND_FOLDERS_LIST, KIND_FOLDERS_MEMBERS_LIST_ACTORS,
};
use fauna_protocol::sync::SyncChange;
use fauna_protocol::sync_row_verify::{
    Held, ROSTER_NOT_SHARED, ReaderBinding, RowReader, RowVerdict, WriterRoster, writer_roster,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::SetNonceSource;

/// One row a listing projects: the set it belongs to and its rebuilt statement
/// — `None` when the projection carries none.
///
/// The set is addressed by its hash ([`Self::set`]): `folder_hash` as the
/// projection carries it, else the hash of `folder`. A sealed set's Media item
/// carries the hash alone — its `folder` is the scrubbed empty string
/// (`path-sealing.md` § the set-name plane) — so a judge keyed by name would
/// fold every sealed set into one blank-named set that matches no folder row.
#[derive(Debug, Clone)]
pub struct ProjectedRow<'a> {
    pub folder: &'a str,
    pub folder_hash: Option<&'a [u8]>,
    pub row: Option<SyncChange>,
}

impl ProjectedRow<'_> {
    /// The set's address — what the folder list's row, the nonce source and
    /// the roster read are all asked by.
    fn set(&self) -> [u8; 32] {
        fauna_core::label_custody::set_name_label_salt(self.folder_hash, self.folder)
    }
}

/// What one judged listing found — "counted", in the ruling's words.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectionTally {
    pub verified: usize,
    pub exempt: usize,
    /// Not records: absent from the listing.
    pub refused: usize,
    /// Not judgeable yet: absent from this listing, judged again on the next.
    pub held: usize,
    /// History (`writer-signed-change-records.md` ruling (11)(c)): a version
    /// of its path and never the item — absent from a Media listing, listed
    /// by a version listing.
    pub history: usize,
}

/// Who is reading — what a projection reader holds to judge by: its own
/// actor id (the owner of every set the folder list reports as its own) and
/// where each set's nonce comes from (the same source the seat signs its own
/// records through). `Default` = neither: no signed row verifies — each is
/// held.
#[derive(Clone, Default)]
pub struct ReaderSeat {
    pub own: Option<[u8; 32]>,
    pub nonces: Option<SetNonceSource>,
    /// The account's **attested** predecessor ids —
    /// `AccountRegistry::attested_predecessor_actor_ids`, attestation by
    /// possession of each predecessor's seed (ruling (8)(b), source (ii)).
    /// Empty for an identity that never succeeded another, and for a seat that
    /// was handed none — which then proves the link itself
    /// ([`Self::learned`]).
    pub predecessors: Vec<[u8; 32]>,
    /// What this seat proved by the statement walk, shared by every clone of
    /// the seat for its lifetime.
    pub learned: LearnedPredecessors,
}

impl ReaderSeat {
    /// The judge for this seat over `nest`.
    pub fn judge<'a, R>(&'a self, nest: &'a R) -> ProjectionJudge<'a, R>
    where
        R: RpcRequester,
        R::Error: RpcErrorClass + core::fmt::Display,
    {
        ProjectionJudge::new(nest, self.own, self.nonces.as_ref())
            .with_predecessors(&self.predecessors, self.learned.clone())
    }
}

/// The predecessor ids a seat **proved for its own identity** from landed
/// succession statements — ruling (8)(b), source (ii)'s second half: a host
/// that was handed no attested set (a device restored from the successor's
/// seed alone, a capability host, an app not yet passing its registry's) runs
/// the same walk the roster runs, ending at its own id, over the pre-identity
/// `fauna.recovery.succession.lookup`. The successor's own `new_sig` is the
/// proof, so the nest that answers the lookup decides nothing.
///
/// Process-lifetime memory, shared by clones: a proven id is not asked for
/// again, and neither is an id the walk proved nothing for — a stranger's
/// signed row costs one lookup per seat, not one per listing.
#[derive(Clone, Default)]
pub struct LearnedPredecessors(Arc<Mutex<Lineage>>);

#[derive(Default)]
struct Lineage {
    proven: Vec<[u8; 32]>,
    asked: HashSet<[u8; 32]>,
}

impl LearnedPredecessors {
    /// Every id the walk proved, in the order learned.
    pub fn proven(&self) -> Vec<[u8; 32]> {
        self.lock().proven.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Lineage> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Mark `actor` asked; `false` when it already was.
    fn first_ask(&self, actor: [u8; 32]) -> bool {
        self.lock().asked.insert(actor)
    }

    /// Keep one walk's result. A walk is a contiguous run of the chain from
    /// the nearest hop back to the id it was asked about, so anything proven
    /// before that it does not name is older: the walk leads, the rest follow.
    fn learn(&self, walk: Vec<[u8; 32]>) {
        let mut lineage = self.lock();
        lineage.proven = Self::ordered(&walk, &lineage.proven);
    }

    /// Whether `other` is a clone of this memory — what one proves, the other
    /// holds.
    pub fn shares_memory_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// `walk`, then every id of `rest` it does not name, in `rest`'s order.
    fn ordered(walk: &[[u8; 32]], rest: &[[u8; 32]]) -> Vec<[u8; 32]> {
        let mut all = walk.to_vec();
        for id in rest {
            if !all.contains(id) {
                all.push(*id);
            }
        }
        all
    }

    /// This identity's whole proven chain, nearest hop first: what the walk
    /// proved, then every id of `handed` (the attested set a host was given,
    /// itself nearest first) the walk did not reach.
    pub fn chain_with(&self, handed: &[[u8; 32]]) -> Vec<[u8; 32]> {
        Self::ordered(&self.proven(), handed)
    }

    /// **The statement walk** (ruling (8)(b), source (ii), the linkless
    /// half) — the one place a reader asks the nest for it, shared by the
    /// projection judge and the sync engine: for each of `unplaced` (signed
    /// actors that verified but resolved to no writer) not asked about before,
    /// read the succession path forward from it and keep what
    /// [`fauna_core::recovery::proven_predecessors_carried`] proves **ends at
    /// `own`**. At most [`MAX_LINK_LOOKUPS_PER_LISTING`] lookups a call.
    /// `true` when something new was proven (the caller judges again); a
    /// lookup that fails or proves nothing changes nothing — and one that
    /// never reached the nest (a transport fault, not the nest's refusal) is
    /// not remembered as asked, so a long-lived reader that met its first
    /// predecessor row offline asks again.
    pub async fn prove_links<R>(
        &self,
        nest: &R,
        own: [u8; 32],
        unplaced: impl IntoIterator<Item = [u8; 32]>,
    ) -> bool
    where
        R: RpcRequester,
        R::Error: RpcErrorClass,
    {
        let mut learned = false;
        let mut asked = 0;
        for signed_as in unplaced {
            if asked == MAX_LINK_LOOKUPS_PER_LISTING {
                break;
            }
            if !self.first_ask(signed_as) {
                continue;
            }
            asked += 1;
            let reply: Result<fauna_protocol::recovery::SuccessionLookupReply, R::Error> = nest
                .request(
                    fauna_protocol::RpcError::SUCCESSION_LOOKUP_KIND,
                    fauna_protocol::recovery::SuccessionLookupRequest {
                        actor_id: fauna_protocol::ByteBuf::from(signed_as.to_vec()),
                        ..Default::default()
                    },
                )
                .await;
            let reply = match reply {
                Ok(reply) => reply,
                Err(e) => {
                    if e.as_rpc_error().is_none() {
                        self.lock().asked.remove(&signed_as);
                    }
                    continue;
                }
            };
            let proven = fauna_core::recovery::proven_predecessors_carried(
                &fauna_core::identity::ActorId(own),
                &reply.statements,
            );
            if !proven.is_empty() {
                self.learn(proven.into_iter().map(|a| a.0).collect());
                learned = true;
            }
        }
        learned
    }
}

/// How many signed actors one listing may ask the succession lookup about. A
/// real chain is units long and one lookup from its oldest id proves all of
/// it; the bound keeps a listing full of strangers' rows from fanning out.
const MAX_LINK_LOOKUPS_PER_LISTING: usize = 4;

/// The judge over a projection listing, for one seat on one nest.
pub struct ProjectionJudge<'a, R> {
    nest: &'a R,
    /// This seat's actor id — the owner of every set the folder list reports
    /// as the caller's own.
    own: Option<[u8; 32]>,
    /// Where each set's nonce comes from — the same source the seat signs its
    /// own records through. `None`: no nonce resolves, so no signed row
    /// verifies (each is held).
    nonces: Option<&'a SetNonceSource>,
    /// The seat's attested predecessor ids ([`ReaderSeat::predecessors`]).
    predecessors: &'a [[u8; 32]],
    /// What the seat proved by the statement walk.
    learned: LearnedPredecessors,
}

impl<'a, R> ProjectionJudge<'a, R>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    pub fn new(nest: &'a R, own: Option<[u8; 32]>, nonces: Option<&'a SetNonceSource>) -> Self {
        Self {
            nest,
            own,
            nonces,
            predecessors: &[],
            learned: LearnedPredecessors::default(),
        }
    }

    /// Judge with the seat's own-account predecessor sources (ruling (8)(b),
    /// source (ii)): the attested ids, and the memory the statement walk fills.
    #[must_use]
    pub fn with_predecessors(
        mut self,
        attested: &'a [[u8; 32]],
        learned: LearnedPredecessors,
    ) -> Self {
        self.predecessors = attested;
        self.learned = learned;
        self
    }

    /// Every predecessor id proven for this seat's own identity so far.
    fn account_predecessors(&self) -> Vec<[u8; 32]> {
        self.learned.chain_with(self.predecessors)
    }

    /// One verdict per row, in order. `certs` is the listing's `signer_certs`
    /// side table (every page's, for a paged listing).
    ///
    /// Gathers only what the listing needs: no control-plane read at all when
    /// no row carries a statement, the binding source's answer for every set
    /// with one (its served state — and its nonce, used for a signed row),
    /// and a roster only for a set with a row the judge held for want of one
    /// (the engine's re-judge-after-one-read shape). A failed folder list
    /// fails the listing — without it no set has an owner, and every row of
    /// an owner-only set would read as a stranger's.
    pub async fn judge(
        &self,
        rows: &[ProjectedRow<'_>],
        certs: &[EmbedAsBytes],
    ) -> Result<Vec<RowVerdict>, R::Error> {
        let unbound = RowReader::new();
        // Each row's set by its hash address, and — for a set with a
        // statement-bearing row — the name the projection gave it (empty for a
        // sealed set, which the projection names by hash alone).
        let sets: Vec<[u8; 32]> = rows.iter().map(ProjectedRow::set).collect();
        let mut with_statement: BTreeMap<[u8; 32], &str> = BTreeMap::new();
        for (p, set) in rows.iter().zip(&sets).filter(|(p, _)| p.row.is_some()) {
            let name = with_statement.entry(*set).or_insert(p.folder);
            if name.is_empty() {
                *name = p.folder;
            }
        }
        if with_statement.is_empty() {
            return Ok(rows
                .iter()
                .map(|p| unbound.judge_projection(p.row.as_ref()))
                .collect());
        }
        let signed: BTreeSet<[u8; 32]> = rows
            .iter()
            .zip(&sets)
            .filter(|(p, _)| p.row.as_ref().is_some_and(|r| r.signature.is_some()))
            .map(|(_, set)| *set)
            .collect();
        let mut readers = self.readers(&with_statement, &signed, certs).await?;
        let judge_all = |readers: &BTreeMap<[u8; 32], RowReader>| -> Vec<RowVerdict> {
            rows.iter()
                .zip(&sets)
                .map(|(p, set)| {
                    readers
                        .get(set)
                        .unwrap_or(&unbound)
                        .judge_projection(p.row.as_ref())
                })
                .collect()
        };
        let mut verdicts = judge_all(&readers);
        let unread: BTreeSet<[u8; 32]> = sets
            .iter()
            .zip(&verdicts)
            .filter(|(_, v)| {
                matches!(
                    v,
                    RowVerdict::Held(Held::RosterUnread | Held::MinterUnplaced)
                )
            })
            .map(|(set, _)| *set)
            .collect();
        if !unread.is_empty() {
            for set in unread {
                let name = with_statement.get(&set).copied().unwrap_or_default();
                if let (Some(writers), Some(reader)) =
                    (self.roster(name, &set).await, readers.get_mut(&set))
                {
                    reader.install_roster(writers);
                }
            }
            verdicts = judge_all(&readers);
        }
        if self.learn_own_links(rows, &sets, &verdicts, &readers).await {
            let proven = self.account_predecessors();
            for reader in readers.values_mut() {
                let binding = ReaderBinding {
                    account_predecessors: proven.clone(),
                    ..reader.binding().clone()
                };
                reader.install_binding(binding);
            }
            verdicts = judge_all(&readers);
        }
        Ok(verdicts)
    }

    /// Ruling (8)(b), source (ii), the linkless half: for each row that
    /// verified cryptographically but whose signed actor resolved to no writer
    /// — on a set this seat itself may write — ask the nest for the succession
    /// path forward from that actor and keep what the walk proves **ends at
    /// this seat's own id**. `true` when something new was proven (the caller
    /// judges again). A lookup that fails or proves nothing changes nothing:
    /// the refusal stands, exactly as before.
    async fn learn_own_links(
        &self,
        rows: &[ProjectedRow<'_>],
        sets: &[[u8; 32]],
        verdicts: &[RowVerdict],
        readers: &BTreeMap<[u8; 32], RowReader>,
    ) -> bool {
        let Some(own) = self.own else {
            return false;
        };
        let mut unplaced: Vec<[u8; 32]> = Vec::new();
        for ((projected, set), verdict) in rows.iter().zip(sets).zip(verdicts) {
            if !verdict.unattributed() {
                continue;
            }
            let (Some(reader), Some(row)) = (readers.get(set), projected.row.as_ref()) else {
                continue;
            };
            if !reader.is_writer(&own) {
                continue;
            }
            if let Some(signed_as) = reader.signed_actor(row)
                && !unplaced.contains(&signed_as)
            {
                unplaced.push(signed_as);
            }
        }
        self.learned.prove_links(self.nest, own, unplaced).await
    }

    /// A reader per set with a statement-bearing row: the owner from one
    /// `fauna.folders.list` (the shared-with-me rows included — read here for
    /// a binding, never rendered), the served state and — for a set with a
    /// signed row — the nonce from the binding source, and the listing's
    /// certs.
    async fn readers(
        &self,
        folders: &BTreeMap<[u8; 32], &str>,
        signed: &BTreeSet<[u8; 32]>,
        certs: &[EmbedAsBytes],
    ) -> Result<BTreeMap<[u8; 32], RowReader>, R::Error> {
        let reply: FoldersListReply = self
            .nest
            .request(
                KIND_FOLDERS_LIST,
                FoldersListRequest {
                    include_shared_with_me: Some(true),
                    extra: Default::default(),
                },
            )
            .await?;
        // Rows match by `name_hash`, never the plaintext: a sealed set's row
        // rests with its `name` scrubbed (`path-sealing.md` § the set-name
        // plane), and this raw list read renders nothing.
        let mut bindings: BTreeMap<[u8; 32], ReaderBinding> = BTreeMap::new();
        for fs in &reply.folders {
            let row_hash = fauna_core::label_custody::set_name_label_salt(
                fs.name_hash.as_deref().map(|b| &b[..]),
                &fs.name,
            );
            // The first row at an address wins (the caller's own sets list
            // first); the same address is what the nonce resolver is asked by.
            if !folders.contains_key(&row_hash) || bindings.contains_key(&row_hash) {
                continue;
            }
            // A member row names no owner here: this reader holds no
            // marker, so the judge takes the owner off the roster's owner row
            // (`writer-signed-change-records.md` ruling (11)(c)) — never off
            // the list row's `owner_actor_id`. Its rows hold for that read.
            let owner = match fs.role.as_deref() {
                Some("member") => None,
                _ => self.own,
            };
            bindings.insert(
                row_hash,
                ReaderBinding {
                    owner,
                    ..Default::default()
                },
            );
        }
        let account_predecessors = self.account_predecessors();
        let mut readers = BTreeMap::new();
        for (set, &name) in folders {
            let mut binding = bindings.remove(set).unwrap_or_default();
            binding.account = self.own;
            binding.account_predecessors = account_predecessors.clone();
            // The binding source, asked for EVERY set the projection names
            // (`writer-signed-change-records.md` ruling (7)(b)(ii) rule (2)):
            // the served state is the owner's word in custody, never the
            // roster's `webdav_enabled` — and the set whose rows are all
            // unsigned DAV rows is exactly the one that needs the answer. No
            // source, or a failed read, exempts nothing.
            let lineage = self.lineage(name, set).await;
            binding.webdav_served = lineage.webdav_served();
            if signed.contains(set) {
                // The live nonce with its minter and the set's lineage (ruling
                // (11)(b)): a row signed under a retired nonce is judged
                // history or current there, never refused as unverifiable.
                binding.set_nonce = lineage.live;
                binding.live_minted_by = lineage.live_minted_by.map(|a| a.0);
                binding.retired_set_nonces = lineage
                    .retired
                    .iter()
                    .map(|r| (r.nonce, r.minted_by.map(|a| a.0)))
                    .collect();
            }
            let mut reader = RowReader::new();
            reader.install_binding(binding);
            reader.ingest_certs(certs);
            readers.insert(*set, reader);
        }
        Ok(readers)
    }

    async fn lineage(
        &self,
        folder: &str,
        set: &[u8; 32],
    ) -> fauna_core::folder_keys::SetNonceLineage {
        let Some(nonces) = self.nonces else {
            return Default::default();
        };
        match nonces.lookup_lineage(folder, set).await {
            Ok(lineage) => lineage,
            Err(e) => {
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(folder),
                    "reader set nonce: lookup failed; the set's signed rows cannot verify: {e}"
                );
                Default::default()
            }
        }
    }

    /// The set's writers, or `None` when the read failed (the rows it would
    /// judge stay held, absent from this listing). An owner-only set has no
    /// roster: its writers are the owner alone. Asked through the one funnel
    /// every set request leaves by (`addressed`): by the hash of the name, or
    /// — for a sealed set the projection named by hash alone — by that hash,
    /// since the nest holds no plaintext name for it and neither does this
    /// judge.
    async fn roster(&self, folder: &str, set: &[u8; 32]) -> Option<WriterRoster> {
        let reply: Result<ActorMembersListReply, R::Error> = self
            .nest
            .request(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS,
                fauna_protocol::folders::addressed(ActorMembersListRequest {
                    name: folder.to_string(),
                    name_hash: folder
                        .is_empty()
                        .then(|| fauna_protocol::ByteBuf::from(set.to_vec())),
                    ..Default::default()
                }),
            )
            .await;
        match reply {
            Ok(reply) => Some(writer_roster(&reply.members)),
            Err(e)
                if e.as_rpc_error()
                    .is_some_and(|r| r.code == ROSTER_NOT_SHARED) =>
            {
                Some(WriterRoster::default())
            }
            Err(e) => {
                tracing::debug!(
                    folder = %fauna_core::log_redact::log_folder_name(folder),
                    error = %e,
                    "reader writer roster: read failed; a member's rows stay held"
                );
                None
            }
        }
    }
}

/// Judge a `fauna.media.list` reply — one page, or every page merged
/// (`fauna_client_media::MediaClient::list_all`) — through each item's head row
/// (`MediaItem::as_change_row`) and the reply's `signer_certs`, keeping only
/// the admitted items (the certs and cursor stay as served). The one media
/// judging path: the Media seam's grid and the search index's file walk and
/// query-time drain all read `fauna.media.list` through it.
pub async fn judge_media_listing<R>(
    nest: &R,
    seat: &ReaderSeat,
    mut listing: fauna_protocol::media::MediaListReply,
    what: &str,
) -> Result<(fauna_protocol::media::MediaListReply, ProjectionTally), R::Error>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let verdicts = {
        let rows: Vec<_> = listing
            .items
            .iter()
            .map(|item| ProjectedRow {
                folder: &item.folder,
                folder_hash: item.folder_hash.as_deref().map(|b| &b[..]),
                row: item.as_change_row(),
            })
            .collect();
        seat.judge(nest).judge(&rows, &listing.signer_certs).await?
    };
    for (item, verdict) in listing.items.iter_mut().zip(&verdicts) {
        // Ruling (8)(c): the item is attributed to the verdict's WRITER (so
        // the author field stays a verified one), and carries whether it was
        // signed as this seat's current identity — what every open of its
        // bytes or label is keyed on.
        if let RowVerdict::Verified { writer, .. } = verdict {
            item.author_actor_id = Some(fauna_protocol::ByteBuf::from(writer.to_vec()));
        }
        item.signed_as_current = verdict.signed_as(seat.own.as_ref());
        item.signed_as = verified_signer(verdict);
    }
    let (items, tally) = retain_admitted(std::mem::take(&mut listing.items), &verdicts, what);
    listing.items = items;
    Ok((listing, tally))
}

/// The identity a verified row was signed as — what a projection stamps for
/// the per-signer bound (ruling (8)(c)): a verified row's, and a history row's
/// (ruling (11)(c): a history version is rendered under the identity it was
/// signed as, and a restore opens it under that identity's roots). `None` for
/// any other verdict.
pub fn verified_signer(verdict: &RowVerdict) -> Option<[u8; 32]> {
    match verdict {
        RowVerdict::Verified { signed_as, .. } | RowVerdict::History { signed_as, .. } => {
            Some(*signed_as)
        }
        _ => None,
    }
}

/// The **version listing's** retain, over `(version, verdict)` pairs — the
/// shape [`crate::SyncClient::versions_list_judged`] answers: every version the
/// judge admits, **and every history version** (ruling (11)(c): a history row
/// is listed among its path's versions, under the identity it was signed as —
/// it is never the item, which is why a Media listing drops it
/// ([`retain_admitted`])).
pub fn retain_judged<T>(judged: Vec<(T, RowVerdict)>, what: &str) -> (Vec<T>, ProjectionTally) {
    let mut tally = ProjectionTally::default();
    let mut kept = Vec::with_capacity(judged.len());
    for (item, verdict) in judged {
        if verdict.is_history() {
            tally.history += 1;
            kept.push(item);
            continue;
        }
        let (mut admitted, one) = retain_admitted(vec![item], std::slice::from_ref(&verdict), what);
        tally.verified += one.verified;
        tally.exempt += one.exempt;
        tally.refused += one.refused;
        tally.held += one.held;
        kept.append(&mut admitted);
    }
    (kept, tally)
}

/// Keep the items whose verdict admits them, in order; warn on each one that
/// does not ("skipped, warned, counted" — ruling (3)). `what` names the
/// listing in the log.
pub fn retain_admitted<T>(
    items: Vec<T>,
    verdicts: &[RowVerdict],
    what: &str,
) -> (Vec<T>, ProjectionTally) {
    debug_assert_eq!(items.len(), verdicts.len());
    let mut tally = ProjectionTally::default();
    let mut kept = Vec::with_capacity(items.len());
    for (item, verdict) in items.into_iter().zip(verdicts) {
        match verdict {
            RowVerdict::Verified { .. } => tally.verified += 1,
            RowVerdict::Exempt => tally.exempt += 1,
            RowVerdict::Refused(why) => {
                tally.refused += 1;
                tracing::warn!(error = %why, "{what}: a row did not verify — treated as absent");
                continue;
            }
            RowVerdict::Held(why) => {
                tally.held += 1;
                tracing::warn!(
                    reason = ?why,
                    "{what}: a row cannot be judged yet — absent from this listing"
                );
                continue;
            }
            // Ruling (11)(c): a version of its path, never the item.
            RowVerdict::History { .. } => {
                tally.history += 1;
                tracing::debug!("{what}: a history row — a version, never the item");
                continue;
            }
        }
        kept.push(item);
    }
    (kept, tally)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RejectingRequester, block_on};
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::RpcError;
    use fauna_protocol::folders::{FolderActorMember, FolderSummary};
    use fauna_protocol::sync_writer_sig::{ChangeSigner, ChangeVerifyError};

    const NONCE: [u8; 32] = [7; 32];
    const OTHER_NONCE: [u8; 32] = [9; 32];

    fn kp(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    /// An unsigned row of `author`'s, as a projection rebuilds it.
    fn row(author: &ActorKeypair) -> SyncChange {
        SyncChange {
            seq: 3,
            path_hash: fauna_core::hex32::encode(&fauna_core::sync::path_hash("a.jpg")),
            manifest_hash: Some(fauna_core::hex32::encode(&[3; 32])),
            size_bytes: 10,
            change_type: "create".into(),
            created_at: 1_000,
            device_id: Some(fauna_core::hex32::encode(&[4; 32])),
            author_actor_id: Some(author.actor_id().to_hex()),
            ..Default::default()
        }
    }

    fn signed(author: &ActorKeypair, nonce: [u8; 32]) -> SyncChange {
        let mut r = row(author);
        ChangeSigner::direct(author)
            .sign_row(&mut r, nonce)
            .unwrap();
        r
    }

    fn own_set(name: &str) -> FolderSummary {
        FolderSummary {
            name: name.into(),
            role: Some("owner".into()),
            ..Default::default()
        }
    }

    /// Projected the way a sealed set rests: plaintext `name` scrubbed, the
    /// row addressed by its `name_hash` alone — so every member-set test also
    /// pins that the binding matches by hash.
    fn member_set(owner: &ActorKeypair) -> FolderSummary {
        FolderSummary {
            name_hash: Some(
                fauna_core::path_crypto::set_name_hash("photos")
                    .to_vec()
                    .into(),
            ),
            role: Some("member".into()),
            owner_actor_id: Some(owner.actor_id().to_hex()),
            ..Default::default()
        }
    }

    fn folders(rows: Vec<FolderSummary>) -> FoldersListReply {
        FoldersListReply {
            folders: rows,
            ..Default::default()
        }
    }

    fn roster(members: &[(&ActorKeypair, &str, Option<&str>)]) -> ActorMembersListReply {
        ActorMembersListReply {
            members: members
                .iter()
                .map(|(k, role, access)| FolderActorMember {
                    actor_id: k.actor_id().to_hex(),
                    role: role.to_string(),
                    access: access.map(str::to_string),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn nonces() -> SetNonceSource {
        SetNonceSource::by_folder([("photos".to_string(), NONCE)].into_iter().collect())
    }

    fn at(folder: &str, row: Option<SyncChange>) -> ProjectedRow<'_> {
        ProjectedRow {
            folder,
            folder_hash: None,
            row,
        }
    }

    /// A binding source that answers one serve window for every set and
    /// counts how often it was asked.
    #[derive(Default)]
    struct ServeWindow {
        served_at: Option<u64>,
        unserved_at: Option<u64>,
        asked: std::sync::atomic::AtomicUsize,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::folder_keys::FolderKeyResolver for ServeWindow {
        async fn resolve(
            &self,
            _name_hash: &[u8; 32],
        ) -> anyhow::Result<fauna_core::folder_keys::ResolvedCustody> {
            anyhow::bail!("not a key read")
        }

        async fn set_lineage(
            &self,
            _folder: &str,
            _name_hash: &[u8; 32],
        ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(fauna_core::folder_keys::SetNonceLineage {
                served_at: self.served_at,
                unserved_at: self.unserved_at,
                ..Default::default()
            })
        }
    }

    /// An unsigned row under the WebDAV pseudo-device of `owner`.
    fn dav_row(owner: &ActorKeypair) -> SyncChange {
        SyncChange {
            device_id: Some(fauna_core::hex32::encode(
                &fauna_core::label_custody::webdav_pseudo_device_id(&owner.actor_id().0),
            )),
            signature: None,
            signer_key: None,
            ..row(owner)
        }
    }

    /// `writer-signed-change-records.md` ruling (7)(b)(ii) rule (2): the
    /// served state is the owner's word in custody, never the roster's flag.
    /// A pseudo-device row planted on a set the NEST flags served and custody
    /// does not is refused; the same row on a set custody calls served is
    /// exempt whatever the nest's flag says — and a projection whose rows are
    /// all unsigned DAV rows still asks the source.
    #[test]
    fn the_served_exemption_follows_custody_never_the_roster_flag() {
        let owner = kp(1);
        let flagged = FolderSummary {
            webdav_enabled: true,
            ..own_set("photos")
        };
        let rows = vec![at("photos", Some(dav_row(&owner)))];

        // The nest flags the set served; custody holds no serve-on.
        let nest = RejectingRequester::new().reply(KIND_FOLDERS_LIST, &folders(vec![flagged]));
        let window = std::sync::Arc::new(ServeWindow::default());
        let source = SetNonceSource::Resolver(window.clone());
        let judge = ProjectionJudge::new(&nest, Some(owner.actor_id().0), Some(&source));
        assert_eq!(
            block_on(judge.judge(&rows, &[])).expect("judged"),
            vec![RowVerdict::Refused(ChangeVerifyError::Unsigned)]
        );
        assert_eq!(
            window.asked.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a projection of unsigned rows still asks the source"
        );

        // Custody calls it served; the nest's flag is off.
        let nest =
            RejectingRequester::new().reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]));
        let source = SetNonceSource::Resolver(std::sync::Arc::new(ServeWindow {
            served_at: Some(100),
            ..Default::default()
        }));
        let judge = ProjectionJudge::new(&nest, Some(owner.actor_id().0), Some(&source));
        assert_eq!(
            block_on(judge.judge(&rows, &[])).expect("judged"),
            vec![RowVerdict::Exempt]
        );

        // The owner unserved it since: a tie or a later serve-off exempts
        // nothing.
        let source = SetNonceSource::Resolver(std::sync::Arc::new(ServeWindow {
            served_at: Some(100),
            unserved_at: Some(100),
            ..Default::default()
        }));
        let judge = ProjectionJudge::new(&nest, Some(owner.actor_id().0), Some(&source));
        assert_eq!(
            block_on(judge.judge(&rows, &[])).expect("judged"),
            vec![RowVerdict::Refused(ChangeVerifyError::Unsigned)]
        );
    }

    /// The owner's row verifies; the same owner's row signed under ANOTHER
    /// set's nonce is refused and absent; so are an unsigned row and a
    /// statement-less projection (every writer signs) — and the owner's own
    /// set needs no roster read.
    #[test]
    fn a_row_signed_under_another_sets_nonce_and_unsigned_rows_are_absent() {
        let owner = kp(1);
        let nest =
            RejectingRequester::new().reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]));
        let nonces = nonces();
        let judge = ProjectionJudge::new(&nest, Some(owner.actor_id().0), Some(&nonces));
        let rows = vec![
            at("photos", Some(signed(&owner, NONCE))),
            at("photos", Some(signed(&owner, OTHER_NONCE))),
            at("photos", Some(row(&owner))),
            at("photos", None),
        ];
        let verdicts = block_on(judge.judge(&rows, &[])).expect("judged");
        assert_eq!(
            verdicts[1],
            RowVerdict::Refused(ChangeVerifyError::SignatureInvalid)
        );
        let (kept, tally) = retain_admitted(
            vec!["good", "foreign-nonce", "unsigned", "bare"],
            &verdicts,
            "test",
        );
        assert_eq!(kept, ["good"]);
        assert_eq!(
            tally,
            ProjectionTally {
                verified: 1,
                refused: 3,
                ..Default::default()
            }
        );
        assert_eq!(nest.kinds(), [KIND_FOLDERS_LIST]);
    }

    /// Ruling (10)(e), the version reader's half: a conflict report's retained
    /// loser signed by its reporter (the flag in the statement) is admitted
    /// and attributed to that reporter; an unsigned retention row is refused,
    /// never `Exempt` — the flag on the projection alone must not list it.
    /// The engine's fold still answers `Exempt` for both.
    #[test]
    fn a_retention_version_is_admitted_only_by_its_reporters_signature() {
        let owner = kp(1);
        let nest =
            RejectingRequester::new().reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]));
        let nonces = nonces();
        let judge = ProjectionJudge::new(&nest, Some(owner.actor_id().0), Some(&nonces));
        let mut signed_loser = row(&owner);
        signed_loser.is_retention = Some(true);
        ChangeSigner::direct(&owner)
            .sign_row(&mut signed_loser, NONCE)
            .unwrap();
        let mut unsigned_loser = row(&owner);
        unsigned_loser.is_retention = Some(true);
        let rows = vec![
            at("photos", Some(signed_loser.clone())),
            at("photos", Some(unsigned_loser.clone())),
        ];
        let verdicts = block_on(judge.judge(&rows, &[])).expect("judged");
        assert!(
            matches!(verdicts[0], RowVerdict::Verified { writer, .. } if writer == owner.actor_id().0),
            "{:?}",
            verdicts[0]
        );
        assert_eq!(
            verdicts[1],
            RowVerdict::Refused(ChangeVerifyError::Unsigned)
        );
        let (kept, _) = retain_admitted(vec!["signed", "unsigned"], &verdicts, "test");
        assert_eq!(kept, ["signed"]);

        let mut fold = RowReader::new();
        fold.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(owner.actor_id().0),
            ..Default::default()
        });
        assert_eq!(fold.judge(&signed_loser), RowVerdict::Exempt);
        assert_eq!(fold.judge(&unsigned_loser), RowVerdict::Exempt);
    }

    /// A listing with no statement-bearing row reads nothing off the control
    /// plane — and admits nothing: a statement-less row is judged unsigned.
    #[test]
    fn a_listing_without_statements_makes_no_control_plane_read() {
        let nest = RejectingRequester::new();
        let judge = ProjectionJudge::new(&nest, None, None);
        let verdicts = block_on(judge.judge(&[at("photos", None)], &[])).expect("judged");
        assert!(!verdicts[0].admits());
        assert!(nest.kinds().is_empty());
    }

    /// On a shared set, a writer member's row verifies after ONE roster read;
    /// a reader member's is refused; the owner (named by the member row of the
    /// folder list) verifies without the roster.
    #[test]
    fn a_member_set_reads_its_roster_once_and_admits_only_writers() {
        let (owner, me, writer, reader) = (kp(1), kp(2), kp(3), kp(4));
        let nest = RejectingRequester::new()
            .reply(KIND_FOLDERS_LIST, &folders(vec![member_set(&owner)]))
            .reply(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS,
                &roster(&[
                    (&owner, "owner", None),
                    (&writer, "member", Some("writer")),
                    (&reader, "member", Some("reader")),
                ]),
            );
        let nonces = nonces();
        let judge = ProjectionJudge::new(&nest, Some(me.actor_id().0), Some(&nonces));
        let rows = vec![
            at("photos", Some(signed(&owner, NONCE))),
            at("photos", Some(signed(&writer, NONCE))),
            at("photos", Some(signed(&reader, NONCE))),
        ];
        let verdicts = block_on(judge.judge(&rows, &[])).expect("judged");
        assert!(matches!(verdicts[0], RowVerdict::Verified { .. }));
        assert!(matches!(verdicts[1], RowVerdict::Verified { .. }));
        assert_eq!(
            verdicts[2],
            RowVerdict::Refused(ChangeVerifyError::NotAWriter)
        );
        assert_eq!(
            nest.kinds(),
            [KIND_FOLDERS_LIST, KIND_FOLDERS_MEMBERS_LIST_ACTORS]
        );
    }

    /// A roster that could not be read holds the member's row — absent from
    /// this listing, counted as held; an unshared set's roster is the owner
    /// alone, so a stranger's signed row there is refused.
    #[test]
    fn an_unread_roster_holds_and_an_unshared_set_admits_only_its_owner() {
        let (owner, stranger) = (kp(1), kp(5));
        let rows = vec![at("photos", Some(signed(&stranger, NONCE)))];
        let nonces = nonces();

        let failing = RejectingRequester::new()
            .reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]))
            .reject(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS,
                RpcError::new("fauna.folders.internal", "error.x"),
            );
        let judge = ProjectionJudge::new(&failing, Some(owner.actor_id().0), Some(&nonces));
        let verdicts = block_on(judge.judge(&rows, &[])).expect("judged");
        let (kept, tally) = retain_admitted(vec![()], &verdicts, "test");
        assert!(kept.is_empty());
        assert_eq!(tally.held, 1);

        let unshared = RejectingRequester::new()
            .reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]))
            .reject(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS,
                RpcError::new(ROSTER_NOT_SHARED, "error.x"),
            );
        let judge = ProjectionJudge::new(&unshared, Some(owner.actor_id().0), Some(&nonces));
        let verdicts = block_on(judge.judge(&rows, &[])).expect("judged");
        assert_eq!(
            verdicts[0],
            RowVerdict::Refused(ChangeVerifyError::NotAWriter)
        );
    }

    /// The one media judging path — the Media grid, the search index's file
    /// walk and its query-time drain all page `fauna.media.list` through it: an
    /// item signed under another set's nonce is dropped, the unsigned one is
    /// kept and counted, and the page's certs and cursor stay as served.
    #[test]
    fn a_media_page_keeps_only_admitted_items() {
        use fauna_protocol::ByteBuf;
        use fauna_protocol::media::{MediaItem, MediaListReply};
        let owner = kp(1);
        let signer = ChangeSigner::direct(&owner);
        let item = |path: &str, nonce: Option<[u8; 32]>| {
            let mut it = MediaItem {
                folder: "photos".into(),
                path: path.into(),
                size_bytes: 10,
                updated_at: 1_000,
                path_hash: Some(ByteBuf::from(fauna_core::sync::path_hash(path).to_vec())),
                manifest_hash: Some(ByteBuf::from(vec![3; 32])),
                device_id: Some(ByteBuf::from(vec![4; 32])),
                author_actor_id: Some(ByteBuf::from(owner.actor_id().0.to_vec())),
                change_type: Some("create".into()),
                ..Default::default()
            };
            if let Some(nonce) = nonce {
                let mut row = it.as_change_row().expect("statement");
                signer.sign_row(&mut row, nonce).expect("signs");
                it.signature = row.signature;
                it.signer_key = row.signer_key;
            }
            it
        };
        let nest =
            RejectingRequester::new().reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]));
        let seat = ReaderSeat {
            own: Some(owner.actor_id().0),
            nonces: Some(nonces()),
            ..Default::default()
        };
        let page = MediaListReply {
            items: vec![
                item("good.jpg", Some(NONCE)),
                item("foreign.jpg", Some(OTHER_NONCE)),
                item("unsigned.jpg", None),
            ],
            next_cursor: Some("next".into()),
            ..Default::default()
        };
        let (page, tally) =
            block_on(judge_media_listing(&nest, &seat, page, "test")).expect("judged");
        let paths: Vec<&str> = page.items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["good.jpg"]);
        assert_eq!((tally.verified, tally.refused), (1, 2));
        assert_eq!(page.next_cursor.as_deref(), Some("next"));
    }

    /// A binding source that knows sets by their hash address alone, as a
    /// reader of sealed sets does.
    struct NonceByHash(Vec<([u8; 32], [u8; 32])>);

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::folder_keys::FolderKeyResolver for NonceByHash {
        async fn resolve(
            &self,
            _name_hash: &[u8; 32],
        ) -> anyhow::Result<fauna_core::folder_keys::ResolvedCustody> {
            anyhow::bail!("not a key read")
        }

        async fn set_lineage(
            &self,
            _folder: &str,
            name_hash: &[u8; 32],
        ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
            Ok(fauna_core::folder_keys::SetNonceLineage {
                live: self
                    .0
                    .iter()
                    .find(|(hash, _)| hash == name_hash)
                    .map(|(_, nonce)| *nonce),
                ..Default::default()
            })
        }
    }

    /// A sealed set's Media item names its set by `folder_hash` alone — the
    /// `folder` is the scrubbed empty string. Each item is judged under ITS
    /// set's row, nonce and roster: two sealed sets on one page do not fold
    /// into one blank-named set, and neither is held for want of a nonce.
    #[test]
    fn a_sealed_sets_media_items_are_judged_by_their_folder_hash() {
        use fauna_protocol::ByteBuf;
        use fauna_protocol::media::{MediaItem, MediaListReply};
        let (owner, me) = (kp(1), kp(2));
        let signer = ChangeSigner::direct(&owner);
        let hash = |name: &str| fauna_core::path_crypto::set_name_hash(name);
        let item = |set: &str, path: &str, nonce: [u8; 32]| {
            let mut it = MediaItem {
                folder_hash: Some(ByteBuf::from(hash(set).to_vec())),
                folder_sealed: Some(ByteBuf::from(vec![1u8])),
                path_hash: Some(ByteBuf::from(fauna_core::sync::path_hash(path).to_vec())),
                path_sealed: Some(ByteBuf::from(path.as_bytes().to_vec())),
                size_bytes: 10,
                updated_at: 1_000,
                manifest_hash: Some(ByteBuf::from(vec![3; 32])),
                device_id: Some(ByteBuf::from(vec![4; 32])),
                author_actor_id: Some(ByteBuf::from(owner.actor_id().0.to_vec())),
                change_type: Some("create".into()),
                ..Default::default()
            };
            let mut row = it.as_change_row().expect("statement");
            signer.sign_row(&mut row, nonce).expect("signs");
            it.signature = row.signature;
            it.signer_key = row.signer_key;
            it
        };
        let sealed_member_set = |name: &str| FolderSummary {
            name_hash: Some(hash(name).to_vec().into()),
            ..member_set(&owner)
        };
        let nest = RejectingRequester::new()
            .reply(
                KIND_FOLDERS_LIST,
                &folders(vec![sealed_member_set("photos"), sealed_member_set("docs")]),
            )
            .reply(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS,
                &roster(&[(&owner, "owner", None)]),
            );
        let seat = ReaderSeat {
            own: Some(me.actor_id().0),
            nonces: Some(SetNonceSource::Resolver(std::sync::Arc::new(NonceByHash(
                vec![(hash("photos"), NONCE), (hash("docs"), OTHER_NONCE)],
            )))),
            ..Default::default()
        };
        let page = MediaListReply {
            items: vec![
                item("photos", "a.jpg", NONCE),
                item("docs", "b.jpg", OTHER_NONCE),
                // Signed under the OTHER set's nonce: not this set's row.
                item("docs", "c.jpg", NONCE),
            ],
            ..Default::default()
        };
        let (page, tally) =
            block_on(judge_media_listing(&nest, &seat, page, "test")).expect("judged");
        let kept: Vec<_> = page
            .items
            .iter()
            .map(|i| i.path_sealed.clone().expect("sealed path"))
            .collect();
        assert_eq!(
            kept,
            [
                ByteBuf::from(b"a.jpg".to_vec()),
                ByteBuf::from(b"b.jpg".to_vec())
            ]
        );
        assert_eq!((tally.verified, tally.refused, tally.held), (2, 1, 0));
    }

    /// No nonce source: a signed row cannot verify yet, so it is HELD — absent
    /// from the listing and judged again on the next — never verified.
    #[test]
    fn without_a_nonce_source_a_signed_row_is_held() {
        let owner = kp(1);
        let nest =
            RejectingRequester::new().reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]));
        let judge = ProjectionJudge::new(&nest, Some(owner.actor_id().0), None);
        let verdicts = block_on(judge.judge(&[at("photos", Some(signed(&owner, NONCE)))], &[]))
            .expect("judged");
        assert_eq!(verdicts[0], RowVerdict::Held(Held::NoNonce));
    }
    // ── Ruling (8): rows signed under a retired identity ────────────────────

    /// `old` succeeded by `new`, as the landed statement carries it; `new_sig`
    /// by `new_signer` (the successor itself for a genuine link).
    fn link(
        old: &ActorKeypair,
        new: &ActorKeypair,
        new_signer: &ActorKeypair,
    ) -> fauna_protocol::ByteBuf {
        let recovery = fauna_core::recovery::RecoveryKey::generate();
        let signed = fauna_core::recovery::IdentitySuccession {
            old_actor_id: old.actor_id(),
            new_actor_id: new.actor_id(),
            recovery_pubkey: recovery.public(),
            seq: 2,
            created_at: fauna_core::data::Timestamp(0),
        }
        .sign(&recovery, new_signer.signing_key(), None)
        .expect("sign the succession");
        fauna_protocol::ByteBuf::from(
            fauna_core::encoding::canonical_encode(&signed).expect("encode"),
        )
    }

    /// A Media item whose head row `signer` signed directly, served — as the
    /// nest serves it after a succession — with `served_author` as its stamp.
    fn media_item(
        path: &str,
        signer: &ActorKeypair,
        served_author: &ActorKeypair,
    ) -> fauna_protocol::media::MediaItem {
        use fauna_protocol::ByteBuf;
        let mut it = fauna_protocol::media::MediaItem {
            folder: "photos".into(),
            path: path.into(),
            size_bytes: 10,
            updated_at: 1_000,
            path_hash: Some(ByteBuf::from(fauna_core::sync::path_hash(path).to_vec())),
            manifest_hash: Some(ByteBuf::from(vec![3; 32])),
            device_id: Some(ByteBuf::from(vec![4; 32])),
            author_actor_id: Some(ByteBuf::from(signer.actor_id().0.to_vec())),
            change_type: Some("create".into()),
            ..Default::default()
        };
        let mut row = it.as_change_row().expect("statement");
        ChangeSigner::direct(signer)
            .sign_row(&mut row, NONCE)
            .expect("signs");
        it.signature = row.signature;
        it.signer_key = row.signer_key;
        it.author_actor_id = Some(ByteBuf::from(served_author.actor_id().0.to_vec()));
        it
    }

    fn page(items: Vec<fauna_protocol::media::MediaItem>) -> fauna_protocol::media::MediaListReply {
        fauna_protocol::media::MediaListReply {
            items,
            ..Default::default()
        }
    }

    const LOOKUP: &str = fauna_protocol::RpcError::SUCCESSION_LOOKUP_KIND;

    /// The regression this ruling repairs: a successor's inherited Media
    /// listing. The predecessor's item — served with the successor as author —
    /// is admitted on a seat whose attested set names the predecessor, is
    /// attributed to the successor, and is NOT marked signed-as-current; the
    /// successor's own item is. No link lookup runs: the attested set
    /// answered.
    #[test]
    fn a_successors_inherited_media_lists_and_carries_who_signed_it() {
        let (pred, succ) = (kp(1), kp(2));
        let nest =
            RejectingRequester::new().reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]));
        let seat = ReaderSeat {
            own: Some(succ.actor_id().0),
            nonces: Some(nonces()),
            predecessors: vec![pred.actor_id().0],
            ..Default::default()
        };
        let listing = page(vec![
            media_item("inherited.jpg", &pred, &succ),
            media_item("mine.jpg", &succ, &succ),
        ]);
        let (listing, tally) =
            block_on(judge_media_listing(&nest, &seat, listing, "test")).expect("judged");
        assert_eq!(tally.verified, 2, "both are the account's own");
        let facts: Vec<(&str, bool, Option<&[u8]>)> = listing
            .items
            .iter()
            .map(|i| {
                (
                    i.path.as_str(),
                    i.signed_as_current,
                    i.author_actor_id.as_ref().map(|a| &a[..]),
                )
            })
            .collect();
        let successor = succ.actor_id().0;
        assert_eq!(
            facts,
            [
                ("inherited.jpg", false, Some(&successor[..])),
                ("mine.jpg", true, Some(&successor[..])),
            ]
        );
        assert_eq!(nest.kinds(), [KIND_FOLDERS_LIST]);

        // The same listing on a seat that proved nothing: the predecessor's
        // item is absent (the lookup is refused by this nest, so nothing is
        // learned), the successor's own stays.
        let linkless = ReaderSeat {
            own: Some(succ.actor_id().0),
            nonces: Some(nonces()),
            ..Default::default()
        };
        let nest = RejectingRequester::new()
            .reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]))
            .reject(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS,
                RpcError::new(ROSTER_NOT_SHARED, "error.x"),
            );
        let listing = page(vec![
            media_item("inherited.jpg", &pred, &succ),
            media_item("mine.jpg", &succ, &succ),
        ]);
        let (listing, tally) =
            block_on(judge_media_listing(&nest, &linkless, listing, "test")).expect("judged");
        let paths: Vec<&str> = listing.items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["mine.jpg"]);
        assert_eq!((tally.verified, tally.refused), (1, 1));
    }

    /// A seat handed no attested set proves the link itself: the statement
    /// walk ending at its own id, over the succession lookup. The proof is the
    /// successor's own `new_sig`; a stranger's row in the same listing stays
    /// refused, and neither id is asked about twice.
    #[test]
    fn a_seat_handed_no_predecessors_proves_the_link_by_the_statement_walk() {
        let (p0, pred, succ, stranger) = (kp(1), kp(2), kp(3), kp(9));
        let nest = RejectingRequester::new()
            .reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]))
            .reject(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS,
                RpcError::new(ROSTER_NOT_SHARED, "error.x"),
            )
            .reply(
                LOOKUP,
                &fauna_protocol::recovery::SuccessionLookupReply {
                    statements: vec![link(&p0, &pred, &pred), link(&pred, &succ, &succ)],
                    ..Default::default()
                },
            );
        let seat = ReaderSeat {
            own: Some(succ.actor_id().0),
            nonces: Some(nonces()),
            ..Default::default()
        };
        let listing = || {
            page(vec![
                media_item("oldest.jpg", &p0, &succ),
                media_item("planted.jpg", &stranger, &succ),
            ])
        };
        let (judged, tally) =
            block_on(judge_media_listing(&nest, &seat, listing(), "test")).expect("judged");
        let paths: Vec<&str> = judged.items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["oldest.jpg"], "the two-hop chain admits P0's item");
        assert!(!judged.items[0].signed_as_current);
        assert_eq!((tally.verified, tally.refused), (1, 1));
        assert_eq!(
            seat.learned.proven(),
            vec![pred.actor_id().0, p0.actor_id().0],
            "nearest hop first"
        );
        let lookups =
            |nest: &RejectingRequester| nest.kinds().iter().filter(|k| **k == LOOKUP).count();
        assert_eq!(lookups(&nest), 2, "one per unplaced signed actor");

        // The next listing asks nothing: the link is remembered, and so is
        // the id the walk proved nothing for.
        let (judged, _) =
            block_on(judge_media_listing(&nest, &seat, listing(), "test")).expect("judged");
        assert_eq!(judged.items.len(), 1);
        assert_eq!(lookups(&nest), 2);
    }

    /// The nest cannot mint the link: a statement naming the seat as successor
    /// that another key signed proves nothing, so the row stays absent.
    #[test]
    fn a_forged_link_admits_nothing() {
        let (pred, succ, forger) = (kp(1), kp(2), kp(9));
        let nest = RejectingRequester::new()
            .reply(KIND_FOLDERS_LIST, &folders(vec![own_set("photos")]))
            .reject(
                KIND_FOLDERS_MEMBERS_LIST_ACTORS,
                RpcError::new(ROSTER_NOT_SHARED, "error.x"),
            )
            .reply(
                LOOKUP,
                &fauna_protocol::recovery::SuccessionLookupReply {
                    statements: vec![link(&pred, &succ, &forger)],
                    ..Default::default()
                },
            );
        let seat = ReaderSeat {
            own: Some(succ.actor_id().0),
            nonces: Some(nonces()),
            ..Default::default()
        };
        let listing = page(vec![media_item("planted.jpg", &pred, &succ)]);
        let (judged, tally) =
            block_on(judge_media_listing(&nest, &seat, listing, "test")).expect("judged");
        assert!(judged.items.is_empty());
        assert_eq!(tally.refused, 1);
        assert!(seat.learned.proven().is_empty());
    }

    /// `writer-signed-change-records.md` ruling (11)(c): a history row is a
    /// version of its path, never the item — the version listing keeps it,
    /// rendered under the identity it was signed as, and a Media listing drops
    /// it; both count it.
    #[test]
    fn a_history_row_is_listed_as_a_version_and_never_as_the_item() {
        let (pred, succ) = (kp(1), kp(2));
        let history = RowVerdict::History {
            writer: succ.actor_id().0,
            signed_as: pred.actor_id().0,
            origin: fauna_core::encoding::AuthoringOrigin::Direct,
            nonce: OTHER_NONCE,
        };
        assert_eq!(verified_signer(&history), Some(pred.actor_id().0));
        let (versions, tally) = retain_judged(vec![("v1", history.clone())], "versions");
        assert_eq!((versions, tally.history), (vec!["v1"], 1));
        let (items, tally) = retain_admitted(vec!["item"], &[history], "media");
        assert!(items.is_empty());
        assert_eq!(tally.history, 1);
    }
}
