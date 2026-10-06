//! A multi-round convergence oracle for the merge-base policy.
//!
//! **Why this exists.** The choice of merge ancestor (`file-sync.md` § Conflicts,
//! *Concurrent resolution & ancestor freshness*) has two independent failure
//! modes, and **neither is visible in a single merge**:
//!
//! - **Lost edit** — a device fast-forwards over its own published work because
//!   `local == base` read as "I have not diverged" while the incoming version
//!   was a concurrent *sibling* that never contained that work. Measured by
//!   `test_filesync_threeseat.py` leg 4, 2026-08-02.
//! - **Non-convergence** — a device merges from an ancestor that never advances,
//!   so each round regenerates content the peer merges again, and the file grows
//!   without bound. Measured by `tests/platform/sync/test_text_merge.py`, same
//!   day, against a candidate fix for the first mode.
//!
//! Both took ~30 minutes of e2e queue to observe, and a policy that fixes one
//! silently causes the other. This module models the round dynamics directly —
//! N seats, a totally ordered nest change log, the real shared
//! [`crate::conflict_resolver::resolve_conflict`] — so a policy is judged in
//! milliseconds on both axes at once. It is a **decision oracle, not a mock of
//! production**: it deliberately omits transport and sealing, because the
//! question it answers is only "which ancestor should a merge read" — but
//! since 2026-08-05 it does model **retention and reporting** (the resolved
//! report's loser-row + winner-row mint, [`CPublish`]), because omitting them
//! is precisely how the third-gap ruling was fuzzer-validated while the live
//! run refuted its completeness (the report-plane remainder).
//!
//! **Read the policies below as the design record: ALL FOUR are known wrong in
//! production, and this module is the cheap way to see why.** Two are refuted by
//! the model itself; two more converge here but were measured running away
//! against a real sync host. That gap is the module's most important lesson, and
//! it is pinned rather than hidden: **model-level convergence does NOT imply
//! production convergence**, because a real host re-delivers duplicate rows
//! and re-uploads on reconcile, and each of those re-enters a merge that this
//! model skips. Use the oracle to REFUTE a candidate cheaply; never to bless one.
//!
//! The standing conclusion as of 2026-08-02: with **one scalar ancestor and no
//! parent pointer on `SyncChange`**, no policy tried satisfies both axes at
//! N ≥ 3 concurrent writers. An ancestor that advances on self-authored content
//! drops a sibling's edit; one that does not advance fails to converge; and "the
//! last content a peer had" is not a common ancestor once peers are concurrent
//! with each other. The next candidate should probably give the engine real
//! causality (a parent pointer, or per-peer ancestors) rather than a fifth
//! heuristic over one slot — a wire-compatibility decision, not a local fix.
//!
//! **RULED, same day:** causality landed as the scalar `derived_through`
//! watermark on `SyncChange` — "this version is my resolution of every set row
//! ≤ this seq", stamped from the writer's own catch-up anchor, a lower bound by
//! contract. Decision record: the 2026-08-02 `SyncChange` causal-watermark
//! design record (tracked internally); binding prose: `file-sync.md` §
//! Conflicts. The **causal-watermark model** at
//! the bottom of this file is its oracle: the licence (`local == base` AND
//! `w ≥ frontier`, the newest row this path's local content reflects — authored
//! OR applied; the first draft used "my last publication" and the 3-seat run
//! refuted it in milliseconds: a head can dominate my publication yet not the
//! peer row I merged after it), the causally-justified merge ancestor (newest
//! held version ≤ the incoming watermark), and the causal batch fold (fold a
//! row only under a later row that provably incorporated it). The legacy
//! policies above are kept verbatim as the refutation record.
//!
//! **THE SCHEDULE FUZZER (added 2026-08-03) — read this before trusting a
//! green run of the hand-written cases.** Every test in the first two sections
//! runs ONE schedule its author wrote down, and that is exactly how two
//! refuted policies passed this module: a hand-written schedule can only
//! encode churn someone already suspected. The third section generates the
//! schedule instead — batch boundaries, duplicate re-delivery and reconcile
//! re-uploads in orders nobody chose — and shrinks a failure to a minimal
//! reproduction. It drives the same two step functions the hand-written tests
//! do, and `causal_apply` now asks the PRODUCTION decision core
//! ([`crate::causal::judge_incoming`]) rather than transcribing its rules, so
//! a refutation here is a refutation of shipped behaviour.
//!
//! Its gate is `the_fuzzer_refutes_the_policy_the_hand_written_model_blessed`:
//! a fuzzer that cannot refute `ParkedUntilIncorporated` — blessed here,
//! refuted by production in 33 minutes — has not closed the blind spot.
//!
//! **On its first run it also refuted the SHIPPED rules, twice.** Both
//! findings are gaps in the ratified rule set rather than in the code, both
//! sit in the N≥3 / shared-anchor class the hand-written schedules never
//! reached, and both are pinned as executable records that assert today's
//! behaviour and flip when the ruling closes them: a reconcile re-upload of
//! already-incorporated content still destroys a peer's edit, and concurrent
//! same-anchor appends quiesce on N different permutations with no churn at
//! all. Binding prose + the adjudication owed: `conflicts.md` § Concurrent
//! resolution & ancestor freshness, the two-open-gaps block.

#![cfg(feature = "format_text")]

use fauna_core::format::ConflictPolicy;
use fauna_core::format_text::TextAdapter;

use crate::causal;
use crate::conflict_resolver::{ConflictResolution, resolve_conflict};

/// Which content a device records as its merge ancestor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BasePolicy {
    /// **Clause 4 as ratified 2026-08-02.** The base advances to whatever this
    /// device last wrote — an applied remote, a merge result, or its own
    /// publication once the nest echoes it back. Converges, because the
    /// ancestor always moves; loses a sibling's edit, because a self-authored
    /// version is not a common ancestor with a peer.
    SelfAuthoredAdvance,
    /// **The first candidate fix (2026-08-02).** A self-authored advance parks
    /// the ancestor it supersedes, if-absent, and any remote differing from the
    /// live base is measured against the parked one. Never loses an edit; does
    /// not converge, because the parked ancestor never advances.
    ParkIfAbsent,
    /// **The candidate this module exists to judge.** The ancestor is the last
    /// content this device knows a PEER had: it advances on an applied remote
    /// and on the remote side of a merge, and never on this device's own
    /// publications or merge results.
    LastPeerContent,
    /// **Parked, but advanced by a DESCENDANT TEST.** The ancestor stays at the
    /// last version every writer shared, exactly as [`Self::ParkIfAbsent`], so a
    /// sibling always merges from a true common ancestor. What unsticks it is
    /// that a merge whose result equals the incoming version proves that
    /// version already contained everything this device had — i.e. the peer
    /// HAS incorporated this device's work — so the ancestor may honestly
    /// advance to it and the parked slot retire. Convergence without a parent
    /// pointer, decided from content alone.
    ParkedUntilIncorporated,
}

struct Seat {
    name: &'static str,
    local: Vec<u8>,
    base: Option<Vec<u8>>,
    parked: Option<Vec<u8>>,
    seen: usize,
}

/// One recorded change on the nest, in the total order `sync_changes` gives.
struct Row {
    author: usize,
    content: Vec<u8>,
}

/// Process one row for a legacy-policy seat, mirroring [`causal_apply`]'s
/// shape: returns the content to publish as a new row, if the outcome calls
/// for one. Extracted so the schedule fuzzer below drives the SAME policy
/// code these hand-written tests do — a second transcription would let the
/// fuzzer refute a policy that is not the one the design record models.
fn legacy_apply(
    policy: BasePolicy,
    seat: &mut Seat,
    idx: usize,
    author: usize,
    content: &[u8],
    i: usize,
) -> Option<Vec<u8>> {
    if author == i {
        // Own echo: never fed to the apply path (it would fast-forward a
        // just-merged file back to the loser). It advances the ancestor only,
        // and only while the local file still matches what was published.
        if seat.local == content {
            match policy {
                BasePolicy::SelfAuthoredAdvance => seat.base = Some(content.to_vec()),
                BasePolicy::ParkIfAbsent | BasePolicy::ParkedUntilIncorporated => {
                    if seat.parked.is_none() {
                        seat.parked = seat.base.clone();
                    }
                    seat.base = Some(content.to_vec());
                }
                BasePolicy::LastPeerContent => {}
            }
        }
        return None;
    }
    let remote = content.to_vec();
    if remote == seat.local {
        return None;
    }

    // The ancestor this policy offers for THIS remote.
    let ancestor: Option<Vec<u8>> = match policy {
        BasePolicy::SelfAuthoredAdvance | BasePolicy::LastPeerContent => seat.base.clone(),
        BasePolicy::ParkIfAbsent | BasePolicy::ParkedUntilIncorporated => {
            match (&seat.base, &seat.parked) {
                (Some(b), Some(p)) if &remote != b => Some(p.clone()),
                (b, _) => b.clone(),
            }
        }
    };

    let diverged = match &ancestor {
        Some(anc) => &seat.local != anc && &remote != anc,
        None => false,
    };

    if !diverged {
        if ancestor.as_deref() == Some(&remote[..]) {
            return None; // already known
        }
        // Fast-forward: the remote is presumed to descend from us.
        seat.local = remote.clone();
        seat.base = Some(remote);
        seat.parked = None;
        return None;
    }

    let resolution = resolve_conflict(
        ConflictPolicy::Auto,
        &TextAdapter,
        ancestor.as_deref(),
        &seat.local,
        1_700_000_000_000 + i as i64,
        &remote,
        1_700_000_000_000 + idx as i64,
    );
    match resolution {
        ConflictResolution::Merged(merged) => {
            let publish = merged != seat.local;
            if publish {
                seat.local = merged.clone();
            }
            match policy {
                // The merge RESULT is self-authored.
                BasePolicy::SelfAuthoredAdvance => seat.base = Some(merged.clone()),
                BasePolicy::ParkIfAbsent => {
                    if seat.parked.is_none() {
                        seat.parked = seat.base.clone();
                    }
                    seat.base = Some(merged.clone());
                }
                BasePolicy::ParkedUntilIncorporated => {
                    // The descendant test: a merge that yields the incoming
                    // version verbatim proves the peer already held everything
                    // this device had.
                    if merged == remote {
                        seat.base = Some(remote);
                        seat.parked = None;
                    } else {
                        if seat.parked.is_none() {
                            seat.parked = seat.base.clone();
                        }
                        seat.base = Some(merged.clone());
                    }
                }
                // The REMOTE we just folded in is what a peer had.
                BasePolicy::LastPeerContent => seat.base = Some(remote),
            }
            publish.then_some(merged)
        }
        ConflictResolution::IncomingWins => {
            seat.local = remote.clone();
            seat.base = Some(remote);
            seat.parked = None;
            None
        }
        ConflictResolution::LocalWins => {
            if policy == BasePolicy::LastPeerContent {
                seat.base = Some(remote);
            }
            Some(seat.local.clone())
        }
    }
}

/// Run every seat to quiescence under `policy`, returning the final contents.
fn converge(policy: BasePolicy, seats: usize, max_rounds: usize) -> Result<Vec<Vec<u8>>, String> {
    let names = ["a", "b", "c", "d"];
    let base0: Vec<u8> = (0..seats)
        .map(|i| format!("{}: base\n", names[i]))
        .collect::<String>()
        .into_bytes();

    let mut nest: Vec<Row> = Vec::new();
    let mut state: Vec<Seat> = (0..seats)
        .map(|i| Seat {
            name: names[i],
            local: base0.clone(),
            base: Some(base0.clone()),
            parked: None,
            seen: 0,
        })
        .collect();

    // Every seat edits its OWN line concurrently, from the shared base, and
    // publishes. Non-overlapping hunks: the only correct convergence is the
    // union of all N edits.
    for i in 0..seats {
        let edited = String::from_utf8(state[i].local.clone())
            .unwrap()
            .replace(
                &format!("{}: base", names[i]),
                &format!("{}: EDITED", names[i]),
            )
            .into_bytes();
        state[i].local = edited.clone();
        // Deliberately does NOT touch the ancestor here: a device learns of its
        // own write only when the nest ECHOES it back, and the echo is applied
        // after any peer rows already ahead of it in the log. Advancing at
        // publication time models a mitigation production does not have, and
        // hides the two-seat case that is genuinely green.
        nest.push(Row {
            author: i,
            content: edited,
        });
    }

    for round in 0..max_rounds {
        let mut progressed = false;
        #[allow(clippy::needless_range_loop)]
        // `state[i]` is mutated inside the loop *and* `nest[idx].author == i`
        // compares against the index itself — the seat number is the model's
        // identity here, not just a cursor, so an iterator would need both the
        // index and a mutable borrow anyway.
        for i in 0..seats {
            while state[i].seen < nest.len() {
                let idx = state[i].seen;
                state[i].seen += 1;
                let author = nest[idx].author;
                let content = nest[idx].content.clone();
                if author != i && content != state[i].local {
                    progressed = true;
                }
                if let Some(published) =
                    legacy_apply(policy, &mut state[i], idx, author, &content, i)
                {
                    nest.push(Row {
                        author: i,
                        content: published,
                    });
                }

                if nest.len() > 400 {
                    return Err(format!(
                        "runaway: {} rows by round {round} — the ancestor is not advancing, so \
                         every round regenerates content the peers merge again. Final size on \
                         seat {}: {} bytes (started at {})",
                        nest.len(),
                        state[i].name,
                        state[i].local.len(),
                        base0.len()
                    ));
                }
            }
        }
        if !progressed {
            break;
        }
    }

    Ok(state.into_iter().map(|s| s.local).collect())
}

/// Every seat's own edit must survive on every seat, and all seats must agree.
fn assert_converged_union(policy: BasePolicy, seats: usize) -> Result<(), String> {
    let names = ["a", "b", "c", "d"];
    let finals = converge(policy, seats, 40)?;
    let first = String::from_utf8_lossy(&finals[0]).to_string();
    for (i, f) in finals.iter().enumerate() {
        let got = String::from_utf8_lossy(f).to_string();
        if got != first {
            return Err(format!(
                "seats disagree: {} has {got:?}, a has {first:?}",
                names[i]
            ));
        }
        for (j, name) in names.iter().enumerate().take(seats) {
            if !got.contains(&format!("{name}: EDITED")) {
                return Err(format!(
                    "seat {} LOST seat {}'s edit — converged on {got:?}",
                    names[i], names[j]
                ));
            }
        }
    }
    Ok(())
}

/// TWO seats cannot tell the three policies apart — which is exactly why the
/// defect shipped. Every policy passes here.
#[test]
fn two_seats_converge_under_every_policy() {
    for policy in [
        BasePolicy::SelfAuthoredAdvance,
        BasePolicy::ParkIfAbsent,
        BasePolicy::LastPeerContent,
    ] {
        assert_converged_union(policy, 2)
            .unwrap_or_else(|e| panic!("two seats should converge under {policy:?}: {e}"));
    }
}

/// The ratified clause-4 policy LOSES an edit at three seats — the leg-4 red,
/// reproduced in milliseconds. Kept as an executable record of why the policy
/// changed; if this ever stops failing, the model has drifted from the bug.
#[test]
fn three_seats_lose_an_edit_under_the_self_authored_advance() {
    let err = assert_converged_union(BasePolicy::SelfAuthoredAdvance, 3)
        .expect_err("the self-authored advance must still lose an edit at three seats");
    assert!(
        err.contains("LOST"),
        "expected a lost edit, got a different failure: {err}"
    );
}

/// ⚠ **A known limit of this model, kept as a warning, not a green light.**
/// `ParkIfAbsent` converges *here* — but in production it produced the
/// unbounded duplicated-line growth of `test_text_merge.py` (2026-08-02,
/// 33 minutes to observe). The model damps what production does not: it skips a
/// remote equal to local and only publishes when a merge changes something,
/// whereas a real host also re-delivers duplicate rows and re-uploads on
/// reconcile, and every one of those re-enters a merge against an ancestor that
/// never advances. So this assertion pins the model's *limit*: do not revive
/// `ParkIfAbsent` on the strength of this oracle. Convergence must come from the
/// ancestor advancing, which is what `ParkedUntilIncorporated` adds.
#[test]
fn park_if_absent_converges_only_because_this_model_damps_the_churn() {
    assert_converged_union(BasePolicy::ParkIfAbsent, 3)
        .expect("model-level convergence — see this test's doc comment before trusting it");
}

/// A peer's version is not a common ancestor either, once peers are concurrent
/// with EACH OTHER: seat `c` merges `a`'s version, adopts it as the ancestor,
/// then reads `b`'s concurrent version as reverting `a`'s edit. Recorded as an
/// executable refutation — the obvious "track what the other side had" fix.
#[test]
fn three_seats_lose_an_edit_under_last_peer_content() {
    let err = assert_converged_union(BasePolicy::LastPeerContent, 3)
        .expect_err("last-peer-content must still lose an edit at three seats");
    assert!(
        err.contains("LOST") || err.contains("disagree"),
        "expected a lost edit, got: {err}"
    );
}

/// ⚠ **Converges here at 2–4 seats, and STILL ran away in production.** This is
/// the sharpest warning in the module. The descendant test (advance the ancestor
/// when a merge yields the incoming version verbatim, proving the peer already
/// held this device's work) makes the model converge on the union at every seat
/// count — and a full e2e run on 2026-08-02 was **green on
/// `test_filesync_threeseat.py` and `test_filesync_twoseat.py` yet still red on
/// `tests/platform/sync/test_text_merge.py`** with the same unbounded
/// duplicated-line growth. The reason the test fires too rarely in production:
/// while both devices keep folding in each other's new content, no merge ever
/// equals its incoming version, so the park is never retired and the ancestor
/// stays pinned at the original base.
///
/// So this assertion pins **model behaviour, not a green light**. A candidate
/// that passes here has cleared the cheap filter and nothing more; it still owes
/// `test_text_merge.py`, which is the only instrument that has caught the
/// non-convergence mode so far.
#[test]
fn parked_until_incorporated_converges_in_the_model_but_not_in_production() {
    for seats in 2..=4 {
        assert_converged_union(BasePolicy::ParkedUntilIncorporated, seats).unwrap_or_else(|e| {
            panic!("model-level convergence at {seats} seats — see the doc comment: {e}")
        });
    }
}

// ────────────────────────────────────────────────────────────────────────────
// The causal-watermark model (the 2026-08-02 ruling — see the module doc).
//
// Seqs: the virtual initial version is seq 0 (every seat starts holding it);
// nest row at index `i` has seq `i + 1`. A row's `derived_through` is the seq
// through which its AUTHOR had incorporated every row when the content was
// produced — the model stamps it the way production does: the author's
// contiguous consumed prefix at capture, never higher.
// ────────────────────────────────────────────────────────────────────────────

/// One recorded change carrying the causal watermark.
#[derive(Clone)]
struct CRow {
    author: usize,
    content: Vec<u8>,
    derived_through: usize,
    /// The second additive wire bit of the ruling: true when this row is a
    /// pure RESOLUTION (an auto-resolve merge result / resolution winner),
    /// false for a fresh user edit. A stale resolution (`derived_through <
    /// frontier`) derives entirely from rows an in-order consumer has already
    /// incorporated — it adds zero information and is SKIPPED; a stale fresh
    /// edit carries novel content and must merge. Without this bit a receiver
    /// cannot tell the two apart, and a stale intermediate resolution can
    /// latest-wins its way over a fresher local state (measured in this model
    /// at 4 seats before the bit existed).
    is_resolution: bool,
    /// The additive retention marker (loser-row ruling, 2026-08-05): true on
    /// the nest-minted loser-retention row of a resolved report. Receivers
    /// account it into the path frontier and do nothing else
    /// ([`causal::IncomingVerdict::RetentionRow`]).
    is_retention: bool,
}

struct CSeat {
    name: &'static str,
    local: Vec<u8>,
    /// The live base slot — still advances per clause 4 (self-echo, applied
    /// remote, merge result). Under the causal licence its advance is no
    /// longer dangerous, because "descendant" is decided by the watermark.
    base: Vec<u8>,
    /// The path FRONTIER — the newest seq this seat's local content reflects,
    /// authored or applied (echo stamp, verbatim apply, merge, equal-content
    /// hold all advance it). The licence threshold: an incoming row may only
    /// fast-forward when its watermark dominates this. NOT merely "my last
    /// publication": a head can incorporate my publication yet miss a peer row
    /// I merged after it, and fast-forwarding onto it would regress that row.
    frontier: usize,
    /// The EDIT-frontier (gap-2 ruling) — the newest seq carrying NOVEL
    /// content this seat's local reflects: advanced by non-resolution rows
    /// applied/merged/authored (and by edits folded under a batch carrier),
    /// never by resolutions or skipped reissues. Upper-bound law: never
    /// under-state it — an under-count licences a regressive adopt; an
    /// over-count only strands (the pre-ruling behaviour).
    edit_frontier: usize,
    /// The CONTENT frontier (`conflicts.md` § *Retention rows are transparent
    /// to the licence*, 2026-09-27) — production's
    /// `CausalStore::content_frontier`, mirrored exactly: `None` until the
    /// first retention row is accounted on a tracked path (then pinned at the
    /// pre-row frontier), advanced with the frontier everywhere else. Rule 4
    /// reads it in place of the full frontier.
    content_frontier: Option<usize>,
    /// Production's `own_novel_in_flight` path flag (`engine.rs:592`),
    /// modeled EXACTLY — the 2026-08-05 leg-4 live refutation replaced the
    /// model's previous shape here. The model used to keep a seq-exact
    /// `BTreeSet` of own published-unechoed rows and fold its max into the
    /// effective edit-frontier at every judgement; that abstraction is
    /// STRICTLY SAFER than production (a covering row reads stale and is
    /// skip-accounted instead of adopted or deferred-and-dropped), which is
    /// precisely why two armed e2e runs RED'd leg 4 while this model stayed
    /// green. Production's actual mechanism, now mirrored:
    ///
    /// - **Armed** only at the resolver arms — a merge write (`WriteMerged`,
    ///   `engine.rs:5413`) and a kept local winner (`KeepLocal`, `:5453`) put
    ///   novel content into local whose carrier is the resolved report's
    ///   rows, at seqs this side cannot know until they are listed. Ordinary
    ///   uploads do NOT arm: their record lands at a known seq and any later
    ///   listing contains it before any later row (log contiguity), so the
    ///   pre-pass ([`pre_pass`]) counts it in time.
    /// - **Read** at two guards: a covering FastForward of a resolution (or
    ///   held-content row) is DEFERRED while the flag is set
    ///   (`engine.rs:5319-5330`), and the author-blind re-adopt refuses
    ///   (`:5701`).
    /// - **Cleared** when an own echo's bytes come back equal to local
    ///   (`:5714`), at the re-adopt itself (`:5727`), and by the partition
    ///   pre-pass on ANY own non-resolution row in a listing — the own
    ///   retention row included (`:3768`, `:3791`; the 2026-08-05 branch-side
    ///   fix — dropping that clear deferred every covering winner forever).
    ///
    /// Since 2026-08-05 (the same-anchor ruling's overlapping-reports fix,
    /// found by this model's plain-schedule search): the window is PER
    /// REPORT, not one bit — the CONTENTS of filed-but-unlisted winner rows.
    /// A single bool let an OLDER report's winner listing clear the window
    /// while a NEWER report's novelty-carrying winner was still unlisted,
    /// releasing the covering adopt over it. Armed at the resolver arms
    /// (winner bytes pushed), retired one occurrence per own winner
    /// listed/echoed; the window is open while non-empty. Production's
    /// analogue: the per-path in-flight set keyed by the winner MANIFEST.
    pending_winners: Vec<Vec<u8>>,
    /// (seq, content) versions this seat holds — the production analogue is
    /// the per-path ledger + the content-addressed chunk cache. Seeded with
    /// (0, base0). The causal merge ancestor for an incoming watermark `w` is
    /// the newest entry with seq ≤ w.
    ledger: Vec<(usize, Vec<u8>)>,
    /// Contiguous consumed prefix (count of nest rows processed).
    seen: usize,
    /// Production's `recorded_content_hash` (`db.rs`, one value per path,
    /// stamped from local at each landed record — `engine.rs:1733`, `:4903`,
    /// `:5449`): the LAST bytes whose record provably landed. The licence's
    /// `local == base` conjunct is a proxy for "no unpublished local work",
    /// and the proxy lags: the base advances on the ECHO, so between a
    /// publication and its echo the local file reads as unpublished
    /// divergence. A covering resolution arriving in that window was then
    /// MERGED from the pre-publication ancestor — and since it already
    /// incorporates the receiver's published edit, the merge DUPLICATES it
    /// (the live e2e leg-4a signature: every seat duplicated two of three
    /// appends). Recorded bytes are nest-retained, so adopting over them
    /// loses nothing irrecoverable — exactly the property the conjunct
    /// exists to protect. ⚠ ONE value, not a set: the model's previous
    /// ever-growing `sent` list licensed adopts over ANY historically
    /// published bytes — stronger than production, and the leg-4 refutation
    /// (2026-08-05) retired every such too-strong shape from this model.
    last_recorded: Option<Vec<u8>>,
    /// A FRESH BIND (the cap-release ruling, 2026-09-21): production's path
    /// frontier is `None` until the first row on the path is accounted, and
    /// `judge_incoming` has a distinct arm for that state. Every seeded seat
    /// in this model starts TRACKED at 0 (its ledger holds row 0, the seed),
    /// which is why the untracked arm was never fuzzed while it parked
    /// production. A fresh-bind seat reports no frontier until its first
    /// advance (seqs start at 1, so `frontier > 0` is "accounted something").
    fresh_bind: bool,
}

impl CSeat {
    /// The frontiers every judgement reads: the STORED pair, exactly as
    /// production passes them (`engine.rs:5305` — `self.causal().frontiers`).
    /// Nothing is folded in: the protection for own unechoed novelty is the
    /// partition pre-pass (seqs in hand, [`pre_pass`]) plus the
    /// `report_pending` defer for the unknown-seq report window — the
    /// 2026-08-05 leg-4 refutation replaced the model's previous
    /// in-flight-max fold, which was strictly safer than what ships.
    fn frontiers(&self) -> causal::PathFrontiers {
        causal::PathFrontiers {
            frontier: (!self.fresh_bind || self.frontier > 0).then_some(self.frontier as i64),
            edit_frontier: Some(self.edit_frontier as i64),
            content_frontier: self.content_frontier.map(|c| c as i64),
        }
    }

    /// Production's `CausalStore::advance_frontier`: every accounting site
    /// but a retention row's — a tracked content frontier moves with it.
    fn advance_frontier(&mut self, seq: usize) {
        if let Some(c) = self.content_frontier.as_mut() {
            *c = (*c).max(seq);
        }
        self.frontier = self.frontier.max(seq);
    }

    /// Production's `CausalStore::account_retention_row`: the frontier
    /// advances, the content frontier does not (pinned at the pre-row
    /// frontier when untracked; an untracked FRONTIER pins nothing). The
    /// model's edit-frontier is always tracked, so its pin is a no-op here.
    fn account_retention(&mut self, seq: usize) {
        if let Some(before) = self.frontiers().frontier
            && seq as i64 > before
            && self.content_frontier.is_none()
        {
            self.content_frontier = Some(before as usize);
        }
        self.frontier = self.frontier.max(seq);
    }

    /// The licence's unpublished-work conjunct, sharpened per the gap-3
    /// ruling: local content equal to the live base OR to the last recorded
    /// bytes is provably recoverable (retained nest-side), so an
    /// adopt/fast-forward over it destroys nothing. Only genuinely
    /// unpublished local work refuses the verbatim licences.
    /// (`engine.rs:5278-5285` — base equality OR `recorded_content_hash`.)
    fn no_unpublished_local_work(&self) -> bool {
        self.local == self.base || self.last_recorded.as_ref() == Some(&self.local)
    }

    /// Production's `holds_content_at_or_below(path, frontier, local)` — the
    /// held-bytes conjunct of the cap's firing rule (2026-09-21): local bytes
    /// the ledger already holds at seq ≤ the frontier are not novelty.
    fn local_held_at_or_below_frontier(&self) -> bool {
        self.frontiers().frontier.is_some_and(|f| {
            self.ledger
                .iter()
                .any(|(s, c)| *s as i64 <= f && *c == self.local)
        })
    }

    /// The newest held `(seq, bytes)` at or below `w` — the seq is what
    /// production stamps as `losing_derived_through` (the ledger entry the
    /// merge actually read, `engine.rs::auto_resolve_conflict`).
    fn ancestor_for(&self, w: usize) -> (usize, Vec<u8>) {
        self.ledger
            .iter()
            .rev()
            .find(|(s, _)| *s <= w)
            .map(|(s, c)| (*s, c.clone()))
            .unwrap_or_else(|| {
                // A FRESH-BIND seat holds nothing yet: production's ladder
                // falls from the causal ancestor to the live base slot
                // (`resolve_divergence`), empty on a fresh bind — a two-way
                // merge. Every seeded seat holds row 0, so only a fresh
                // seat can get here.
                assert!(
                    self.fresh_bind,
                    "ledger is seeded with seq 0 — a causal ancestor always exists"
                );
                (0, self.base.clone())
            })
    }
    fn hold(&mut self, seq: usize, content: &[u8]) {
        if self.ledger.iter().all(|(s, _)| *s != seq) {
            self.ledger.push((seq, content.to_vec()));
            self.ledger.sort_by_key(|(s, _)| *s);
        }
    }
}

/// How rows reach a seat.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Delivery {
    /// Every row is processed individually (the shape the legacy model uses).
    PerRow,
    /// All unseen rows arrive as one batch, and the batch-latest fold applies.
    /// The causal fold is ALL OR NOTHING per batch: earlier same-path rows are
    /// folded only when the batch-latest row is a PEER row whose
    /// `derived_through` covers the seq of EVERY earlier batch row — the one
    /// row then adopted carries all the folded information, so the frontier
    /// stays honest. A per-row fold ("each row folds under any later covering
    /// row") is REFUTED by this model: it leaves a hole when an intermediate
    /// fast-forward raises the frontier past the designated carrier's
    /// watermark, and the carrier is then skipped as a stale resolution with
    /// the folded rows' content never delivered (measured here at 3 seats).
    /// Production's pre-ruling fold skipped unconditionally with no causal
    /// test at all, which is the b-side loss.
    BatchedWithFold,
}

/// What one processed row makes a causal seat publish — production's TWO
/// write shapes, added 2026-08-05 (the report-plane remainder, `conflicts.md`
/// § Concurrent resolution's ⚠ OPEN REMAINDER block). The model previously
/// collapsed both into "one resolution row at the capture prefix", and that
/// abstraction is exactly why the fuzzer validated the gap-3 ruling while the
/// live 3-seat run refuted its completeness: a daemon merge does not publish
/// one row — it files a RESOLVED REPORT, and the nest's transaction mints a
/// loser-retention row plus the winner (`sync_storage.rs::report_conflict`).
#[derive(Clone, Debug)]
enum CPublish {
    /// A direct single-row publish — the author-arm re-assert (production:
    /// `upload_file`, stamped a resolution by the widened proven-reissue
    /// stamp). One row at the capture prefix, as the model always minted.
    Direct(Vec<u8>),
    /// The divergence path filed a resolved report; the NEST mints the rows.
    Report {
        /// The head every device converges on (merge result, or the winning
        /// candidate under latest-wins).
        winner: Vec<u8>,
        /// Attribution follows the choose-winner precedent
        /// (`folder_handlers.rs`): a merged (non-candidate) winner belongs
        /// to the reporter; a candidate winner to the candidate's device —
        /// so an IncomingWins winner row returns to the PEER as an own echo.
        winner_author: usize,
        /// The reporter's pre-merge candidate — the version that would
        /// otherwise exist nowhere (its device produced it but never recorded
        /// it; the conflict pre-empted that). `None` when the reporter's own
        /// candidate IS the winner (LocalWins): the nest then mints no loser
        /// row.
        loser: Option<Vec<u8>>,
        /// `losing_derived_through` — the seq of the ledger entry the merge
        /// actually read. In a cascade this is a DEEP ancestor: the loser row
        /// re-enters peers' merge arms from it.
        ancestor_seq: usize,
        /// `winning_derived_through` — the reporter's claim, HONEST under the
        /// lower-bound law (the leg-4 ruling, 2026-08-05): the incoming
        /// row's seq when that row is contiguous with the reporter's
        /// pre-merge frontier (`seq == frontier + 1` — the merge then
        /// provably incorporates every row ≤ seq), else the pre-merge
        /// frontier itself. The pre-ruling stamp claimed `incoming_seq`
        /// unconditionally, which OVER-CLAIMS whenever unconsumed rows sit
        /// below the incoming row — a licensed fast-forward had dropped a
        /// peer's published edit from local, the merge missed it, and the
        /// inflated claim then read as covering at every seat whose
        /// edit-frontier honestly counted that edit: the leg-4 lost line.
        winning_w: usize,
        /// Conjunct 2 of the same-anchor ruling (2026-08-05): the report
        /// consumed UNPUBLISHED local novelty — pre-merge content with no
        /// seq anywhere (≠ base, ≠ the recorded witness). The winner row is
        /// then that novelty's ONLY surviving carrier, and under the
        /// ratified `is_resolution` semantics ("carries no novel content" —
        /// the gap-3 widening) a resolution stamp on it is a LIE: receivers
        /// stale-skip it byte-free, the novelty's last off-disk copy dies,
        /// and the skippers' frontiers still feed claims that later adopt
        /// the on-disk copy away (the armed-run loss). The winner therefore
        /// mints EDIT-class: stale receivers MERGE it (lossless under the
        /// conjunct-1 union), their edit-frontiers count it, and the
        /// reporter's own echo binds it through the standing
        /// non-resolution accounting.
        winner_carries_novelty: bool,
    },
}

/// Mint the nest rows one publish produces — the transactional shape of
/// `sync_storage.rs::report_conflict` for a report, one row for a direct
/// publish. `capture_w` stamps only the direct shape (the reporter's
/// contiguous consumed prefix at capture).
fn mint_publish(nest: &mut Vec<CRow>, reporter: usize, capture_w: usize, p: CPublish) {
    match p {
        CPublish::Direct(content) => nest.push(CRow {
            author: reporter,
            content,
            derived_through: capture_w,
            is_resolution: true,
            is_retention: false,
        }),
        CPublish::Report {
            winner,
            winner_author,
            loser,
            ancestor_seq,
            winning_w,
            winner_carries_novelty,
        } => {
            // First the reporter's LOSING candidate as an ordinary change row
            // (version retention): edit-class, stamped at the reporter's
            // merge-ancestor seq — "a fresh edit causally (the reporter's
            // pre-merge local), never `is_resolution`" — and, since the
            // loser-row ruling, wearing the RETENTION marker receivers
            // account-and-skip on.
            if let Some(content) = loser {
                nest.push(CRow {
                    author: reporter,
                    content,
                    derived_through: ancestor_seq,
                    is_resolution: false,
                    is_retention: true,
                });
            }
            // The winner's claim is minted EXACTLY as sent (`conflicts.md` §
            // *Retention rows are transparent to the licence*, 2026-09-27):
            // the gap-2 winner-stamp upgrade to the loser row's seq is
            // retired — a reporter cannot sign a seq the nest assigns after
            // it signs, and receivers now read rule 4 against the content
            // frontier, which the retention row never advances.
            nest.push(CRow {
                author: winner_author,
                content: winner,
                derived_through: winning_w,
                // The honest class (conjunct 2, 2026-08-05): a winner that
                // consumed unpublished novelty CARRIES novel content, so the
                // ratified `is_resolution` meaning forbids the resolution
                // stamp on it.
                is_resolution: !winner_carries_novelty,
                is_retention: false,
            });
        }
    }
}

/// Record the publish in the reporter's `last_recorded` slot — production's
/// `recorded_content_hash`, stamped from local when the record lands. For a
/// report the reporter's final local content IS the winner on every arm
/// (merged result / kept local / applied incoming), and the loser candidate —
/// though uploaded for retention — never becomes the entry's recorded content
/// (`engine.rs`: the stamp follows the entry's final manifest, not the
/// retention upload).
fn note_published(last_recorded: &mut Option<Vec<u8>>, _reporter: usize, p: &CPublish) {
    match p {
        CPublish::Direct(c) => *last_recorded = Some(c.clone()),
        CPublish::Report { winner, .. } => *last_recorded = Some(winner.clone()),
    }
}

/// Production's partition PRE-PASS (`engine.rs:3742-3793`), run over a
/// listing before ANY of its rows is judged: own NON-resolution create/modify
/// rows fold their seqs into the STORED edit-frontier — the seqs are in hand,
/// so this is the exact accounting the in-flight window exists to
/// approximate — and every such row closes the unknown-seq window the
/// resolver arms opened (`report_pending`), the own RETENTION row included
/// (which advances nothing: its bytes bind no novelty — the 2026-08-05
/// branch-side fix; dropping that clear deferred every covering winner
/// forever). The reissue witness mirrors production's: the same bytes seen
/// earlier among this listing's own rows (the original + its retry listed
/// together), or bytes the ledger already holds at seq ≤ the frontier.
fn pre_pass(seat: &mut CSeat, nest: &[CRow], batch: &[usize], i: usize) {
    let mut seen_batch_bytes: Vec<Vec<u8>> = Vec::new();
    for &idx in batch {
        let row = &nest[idx];
        if row.author != i {
            continue;
        }
        if row.is_retention {
            // Conjunct 2 of the same-anchor ruling (2026-08-05): the LOSER
            // row no longer closes the report window — the pre-2026-08-05
            // clear here released a covering adopt while the reporter's
            // consumed pre-merge novelty had no edit-frontier seq, which is
            // the armed-run silent loss. The WINNER row (transactionally
            // co-minted, so at most one listing boundary behind) is what
            // closes the window now, in the arm below.
            continue;
        }
        if row.is_resolution {
            // An own WINNER row retires ITS report's window entry — one
            // occurrence, by content: with overlapping reports a blanket
            // clear let an older winner release a newer report's
            // still-unlisted novelty carrier (this model's plain-schedule
            // search found the adopt that follows). A reseal matches
            // nothing and retires nothing.
            if let Some(k) = seat.pending_winners.iter().position(|w| *w == row.content) {
                seat.pending_winners.remove(k);
            }
            continue;
        }
        let batch_dup = seen_batch_bytes.contains(&row.content);
        seen_batch_bytes.push(row.content.clone());
        let remembered = seat
            .ledger
            .iter()
            .any(|(s, c)| *s <= seat.frontier && c == &row.content);
        if !(batch_dup || remembered) {
            seat.edit_frontier = seat.edit_frontier.max(idx + 1);
        }
        // An EDIT-class winner (conjunct 2) lands here; ordinary edit
        // echoes match no pending winner and retire nothing.
        if let Some(k) = seat.pending_winners.iter().position(|w| *w == row.content) {
            seat.pending_winners.remove(k);
        }
    }
}

/// What processing one row did — the leg-4 ruling's second conjunct
/// (2026-08-05) splits the covering-adopt defer OUT of the ordinary
/// outcomes: it is a transient-class CAP (the seal-cap / unresolved-cap
/// family, `engine.rs:3674-3721`), never a consumed skip. Everything at and
/// past the deferred seq waits for the next listing; the cursor (production:
/// the pull anchor) stays below it, so the row RE-LISTS once the report's
/// own rows have closed the unknown-seq window. The pre-ruling engine
/// consumed-and-dropped the row instead (`return Ok(())` with the anchor
/// advancing regardless), which permanently discarded the very covering
/// winners that carry the fleet's convergence — the leg-4 repair rows.
enum CApply {
    Done(Option<CPublish>),
    Defer,
}

impl CApply {
    /// Unwrap for the hand-written scripted tests, which never construct a
    /// defer window.
    #[track_caller]
    fn done(self) -> Option<CPublish> {
        match self {
            CApply::Done(p) => p,
            CApply::Defer => panic!("unexpected covering-adopt defer in a scripted test"),
        }
    }
}

fn trace(on: bool, msg: impl FnOnce() -> String) {
    if on {
        eprintln!("{}", msg());
    }
}

fn tracing_on() -> bool {
    std::env::var("ORACLE_TRACE").is_ok()
}

/// Process one row for a causal seat. Returns what the seat publishes, if
/// the outcome calls for one — in production's shape ([`CPublish`]): the
/// divergence path always files a resolved report (whatever the resolver
/// said), the author-arm re-assert publishes directly.
fn causal_apply(
    seat: &mut CSeat,
    seq: usize,
    row: &CRow,
    i: usize,
    idx_ts: i64,
    claim_w: usize,
) -> CApply {
    let t = tracing_on();
    trace(t, || {
        format!(
            "seat {} <- seq {} (author {}, w {}, res {}): local={:?} base_eq_local={} \
             frontier={} edit_frontier={}",
            seat.name,
            seq,
            row.author,
            row.derived_through,
            row.is_resolution,
            String::from_utf8_lossy(&seat.local).replace('\n', "|"),
            seat.local == seat.base,
            seat.frontier,
            seat.edit_frontier
        )
    });
    // ── The AUTHOR ARM, first and whole — production's partition mirrored:
    // own rows never reach rules 1–6, both hosts route them to
    // `apply_self_echo` before the peer rules run. Ordering matters twice
    // over: an own row consumed by rule 1 (frontier already past it via
    // same-batch skip-accounts) would silently drop both its novelty
    // accounting AND the author-blind re-adopt (measured: a seat adopting a
    // "covering" resolution over its own rule-1-swallowed edit; and the
    // log-tail publisher stranded on an earlier adopt because its own tail
    // echo arrived rule-1-late).
    if row.author == i {
        // An own NON-resolution row's novelty lands in the STORED
        // edit-frontier in the same event, whichever path the row took to
        // get here — UNLESS the bytes are already held at a lower seq (the
        // gap-3 ruling: novelty is a property of BYTES, not of rows). An
        // own edit-stamped reissue echo — the lost-ack retry whose record
        // did land — carries content whose earliest carrier this seat
        // already counted; advancing the edit-frontier to the reissue's seq
        // is the inflation that stranded every later covering resolution
        // and permanently diverged the same-anchor fold. (The unknown-seq
        // report window is cleared by the PRE-PASS, not here — production's
        // main loop intercepts retention rows before `apply_self_echo`,
        // `engine.rs:3870`.)
        if row.is_retention {
            // The own retention row (loser-row ruling): accounted, nothing
            // else — no edit-frontier advance (the candidate's novelty was
            // counted when its content was authored), no ledger hold (it was
            // never anyone's reflected content), no re-adopt or re-assert
            // (production's main-loop interception mirrors this).
            trace(t, || "  -> own retention echo: accounted".to_string());
            seat.account_retention(seq);
            return CApply::Done(None);
        }
        let reissue_of_held = seat
            .ledger
            .iter()
            .any(|(s, c)| *s < seq && c == &row.content);
        if !row.is_resolution && !reissue_of_held {
            seat.edit_frontier = seat.edit_frontier.max(seq);
        }
        // Supersession is AUTHOR-BLIND (gap-2 ruling): an own COVERING
        // resolution is still a log entry, and if it is LATER than whatever
        // row local currently sits on, it re-adopts — even when the frontier
        // already passed it. The ordering guard is a ledger lookup (the
        // newest held row whose bytes equal local), because seq-vs-frontier
        // cannot say which row local came from. The gap-3 class UPGRADE
        // applies here too: an own edit-stamped reissue echo (bytes held at
        // a lower seq) is proven novel-content-free, so it re-adopts by the
        // same licence — without this, the reissue's author is the one seat
        // that never converges to its own log-tail row after adopting an
        // earlier sibling resolution (measured: a and b quiesced SWAPPED).
        let local_at = seat
            .ledger
            .iter()
            .rev()
            .find(|(_, c)| *c == seat.local)
            .map(|(s, _)| *s)
            .unwrap_or(0);
        // The covering test reads the STORED effective edit-frontier
        // (production: `engine.rs:5703-5708`), and the tail licence carries
        // production's `!own_novelty_in_flight` conjunct (`:5701`) — while
        // an own report's rows are unlisted the edit-frontier provably
        // under-counts, so neither re-adopt nor re-assert may fire.
        let ef_eff = seat.frontiers().effective_edit_frontier().unwrap_or(0) as usize;
        let log_later_tail = (row.is_resolution || reissue_of_held)
            && seat.local != row.content
            && seat.local == seat.base
            && seat.pending_winners.is_empty()
            && seq > local_at;
        let own_covering_adopt = log_later_tail && row.derived_through >= ef_eff;
        seat.advance_frontier(seq);
        seat.hold(seq, &row.content);
        if seat.local == row.content || own_covering_adopt {
            if own_covering_adopt {
                trace(t, || {
                    "  -> own covering resolution echo: re-adopted".to_string()
                });
                seat.local = row.content.clone();
            }
            seat.base = row.content.clone();
            // An own row's bytes came back equal to local (or local just
            // re-adopted them): this report's window entry retires — one
            // occurrence, by content (`engine.rs:5714`, `:5727`; per-report
            // since 2026-08-05, see `CSeat::pending_winners`).
            if let Some(k) = seat.pending_winners.iter().position(|w| *w == row.content) {
                seat.pending_winners.remove(k);
            }
        } else if log_later_tail {
            // RE-ASSERT on a stale-declined own tail (gap-3 ruling, the last
            // conjunct). This own novel-content-free row would re-adopt by
            // log order, and the ONLY refusal is the staleness guard — a
            // stamp-vs-content comparison that is genuinely undecidable here
            // (the edit-frontier honestly counts an unprovable reissue's seq,
            // and the row's stamp honestly under-claims; neither side can be
            // fixed — measured: the publisher of the log-tail permutation
            // stranded on an earlier adopted row while every peer converged
            // onto its tail). Clause 1 applied reflexively is the way out: a
            // resolution is an ordinary change, so the seat re-asserts its
            // CURRENT bytes as a fresh resolution at its current anchor —
            // the new log-tail row is covering by construction wherever the
            // stranded one was in content, and the fleet converges on it.
            trace(t, || {
                "  -> own stale-declined tail: re-asserting local at the current anchor".to_string()
            });
            return CApply::Done(Some(CPublish::Direct(seat.local.clone())));
        }
        return CApply::Done(None);
    }
    let frontiers = seat.frontiers();
    // The byte-free half of the licence, taken from the PRODUCTION decision
    // core rather than transcribed here — `judge_incoming_before_fetch` is the
    // same function both receivers call ahead of a download, so a divergence
    // between this model and shipped behaviour cannot hide in a copy.
    let pre = causal::judge_incoming_before_fetch(
        seq as i64,
        Some(row.derived_through as i64),
        row.is_resolution,
        row.is_retention,
        frontiers,
    );
    if pre == causal::PreFetchVerdict::Duplicate {
        // THE IDEMPOTENCE GUARD — a load-bearing part of the ruling, found by
        // this model's churn run: a row with seq ≤ the path frontier is
        // already reflected in local content, so a duplicate delivery
        // (overlapping pull, reconcile re-send) must be SKIPPED — never
        // re-applied and never re-merged. Re-merging an already-incorporated
        // row from an old ancestor produces overlapping-identical hunks, which
        // the resolver treats as unmergeable → latest-wins → a union edit
        // destroyed. This is precisely the churn class that made ParkIfAbsent
        // run away in production while passing the un-churned model; the
        // frontier is the per-path seq memory the pre-ruling system lacked.
        trace(t, || {
            "  -> duplicate (seq <= frontier): skipped".to_string()
        });
        return CApply::Done(None);
    }
    if pre == causal::PreFetchVerdict::StaleResolution {
        // A STALE RESOLUTION: a merge result that misses NOVEL content this
        // seat reflects (watermark below the EDIT-frontier — the gap-2
        // ruling's narrowed rule 2). Merging it anyway produces
        // overlapping-identical hunks that read as unmergeable, letting
        // latest-wins regress a fresher local (the 4-seat failure that forced
        // this bit onto the wire). Skip; the frontier still advances (the row
        // is accounted for), the edit-frontier does not.
        //
        // (Own rows never reach this arm — the author arm above returns
        // first, exactly like production's peer-first partition.)
        //
        // ⚠ NO `seat.hold` here — the skip is BYTE-FREE. Production acts on
        // this verdict BEFORE the download (`judge_incoming_before_fetch` is
        // the point of the pre-fetch pass), so a stale resolution's bytes
        // never enter the receiver's ledger. The model used to hold them
        // anyway, and that single over-hold masked the report-plane defect
        // for a day: a peer's merge INTERMEDIATE (published as a winner row,
        // stale-skipped here) later returns as a loser-retention row's
        // bytes, and a receiver that held them at the skip absorbs the loser
        // row through the reissue rung — a licence production does not have,
        // because it never fetched the bytes.
        trace(t, || "  -> stale resolution: skipped".to_string());
        seat.advance_frontier(seq);
        return CApply::Done(None);
    }
    if pre == causal::PreFetchVerdict::RetentionRow {
        // The retention rung (loser-row ruling): account the seq into the
        // frontier and nothing else — byte-free like the stale-resolution
        // skip (no fetch, so no ledger hold), and never an edit-frontier
        // advance whatever stamp the row wears.
        trace(t, || "  -> retention row: accounted, skipped".to_string());
        seat.account_retention(seq);
        return CApply::Done(None);
    }
    // The content-keyed idempotence rung (gap-1 ruling): a stale-watermarked
    // row whose exact bytes this seat already incorporated is a REISSUE
    // (lost-ack retry whose record landed, a re-seal) — skipped
    // before any merge. The production analogue compares the manifest's
    // content hash against hashes of held ledger bytes.
    let content_held = seat
        .ledger
        .iter()
        .any(|(s, c)| *s <= seat.frontier && c == &row.content);
    let remote = &row.content;
    if *remote == seat.local {
        seat.advance_frontier(seq);
        // Same content licence as the author arm (gap-3 ruling): an
        // equal-content arrival whose bytes are already held at a lower seq
        // is a reissue — its novelty was counted at its earliest carrier,
        // and re-counting it here inflates the edit-frontier identically.
        if !row.is_resolution {
            let reissue_of_held = seat
                .ledger
                .iter()
                .any(|(s, c)| *s < seq && c == &row.content);
            if !reissue_of_held {
                seat.edit_frontier = seat.edit_frontier.max(seq);
            }
        }
        seat.hold(seq, remote);
        seat.base = remote.clone();
        return CApply::Done(None);
    }
    // The sibling-vs-descendant call, again from the production core. The
    // settled conjunct is `local == base` AND the in-flight guard: while an
    // own novel publication awaits its echo, the edit-frontier provably
    // under-counts and no fast-forward/adopt may fire (2026-08-03, found
    // live by the 3-seat cell the moment the covering adopt shipped).
    let verdict = causal::judge_incoming(
        seq as i64,
        Some(row.derived_through as i64),
        row.is_resolution,
        row.is_retention,
        frontiers,
        content_held,
        seat.no_unpublished_local_work(),
    );
    // THE DEFER ARM (the leg-4 ruling, 2026-08-05, generalizing
    // `engine.rs:5319-5330`): a verbatim adopt is NEVER taken while this
    // seat's own pending rows are unlisted — that is (a) the unknown-seq
    // report window (`report_pending`), and (b) the ordinary ack→echo
    // window, where the licence rode the RECORDED witness alone
    // (`local != base`). Adopting in either window drops own novelty whose
    // carrier then falsely advances the frontier at its echo — the poisoned
    // state every downstream claim inherited (the leg-4 lost line). The
    // defer is a transient-class CAP ([`CApply::Defer`]): no accounting, no
    // hold; the caller keeps the cursor below this seq so the row RE-LISTS
    // — by then the pending row is listed (its record landed, and listings
    // are contiguous), the pre-pass has counted it, and the judgement is
    // exact: a truly covering row still adopts (the gap-3 anti-duplication
    // adopt, delayed, not weakened), a non-covering one merges.
    if verdict == causal::IncomingVerdict::FastForward
        && causal::verbatim_adopt_deferred(
            !seat.pending_winners.is_empty(),
            seat.local == seat.base,
            seat.local_held_at_or_below_frontier(),
        )
    {
        trace(t, || {
            "  -> verbatim adopt deferred (own pending rows unlisted) — capped for re-listing"
                .to_string()
        });
        return CApply::Defer;
    }
    let ancestor_bound = match verdict {
        causal::IncomingVerdict::ReissueOfHeldContent => {
            // Bytes this seat provably already incorporated, re-published at
            // a stale watermark: skip. The frontier advances (the row is
            // accounted for); the edit-frontier does NOT, even when the row
            // wears an edit stamp — it carries no novel content, which is
            // the whole point of keying on content rather than the stamp.
            trace(t, || "  -> reissue of held content: skipped".to_string());
            seat.advance_frontier(seq);
            return CApply::Done(None);
        }
        causal::IncomingVerdict::FastForward => {
            // Causal fast-forward: the writer provably incorporated every
            // NOVEL row this seat's local content reflects (an edit must
            // dominate the full frontier; a resolution need only dominate the
            // edit-frontier — the gap between the two is order-choices this
            // later row supersedes by nest-log order), and this seat has no
            // unpublished work.
            trace(t, || "  -> fast-forward".to_string());
            seat.local = remote.clone();
            seat.base = remote.clone();
            seat.advance_frontier(seq);
            // Content licence (gap-3 ruling), same as every other arm: a
            // caught-up writer's re-publication of held bytes fast-forwards
            // as fresh intent (the rung's recorded trade), but it carries no
            // novel content — advancing the edit-frontier to its seq strands
            // every covering resolution stamped between the bytes' earliest
            // carrier and the reissue (measured: it broke the author-blind
            // re-adopt on the publisher of the log-tail permutation).
            if !row.is_resolution {
                let reissue_of_held = seat
                    .ledger
                    .iter()
                    .any(|(s, c)| *s < seq && c == &row.content);
                if !reissue_of_held {
                    seat.edit_frontier = seat.edit_frontier.max(seq);
                }
            }
            seat.hold(seq, remote);
            return CApply::Done(None);
        }
        causal::IncomingVerdict::Diverged { ancestor_seq_bound } => ancestor_seq_bound
            .expect("the model always stamps a watermark, so the bound is present"),
        // Both byte-free verdicts were taken above, and `judge_incoming` is
        // defined in terms of the same pre-fetch call, so neither can reappear.
        other => unreachable!("byte-free verdict resurfaced after the pre-fetch pass: {other:?}"),
    };
    resolve_and_report(seat, seq, row, i, idx_ts, claim_w, ancestor_bound as usize)
}

/// The divergence tail (factored 2026-08-05, when the model also carried the
/// since-removed daemon host): merge from the
/// causally-justified ancestor — the newest version this seat holds that the
/// incoming row's writer had incorporated. Whatever the resolver answers,
/// production files a RESOLVED REPORT here — there is no silent outcome on
/// this path (`engine.rs::auto_resolve_conflict` reports on all three arms),
/// and the report retains the pre-merge candidate. The claim (`claim_w`) is
/// HOST-COMPUTED by the caller: the engine widens over own-authored listing
/// gaps.
fn resolve_and_report(
    seat: &mut CSeat,
    seq: usize,
    row: &CRow,
    i: usize,
    idx_ts: i64,
    claim_w: usize,
    ancestor_bound: usize,
) -> CApply {
    let t = tracing_on();
    let remote = &row.content;
    let (ancestor_seq, ancestor) = seat.ancestor_for(ancestor_bound);
    let pre_merge = seat.local.clone();
    // Conjunct 2 of the same-anchor ruling (2026-08-05): does this report
    // consume UNPUBLISHED local novelty — pre-merge content with no seq
    // anywhere (≠ base, ≠ the recorded witness)? If so, the report's winner
    // row is its only surviving carrier and mints EDIT-class (see
    // `CPublish::Report::winner_carries_novelty`).
    let consumed_unpublished =
        pre_merge != seat.base && seat.last_recorded.as_ref() != Some(&pre_merge);
    let resolution = resolve_conflict(
        ConflictPolicy::Auto,
        &TextAdapter,
        Some(&ancestor),
        &seat.local,
        1_700_000_000_000 + i as i64,
        remote,
        1_700_000_000_000 + idx_ts,
    );
    // The HONEST winner claim (leg-4 ruling, 2026-08-05 — the lower-bound
    // law applied to `winning_derived_through`), computed by the CALLER
    // with the listing in hand: the incoming seq when every gap row below
    // it is this seat's own (their content is reflected in local by the
    // defer-arm invariant above), else the pre-merge frontier. The
    // pre-ruling stamp claimed the incoming seq unconditionally — the
    // over-claim every covering test downstream then trusted (the leg-4
    // lost line).
    let winning_w = claim_w.min(seq);
    seat.advance_frontier(seq);
    if !row.is_resolution {
        // The judged row's novel bytes enter local content on every arm below
        // (merged in, applied, or latest-wins-judged) — upper-bound law —
        // content-licensed per the gap-3 ruling: bytes already held at a
        // lower seq were counted at their earliest carrier.
        let reissue_of_held = seat
            .ledger
            .iter()
            .any(|(s, c)| *s < seq && c == &row.content);
        if !reissue_of_held {
            seat.edit_frontier = seat.edit_frontier.max(seq);
        }
    }
    seat.hold(seq, remote);
    match resolution {
        ConflictResolution::Merged(merged) => {
            trace(t, || {
                format!(
                    "  -> merged (report): {:?}",
                    String::from_utf8_lossy(&merged).replace('\n', "|")
                )
            });
            seat.local = merged.clone();
            seat.base = merged.clone();
            // The merged head carries this device's pre-merge candidate —
            // novel content whose carrier rows have not been listed yet, at
            // seqs this side cannot know: arm the unknown-seq window
            // (`engine.rs:5413`), and — when the candidate was UNPUBLISHED —
            // the winner-carrier memory (conjunct 2, 2026-08-05).
            seat.pending_winners.push(merged.clone());
            CApply::Done(Some(CPublish::Report {
                // A merged (non-candidate) winner is attributed to the
                // reporter, who wrote it locally.
                winner: merged,
                winner_author: i,
                loser: (pre_merge != seat.local).then_some(pre_merge),
                ancestor_seq,
                winning_w,
                winner_carries_novelty: consumed_unpublished,
            }))
        }
        ConflictResolution::IncomingWins => {
            trace(t, || "  -> incoming wins (report)".to_string());
            seat.local = remote.clone();
            seat.base = remote.clone();
            CApply::Done(Some(CPublish::Report {
                // The winning candidate is the peer's — the winner row is
                // attributed to the peer's device, and returns to it as an
                // own echo. Its bytes are an existing row's: never a novelty
                // carrier.
                winner: remote.clone(),
                winner_author: row.author,
                loser: Some(pre_merge),
                ancestor_seq,
                winning_w,
                winner_carries_novelty: false,
            }))
        }
        ConflictResolution::LocalWins => {
            trace(t, || "  -> local wins (report)".to_string());
            seat.base = seat.local.clone();
            // The kept head is this device's candidate, riding the report's
            // winner row at a seq this side cannot know: arm the unknown-seq
            // window (`engine.rs:5453`) and — when the candidate was
            // UNPUBLISHED — the winner-carrier memory (conjunct 2,
            // 2026-08-05). IncomingWins deliberately does NOT arm — the
            // adopted local is the peer's existing row's bytes, no own
            // novelty rides the report.
            seat.pending_winners.push(seat.local.clone());
            CApply::Done(Some(CPublish::Report {
                // The reporter's own candidate won: no loser row (the winner
                // row below covers it — `sync_storage.rs`'s loser filter).
                winner: seat.local.clone(),
                winner_author: i,
                loser: None,
                ancestor_seq,
                winning_w,
                winner_carries_novelty: consumed_unpublished,
            }))
        }
    }
}

/// Run every seat to quiescence under the causal-watermark rules.
///
/// `churn`: re-delivers every row a second time after the seat has moved on
/// (the duplicate-row re-delivery a real host produces), which the causal
/// rules must absorb without row growth — re-processing a held row either
/// matches local, fast-forwards to known content, or merges idempotently.
fn converge_causal(
    seats: usize,
    delivery: Delivery,
    churn: bool,
    max_rounds: usize,
) -> Result<Vec<Vec<u8>>, String> {
    let names = ["a", "b", "c", "d"];
    let base0: Vec<u8> = (0..seats)
        .map(|i| format!("{}: base\n", names[i]))
        .collect::<String>()
        .into_bytes();

    let mut nest: Vec<CRow> = Vec::new();
    let mut state: Vec<CSeat> = (0..seats)
        .map(|i| CSeat {
            name: names[i],
            local: base0.clone(),
            base: base0.clone(),
            frontier: 0,
            edit_frontier: 0,
            content_frontier: None,
            pending_winners: Vec::new(),
            ledger: vec![(0, base0.clone())],
            seen: 0,
            last_recorded: None,
            fresh_bind: false,
        })
        .collect();

    // Concurrent own-line edits from the shared base; watermark 0 (nothing
    // incorporated beyond the initial version) — exactly production's stamp.
    // An ordinary upload does NOT arm the report window (`engine.rs` arms
    // only at the resolver arms); its record landed, so `last_recorded`
    // carries the recoverability witness and the pre-pass counts its seq
    // when its listing arrives.
    for i in 0..seats {
        let edited = String::from_utf8(state[i].local.clone())
            .unwrap()
            .replace(
                &format!("{}: base", names[i]),
                &format!("{}: EDITED", names[i]),
            )
            .into_bytes();
        state[i].local = edited.clone();
        state[i].last_recorded = Some(edited.clone());
        nest.push(CRow {
            author: i,
            content: edited,
            derived_through: 0,
            is_resolution: false,
            is_retention: false,
        });
    }

    let mut redelivered = vec![false; seats];
    for _round in 0..max_rounds {
        let mut progressed = false;
        for i in 0..seats {
            let end = nest.len();
            if state[i].seen < end {
                progressed = true;
                let start = state[i].seen;
                let batch: Vec<usize> = (start..end).collect();
                // The causal batch fold: a row folds under a LATER batch row
                // only when that later row provably incorporated it. Own rows
                // still stamp (they are processed, just cheaply).
                let folded: Vec<bool> = match delivery {
                    Delivery::PerRow => vec![false; batch.len()],
                    Delivery::BatchedWithFold => {
                        // The CARRIER is the latest PEER row, not the raw
                        // batch tail (2026-08-04): own
                        // echoes above it neither carry nor block the fold —
                        // production picks the latest superseding row the
                        // same way. No same-author rung here: production
                        // licenses it for DELETE carriers only, and this
                        // model has no deletes (the content-carrier variant
                        // is REFUTED — this fuzzer found the counterexample:
                        // folding the carrier author's earlier rows starves
                        // the ledger holds the content rung needs to absorb
                        // a later stale-watermarked reissue). A RETENTION row
                        // can never carry either (loser-row ruling): it is
                        // skipped, not applied, so folding rows under it
                        // would consume their content undelivered.
                        let carrier = batch
                            .iter()
                            .rev()
                            .find(|&&idx| nest[idx].author != i && !nest[idx].is_retention);
                        match carrier {
                            None => vec![false; batch.len()],
                            Some(&last) => {
                                // Carrier-never-stale — see `Fuzz::deliver_batch`.
                                // STORED frontiers, read before the pre-pass
                                // runs — production computes `fold_licensed`
                                // (`engine.rs:3593`) ahead of the pre-pass
                                // (`:3742`), so the conjunct sees the
                                // pre-listing state.
                                // Carrier-never-DEFERRED (leg-4 ruling): while
                                // a pending window is open the carrier's own
                                // judgement may defer-cap, which would consume
                                // the folded rows' content under a carrier
                                // that never applied — process per-row instead.
                                // The test is the DEFER's exact reachability:
                                // the report window, or the recorded-witness
                                // window (an unrecorded divergence merges, and
                                // a merging carrier still applies).
                                let pending = !state[i].pending_winners.is_empty()
                                    || (state[i].local != state[i].base
                                        && state[i].last_recorded.as_ref()
                                            == Some(&state[i].local));
                                let carrier_stale = pending
                                    || nest[last].is_resolution
                                        && (nest[last].derived_through as i64)
                                            < state[i]
                                                .frontiers()
                                                .effective_edit_frontier()
                                                .unwrap_or(0);
                                // Reads as "the watermark covers row `idx`'s
                                // seq": the model's seq for row `idx` is
                                // `idx + 1`, so `> idx` is `>= idx + 1`
                                // (clippy `int_plus_one`).
                                let covers_all = !carrier_stale
                                    && batch
                                        .iter()
                                        .filter(|&&idx| idx < last)
                                        .all(|&idx| nest[last].derived_through > idx);
                                batch
                                    .iter()
                                    .map(|&idx| covers_all && idx < last && nest[idx].author != i)
                                    .collect()
                            }
                        }
                    }
                };
                // Production's partition PRE-PASS, after the fold license and
                // before any row is judged (`engine.rs` order: fold maps at
                // :3593, pre-pass at :3742, main loop at :3795).
                pre_pass(&mut state[i], &nest, &batch, i);
                // Clause-4 partition, as in `Fuzz::deliver_batch`: peers
                // first, own echoes last — the ordering both hosts use.
                let mut order: Vec<usize> = Vec::with_capacity(batch.len());
                order.extend((0..batch.len()).filter(|&k| nest[batch[k]].author != i));
                order.extend((0..batch.len()).filter(|&k| nest[batch[k]].author == i));
                let mut publishes: Vec<(usize, CPublish)> = Vec::new();
                // The transient-class cap (leg-4 ruling): a covering-adopt
                // defer holds the cursor below its seq — everything at and
                // past it waits for the next round's re-listing.
                let mut cap: Option<usize> = None;
                for k in order {
                    let idx = batch[k];
                    let seq = idx + 1;
                    if cap.is_some_and(|c| seq >= c) {
                        continue;
                    }
                    if folded[k] {
                        state[i].seen = seq.max(state[i].seen);
                        // Folded edits ride the carrier: the edit-frontier
                        // advances past them (upper-bound law — see the
                        // matching arm in `Fuzz::deliver_batch`) — unless the
                        // folded row is a reissue of held bytes (gap-3
                        // ruling: its novelty was counted at its earliest
                        // carrier) or a RETENTION row (loser-row ruling:
                        // invisible to content, folded or not).
                        if !nest[idx].is_resolution && !nest[idx].is_retention {
                            let reissue_of_held = state[i]
                                .ledger
                                .iter()
                                .any(|(s, c)| *s < idx + 1 && c == &nest[idx].content);
                            if !reissue_of_held {
                                state[i].edit_frontier = state[i].edit_frontier.max(idx + 1);
                            }
                        }
                        continue;
                    }
                    let row = nest[idx].clone();
                    // The honest winner claim, listing in hand: the incoming
                    // seq when the gap below it holds only this seat's own
                    // rows, else the pre-merge frontier (leg-4 ruling).
                    let f = state[i].frontier;
                    let claim = if ((f + 1)..seq).all(|gs| nest[gs - 1].author == i) {
                        seq
                    } else {
                        f
                    };
                    match causal_apply(&mut state[i], seq, &row, i, idx as i64, claim) {
                        CApply::Defer => {
                            cap = Some(seq);
                        }
                        CApply::Done(publish) => {
                            state[i].seen = seq.max(state[i].seen);
                            if let Some(p) = publish {
                                // Stamped at capture: the contiguous prefix
                                // processed so far — never anything the seat
                                // has not consumed.
                                publishes.push((state[i].seen, p));
                            }
                        }
                    }
                }
                for (w, p) in publishes {
                    note_published(&mut state[i].last_recorded, i, &p);
                    mint_publish(&mut nest, i, w, p);
                }
            } else if churn && !redelivered[i] && state[i].seen == nest.len() {
                // One full re-delivery pass of everything this seat has seen
                // — a re-listed span goes through the same partition
                // machinery, pre-pass included.
                redelivered[i] = true;
                progressed = true;
                let span: Vec<usize> = (0..state[i].seen).collect();
                pre_pass(&mut state[i], &nest, &span, i);
                for idx in span {
                    let row = nest[idx].clone();
                    let seq = idx + 1;
                    let f = state[i].frontier;
                    let claim = if ((f + 1)..seq).all(|gs| nest[gs - 1].author == i) {
                        seq
                    } else {
                        f
                    };
                    match causal_apply(&mut state[i], seq, &row, i, idx as i64, claim) {
                        // A deferred row during redelivery: the span past it
                        // waits for the next re-listing (the cursor never
                        // moved — this is a redelivery).
                        CApply::Defer => break,
                        CApply::Done(Some(p)) => {
                            let w = state[i].seen;
                            note_published(&mut state[i].last_recorded, i, &p);
                            mint_publish(&mut nest, i, w, p);
                        }
                        CApply::Done(None) => {}
                    }
                }
            }
            if nest.len() > 400 {
                return Err(format!(
                    "runaway: {} rows — the causal rules failed to damp (seat {} at {} bytes)",
                    nest.len(),
                    state[i].name,
                    state[i].local.len()
                ));
            }
        }
        if !progressed {
            break;
        }
    }

    // A model that walks out of the round budget with rows unconsumed has not
    // converged — say so instead of returning a half-state that a union assert
    // then misreports as a lost edit.
    for s in &state {
        if s.seen < nest.len() {
            return Err(format!(
                "did not quiesce: seat {} consumed {}/{} rows in the round budget",
                s.name,
                s.seen,
                nest.len()
            ));
        }
    }

    Ok(state.into_iter().map(|s| s.local).collect())
}

/// The winner bytes of a publish — for the hand-written design-record tests,
/// which script their logs manually and only need the content the seat
/// produced (their scripted logs deliberately omit the report plane's loser
/// rows: they pin specific pre-report-plane mechanisms, and the report plane
/// has its own pins below).
fn winner_bytes(p: CPublish) -> Vec<u8> {
    match p {
        CPublish::Direct(c) => c,
        CPublish::Report { winner, .. } => winner,
    }
}

fn assert_causal_union(seats: usize, delivery: Delivery, churn: bool) -> Result<(), String> {
    let names = ["a", "b", "c", "d"];
    let finals = converge_causal(seats, delivery, churn, 60)?;
    let first = String::from_utf8_lossy(&finals[0]).to_string();
    for (i, f) in finals.iter().enumerate() {
        let got = String::from_utf8_lossy(f).to_string();
        if got != first {
            return Err(format!(
                "seats disagree: {} has {got:?}, a has {first:?}",
                names[i]
            ));
        }
        for (j, name) in names.iter().enumerate().take(seats) {
            if !got.contains(&format!("{name}: EDITED")) {
                return Err(format!(
                    "seat {} LOST seat {}'s edit — converged on {got:?}",
                    names[i], names[j]
                ));
            }
        }
        // The union, not a runaway superset: each edited line exactly once.
        for name in names.iter().take(seats) {
            let needle = format!("{name}: EDITED");
            if got.matches(&needle).count() != 1 {
                return Err(format!(
                    "seat {} duplicated {name}'s line — converged on {got:?}",
                    names[i]
                ));
            }
        }
    }
    Ok(())
}

/// The ruling's core promise: the union survives and the seats converge at
/// every seat count the legacy policies fail — under BOTH delivery shapes.
///
/// Since 2026-08-05 this runs against the full report-plane model under the
/// loser-row ruling: retention rows are minted on every merge report and
/// accounted-but-never-applied. (Between the report plane's arrival and the
/// ruling, 4 seats lost an edit here — a loser row's stale content won a
/// latest-wins; the pin below keeps that narrative.)
#[test]
fn causal_cursor_preserves_the_union_at_two_three_and_four_seats() {
    for seats in 2..=4 {
        for delivery in [Delivery::PerRow, Delivery::BatchedWithFold] {
            assert_causal_union(seats, delivery, false).unwrap_or_else(|e| {
                panic!("causal cursor must hold the union at {seats} seats: {e}")
            });
        }
    }
}

/// Duplicate re-delivery — the churn class that separated model from
/// production for `ParkIfAbsent` — must be absorbed without row growth: a
/// re-delivered row is already in the ledger, so it either matches local,
/// fast-forwards to known content, or merges idempotently against the same
/// ancestor it merged against the first time.
///
#[test]
fn causal_cursor_survives_duplicate_redelivery_churn() {
    for seats in 2..=4 {
        for delivery in [Delivery::PerRow, Delivery::BatchedWithFold] {
            assert_causal_union(seats, delivery, true).unwrap_or_else(|e| {
                panic!("causal cursor must absorb re-delivery at {seats} seats: {e}")
            });
        }
    }
}

/// **The interleaving, as an executable record.** Two
/// seats, 80 ms apart in production; here the exact schedule: seat a consumes
/// its own echo ALONE (base advances to its own edit), then b's concurrent row
/// arrives in the NEXT batch; seat b gets both rows in ONE batch, where the
/// pre-ruling unconditional fold skips a's row entirely. Pre-ruling behaviour
/// loses a's edit on BOTH seats through two different doors (a: the
/// `local == base` licence; b: the fold). The causal rules hold the union.
#[test]
fn row20_interleaving_two_seats_pre_ruling_loses_and_causal_holds() {
    let base0 = b"a: base\nb: base\n".to_vec();
    let a_edit = b"a: EDITED\nb: base\n".to_vec();
    let b_edit = b"a: base\nb: EDITED\n".to_vec();

    // ── Pre-ruling (clause-4 licence + unconditional fold), scripted ──
    // Seat a: own echo arrives in a solo batch → base := a_edit; b's row in
    // the NEXT batch meets local == base → fast-forward over a's own edit.
    let mut a_local = a_edit.clone();
    let a_base = a_edit.clone(); // own echo, solo batch
    if a_local == a_base {
        a_local = b_edit.clone(); // the licence door: verbatim apply
    }
    // Seat b: one batch [a's row (seq 1), own echo (seq 2)] — the
    // unconditional batch-latest fold skips a's row; the echo advances the
    // base only. b never sees a's edit at all.
    let b_local = b_edit.clone();
    assert_eq!(
        a_local, b_edit,
        "pre-ruling: seat a must fast-forward over its own edit (the licence door)"
    );
    assert!(
        !String::from_utf8_lossy(&b_local).contains("a: EDITED"),
        "pre-ruling: seat b must never have seen a's edit (the fold door)"
    );

    // ── The causal rules on the same schedule ──
    let mut a = CSeat {
        name: "a",
        local: a_edit.clone(),
        base: base0.clone(),
        frontier: 0,
        edit_frontier: 0,
        content_frontier: None,
        pending_winners: Vec::new(),
        ledger: vec![(0, base0.clone())],
        seen: 0,
        last_recorded: Some(a_edit.clone()),
        fresh_bind: false,
    };
    let mut b = CSeat {
        name: "b",
        local: b_edit.clone(),
        base: base0.clone(),
        frontier: 0,
        edit_frontier: 0,
        content_frontier: None,
        pending_winners: Vec::new(),
        ledger: vec![(0, base0.clone())],
        seen: 0,
        last_recorded: Some(b_edit.clone()),
        fresh_bind: false,
    };
    let row1 = CRow {
        author: 0,
        content: a_edit.clone(),
        derived_through: 0,
        is_resolution: false,
        is_retention: false,
    };
    let row2 = CRow {
        author: 1,
        content: b_edit.clone(),
        derived_through: 0,
        is_resolution: false,
        is_retention: false,
    };
    let log01 = [row1.clone(), row2.clone()];

    // Seat a, batch [row1] (own echo alone): pre-pass counts the own edit,
    // then frontier → 1, base := a_edit — the exact armed-trap state the
    // pre-ruling licence fired from.
    pre_pass(&mut a, &log01, &[0], 0);
    assert!(causal_apply(&mut a, 1, &row1, 0, 0, 1).done().is_none());
    assert_eq!(a.base, a_edit, "echo advances the base as before");
    assert_eq!(a.frontier, 1);
    // Seat b, batch [row1, row2]: the pre-pass counts b's own row-2 novelty
    // with the seq in hand BEFORE row1 is judged (production's partition,
    // `engine.rs:3742` — this is what the retired in-flight fold
    // approximated). The causal fold does NOT fold row1 under row2
    // (row2.derived_through = 0 < seq 1 — b's own row never incorporated
    // a's). row1 is a sibling of b's unpublished edit → merge from base0.
    pre_pass(&mut b, &log01, &[0, 1], 1);
    let published = causal_apply(&mut b, 1, &row1, 1, 0, 1).done();
    let union =
        winner_bytes(published.expect("b must merge a's sibling row and publish the union"));
    assert!(String::from_utf8_lossy(&union).contains("a: EDITED"));
    assert!(String::from_utf8_lossy(&union).contains("b: EDITED"));
    assert!(causal_apply(&mut b, 2, &row2, 1, 1, 2).done().is_none()); // own echo
    let row3 = CRow {
        author: 1,
        content: union.clone(),
        derived_through: 2,
        is_resolution: true,
        is_retention: false,
    };
    // Seat a, next batch [row2, row3]: row2 folds under row3 causally (row3
    // carries derived_through 2 ≥ row2's seq 2 — b's union provably
    // incorporated its own row). Row3 reaches the licence with derived_through
    // 2 ≥ a's frontier 1 and local == base → the causal fast-forward applies
    // the true descendant verbatim. a's edit is inside it.
    assert!(causal_apply(&mut a, 3, &row3, 0, 2, 3).done().is_none());
    assert_eq!(a.local, union, "a fast-forwards onto the true descendant");
    assert_eq!(b.local, union, "b already merged");
}

/// `conflicts.md` § *Retention rows are transparent to the licence* (ruled
/// 2026-09-27): the nest mints the winner's `derived_through` exactly as the
/// reporter sent it — the gap-2 winner-stamp upgrade is retired — and a
/// caught-up receiver still fast-forwards an EDIT-class winner behind its own
/// loser retention row, because rule 4 reads the content frontier. Log: seq 1
/// = p's edit; the reporter r, holding unpublished novelty, merges it and
/// files a resolved report — seq 2 = r's loser retention row, seq 3 = the
/// winner, edit-class (it carries r's novelty), claiming 1. Red with the
/// upgrade dropped and no content frontier: the retention row lifts the
/// receiver's frontier to 2, the claim of 1 fails rule 4, and the receiver
/// takes a merge round and files a report of its own.
#[test]
fn an_edit_class_winner_behind_its_retention_row_fast_forwards_at_a_caught_up_seat() {
    let base0 = b"p: base\nr: base\n".to_vec();
    let p_edit = b"p: EDITED\nr: base\n".to_vec();
    let r_edit = b"p: base\nr: EDITED\n".to_vec();
    let seat = |name: &'static str, local: &[u8]| CSeat {
        name,
        local: local.to_vec(),
        base: base0.clone(),
        frontier: 0,
        edit_frontier: 0,
        content_frontier: None,
        pending_winners: Vec::new(),
        ledger: vec![(0, base0.clone())],
        seen: 0,
        last_recorded: None,
        fresh_bind: false,
    };
    let mut r = seat("r", &r_edit);
    let mut c = seat("c", &base0);
    let mut nest = vec![CRow {
        author: 0,
        content: p_edit.clone(),
        derived_through: 0,
        is_resolution: false,
        is_retention: false,
    }];

    // r merges p's sibling edit over its unpublished novelty and reports,
    // claiming seq 1 (contiguous with its pre-merge frontier 0).
    let report = causal_apply(&mut r, 1, &nest[0].clone(), 1, 0, 1)
        .done()
        .expect("r merges p's sibling edit and files a resolved report");
    let CPublish::Report {
        winning_w,
        winner_carries_novelty,
        ref winner,
        ..
    } = report
    else {
        panic!("a merge files a resolved report, not a direct publish");
    };
    assert_eq!(winning_w, 1);
    assert!(winner_carries_novelty, "r consumed unpublished novelty");
    let merged = winner.clone();
    mint_publish(&mut nest, 1, 1, report);
    assert_eq!(
        nest.len(),
        3,
        "the nest minted the loser retention row and the winner"
    );
    assert!(nest[1].is_retention);
    assert!(
        !nest[2].is_resolution,
        "the winner carries novelty: edit-class"
    );
    assert_eq!(
        nest[2].derived_through, 1,
        "the winner row's claim is minted exactly as sent — no upgrade"
    );

    // c, caught up and with no unpublished work, consumes the three rows.
    for (k, row) in nest.clone().iter().enumerate() {
        let seq = k + 1;
        assert!(
            causal_apply(&mut c, seq, row, 2, k as i64, seq)
                .done()
                .is_none(),
            "row {seq} publishes nothing at a caught-up seat — no merge round, no report"
        );
    }
    assert_eq!(c.local, merged, "c fast-forwarded onto the winner verbatim");
    assert_eq!(c.frontier, 3);
    assert_eq!(c.content_frontier, Some(3));
}

/// The N≥3 leg-4 red under the causal
/// rules, at its faithful production schedule. Log: seq 1 = b's edit, seq 2 =
/// c's edit, seq 3 = a's edit, seq 4 = b's merge of {1,2} (a resolution at
/// derived_through 2 — b never saw a's seq-3 row). Seat a consumes in order:
/// it merges both peers' fresh sibling edits (reaching the full union — "a
/// folded in BOTH peers' edits", exactly as the failing run logged), takes its
/// own echo, and then meets the head. Pre-ruling, `local == base` read that
/// head as a fast-forward and applied it verbatim — the measured `a: base`
/// loss. Under the ruling the head is a STALE RESOLUTION (derived_through 2 <
/// frontier 3): it derives entirely from rows a already incorporated, so a
/// skips it and keeps the union.
#[test]
fn third_writer_head_is_read_as_stale_resolution_not_descendant() {
    let base0 = b"a: base\nb: base\nc: base\n".to_vec();
    let a_edit = b"a: EDITED\nb: base\nc: base\n".to_vec();
    let b_edit = b"a: base\nb: EDITED\nc: base\n".to_vec();
    let c_edit = b"a: base\nb: base\nc: EDITED\n".to_vec();
    let bc_merge = b"a: base\nb: EDITED\nc: EDITED\n".to_vec(); // never saw a's row

    let mut a = CSeat {
        name: "a",
        local: a_edit.clone(),
        base: base0.clone(),
        frontier: 0,
        edit_frontier: 0,
        content_frontier: None,
        pending_winners: Vec::new(),
        ledger: vec![(0, base0.clone())],
        seen: 0,
        last_recorded: Some(a_edit.clone()),
        fresh_bind: false,
    };

    // seq 1, b's fresh edit: a has unpublished work (local != base) → merge
    // from the causal ancestor (base0).
    let row1 = CRow {
        author: 1,
        content: b_edit,
        derived_through: 0,
        is_resolution: false,
        is_retention: false,
    };
    // seq 2, c's fresh edit; seq 3, a's own echo. One contiguous listing —
    // a's own row 3 exists, so any listing reaching rows 1–2 contains it
    // (log contiguity), and production's pre-pass counts its novelty at
    // seq 3 BEFORE the peer rows are judged (`engine.rs:3742`).
    let row2 = CRow {
        author: 2,
        content: c_edit,
        derived_through: 0,
        is_resolution: false,
        is_retention: false,
    };
    let a_echo_pre = CRow {
        author: 0,
        content: a_edit.clone(),
        derived_through: 0,
        is_resolution: false,
        is_retention: false,
    };
    let log123 = [row1.clone(), row2.clone(), a_echo_pre];
    pre_pass(&mut a, &log123, &[0, 1, 2], 0);
    assert_eq!(
        a.edit_frontier, 3,
        "the pre-pass counts a's own row-3 novelty"
    );
    let m1 = winner_bytes(
        causal_apply(&mut a, 1, &row1, 0, 0, 1)
            .done()
            .expect("a merges b's sibling edit"),
    );
    assert!(String::from_utf8_lossy(&m1).contains("a: EDITED"));

    let m2 = winner_bytes(
        causal_apply(&mut a, 2, &row2, 0, 1, 2)
            .done()
            .expect("a merges c's sibling edit"),
    );
    let got = String::from_utf8_lossy(&m2).to_string();
    assert!(
        got.contains("a: EDITED") && got.contains("b: EDITED") && got.contains("c: EDITED"),
        "a folded in both peers' edits: {got:?}"
    );

    // seq 3, a's own echo: frontier → 3. (The base holds the merge result, not
    // the echoed original — clause 4's iff-local-matches guard.)
    let a_echo = CRow {
        author: 0,
        content: a_edit,
        derived_through: 0,
        is_resolution: false,
        is_retention: false,
    };
    assert!(causal_apply(&mut a, 3, &a_echo, 0, 2, 3).done().is_none());
    assert_eq!(a.frontier, 3);
    assert_eq!(
        a.local, a.base,
        "fully merged and published — the armed state"
    );

    // seq 4, b's merged head — the row that destroyed a's edit in production.
    let head = CRow {
        author: 1,
        content: bc_merge,
        derived_through: 2, // b resolved rows 1-2 only — a's seq-3 row unseen
        is_resolution: true,
        is_retention: false,
    };
    assert!(
        causal_apply(&mut a, 4, &head, 0, 3, 4).done().is_none(),
        "a stale resolution is skipped, never applied and never re-merged"
    );
    let kept = String::from_utf8_lossy(&a.local).to_string();
    assert!(
        kept.contains("a: EDITED") && kept.contains("b: EDITED") && kept.contains("c: EDITED"),
        "a keeps the union — pre-ruling this exact state fast-forwarded to {kept:?}"
    );
    assert_eq!(a.frontier, 4, "the skipped row is still accounted for");
}

// ────────────────────────────────────────────────────────────────────────────
// The schedule fuzzer.
//
// Every test above runs ONE schedule the author wrote down. That is what let
// two refuted policies pass this module: a hand-written schedule encodes the
// churn its author already suspected, and both `ParkIfAbsent` and
// `ParkedUntilIncorporated` failed on churn nobody had thought to write. The
// hand-written cases stay as the design record; this section generates the
// schedule instead — delivery batch boundaries, duplicate re-delivery, and
// reconcile re-uploads, in orders no one chose — and asserts the invariants
// over the result, shrinking a failure to a minimal reproduction.
//
// It drives the SAME two step functions the hand-written tests do
// (`legacy_apply` / `causal_apply`), and `causal_apply` in turn asks the
// production decision core (`crate::causal::judge_incoming`), so a refutation
// here is a refutation of shipped behaviour rather than of a model of it.
// ────────────────────────────────────────────────────────────────────────────

/// Which merge-base engine a fuzz run drives.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Engine {
    /// One of the pre-ruling policies, kept as the refutation record. Per-row
    /// delivery only — the legacy model has no fold.
    Legacy(BasePolicy),
    /// The shipped causal-watermark rules as the ENGINE host runs them
    /// (`engine.rs` — the shape `causal_apply` mirrors).
    Causal,
}

/// What the seats' concurrent edits do to the shared base — the axis the
/// hand-written cases never varied, and the one that decides whether a stale
/// ancestor can duplicate lines at all.
#[derive(Clone, Copy, Debug, PartialEq)]
enum EditShape {
    /// Each seat rewrites its OWN pre-existing line. Hunks never overlap, so a
    /// clean three-way merge always exists and the union is well defined —
    /// this is the shape every test above uses.
    OwnLine,
    /// Each seat APPENDS a line at the end. Every seat's hunk shares one
    /// anchor, which is what `tests/platform/sync/test_text_merge.py` does and
    /// what lets a merge from a stale ancestor produce overlapping-identical
    /// hunks. A latest-wins fall-back may legitimately drop a side here, so
    /// only convergence and duplication-freedom are asserted for this shape.
    Append,
}

/// One scheduling decision. A schedule is a `Vec<Step>`; seat indices are
/// taken modulo the seat count so the strategy needs no dependence on it
/// (which keeps shrinking simple).
#[derive(Clone, Debug)]
enum Step {
    /// Seat `seat` consumes the next `n` unseen rows as ONE batch — the batch
    /// boundary is what the fold rules are sensitive to.
    Deliver { seat: usize, n: usize },
    /// Seat `seat` re-receives the `n` rows it most recently consumed, without
    /// advancing its cursor: the overlapping-pull / reconcile-re-send
    /// duplicate class a real host produces and the un-churned model skips.
    Redeliver { seat: usize, n: usize },
    /// Seat `seat` re-uploads its current local content as a fresh EDIT row —
    /// the lost-ack retry shape (`is_resolution = false`: the writer cannot
    /// prove the original record landed, so the honest stamp is an edit).
    /// When the record DID land, the bytes are already incorporated
    /// downstream and the receivers' content rung absorbs the reissue.
    Reupload { seat: usize },
    /// Seat `seat` re-uploads its current local content as a PROVEN reissue —
    /// the re-seal migration holding its recorded-content proof, which stamps
    /// `is_resolution = true` (no novel content) per the gap-1 ruling.
    /// Receivers whose frontier passed it skip it as a stale reissue.
    Reseal { seat: usize },
    /// Seat `seat` authors a FRESH novel append (a numbered marker line) —
    /// the live watcher shape the alphabet lacked until 2026-08-05 (row 8).
    /// `publish: true` = the watcher fired: the row lands stamped at the
    /// seat's CURRENT anchor (`handle_local_change` stamps
    /// `edit_stamp(anchor_mirror)` — a mid-run edit claims HIGH, unlike the
    /// seed rows' `w = 0`). `publish: false` = the PENDING window: the bytes
    /// rest only on the seat's disk — no row, no recorded witness — and if a
    /// conflict merge overwrites the file before a later `Reupload`, the
    /// append never becomes a row at all (the report's rows are its only
    /// carriers). Both shapes existed in the live leg-4a cell; neither
    /// existed here, which is how the model blessed a machinery that loses
    /// the pending append (the 2026-08-05 agreement-with-loss).
    Edit { seat: usize, publish: bool },
    /// Seat `seat` REVERTS: rewrites local to a DIFFERENT version its ledger
    /// holds at seq ≤ its frontier (the oldest that differs — an editor
    /// undo-then-save, or a snapshot restore into a live watch dir) and
    /// re-uploads it; `upload_file`'s widened proven-reissue proof stamps the
    /// row resolution-class, so neither frontier counts it. NOT in the random
    /// walk's alphabet: an uncontested revert legitimately drops the seat's
    /// own line, which the union invariant reads as a loss — it is driven
    /// by the deterministic pin only (the held-bytes release, 2026-09-21).
    Revert { seat: usize },
}

/// The seats, in whichever representation the engine under test needs.
enum Seats {
    Legacy(Vec<Seat>),
    Causal(Vec<CSeat>),
}

struct Fuzz {
    engine: Engine,
    delivery: Delivery,
    n: usize,
    nest: Vec<CRow>,
    seats: Seats,
    /// Every marker line a user "wrote" on some seat's disk — the seed edits
    /// plus every [`Step::Edit`] — i.e. the content the no-user-data-loss
    /// invariant owes survival to. The exactly-once checkers read this, not a
    /// hardcoded name list, so mid-schedule edits are owed survival exactly
    /// like the seeds.
    authored: Vec<String>,
    /// Serial for [`Step::Edit`] marker lines (distinct content per edit).
    edit_serial: usize,
}

/// The shared base and each seat's concurrent edit, per shape. Both shapes put
/// the marker `"{name}: EDITED"` on exactly one line, so one set of invariants
/// covers both.
fn seed(shape: EditShape, seats: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
    let names = ["a", "b", "c", "d"];
    match shape {
        EditShape::OwnLine => {
            let base0: Vec<u8> = (0..seats)
                .map(|i| format!("{}: base\n", names[i]))
                .collect::<String>()
                .into_bytes();
            let edits = (0..seats)
                .map(|i| {
                    String::from_utf8(base0.clone())
                        .unwrap()
                        .replace(
                            &format!("{}: base", names[i]),
                            &format!("{}: EDITED", names[i]),
                        )
                        .into_bytes()
                })
                .collect();
            (base0, edits)
        }
        EditShape::Append => {
            let base0 = b"shared: base\n".to_vec();
            let edits = (0..seats)
                .map(|i| {
                    let mut v = base0.clone();
                    v.extend_from_slice(format!("{}: EDITED\n", names[i]).as_bytes());
                    v
                })
                .collect();
            (base0, edits)
        }
    }
}

impl Fuzz {
    fn new(engine: Engine, shape: EditShape, seats: usize, delivery: Delivery) -> Self {
        let names = ["a", "b", "c", "d"];
        let (base0, edits) = seed(shape, seats);
        let seat_states = match engine {
            Engine::Legacy(_) => Seats::Legacy(
                (0..seats)
                    .map(|i| Seat {
                        name: names[i],
                        local: edits[i].clone(),
                        base: Some(base0.clone()),
                        parked: None,
                        seen: 0,
                    })
                    .collect(),
            ),
            Engine::Causal => Seats::Causal(
                (0..seats)
                    .map(|i| CSeat {
                        name: names[i],
                        local: edits[i].clone(),
                        base: base0.clone(),
                        frontier: 0,
                        edit_frontier: 0,
                        content_frontier: None,
                        // The seed is an ORDINARY upload — production does
                        // not arm the report window for it (`engine.rs`
                        // arms only at the resolver arms); the record
                        // landed, so `last_recorded` carries the
                        // recoverability witness and the pre-pass counts
                        // the seq when the row's listing arrives.
                        pending_winners: Vec::new(),
                        ledger: vec![(0, base0.clone())],
                        seen: 0,
                        last_recorded: Some(edits[i].clone()),
                        fresh_bind: false,
                    })
                    .collect(),
            ),
        };
        // The concurrent publications, watermark 0 — nothing incorporated
        // beyond the initial version, exactly production's stamp.
        let nest = (0..seats)
            .map(|i| CRow {
                author: i,
                content: edits[i].clone(),
                derived_through: 0,
                is_resolution: false,
                is_retention: false,
            })
            .collect();
        let authored = (0..seats)
            .map(|i| format!("{}: EDITED", ["a", "b", "c", "d"][i]))
            .collect();
        Self {
            engine,
            delivery,
            n: seats,
            nest,
            seats: seat_states,
            authored,
            edit_serial: 0,
        }
    }

    /// A CLEAN start — every seat holds the shared append-shape base, nothing
    /// authored, nothing published (added 2026-08-05, row 8). The seeded
    /// [`Fuzz::new`] publishes every seat's edit as a row before the first
    /// step, and those ever-present raw edit rows RESCUE the pending-window
    /// loss: a peer's raw append row always re-merges the content back in.
    /// The live cell has no such guarantee — an append pre-empted by a
    /// conflict merge never becomes a row at all — and reproducing that needs
    /// a start where [`Step::Edit`] authors everything.
    fn new_clean(engine: Engine, seats: usize) -> Self {
        let names = ["a", "b", "c", "d"];
        let (base0, _) = seed(EditShape::Append, seats);
        let seat_states = Seats::Causal(
            (0..seats)
                .map(|i| CSeat {
                    name: names[i],
                    local: base0.clone(),
                    base: base0.clone(),
                    frontier: 0,
                    edit_frontier: 0,
                    content_frontier: None,
                    pending_winners: Vec::new(),
                    ledger: vec![(0, base0.clone())],
                    seen: 0,
                    last_recorded: None,
                    fresh_bind: false,
                })
                .collect(),
        );
        Self {
            engine,
            delivery: Delivery::PerRow,
            n: seats,
            nest: Vec::new(),
            seats: seat_states,
            authored: Vec::new(),
            edit_serial: 0,
        }
    }

    /// [`Step::Edit`] — a fresh user append on seat `i`'s disk, then (when
    /// `publish`) the watcher upload exactly as `handle_local_change` does
    /// it: the row carries the file's CURRENT bytes, stamped
    /// `edit_stamp(anchor)` — `derived_through` = the seat's consumption
    /// anchor at upload time (`main.rs:1194-1211`; fresh novel bytes are
    /// never a proven reissue, so the honest stamp is an edit). With
    /// `publish: false` the append stays in the PENDING window: on the
    /// seat's disk only — no row, no recorded witness — until a later
    /// `Reupload` publishes whatever the file holds BY THEN (a conflict
    /// merge in between consumes the append into the report plane, and the
    /// raw append never uploads at all — the live leg-4a shape).
    fn edit(&mut self, i: usize, publish: bool) {
        let names = ["a", "b", "c", "d"];
        let marker = format!("{}: EDITED{}", names[i], self.edit_serial);
        self.edit_serial += 1;
        let Seats::Causal(v) = &mut self.seats else {
            return; // the legacy model predates this shape
        };
        v[i].local
            .extend_from_slice(format!("{marker}\n").as_bytes());
        self.authored.push(marker);
        if publish {
            let content = v[i].local.clone();
            let w = v[i].seen;
            v[i].last_recorded = Some(content.clone());
            self.nest.push(CRow {
                author: i,
                content,
                derived_through: w,
                is_resolution: false,
                is_retention: false,
            });
        }
    }

    /// Agreement + every authored marker exactly once on every seat — the
    /// e2e leg's own invariant, read off [`Fuzz::authored`] so mid-schedule
    /// edits are owed survival exactly like the seeds.
    fn check_exactly_once(&self) -> Result<(), String> {
        let finals: Vec<String> = (0..self.n)
            .map(|i| String::from_utf8_lossy(&self.local(i)).to_string())
            .collect();
        for (i, got) in finals.iter().enumerate() {
            for marker in &self.authored {
                // Whole-line match: the seed marker "a: EDITED" is a strict
                // prefix of the serialized "a: EDITED0", so a substring count
                // would double-count it.
                let count = got.lines().filter(|l| l == marker).count();
                if count != 1 {
                    return Err(format!(
                        "seat {i} holds {marker:?} {count}× (want exactly once): {got:?}"
                    ));
                }
            }
        }
        let distinct: std::collections::BTreeSet<&String> = finals.iter().collect();
        if distinct.len() != 1 {
            return Err(format!("seats disagree: {finals:?}"));
        }
        Ok(())
    }

    fn seen(&self, i: usize) -> usize {
        match &self.seats {
            Seats::Legacy(v) => v[i].seen,
            Seats::Causal(v) => v[i].seen,
        }
    }
    fn set_seen(&mut self, i: usize, v: usize) {
        match &mut self.seats {
            Seats::Legacy(s) => s[i].seen = v,
            Seats::Causal(s) => s[i].seen = v,
        }
    }
    fn local(&self, i: usize) -> Vec<u8> {
        match &self.seats {
            Seats::Legacy(v) => v[i].local.clone(),
            Seats::Causal(v) => v[i].local.clone(),
        }
    }

    /// One row into one seat, through the engine's own step function. Legacy
    /// publishes wear the direct shape — the legacy model predates the report
    /// plane and its seats ignore stamps entirely, so the single-row mint is
    /// exactly what it always was (and never defers).
    fn apply(&mut self, i: usize, idx: usize, row: &CRow) -> CApply {
        // The honest winner claim, listing in hand (leg-4 ruling): the
        // incoming seq when the gap below it holds only this seat's own
        // rows, else the pre-merge frontier. Conjunct 3 of the same-anchor
        // ruling (2026-08-05) gives the DAEMON the same own-gap widening —
        // its catch-up caller has the listing in hand too
        // (`apply_caught_up_changes` holds `changes`), and the pre-ruling
        // contiguity-only claim (`ws_client.rs:2155-2158`) systematically
        // under-claimed merge winners, which peers then stale-skipped BOTH
        // ways: the 2-seat mutual strand this model measured (each seat
        // quiesced on its own permutation).
        let claim = match &self.seats {
            Seats::Causal(v) => {
                let f = v[i].frontier;
                if ((f + 1)..idx + 1).all(|gs| self.nest[gs - 1].author == i) {
                    idx + 1
                } else {
                    f
                }
            }
            Seats::Legacy(_) => idx + 1,
        };
        match (&mut self.seats, self.engine) {
            (Seats::Legacy(v), Engine::Legacy(policy)) => CApply::Done(
                legacy_apply(policy, &mut v[i], idx, row.author, &row.content, i)
                    .map(CPublish::Direct),
            ),
            (Seats::Causal(v), Engine::Causal) => {
                causal_apply(&mut v[i], idx + 1, row, i, idx as i64, claim)
            }
            _ => unreachable!("the seat representation is chosen from the engine"),
        }
    }

    /// Deliver rows `[start, end)` as one batch.
    fn deliver_batch(&mut self, i: usize, start: usize, end: usize) {
        let batch: Vec<usize> = (start..end).collect();
        if batch.is_empty() {
            return;
        }
        // The causal batch fold, identical to `converge_causal`'s: earlier
        // rows fold only under a batch-latest PEER row whose watermark covers
        // every earlier batch row's seq.
        let folded: Vec<bool> = match (self.engine, self.delivery) {
            (Engine::Causal, Delivery::BatchedWithFold) => {
                // The CARRIER is the latest PEER row, not the raw batch tail
                // (own echoes above it neither carry
                // nor block the fold). No same-author rung here — DELETE
                // carriers only in production, and this model has no deletes
                // (see `converge_causal`'s matching note; the
                // content-carrier variant is fuzzer-refuted, 2026-08-04). A
                // RETENTION row can never carry (loser-row ruling): it is
                // skipped, not applied, so folding rows under it would
                // consume their content undelivered.
                let carrier = batch
                    .iter()
                    .rev()
                    .find(|&&idx| self.nest[idx].author != i && !self.nest[idx].is_retention)
                    .copied();
                match carrier {
                    None => vec![false; batch.len()],
                    Some(last) => {
                        // A licensed fold's CARRIER must never itself be
                        // skipped: a stale-resolution carrier (w below the
                        // seat's EFFECTIVE edit-frontier, in-flight novelty
                        // included) would consume the folded rows' content
                        // undelivered — a lost edit, not a strand (measured
                        // by this fuzzer, 2026-08-03). A carrier below the
                        // full frontier is safe without this conjunct: the
                        // folded seqs are ≤ w < frontier, already reflected.
                        // STORED frontiers, read before the pre-pass runs —
                        // production computes `fold_licensed`
                        // (`engine.rs:3593`) ahead of the pre-pass (`:3742`).
                        // Carrier-never-DEFERRED (leg-4 ruling): while a
                        // pending window is open the carrier's own judgement
                        // may defer-cap, which would consume the folded rows'
                        // content under a carrier that never applied —
                        // process per-row instead. The test is the DEFER's
                        // exact reachability: the report window, or the
                        // recorded-witness window.
                        let carrier_stale = match &self.seats {
                            Seats::Causal(v) => {
                                !v[i].pending_winners.is_empty()
                                    || (v[i].local != v[i].base
                                        && v[i].last_recorded.as_ref() == Some(&v[i].local))
                                    || self.nest[last].is_resolution
                                        && (self.nest[last].derived_through as i64)
                                            < v[i]
                                                .frontiers()
                                                .effective_edit_frontier()
                                                .unwrap_or(0)
                            }
                            Seats::Legacy(_) => false,
                        };
                        let covers_all = !carrier_stale
                            && batch
                                .iter()
                                .filter(|&&idx| idx < last)
                                .all(|&idx| self.nest[last].derived_through > idx);
                        batch
                            .iter()
                            .map(|&idx| covers_all && idx < last && self.nest[idx].author != i)
                            .collect()
                    }
                }
            }
            _ => vec![false; batch.len()],
        };
        // Production's partition PRE-PASS, after the fold license and before
        // any row is judged (`engine.rs` order: fold maps at :3593, pre-pass
        // at :3742, main loop at :3795): own non-resolution rows fold their
        // seqs into the stored edit-frontier with the seqs in hand, and any
        // own non-resolution row — retention included — closes the
        // unknown-seq report window.
        if let Seats::Causal(v) = &mut self.seats {
            pre_pass(&mut v[i], &self.nest, &batch, i);
        }
        // Production's clause-4 partition (BOTH hosts do this): peer rows
        // first, own echoes last, so same-batch merges use the
        // pre-publication ancestor. Mirrored here since 2026-08-03 because
        // its absence hid a live defect from the fuzzer: a covering
        // resolution in the same batch is judged BEFORE the seat's own edit
        // echo, i.e. against an edit-frontier that has not counted the
        // seat's own published novelty — the pre-pass above is what makes
        // that judgement safe.
        let mut order: Vec<usize> = Vec::with_capacity(batch.len());
        order.extend((0..batch.len()).filter(|&k| self.nest[batch[k]].author != i));
        order.extend((0..batch.len()).filter(|&k| self.nest[batch[k]].author == i));
        let mut publishes: Vec<(usize, CPublish)> = Vec::new();
        // The transient-class cap (leg-4 ruling): a covering-adopt defer
        // holds the cursor below its seq — everything at and past it waits
        // for the next listing (production: the pull anchor stays below the
        // cap, `engine.rs`'s seal-cap pattern).
        let mut cap: Option<usize> = None;
        for k in order {
            let idx = batch[k];
            let seq = idx + 1;
            if cap.is_some_and(|c| seq >= c) {
                continue;
            }
            if folded[k] {
                let new_seen = seq.max(self.seen(i));
                self.set_seen(i, new_seen);
                // A folded EDIT's novel bytes arrive through the carrier, so
                // the edit-frontier must advance past it (upper-bound law:
                // under-counting here would licence a later resolution that
                // misses the folded edit to adopt over it) — unless the
                // folded row is a reissue of held bytes (gap-3 ruling: its
                // novelty was counted at its earliest carrier) or a
                // RETENTION row (loser-row ruling: invisible to content).
                if !self.nest[idx].is_resolution
                    && !self.nest[idx].is_retention
                    && let Seats::Causal(v) = &mut self.seats
                {
                    let reissue_of_held = v[i]
                        .ledger
                        .iter()
                        .any(|(s, c)| *s < idx + 1 && c == &self.nest[idx].content);
                    if !reissue_of_held {
                        v[i].edit_frontier = v[i].edit_frontier.max(idx + 1);
                    }
                }
                continue;
            }
            let row = self.nest[idx].clone();
            match self.apply(i, idx, &row) {
                CApply::Defer => {
                    cap = Some(seq);
                }
                CApply::Done(publish) => {
                    let new_seen = seq.max(self.seen(i));
                    self.set_seen(i, new_seen);
                    if let Some(p) = publish {
                        publishes.push((self.seen(i), p));
                    }
                }
            }
        }
        for (w, p) in publishes {
            self.note_published(i, &p);
            mint_publish(&mut self.nest, i, w, p);
        }
    }

    /// Record a publish in the seat's `last_recorded` slot (the sharpened
    /// unpublished-work conjunct — see [`note_published`]).
    fn note_published(&mut self, i: usize, p: &CPublish) {
        if let Seats::Causal(v) = &mut self.seats {
            note_published(&mut v[i].last_recorded, i, p);
        }
    }

    /// Record raw published bytes (the reissue steps' own shape — an upload
    /// whose record landed stamps `recorded_content_hash`).
    fn note_sent(&mut self, i: usize, content: &[u8]) {
        if let Seats::Causal(v) = &mut self.seats {
            v[i].last_recorded = Some(content.to_vec());
        }
    }

    /// Re-deliver the `n` most recently consumed rows without moving the
    /// cursor — the duplicate class. A re-listed span goes through the same
    /// partition machinery, pre-pass included.
    fn redeliver(&mut self, i: usize, n: usize) {
        let seen = self.seen(i);
        let start = seen.saturating_sub(n);
        let span: Vec<usize> = (start..seen).collect();
        if let Seats::Causal(v) = &mut self.seats {
            pre_pass(&mut v[i], &self.nest, &span, i);
        }
        let mut publishes: Vec<CPublish> = Vec::new();
        for idx in span {
            let row = self.nest[idx].clone();
            match self.apply(i, idx, &row) {
                // A deferred row during redelivery: the rest of the span
                // waits (the cursor never moved — this is a redelivery).
                CApply::Defer => break,
                CApply::Done(Some(p)) => publishes.push(p),
                CApply::Done(None) => {}
            }
        }
        let w = self.seen(i);
        for p in publishes {
            self.note_published(i, &p);
            mint_publish(&mut self.nest, i, w, p);
        }
    }

    /// Re-upload the seat's current local content as a fresh (non-resolution)
    /// row, stamped at its catch-up anchor — the lost-ack retry shape.
    fn reupload(&mut self, i: usize) {
        let content = self.local(i);
        let w = self.seen(i);
        // Writer-side proven-reissue stamp (gap-3 ruling — gap 1's stamp on a
        // wider proof): bytes the writer's OWN ledger holds at seq ≤ its
        // frontier provably rest in the log already, so the re-upload carries
        // no novel content and says so — receivers then supersede it by the
        // ordinary resolution licences even when they never held its bytes
        // (measured: two crossed unproven reuploads of opposite permutations
        // left the seats swapped, because each seat's adopt of the OTHER's
        // reupload-as-edit inflated its edit-frontier past the log tail's
        // watermark). A retry whose bytes the ledger does NOT hold is the
        // true lost-ack shape and keeps the honest edit stamp.
        let proven = match &self.seats {
            // Two witnesses of already-in-log-ness, either sufficient (the
            // BASE witness added by the 2026-08-05 loser-row ruling): bytes
            // the ledger holds at seq ≤ the frontier, OR bytes equal to the
            // live BASE — the base only ever advances on an echo (row in
            // log), an applied remote (row in log), or a merge write whose
            // report landed transactionally (winner row in log), so
            // base-matching content provably rests in the log even when the
            // ledger never held it (a merge RESULT is no row's bytes, and a
            // peer that stale-skipped the winner byte-free never held it
            // either). Without this witness, a reconcile re-upload of a
            // merge result wears the edit stamp, receivers' edit-frontiers
            // inflate to its seq, every union-carrying winner reads stale,
            // and a latest-wins against the coalesced-hunk merge destroys a
            // third seat's edit (the shrunk counterexample:
            // `[Deliver{b,1}, Reupload{b}]`, own-line, 3 seats).
            Seats::Causal(v) => {
                v[i].local == v[i].base
                    || v[i].last_recorded.as_ref() == Some(&content)
                    || v[i]
                        .ledger
                        .iter()
                        .any(|(s, c)| *s <= v[i].frontier && *c == content)
            }
            Seats::Legacy(_) => false,
        };
        self.note_sent(i, &content);
        self.nest.push(CRow {
            author: i,
            content,
            derived_through: w,
            is_resolution: proven,
            is_retention: false,
        });
        // Deliberately does NOT arm the report window: an ordinary upload
        // never does (`engine.rs` arms only at the resolver arms), and a
        // reissue binds no novelty besides — its seq is counted (or
        // reissue-excused) by the pre-pass when its listing arrives.
    }

    /// Re-upload the seat's current local content as a PROVEN reissue
    /// (`is_resolution = true`) — the re-seal migration's stamp under the
    /// gap-1 ruling.
    fn reseal(&mut self, i: usize) {
        let content = self.local(i);
        let w = self.seen(i);
        self.note_sent(i, &content);
        self.nest.push(CRow {
            author: i,
            content,
            derived_through: w,
            is_resolution: true,
            is_retention: false,
        });
    }

    /// [`Step::Revert`] — local moves to the oldest ledger-held version that
    /// differs from it, and that content is re-uploaded resolution-class
    /// (proven reissue: the bytes provably rest in the log). A seat holding
    /// nothing different is a no-op.
    fn revert(&mut self, i: usize) {
        let Seats::Causal(v) = &mut self.seats else {
            return;
        };
        let Some(target) = v[i]
            .ledger
            .iter()
            .find(|(s, c)| *s <= v[i].frontier && *c != v[i].local)
            .map(|(_, c)| c.clone())
        else {
            return;
        };
        v[i].local = target.clone();
        let w = v[i].seen;
        self.note_sent(i, &target);
        self.nest.push(CRow {
            author: i,
            content: target,
            derived_through: w,
            is_resolution: true,
            is_retention: false,
        });
    }

    /// The runaway detector: the invariant production violated, and the one a
    /// bounded model can actually check.
    fn guard(&self) -> Result<(), String> {
        if self.nest.len() > 400 {
            return Err(format!(
                "runaway: {} rows — the ancestor is not advancing, so every round \
                 regenerates content the peers merge again",
                self.nest.len()
            ));
        }
        Ok(())
    }

    /// Deliver everything outstanding to every seat until nothing is left —
    /// convergence is a property of the QUIESCED state, so a random schedule
    /// that stops mid-flight is drained before the invariants are read.
    ///
    /// A caught-up seat whose local content is UNRECORDED flushes it as a
    /// reupload first (2026-08-05): production's rescan tick
    /// (`reconcile_ws`) re-records any path whose manifest differs from the
    /// synced one, so a pending edit ([`Step::Edit`] with `publish: false`)
    /// never stays local-only forever — a drain without the flush would
    /// read a merely-unpublished append as a fleet-wide loss.
    fn drain(&mut self, max_rounds: usize) -> Result<(), String> {
        for _ in 0..max_rounds {
            let mut progressed = false;
            for i in 0..self.n {
                let end = self.nest.len();
                if self.seen(i) < end {
                    progressed = true;
                    let start = self.seen(i);
                    self.deliver_batch(i, start, end);
                } else if let Seats::Causal(v) = &self.seats
                    && v[i].last_recorded.as_ref() != Some(&v[i].local)
                {
                    progressed = true;
                    self.reupload(i);
                }
                self.guard()?;
            }
            if !progressed {
                return Ok(());
            }
        }
        Err(format!(
            "did not quiesce in {max_rounds} rounds — {} rows outstanding",
            self.nest.len()
        ))
    }
}

/// Run one schedule to quiescence and return the seats' final contents.
fn run_schedule(
    engine: Engine,
    shape: EditShape,
    seats: usize,
    delivery: Delivery,
    steps: &[Step],
) -> Result<Vec<Vec<u8>>, String> {
    let mut f = Fuzz::new(engine, shape, seats, delivery);
    for step in steps {
        match *step {
            Step::Deliver { seat, n } => {
                let i = seat % seats;
                let start = f.seen(i);
                let end = (start + n).min(f.nest.len());
                f.deliver_batch(i, start, end);
            }
            Step::Redeliver { seat, n } => f.redeliver(seat % seats, n),
            Step::Reupload { seat } => f.reupload(seat % seats),
            Step::Reseal { seat } => f.reseal(seat % seats),
            Step::Edit { seat, publish } => f.edit(seat % seats, publish),
            Step::Revert { seat } => f.revert(seat % seats),
        }
        f.guard()?;
    }
    f.drain(200)?;
    Ok((0..seats).map(|i| f.local(i)).collect())
}

/// The invariants every merge-base policy owes, read off the quiesced state.
///
/// * **Agreement** — all seats hold identical content (`conflicts.md` §
///   Concurrent resolution: "Convergence — not winner-permanence — is the
///   invariant").
/// * **No duplication** — no seat's marker appears twice. This is the
///   *signature* of the production runaway: re-merging already-incorporated
///   content from a stale ancestor duplicates lines, and the duplicates then
///   feed the next round.
/// * **No lost edit** — every seat's marker survives. Asserted only for
///   [`EditShape::OwnLine`], where a clean three-way merge always exists; with
///   overlapping appends a latest-wins fall-back may drop a side by design.
fn check_finals(shape: EditShape, seats: usize, finals: &[Vec<u8>]) -> Result<(), String> {
    let names = ["a", "b", "c", "d"];
    let first = String::from_utf8_lossy(&finals[0]).to_string();
    for (i, f) in finals.iter().enumerate() {
        let got = String::from_utf8_lossy(f).to_string();
        if got != first {
            return Err(format!(
                "seats disagree: {} has {got:?}, a has {first:?}",
                names[i]
            ));
        }
        for name in names.iter().take(seats) {
            let needle = format!("{name}: EDITED");
            let count = got.matches(&needle).count();
            if count > 1 {
                return Err(format!(
                    "seat {} DUPLICATED {name}'s line ({count}×) — the runaway signature — \
                     converged on {got:?}",
                    names[i]
                ));
            }
            if count == 0 && shape == EditShape::OwnLine {
                return Err(format!(
                    "seat {} LOST seat {}'s edit — converged on {got:?}",
                    names[i], name
                ));
            }
        }
    }
    Ok(())
}

fn check_schedule(
    engine: Engine,
    shape: EditShape,
    seats: usize,
    delivery: Delivery,
    steps: &[Step],
) -> Result<(), String> {
    let finals = run_schedule(engine, shape, seats, delivery, steps)?;
    check_finals(shape, seats, &finals)
}

/// The standing alphabet: batch boundaries, duplicate re-delivery, and — since
/// the 2026-08-03 gap ruling closed what they used to refute — both reissue
/// shapes ([`Step::Reupload`], the retry; [`Step::Reseal`], the proven
/// reissue). Their return to the alphabet is the ruling's regression guard:
/// any rule change that re-opens the reissue class fails the random walk.
fn step_strategy() -> impl proptest::strategy::Strategy<Value = Step> {
    use proptest::prelude::*;
    prop_oneof![
        // Weighted toward delivery: churn is the interesting axis, but a
        // schedule that never delivers exercises nothing.
        6 => (0usize..4, 1usize..=4).prop_map(|(seat, n)| Step::Deliver { seat, n }),
        3 => (0usize..4, 1usize..=3).prop_map(|(seat, n)| Step::Redeliver { seat, n }),
        2 => (0usize..4).prop_map(|seat| Step::Reupload { seat }),
        1 => (0usize..4).prop_map(|seat| Step::Reseal { seat }),
    ]
}

/// Search a schedule space for a counterexample, returning the shrunk failure
/// if one exists.
///
/// Deterministically seeded on purpose: these callers assert a *specific*
/// answer (a refutation is found / is not found), so a randomly-seeded search
/// would make the verdict depend on luck — the class of test this project
/// treats as defunct. The random-seeded exploration lives in the `proptest!`
/// block below, where a new seed each run is the point. Persistence is off:
/// a regression file for a failure the caller demands would be replayed first
/// on every later run.
fn search_for_counterexample(
    engine: Engine,
    shape: EditShape,
    seats: usize,
    delivery: Delivery,
    cases: u32,
) -> Option<String> {
    use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    );
    runner
        .run(
            &proptest::collection::vec(step_strategy(), 0..24),
            |steps| {
                check_schedule(engine, shape, seats, delivery, &steps).map_err(TestCaseError::fail)
            },
        )
        .err()
        .map(|e| format!("{e}"))
}

/// **The gate this whole section exists to pass.** `ParkedUntilIncorporated`
/// is the policy the hand-written model BLESSED — it converges in every test
/// above, at 2, 3 and 4 seats — and production then ran away on it, at 2–33
/// minutes of e2e per observation. A fuzzer that cannot refute a policy known
/// to be wrong has not closed the blind spot, so this asserts the refutation
/// rather than the absence of one.
#[test]
fn the_fuzzer_refutes_the_policy_the_hand_written_model_blessed() {
    let found = search_for_counterexample(
        Engine::Legacy(BasePolicy::ParkedUntilIncorporated),
        EditShape::OwnLine,
        3,
        Delivery::PerRow,
        256,
    );
    assert!(
        found.is_some(),
        "the schedule fuzzer must refute ParkedUntilIncorporated — the hand-written \
         model blessed it and production refuted it in 33 minutes. If this test starts \
         passing vacuously, the generated schedules have stopped reaching the churn."
    );
}

/// The control for the gate above: the same search, same budget, against the
/// SHIPPED rules — no counterexample. This is the hardening result the track
/// was opened for; without it the gate would be satisfied by a fuzzer that
/// simply fails everything.
///
#[test]
fn the_same_search_finds_nothing_against_the_shipped_causal_rules() {
    for seats in 2..=4 {
        for delivery in [Delivery::PerRow, Delivery::BatchedWithFold] {
            let found =
                search_for_counterexample(Engine::Causal, EditShape::OwnLine, seats, delivery, 256);
            assert!(
                found.is_none(),
                "shipped rules refuted at {seats} seats ({delivery:?}): {}",
                found.unwrap()
            );
        }
    }
}

/// **The FLIPPED pin — clause 5's THIRD gap (found 2026-08-04, RULED CLOSED
/// the same day: content-licensed novelty accounting, `conflicts.md` clause
/// 5's gap-3 decision record).** This test pinned the defect; it now asserts
/// the ruled behaviour, and the doc comment keeps the defect narrative
/// because the file's standing rule is that sessions re-propose what it
/// already refutes.
///
/// **The defect.** Seat b re-uploads its own content as a lost-ack retry.
/// That row honestly keeps its EDIT stamp (per gap 1's ruling: its record may
/// never have landed, so its bytes may be the only carrier of a real edit) —
/// and applying it advanced b's EDIT-frontier. Every covering resolution a
/// peer published afterwards then read as stale against that inflated
/// edit-frontier and was skipped by rule 2, forever. Clause 5 *recorded* this
/// trade — "an over-count only strands a covering resolution, the pre-ruling
/// behaviour" — and that is sound for the own-line shape, where the bytes
/// converge anyway. For the same-anchor shape it was not a degrade but a
/// permanent divergence: order-choices are the only thing left to converge
/// on, so a stranded covering resolution IS the disagreement. Nothing lost,
/// nothing duplicated — which is exactly why no other assertion in this file
/// caught it.
///
/// **The ruling: novelty is a property of BYTES, not of rows.** The gap-1
/// content rung — which already keyed the receiver's merge-arm skip on
/// content rather than on the stamp — becomes the licence for ALL
/// edit-frontier accounting, the author's own echo sites included: a
/// non-resolution row whose exact bytes are already held at a lower seq
/// advances the full frontier only, never the edit-frontier, whoever
/// authored it and whatever stamp it wears. The row's earliest carrier is
/// where its novelty was counted; counting it again at the reissue's seq is
/// what inflated the frontier. The stamp cannot be fixed writer-side (gap
/// 1's ruling stands — a lost-ack retry must keep the edit stamp), and the
/// nest's head-row replay dedupe cannot catch a retry that lands after a
/// peer's row moved the head — so the licence lives where the proof lives,
/// in the receiver's own ledger.
///
/// **How the defect hid.** The `proptest!` block below used to fuzz
/// [`EditShape::OwnLine`] only, and the hand-written same-anchor test
/// (`concurrent_same_anchor_appends_converge_at_three_and_four_seats`) runs
/// the EMPTY schedule. So the one shape whose fold is order-DEPENDENT was
/// never fuzzed. The blind spot was found from the outside: e2e leg 4a
/// (`tests/e2e-unified/helpers/convergence_legs.py`) drove the shape on the
/// real nest and three real daemons, and every seat both duplicated appends
/// and disagreed. The walk now fuzzes both shapes.
#[test]
fn same_anchor_appends_converge_even_when_a_reissue_enters_the_schedule() {
    // The formerly-diverging shrunk counterexample, kept as a literal: two
    // steps, two seats.
    let steps = [Step::Reupload { seat: 1 }, Step::Deliver { seat: 1, n: 3 }];
    check_schedule(
        Engine::Causal,
        EditShape::Append,
        2,
        Delivery::PerRow,
        &steps,
    )
    .unwrap_or_else(|e| panic!("the ruled rules must converge the minimal reissue schedule: {e}"));

    // Breadth: the same deterministic search that refuted the pre-ruling
    // rules at every seat count and both delivery modes finds nothing.
    for seats in 2..=4 {
        for delivery in [Delivery::PerRow, Delivery::BatchedWithFold] {
            let found =
                search_for_counterexample(Engine::Causal, EditShape::Append, seats, delivery, 256);
            assert!(
                found.is_none(),
                "ruled rules refuted at {seats} seats ({delivery:?}): {}",
                found.unwrap()
            );
        }
    }
}

// ── Two findings from this fuzzer's first run, RULED CLOSED 2026-08-03 ──────
//
// Both were gaps in the RATIFIED clause-5 rule set (`conflicts.md` §
// Concurrent resolution & ancestor freshness), found by this fuzzer's first
// run and adjudicated the same day (the content rung + the proven-reissue
// stamp for finding A; the edit-frontier + covering-resolution adoption for
// finding B). These tests originally pinned the DEFECTS; they now assert the
// ruled behaviour, and their doc comments keep the defect narratives because
// several sessions have re-proposed policies this file already refutes.

/// **FINDING A (ruled closed 2026-08-03) — a reconcile re-upload of
/// already-incorporated content no longer destroys a peer's edit.**
///
/// The defect: seat a re-uploads its own local file before consuming
/// anything — the lost-ack retry shape, stamped `CausalStamp::edit(anchor)`
/// at a stale anchor. That row escaped rule 1 (new seq) and rule 2
/// (`is_resolution = false`), reached rule 4, merged from the ORIGINAL base
/// against a peer that had already folded the same content in →
/// overlapping-identical hunks → latest-wins destroyed the peer's edit. The
/// rule set treated `is_resolution` as the proxy for "carries no novel
/// content", and it is not one: a reissue is neither a resolution nor novel.
///
/// The ruling closes it twice over: the receiver-side CONTENT RUNG skips any
/// stale-watermarked row whose exact bytes the ledger already holds (this
/// test's path — the retry cannot be writer-fixed, its record may never have
/// landed), and the re-seal migration now stamps its proven reissues
/// `is_resolution = true`, which rule 2 skips (see `Step::Reseal`).
#[test]
fn a_reconcile_reupload_of_incorporated_content_is_absorbed_as_a_reissue() {
    for step in [Step::Reupload { seat: 0 }, Step::Reseal { seat: 0 }] {
        let finals = run_schedule(
            Engine::Causal,
            EditShape::OwnLine,
            2,
            Delivery::PerRow,
            std::slice::from_ref(&step),
        )
        .expect("the schedule quiesces");
        let a = String::from_utf8_lossy(&finals[0]).to_string();
        let b = String::from_utf8_lossy(&finals[1]).to_string();
        assert_eq!(a, "a: EDITED\nb: EDITED\n", "seat a holds the union");
        assert_eq!(
            b, "a: EDITED\nb: EDITED\n",
            "seat b keeps its own edit: the reissue ({step:?}) is skipped — by \
             the content rung (retry shape) or rule 2 (proven-reissue stamp) — \
             instead of merging from a stale ancestor and latest-winning over it"
        );
    }
}

/// **FINDING B (ruled closed 2026-08-03) — concurrent same-anchor appends now
/// converge at N ≥ 3.** The defect's minimal schedule was the EMPTY one:
/// three seats append a line each, the log is delivered in plain order, and
/// the seats used to quiesce holding three different permutations.
///
/// The defect's mechanism (kept because it refutes a tempting rule):
///
/// 1. A three-way text merge of insertions at a SHARED anchor is
///    order-dependent, so each seat's fold of its peers' rows yields its own
///    permutation — `a,b,c` / `b,a,c` / `c,a,b`. No edit is lost and nothing
///    is duplicated; the seats simply disagree.
/// 2. Each seat publishes its permutation as a resolution stamped at its own
///    anchor.
/// 3. Every peer then read those rows as STALE RESOLUTIONS (`w < frontier`)
///    and skipped them under the old rule 2 — so no seat ever saw another's
///    permutation, and the disagreement was permanent. Rule 2's "adds zero
///    information" rationale was true of the row SET a resolution
///    incorporated and false of the BYTES it produced whenever the fold is
///    order-dependent.
///
/// The ruling: rule 2's staleness test now reads the EDIT-frontier, and a
/// COVERING resolution (every novel edit incorporated, only order-choices
/// missed) is adopted verbatim when the receiver has nothing unpublished —
/// the nest log's total order arbitrates which permutation wins, per clause
/// 1's "a later resolution may supersede". All seats therefore converge on
/// the log-tail covering resolution; adoption publishes nothing, so the fold
/// terminates. (A resolver-level deterministic ordering was REJECTED: block-
/// level ordering is not confluent across multi-round folds — the result
/// depends on how earlier merges grouped the blocks — and line-level sorting
/// scrambles multi-line user blocks. The log order is the one arbiter every
/// seat already shares.)
///
/// It still bounds `conflicts.md`'s "Equal ancestors make the first winner
/// durable": `resolve_conflict` is deterministic per CALL, which does not
/// make a multi-round FOLD order-independent — convergence comes from
/// supersession, not from fold confluence.
///
#[test]
fn concurrent_same_anchor_appends_converge_at_three_and_four_seats() {
    for seats in 2..=4 {
        let finals = run_schedule(
            Engine::Causal,
            EditShape::Append,
            seats,
            Delivery::PerRow,
            &[],
        )
        .expect("the schedule quiesces");
        let rendered: Vec<String> = finals
            .iter()
            .map(|c| String::from_utf8_lossy(c).to_string())
            .collect();
        // Every marker survives on every seat exactly once: convergence must
        // not be bought with a lost or duplicated append.
        for (i, got) in rendered.iter().enumerate() {
            for name in ["a", "b", "c", "d"].iter().take(seats) {
                assert_eq!(
                    got.matches(&format!("{name}: EDITED")).count(),
                    1,
                    "seat {i} should hold every marker exactly once: {got:?}"
                );
            }
        }
        let distinct: std::collections::BTreeSet<&String> = rendered.iter().collect();
        assert_eq!(
            distinct.len(),
            1,
            "all {seats} seats agree on ONE permutation (the log-tail covering \
             resolution). Divergence here means the covering-resolution \
             adoption regressed: {rendered:?}"
        );
    }
}

/// **THE FLIPPED PIN — the report-plane remainder, RULED CLOSED 2026-08-05
/// (the loser-row ruling: a retention row is a RETENTION VEHICLE, not a
/// change to apply — `conflicts.md` § Concurrent resolution & ancestor
/// freshness carries the decision record).** Found 2026-08-04 by the armed
/// e2e leg 4a; model-reproduced and ruled 2026-08-05. This test pinned the
/// defect; it now asserts the ruled behaviour, and the doc comment keeps the
/// defect narrative per the file's standing rule.
///
/// **The defect.** A daemon merge does not publish one resolution row: it
/// files a resolved report, and the nest's transaction mints a
/// **loser-retention row** — ordinary `sync_changes`, EDIT-class, stamped at
/// the reporter's merge-ancestor seq (`losing_derived_through`), carrying the
/// reporter's pre-merge candidate — plus the winner row, whose stamp upgrade
/// covers its loser only when no same-path row intervenes (rare in a
/// cascade). At N ≥ 3 the loser candidates are never-published INTERMEDIATE
/// merge results: at peers they were stale edit-stamped rows whose bytes no
/// ledger holds (a stale-RESOLUTION skip is byte-free, so a peer that
/// stale-skipped the corresponding winner never held the intermediate
/// either), so the content licence and the class upgrade could not see them,
/// they reached the merge arm from a DEEP ancestor, and the same-anchor
/// merge of a derivative candidate DUPLICATED — the resolver concatenates
/// same-anchor insertions (the live `a, b, c, b, c` signature) — while each
/// such merge filed ANOTHER report, minting the next loser: a cascade that
/// never quiesced in the model. Own-line was not safe either: a loser row's
/// stale content won a latest-wins (its `created_at` is the report's mint
/// time, so it read newest) and the fleet LOST an edit at 3 seats under
/// batch-boundary schedules, at 4 on the plain drain. And no seat count was
/// safe on the append shape — reissue churn duplicated even at 2.
///
/// **The ruling: a retention row is INVISIBLE TO CONTENT.** The nest marks
/// the loser row with the additive `is_retention` wire flag; every receiver
/// accounts its seq into the path FRONTIER and does nothing else — never
/// fetches, applies, merges, or adopts it; never advances the edit-frontier
/// (the reporter-side echo half was the covering-resolution stranding);
/// never holds it in the ledger (it was no one's reflected content, so it
/// can never be a correct common ancestor); and it can never carry a fold.
/// Retention nest-side is untouched (GC-pinning, candidate listing);
/// clause 1's `use_other_version` re-points via a NEW head row. A
/// receiver that ignores the marker degrades to the unconditional-fold
/// behaviour (the refuted model above).
/// Shared rung: `causal::IncomingVerdict::RetentionRow` — both hosts and
/// this model judge through it, so the licence cannot fork.
///
/// **Why the fuzzer blessed the gap-3 ruling anyway (2026-08-04):** the model
/// had no report plane at all — publishes minted one resolution row at the
/// capture prefix — and its stale-resolution arm HELD the skipped bytes,
/// which production (skipping before fetch) cannot; the over-hold made every
/// later loser row read as a reissue of held content. Model-level convergence
/// does NOT imply production convergence; the module doc's standing warning,
/// paid for a second time.
#[test]
fn report_plane_retention_rows_are_accounted_and_never_applied() {
    // (1) The formerly-duplicating deterministic witness: seat a merges both
    // peers' seeds (filing two reports; the second loser row carries a's
    // never-published intermediate at w = 0), then seat b consumes the log.
    // Pre-ruling, b's merge of that loser row from the deep ancestor
    // concatenated the same-anchor insertions (`b, a, c, a, b`). Under the
    // ruling the loser row is accounted and skipped: every marker exactly
    // once.
    let mut f = Fuzz::new(Engine::Causal, EditShape::Append, 3, Delivery::PerRow);
    f.deliver_batch(0, 0, 3);
    let end = f.nest.len();
    assert!(
        end > 3,
        "seat a's two merges must have filed reports (loser + winner rows)"
    );
    assert!(
        f.nest.iter().any(|r| r.is_retention),
        "the reports must have minted marked retention rows"
    );
    f.deliver_batch(1, 0, end);
    let got = String::from_utf8_lossy(&f.local(1)).to_string();
    for name in ["a", "b", "c"] {
        assert_eq!(
            got.matches(&format!("{name}: EDITED")).count(),
            1,
            "seat b must hold {name}'s append exactly once (retention rows \
             accounted, never merged): {got:?}"
        );
    }

    // (2) The formerly-runaway cascade quiesces: the empty 3-seat schedule
    // converges with every marker exactly once on every seat (the breadth
    // tests above re-assert this at 2–4 seats; this is the pinned minimal
    // reproduction).
    let finals = run_schedule(Engine::Causal, EditShape::Append, 3, Delivery::PerRow, &[])
        .expect("the loser-row cascade must quiesce under the retention ruling");
    check_finals(EditShape::Append, 3, &finals)
        .expect("agreement with no duplication and no loss at 3 seats");
}

/// **THE FLIPPED PIN — the leg-4 live refutation, REPRODUCED and RULED
/// CLOSED 2026-08-05 (`conflicts.md` § Concurrent resolution & ancestor
/// freshness carries the decision record).** Two armed runs of the 3-seat
/// e2e engine cell RED'd leg 4 (the unique-anchor leg, green pre-ruling),
/// losing a different seat's line each run — while this model stayed green,
/// because its own-novelty machinery was STRICTLY SAFER than production's (a
/// seq-exact in-flight fold where production has the pre-pass + the
/// unknown-seq report flag + defer-and-drop). With the machinery
/// production-shaped, the schedule fuzzer found the loss in its first pass:
/// FIVE PLAIN DELIVERIES, no churn steps at all — pinned below. This test
/// pinned the defect; it now asserts the ruled behaviour, and the doc
/// comment keeps the defect narrative per the file's standing rule.
///
/// **The defect chain (all three links needed):** (1) a licensed
/// fast-forward — the recorded witness allows adopting over
/// published-unechoed local work — dropped a seat's own edit from local,
/// after which that row's echo FALSELY advanced the frontiers ("local
/// reflects it" was no longer true); (2) the seat's next merge stamped
/// `winning_derived_through = incoming_seq` unconditionally, OVER-CLAIMING
/// coverage of the dropped row, and every covering test downstream trusted
/// the claim (at the author arm this is the live red's own-covering-re-adopt
/// suspect line); (3) the engine's covering-adopt defer consumed-and-DROPPED
/// the very repair rows that carried the true union (the anchor advanced
/// regardless, and nothing re-lists a consumed row).
///
/// **The ruling (three conjuncts, each fuzzer-validated):** a verbatim adopt
/// is NEVER taken while the seat's own pending rows are unlisted — the
/// recorded-witness window (`local != base`) and the unknown-seq report
/// window (`report_pending`) both defer-as-cap, transient-class (the
/// seal-cap family): the cursor holds below the row and the next listing —
/// which contains the pending rows, by log contiguity — retries it with
/// exact frontiers, so the gap-3 anti-duplication adopt is delayed, never
/// weakened. The winner claim is HONEST under the lower-bound law: the
/// incoming seq only when every gap row below it is the reporter's own
/// (their content is in local by the defer invariant), else the pre-merge
/// frontier. And the fold's licence gains the carrier-never-DEFERRED
/// conjunct beside carrier-never-stale — a carrier that would defer must
/// not carry (its folded rows' content would be consumed under a row that
/// never applied); while a pending window is open the listing processes
/// per-row.
#[test]
fn leg4_unique_anchor_plain_deliveries_converge() {
    let steps = [
        Step::Deliver { seat: 0, n: 1 },
        Step::Deliver { seat: 2, n: 1 },
        Step::Deliver { seat: 0, n: 1 },
        Step::Deliver { seat: 1, n: 1 },
        Step::Deliver { seat: 0, n: 1 },
    ];
    check_schedule(
        Engine::Causal,
        EditShape::OwnLine,
        3,
        Delivery::PerRow,
        &steps,
    )
    .unwrap_or_else(|e| panic!("the ruled rules must converge the leg-4 reproduction: {e}"));
}

/// The PLAIN-schedule search: Deliver/Redeliver/Reupload/Edit steps — no
/// reissue-churn `Reseal`, so no schedule contains the sanctioned
/// stale-anchored-revert trade — and the check asserts the e2e leg's OWN
/// invariant on the Append shape: agreement AND every authored append
/// exactly once. [`check_finals`] deliberately does not assert loss for
/// [`EditShape::Append`] (a latest-wins fall-back may drop a side under
/// reissue churn), which means the standing search is STRUCTURALLY BLIND to
/// the live leg-4a signature — seats AGREEING on a body missing appends.
/// This search exists to see exactly that; the [`Step::Edit`] shapes
/// (anchor-stamped mid-run uploads + the pending window) are what the
/// 2026-08-05 armed-run logs showed the loss riding on.
/// RED until row 8's ruling lands.
#[test]
fn plain_schedules_keep_every_append_on_both_hosts() {
    use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
    let plain_step = {
        use proptest::prelude::*;
        prop_oneof![
            4 => (0usize..4, 1usize..=4).prop_map(|(seat, n)| Step::Deliver { seat, n }),
            1 => (0usize..4, 1usize..=3).prop_map(|(seat, n)| Step::Redeliver { seat, n }),
            // The live "no-churn" cell still has a host's periodic
            // reconcile re-send (a rescan tick re-records a path whose
            // manifest differs from the one it last synced), so the
            // reupload shape belongs to the PLAIN class. No sanctioned loss
            // hides in it: a reupload of current content is
            // proven-reissue-stamped whenever ledger/base/last-recorded hold
            // the bytes, and the model has no genuine-revert step — the one
            // shape whose latest-wins drop the ruling sanctions.
            1 => (0usize..4).prop_map(|seat| Step::Reupload { seat }),
            // The watcher shapes (2026-08-05): a published mid-run append
            // (claims the seat's CURRENT anchor — high, unlike the seeds'
            // w = 0) and a pending one (disk-only until a later Reupload or
            // a pre-empting conflict merge).
            2 => (0usize..4, proptest::bool::ANY)
                .prop_map(|(seat, publish)| Step::Edit { seat, publish }),
        ]
    };
    {
        let engine = Engine::Causal;
        for seats in [3usize, 4] {
            let mut runner = TestRunner::new_with_rng(
                Config {
                    cases: 512,
                    failure_persistence: None,
                    ..Config::default()
                },
                TestRng::deterministic_rng(RngAlgorithm::ChaCha),
            );
            let found = runner
                .run(
                    &proptest::collection::vec(plain_step.clone(), 0..24),
                    |steps| {
                        let mut f = Fuzz::new(engine, EditShape::Append, seats, Delivery::PerRow);
                        for step in &steps {
                            match *step {
                                Step::Deliver { seat, n } => {
                                    let i = seat % seats;
                                    let start = f.seen(i);
                                    let end = (start + n).min(f.nest.len());
                                    f.deliver_batch(i, start, end);
                                }
                                Step::Redeliver { seat, n } => f.redeliver(seat % seats, n),
                                Step::Reupload { seat } => f.reupload(seat % seats),
                                Step::Reseal { seat } => f.reseal(seat % seats),
                                Step::Edit { seat, publish } => f.edit(seat % seats, publish),
                                Step::Revert { seat } => f.revert(seat % seats),
                            }
                            f.guard().map_err(TestCaseError::fail)?;
                        }
                        f.drain(200).map_err(TestCaseError::fail)?;
                        f.check_exactly_once().map_err(TestCaseError::fail)
                    },
                )
                .err();
            assert!(
                found.is_none(),
                "{engine:?} host loses/duplicates an append on a PLAIN schedule at {seats} \
                 seats: {}",
                found.map(|e| e.to_string()).unwrap()
            );
        }
    }
}

/// **THE DETERMINISTIC PENDING-APPEND PIN — the 2026-08-05 armed-run loss
/// chain, scripted (both hosts — the defect is in the SHARED law, not a
/// daemon fork).** The chain, exactly as the seat logs showed it:
///
/// 1. Seat b's append sits in the PENDING window (disk only — no row, no
///    recorded witness) when a peer's row arrives: the divergence goes to
///    the resolver, the merge consumes the append, and its ONLY carriers
///    are now the report's rows — the retention loser (invisible to content
///    by the loser-row ruling) and the winner (whose honest claim is LOW:
///    b's pre-merge frontier).
/// 2. The winner is rightly stale-skipped at peers whose edit-frontiers
///    count novelty it misses — the append's last off-seat carrier dies,
///    byte-free.
/// 3. At b: the merge advanced base to local, so the recorded-witness defer
///    window is closed; the retention row's listing clears `report_pending`
///    (the pre-pass clear the loser-row ruling added — without it every
///    covering winner deferred forever). Both guards are now down while b's
///    pre-merge novelty has NO seq anywhere — both frontiers are
///    structurally blind to it.
/// 4. A covering row whose watermark honestly dominates b's frontiers — a
///    peer's mid-run edit stamped at its consumption anchor, or a covering
///    resolution — FastForwards over b's merged head. The append's last
///    live copy is gone; every seat agrees on a body without it.
///
/// RED until row 8's ruling lands: reproducing the live loss model-side is
/// this pin's first job.
#[test]
fn a_pending_append_consumed_by_a_conflict_merge_survives() {
    {
        let engine = Engine::Causal;
        let mut f = Fuzz::new_clean(engine, 3);
        f.edit(0, true); // row 1: a's append, w = 0
        f.deliver_batch(2, 0, 1); // c fast-forwards onto a's append
        f.edit(2, true); // row 2: c's append ON TOP (content a+c, w = 1)
        f.edit(1, false); // b's append — PENDING, never published
        f.deliver_batch(1, 0, 1); // b conflict-merges a's row → report (loser RET, winner claim 1)
        f.deliver_batch(0, 0, 2); // a: own echo, then adopt of c's covering row
        let end = f.nest.len();
        f.deliver_batch(1, 1, end); // b: c's row 2 defers (report window), then re-lists:
        let end = f.nest.len(); //     the retention row clears the flag → the adopt fires
        f.deliver_batch(1, 1, end);
        f.drain(200).expect("the pending-append chain quiesces");
        f.check_exactly_once().unwrap_or_else(|e| {
            panic!("{engine:?} host lost the pending append (the live leg-4a loss): {e}")
        });
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        cases: 128,
        ..Default::default()
    })]

    /// The shipped rules hold the union under schedules nobody wrote down:
    /// arbitrary batch boundaries and duplicate re-delivery, interleaved
    /// across seats, per-row and batched-with-fold alike — over BOTH edit
    /// shapes. The shape axis joined the walk with the gap-3 ruling: fuzzing
    /// `OwnLine` only is exactly how the third gap hid (the one shape whose
    /// fold is order-dependent met none of the churn alphabet).
    ///
    /// Randomly seeded on each run by design (`testing.md` § point 17 — random
    /// walks over the real input doors); a failure persists to
    /// `proptest-regressions/` — commit that file if one ever appears.
    #[test]
    fn causal_rules_hold_the_union_under_random_delivery_schedules(
        seats in 2usize..=4,
        batched in proptest::bool::ANY,
        append in proptest::bool::ANY,
        steps in proptest::collection::vec(step_strategy(), 0..24),
    ) {
        let delivery = if batched { Delivery::BatchedWithFold } else { Delivery::PerRow };
        let shape = if append { EditShape::Append } else { EditShape::OwnLine };
        let outcome = check_schedule(Engine::Causal, shape, seats, delivery, &steps);
        proptest::prop_assert!(outcome.is_ok(), "{}", outcome.unwrap_err());
    }
}
/// **The fresh-bind park and its release, at the oracle** (the cap-release
/// ruling, 2026-09-21 — `conflicts.md` clause 5).
/// Every seat this model had ever run was SEEDED — frontier 0 tracked, a
/// ledger holding row 0 — so `judge_incoming`'s untracked-frontier arm was
/// never fuzzed, and the shape it parked stayed invisible here while
/// production parked at tier_1 and live (2026-09-20). The seat below is what
/// a fresh bind actually is: no frontier, no base, no ledger; its local file
/// already RECORDED by the startup converge (the ack counts its seq into the
/// edit-frontier — production's `record_change` Ok arm; here the listing
/// pre-pass does, as on both hosts), and its first listing carrying a peer's
/// earlier create at the same path plus its own echo.
///
/// Pre-ruling the peer row judged fast-forward on the recorded witness
/// alone, the cap held the cursor at 0, the own echo (seq 2) sat ABOVE the
/// cap and never processed — so the frontier never advanced and the next
/// listing re-judged identically: `seen` stayed 0 for ever. Under the ruling
/// the untracked arm honours the counted edit-frontier: the peer row is a
/// SIBLING, merged and reported; the own echo lands; and the two seats
/// converge onto the reported union, exactly as the seeded schedules do.
#[test]
fn a_fresh_binds_counted_own_row_releases_the_defer_cap() {
    let peer_body = b"shared: peer\n".to_vec();
    let user_body = b"shared: user\n".to_vec();
    // Log: seq 1 = the peer's create (never deleted), seq 2 = the fresh
    // seat's own converge upload.
    let mut nest = vec![
        CRow {
            author: 0,
            content: peer_body.clone(),
            derived_through: 0,
            is_resolution: false,
            is_retention: false,
        },
        CRow {
            author: 1,
            content: user_body.clone(),
            derived_through: 0,
            is_resolution: false,
            is_retention: false,
        },
    ];
    // The peer: an ordinary seeded seat — row 0 is the seed, here the EMPTY
    // pre-history of a path it created — that authored row 1 and took its
    // own echo.
    let peer = CSeat {
        name: "peer",
        local: peer_body.clone(),
        base: peer_body.clone(),
        frontier: 1,
        edit_frontier: 1,
        content_frontier: None,
        pending_winners: Vec::new(),
        ledger: vec![(0, Vec::new()), (1, peer_body.clone())],
        seen: 1,
        last_recorded: Some(peer_body.clone()),
        fresh_bind: false,
    };
    let fresh = CSeat {
        name: "fresh",
        local: user_body.clone(),
        base: Vec::new(),
        frontier: 0,
        edit_frontier: 0,
        content_frontier: None,
        pending_winners: Vec::new(),
        ledger: Vec::new(),
        seen: 0,
        last_recorded: Some(user_body.clone()),
        fresh_bind: true,
    };
    assert_eq!(
        fresh.frontiers().frontier,
        None,
        "a fresh bind has no path frontier"
    );
    let mut seats = [peer, fresh];

    // One listing, delivered exactly as `converge_causal` delivers it:
    // pre-pass, peers first, own echoes last, under the transient cap.
    // Nothing folds on this shape — the carrier is the latest PEER row and
    // nothing in the listing sits below it (the carrier-less form).
    fn deliver(seat: &mut CSeat, nest: &[CRow], me: usize) -> Vec<(usize, CPublish)> {
        let batch: Vec<usize> = (seat.seen..nest.len()).collect();
        pre_pass(seat, nest, &batch, me);
        let mut order: Vec<usize> = Vec::with_capacity(batch.len());
        order.extend(batch.iter().copied().filter(|&idx| nest[idx].author != me));
        order.extend(batch.iter().copied().filter(|&idx| nest[idx].author == me));
        let mut publishes = Vec::new();
        let mut cap: Option<usize> = None;
        for idx in order {
            let seq = idx + 1;
            if cap.is_some_and(|c| seq >= c) {
                continue;
            }
            let f = seat.frontier;
            let claim = if ((f + 1)..seq).all(|gs| nest[gs - 1].author == me) {
                seq
            } else {
                f
            };
            let row = nest[idx].clone();
            match causal_apply(seat, seq, &row, me, idx as i64, claim) {
                CApply::Defer => cap = Some(seq),
                CApply::Done(publish) => {
                    seat.seen = seq.max(seat.seen);
                    if let Some(p) = publish {
                        publishes.push((seat.seen, p));
                    }
                }
            }
        }
        publishes
    }

    // The fresh seat's first listing: [peer create@1, own echo@2].
    let published = deliver(&mut seats[1], &nest, 1);
    assert_eq!(
        seats[1].seen, 2,
        "the fresh bind PARKED: the peer's create was capped and the own echo \
         above the cap never processed (pre-ruling `seen` stayed 0 on every \
         listing)"
    );
    assert_ne!(
        seats[1].local, peer_body,
        "the peer's create was adopted verbatim over the recorded local file"
    );
    assert_eq!(
        seats[1].frontiers().frontier,
        Some(2),
        "the own echo must land: the path frontier starts tracking at its seq"
    );
    let (w, report) = published
        .into_iter()
        .next()
        .expect("the sibling create must be merged and reported, never held");
    let union = winner_bytes(report.clone());
    assert!(String::from_utf8_lossy(&union).contains("shared: peer"));
    assert!(String::from_utf8_lossy(&union).contains("shared: user"));
    assert_eq!(
        seats[1].local, union,
        "the fresh seat holds the merged union"
    );
    note_published(&mut seats[1].last_recorded, 1, &report);
    mint_publish(&mut nest, 1, w, report);

    // Both seats drain the log — the model's ordinary convergence.
    for _round in 0..12 {
        let mut progressed = false;
        #[allow(clippy::needless_range_loop)]
        // The seat number is the model's identity (`deliver` and
        // `mint_publish` take it), not just a cursor — as in `converge` above.
        for me in 0..2 {
            if seats[me].seen < nest.len() {
                progressed = true;
                for (w, p) in deliver(&mut seats[me], &nest, me) {
                    note_published(&mut seats[me].last_recorded, me, &p);
                    mint_publish(&mut nest, me, w, p);
                }
            }
        }
        if !progressed {
            break;
        }
    }
    assert_eq!(seats[0].seen, nest.len(), "the peer must drain the log");
    assert_eq!(
        seats[1].seen,
        nest.len(),
        "the fresh seat must drain the log"
    );
    assert_eq!(
        seats[0].local, seats[1].local,
        "the seats did not converge after the release"
    );
    for seat in &seats {
        let text = String::from_utf8_lossy(&seat.local);
        assert!(
            text.contains("shared: peer"),
            "{}: lost the peer's create",
            seat.name
        );
        assert!(
            text.contains("shared: user"),
            "{}: lost the user's file",
            seat.name
        );
    }
}

/// **A revert to held bytes does not park a covering peer edit — at the
/// oracle** (the held-bytes release, 2026-09-21 — `conflicts.md` clause 5). Both seats merge the seeds and drain the log to
/// its tail, converged on the union; b then appends a fresh edit — its
/// watermark the tail, so it COVERS everything a reflects — and a REVERTS to
/// the seed base, a version its ledger holds at seq 0, re-uploading it
/// resolution-class (neither frontier counts the row). a's next listing
/// carries b's covering edit BELOW a's own revert row. Pre-ruling: the edit
/// judged fast-forward, the cap held (local ≠ live base), the revert's echo
/// sat above the cap and never processed, and the schedule never quiesced
/// (`drain` ran out of rounds). Under the ruling the cap does not hold for
/// bytes the ledger already has: the edit is adopted, the revert's echo
/// re-asserts the current bytes as a stale-declined own tail, and both seats
/// converge on b's edit — the stale revert loses at its author exactly as
/// rule 5 skips it at peers. (An earlier schedule that edited before the
/// drain never parked: the reports' retention rows had advanced a's full
/// frontier past the edit's watermark, so it merged instead — the trace is
/// the oracle's own witness that the park needs a COVERING edit.)
#[test]
fn a_reverts_own_held_bytes_do_not_park_a_covering_peer_edit() {
    const ALL: usize = 50; // clamps to the log's tail
    let steps = [
        Step::Deliver { seat: 0, n: 2 },
        Step::Deliver { seat: 1, n: 2 },
        Step::Deliver { seat: 0, n: ALL },
        Step::Deliver { seat: 1, n: ALL },
        Step::Deliver { seat: 0, n: ALL },
        Step::Deliver { seat: 1, n: ALL },
        Step::Deliver { seat: 0, n: ALL },
        Step::Deliver { seat: 1, n: ALL },
        Step::Edit {
            seat: 1,
            publish: true,
        },
        Step::Revert { seat: 0 },
        Step::Deliver { seat: 0, n: ALL },
    ];
    let finals = run_schedule(
        Engine::Causal,
        EditShape::OwnLine,
        2,
        Delivery::PerRow,
        &steps,
    )
    .unwrap_or_else(|e| {
        panic!("the reverter PARKED behind its own held-bytes row (pre-ruling: no quiescence): {e}")
    });
    // Agreement, then every marker exactly once by WHOLE line (`check_finals`
    // counts substrings, and the seed "b: EDITED" is a prefix of the
    // serialized "b: EDITED0" — see `Fuzz::check_exactly_once`).
    assert_eq!(
        finals[0], finals[1],
        "the seats did not converge after the release"
    );
    let a = String::from_utf8_lossy(&finals[0]);
    for marker in ["a: EDITED", "b: EDITED", "b: EDITED0"] {
        assert_eq!(
            a.lines().filter(|l| *l == marker).count(),
            1,
            "{marker:?} must survive exactly once at the reverter (got {a:?}) — b's \
             covering edit adopted, the stale revert lost as rule 5 trades it"
        );
    }
}
