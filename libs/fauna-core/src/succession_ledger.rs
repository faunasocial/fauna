//! The account's **succession ledger** — the owner chain, the signed grant
//! log and the three adjudication planes a succession raises — as one value
//! with one join, and as the rows of the `fauna.state.succession-ledger` kind
//! (`config-dissolution.md` § Phases and gates → *Bounded rows* → *The
//! ledger* owns the shape; `succession-aftermath.md` § Adjudicating what the
//! aftermath carries across owns the marks; `identity-succession.md` owns the
//! chain).
//!
//! **One statement of the rule (P1).** [`SuccessionLedger::merge`] is the
//! six-field join (before the `__config` rail retired at closure step (6),
//! the blob arm ran it too; see `docs/goal/architecture/config-dissolution.md`),
//! and the plane arm (`fauna_protocol::merge_policy`) runs its per-row halves
//! ([`SuccessionLedgerRecord::merge`]) — so the whole and the rows cannot
//! drift.
//!
//! **Five row families under one kind, the key's first segment
//! dispatching** ([`SuccessionLedgerRowKey`]): the ONE `chain` row
//! ([`ChainState`], the one row that grows — 34 B per succession ceremony);
//! one write-once row per signed [`GrantEvent`]; one row per mark on each of
//! the three planes. The grant log and the marks are one row per entity
//! because nothing in either may ever be dropped (the log is append-only
//! forensic history; a decided mark is kept, never deleted), so per-entity
//! rows are the only bounded shape.
//!
//! **Where the allowed-signer clause runs.** On the plane, at READ:
//! [`SuccessionLedger::fold`] seeds the chain with the runtime's own identity,
//! joins the stored `chain` row, and keeps only the events verifying against
//! the folded chain. A stranger's event row is stored and
//! invisible.
//!
//! **Decode posture.** A row's value must decode as its key's type and
//! re-encode to the SAME canonical bytes — a newer build's field is refused
//! (`BadValue` at the plane arm, re-presented on the next reconcile) rather
//! than silently stripped from a re-encoded union. [`ChainState`] is new and
//! carries `deny_unknown_fields` besides; the four pre-existing value types
//! keep their tolerant serde, so the round-trip check, not the type, gives
//! the plane its strict posture.

use serde::{Deserialize, Serialize};

use crate::data::{
    FilterUnattestedMark, GrantUnattestedMark, MemberReview, MemberUnattestedItem,
    MemberUnattestedReason, UnattestedVerdict,
};
use crate::encoding::{canonical_decode, canonical_encode, canonical_tiebreak_key as tiebreak_key};
use crate::error::{Error, Result};
use crate::grant_event::{GRANT_EVENT_SIGNATURE_LEN, GrantEvent};
use crate::identity::ActorId;

/// Length of a grant id — `GrantEvent::grant_id`, chosen at mint.
pub const GRANT_ID_LEN: usize = 16;

/// The owner chain: the current identity and every identity it succeeded.
///
/// The value of the ONE `chain` row, and the chain half of
/// [`SuccessionLedger`]. `prior_actor_ids` is held sorted ascending by id
/// bytes, each id once, and never names `actor_id` itself — the shape every
/// honest writer produces, checked at decode ([`decode_succession_ledger_row`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainState {
    /// The current identity.
    pub actor_id: ActorId,
    /// Every retired identity of this chain.
    pub prior_actor_ids: Vec<ActorId>,
}

impl ChainState {
    /// A fresh chain naming only `actor_id`.
    #[must_use]
    pub fn of(actor_id: ActorId) -> Self {
        Self {
            actor_id,
            prior_actor_ids: Vec::new(),
        }
    }

    /// Every identity in the chain — the allowed signers of its grant log.
    pub fn members(&self) -> impl Iterator<Item = &ActorId> {
        std::iter::once(&self.actor_id).chain(self.prior_actor_ids.iter())
    }

    /// The chain half: the **gated** sorted union and the successor
    /// pick. When [`one_owner_chain`] refuses the pair, `self`'s chain is kept
    /// (sorted) — "two chains never converge".
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        let linked = one_owner_chain(self, other);
        // The owner chain itself: a sorted union — a predecessor never stops
        // being one, and sorting keeps `merge(a,b) == merge(b,a)` byte-exact.
        //
        // Gated on the same predicate the allowed-signer extension takes, and
        // that pairing is load-bearing rather than tidy: absorbing a list we
        // declined to license would let the next merge license it out of
        // *our* chain, so the refusal would last exactly one merge.
        let mut prior_actor_ids = self.prior_actor_ids.clone();
        if linked {
            for id in &other.prior_actor_ids {
                if !prior_actor_ids.contains(id) {
                    prior_actor_ids.push(*id);
                }
            }
        }
        prior_actor_ids.sort_by_key(|a| a.0);
        Self {
            // **The successor's id, not the merger's.** When one side names
            // the other a predecessor, the *other* side is the successor and
            // its id survives whichever device is merging — so a straggler
            // still on a predecessor identity never walks the re-key back.
            // Symmetric by construction; the union above keeps the predecessor
            // in the chain either way.
            actor_id: if other.prior_actor_ids.contains(&self.actor_id) {
                other.actor_id
            } else {
                self.actor_id
            },
            prior_actor_ids,
        }
    }

    /// The `chain` row's plane arm: [`Self::merge`] where the gate admits
    /// the pair, a refusal where it does not. A refused pair is a
    /// row-content refusal (`BadValue`): the walk skips the incoming row on
    /// every reconcile and each replica keeps its own chain.
    ///
    /// Two chains each naming the other's identity a predecessor are refused
    /// too: the successor pick has no answer for them (each side would pick
    /// the other), and the result would name its own identity as retired.
    ///
    /// # Errors
    /// The pair is not one owner chain, or its two identities name each
    /// other retired.
    pub fn join(&self, other: &Self) -> Result<Self> {
        if !one_owner_chain(self, other) {
            return Err(Error::Encoding(
                "succession-ledger chain rows describe two owner chains (a fork)".into(),
            ));
        }
        if self.actor_id != other.actor_id
            && self.prior_actor_ids.contains(&other.actor_id)
            && other.prior_actor_ids.contains(&self.actor_id)
        {
            return Err(Error::Encoding(
                "succession-ledger chain rows each name the other's identity retired".into(),
            ));
        }
        Ok(self.merge(other))
    }

    /// Record `retired` in the chain, then re-point `actor_id` to
    /// `successor` — **the one re-point rule every door of a succession's
    /// re-key runs** (`fauna-client-config`'s ledger port and the plane's
    /// succession write). Both halves
    /// or neither: the allowed-signer clause licenses a chain's grant events
    /// only across a recorded link, so a re-point without the record leaves
    /// the predecessor's signed events verifiable against nobody.
    ///
    /// The caller picks `retired` and owns what it proves — the plane write
    /// passes the ATTESTED predecessor, never an id a row asserts.
    /// Re-pointing an identity onto itself records nothing.
    pub fn repoint_to_successor(&mut self, retired: ActorId, successor: ActorId) {
        if retired != successor && !self.prior_actor_ids.contains(&retired) {
            self.prior_actor_ids.push(retired);
            self.prior_actor_ids.sort_by_key(|a| a.0);
        }
        self.actor_id = successor;
    }
}

/// Whether two chains are demonstrably **one owner chain** — the gate on
/// everything a merge takes from the other side's `prior_actor_ids` (both
/// the allowed-signer extension and the chain's own union).
///
/// Three arms, one per legitimate merge direction:
///
/// * `their.actor_id == our.actor_id` — two devices of one account.
/// * `our.prior_actor_ids.contains(&their.actor_id)` — we re-keyed, they are
///   a straggler still on a predecessor of ours.
/// * `their.prior_actor_ids.contains(&our.actor_id)` — they re-keyed, we have
///   not; the reverse-direction merge a not-yet-re-keyed device performs.
///
/// **Symmetric by construction** (arms 2 and 3 swap into each other, arm 1 is
/// self-symmetric), which is what preserves `merge(a,b) == merge(b,a)`.
///
/// ⚠ **Why an unrelated chain must not license its own prior list.** Being
/// able to write a value is not evidence about whose chain it describes: a
/// plain value carries no signature over authorship, a plane entry's
/// signature proves only that a fleet device's writer key signed it, and the
/// adversary this whole feature answers — a pre-succession seed thief — holds
/// every pre-succession key by definition. The gate is about content, so it
/// stays a property of the value type at every door
/// (`config-dissolution.md` → *The ledger*: the tip does NOT subsume it).
#[must_use]
pub fn one_owner_chain(ours: &ChainState, theirs: &ChainState) -> bool {
    theirs.actor_id == ours.actor_id
        || ours.prior_actor_ids.contains(&theirs.actor_id)
        || theirs.prior_actor_ids.contains(&ours.actor_id)
}

/// Rank of an adjudication verdict, **most restrictive first** — the surviving
/// verdict on a key held by both replicas is the minimum.
///
/// The ordering is one rule doing three jobs, which is why it replaced the
/// three-arm match it grew out of:
///
/// * Both decided arms rank below both undecided ones, which *is* the ratified
///   "a decided verdict beats `Open`" (`succession-aftermath.md` § Re-key scope):
///   the union stays lossless and resurrection-free.
/// * *Removed* beats *Kept*. Two devices genuinely deciding one item differently
///   used to keep whichever ran the merge; the restrictive verdict is the safe
///   direction — a person the owner removed is never silently re-admitted, and a
///   *Kept* is re-givable where a wrongly-dropped removal is not observable.
/// * An unnameable verdict ([`UnattestedVerdict::Other`], a newer build's value)
///   ranks between them: it survives a *Kept* — since it renders as still open,
///   losing it would silently vouch where the newer build may not have — and is
///   preserved verbatim, including which of two differing ones wins (the smaller
///   raw string).
#[must_use]
pub fn verdict_precedence(verdict: &UnattestedVerdict) -> (u8, &str) {
    match verdict {
        UnattestedVerdict::Removed => (0, ""),
        UnattestedVerdict::Other(raw) => (1, raw.as_str()),
        UnattestedVerdict::Kept => (2, ""),
        UnattestedVerdict::Open => (3, ""),
    }
}

/// The per-key half of every mark plane's merge: the [`verdict_precedence`]
/// minimum, `ours` kept on a tie (a tie is the same verdict).
fn join_verdict(ours: &mut UnattestedVerdict, theirs: &UnattestedVerdict) {
    if verdict_precedence(theirs) < verdict_precedence(ours) {
        *ours = theirs.clone();
    }
}

/// The owner chain, the signed grant log and the three adjudication planes a
/// succession raises — the six succession fields as one value with
/// one join, and the READ fold of the `fauna.state.succession-ledger` rows.
///
/// Serde crosses it whole, as canonical bytes, where no plane row does — the
/// web SPA lends its core chunk's ledger to the other chunks as data
/// over the account port (`fauna_client_config::succession_ledger_port`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuccessionLedger {
    /// The chain's current identity.
    pub actor_id: ActorId,
    /// The chain's retired identities.
    pub prior_actor_ids: Vec<ActorId>,
    /// The signed grant log.
    pub grant_events: Vec<GrantEvent>,
    /// The un-adjudicated marks on carried-across grants.
    pub unattested_grant_marks: Vec<GrantUnattestedMark>,
    /// The open review items on people a succession's group sweep could not
    /// vouch for.
    pub unattested_member_items: Vec<MemberUnattestedItem>,
    /// The un-adjudicated marks on carried-across email filter rules.
    pub unattested_filter_marks: Vec<FilterUnattestedMark>,
}

impl SuccessionLedger {
    /// An empty ledger whose chain names only `actor_id`.
    #[must_use]
    pub fn empty(actor_id: ActorId) -> Self {
        Self {
            actor_id,
            prior_actor_ids: Vec::new(),
            grant_events: Vec::new(),
            unattested_grant_marks: Vec::new(),
            unattested_member_items: Vec::new(),
            unattested_filter_marks: Vec::new(),
        }
    }

    /// The chain half.
    #[must_use]
    pub fn chain(&self) -> ChainState {
        ChainState {
            actor_id: self.actor_id,
            prior_actor_ids: self.prior_actor_ids.clone(),
        }
    }

    /// The cross-device join of two ledgers — the one rule for the
    /// six succession fields.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        let (our_chain, their_chain) = (self.chain(), other.chain());
        // Every grant event taken from `other` must verify against the owner
        // chain before it enters the audit log — the Ed25519 signature, not
        // the transport's seal, is what makes the forensic grant log
        // "unforgeable … or a malicious box forges/erases its own entries"
        // (`grant_event.rs`). A non-verifying event is dropped rather than
        // trusted: this closes a forged entry reaching the audit surface, AND
        // the cross-owner blend (nothing else asserts the two sides are one
        // owner's). Our own side is already trusted — we signed it locally —
        // so only `other` is re-verified.
        //
        // **The allowed-signer set is the owner *chain*, not the current owner
        // alone** (identity-succession, 2026-08-03). A succession re-points
        // `actor_id` while the ledger's pre-succession events remain signed by
        // the predecessor — verifying against the current owner only would let
        // the identity transition itself erase forensic history. Their
        // `prior_actor_ids` count ONLY once the two sides are shown to be one
        // owner chain ([`one_owner_chain`]): taking them on no predicate at
        // all donated signers permanently — the list is unbounded, never
        // shrinks, and is absorbed into ours by the chain merge, so one merge
        // licensed a stranger on every future merge. Worse than a forged *mint*,
        // a donated signer's `Revoke` is terminal in the fold with no
        // timestamp comparison, so it silently kills an owner-minted grant —
        // and no mark can show it, because the suppressed grant is gone from
        // `current_grants` before any surface reads it.
        let linked = one_owner_chain(&our_chain, &their_chain);
        let mut allowed_signers: Vec<ActorId> = vec![self.actor_id];
        allowed_signers.extend(self.prior_actor_ids.iter().copied());
        if linked {
            allowed_signers.extend(other.prior_actor_ids.iter().copied());
            // Under the same-actor arm `their.actor_id == our.actor_id` is
            // already the first entry, and under the our-prior arm it is
            // already in `self.prior_actor_ids` — so this licenses no signer
            // the older, narrower guard refused.
            allowed_signers.push(other.actor_id);
        }
        let mut grant_events = self.grant_events.clone();
        for e in &other.grant_events {
            if !grant_events.contains(e) && allowed_signers.iter().any(|id| e.verify(id).is_ok()) {
                grant_events.push(e.clone());
            }
        }
        sort_grant_events(&mut grant_events);

        let chain = our_chain.merge(&their_chain);

        // The adjudication planes merge as a **per-key union where the
        // [`verdict_precedence`] minimum survives**, because it is ONE ruling
        // (`succession-aftermath.md` § Re-key scope → *Adjudicating what the
        // aftermath carries across*) — two encodings of one rule would be
        // drift. Carrying the verdict at rest is what makes the union
        // simultaneously lossless and resurrection-free, so a *Keep* on the
        // laptop and a *Remove* on the phone both stick. Rows are then ordered
        // by their key, so each list is a function of its contents rather than
        // of who merged.
        let mut unattested_grant_marks = self.unattested_grant_marks.clone();
        for theirs in &other.unattested_grant_marks {
            match unattested_grant_marks.iter_mut().find(|ours| {
                ours.grant_id == theirs.grant_id && ours.predecessor == theirs.predecessor
            }) {
                Some(ours) => join_verdict(&mut ours.verdict, &theirs.verdict),
                None => unattested_grant_marks.push(theirs.clone()),
            }
        }
        sort_grant_marks(&mut unattested_grant_marks);

        // The member items, same rule, keyed on `(person, raising event,
        // reason)`.
        let mut unattested_member_items = self.unattested_member_items.clone();
        for theirs in &other.unattested_member_items {
            match unattested_member_items.iter_mut().find(|ours| {
                ours.person == theirs.person
                    && ours.predecessor == theirs.predecessor
                    && ours.reason == theirs.reason
            }) {
                Some(ours) => join_verdict(&mut ours.verdict, &theirs.verdict),
                None => unattested_member_items.push(theirs.clone()),
            }
        }
        sort_member_items(&mut unattested_member_items);

        // The filter marks — the FOURTH plane —
        // same rule, keyed on `(filter_id, raising event)`.
        //
        // ⚠ **No pruning arm, and its absence is the ruling rather than an
        // omission.** A filter row cannot merge at all — `email_filters` is
        // nest-side SQL under a single authority — so there is nothing here to
        // prune, and the removal itself happens nest-side through the deletion
        // the filter list already offers. This plane's `Removed` is
        // *recorded*, not enforced (`succession-aftermath.md` § Adjudicating
        // what the aftermath carries across, the 2026-08-14 paragraphs).
        let mut unattested_filter_marks = self.unattested_filter_marks.clone();
        for theirs in &other.unattested_filter_marks {
            match unattested_filter_marks.iter_mut().find(|ours| {
                ours.filter_id == theirs.filter_id && ours.predecessor == theirs.predecessor
            }) {
                Some(ours) => join_verdict(&mut ours.verdict, &theirs.verdict),
                None => unattested_filter_marks.push(theirs.clone()),
            }
        }
        sort_filter_marks(&mut unattested_filter_marks);

        Self {
            actor_id: chain.actor_id,
            prior_actor_ids: chain.prior_actor_ids,
            grant_events,
            unattested_grant_marks,
            unattested_member_items,
            unattested_filter_marks,
        }
    }

    /// Every person with at least one open review item, each with the reasons
    /// raised against them, in first-raised order.
    ///
    /// **This is the read accessor every unattested-member surface should
    /// use** — the permanent review view, the group member list's flag, and the
    /// contacts badge are all this one projection, which is what makes them one
    /// state rather than three. Rendering one row per *person* (not per item) is
    /// the ratified shape: two rows for one human reads as being asked the same
    /// question twice, when the user's decision is singular.
    pub fn open_member_reviews(&self) -> Vec<MemberReview> {
        let mut out: Vec<MemberReview> = Vec::new();
        for item in self
            .unattested_member_items
            .iter()
            .filter(|item| item.verdict.is_open())
        {
            match out.iter_mut().find(|r| r.person == item.person) {
                Some(review) => {
                    if !review.reasons.contains(&item.reason) {
                        review.reasons.push(item.reason.clone());
                    }
                }
                None => out.push(MemberReview {
                    person: item.person,
                    reasons: vec![item.reason.clone()],
                }),
            }
        }
        out
    }

    /// Whether `person` has an open review item — the group member list's and
    /// contacts list's per-row question.
    pub fn member_is_unattested(&self, person: &ActorId) -> bool {
        self.unattested_member_items
            .iter()
            .any(|item| &item.person == person && item.verdict.is_open())
    }

    /// Raise a review item for every person in `people`, attributed to the
    /// succession that retired `predecessor`.
    ///
    /// Idempotent per `(person, predecessor, reason)` **whatever the existing
    /// verdict** — so a resumed sweep, or the *"finish moving your groups"*
    /// retry, re-reports the same roster without re-asking a question the owner
    /// already answered. A *later* succession carries a different `predecessor`
    /// and therefore legitimately raises the person again, which is exactly the
    /// distinction `(person, raising event)` keying exists to draw.
    ///
    /// Returns **how many items were actually added**, so the caller can skip a
    /// plane put that would store identical rows — the same contract
    /// contract every raise on this value shares. A zero here is
    /// the ordinary steady state on every re-run, not a failure.
    pub fn raise_member_reviews(
        &mut self,
        people: impl IntoIterator<Item = ActorId>,
        predecessor: ActorId,
        reason: MemberUnattestedReason,
    ) -> usize {
        let mut raised = 0;
        for person in people {
            let already = self.unattested_member_items.iter().any(|item| {
                item.person == person && item.predecessor == predecessor && item.reason == reason
            });
            if !already {
                self.unattested_member_items.push(MemberUnattestedItem {
                    person,
                    predecessor,
                    reason: reason.clone(),
                    verdict: UnattestedVerdict::Open,
                });
                raised += 1;
            }
        }
        raised
    }

    /// Record `verdict` on every open item for `person` — what both *Keep* and
    /// *Remove* do.
    ///
    /// One gesture closes the person's whole backlog because the decision is
    /// about the human, not the event; a later succession raises fresh items and
    /// legitimately brings them back. Returns whether anything was open.
    pub fn decide_member_reviews_for(
        &mut self,
        person: &ActorId,
        verdict: UnattestedVerdict,
    ) -> bool {
        let mut changed = false;
        for item in self
            .unattested_member_items
            .iter_mut()
            .filter(|item| &item.person == person && item.verdict.is_open())
        {
            item.verdict = verdict.clone();
            changed = true;
        }
        changed
    }

    /// Whether this filter row is raised and still unadjudicated — the filter
    /// list's per-row question, and the filter twin of
    /// [`Self::member_is_unattested`].
    ///
    /// ⚠ Deliberately **not** delegating to a `row_is_raised`-shaped helper:
    /// this plane has no legacy row stamp for one to fall back on
    /// ([`FilterUnattestedMark`]), so the open-mark question *is* the whole
    /// question. An app holding only the rows and the marks calls
    /// [`FilterUnattestedMark::any_open`] directly.
    pub fn filter_is_unattested(&self, filter_id: i64) -> bool {
        FilterUnattestedMark::any_open(&self.unattested_filter_marks, filter_id)
    }

    /// Every filter row still awaiting the owner's verdict — what the aftermath
    /// review item counts and what the filter list's *"review these"* affordance
    /// walks. The filter twin of [`Self::open_member_reviews`], returning bare
    /// ids because the row itself is the unit of decision here (there is no
    /// per-person collapse to do).
    ///
    /// Ordered by id so the surface is a function of its contents rather than of
    /// the order marks happened to be raised or merged in.
    pub fn open_filter_reviews(&self) -> Vec<i64> {
        let mut out: Vec<i64> = self
            .unattested_filter_marks
            .iter()
            .filter(|m| m.verdict.is_open())
            .map(|m| m.filter_id)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Raise a mark for every filter id in `filter_ids`, attributed to the
    /// succession that retired `predecessor`.
    ///
    /// Idempotent per `(filter_id, predecessor)` **whatever the existing
    /// verdict** — so a re-run re-reports the same rows without re-asking a
    /// question the owner already answered. A *later* succession carries a
    /// different `predecessor` and therefore legitimately raises the row again,
    /// which is exactly the distinction `(filter_id, raising event)` keying
    /// exists to draw.
    ///
    /// ⚠ **The caller owes the classification, and it must be time-bounded.**
    /// `filter_ids` is the *inherited* set — rows created at or before the
    /// succession's recorded time — never "every filter I own right now". The
    /// two are indistinguishable in the successor's first session and diverge on
    /// every later re-run, once the successor has authored filters of their own;
    /// this key would then happily raise the successor's own rules against their
    /// own predecessor. `fauna_client_config::inherited_filter_ids` is the one
    /// classifier.
    ///
    /// Returns **how many marks were actually added**, so the caller can skip a
    /// plane put that would store identical rows — the same contract
    /// [`Self::raise_member_reviews`] states for the same reason. A zero here is
    /// the ordinary steady state on every re-run, not a failure.
    pub fn raise_filter_marks(
        &mut self,
        filter_ids: impl IntoIterator<Item = i64>,
        predecessor: ActorId,
    ) -> usize {
        let mut raised = 0;
        for filter_id in filter_ids {
            let already = self
                .unattested_filter_marks
                .iter()
                .any(|m| m.filter_id == filter_id && m.predecessor == predecessor);
            if !already {
                self.unattested_filter_marks.push(FilterUnattestedMark {
                    filter_id,
                    predecessor,
                    verdict: UnattestedVerdict::Open,
                });
                raised += 1;
            }
        }
        raised
    }

    /// Record `verdict` on every open mark for `filter_id` — what both *Keep*
    /// and *Remove* do.
    ///
    /// One gesture closes the row's whole backlog because the decision is about
    /// the rule, not the event; a later succession raises a fresh mark and
    /// legitimately brings it back. Returns whether anything was open.
    ///
    /// ⚠ *Remove* records the verdict here and **deletes the rule through the
    /// filter list's existing deletion**, nest-side — this plane's `Removed` is
    /// recorded, never enforced by the merge ([`FilterUnattestedMark`]). A
    /// caller that recorded `Removed` without issuing that deletion would leave
    /// an armed rule running under a clean-looking list.
    pub fn decide_filter_marks_for(&mut self, filter_id: i64, verdict: UnattestedVerdict) -> bool {
        let mut changed = false;
        for mark in self
            .unattested_filter_marks
            .iter_mut()
            .filter(|m| m.filter_id == filter_id && m.verdict.is_open())
        {
            mark.verdict = verdict.clone();
            changed = true;
        }
        changed
    }

    /// Record `verdict` on every open grant mark of `grant_id` — the Nests
    /// page's **Keep** on a carried-across grant. Closes *this* raising
    /// event's mark, never the grant forever: a later succession raises its
    /// own. Records the verdict and never deletes the mark (an answered mark
    /// must stay distinguishable from one never raised —
    /// [`UnattestedVerdict`]). Returns whether anything was open.
    pub fn decide_grant_marks_for(&mut self, grant_id: &[u8], verdict: UnattestedVerdict) -> bool {
        let mut changed = false;
        for mark in self
            .unattested_grant_marks
            .iter_mut()
            .filter(|m| m.grant_id == grant_id && m.verdict.is_open())
        {
            mark.verdict = verdict.clone();
            changed = true;
        }
        changed
    }

    /// The grant events alone, over a chain naming only `actor_id` — the
    /// replica a grant-log write merges (a `Mint`, `Renew` or `Revoke` the
    /// caller signed). Like [`Self::marks_replica`] it claims no chain link
    /// beyond the writer's own identity; the door re-verifies each event
    /// against the attested signer set.
    #[must_use]
    pub fn events_replica(actor_id: ActorId, events: Vec<GrantEvent>) -> Self {
        Self {
            grant_events: events,
            ..Self::empty(actor_id)
        }
    }

    /// The three adjudication planes alone, over a chain naming only this
    /// ledger's `actor_id` — the replica a mark write merges. It carries no
    /// grant event (the door re-verifies every event it is handed, and a mark
    /// write has none to add) and claims no chain link beyond the writer's
    /// own identity, which every stored chain already names.
    #[must_use]
    pub fn marks_replica(&self) -> Self {
        Self {
            unattested_grant_marks: self.unattested_grant_marks.clone(),
            unattested_member_items: self.unattested_member_items.clone(),
            unattested_filter_marks: self.unattested_filter_marks.clone(),
            ..Self::empty(self.actor_id)
        }
    }

    /// Every entity as its plane row, `(key, record)`: the `chain` row, then
    /// the events, then the three mark planes, each in its canonical order.
    ///
    /// # Errors
    /// An event or grant mark whose grant id is not [`GRANT_ID_LEN`] bytes,
    /// or an event whose signature is not [`GRANT_EVENT_SIGNATURE_LEN`] —
    /// neither has a key, and no minting site produces one.
    pub fn rows(&self) -> Result<Vec<(String, SuccessionLedgerRecord)>> {
        let records = std::iter::once(SuccessionLedgerRecord::Chain(self.chain()))
            .chain(
                self.grant_events
                    .iter()
                    .cloned()
                    .map(SuccessionLedgerRecord::Event),
            )
            .chain(
                self.unattested_grant_marks
                    .iter()
                    .cloned()
                    .map(SuccessionLedgerRecord::GrantMark),
            )
            .chain(
                self.unattested_member_items
                    .iter()
                    .cloned()
                    .map(SuccessionLedgerRecord::MemberItem),
            )
            .chain(
                self.unattested_filter_marks
                    .iter()
                    .cloned()
                    .map(SuccessionLedgerRecord::FilterMark),
            );
        records.map(|r| Ok((r.key()?.render(), r))).collect()
    }

    /// Fold one plane row in — the read side's per-row step, WITHOUT the
    /// allowed-signer clause (which needs the whole chain first:
    /// [`Self::fold`] runs it once every row is in). The `chain` row joins
    /// through [`ChainState::merge`], so a chain the gate refuses leaves this
    /// ledger's own chain standing.
    ///
    /// # Errors
    /// An unparseable key, an undecodable value, or a value not naming its
    /// key ([`decode_succession_ledger_row`]).
    pub fn fold_row(&mut self, key: &str, value: &[u8]) -> Result<()> {
        match decode_succession_ledger_row(key, value)? {
            SuccessionLedgerRecord::Chain(stored) => {
                let chain = self.chain().merge(&stored);
                self.actor_id = chain.actor_id;
                self.prior_actor_ids = chain.prior_actor_ids;
            }
            SuccessionLedgerRecord::Event(e) => {
                if !self.grant_events.contains(&e) {
                    self.grant_events.push(e);
                    sort_grant_events(&mut self.grant_events);
                }
            }
            SuccessionLedgerRecord::GrantMark(m) => {
                match self
                    .unattested_grant_marks
                    .iter_mut()
                    .find(|o| o.grant_id == m.grant_id && o.predecessor == m.predecessor)
                {
                    Some(o) => join_verdict(&mut o.verdict, &m.verdict),
                    None => {
                        self.unattested_grant_marks.push(m);
                        sort_grant_marks(&mut self.unattested_grant_marks);
                    }
                }
            }
            SuccessionLedgerRecord::MemberItem(m) => {
                match self.unattested_member_items.iter_mut().find(|o| {
                    o.person == m.person && o.predecessor == m.predecessor && o.reason == m.reason
                }) {
                    Some(o) => join_verdict(&mut o.verdict, &m.verdict),
                    None => {
                        self.unattested_member_items.push(m);
                        sort_member_items(&mut self.unattested_member_items);
                    }
                }
            }
            SuccessionLedgerRecord::FilterMark(m) => {
                match self
                    .unattested_filter_marks
                    .iter_mut()
                    .find(|o| o.filter_id == m.filter_id && o.predecessor == m.predecessor)
                {
                    Some(o) => join_verdict(&mut o.verdict, &m.verdict),
                    None => {
                        self.unattested_filter_marks.push(m);
                        sort_filter_marks(&mut self.unattested_filter_marks);
                    }
                }
            }
        }
        Ok(())
    }

    /// **The READ fold** over an account's `fauna.state.succession-ledger`
    /// rows, as the runtime holding `self_actor` sees them: the chain is
    /// seeded with `self_actor` and joined with the stored `chain` row, every
    /// row folds in, and then only the events verifying against the folded
    /// chain (`actor_id` ∪ `prior_actor_ids`) are kept — the set a merge over
    /// our chain plus a linked peer's admits. A stranger's event row
    /// is stored and invisible.
    ///
    /// # Errors
    /// Any row [`Self::fold_row`] refuses.
    pub fn fold<'a>(
        self_actor: ActorId,
        rows: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> Result<Self> {
        let mut ledger = Self::empty(self_actor);
        for (key, value) in rows {
            ledger.fold_row(key, value)?;
        }
        let signers: Vec<ActorId> = ledger.chain().members().copied().collect();
        ledger
            .grant_events
            .retain(|e| signers.iter().any(|id| e.verify(id).is_ok()));
        Ok(ledger)
    }
}

/// Canonical order of the grant log — not arrival order. Pure
/// representation: the fold that reads the log keys each grant on
/// `(at, sig)` rather than on position (`grant_event::latest_live_events`),
/// so nothing downstream observes the order — and representation is exactly
/// what two replicas compare.
fn sort_grant_events(events: &mut [GrantEvent]) {
    events.sort_by_cached_key(tiebreak_key);
}

fn sort_grant_marks(marks: &mut [GrantUnattestedMark]) {
    marks.sort_by(|a, b| {
        (a.grant_id.as_slice(), a.predecessor.0).cmp(&(b.grant_id.as_slice(), b.predecessor.0))
    });
}

fn sort_member_items(items: &mut [MemberUnattestedItem]) {
    items.sort_by_cached_key(|item| tiebreak_key(&(item.person, item.predecessor, &item.reason)));
}

fn sort_filter_marks(marks: &mut [FilterUnattestedMark]) {
    marks.sort_by_key(|m| (m.filter_id, m.predecessor.0));
}

// ── The plane rows' key grammar (`config-dissolution.md` § Phases and gates
// → *Bounded rows* → *The ledger*) ──

/// Key of the account's ONE owner-chain row.
pub const CHAIN_KEY: &str = "chain";

/// Key prefix of a grant-log row: `event/<grant id hex32>/<sig hex128>`.
pub const EVENT_KEY_PREFIX: &str = "event/";

/// Key prefix of a grant mark: `grant-mark/<grant id hex32>/<predecessor hex64>`.
pub const GRANT_MARK_KEY_PREFIX: &str = "grant-mark/";

/// Key prefix of a member item:
/// `member-item/<person hex64>/<predecessor hex64>/<reason wire string>` —
/// the reason is the key's tail, everything after the third `/`.
pub const MEMBER_ITEM_KEY_PREFIX: &str = "member-item/";

/// Key prefix of a filter mark:
/// `filter-mark/<filter id decimal i64>/<predecessor hex64>`.
pub const FILTER_MARK_KEY_PREFIX: &str = "filter-mark/";

/// A parsed `fauna.state.succession-ledger` row key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuccessionLedgerRowKey {
    /// `chain` — holds the one [`ChainState`].
    Chain,
    /// `event/<grant id>/<sig>` — holds one signed [`GrantEvent`].
    Event {
        grant_id: [u8; GRANT_ID_LEN],
        sig: [u8; GRANT_EVENT_SIGNATURE_LEN],
    },
    /// `grant-mark/<grant id>/<predecessor>` — one [`GrantUnattestedMark`].
    GrantMark {
        grant_id: [u8; GRANT_ID_LEN],
        predecessor: ActorId,
    },
    /// `member-item/<person>/<predecessor>/<reason>` — one
    /// [`MemberUnattestedItem`].
    MemberItem {
        person: ActorId,
        predecessor: ActorId,
        reason: MemberUnattestedReason,
    },
    /// `filter-mark/<filter id>/<predecessor>` — one [`FilterUnattestedMark`].
    FilterMark {
        filter_id: i64,
        predecessor: ActorId,
    },
}

/// Lowercase hex of `bytes`.
fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Exactly `N` bytes of strict lowercase hex.
fn parse_lower_hex<const N: usize>(segment: &str, what: &str) -> Result<[u8; N]> {
    let refuse = || {
        Error::Encoding(format!(
            "succession-ledger key segment {what} is not {N} bytes of lowercase hex: {segment:?}"
        ))
    };
    if segment.len() != N * 2
        || !segment
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(refuse());
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        // ASCII was checked above, so every two-char slice is on a boundary.
        *byte = u8::from_str_radix(&segment[2 * i..2 * i + 2], 16).map_err(|_| refuse())?;
    }
    Ok(out)
}

impl SuccessionLedgerRowKey {
    /// Parse a row key. Strict: only the canonical spelling (lowercase hex of
    /// the exact widths, a decimal filter id with no sign but `-`, no leading
    /// zero) parses, so one entity has one key.
    ///
    /// # Errors
    /// Any other string.
    pub fn parse(key: &str) -> Result<Self> {
        let segments = |rest: &str, n: usize| -> Result<Vec<String>> {
            let parts: Vec<&str> = rest.splitn(n, '/').collect();
            if parts.len() != n {
                return Err(Error::Encoding(format!(
                    "succession-ledger key has too few segments: {key:?}"
                )));
            }
            Ok(parts.into_iter().map(str::to_string).collect())
        };
        if key == CHAIN_KEY {
            return Ok(Self::Chain);
        }
        if let Some(rest) = key.strip_prefix(EVENT_KEY_PREFIX) {
            let s = segments(rest, 2)?;
            return Ok(Self::Event {
                grant_id: parse_lower_hex(&s[0], "grant id")?,
                sig: parse_lower_hex(&s[1], "signature")?,
            });
        }
        if let Some(rest) = key.strip_prefix(GRANT_MARK_KEY_PREFIX) {
            let s = segments(rest, 2)?;
            return Ok(Self::GrantMark {
                grant_id: parse_lower_hex(&s[0], "grant id")?,
                predecessor: ActorId(parse_lower_hex(&s[1], "predecessor")?),
            });
        }
        if let Some(rest) = key.strip_prefix(MEMBER_ITEM_KEY_PREFIX) {
            let s = segments(rest, 3)?;
            return Ok(Self::MemberItem {
                person: ActorId(parse_lower_hex(&s[0], "person")?),
                predecessor: ActorId(parse_lower_hex(&s[1], "predecessor")?),
                reason: MemberUnattestedReason::from(s[2].as_str()),
            });
        }
        if let Some(rest) = key.strip_prefix(FILTER_MARK_KEY_PREFIX) {
            let s = segments(rest, 2)?;
            let filter_id: i64 = s[0].parse().map_err(|_| {
                Error::Encoding(format!(
                    "succession-ledger filter id is not an i64: {key:?}"
                ))
            })?;
            if filter_id.to_string() != s[0] {
                return Err(Error::Encoding(format!(
                    "succession-ledger filter id is not canonical decimal: {key:?}"
                )));
            }
            return Ok(Self::FilterMark {
                filter_id,
                predecessor: ActorId(parse_lower_hex(&s[1], "predecessor")?),
            });
        }
        Err(Error::Encoding(format!(
            "not a succession-ledger key: {key:?}"
        )))
    }

    /// The canonical key string.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Chain => CHAIN_KEY.to_string(),
            Self::Event { grant_id, sig } => {
                format!(
                    "{EVENT_KEY_PREFIX}{}/{}",
                    lower_hex(grant_id),
                    lower_hex(sig)
                )
            }
            Self::GrantMark {
                grant_id,
                predecessor,
            } => format!(
                "{GRANT_MARK_KEY_PREFIX}{}/{}",
                lower_hex(grant_id),
                lower_hex(&predecessor.0)
            ),
            Self::MemberItem {
                person,
                predecessor,
                reason,
            } => format!(
                "{MEMBER_ITEM_KEY_PREFIX}{}/{}/{}",
                lower_hex(&person.0),
                lower_hex(&predecessor.0),
                reason.as_wire()
            ),
            Self::FilterMark {
                filter_id,
                predecessor,
            } => format!(
                "{FILTER_MARK_KEY_PREFIX}{filter_id}/{}",
                lower_hex(&predecessor.0)
            ),
        }
    }
}

/// One plane row's value: the record its key names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuccessionLedgerRecord {
    Chain(ChainState),
    Event(GrantEvent),
    GrantMark(GrantUnattestedMark),
    MemberItem(MemberUnattestedItem),
    FilterMark(FilterUnattestedMark),
}

/// A fixed-width copy of `bytes`, or the refusal naming `what`.
fn fixed<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N]> {
    bytes.try_into().map_err(|_| {
        Error::Encoding(format!(
            "succession-ledger {what} is {} bytes, not {N}",
            bytes.len()
        ))
    })
}

impl SuccessionLedgerRecord {
    /// The key this record is filed under.
    ///
    /// # Errors
    /// A grant id or signature of the wrong width (no minting site produces
    /// one; such a record has no key).
    pub fn key(&self) -> Result<SuccessionLedgerRowKey> {
        Ok(match self {
            Self::Chain(_) => SuccessionLedgerRowKey::Chain,
            Self::Event(e) => SuccessionLedgerRowKey::Event {
                grant_id: fixed(&e.grant_id, "grant id")?,
                sig: fixed(&e.sig, "grant event signature")?,
            },
            Self::GrantMark(m) => SuccessionLedgerRowKey::GrantMark {
                grant_id: fixed(&m.grant_id, "grant id")?,
                predecessor: m.predecessor,
            },
            Self::MemberItem(m) => SuccessionLedgerRowKey::MemberItem {
                person: m.person,
                predecessor: m.predecessor,
                reason: m.reason.clone(),
            },
            Self::FilterMark(m) => SuccessionLedgerRowKey::FilterMark {
                filter_id: m.filter_id,
                predecessor: m.predecessor,
            },
        })
    }

    /// The canonical value bytes.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Chain(r) => canonical_encode(r),
            Self::Event(r) => canonical_encode(r),
            Self::GrantMark(r) => canonical_encode(r),
            Self::MemberItem(r) => canonical_encode(r),
            Self::FilterMark(r) => canonical_encode(r),
        }
    }

    /// **The per-row arms** — the halves [`SuccessionLedger::merge`] runs:
    /// the `chain` row is [`ChainState::join`] (the gated union and the
    /// successor pick; a fork refused); an event row is write-once (equal
    /// values join to themselves; Ed25519 is deterministic over the content,
    /// so two honest writers never differ and a differing value under one key
    /// is refused); a mark row is the [`verdict_precedence`] minimum.
    ///
    /// # Errors
    /// Different record types or keys, a refused chain pair, or two differing
    /// events under one key.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        let mismatch = || Error::Encoding("succession-ledger records name different rows".into());
        Ok(match (self, other) {
            (Self::Chain(a), Self::Chain(b)) => Self::Chain(a.join(b)?),
            (Self::Event(a), Self::Event(b)) => {
                if a != b {
                    return Err(Error::Encoding(
                        "a succession-ledger event row is write-once; two differing values \
                         share one key"
                            .into(),
                    ));
                }
                Self::Event(a.clone())
            }
            (Self::GrantMark(a), Self::GrantMark(b))
                if a.grant_id == b.grant_id && a.predecessor == b.predecessor =>
            {
                let mut m = a.clone();
                join_verdict(&mut m.verdict, &b.verdict);
                Self::GrantMark(m)
            }
            (Self::MemberItem(a), Self::MemberItem(b))
                if a.person == b.person
                    && a.predecessor == b.predecessor
                    && a.reason == b.reason =>
            {
                let mut m = a.clone();
                join_verdict(&mut m.verdict, &b.verdict);
                Self::MemberItem(m)
            }
            (Self::FilterMark(a), Self::FilterMark(b))
                if a.filter_id == b.filter_id && a.predecessor == b.predecessor =>
            {
                let mut m = a.clone();
                join_verdict(&mut m.verdict, &b.verdict);
                Self::FilterMark(m)
            }
            _ => return Err(mismatch()),
        })
    }
}

/// Decode `value` as `T`, refusing anything that does not re-encode to the
/// same bytes — the plane's strict posture over the four value types whose
/// serde stays tolerant (module docs).
fn decode_exact<T: Serialize + serde::de::DeserializeOwned>(value: &[u8], what: &str) -> Result<T> {
    let decoded: T = canonical_decode(value)?;
    if canonical_encode(&decoded)?.as_slice() != value {
        return Err(Error::Encoding(format!(
            "succession-ledger {what} row does not re-encode to its own bytes \
             (an unknown field, or a non-canonical encoding)"
        )));
    }
    Ok(decoded)
}

/// Decode one `fauna.state.succession-ledger` row: the key's first segment
/// picks the record type, the value must decode as it and re-encode to the
/// same bytes (a newer build's field is refused, never stripped), and the
/// value must name its key — grant id + signature for an event, grant id +
/// predecessor, person + predecessor + reason, filter id + predecessor for
/// the marks. A `chain` value must hold its prior ids sorted, each once, and
/// never its own identity.
///
/// # Errors
/// An unparseable key, an undecodable or non-canonical value, a value not
/// naming its key, or a malformed chain.
pub fn decode_succession_ledger_row(key: &str, value: &[u8]) -> Result<SuccessionLedgerRecord> {
    let parsed = SuccessionLedgerRowKey::parse(key)?;
    let record = match parsed {
        SuccessionLedgerRowKey::Chain => {
            let chain: ChainState = decode_exact(value, "chain")?;
            let sorted_once = chain.prior_actor_ids.windows(2).all(|w| w[0].0 < w[1].0);
            if !sorted_once || chain.prior_actor_ids.contains(&chain.actor_id) {
                return Err(Error::Encoding(
                    "succession-ledger chain row is malformed (prior ids unsorted, repeated, \
                     or naming the current identity)"
                        .into(),
                ));
            }
            SuccessionLedgerRecord::Chain(chain)
        }
        SuccessionLedgerRowKey::Event { .. } => {
            SuccessionLedgerRecord::Event(decode_exact(value, "event")?)
        }
        SuccessionLedgerRowKey::GrantMark { .. } => {
            SuccessionLedgerRecord::GrantMark(decode_exact(value, "grant-mark")?)
        }
        SuccessionLedgerRowKey::MemberItem { .. } => {
            SuccessionLedgerRecord::MemberItem(decode_exact(value, "member-item")?)
        }
        SuccessionLedgerRowKey::FilterMark { .. } => {
            SuccessionLedgerRecord::FilterMark(decode_exact(value, "filter-mark")?)
        }
    };
    if record.key()? != parsed {
        return Err(Error::Encoding(format!(
            "succession-ledger row at {key:?} holds the record for {:?}",
            record.key()?.render()
        )));
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant_event::GrantEventKind;
    use crate::identity::ActorKeypair;

    fn kp(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    fn mint(kp: &ActorKeypair, grant: u8, at: u64) -> GrantEvent {
        GrantEvent {
            grant_id: vec![grant; GRANT_ID_LEN],
            holder: vec![0xAA; 32],
            kind: GrantEventKind::Mint,
            scope: vec![],
            window_start: at,
            window_end: at + 1000,
            at,
            sig: vec![0u8; GRANT_EVENT_SIGNATURE_LEN],
        }
        .sign(kp.signing_key())
        .unwrap()
    }

    fn ledger_with_every_family(owner: &ActorKeypair, pred: ActorId) -> SuccessionLedger {
        SuccessionLedger {
            actor_id: owner.actor_id(),
            prior_actor_ids: vec![pred],
            grant_events: vec![mint(owner, 1, 10)],
            unattested_grant_marks: vec![GrantUnattestedMark {
                grant_id: vec![1; GRANT_ID_LEN],
                predecessor: pred,
                verdict: UnattestedVerdict::Open,
            }],
            unattested_member_items: vec![MemberUnattestedItem {
                person: ActorId([7; 32]),
                predecessor: pred,
                reason: MemberUnattestedReason::Other("a/reason/with/slashes".into()),
                verdict: UnattestedVerdict::Kept,
            }],
            unattested_filter_marks: vec![FilterUnattestedMark {
                filter_id: -42,
                predecessor: pred,
                verdict: UnattestedVerdict::Other("future".into()),
            }],
        }
    }

    /// Every family renders a key that parses back to itself, and the rows
    /// fold back into the ledger they came from.
    #[test]
    fn succession_ledger_rows_round_trip_through_their_keys() {
        let owner = kp(1);
        let ledger = ledger_with_every_family(&owner, ActorId([2; 32]));
        let rows = ledger.rows().unwrap();
        assert_eq!(rows.len(), 5, "one row per entity, one chain row");
        let mut encoded = Vec::new();
        for (key, record) in &rows {
            assert_eq!(SuccessionLedgerRowKey::parse(key).unwrap().render(), *key);
            let bytes = record.encode().unwrap();
            assert_eq!(decode_succession_ledger_row(key, &bytes).unwrap(), *record);
            encoded.push((key.clone(), bytes));
        }
        let folded = SuccessionLedger::fold(
            owner.actor_id(),
            encoded.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        )
        .unwrap();
        assert_eq!(folded, ledger);
    }

    /// The grammar is strict: one entity has one key.
    #[test]
    fn succession_ledger_keys_outside_the_grammar_are_refused() {
        let pred = "ab".repeat(32);
        for bad in [
            "chain/",
            "Chain",
            "event/00",
            &format!("event/{}/{}", "0".repeat(32), "0".repeat(127)),
            &format!("grant-mark/{}/{}", "0".repeat(32), pred.to_uppercase()),
            &format!("grant-mark/{}/{pred}/extra", "0".repeat(32)),
            &format!("filter-mark/007/{pred}"),
            &format!("filter-mark/+7/{pred}"),
            &format!("filter-mark/-0/{pred}"),
            &format!("member-item/{pred}/{pred}"),
            "self",
        ] {
            assert!(
                SuccessionLedgerRowKey::parse(bad).is_err(),
                "{bad:?} must be refused"
            );
        }
        // The member item's reason is the whole tail, slashes included.
        let key = format!("member-item/{pred}/{pred}/a/b");
        assert_eq!(
            SuccessionLedgerRowKey::parse(&key).unwrap(),
            SuccessionLedgerRowKey::MemberItem {
                person: ActorId([0xAB; 32]),
                predecessor: ActorId([0xAB; 32]),
                reason: MemberUnattestedReason::Other("a/b".into()),
            }
        );
    }

    /// The read fold's signer clause: an event signed by an identity outside
    /// the folded chain is stored and invisible; the same event becomes
    /// visible once the chain names its signer.
    #[test]
    fn the_read_fold_keeps_only_events_the_folded_chain_signed() {
        let (me, pred, stranger) = (kp(1), kp(2), kp(3));
        let rec = |r: SuccessionLedgerRecord| (r.key().unwrap().render(), r.encode().unwrap());
        let rows = [
            rec(SuccessionLedgerRecord::Event(mint(&me, 1, 10))),
            rec(SuccessionLedgerRecord::Event(mint(&pred, 2, 11))),
            rec(SuccessionLedgerRecord::Event(mint(&stranger, 3, 12))),
        ];
        let fold = |extra: Option<(String, Vec<u8>)>| {
            let all: Vec<_> = rows.iter().cloned().chain(extra).collect();
            SuccessionLedger::fold(
                me.actor_id(),
                all.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
            )
            .unwrap()
        };
        let bare = fold(None);
        assert_eq!(bare.grant_events, vec![mint(&me, 1, 10)]);
        let linked = fold(Some(rec(SuccessionLedgerRecord::Chain(ChainState {
            actor_id: me.actor_id(),
            prior_actor_ids: vec![pred.actor_id()],
        }))));
        assert_eq!(
            linked.grant_events.len(),
            2,
            "the predecessor's event is ours"
        );
        assert!(!linked.grant_events.contains(&mint(&stranger, 3, 12)));
        // A stranger's chain row does not license its signer: the gate
        // refuses it and the seeded chain stands.
        let forged = fold(Some(rec(SuccessionLedgerRecord::Chain(ChainState {
            actor_id: stranger.actor_id(),
            prior_actor_ids: vec![],
        }))));
        assert_eq!(forged.actor_id, me.actor_id());
        assert_eq!(forged.grant_events, vec![mint(&me, 1, 10)]);
    }

    /// The chain arm: the gate's arms join, a fork and a mutual pair are
    /// refused, and the successor's id survives either merge direction.
    #[test]
    fn the_chain_arm_joins_one_owner_chain_and_refuses_a_fork() {
        let (p, s, t) = (ActorId([1; 32]), ActorId([2; 32]), ActorId([3; 32]));
        let pred_row = ChainState::of(p);
        let mut succ = ChainState::of(s);
        succ.repoint_to_successor(p, s);
        assert_eq!(succ.prior_actor_ids, vec![p]);
        assert_eq!(pred_row.join(&succ).unwrap().actor_id, s);
        assert_eq!(succ.join(&pred_row).unwrap().actor_id, s);
        // A fork: two successors of one predecessor.
        let mut thief = ChainState::of(t);
        thief.repoint_to_successor(p, t);
        assert!(succ.join(&thief).is_err());
        // The plain merge keeps ours where the plane arm refuses.
        assert_eq!(succ.merge(&thief), succ);
        // A mutual pair names each identity retired by the other.
        let a = ChainState {
            actor_id: s,
            prior_actor_ids: vec![t],
        };
        let b = ChainState {
            actor_id: t,
            prior_actor_ids: vec![s],
        };
        assert!(a.join(&b).is_err());
        // Re-pointing onto itself records nothing.
        let mut same = ChainState::of(s);
        same.repoint_to_successor(s, s);
        assert_eq!(same, ChainState::of(s));
    }

    /// A value must name its key, and a chain must be in normal form.
    #[test]
    fn a_row_not_naming_its_key_or_a_malformed_chain_is_refused() {
        let owner = kp(1);
        let e = SuccessionLedgerRecord::Event(mint(&owner, 1, 10));
        let other_key = SuccessionLedgerRecord::Event(mint(&owner, 2, 10))
            .key()
            .unwrap()
            .render();
        assert!(decode_succession_ledger_row(&other_key, &e.encode().unwrap()).is_err());
        let (p, s) = (ActorId([1; 32]), ActorId([2; 32]));
        for bad in [
            ChainState {
                actor_id: s,
                prior_actor_ids: vec![s],
            },
            ChainState {
                actor_id: s,
                prior_actor_ids: vec![ActorId([9; 32]), p],
            },
            ChainState {
                actor_id: s,
                prior_actor_ids: vec![p, p],
            },
        ] {
            let bytes = SuccessionLedgerRecord::Chain(bad).encode().unwrap();
            assert!(decode_succession_ledger_row(CHAIN_KEY, &bytes).is_err());
        }
    }
}
