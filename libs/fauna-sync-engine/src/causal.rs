//! Causal-watermark primitives — the 2026-08-02 ruling's shared vocabulary,
//! extended by the 2026-08-03 gap ruling (edit-frontier, content rung,
//! covering-resolution adoption).
//!
//! Binding prose: `conflicts.md` § Concurrent resolution & ancestor freshness
//! (split out of `file-sync.md` § Conflicts on 2026-08-02; dated decision
//! records live in the internal plans tree); executable model:
//! `crate::merge_convergence_test` (the causal section). Every host stamps
//! and judges causality through these types, so the licence can never fork
//! between them.

/// The `derived_through`/`is_resolution` pair a writer stamps onto a recorded
/// change (`fauna_protocol::sync::SyncChange::derived_through` carries the
/// full wire contract).
///
/// **The lower-bound law:** `derived_through` must never exceed the set seq
/// through which the writer has genuinely incorporated *every* row at the
/// moment the content was produced — stamp the persisted catch-up anchor,
/// never `max(anchor, some row seen out of order)`. Over-claiming recreates
/// the lost-edit defect the watermark exists to close; under-claiming costs at
/// most one extra idempotent merge round.
///
/// **`is_resolution` means "carries no novel content"** (widened by the
/// 2026-08-03 gap ruling, `conflicts.md` § Concurrent resolution): true for a
/// pure resolution (auto-resolve merge result / propagated winner) AND for a
/// **proven reissue** — a re-upload of bytes whose change record provably
/// landed (the re-seal migration holding its `recorded_content_hash` proof).
/// A re-upload WITHOUT that proof (the lost-ack retry) must keep the edit
/// stamp: its record may never have landed, in which case its bytes are the
/// only carrier of a genuine user edit and a no-novel-content stamp would let
/// receivers skip — and lose — it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CausalStamp {
    pub derived_through: Option<i64>,
    pub is_resolution: Option<bool>,
}

impl CausalStamp {
    /// Honestly-unknown causality — the row will be read exactly like a
    /// pre-ruling row (the `local == base` heuristic). The right stamp for
    /// writers that hold no catch-up anchor.
    pub fn unknown() -> Self {
        Self::default()
    }

    /// A fresh user edit derived from everything up to `anchor`.
    pub fn edit(anchor: i64) -> Self {
        Self {
            derived_through: Some(anchor),
            is_resolution: None,
        }
    }

    /// A no-novel-content row (a proven reissue, a re-assert) derived from
    /// everything up to `anchor` — receivers whose edit-frontier is past
    /// `anchor` skip it as information-free. Production stamp sites pass the
    /// FLOOR-REDUCED anchor ([`CausalStore::honest_anchor`]), never the raw
    /// one: the lower-bound law binds this stamp exactly as it binds an edit
    /// (ruled 2026-09-20).
    pub fn resolution(anchor: i64) -> Self {
        Self {
            derived_through: Some(anchor),
            is_resolution: Some(true),
        }
    }
}

/// The per-path receiver state the licence reads. Both hosts' stores serve
/// this pair; the model's `CSeat` mirrors it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PathFrontiers {
    /// The newest seq this path's local content reflects, authored or applied
    /// — rows of ANY kind. `None` when the device tracks no frontier for the
    /// path yet (pre-ruling licencing throughout).
    pub frontier: Option<i64>,
    /// The newest seq carrying NOVEL content that local content reflects —
    /// advanced only by non-resolution rows actually applied, merged, or
    /// authored; never by resolutions, and never by skipped reissues.
    ///
    /// **The upper-bound law (the watermark's mirror):** never UNDER-state
    /// the novel content your local reflects — an under-count licences
    /// adopting a resolution that misses content you hold (a regression); an
    /// over-count only strands a covering resolution (the pre-ruling
    /// behaviour). When in doubt, advance it. `None` = untracked → read as
    /// equal to `frontier`, which is exactly the pre-gap-ruling semantics.
    pub edit_frontier: Option<i64>,
    /// The CONTENT frontier (`conflicts.md` § *Retention rows are transparent
    /// to the licence*, 2026-09-27) — the newest seq whose content local
    /// reflects: the frontier minus retention rows. Advanced wherever the
    /// frontier is EXCEPT by the accounting of a retention row
    /// ([`CausalStore::account_retention_row`], the one funnel), because a
    /// retention row is reflected by no receiver — the retention rung never
    /// fetches, applies, merges or ledgers it. Read by rule 4's fast-forward
    /// conjunct only; rule 1's duplicate guard, rule 5's reissue rung and
    /// `honest_winner_claim`'s contiguity keep the full frontier. `None` =
    /// untracked (lost or never-stamped state) → read as equal to `frontier`.
    pub content_frontier: Option<i64>,
}

impl PathFrontiers {
    /// The effective edit-frontier: untracked degrades to the full frontier
    /// (rule 2 then skips exactly what it skipped before the gap ruling).
    pub fn effective_edit_frontier(&self) -> Option<i64> {
        self.edit_frontier.or(self.frontier)
    }

    /// The effective content frontier: untracked degrades to the full
    /// frontier (rule 4 then licenses exactly what it licensed before
    /// retention rows became transparent).
    pub fn effective_content_frontier(&self) -> Option<i64> {
        self.content_frontier.or(self.frontier)
    }
}

/// What the causal licence says to do with an incoming row — the shared
/// decision both hosts' apply paths consult (the model's `causal_apply`,
/// production-shaped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncomingVerdict {
    /// `seq <= frontier`: already reflected in local content — a duplicate
    /// delivery. Never re-apply, never re-merge (the idempotence guard: a
    /// re-merge from an old ancestor yields overlapping-identical hunks →
    /// unmergeable → latest-wins destroys content).
    Duplicate,
    /// A resolution whose watermark is below the EDIT-frontier: it misses
    /// novel content this device reflects. Skip; account for the row (advance
    /// the frontier — not the edit-frontier — past its seq).
    StaleResolution,
    /// A stale-watermarked row whose exact content this device already holds
    /// at a seq ≤ its frontier — a reissue (re-seal re-upload,
    /// lost-ack retry whose record landed, duplicate
    /// content re-delivery). Skip; advance the frontier only — the bytes
    /// carry no novel content, so the edit-frontier must NOT advance even
    /// when the row wears an edit stamp.
    ReissueOfHeldContent,
    /// The writer provably incorporated every NOVEL row this device's local
    /// content reflects, and there is no unpublished local work — apply
    /// verbatim. For an edit row the watermark must dominate the full
    /// frontier; for a resolution row dominating the edit-frontier suffices
    /// (the rows between the two are resolutions — order-choices this LATER
    /// row supersedes by nest-log order, `conflicts.md` clause 1 applied to
    /// clause 5).
    FastForward,
    /// Divergence: unpublished local work, or a concurrent sibling. Route to
    /// the resolver, with the ancestor causally justified when the watermark
    /// is present (`ancestor_seq_bound`).
    Diverged {
        /// `Some(w)`: the merge ancestor must be the newest version this
        /// device holds with seq ≤ w (a version the incoming row's writer
        /// provably had). `None`: no watermark — fall back to the live base
        /// slot exactly as before the ruling.
        ancestor_seq_bound: Option<i64>,
    },
    /// A RETENTION row (the loser-row ruling, 2026-08-05 — `conflicts.md` §
    /// Concurrent resolution & ancestor freshness, the gap-3 decision
    /// record's remainder): the resolved report's transactional
    /// loser-retention vehicle, minted by the nest so the losing candidate
    /// stays listable and GC-pinned. **Invisible to content**: account its
    /// seq into the path FRONTIER and do nothing else — never fetch, apply,
    /// merge, or adopt it; never advance the edit-frontier (its bytes are a
    /// pre-merge candidate, not novel work); never enter it in the ledger
    /// (it was no one's reflected content, so it can never be a correct
    /// common ancestor). Clause 1's `use_other_version` re-points among
    /// retained candidates via a NEW head row — peers never apply retention
    /// rows. Old receivers ignore the marker and degrade to today's
    /// behaviour (they merge it — the duplication/lost-edit defect this
    /// verdict closes).
    RetentionRow,
}

/// The verdicts reachable from the row's stamp ALONE, before the receiver has
/// read (or downloaded) the local file — see [`judge_incoming_before_fetch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreFetchVerdict {
    /// [`IncomingVerdict::Duplicate`], decided without the local bytes.
    Duplicate,
    /// [`IncomingVerdict::StaleResolution`], decided without the local bytes.
    StaleResolution,
    /// [`IncomingVerdict::RetentionRow`], decided without the local bytes.
    RetentionRow,
    /// Undecidable from the stamp alone: read the local file and finish with
    /// [`judge_incoming`].
    NeedsLocalBytes,
}

/// The receiver's half of the watermark's lower-bound law (`conflicts.md`
/// clause 5, the watermark-bound ruling, 2026-09-21): read a row's
/// `derived_through` as `min(w, seq − 1)`.
///
/// `derived_through` is the WRITER's word — the nest stores it as sent and
/// assigns the row's seq afterwards — so "stamp the persisted catch-up anchor,
/// never more" binds an honest writer only. The log itself refutes any claim
/// at or above the row's own seq: a row cannot have incorporated itself, nor
/// anything recorded after it. An honest anchor is always below the seq the
/// nest assigns later, so the bound never moves an honest verdict; a forged
/// one is judged exactly as the most-caught-up honest writer's row would be.
/// Unbounded, one lying field bought a stale row a fast-forward past this
/// device's counted own row, which the leg-4 DEFER cap then held the anchor
/// under for ever — the released fresh-bind park, reinstated by any writer
/// member.
///
/// Both public judges apply it, and both hosts apply it where a row enters
/// (the listing, the real-time forward), so no receiver read — licence,
/// stale-skip, fold gate, merge ancestor — ever sees the raw claim. A claim
/// inside `[honest anchor, seq − 1]` stays unfalsifiable from the log; what it
/// buys is a latest-wins over retained versions, never a park.
pub fn bounded_watermark(seq: i64, derived_through: Option<i64>) -> Option<i64> {
    derived_through.map(|w| w.min(seq.saturating_sub(1)))
}

/// The byte-free half of [`judge_incoming`] — the verdicts a receiver may act
/// on *before* paying for a download.
///
/// Both hosts skip duplicates and stale resolutions ahead of the fetch, and
/// both used to do it by calling `judge_incoming` with a hardcoded
/// `local_matches_base: true` and a comment promising the argument could not
/// matter for those two verdicts. That promise was true but unenforced: it
/// lived in a comment, duplicated in two crates, and any reordering inside
/// `judge_incoming` would have quietly turned both call sites into
/// wrong-answer machines. Here the guarantee is structural — this function
/// cannot read the local bytes because it is not given them, and
/// `judge_incoming` is defined in terms of it, so the two can never disagree.
pub fn judge_incoming_before_fetch(
    seq: i64,
    derived_through: Option<i64>,
    is_resolution: bool,
    is_retention: bool,
    frontiers: PathFrontiers,
) -> PreFetchVerdict {
    // The receiver's half of the lower-bound law — HERE as well as in
    // `judge_incoming`, because both hosts act on these byte-free verdicts
    // before the fetch: bounded in the full judge alone, a forged resolution
    // would merge where its honest sibling is stale-skipped.
    let derived_through = bounded_watermark(seq, derived_through);
    if let Some(f) = frontiers.frontier
        && seq <= f
    {
        return PreFetchVerdict::Duplicate;
    }
    // The retention rung (loser-row ruling, 2026-08-05): unconditional and
    // byte-free — see [`IncomingVerdict::RetentionRow`]. Deliberately OUTSIDE
    // the tracked-frontier gate: a receiver with no frontier yet must still
    // never merge a retention row (accounting it is what starts the
    // frontier). It lives in this shared core, not at call sites, so neither
    // host can forget it and re-open the duplication engine.
    if is_retention {
        return PreFetchVerdict::RetentionRow;
    }
    if let Some(f) = frontiers.frontier {
        debug_assert!(seq > f, "the duplicate rung above already returned");
        // Stale = misses novel content this device reflects. Testing the
        // EDIT-frontier (not the full frontier) is the 2026-08-03 gap-2
        // ruling: a resolution covering every edit but not every RESOLUTION
        // is a later order-choice, not stale — it proceeds, and the
        // local-bytes judgement adopts or merges it.
        if is_resolution
            && let Some(ef) = frontiers.effective_edit_frontier()
            && derived_through.is_some_and(|w| w < ef)
        {
            return PreFetchVerdict::StaleResolution;
        }
    }
    PreFetchVerdict::NeedsLocalBytes
}

/// The content-keyed idempotence rung (2026-08-03 gap-1 ruling): a
/// stale-watermarked row whose exact content the receiver already holds is a
/// REISSUE when it would otherwise MERGE — skip it instead of re-merging it
/// from a stale ancestor (overlapping-identical hunks → latest-wins → a
/// peer's edit destroyed).
///
/// **This predicate guards [`judge_incoming`]'s merge arm only — it never
/// preempts a fast-forward** (a ledger hold proves the bytes were SEEN, not
/// incorporated: rule-2 skips hold too, so a covering resolution's bytes can
/// match an earlier identical publication the receiver never adopted). Hosts
/// therefore consult it through [`judge_incoming`], not directly.
///
/// `content_already_held` = the incoming row's content hash equals a ledger
/// entry held at seq ≤ the path frontier (caller-computed; hosts compare the
/// manifest's `file_hash` against hashes of held ledger bytes — manifest
/// hashes cannot be compared, a re-seal mints a new manifest for the same
/// bytes). False negatives are safe: the row just proceeds to the ordinary
/// judgement and at worst merges idempotently.
///
/// The rung deliberately fires only on `w < frontier`: a CAUGHT-UP writer
/// re-publishing held bytes is a genuine revert (fresh intent) and must
/// proceed — only a stale-watermarked reissue is indistinguishable from the
/// retry/re-seal class, and there the peer-protecting skip wins (the ruling's
/// recorded trade: a stale-anchored revert loses to a peer's newer concurrent
/// edit, instead of destroying it via latest-wins).
pub fn is_reissue_of_held_content(
    derived_through: Option<i64>,
    frontiers: PathFrontiers,
    content_already_held: bool,
) -> bool {
    content_already_held
        && matches!((derived_through, frontiers.frontier), (Some(w), Some(f)) if w < f)
}

/// The leg-4 DEFER cap's FIRING RULE (`conflicts.md` clause 5, conjunct (1);
/// the held-bytes release ruled 2026-09-21) — one implementation, called by
/// both hosts' apply paths and the model oracle.
///
/// A verbatim adopt is held while this device's own pending rows are
/// unlisted: the unknown-seq report window (`own_novelty_in_flight`) and the
/// ordinary ack→echo window, where the fast-forward licence rode the recorded
/// witness alone (`!local_matches_live_base`). The window exists to protect
/// own NOVEL bytes — content the log would lack if the adopt dropped it before
/// its carrier listed, whose echo then falsely advances the frontiers. Bytes
/// the ledger ALREADY HOLDS at or below the frontier
/// (`local_held_at_or_below_frontier`: a revert to a held version — the
/// widened proven-reissue proof stamps its row resolution-class, and neither
/// frontier counts it) protect nothing the log lacks, so the cap does not
/// hold for them: holding did park the anchor below the row's own echo for
/// ever (measured at the oracle). The covering peer edit is adopted and the
/// revert's echo, a stale-declined own tail, re-asserts the current bytes —
/// the reverter loses its stale revert exactly as peers skip it (rule 5's
/// ratified trade).
pub fn verbatim_adopt_deferred(
    own_novelty_in_flight: bool,
    local_matches_live_base: bool,
    local_held_at_or_below_frontier: bool,
) -> bool {
    own_novelty_in_flight || (!local_matches_live_base && !local_held_at_or_below_frontier)
}

/// Judge one incoming row against this device's per-path causal state.
///
/// * `seq` — the incoming row's set seq.
/// * `derived_through` / `is_resolution` — the row's stamp (absent = unknown
///   = pre-ruling reader semantics).
/// * `frontiers` — the path's causal state ([`PathFrontiers`]).
/// * `content_already_held` — the row's content hash matches a ledger entry
///   at seq ≤ the frontier (see [`is_reissue_of_held_content`]); pass `false`
///   when unknown — the degrade is one idempotent merge, never a loss.
/// * `local_matches_base` — the unpublished-divergence guard, unchanged from
///   the pre-ruling licence.
///
/// ⚠ The `frontiers.edit_frontier` a CALLER passes must obey the upper-bound
/// law for the device's OWN work too: own published-but-unechoed
/// NON-resolution rows bind novelty at seqs the stored edit-frontier has not
/// counted yet (both hosts partition own echoes to the end of a batch, so a
/// same-batch covering resolution is otherwise judged before the own edit
/// echo advances it — a covering adopt then destroys the un-counted edit;
/// measured live, 3-seat cell, 2026-08-03). Callers therefore fold every
/// known in-flight own non-resolution seq into the edit-frontier they pass,
/// and DEFER a would-be covering adopt while an own publication with an
/// unknown seq is in flight.
///
/// With no watermark on the row, the verdict degrades to exactly the
/// pre-ruling behaviour: fast-forward on `local == base`, else diverged with
/// the base-slot ancestor. (The duplicate guard still applies when a frontier
/// is tracked — a seq the content already reflects is a duplicate regardless
/// of what the row claims.)
pub fn judge_incoming(
    seq: i64,
    derived_through: Option<i64>,
    is_resolution: bool,
    is_retention: bool,
    frontiers: PathFrontiers,
    content_already_held: bool,
    local_matches_base: bool,
) -> IncomingVerdict {
    // Every arm below — the licence's conjuncts, the reissue rung, the merge
    // ancestor's bound — reads the BOUNDED claim ([`bounded_watermark`]).
    let derived_through = bounded_watermark(seq, derived_through);
    match judge_incoming_before_fetch(seq, derived_through, is_resolution, is_retention, frontiers)
    {
        PreFetchVerdict::Duplicate => return IncomingVerdict::Duplicate,
        PreFetchVerdict::StaleResolution => return IncomingVerdict::StaleResolution,
        PreFetchVerdict::RetentionRow => return IncomingVerdict::RetentionRow,
        PreFetchVerdict::NeedsLocalBytes => {}
    }
    match (derived_through, frontiers.frontier) {
        (Some(w), Some(f)) => {
            // Receiver-side CLASS UPGRADE (gap-3 ruling): `is_resolution`
            // means "carries no novel content", and a row whose exact bytes
            // this receiver already holds at seq ≤ its frontier is PROVEN
            // novel-content-free — whatever stamp it wears. The writer of a
            // lost-ack retry cannot stamp it (its record may never have
            // landed — gap 1's ruling stands), but the receiver holding the
            // bytes has the proof the writer lacked, so the row is judged by
            // the resolution licences: rule-2/3 supersession by nest-log
            // order applies to it, which is what converges the same-anchor
            // fold once a reissue enters the schedule (an un-upgraded
            // reissue adopted at some receivers — caught-up fast-forward —
            // and skipped at others, an order-dependence that IS divergence).
            let effective_resolution = is_resolution || content_already_held;
            // The fast-forward licence's watermark conjunct: an edit must
            // dominate the CONTENT frontier (the full frontier minus
            // retention rows, which no receiver reflects — `conflicts.md` §
            // *Retention rows are transparent to the licence*) AND the
            // effective edit-frontier — a
            // caller-folded in-flight own edit can sit ABOVE the frontier,
            // and "the writer incorporated everything this device reflects"
            // must cover it (measured: a reupload with w == frontier
            // fast-forwarded over the receiver's own in-flight edit). A
            // resolution — stamped or upgraded — need only dominate the
            // edit-frontier (gap-2 ruling — everything between the two is
            // order-choices this later row supersedes by nest-log order).
            let watermark_dominates = if effective_resolution {
                frontiers
                    .effective_edit_frontier()
                    .is_some_and(|ef| w >= ef)
            } else {
                let content_frontier = frontiers.effective_content_frontier().unwrap_or(f);
                w >= content_frontier
                    && frontiers.effective_edit_frontier().is_none_or(|ef| w >= ef)
            };
            if local_matches_base && watermark_dominates {
                IncomingVerdict::FastForward
            } else if is_reissue_of_held_content(derived_through, frontiers, content_already_held) {
                // The rung guards the MERGE arm only — it must never preempt
                // an adopt. A ledger hold is proof the bytes were SEEN, not
                // that they were incorporated (rule-2 skips hold too), so
                // skipping an adoptable covering resolution here would strand
                // the very supersession the gap-2 ruling built (measured: the
                // model's three-seat append run re-diverged when the rung ran
                // ahead of the licence, because the log-tail permutation's
                // bytes matched an earlier seat's identical publication).
                IncomingVerdict::ReissueOfHeldContent
            } else {
                IncomingVerdict::Diverged {
                    ancestor_seq_bound: Some(w),
                }
            }
        }
        // No frontier tracked yet: any watermarked row may fast-forward when
        // local == base (nothing local to protect beyond the base check), and
        // a watermark still bounds the merge ancestor on divergence —
        // PROVIDED the watermark dominates the effective edit-frontier, the
        // same conjunct the tracked arm demands (the cap-release ruling,
        // 2026-09-21, `conflicts.md` clause 5). An untracked path can still
        // carry a COUNTED own row: the record ack (engine) and the listing
        // pre-pass (both hosts) advance the edit-frontier to this device's
        // own just-recorded seq ahead of the frontier, which only its echo
        // advances. A row whose watermark sits below that count never saw
        // the own row, so it is a SIBLING — merge, never adopt. Reading it as
        // a fast-forward here handed it to the leg-4 DEFER cap, and the cap
        // held the anchor BELOW the very own row whose echo releases it: the
        // fresh-bind park (measured 2026-09-20, tier_1 and live).
        (Some(w), None) => {
            let dominates_counted_novelty =
                frontiers.effective_edit_frontier().is_none_or(|ef| w >= ef);
            if local_matches_base && dominates_counted_novelty {
                IncomingVerdict::FastForward
            } else {
                IncomingVerdict::Diverged {
                    ancestor_seq_bound: Some(w),
                }
            }
        }
        // No watermark: the pre-ruling licence, verbatim — behind the same
        // log-order floor (2026-09-21): a row at or below the effective
        // edit-frontier was recorded BEFORE the own row counted there, so it
        // cannot have incorporated it, whatever it would have claimed. The
        // tracked-frontier duplicate rung above already covers seq ≤ f; this
        // bites only in the ack→echo window, where ef sits above f.
        (None, _) => {
            let predates_counted_novelty = frontiers
                .effective_edit_frontier()
                .is_some_and(|ef| seq <= ef);
            if local_matches_base && !predates_counted_novelty {
                IncomingVerdict::FastForward
            } else {
                IncomingVerdict::Diverged {
                    ancestor_seq_bound: None,
                }
            }
        }
    }
}

/// Dir-backed per-path causal state — the FRONTIER (newest seq local content
/// reflects) and the LEDGER of held row versions (the causally-justified merge
/// ancestors). One flat directory, hashed filenames (no user-chosen path ever
/// rests as a name — the same confidentiality rule the merge-base cache
/// follows). Both hosts share this store type so the semantics cannot fork;
/// entries are re-derivable caches (a loss costs a merge, never data).
///
/// ⚠ **The directory is the set scope.** Every filename here is keyed by
/// `path_hash` alone, with no set component, and `seq` is **per set** — so one
/// directory shared by two sets is not a namespace collision but a wrong
/// answer: the frontier only grows, and rule 1 skips `seq ≤ frontier` as a
/// duplicate byte-free. The scope therefore lives one level up, in the
/// directory the store is rooted at ([`scoped_store_dir`]).
pub struct CausalStore {
    dir: std::path::PathBuf,
}

// ─────────────────────────────────────────────────────────────────────
// The per-(device, set) state directory — where the set scope lives
// ─────────────────────────────────────────────────────────────────────

/// The identity of one host's per-`(device, set)` state directory:
/// `BLAKE3(device_id \0 folder)`, hex.
///
/// It *names* the directory ([`scoped_store_dir`]). The folder name never
/// appears: a user-chosen name must not rest on disk or ride into a log line, the same
/// rule every filename in [`CausalStore`] is hashed for.
pub fn store_owner_digest(device_id_hex: &str, folder: &str) -> String {
    hex::encode(blake3::hash(format!("{device_id_hex}\0{folder}").as_bytes()).as_bytes())
}

/// A per-`(device, set)` subdirectory of `root` — **a construction, not a
/// gate**: with each binding resolving to its own directory, a cross-set
/// collision is unrepresentable rather than merely detected.
pub fn scoped_store_dir(
    root: &std::path::Path,
    device_id_hex: &str,
    folder: &str,
) -> std::path::PathBuf {
    // 32 hex chars (128 bits) — collision-free at any plausible binding count,
    // and short enough to keep the path readable on Windows.
    root.join(&store_owner_digest(device_id_hex, folder)[..32])
}

/// Resolve the per-`(device, set)` state directory under `root`, creating it
/// if absent.
///
/// Nothing is carried in from `root`'s own flat entries: the pre-scoping flat
/// layout predates the compat-remnant sweep, so no directory holding one
/// exists (`../architecture/version-compatibility.md` § Dimension 2, program
/// 4). A failure to create the scope is logged, never fatal: the store's own
/// writes then fail as a cold cache would, which costs merges, never data.
pub fn open_scoped_store_dir(
    root: &std::path::Path,
    device_id_hex: &str,
    folder: &str,
) -> std::path::PathBuf {
    let scoped = scoped_store_dir(root, device_id_hex, folder);
    create_scoped_store_dir(&scoped);
    scoped
}

/// `create_dir_all` the scope, logging a failure; `false` when it failed.
fn create_scoped_store_dir(scoped: &std::path::Path) -> bool {
    match std::fs::create_dir_all(scoped) {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(
                dir = %fauna_core::log_redact::log_path(&scoped.to_string_lossy()),
                error = %e,
                "could not create the per-set state directory"
            );
            false
        }
    }
}

/// Only content a three-way merge could read as an ancestor is worth holding —
/// nothing above this merges as text, and binaries fall to latest-wins with no
/// ancestor at all.
pub const LEDGER_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Held versions per path (newest kept).
pub const LEDGER_KEEP: usize = 8;

/// Written into a per-path floor file, beside the new seq, when a skip is
/// recorded over a floor that could not be read. It is not a seq, so every
/// reader parses the file as [`FloorRead::Unreadable`] — the loss stays on
/// record instead of being overwritten by a readable value that forgets it.
const LOST_FLOOR_MARKER: &str = "lost";

/// What a skip-floor file says — three states, because two of them used to
/// collapse into "no floor" and that fold IS an over-claim.
///
/// A floor file that cannot be read or parsed is **not** the absence of a
/// skip: the file exists only because [`CausalStore::note_permanent_skip`]
/// wrote it, so unreadability means *a skip was recorded here and its seq is
/// lost* — the one thing a claim must never round down to "nothing was
/// skipped". The write side has always degraded toward OVER-claiming and said
/// so loudly; the read side silently did the same until 2026-09-22
/// .
enum FloorRead {
    /// No file — no skip has ever been recorded here. The only honest "no
    /// reduction" answer.
    Absent,
    /// The seqs permanently skipped under this key, ascending and
    /// deduplicated — never empty. A per-path key holds every skip recorded
    /// above its path's frontier, not only the lowest ever: the reduction
    /// reads the lowest one still LIVE, so a skip recorded after an earlier
    /// one released — or one left standing when a release passed a lower
    /// skip — is still on record . The set-wide floor never releases, so there the lowest seq is
    /// exact and it keeps only that one.
    Floor(Vec<i64>),
    /// The file exists but its value could not be recovered — or it carries
    /// [`LOST_FLOOR_MARKER`], a per-path record that an earlier value was.
    Unreadable,
}

impl CausalStore {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }

    fn hex_of(relative_path: &str) -> String {
        hex::encode(fauna_core::sync::path_hash(relative_path))
    }
    fn frontier_file(&self, relative_path: &str) -> std::path::PathBuf {
        self.frontier_file_for_key(&fauna_core::sync::path_hash(relative_path))
    }
    /// The frontier file under a DECODED `path_hash` — what the floor write
    /// reads, holding a wire key rather than a path.
    fn frontier_file_for_key(&self, path_hash: &[u8; 32]) -> std::path::PathBuf {
        self.dir
            .join(format!("frontier-{}", hex::encode(path_hash)))
    }
    fn edit_frontier_file(&self, relative_path: &str) -> std::path::PathBuf {
        self.dir
            .join(format!("editfrontier-{}", Self::hex_of(relative_path)))
    }
    fn content_frontier_file(&self, relative_path: &str) -> std::path::PathBuf {
        self.dir
            .join(format!("contentfrontier-{}", Self::hex_of(relative_path)))
    }
    fn ledger_file(&self, relative_path: &str, seq: i64) -> std::path::PathBuf {
        self.dir
            .join(format!("ledger-{}-{seq:020}", Self::hex_of(relative_path)))
    }
    fn ledger_index_file(&self, relative_path: &str) -> std::path::PathBuf {
        self.dir
            .join(format!("ledgeridx-{}", Self::hex_of(relative_path)))
    }
    fn manifest_memory_file(&self, relative_path: &str) -> std::path::PathBuf {
        self.dir
            .join(format!("ledgerman-{}", Self::hex_of(relative_path)))
    }

    /// The path's frontier; `None` = never stamped (pre-ruling licencing).
    pub fn frontier(&self, relative_path: &str) -> Option<i64> {
        std::fs::read_to_string(self.frontier_file(relative_path))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// Advance the frontier (monotonic) — and the content frontier with it,
    /// when tracked (an untracked one already reads as the frontier). Every
    /// accounting site but one calls this; a RETENTION row's goes through
    /// [`Self::account_retention_row`] instead. Best-effort: a failed write
    /// costs a merge later — the licence degrades toward MORE merging, never
    /// less (a content frontier left behind only narrows rule 4).
    pub fn advance_frontier(&self, relative_path: &str, seq: i64) {
        if self.content_frontier(relative_path).is_some() {
            self.write_content_frontier(relative_path, seq);
        }
        self.advance_seen_frontier(relative_path, seq);
    }

    /// The ONE funnel for a retention row's accounting (`conflicts.md` §
    /// *Retention rows are transparent to the licence*): the frontier
    /// advances — the row still counts as seen for rule 1, rule 5 and
    /// `honest_winner_claim`'s contiguity — and the content frontier does
    /// NOT, because no receiver's local reflects a retention row's bytes.
    ///
    /// An untracked content frontier would silently follow the frontier past
    /// the row, so it is first pinned at the frontier the path had BEFORE this
    /// row — exactly the value it read as until now. The edit-frontier gets
    /// the same treatment for the same reason: a retention row never advances
    /// it ([`IncomingVerdict::RetentionRow`]), and its untracked fallback
    /// would do so behind the rule's back. Both pins precede the frontier
    /// write, so a crash between them leaves a correct pair. An untracked
    /// FRONTIER pins nothing: this row starts it, and what local held before
    /// tracking began is unknowable here (pinning 0 would under-state the
    /// edit-frontier, the regressive direction).
    pub fn account_retention_row(&self, relative_path: &str, seq: i64) {
        if let Some(before) = self.frontier(relative_path)
            && seq > before
        {
            if self.content_frontier(relative_path).is_none() {
                self.write_content_frontier(relative_path, before);
            }
            if self.edit_frontier(relative_path).is_none() {
                self.advance_edit_frontier(relative_path, before);
            }
        }
        self.advance_seen_frontier(relative_path, seq);
    }

    fn advance_seen_frontier(&self, relative_path: &str, seq: i64) {
        if seq <= self.frontier(relative_path).unwrap_or(0) {
            return;
        }
        let _ = std::fs::create_dir_all(&self.dir);
        if let Err(e) = std::fs::write(self.frontier_file(relative_path), seq.to_string()) {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(relative_path),
                error = %format!("{e:#}"),
                "recording the causal frontier failed — the next incoming row \
                 will be judged by the pre-ruling rules"
            );
        }
    }

    /// The path's content frontier; `None` = untracked (readers degrade it to
    /// the full frontier per [`PathFrontiers::effective_content_frontier`]).
    pub fn content_frontier(&self, relative_path: &str) -> Option<i64> {
        std::fs::read_to_string(self.content_frontier_file(relative_path))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    fn write_content_frontier(&self, relative_path: &str, seq: i64) {
        if self
            .content_frontier(relative_path)
            .is_some_and(|c| seq <= c)
        {
            return;
        }
        let _ = std::fs::create_dir_all(&self.dir);
        if let Err(e) = std::fs::write(self.content_frontier_file(relative_path), seq.to_string()) {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(relative_path),
                error = %format!("{e:#}"),
                "recording the causal content frontier failed — an edit behind \
                 a retention row will merge instead of fast-forwarding"
            );
        }
    }

    /// The path's edit-frontier; `None` = untracked (readers degrade it to
    /// the full frontier per [`PathFrontiers::effective_edit_frontier`] —
    /// the safe, over-counting direction of the upper-bound law).
    pub fn edit_frontier(&self, relative_path: &str) -> Option<i64> {
        std::fs::read_to_string(self.edit_frontier_file(relative_path))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// Advance the edit-frontier (monotonic). Called wherever a
    /// NON-resolution row's bytes actually enter local content (apply, merge,
    /// authored echo) — never for resolutions or skipped reissues. A failed
    /// write degrades toward over-counting (the untracked → full-frontier
    /// fallback), which strands at worst and never regresses.
    pub fn advance_edit_frontier(&self, relative_path: &str, seq: i64) {
        if seq <= self.edit_frontier(relative_path).unwrap_or(0) {
            return;
        }
        let _ = std::fs::create_dir_all(&self.dir);
        if let Err(e) = std::fs::write(self.edit_frontier_file(relative_path), seq.to_string()) {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(relative_path),
                error = %format!("{e:#}"),
                "recording the causal edit-frontier failed — covering \
                 resolutions will be judged by the pre-gap-ruling rules"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // The skip floor — what this device may HONESTLY claim to have read
    // ─────────────────────────────────────────────────────────────────

    /// Where a path's skip floor rests. Keyed by the row's `path_hash`, like
    /// every other per-path entry, so the two producers agree: an applier that
    /// holds the plaintext path passes it, and one refusing a row it could not
    /// even name passes the hash the nest filed the row under.
    ///
    /// Takes the DECODED hash, never a string: this is the one store key that
    /// can arrive from the wire, and only the re-encoding of 32 raw bytes is
    /// guaranteed to name a file inside `dir` — and to be the same lowercase
    /// name [`Self::hex_of`] derives for the readers .
    fn skip_floor_file(&self, path_hash: &[u8; 32]) -> std::path::PathBuf {
        self.dir
            .join(format!("skipfloor-{}", hex::encode(path_hash)))
    }

    /// A nest-served `path_hash` as a per-path floor key: `Some` only when it
    /// is exactly 32 hex-encoded bytes (either case). Anything else is the
    /// malformed shape the set-wide floor exists for.
    fn floor_key_of(path_hash_hex: &str) -> Option<[u8; 32]> {
        hex::decode(path_hash_hex)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    }

    /// The set-wide floor, for the one row shape with no usable per-path key:
    /// a `path_hash` that is not even 32 hex bytes. Pathological (no correct
    /// nest serves one), and deliberately absolute — nothing ever clears it,
    /// so it pins every later claim on this device below the earliest such
    /// skip for good, which is the conservative answer for a record nothing
    /// about can be trusted.
    fn set_skip_floor_file(&self) -> std::path::PathBuf {
        self.dir.join("skipfloor-set")
    }

    /// Read a floor file. **Never folds an error into "no floor"** — the three
    /// answers are distinct and each has its own conservative response; see
    /// [`FloorRead`].
    fn read_floor(path: &std::path::Path) -> FloorRead {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                match s
                    .split_whitespace()
                    .map(str::parse::<i64>)
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(mut seqs) if !seqs.is_empty() => {
                        seqs.sort_unstable();
                        seqs.dedup();
                        FloorRead::Floor(seqs)
                    }
                    // Empty (a truncation), unparseable, or the lost marker.
                    _ => FloorRead::Unreadable,
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FloorRead::Absent,
            Err(_) => FloorRead::Unreadable,
        }
    }

    /// Write a floor value so a crash can never leave a TRUNCATED one behind.
    ///
    /// `std::fs::write` truncates before it writes, so a process death in
    /// between leaves a zero-length file — which [`Self::read_floor`] now
    /// reads as [`FloorRead::Unreadable`], pinning every claim under that key
    /// to its conservative bound. Temp-plus-rename makes that state
    /// unreachable from our own crashes, leaving only genuinely external
    /// damage (a planted directory, EACCES, bad hardware) able to pin
    /// anything. The frontier writes above need no such care: a lost frontier
    /// reads as 0, which only ever over-merges.
    fn write_floor_atomically(&self, file: &std::path::Path, body: &str) -> std::io::Result<()> {
        static TMP_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = TMP_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = file.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, file).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// Record that `seq` was **permanently skipped** — accounted for by the
    /// anchor without its content ever entering this device.
    ///
    /// `path_hash_hex` is the row's own `path_hash` as the nest served it, or
    /// `None` when the row offers no key at all. **This sink decides whether it
    /// is a usable key, not the caller:** only exactly 32 hex-encoded bytes
    /// take the per-path floor; anything else — a wire string that could
    /// otherwise name a file outside the store  —
    /// takes the set-wide floor, the conservative answer for a record nothing
    /// about can be trusted (`conflicts.md` § the causal watermark).
    ///
    /// **A per-path key keeps every LIVE skip, not just the lowest.** The
    /// reduction reads the lowest skip above the path's frontier, and the
    /// frontier only climbs, so a skip at or below it has released for good
    /// and is pruned here; every other recorded skip stays. Keeping one seq
    /// forgot the next skip the moment the lowest one released
    /// . The **set-wide** floor
    /// never releases, so its lowest seq alone is exact: monotone downward.
    ///
    /// Best-effort, and a failed write degrades toward OVER-claiming — the
    /// unsafe direction, unlike the frontier writes above. It is still
    /// best-effort rather than fatal because the alternative is refusing to
    /// account for a row the anchor has already passed, which strands the
    /// device; the failure is logged loudly instead.
    pub fn note_permanent_skip(&self, path_hash_hex: Option<&str>, seq: i64) {
        let key = path_hash_hex.and_then(Self::floor_key_of);
        if path_hash_hex.is_some() && key.is_none() {
            tracing::warn!(
                seq,
                "a skipped row's path_hash is not 32 hex bytes — recording it on \
                 the set-wide floor"
            );
        }
        let body = match &key {
            Some(hash) => self.per_path_floor_after_skip(hash, seq),
            None => self.set_wide_floor_after_skip(seq),
        };
        let Some(body) = body else { return };
        let file = match &key {
            Some(hash) => self.skip_floor_file(hash),
            None => self.set_skip_floor_file(),
        };
        let _ = std::fs::create_dir_all(&self.dir);
        if let Err(e) = self.write_floor_atomically(&file, &body) {
            tracing::error!(
                seq,
                error = %format!("{e:#}"),
                "recording a permanently-skipped seq failed — this device's later \
                 edit stamps may over-claim causality for it"
            );
        }
    }

    /// Whether `seq` is on record as permanently skipped under `path_hash_hex`
    /// — this device's own word that it never read that row. `false` for a
    /// key that is not 32 hex bytes (the set-wide floor names no path) and
    /// for a floor that cannot be read: a reader asking "may I act on having
    /// skipped this?" gets yes only on the record itself.
    pub fn skip_noted(&self, path_hash_hex: &str, seq: i64) -> bool {
        Self::floor_key_of(path_hash_hex).is_some_and(|key| {
            matches!(
                Self::read_floor(&self.skip_floor_file(&key)),
                FloorRead::Floor(seqs) if seqs.contains(&seq)
            )
        })
    }

    /// Take `seq` off the per-path skip record: the row was read after all
    /// (the head re-judge handed it to the ordinary fold), so this device's
    /// later claims may reach it exactly as if it had been read when first
    /// served. A no-op when it is not on record; a floor that cannot be read
    /// is left as it is (its loss stays on record). Best-effort like the
    /// write — a failure leaves the skip standing, the under-claiming side.
    pub fn release_skip(&self, path_hash_hex: &str, seq: i64) {
        let Some(key) = Self::floor_key_of(path_hash_hex) else {
            return;
        };
        let file = self.skip_floor_file(&key);
        let FloorRead::Floor(seqs) = Self::read_floor(&file) else {
            return;
        };
        if !seqs.contains(&seq) {
            return;
        }
        let rest: Vec<String> = seqs
            .iter()
            .filter(|s| **s != seq)
            .map(i64::to_string)
            .collect();
        let written = if rest.is_empty() {
            std::fs::remove_file(&file)
        } else {
            self.write_floor_atomically(&file, &rest.join(" "))
        };
        if let Err(e) = written {
            tracing::warn!(
                seq,
                error = %format!("{e:#}"),
                "releasing a skip the head re-judge folded failed — this path's \
                 claims stay below it"
            );
        }
    }

    /// The per-path floor file's new body once `seq` is recorded under
    /// `path_hash`; `None` = already on record, nothing to write.
    fn per_path_floor_after_skip(&self, path_hash: &[u8; 32], seq: i64) -> Option<String> {
        match Self::read_floor(&self.skip_floor_file(path_hash)) {
            FloorRead::Absent => Some(seq.to_string()),
            FloorRead::Floor(seqs) if seqs.contains(&seq) => None,
            FloorRead::Floor(seqs) => {
                // Prune what has released — every skip the frontier has
                // reached — but never the new seq itself, so the body is
                // never empty (an empty file reads as a lost floor).
                let frontier = std::fs::read_to_string(self.frontier_file_for_key(path_hash))
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .unwrap_or(0);
                let mut live: Vec<i64> = seqs.into_iter().filter(|s| *s > frontier).collect();
                live.push(seq);
                live.sort_unstable();
                Some(
                    live.iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            }
            // A floor was recorded under this key and its seqs are lost. The
            // per-path degradation for that — claim no further than the path's
            // own frontier — is no strand (it climbs as the path does), so
            // there is nothing to buy by overwriting it with a readable value
            // that FORGETS the loss, which is the over-claiming direction. The
            // marker keeps the loss on record beside the new seq; a file that
            // cannot even be replaced (a planted directory, EACCES) fails the
            // write and stays as it is — unreadable, which reads the same.
            FloorRead::Unreadable => {
                tracing::error!(
                    seq,
                    "a per-path skip floor exists but could not be read — keeping \
                     it marked lost beside this seq; that path's claims stay at \
                     or below its own frontier"
                );
                Some(format!("{LOST_FLOOR_MARKER} {seq}"))
            }
        }
    }

    /// The set-wide floor file's new body once `seq` is recorded on it;
    /// `None` = an equal or earlier skip already governs.
    fn set_wide_floor_after_skip(&self, seq: i64) -> Option<String> {
        match Self::read_floor(&self.set_skip_floor_file()) {
            // Already at or below this seq: no release exists, so the lowest
            // skip is exact and the claim has to be honest about the EARLIEST
            // row this device is missing.
            FloorRead::Floor(seqs) if seqs[0] <= seq => None,
            FloorRead::Floor(_) | FloorRead::Absent => Some(seq.to_string()),
            // The set-wide floor's seq is lost, so the monotone comparison
            // cannot run. Writing `seq` may RAISE the floor above the value
            // that was destroyed — the over-claiming direction — but, unlike a
            // per-path floor, leaving it unreadable pins every claim on the
            // DEVICE to 0 for ever with no way back. What must not happen is
            // doing either silently.
            FloorRead::Unreadable => {
                tracing::error!(
                    seq,
                    "the set-wide skip floor exists but could not be read — \
                     replacing it with this seq; if an EARLIER skip was recorded \
                     there its seq is lost and later stamps may over-claim \
                     causality for it"
                );
                Some(seq.to_string())
            }
        }
    }

    /// The honest ceiling for a stamp on `relative_path` — edit OR resolution
    /// — given this device's persisted catch-up `anchor`.
    ///
    /// `derived_through` is a **lower bound by law** ([`conflicts.md`] § the
    /// causal watermark): *stamp the persisted catch-up anchor, never more*.
    /// The anchor alone stopped being that bound the moment the apply path
    /// grew a permanent skip — a row the anchor accounts for and this device
    /// never read. Stamping over one tells a peer "I incorporated your row",
    /// and a peer whose file still matches its base then judges the edit
    /// fast-forward and overwrites its own unread work. Under-claiming costs
    /// one extra idempotent merge round, which is the trade the law names.
    ///
    /// **Per path, and self-releasing.** A skip on path P says nothing about
    /// path Q, so only P's claims are reduced — a set-wide ceiling would tax
    /// every edit on the device for ever after one bad row. And once P's own
    /// frontier reaches the skipped seq, the skip is superseded: this device's
    /// content for P reflects a row at or past it in nest-log order, so a peer
    /// fast-forwarded to these bytes loses nothing it should have kept. No
    /// clearing pass is needed — the comparison IS the release.
    ///
    /// **Every stamp kind — resolutions included (ruled 2026-09-20).** The
    /// lower-bound law binds `derived_through` whatever the row's class; the
    /// `is_resolution` bit is a separate assertion about the row's BYTES ("no
    /// novel content beyond `w`") and changes nothing about what `w` may
    /// claim. The raw-anchor resolution sites — a proven reissue, the re-seal
    /// re-upload, the re-assert of an own stale-declined tail — all carry
    /// bytes this device already holds or has published, so their class bit
    /// is honest at any `w`; it is the *incorporated every row ≤ w* half that
    /// rule 3 reads which lies once `w ≥ S`: a peer whose edit-frontier
    /// reached S (it applied the novel row this device could not read — the
    /// per-reader permanent classes are exactly rows other readers apply)
    /// judges the row covering and adopts OLD bytes verbatim over S's content,
    /// no conflict row: the lost-edit defect wearing a resolution stamp. The
    /// worry that reducing a resolution gets a MERGE RESULT stale-skipped was
    /// about the winner row, which never reads the anchor at all — its claim
    /// is `honest_w`, which is *usually* bounded by the path's pre-merge
    /// frontier, and a live PER-PATH skip sits above that frontier by
    /// construction (the release IS `frontier ≥ S`). That argument held for
    /// the two frontier-bounded branches of the claim and for neither the
    /// set-wide floor nor the own-gap-widened branch, so the winner claim now
    /// runs through the very same reductions this does — see
    /// [`Self::honest_winner_claim`]. With the honest claim the
    /// peer past S stale-skips a row that is information-free to it (rule 2
    /// doing its job), while every receiver below S still reads it as covering
    /// — no legitimate covering resolution is stranded.
    pub fn honest_anchor(&self, relative_path: &str, anchor: i64) -> i64 {
        self.set_wide_reduced(self.per_path_reduced(relative_path, anchor))
            .max(0)
    }

    /// The per-path floor's reduction — the arm that RELEASES, shared by every
    /// claim this device mints for `relative_path` ([`Self::honest_anchor`]
    /// and [`Self::honest_winner_claim`] alike).
    fn per_path_reduced(&self, relative_path: &str, claim: i64) -> i64 {
        let mut claim = claim;
        match Self::read_floor(&self.skip_floor_file(&fauna_core::sync::path_hash(relative_path))) {
            FloorRead::Absent => {}
            // The earliest LIVE skip: the lowest one the path's frontier has
            // not yet reached. Every skip at or below the frontier has released.
            FloorRead::Floor(seqs) => {
                let frontier = self.frontier(relative_path).unwrap_or(0);
                if let Some(floor) = seqs.into_iter().find(|s| *s > frontier) {
                    claim = claim.min(floor - 1);
                }
            }
            // A per-path floor whose seq is lost still admits a provable
            // bound: whatever it was, either it has RELEASED (`frontier ≥
            // floor`, so the claim stands) or it is live (`floor > frontier`,
            // so the true reduction `min(claim, floor − 1)` is itself ≥
            // `min(claim, frontier)`). The path's own frontier is therefore
            // honest under both readings, and it climbs as the path does — so
            // unlike the set-wide arm this degradation is not a strand.
            FloorRead::Unreadable => {
                tracing::error!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    "this path's skip floor could not be read, or is marked \
                     lost — claiming no \
                     further than the path's own frontier until it can be"
                );
                claim = claim.min(self.frontier(relative_path).unwrap_or(0));
            }
        }
        claim
    }

    /// The set-wide floor's reduction — the arm with **no release**, shared by
    /// every claim this device mints.
    ///
    /// ⚠ The exemption the auto-resolve winner arm was granted (the leg-4
    /// ruling, 2026-08-05; restated 2026-09-20) was argued from the PER-PATH
    /// floor's release and never reached this one. A set-wide skip is
    /// unattributable to any path by definition (the row's `path_hash` was not
    /// even 32 hex bytes), so NO path's frontier is held below it and every
    /// frontier climbs past it freely; a frontier-bounded claim then crosses a
    /// skipped row, which is precisely the lost-edit defect the floor exists
    /// to prevent . Hence this
    /// reduction runs on the winner claim too, and the two claim kinds can no
    /// longer disagree about the same skipped row.
    fn set_wide_reduced(&self, claim: i64) -> i64 {
        match Self::read_floor(&self.set_skip_floor_file()) {
            FloorRead::Absent => claim,
            FloorRead::Floor(seqs) => claim.min(seqs[0] - 1),
            // No release exists for this floor, so an unknown value admits no
            // bound above 0 — the lost seq could be the very first row. That
            // pins every claim on this device until the file reads again:
            // pathological, like the row shape that mints the floor at all,
            // and the only honest answer to "a skip was recorded here and
            // nothing about it can be trusted".
            FloorRead::Unreadable => {
                tracing::error!(
                    "the set-wide skip floor could not be read — pinning every \
                     causal claim on this device until it can be"
                );
                0
            }
        }
    }

    /// The honest `winning_derived_through` for an auto-resolve winner row —
    /// the ONE derivation (`SyncEngine::resolve_and_report` consumes it),
    /// exactly as the edit and resolution stamp sites already share
    /// [`Self::honest_anchor`].
    ///
    /// Two bounds, and the second is the one the leg-4 exemption missed:
    ///
    /// 1. **The frontier bound (leg-4 ruling, 2026-08-05 — the lower-bound law
    ///    applied to `winning_derived_through`).** The incoming seq only when
    ///    the merge provably incorporates every row ≤ it: contiguous with this
    ///    path's pre-merge frontier, or every gap row below it authored by
    ///    this device (`gap_all_own`, which the caller proves with the listing
    ///    in hand — their content is reflected in local by the defer-arm
    ///    invariant). Otherwise the claim falls back to the frontier. The
    ///    pre-ruling unconditional `incoming_seq` stamp OVER-CLAIMED whenever
    ///    unconsumed rows sat below the incoming row, and every covering test
    ///    downstream trusted it — the leg-4 lost line.
    /// 2. **Both skip floors** — the very reductions [`Self::honest_anchor`]
    ///    applies, in the same order. The winner arm used to be exempt from
    ///    both on one argument: *a live skip sits above the path's frontier by
    ///    construction, and this claim is frontier-bounded*. The argument is
    ///    sound for exactly two of the three cases above and was taken for all
    ///    of them :
    ///
    ///    - The **set-wide** floor has no release and belongs to no path, so
    ///      no frontier is held below it and even a frontier-bounded claim
    ///      crosses it. The exemption never covered it at all.
    ///    - The **own-gap-widened** branch is not frontier-bounded: it claims
    ///      `incoming_seq` outright. A row skipped under this path's own
    ///      `path_hash` can be invisible to the caller's gap proof — at the
    ///      seal-refusal mint sites the row's path never resolved, so it
    ///      matches no same-path filter (the resolved-path site, a change
    ///      that can never apply here, does not have that property, and the
    ///      unconditional reduction does not rely on it) — and both hosts
    ///      mint per-path floors earlier in the same batch they later claim
    ///      in. So `gap_all_own` can be true with a LIVE floor in the
    ///      gap, and the claim crossed it.
    ///
    ///    Applying the per-path reduction here costs the sound cases nothing,
    ///    which is why it is applied unconditionally rather than branched on:
    ///    a live floor sits above the frontier (`floor > f`) and is a
    ///    different row from the incoming one (`floor ≠ incoming_seq`), so on
    ///    the contiguous branch `floor ≥ f + 2 ⇒ floor − 1 ≥ incoming_seq`
    ///    and on the frontier-fallback branch `floor − 1 ≥ f` — both
    ///    untouched. It bites only where the claim outran the frontier.
    pub fn honest_winner_claim(
        &self,
        relative_path: &str,
        incoming_seq: i64,
        gap_all_own: bool,
    ) -> i64 {
        let frontier = self.frontier(relative_path).unwrap_or(0);
        let claim = if incoming_seq == frontier + 1 || gap_all_own {
            incoming_seq
        } else {
            frontier
        };
        self.set_wide_reduced(self.per_path_reduced(relative_path, claim))
            .max(0)
    }

    /// Both frontiers in the shape the licence reads.
    ///
    /// ⚠ A tracked frontier with an untracked edit-frontier reads as
    /// `edit_frontier == frontier` (see [`PathFrontiers`]) — the correct
    /// reading for state whose edit-frontier was lost or never stamped.
    pub fn frontiers(&self, relative_path: &str) -> PathFrontiers {
        PathFrontiers {
            frontier: self.frontier(relative_path),
            edit_frontier: self.edit_frontier(relative_path),
            content_frontier: self.content_frontier(relative_path),
        }
    }

    /// Does any held ledger entry at seq ≤ `bound` have exactly this content
    /// hash? The rung's caller-side lookup ([`is_reissue_of_held_content`]):
    /// hashes held BYTES because manifest hashes cannot witness content
    /// equality across seal generations (a re-seal mints a new manifest for
    /// the same bytes — comparing manifests would miss precisely the re-seal
    /// class the rung exists to catch).
    pub fn holds_content_at_or_below(
        &self,
        relative_path: &str,
        bound: i64,
        content_hash: &fauna_core::data::ContentHash,
    ) -> bool {
        self.index(relative_path)
            .into_iter()
            .filter(|s| *s <= bound)
            .any(|s| {
                std::fs::read(self.ledger_file(relative_path, s))
                    .map(|bytes| fauna_core::data::ContentHash::of_raw(&bytes) == *content_hash)
                    .unwrap_or(false)
            })
    }

    /// The NEWEST held seq whose bytes equal `data` — the author-blind
    /// supersession's ordering guard: it says which log row local content
    /// came from (when it came from one at all), which seq-vs-frontier
    /// cannot (`conflicts.md` clause 5). `None` = local matches no held row
    /// (fresh merge output, or never held) — callers treat that as seq 0.
    pub fn newest_held_seq_matching(&self, relative_path: &str, data: &[u8]) -> Option<i64> {
        self.index(relative_path).into_iter().rev().find(|s| {
            std::fs::read(self.ledger_file(relative_path, *s))
                .map(|bytes| bytes == data)
                .unwrap_or(false)
        })
    }

    /// Hold row `seq`'s content (capped + pruned) as a future merge ancestor.
    pub fn hold(&self, relative_path: &str, seq: i64, data: &[u8]) {
        if data.len() > LEDGER_MAX_BYTES {
            return;
        }
        let mut seqs = self.index(relative_path);
        if seqs.contains(&seq) {
            return;
        }
        let _ = std::fs::create_dir_all(&self.dir);
        if std::fs::write(self.ledger_file(relative_path, seq), data).is_err() {
            return; // best-effort — a missing entry degrades the ancestor ladder
        }
        seqs.push(seq);
        seqs.sort_unstable();
        while seqs.len() > LEDGER_KEEP {
            let old = seqs.remove(0);
            let _ = std::fs::remove_file(self.ledger_file(relative_path, old));
        }
        let joined = seqs
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(self.ledger_index_file(relative_path), joined);
    }

    /// Remember which MANIFEST a held/consumed row carried (gap-3 ruling) —
    /// the byte-free reissue witness for judgement sites that see rows before
    /// any fetch (the batch pre-pass, the fold arm). A manifest hash is a
    /// weaker witness than the ledger's byte compare (a re-seal mints a new
    /// manifest for the same bytes — false negatives), but every false
    /// negative just advances the edit-frontier: the safe, at-worst-stranding
    /// direction of the upper-bound law. Capped alongside the ledger.
    pub fn remember_manifest(&self, relative_path: &str, seq: i64, manifest_hex: &str) {
        let file = self.manifest_memory_file(relative_path);
        let mut rows: Vec<(i64, String)> = std::fs::read_to_string(&file)
            .ok()
            .map(|s| {
                s.lines()
                    .filter_map(|l| {
                        let (s, m) = l.trim().split_once(' ')?;
                        Some((s.parse().ok()?, m.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if rows.iter().any(|(s, _)| *s == seq) {
            return;
        }
        let _ = std::fs::create_dir_all(&self.dir);
        rows.push((seq, manifest_hex.to_string()));
        rows.sort_unstable_by_key(|(s, _)| *s);
        while rows.len() > LEDGER_KEEP {
            rows.remove(0);
        }
        let joined = rows
            .iter()
            .map(|(s, m)| format!("{s} {m}"))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(file, joined);
    }

    /// Is `manifest_hex` remembered at a seq ≤ `bound`? (See
    /// [`Self::remember_manifest`].)
    pub fn manifest_remembered_at_or_below(
        &self,
        relative_path: &str,
        bound: i64,
        manifest_hex: &str,
    ) -> bool {
        std::fs::read_to_string(self.manifest_memory_file(relative_path))
            .ok()
            .is_some_and(|s| {
                s.lines().any(|l| {
                    l.trim()
                        .split_once(' ')
                        .and_then(|(s, m)| Some((s.parse::<i64>().ok()?, m)))
                        .is_some_and(|(s, m)| s <= bound && m == manifest_hex)
                })
            })
    }

    /// The newest held version with seq ≤ `bound` — the ancestor the incoming
    /// row's writer provably had. `None` = fall down the ladder (live base
    /// slot, then no-base latest-wins).
    pub fn ancestor_at_or_below(&self, relative_path: &str, bound: i64) -> Option<(i64, Vec<u8>)> {
        let best = self
            .index(relative_path)
            .into_iter()
            .filter(|s| *s <= bound)
            .max()?;
        let bytes = std::fs::read(self.ledger_file(relative_path, best)).ok()?;
        Some((best, bytes))
    }

    fn index(&self, relative_path: &str) -> Vec<i64> {
        std::fs::read_to_string(self.ledger_index_file(relative_path))
            .ok()
            .map(|s| s.lines().filter_map(|l| l.trim().parse().ok()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frontier-only state, edit-frontier untracked — the shape of a path whose
    /// edit-frontier state was lost or never stamped.
    fn f(frontier: Option<i64>) -> PathFrontiers {
        PathFrontiers {
            frontier,
            edit_frontier: None,
            content_frontier: None,
        }
    }
    /// Fully-tracked state.
    fn fe(frontier: i64, edit_frontier: i64) -> PathFrontiers {
        PathFrontiers {
            frontier: Some(frontier),
            edit_frontier: Some(edit_frontier),
            content_frontier: None,
        }
    }

    #[test]
    fn duplicate_wins_over_everything() {
        assert_eq!(
            judge_incoming(5, Some(9), true, false, f(Some(5)), false, true),
            IncomingVerdict::Duplicate
        );
        assert_eq!(
            judge_incoming(4, None, false, false, f(Some(5)), true, false),
            IncomingVerdict::Duplicate
        );
    }

    /// Both receivers act on the byte-free verdicts BEFORE reading the local
    /// file, so those verdicts must not depend on the local bytes — and must
    /// agree with the full judgement. Exhaustive over a small grid rather than
    /// spot-checked: the property is what licenses skipping the download, and
    /// it used to rest on a `local_matches_base: true` placeholder passed at
    /// two call sites with a comment for a proof.
    #[test]
    fn the_byte_free_verdicts_ignore_the_local_bytes_and_agree_with_the_full_judgement() {
        for seq in [1i64, 5, 9] {
            for derived_through in [None, Some(2i64), Some(5), Some(9)] {
                for is_resolution in [false, true] {
                    for is_retention in [false, true] {
                        for frontier in [None, Some(1i64), Some(5), Some(9)] {
                            for edit_frontier in [None, Some(1i64), Some(5)] {
                                for held in [false, true] {
                                    let frontiers = PathFrontiers {
                                        frontier,
                                        edit_frontier,
                                        content_frontier: None,
                                    };
                                    let pre = judge_incoming_before_fetch(
                                        seq,
                                        derived_through,
                                        is_resolution,
                                        is_retention,
                                        frontiers,
                                    );
                                    let yes = judge_incoming(
                                        seq,
                                        derived_through,
                                        is_resolution,
                                        is_retention,
                                        frontiers,
                                        held,
                                        true,
                                    );
                                    let no = judge_incoming(
                                        seq,
                                        derived_through,
                                        is_resolution,
                                        is_retention,
                                        frontiers,
                                        held,
                                        false,
                                    );
                                    let case = format!(
                                        "seq={seq} w={derived_through:?} res={is_resolution} \
                                         ret={is_retention} f={frontier:?} ef={edit_frontier:?} \
                                         held={held}"
                                    );
                                    match pre {
                                        PreFetchVerdict::Duplicate => {
                                            assert_eq!(yes, IncomingVerdict::Duplicate, "{case}");
                                            assert_eq!(no, IncomingVerdict::Duplicate, "{case}");
                                        }
                                        PreFetchVerdict::StaleResolution => {
                                            assert_eq!(
                                                yes,
                                                IncomingVerdict::StaleResolution,
                                                "{case}"
                                            );
                                            assert_eq!(
                                                no,
                                                IncomingVerdict::StaleResolution,
                                                "{case}"
                                            );
                                        }
                                        PreFetchVerdict::RetentionRow => {
                                            assert_eq!(
                                                yes,
                                                IncomingVerdict::RetentionRow,
                                                "{case}"
                                            );
                                            assert_eq!(no, IncomingVerdict::RetentionRow, "{case}");
                                            // The rung is unconditional past the
                                            // duplicate guard: every retention
                                            // row lands here or in Duplicate.
                                            assert!(is_retention, "{case}");
                                        }
                                        PreFetchVerdict::NeedsLocalBytes => {
                                            // A retention row is never
                                            // undecidable — the rung would have
                                            // returned.
                                            assert!(!is_retention, "{case}");
                                            // Skipping the fetch here would drop a row
                                            // — except the reissue rung, which is
                                            // allowed to skip because the content is
                                            // proven already incorporated.
                                            for (v, which) in
                                                [(yes, "local==base"), (no, "local!=base")]
                                            {
                                                assert!(
                                                    !matches!(
                                                        v,
                                                        IncomingVerdict::Duplicate
                                                            | IncomingVerdict::StaleResolution
                                                            | IncomingVerdict::RetentionRow
                                                    ),
                                                    "{case} ({which}): full judgement said {v:?} \
                                                     but the pre-fetch pass would have downloaded \
                                                     and applied it"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The retention rung (loser-row ruling, 2026-08-05): unconditional and
    /// byte-free — whatever the stamps, frontiers, or held bytes say, a
    /// retention row is accounted and never applied. The one thing that
    /// outranks it is the duplicate guard (a seq the frontier already covers
    /// stays a duplicate — same skip, idempotence stays primary).
    #[test]
    fn a_retention_row_is_accounted_and_never_applied() {
        // Fresh seq, no frontier tracked yet: still skipped (the account is
        // what STARTS the frontier — a new device must not merge it either).
        assert_eq!(
            judge_incoming(9, Some(2), false, true, f(None), false, true),
            IncomingVerdict::RetentionRow
        );
        // Tracked frontier, would-be-divergent stamps, held bytes, unpublished
        // local work — none of it matters.
        assert_eq!(
            judge_incoming(9, Some(2), false, true, fe(7, 3), true, false),
            IncomingVerdict::RetentionRow
        );
        // A watermark that would fast-forward an ordinary row: still skipped.
        assert_eq!(
            judge_incoming(9, Some(7), false, true, fe(7, 7), false, true),
            IncomingVerdict::RetentionRow
        );
        // Behind the frontier it is an ordinary duplicate.
        assert_eq!(
            judge_incoming(5, Some(2), false, true, f(Some(5)), false, true),
            IncomingVerdict::Duplicate
        );
    }

    #[test]
    fn a_covering_stale_resolution_fast_forwards_past_order_choices() {
        // w=5 misses resolutions 6..7 but covers every novel edit (ef=3):
        // the rows it misses are order-choices this later row supersedes.
        assert_eq!(
            judge_incoming(9, Some(5), true, false, fe(7, 3), false, true),
            IncomingVerdict::FastForward
        );
        // Unpublished local work still refuses the verbatim adopt — it merges.
        assert_eq!(
            judge_incoming(9, Some(5), true, false, fe(7, 3), false, false),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(5)
            }
        );
        // Below the edit-frontier it misses NOVEL content — genuinely stale.
        assert_eq!(
            judge_incoming(9, Some(2), true, false, fe(7, 3), false, true),
            IncomingVerdict::StaleResolution
        );
        // An EDIT at the same watermark gets no such licence: novel bytes at
        // a stale watermark are a concurrent sibling and must merge.
        assert_eq!(
            judge_incoming(9, Some(5), false, false, fe(7, 3), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(5)
            }
        );
    }

    /// The cap-release ruling (2026-09-21, `conflicts.md` clause 5): the
    /// untracked-frontier arms honour the effective edit-frontier exactly as
    /// the tracked arm does. `e(ef)` is the fresh-bind state — no frontier,
    /// an own row counted at the record ack.
    fn e(edit_frontier: i64) -> PathFrontiers {
        PathFrontiers {
            frontier: None,
            edit_frontier: Some(edit_frontier),
            content_frontier: None,
        }
    }

    #[test]
    fn a_row_below_the_counted_own_novelty_is_a_sibling_on_an_untracked_path() {
        // Watermarked: a peer row whose watermark sits below the own row this
        // device already counted never saw it — merge, never fast-forward
        // (the pre-ruling arm read the recorded witness alone and handed the
        // row to the DEFER cap, which parked the fresh bind for ever).
        assert_eq!(
            judge_incoming(1, Some(0), false, false, e(3), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(0)
            }
        );
        // A resolution row is held to the same conjunct.
        assert_eq!(
            judge_incoming(1, Some(0), true, false, e(3), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(0)
            }
        );
        // One that DOES dominate the count still fast-forwards on the
        // recorded witness — the gap-3 anti-duplication adopt, untouched
        // (the caller's cap then holds it until the own echo lands, which
        // now always sits BELOW it).
        assert_eq!(
            judge_incoming(5, Some(3), false, false, e(3), false, true),
            IncomingVerdict::FastForward
        );
        // Unwatermarked: the log-order floor — a row at or below the count
        // was recorded before the own row and cannot have incorporated it.
        assert_eq!(
            judge_incoming(1, None, false, false, e(3), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: None
            }
        );
        assert_eq!(
            judge_incoming(3, None, false, false, e(3), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: None
            }
        );
        assert_eq!(
            judge_incoming(4, None, false, false, e(3), false, true),
            IncomingVerdict::FastForward
        );
        // Nothing counted → the pre-ruling degrade, verbatim.
        assert_eq!(
            judge_incoming(1, Some(0), false, false, f(None), false, true),
            IncomingVerdict::FastForward
        );
        assert_eq!(
            judge_incoming(1, None, false, false, f(None), false, true),
            IncomingVerdict::FastForward
        );
        // Tracked frontier, unwatermarked row in the ack→echo window (ef
        // above f): the same floor; above the count it is unchanged.
        assert_eq!(
            judge_incoming(6, None, false, false, fe(5, 7), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: None
            }
        );
        assert_eq!(
            judge_incoming(8, None, false, false, fe(5, 7), false, true),
            IncomingVerdict::FastForward
        );
    }

    /// The receiver's half of the lower-bound law (`conflicts.md` clause 5,
    /// the watermark-bound ruling): a watermark is the WRITER's word, and the
    /// nest assigns the row's seq after it, so a claim at or above the row's
    /// own seq is a lie the log itself refutes. Every case pairs a forged row
    /// with its honest sibling — the forged one must read exactly the same.
    #[test]
    fn a_forged_watermark_is_bounded_by_the_rows_own_seq() {
        // Untracked arm (the fresh-bind shape, own row counted at 20): the
        // honest sibling is a SIBLING, and the forged claim used to buy a
        // fast-forward the leg-4 cap then parked the anchor under for ever.
        let honest = judge_incoming(11, Some(10), false, false, e(20), false, true);
        assert_eq!(
            honest,
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(10)
            }
        );
        assert_eq!(
            judge_incoming(11, Some(i64::MAX), false, false, e(20), false, true),
            honest,
            "a forged watermark must not out-vote the counted own row"
        );
        // Tracked arm, the ack→echo window (f 15, own edit counted at 20).
        assert_eq!(
            judge_incoming(17, Some(i64::MAX), false, false, fe(15, 20), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(16)
            }
        );
        // Resolution class, tracked, below the counted edit: the honest
        // sibling is rule-2 stale and skipped byte-free; the forged one must
        // be too — on BOTH public entries, because both hosts act on the
        // byte-free one before the fetch.
        assert_eq!(
            judge_incoming_before_fetch(17, Some(16), true, false, fe(15, 20)),
            PreFetchVerdict::StaleResolution
        );
        assert_eq!(
            judge_incoming_before_fetch(17, Some(i64::MAX), true, false, fe(15, 20)),
            PreFetchVerdict::StaleResolution
        );
        assert_eq!(
            judge_incoming(17, Some(i64::MAX), true, false, fe(15, 20), false, true),
            IncomingVerdict::StaleResolution
        );
        // The bound never moves an honest verdict: an honest anchor is always
        // below the seq the nest assigns afterwards.
        assert_eq!(bounded_watermark(11, Some(10)), Some(10));
        assert_eq!(bounded_watermark(11, Some(11)), Some(10));
        assert_eq!(bounded_watermark(11, None), None);
        assert_eq!(bounded_watermark(i64::MIN, Some(0)), Some(i64::MIN));
        // A row above the count with a forged claim still fast-forwards, as
        // its honest sibling (w = seq − 1 ≥ ef) would: the cap that may then
        // hold it sits ABOVE every counted own row and lasts one pull.
        assert_eq!(
            judge_incoming(25, Some(i64::MAX), false, false, e(20), false, true),
            IncomingVerdict::FastForward
        );
    }

    /// The cap's firing rule (held-bytes release, 2026-09-21).
    #[test]
    fn the_cap_holds_for_novel_pending_bytes_and_not_for_held_ones() {
        // The report window always holds.
        assert!(verbatim_adopt_deferred(true, true, true));
        // The ack→echo window with NOVEL pending bytes holds.
        assert!(verbatim_adopt_deferred(false, false, false));
        // …but not when the ledger already holds those bytes (a revert).
        assert!(!verbatim_adopt_deferred(false, false, true));
        // Local equal to the live base never holds.
        assert!(!verbatim_adopt_deferred(false, true, false));
    }

    #[test]
    fn an_untracked_edit_frontier_degrades_to_the_pre_gap_rules() {
        // ef untracked reads as ef == f: exactly the pre-gap-2 skip.
        assert_eq!(
            judge_incoming(9, Some(5), true, false, f(Some(7)), false, true),
            IncomingVerdict::StaleResolution
        );
    }

    #[test]
    fn the_reissue_rung_skips_held_content_only_at_a_stale_watermark() {
        // Stale watermark + held bytes = reissue: skip.
        assert_eq!(
            judge_incoming(9, Some(2), false, false, fe(7, 7), true, true),
            IncomingVerdict::ReissueOfHeldContent
        );
        // A caught-up writer re-publishing held bytes is a genuine REVERT —
        // fresh intent, never skipped by the rung.
        assert_eq!(
            judge_incoming(9, Some(7), false, false, fe(7, 7), true, true),
            IncomingVerdict::FastForward
        );
        // Unheld content at the same stale watermark merges as before.
        assert_eq!(
            judge_incoming(9, Some(2), false, false, fe(7, 7), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(2)
            }
        );
        // No watermark → no rung (pre-ruling degrade), even with held bytes.
        assert_eq!(
            judge_incoming(9, None, false, false, fe(7, 7), true, true),
            IncomingVerdict::FastForward
        );
        // The rung never preempts an adopt: a COVERING resolution whose bytes
        // happen to match an earlier identical publication still
        // fast-forwards — skipping it here is what re-stranded the model's
        // three-seat append run.
        assert_eq!(
            judge_incoming(9, Some(5), true, false, fe(7, 3), true, true),
            IncomingVerdict::FastForward
        );
        // With unpublished local work the same row falls past the licence,
        // and there the rung DOES catch it (skip beats a stale-ancestor
        // merge of bytes this device already holds).
        assert_eq!(
            judge_incoming(9, Some(5), true, false, fe(7, 3), true, false),
            IncomingVerdict::ReissueOfHeldContent
        );
    }

    /// The gap-3 class upgrade: a row whose bytes the receiver holds at seq ≤
    /// its frontier is proven novel-content-free — `is_resolution`'s ratified
    /// meaning — so it is judged by the resolution licences whatever stamp it
    /// wears. The writer of a lost-ack retry cannot stamp it; the receiver
    /// holding the bytes has the proof the writer lacked.
    #[test]
    fn held_bytes_upgrade_an_edit_stamped_row_to_the_resolution_licences() {
        // Held + edit-stamped + covering (w ≥ ef) + no unpublished work:
        // ADOPT by supersession — the pre-upgrade rules rung-skipped this,
        // which made the reissue's adoption ORDER-DEPENDENT (a receiver whose
        // frontier had not yet passed the watermark fast-forwarded onto it
        // instead), and that order-dependence IS the same-anchor divergence.
        assert_eq!(
            judge_incoming(9, Some(5), false, false, fe(7, 3), true, true),
            IncomingVerdict::FastForward
        );
        // Same row with unpublished local work: the rung still catches it
        // (skip beats a stale-ancestor merge of held bytes).
        assert_eq!(
            judge_incoming(9, Some(5), false, false, fe(7, 3), true, false),
            IncomingVerdict::ReissueOfHeldContent
        );
        // Below the edit-frontier the upgraded row is genuinely stale — the
        // rung skips it (never `StaleResolution`: the byte-free pass cannot
        // see the upgrade, and the two passes must agree on their verdicts).
        assert_eq!(
            judge_incoming(9, Some(2), false, false, fe(7, 3), true, true),
            IncomingVerdict::ReissueOfHeldContent
        );
        // Unheld, the same stamps still merge — the upgrade needs the proof.
        assert_eq!(
            judge_incoming(9, Some(5), false, false, fe(7, 3), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(5)
            }
        );
    }

    /// `conflicts.md` § *Retention rows are transparent to the licence*
    /// (2026-09-27): the resolved report's shape — the reporter's claim sits
    /// at 10, the nest mints the loser retention row at 11 and the edit-class
    /// winner at 12 with its claim AS SENT. A receiver caught up through 10
    /// with no unpublished work fast-forwards the winner: the retention row
    /// advanced its frontier, not the content its local reflects.
    #[test]
    fn an_edit_behind_its_retention_row_fast_forwards_at_a_caught_up_receiver() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().to_path_buf());
        store.advance_frontier("p", 10);
        store.advance_edit_frontier("p", 10);
        store.account_retention_row("p", 11);
        let frontiers = store.frontiers("p");
        assert_eq!(
            frontiers.frontier,
            Some(11),
            "the retention row counts as seen"
        );
        assert_eq!(frontiers.effective_content_frontier(), Some(10));
        assert_eq!(
            judge_incoming(12, Some(10), false, false, frontiers, false, true),
            IncomingVerdict::FastForward
        );
        // Unpublished local work still refuses the verbatim licence.
        assert_eq!(
            judge_incoming(12, Some(10), false, false, frontiers, false, false),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(10)
            }
        );
    }

    /// The same winner with a genuine CONTENT row between its claim and the
    /// retention row still merges — the receiver reflects row 10, which the
    /// writer (claiming 9) never incorporated.
    #[test]
    fn an_edit_missing_a_content_row_behind_the_retention_row_still_merges() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().to_path_buf());
        store.advance_frontier("p", 9);
        store.advance_frontier("p", 10);
        store.advance_edit_frontier("p", 10);
        store.account_retention_row("p", 11);
        assert_eq!(
            judge_incoming(12, Some(9), false, false, store.frontiers("p"), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(9)
            }
        );
    }

    /// Every other accounting site moves the content frontier with the
    /// frontier, and the retention funnel pins an untracked edit-frontier at
    /// its pre-row value instead of letting the fallback carry it past a row
    /// that binds no novelty. An untracked content frontier degrades to the
    /// full frontier — the licence exactly as before the ruling.
    #[test]
    fn the_content_frontier_follows_every_advance_but_the_retention_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().to_path_buf());
        store.advance_frontier("p", 5);
        assert_eq!(
            store.content_frontier("p"),
            None,
            "untracked until a retention row"
        );
        store.account_retention_row("p", 6);
        assert_eq!(store.content_frontier("p"), Some(5));
        assert_eq!(store.edit_frontier("p"), Some(5));
        store.advance_frontier("p", 8);
        assert_eq!(store.content_frontier("p"), Some(8));
        assert_eq!(store.frontier("p"), Some(8));
        store.account_retention_row("p", 9);
        assert_eq!(store.content_frontier("p"), Some(8));
        assert_eq!(store.frontier("p"), Some(9));
        // A retention row on an untracked path starts the frontier and pins
        // nothing.
        store.account_retention_row("q", 4);
        assert_eq!(store.frontier("q"), Some(4));
        assert_eq!(store.content_frontier("q"), None);
        assert_eq!(store.edit_frontier("q"), None);
        // Untracked degrades: an edit claiming below the full frontier merges.
        assert_eq!(
            judge_incoming(12, Some(10), false, false, fe(11, 10), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(10)
            }
        );
    }

    #[test]
    fn the_manifest_memory_remembers_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().to_path_buf());
        assert!(!store.manifest_remembered_at_or_below("p", 10, "aa"));
        store.remember_manifest("p", 3, "aa");
        assert!(store.manifest_remembered_at_or_below("p", 3, "aa"));
        assert!(store.manifest_remembered_at_or_below("p", 10, "aa"));
        // Below the bound it is not a witness.
        assert!(!store.manifest_remembered_at_or_below("p", 2, "aa"));
        assert!(!store.manifest_remembered_at_or_below("p", 10, "bb"));
        // Pruned alongside the ledger cap: oldest rows fall off.
        for s in 4..(4 + LEDGER_KEEP as i64) {
            store.remember_manifest("p", s, &format!("m{s}"));
        }
        assert!(!store.manifest_remembered_at_or_below("p", 10, "aa"));
    }

    #[test]
    fn stale_resolution_is_skipped_but_stale_fresh_edit_merges() {
        assert_eq!(
            judge_incoming(9, Some(2), true, false, f(Some(3)), false, true),
            IncomingVerdict::StaleResolution
        );
        assert_eq!(
            judge_incoming(9, Some(2), false, false, f(Some(3)), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(2)
            }
        );
    }

    #[test]
    fn fast_forward_needs_both_conjuncts() {
        assert_eq!(
            judge_incoming(9, Some(3), false, false, f(Some(3)), false, true),
            IncomingVerdict::FastForward
        );
        // Sibling: watermark below the frontier.
        assert_eq!(
            judge_incoming(9, Some(2), false, false, f(Some(3)), false, true),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(2)
            }
        );
        // Unpublished local work: base mismatch refuses the fast-forward even
        // for a genuine descendant.
        assert_eq!(
            judge_incoming(9, Some(3), false, false, f(Some(3)), false, false),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: Some(3)
            }
        );
    }

    #[test]
    fn absent_watermark_degrades_to_the_pre_ruling_licence() {
        assert_eq!(
            judge_incoming(9, None, false, false, f(Some(3)), false, true),
            IncomingVerdict::FastForward
        );
        assert_eq!(
            judge_incoming(9, None, false, false, f(Some(3)), false, false),
            IncomingVerdict::Diverged {
                ancestor_seq_bound: None
            }
        );
        assert_eq!(
            judge_incoming(9, None, false, false, f(None), false, true),
            IncomingVerdict::FastForward
        );
    }
}
