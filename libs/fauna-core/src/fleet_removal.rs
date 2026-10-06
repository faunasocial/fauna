//! Which fleet member a devices-page removal excludes — resolved from
//! **client-held truth**, never from the nest's word.
//!
//! Authority: `docs/goal/architecture/account-data-taxonomy.md` § The
//! generation machinery → *Fleet-scope reclamation*, clause (4) (*The removal
//! target*); `docs/goal/behavior/devices.md` § Removing a Device.
//!
//! The devices page lists `fauna.sync.devices.list` rows, and every field on
//! such a row — its `principal` included — is the nest's to write. A `Removed`
//! device-set row is absorbing and excludes its id unconditionally, so a
//! removal that trusted the row's principal let a hostile nest choose which
//! device a removal permanently excludes (a live sibling, the caller itself)
//! **and** which one it spares (the stolen laptop the user meant, which then
//! stays a wrap target for every later generation). The nest cannot seal a
//! fleet-plane row, so the binding is one: each device states the nest row it
//! enrolled on in its own generation-sealed
//! [`DeviceEndpointsEntry::enrolled_row`](crate::device_endpoints::DeviceEndpointsEntry::enrolled_row),
//! and [`resolve_removal_targets`] reads the target off those statements.
//!
//! Pure over [`RemovalFacts`] — no store, no clock — so the rule is pinned
//! here once and the account runtime only gathers the facts.

use std::collections::{BTreeMap, BTreeSet};

/// Why a removal gesture is refused **before** anything is deleted — so the
/// user is told the device was not removed, rather than shown a nest row gone
/// while the fleet member it named stays enrolled.
///
/// The serde derives carry it across web's account port (the Devices page's
/// fleet seam, `account-client-lifecycle.md` § *The account port*); it is
/// not a wire or at-rest type, so its shape carries no compatibility duty
/// (decision (g)).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FleetRemovalRefusal {
    /// The target resolves to the removing device itself. Leaving the fleet is
    /// sign-out's path (clause (4)'s first half), never a roster gesture — and
    /// it is exactly what a hostile nest would aim a removal at.
    OwnDevice,
    /// The nest names a principal this replica's verified fleet view does not
    /// hold as a member (unknown, unverifiable, or not 32 bytes).
    NotAMember,
    /// The nest names a verified member that states a *different* nest row as
    /// its own. Either the nest re-paired the row and the principal, or the
    /// member mis-states its row — the rule cannot tell which, so it trusts
    /// neither and no retry clears it: the way through is the member-addressed
    /// door ([`resolve_member_removal`]), where the user settles it.
    RowMismatch,
    /// The facts could not be read (no account runtime up, a store error).
    /// Refused rather than skipped: deleting the nest row now would leave no
    /// row to retry the fleet leg from.
    Unavailable(String),
}

/// Everything [`resolve_removal_targets`] decides from — all of it held by
/// the client: the verified fleet view's two sets, the members' own
/// row statements, and this device's own identity and enrolled row.
#[derive(Debug, Clone, Default)]
pub struct RemovalFacts {
    /// This device's own fleet id (its device principal's public key).
    pub me: [u8; 32],
    /// The nest row this device itself enrolled on (the registration latch's
    /// row half), when it has enrolled.
    pub own_row: Option<String>,
    /// Verified, non-removed fleet members (`FleetView::wrap_targets`).
    pub members: BTreeSet<[u8; 32]>,
    /// Ids a `Removed` row already excludes (`FleetView::removed`).
    pub removed: BTreeSet<[u8; 32]>,
    /// Each device's own statement of the nest row it enrolled on, keyed by
    /// the stating device's fleet id.
    pub bindings: BTreeMap<[u8; 32], String>,
}

/// Row ids are hex strings the nest and the devices both write; compare them
/// the way every other row comparison on this page does — as the same id
/// whatever its case.
fn same_row(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The fleet ids a removal of nest row `row` must write `Removed` for.
///
/// `claimed` is the principal the nest's row carries — consulted only when no
/// member states `row` as its own, and then only as a *claim* to check. An
/// empty `Ok` is the honest "this row names no fleet member" (a web-only row, or a member
/// already removed): the nest deletion proceeds
/// alone.
///
/// 1. `row` is this device's own enrolled row → [`FleetRemovalRefusal::OwnDevice`].
/// 2. Some verified member states `row` → **those members are the target**,
///    whatever the nest claims (several only when one machine re-minted its
///    principal: every principal that enrolled on the row is that device).
///    This device among them → `OwnDevice`.
/// 3. Otherwise the nest's claim is checked: itself → `OwnDevice`; already
///    removed → nothing to write; not a verified member → `NotAMember`; a
///    member stating a different row → `RowMismatch`; a member that has stated
///    no row yet (its enrollment legs have not run) → accepted.
pub fn resolve_removal_targets(
    facts: &RemovalFacts,
    row: &str,
    claimed: Option<&[u8; 32]>,
) -> Result<Vec<[u8; 32]>, FleetRemovalRefusal> {
    if facts
        .own_row
        .as_deref()
        .is_some_and(|own| same_row(own, row))
    {
        return Err(FleetRemovalRefusal::OwnDevice);
    }
    let bound: Vec<[u8; 32]> = facts
        .bindings
        .iter()
        .filter(|(id, stated)| facts.members.contains(*id) && same_row(stated, row))
        .map(|(id, _)| *id)
        .collect();
    if bound.contains(&facts.me) {
        return Err(FleetRemovalRefusal::OwnDevice);
    }
    if !bound.is_empty() {
        return Ok(bound);
    }
    let Some(claimed) = claimed else {
        return Ok(Vec::new());
    };
    if *claimed == facts.me {
        return Err(FleetRemovalRefusal::OwnDevice);
    }
    if facts.removed.contains(claimed) {
        return Ok(Vec::new());
    }
    if !facts.members.contains(claimed) {
        return Err(FleetRemovalRefusal::NotAMember);
    }
    if facts.bindings.contains_key(claimed) {
        return Err(FleetRemovalRefusal::RowMismatch);
    }
    Ok(vec![*claimed])
}

/// The fleet ids a **member-addressed** removal of `member` must write
/// `Removed` for — the second door of clause (4) (*A disagreement is the
/// user's to settle*). The user picked `member` by its fleet id, so this door
/// takes no nest input (a hostile nest cannot aim it) and reads no row
/// statement (the member cannot veto it): this device → `OwnDevice`; already
/// removed → nothing to write; not a verified member → `NotAMember`.
pub fn resolve_member_removal(
    facts: &RemovalFacts,
    member: &[u8; 32],
) -> Result<Vec<[u8; 32]>, FleetRemovalRefusal> {
    if *member == facts.me {
        return Err(FleetRemovalRefusal::OwnDevice);
    }
    if facts.removed.contains(member) {
        return Ok(Vec::new());
    }
    if !facts.members.contains(member) {
        return Err(FleetRemovalRefusal::NotAMember);
    }
    Ok(vec![*member])
}

/// One fleet member the page offers the member-addressed door for — what a
/// card can show, and all of it: the member's fleet id and the enrollment
/// instant its own enrollment record asserts (the member's self-signed word —
/// a hint, never proof). No label: the label is the nest row's, and a member
/// listed here has no row the client trusts.
///
/// The serde derives carry it across web's account port (the Devices page's
/// fleet seam, `account-client-lifecycle.md` § *The account port*); it is
/// not a wire or at-rest type, so its shape carries no compatibility duty
/// (decision (g)).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnaccountedMember {
    #[serde(with = "serde_bytes")]
    pub device_id: [u8; 32],
    pub enrolled_at_ms: i64,
}

/// The Devices page's read of the member door: this device's own fleet id —
/// what `device-own-fingerprint` renders, the user's half of the elimination
/// — and every verified member no roster row accounts for
/// ([`unaccounted_members`]), each with what its card shows.
///
/// The serde derives carry it across web's account port (the Devices page's
/// fleet seam, `account-client-lifecycle.md` § *The account port*); it is
/// not a wire or at-rest type, so its shape carries no compatibility duty
/// (decision (g)).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FleetMembersView {
    #[serde(with = "serde_bytes")]
    pub me: [u8; 32],
    pub unaccounted: Vec<UnaccountedMember>,
}

/// The verified members — other than this device — that **no roster row
/// accounts for**: the ones the page must offer the member-addressed door
/// for, because no row gesture removes exactly them. `roster` is every nest
/// row the page lists, as `(row id, claimed principal)`.
///
/// A row accounts for a member only when removing it resolves
/// ([`resolve_removal_targets`]) to **that member alone**. So a member is
/// listed when its row was deleted elsewhere (web), when the nest stripped or
/// re-aimed its claim, and when its own statement keeps every row from
/// resolving to it — a fabricated row, a sibling's row (that row then
/// resolves to two members, so the sibling is listed beside it), or the
/// caller's own. No statement a member can publish takes it off this list
/// while keeping it irremovable.
#[must_use]
pub fn unaccounted_members(
    facts: &RemovalFacts,
    roster: &[(String, Option<[u8; 32]>)],
) -> Vec<[u8; 32]> {
    let accounted: BTreeSet<[u8; 32]> = roster
        .iter()
        .filter_map(
            |(row, claimed)| match resolve_removal_targets(facts, row, claimed.as_ref()) {
                Ok(targets) if targets.len() == 1 => Some(targets[0]),
                _ => None,
            },
        )
        .collect();
    facts
        .members
        .iter()
        .filter(|id| **id != facts.me && !accounted.contains(*id))
        .copied()
        .collect()
}

/// One removal the user asked for whose fleet leg is not known finished — the
/// **durable intent** that makes the two-leg removal crash-safe (clause (4),
/// *The completion rule*). Staged before `fauna.sync.devices.delete` with the
/// ids [`resolve_removal_targets`] answered — never a principal the nest hands
/// back later — and cleared only once every `Removed` row is journaled, once
/// the nest has definitively refused the deletion ([`NestDeletion::Kept`]), or
/// once the reconcile has seen the row outlast any flight of its deletion
/// ([`reconcile_verdict`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFleetRemoval {
    /// The nest `sync_devices` row the user removed.
    pub row: String,
    /// The client-verified fleet ids that row stood for.
    pub targets: Vec<[u8; 32]>,
}

/// A [`PendingFleetRemoval`] as the credential slot holds it: the user's ask,
/// plus the one fact the reconcile keeps about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFleetRemoval {
    pub removal: PendingFleetRemoval,
    /// When (wall-clock ms) a reconcile pass first found the row still on the
    /// nest's roster — `None` until one has. Written only by the reconcile
    /// ([`note_row_present`]); every (re)stage resets it, since a re-staged
    /// removal is a new deletion about to fly.
    pub present_since_ms: Option<u64>,
}

/// What the nest's deletion of the row came to, as the completion rule reads
/// it. The nest delete is the transition's **single decision point**.
///
/// The serde derives carry it across web's account port (the Devices page's
/// fleet seam, `account-client-lifecycle.md` § *The account port*); it is
/// not a wire or at-rest type, so its shape carries no compatibility duty
/// (decision (g)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NestDeletion {
    /// The row is gone: the deletion succeeded, or the nest no longer holds
    /// the row at all (`not_found` — the deletion's own postcondition, however
    /// it came about). The `Removed` rows MUST land.
    Gone,
    /// The nest definitively refused and kept the row, and the page told the
    /// user why — the refusals the page's error mapping names:
    /// `guardian_marked` (a guardian enrolled it), `conflict`, and a malformed
    /// request. Nothing may be
    /// written — `Removed` is absorbing, and the user was told the device
    /// stays.
    Kept,
    /// Nobody knows — a transport failure, a reply lost after the nest acted,
    /// a refusal the page's error mapping does not name: the intent stays
    /// staged and the reconcile's roster reads decide ([`reconcile_verdict`]).
    Unknown,
}

/// How long the reconcile waits, from the first pass that finds a staged
/// removal's row still on the roster, before it reads "still there" as "the
/// deletion never happened". **A pass cannot tell a deletion that never
/// happened from one still in flight**: the page stages, and while its
/// `fauna.sync.devices.delete` is on the wire passes run beside it — the
/// runtime serves the page's commands one at a time and passes between them,
/// and on a desktop the co-located agent's pump passes on its own clock. So a
/// present row is evidence only once no deletion can still land. An hour is
/// hundreds of times the deletion's own deadline (the kind registry's 5 s),
/// and it bounds what a never-happened intent costs: one roster read per full
/// pass, ending within the hour.
pub const DELETION_IN_FLIGHT_BOUND_MS: u64 = 60 * 60 * 1000;

/// What one reconcile pass does with one staged removal whose roster it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileVerdict {
    /// The row is gone — the deletion happened, however it came about
    /// ([`NestDeletion::Gone`]): write every `Removed` row, then clear.
    Complete,
    /// The row is still present and a deletion may still be in flight: leave
    /// the intent staged. `stamp` = record this pass's sighting at that time
    /// ([`note_row_present`]); `None` = nothing to record.
    Wait { stamp: Option<u64> },
    /// The row has stayed present across [`DELETION_IN_FLIGHT_BOUND_MS`], so no
    /// deletion is in flight and none happened: clear the intent unwritten
    /// ([`drop_if_unchanged`], on `since` — the sighting this verdict rests
    /// on) — the user's row is there to retry from.
    Drop { since: u64 },
}

/// The completion rule's reconcile half, pure. `row_held` = the roster this
/// pass read still lists the row; `now_ms` = this pass's wall clock (`0` when
/// unreadable). A drop needs two sightings of the row at least
/// [`DELETION_IN_FLIGHT_BOUND_MS`] apart, both after the latest stage — never
/// one pass's word. An unreadable clock records nothing (a zero stamp would
/// read as ancient), and a stamp from the future (the clock moved back) is
/// re-taken now rather than trusted: both only ever lengthen the wait.
#[must_use]
pub fn reconcile_verdict(
    row_held: bool,
    present_since_ms: Option<u64>,
    now_ms: u64,
) -> ReconcileVerdict {
    if !row_held {
        return ReconcileVerdict::Complete;
    }
    match present_since_ms {
        Some(since) if since <= now_ms => {
            if now_ms - since >= DELETION_IN_FLIGHT_BOUND_MS {
                ReconcileVerdict::Drop { since }
            } else {
                ReconcileVerdict::Wait { stamp: None }
            }
        }
        _ => ReconcileVerdict::Wait {
            stamp: (now_ms != 0).then_some(now_ms),
        },
    }
}

/// Stage `removal`, replacing any earlier intent for the same row — and with
/// it the reconcile's sighting ([`StagedFleetRemoval::present_since_ms`]).
pub fn stage_pending(pending: &mut Vec<StagedFleetRemoval>, removal: PendingFleetRemoval) {
    let row = removal.row.clone();
    crate::keyed_staging::stage(
        pending,
        StagedFleetRemoval {
            removal,
            present_since_ms: None,
        },
        |r| same_row(&r.removal.row, &row),
    );
}

/// Clear the intent for `row`. `false` = nothing was staged for it.
pub fn clear_pending(pending: &mut Vec<StagedFleetRemoval>, row: &str) -> bool {
    crate::keyed_staging::clear(pending, |r| same_row(&r.removal.row, row))
}

/// Record a [`ReconcileVerdict::Wait`]'s sighting: stamp `row`'s intent with
/// `at_ms`, but only while it still carries `judged` — the sighting the
/// verdict was reached on. A removal re-staged since then is a new flight and
/// keeps its own clock. `false` = nothing changed.
pub fn note_row_present(
    pending: &mut [StagedFleetRemoval],
    row: &str,
    judged: Option<u64>,
    at_ms: u64,
) -> bool {
    match pending
        .iter_mut()
        .find(|r| same_row(&r.removal.row, row) && r.present_since_ms == judged)
    {
        Some(staged) => {
            staged.present_since_ms = Some(at_ms);
            true
        }
        None => false,
    }
}

/// Carry out a [`ReconcileVerdict::Drop`]: clear `row`'s intent only while it
/// still carries the sighting `since` the verdict was reached on — a removal
/// re-staged in the meantime (a retry whose deletion may be in flight right
/// now) is not the one judged. `false` = nothing cleared.
pub fn drop_if_unchanged(pending: &mut Vec<StagedFleetRemoval>, row: &str, since: u64) -> bool {
    crate::keyed_staging::clear(pending, |r| {
        same_row(&r.removal.row, row) && r.present_since_ms == Some(since)
    })
}

/// Splits a sighting group's row from its time. A group carrying no `:` is
/// never an intent, which is what keeps a sighting invisible to a reader that
/// predates it ([`encode_pending`]).
const SIGHTING_SEPARATOR: char = '@';

/// The slot spelling of the staged intents, `;`-joined: one `row:id,id` group
/// per intent (every id 32-byte hex), followed — for an intent a pass has
/// sighted — by a `row@ms` group. Plain text because the slot is a string
/// store; rows, ids and times are public values, never key material.
///
/// **The sighting is a group of its own so the spelling stays additive**
/// (`version-compatibility.md`): a pre-sighting reader parses `row:ids`
/// groups and drops any other group alone, so it reads every intent unchanged;
/// a slot it rewrites merely loses sightings, which the next pass re-takes (a
/// later drop, never an earlier one). This reader parses a pre-sighting slot
/// as intents no pass has sighted yet.
#[must_use]
pub fn encode_pending(pending: &[StagedFleetRemoval]) -> String {
    let mut groups = Vec::new();
    for staged in pending {
        let r = &staged.removal;
        let ids: Vec<String> = r.targets.iter().map(crate::hex32::encode).collect();
        groups.push(format!("{}:{}", r.row, ids.join(",")));
        if let Some(since) = staged.present_since_ms {
            groups.push(format!("{}{SIGHTING_SEPARATOR}{since}", r.row));
        }
    }
    groups.join(";")
}

/// Parse [`encode_pending`]'s spelling. A group that does not parse is
/// dropped alone (an unreadable intent cannot be completed, and must not take
/// the readable ones with it); so is a sighting naming no staged intent, or a
/// zero time (never written — it would read as ancient).
#[must_use]
pub fn decode_pending(stored: &str) -> Vec<StagedFleetRemoval> {
    let groups = || stored.split(';').filter(|g| !g.is_empty());
    let mut staged: Vec<StagedFleetRemoval> = groups()
        .filter_map(|group| {
            let (row, ids) = group.split_once(':')?;
            let targets = ids
                .split(',')
                .filter(|i| !i.is_empty())
                .map(|i| crate::hex32::decode(i).ok())
                .collect::<Option<Vec<_>>>()?;
            (!row.is_empty()).then(|| StagedFleetRemoval {
                removal: PendingFleetRemoval {
                    row: row.to_string(),
                    targets,
                },
                present_since_ms: None,
            })
        })
        .collect();
    for group in groups().filter(|g| !g.contains(':')) {
        let Some((row, since)) = group.rsplit_once(SIGHTING_SEPARATOR) else {
            continue;
        };
        let Some(since) = since.parse::<u64>().ok().filter(|s| *s != 0) else {
            continue;
        };
        if let Some(intent) = staged
            .iter_mut()
            .find(|s| same_row(&s.removal.row, row) && s.present_since_ms.is_none())
        {
            intent.present_since_ms = Some(since);
        }
    }
    staged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_intents_round_trip_and_upsert_by_row() {
        let mut pending = Vec::new();
        stage_pending(&mut pending, removal("bb", vec![LAPTOP]));
        stage_pending(&mut pending, removal("cc", vec![]));
        assert!(note_row_present(&mut pending, &row("bb"), None, T0));
        assert_eq!(decode_pending(&encode_pending(&pending)), pending);
        // Re-staging a row replaces its intent (a retried gesture re-resolves)
        // and, being a new flight, its sighting.
        stage_pending(&mut pending, removal("BB", vec![LAPTOP, PHONE]));
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|s| s.present_since_ms.is_none()));
        let back = decode_pending(&encode_pending(&pending));
        assert_eq!(back, pending);

        assert!(clear_pending(&mut pending, &row("bb")));
        assert!(!clear_pending(&mut pending, &row("bb")), "already cleared");
        assert_eq!(decode_pending(&encode_pending(&pending)), pending);
        assert_eq!(decode_pending(""), vec![]);
    }

    /// One unreadable group never takes the readable ones with it — nor does
    /// a sighting that names no intent, or a zero time.
    #[test]
    fn an_unreadable_intent_is_dropped_alone() {
        let good = staged("bb", vec![LAPTOP], None);
        let stored = format!(
            "garbage;{};{}:not-hex;{}@{T0};{}@0",
            encode_pending(std::slice::from_ref(&good)),
            row("cc"),
            row("dd"),
            row("bb"),
        );
        assert_eq!(decode_pending(&stored), vec![good]);
    }

    /// The in-flight rule: a row still on the roster is a
    /// wait — first sighted, then waited out — and a drop only once two
    /// sightings stand [`DELETION_IN_FLIGHT_BOUND_MS`] apart. A row gone
    /// completes whatever the sighting says.
    #[test]
    fn a_row_still_held_is_waited_out_before_the_intent_is_dropped() {
        use ReconcileVerdict::{Complete, Drop, Wait};
        let bound = DELETION_IN_FLIGHT_BOUND_MS;
        assert_eq!(reconcile_verdict(false, None, T0), Complete);
        assert_eq!(reconcile_verdict(false, Some(T0), T0 + bound), Complete);
        assert_eq!(
            reconcile_verdict(true, None, T0),
            Wait { stamp: Some(T0) },
            "the first sighting is recorded, never acted on"
        );
        assert_eq!(reconcile_verdict(true, Some(T0), T0), Wait { stamp: None });
        assert_eq!(
            reconcile_verdict(true, Some(T0), T0 + bound - 1),
            Wait { stamp: None }
        );
        assert_eq!(
            reconcile_verdict(true, Some(T0), T0 + bound),
            Drop { since: T0 }
        );
        // Clock trouble only ever lengthens the wait.
        assert_eq!(
            reconcile_verdict(true, None, 0),
            Wait { stamp: None },
            "an unreadable clock records nothing"
        );
        assert_eq!(reconcile_verdict(true, Some(T0), 0), Wait { stamp: None });
        assert_eq!(
            reconcile_verdict(true, Some(T0 + bound), T0),
            Wait { stamp: Some(T0) },
            "a sighting from the future is re-taken now"
        );
    }

    /// A sighting belongs to the flight it was taken on: a removal re-staged
    /// between a pass's read and its write (a retry, whose deletion may be in
    /// flight right now) is neither stamped with the old sighting nor dropped
    /// on it.
    #[test]
    fn a_re_staged_removal_is_neither_stamped_nor_dropped_on_an_older_sighting() {
        let mut pending = vec![staged("bb", vec![LAPTOP], Some(T0))];
        assert!(!note_row_present(&mut pending, &row("bb"), None, T0 + 5));
        assert!(!drop_if_unchanged(&mut pending, &row("bb"), T0 + 5));
        assert_eq!(pending, vec![staged("bb", vec![LAPTOP], Some(T0))]);

        stage_pending(&mut pending, removal("bb", vec![LAPTOP]));
        assert!(
            !drop_if_unchanged(&mut pending, &row("bb"), T0),
            "the retry is a new flight"
        );
        assert!(!note_row_present(
            &mut pending,
            &row("bb"),
            Some(T0),
            T0 + 5
        ));
        assert_eq!(pending, vec![staged("bb", vec![LAPTOP], None)]);

        assert!(note_row_present(&mut pending, &row("BB"), None, T0 + 5));
        assert!(drop_if_unchanged(&mut pending, &row("BB"), T0 + 5));
        assert!(pending.is_empty());
    }

    /// **The sighting spelling is additive, both ways**
    /// (`version-compatibility.md`): a slot written before sightings existed
    /// reads as unsighted intents, and the reader that predates sightings —
    /// frozen here verbatim — reads every intent of a sighted slot unchanged.
    #[test]
    fn the_sighting_spelling_is_additive_in_both_directions() {
        let legacy = format!(
            "{}:{};{}:",
            row("bb"),
            crate::hex32::encode(&LAPTOP),
            row("cc")
        );
        assert_eq!(
            decode_pending(&legacy),
            vec![staged("bb", vec![LAPTOP], None), staged("cc", vec![], None)]
        );

        let sighted = vec![
            staged("bb", vec![LAPTOP, PHONE], Some(T0)),
            staged("cc", vec![PHONE], None),
        ];
        let encoded = encode_pending(&sighted);
        assert_eq!(decode_pending(&encoded), sighted);
        assert_eq!(
            pre_sighting_decode(&encoded),
            sighted.into_iter().map(|s| s.removal).collect::<Vec<_>>()
        );
    }

    /// `decode_pending` as it stood before sightings (2026-09-19), verbatim
    /// but for its name — the reader an older process sharing the slot runs.
    fn pre_sighting_decode(stored: &str) -> Vec<PendingFleetRemoval> {
        stored
            .split(';')
            .filter(|g| !g.is_empty())
            .filter_map(|group| {
                let (row, ids) = group.split_once(':')?;
                let targets = ids
                    .split(',')
                    .filter(|i| !i.is_empty())
                    .map(|i| crate::hex32::decode(i).ok())
                    .collect::<Option<Vec<_>>>()?;
                (!row.is_empty()).then(|| PendingFleetRemoval {
                    row: row.to_string(),
                    targets,
                })
            })
            .collect()
    }

    /// A plausible wall-clock time (2026-09-21) for sightings.
    const T0: u64 = 1_790_000_000_000;

    fn removal(tag: &str, targets: Vec<[u8; 32]>) -> PendingFleetRemoval {
        PendingFleetRemoval {
            row: row(tag),
            targets,
        }
    }

    fn staged(tag: &str, targets: Vec<[u8; 32]>, since: Option<u64>) -> StagedFleetRemoval {
        StagedFleetRemoval {
            removal: removal(tag, targets),
            present_since_ms: since,
        }
    }

    const ME: [u8; 32] = [0x01; 32];
    const LAPTOP: [u8; 32] = [0x02; 32];
    const PHONE: [u8; 32] = [0x03; 32];

    fn row(tag: &str) -> String {
        tag.repeat(32)
    }

    /// Me (row aa), a laptop and a phone — all verified members; which of the
    /// two siblings has stated its row is each test's own business.
    fn facts() -> RemovalFacts {
        RemovalFacts {
            me: ME,
            own_row: Some(row("aa")),
            members: [ME, LAPTOP, PHONE].into(),
            removed: BTreeSet::new(),
            bindings: [(ME, row("aa"))].into(),
        }
    }

    /// The hit arm, caller edition: the nest puts the caller's own principal
    /// on the row the user picked. Nothing may be written.
    #[test]
    fn a_row_carrying_the_callers_own_principal_is_refused() {
        assert_eq!(
            resolve_removal_targets(&facts(), &row("bb"), Some(&ME)),
            Err(FleetRemovalRefusal::OwnDevice)
        );
    }

    /// Client-held truth about which row is ours beats anything on the row:
    /// the own enrolled row is refused even with a sibling's principal on it.
    #[test]
    fn this_devices_own_row_is_refused_whatever_principal_it_carries() {
        assert_eq!(
            resolve_removal_targets(&facts(), &row("AA"), Some(&LAPTOP)),
            Err(FleetRemovalRefusal::OwnDevice)
        );
        let mut unlatched = facts();
        unlatched.own_row = None; // the latch is blank after a re-mint
        assert_eq!(
            resolve_removal_targets(&unlatched, &row("aa"), Some(&LAPTOP)),
            Err(FleetRemovalRefusal::OwnDevice),
            "this device's own published statement still names the row"
        );
    }

    #[test]
    fn a_principal_that_is_not_a_verified_member_is_refused() {
        assert_eq!(
            resolve_removal_targets(&facts(), &row("bb"), Some(&[0x7f; 32])),
            Err(FleetRemovalRefusal::NotAMember)
        );
    }

    /// The binding's own pin, both arms at once: the user removes the laptop's
    /// row and the nest swaps the live phone's principal onto it. The member
    /// that STATES the row is removed (the laptop is not spared) and the
    /// nest's pick is not (the phone is not hit).
    #[test]
    fn the_member_stating_the_row_is_the_target_whatever_the_nest_claims() {
        let mut f = facts();
        f.bindings.insert(LAPTOP, row("bb"));
        f.bindings.insert(PHONE, row("cc"));
        assert_eq!(
            resolve_removal_targets(&f, &row("bb"), Some(&PHONE)),
            Ok(vec![LAPTOP])
        );
        // A stripped principal spares nobody either.
        assert_eq!(
            resolve_removal_targets(&f, &row("bb"), None),
            Ok(vec![LAPTOP])
        );
    }

    /// The laptop states no row yet (enrollment legs not run), so the nest's claim
    /// is all there is — but it may not name a member that states another row.
    #[test]
    fn a_claimed_member_stating_a_different_row_is_a_mismatch() {
        let mut f = facts();
        f.bindings.insert(PHONE, row("cc"));
        assert_eq!(
            resolve_removal_targets(&f, &row("bb"), Some(&PHONE)),
            Err(FleetRemovalRefusal::RowMismatch)
        );
    }

    #[test]
    fn an_unbound_verified_member_is_accepted_on_the_nests_claim() {
        assert_eq!(
            resolve_removal_targets(&facts(), &row("bb"), Some(&LAPTOP)),
            Ok(vec![LAPTOP])
        );
    }

    /// One machine that re-minted its principal enrolled two ids on one row;
    /// removing the row removes the device, so both go.
    #[test]
    fn every_member_stating_the_row_is_removed() {
        let mut f = facts();
        f.bindings.insert(LAPTOP, row("bb"));
        f.bindings.insert(PHONE, row("bb"));
        assert_eq!(
            resolve_removal_targets(&f, &row("bb"), Some(&LAPTOP)),
            Ok(vec![LAPTOP, PHONE])
        );
    }

    /// A statement filed at a cell that is not a verified member is not a
    /// binding at all — it neither targets that id nor blocks the fallback.
    #[test]
    fn a_non_members_statement_binds_nothing() {
        let mut f = facts();
        f.bindings.insert([0x7f; 32], row("bb"));
        assert_eq!(
            resolve_removal_targets(&f, &row("bb"), Some(&LAPTOP)),
            Ok(vec![LAPTOP])
        );
    }

    /// The honest roster: my row, the laptop's and the phone's, each carrying
    /// its real principal.
    fn roster() -> Vec<(String, Option<[u8; 32]>)> {
        vec![
            (row("aa"), Some(ME)),
            (row("bb"), Some(LAPTOP)),
            (row("cc"), Some(PHONE)),
        ]
    }

    /// The review probe's scenario: a stolen laptop mis-states its row —
    /// fabricated, the phone's, or the caller's own — against an honest nest.
    /// Its real row still refuses (the rule cannot tell this from a
    /// re-pairing), but it is always listed and the member door removes it.
    #[test]
    fn a_member_that_misstates_its_row_is_listed_and_still_removable() {
        for stated in [row("ee"), row("cc"), row("aa")] {
            let mut f = facts();
            f.bindings.insert(PHONE, row("cc"));
            f.bindings.insert(LAPTOP, stated.clone());
            assert_eq!(
                resolve_removal_targets(&f, &row("bb"), Some(&LAPTOP)),
                Err(FleetRemovalRefusal::RowMismatch),
                "stated {stated}"
            );
            assert!(
                unaccounted_members(&f, &roster()).contains(&LAPTOP),
                "stated {stated}: the laptop cannot state itself off the list"
            );
            assert_eq!(resolve_member_removal(&f, &LAPTOP), Ok(vec![LAPTOP]));
        }
    }

    /// Stating a sibling's row makes that row resolve to two members, so the
    /// honest sibling is listed beside the liar — the user removes one by its
    /// key instead of both by the row.
    #[test]
    fn a_row_two_members_state_accounts_for_neither() {
        let mut f = facts();
        f.bindings.insert(PHONE, row("cc"));
        f.bindings.insert(LAPTOP, row("cc"));
        assert_eq!(unaccounted_members(&f, &roster()), vec![LAPTOP, PHONE]);
    }

    /// What `RowMismatch` buys is kept: a nest that re-pairs the laptop's row
    /// onto the stating phone is refused, the phone stays accounted for by its
    /// own row, and the spared laptop is the one listed.
    #[test]
    fn a_re_pairing_nest_lists_the_spared_member_not_its_pick() {
        let mut f = facts();
        f.bindings.insert(PHONE, row("cc"));
        let mut re_paired = roster();
        re_paired[1].1 = Some(PHONE);
        assert_eq!(
            resolve_removal_targets(&f, &row("bb"), Some(&PHONE)),
            Err(FleetRemovalRefusal::RowMismatch)
        );
        assert_eq!(unaccounted_members(&f, &re_paired), vec![LAPTOP]);
    }

    /// A settled honest fleet — stating or not, none re-minted — lists nobody,
    /// and a member whose row is gone (removed on web, or a stripped claim) is
    /// listed.
    #[test]
    fn an_honest_roster_accounts_for_everyone_and_a_missing_row_does_not() {
        assert!(unaccounted_members(&facts(), &roster()).is_empty());
        let mut f = facts();
        f.bindings.insert(LAPTOP, row("bb"));
        f.bindings.insert(PHONE, row("cc"));
        assert!(unaccounted_members(&f, &roster()).is_empty());
        assert_eq!(unaccounted_members(&f, &roster()[..2]), vec![PHONE]);
        let mut stripped = roster();
        stripped[1].1 = None;
        assert_eq!(unaccounted_members(&facts(), &stripped), vec![LAPTOP]);
    }

    /// An honest device left stating a stale row (adopted onto another row,
    /// lost before it restated): the stale row still resolves to it while it
    /// exists, and once it is gone the device is listed.
    #[test]
    fn an_honest_stale_statement_never_strands_the_device() {
        let mut f = facts();
        f.bindings.insert(LAPTOP, row("dd"));
        let mut with_stale = roster();
        with_stale.push((row("dd"), None));
        assert_eq!(
            resolve_removal_targets(&f, &row("dd"), None),
            Ok(vec![LAPTOP])
        );
        assert!(!unaccounted_members(&f, &with_stale).contains(&LAPTOP));
        assert!(unaccounted_members(&f, &roster()).contains(&LAPTOP));
        // Once removed by either door, its surviving row needs no fleet write.
        f.members.remove(&LAPTOP);
        f.removed.insert(LAPTOP);
        assert_eq!(
            resolve_removal_targets(&f, &row("bb"), Some(&LAPTOP)),
            Ok(vec![])
        );
    }

    #[test]
    fn the_member_door_refuses_this_device_and_strangers() {
        assert_eq!(
            resolve_member_removal(&facts(), &ME),
            Err(FleetRemovalRefusal::OwnDevice)
        );
        assert_eq!(
            resolve_member_removal(&facts(), &[0x7f; 32]),
            Err(FleetRemovalRefusal::NotAMember)
        );
        let mut f = facts();
        f.members.remove(&LAPTOP);
        f.removed.insert(LAPTOP);
        assert_eq!(resolve_member_removal(&f, &LAPTOP), Ok(vec![]));
    }

    #[test]
    fn a_row_naming_no_fleet_member_needs_no_fleet_write() {
        assert_eq!(
            resolve_removal_targets(&facts(), &row("bb"), None),
            Ok(vec![])
        );
        let mut f = facts();
        f.members.remove(&LAPTOP);
        f.removed.insert(LAPTOP);
        assert_eq!(
            resolve_removal_targets(&f, &row("bb"), Some(&LAPTOP)),
            Ok(vec![]),
            "already removed — a retried gesture is idempotent"
        );
    }
}
