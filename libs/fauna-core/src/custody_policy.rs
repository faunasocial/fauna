//! T15's custody policy as pure arithmetic — the meter a custodian keeps per
//! custody, and the eviction plan a byte budget implies.
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § Replica posture
//! → *Custody policy (T15)*. Three of its bullets are this module:
//!
//! - **Two-sided bounds** — "the acceptance names the budget … Bound retained
//!   bytes, never just quota counters". The budget is
//!   [`crate::custodies_held::CustodyHeld::retained_bytes_cap`]; what it bounds
//!   is [`CustodyMeter::held_bytes`], measured from the bytes actually at rest.
//! - **Metering** — "bytes and item counts, by scope family". One
//!   [`ScopeMeter`] per `(scope, item class)`.
//! - **Eviction** — "over budget, a custodian evicts **payload bytes only** —
//!   journal, frontiers, tombstones, and the index floor are always-present …
//!   and eviction is always **receipt-visible**".
//!
//! # Why the plan is a pure function, in a wasm-safe crate
//!
//! The same arithmetic has two consumers: the custodian runtime that *enforces*
//! the budget (`fauna_sync_engine::custody_leg`) and the T16 host-side facet
//! that *renders* it — on every app, web included. Splitting plan from act
//! (the shape [`fauna_client_backup::custodian::plan_reclaim`] already uses for
//! the segment-backup custodian) is what keeps those two from drifting into
//! two different answers to "how full am I".
//!
//! It is deliberately **not** the same *rule* as that sibling: the
//! backup-destination custodian never evicts a live path and reports
//! cap-reached instead, because there the custodian may hold the only copy of a
//! generation. Here the owner's own fleet and nest hold the canonical planes
//! and the custodian is opportunistic redundancy, so T15 rules the other way —
//! payload goes, and the shrinkage is reported rather than absorbed.
//!
//! # Keyless by construction
//!
//! Everything here is byte counts and cleartext coordinates. Nothing in this
//! module reads, or could read, a sealed envelope's contents — the constraint
//! the frozen posture analysis holds the custodian to.

use serde::{Deserialize, Serialize};

/// The [`ScopeMeter::item_class`] of a scope's **adopted segment files** — the
/// bulk plane a custodian adopts beside the relay rows (the bootstrap
/// contract's segment half). Not a wire item class: the relay plane's families
/// carry those (`state-entry`, `record-cid`); this names the second plane the
/// same budget bounds.
///
/// A segment family's `rows` are segments, its `payload_bytes` the held `.dat`
/// bytes plus every `.meta` sidecar, and its evictable subset the `.dat` bytes
/// alone. The sidecar is the index floor — it is what keeps an evicted
/// segment's records "complete in metadata" — so, like a tombstone, it is never
/// eviction's to take. The eviction unit is the whole `.dat`
/// (`message-segment-store.md` § Nest dehydration: a CARv2 file is immutable),
/// oldest segment first.
pub const SEGMENT_ITEM_CLASS: &str = "segment";

/// One scope family's meter — the "bytes and item counts, by scope family"
/// T15 requires, for one `(scope, item class)` pair.
///
/// The split that matters is **payload vs floor**: `payload_bytes` is every
/// byte held for this family, of which `evictable_bytes` may be dropped under
/// budget pressure. The difference is the always-present floor — tombstones and
/// the coordinate rows themselves — and it is why an over-budget custodian can
/// be honestly unable to get under its cap (see [`CustodyBudgetState::AtFloor`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeMeter {
    /// The scope this family covers.
    #[serde(default)]
    pub scope: String,
    /// The wire item-class discriminator (`state-entry`, `record-cid`).
    #[serde(default)]
    pub item_class: String,
    /// Rows held for this family, evicted payload included — an evicted row
    /// still counts, because its coordinate floor is still held and still
    /// served.
    #[serde(default)]
    pub rows: u64,
    /// Payload bytes currently at rest for this family.
    #[serde(default)]
    pub payload_bytes: u64,
    /// The subset of [`Self::rows`] whose payload may be dropped.
    #[serde(default)]
    pub evictable_rows: u64,
    /// The subset of [`Self::payload_bytes`] that may be dropped.
    #[serde(default)]
    pub evictable_bytes: u64,
}

/// What one custody holds right now, by scope family.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyMeter {
    #[serde(default)]
    pub scopes: Vec<ScopeMeter>,
}

impl CustodyMeter {
    /// Total payload bytes held — the quantity the budget bounds.
    pub fn held_bytes(&self) -> u64 {
        self.scopes
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.payload_bytes))
    }

    /// Total rows held across every family.
    pub fn rows(&self) -> u64 {
        self.scopes
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.rows))
    }

    /// Total bytes eviction could free.
    pub fn evictable_bytes(&self) -> u64 {
        self.scopes
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.evictable_bytes))
    }

    /// Bytes no budget pressure may touch — T15's always-present floor.
    pub fn floor_bytes(&self) -> u64 {
        self.held_bytes().saturating_sub(self.evictable_bytes())
    }
}

/// Where a custody stands against its budget. Three states, not two, for the
/// same reason the generation rows have three: *under budget*, *over but
/// reclaimable*, and *over with nothing left to give* are different facts about
/// this custodian's coverage, and collapsing the last two would report a
/// custodian as healthy-after-eviction when it is permanently over its cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustodyBudgetState {
    /// Held bytes are within the cap. Nothing to do.
    Ok,
    /// Over the cap, and evicting payload brings it back under.
    OverBudget,
    /// Over the cap with every evictable byte already spoken for — the
    /// always-present floor alone exceeds the budget. The honest answer is a
    /// custodian that keeps holding and reports the overage, never one that
    /// eats its floor. T15: "a custodian refusing, narrowing, or evicting is
    /// legal — availability is honest".
    AtFloor,
}

/// How many payload bytes to free from one scope family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeEviction {
    pub scope: String,
    pub item_class: String,
    /// Bytes to free from this family. Never exceeds the family's
    /// [`ScopeMeter::evictable_bytes`].
    pub target_bytes: u64,
}

/// The answer [`plan_custody_eviction`] gives: what to free, and the honest
/// state to report whether or not freeing it is enough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyEvictionPlan {
    /// Per-family byte targets, largest evictable pool first. Empty under
    /// budget **and** at the floor — in the second case because there is
    /// nothing left to plan, which is exactly what [`CustodyBudgetState::AtFloor`]
    /// says.
    pub evictions: Vec<ScopeEviction>,
    pub state: CustodyBudgetState,
    /// Payload bytes held when the plan was made.
    pub held_bytes: u64,
    /// The budget this plan was made against.
    pub cap_bytes: u64,
    /// Bytes above the cap before the plan runs (0 when within budget).
    pub over_by: u64,
    /// Bytes that will *still* be above the cap once the whole plan has run.
    /// Non-zero only under [`CustodyBudgetState::AtFloor`], and it is the
    /// number the receipt reports rather than absorbs.
    pub unreclaimable: u64,
}

impl CustodyEvictionPlan {
    /// Total bytes this plan frees.
    pub fn planned_bytes(&self) -> u64 {
        self.evictions
            .iter()
            .fold(0u64, |acc, e| acc.saturating_add(e.target_bytes))
    }
}

/// Plan what payload to evict so `meter` fits inside `cap_bytes`.
///
/// The rules, in order:
///
/// 1. **Within budget → nothing.** No eviction, [`CustodyBudgetState::Ok`].
/// 2. **Over budget → free the overage, payload only.** Families are drawn from
///    **largest evictable pool first** — T15 leaves the ordering heuristic to
///    the build, and biggest-first disturbs the fewest families to free a given
///    number of bytes. (Within a family, *which* rows go is the store's
///    oldest-first walk; this plan only sizes the ask.)
/// 3. **Overage exceeds every evictable byte → plan them all, report the rest.**
///    [`CustodyBudgetState::AtFloor`] with a non-zero
///    [`CustodyEvictionPlan::unreclaimable`]. The floor is never eaten to hit a
///    number.
///
/// A `cap_bytes` of 0 is a real budget — "hold the floor, no payload" — not a
/// missing one; a custody with no cap recorded never reaches this function
/// (the runtime treats an absent cap as the ceremony default).
pub fn plan_custody_eviction(meter: &CustodyMeter, cap_bytes: u64) -> CustodyEvictionPlan {
    let held_bytes = meter.held_bytes();
    if held_bytes <= cap_bytes {
        return CustodyEvictionPlan {
            evictions: Vec::new(),
            state: CustodyBudgetState::Ok,
            held_bytes,
            cap_bytes,
            over_by: 0,
            unreclaimable: 0,
        };
    }

    let over_by = held_bytes - cap_bytes;
    let mut families: Vec<&ScopeMeter> = meter
        .scopes
        .iter()
        .filter(|s| s.evictable_bytes > 0)
        .collect();
    // Largest pool first; ties broken by the coordinate so a plan is
    // deterministic for a given meter (two runs of the same custody must not
    // disagree about which family to disturb).
    families.sort_by(|a, b| {
        b.evictable_bytes
            .cmp(&a.evictable_bytes)
            .then_with(|| a.scope.cmp(&b.scope))
            .then_with(|| a.item_class.cmp(&b.item_class))
    });

    let mut evictions = Vec::new();
    let mut remaining = over_by;
    for family in families {
        if remaining == 0 {
            break;
        }
        let take = family.evictable_bytes.min(remaining);
        remaining -= take;
        evictions.push(ScopeEviction {
            scope: family.scope.clone(),
            item_class: family.item_class.clone(),
            target_bytes: take,
        });
    }

    let state = if remaining > 0 {
        CustodyBudgetState::AtFloor
    } else {
        CustodyBudgetState::OverBudget
    };
    CustodyEvictionPlan {
        evictions,
        state,
        held_bytes,
        cap_bytes,
        over_by,
        unreclaimable: remaining,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(scope: &str, payload: u64, evictable: u64) -> ScopeMeter {
        ScopeMeter {
            scope: scope.into(),
            item_class: "state-entry".into(),
            rows: 10,
            payload_bytes: payload,
            evictable_rows: 8,
            evictable_bytes: evictable,
        }
    }

    #[test]
    fn within_budget_plans_nothing() {
        let meter = CustodyMeter {
            scopes: vec![family("state", 400, 300)],
        };
        let plan = plan_custody_eviction(&meter, 1000);
        assert_eq!(plan.state, CustodyBudgetState::Ok);
        assert!(plan.evictions.is_empty());
        assert_eq!(plan.over_by, 0);
        assert_eq!(plan.unreclaimable, 0);
    }

    /// Exactly at the cap is within it — a budget is a ceiling, not a
    /// threshold to cross.
    #[test]
    fn exactly_at_the_cap_plans_nothing() {
        let meter = CustodyMeter {
            scopes: vec![family("state", 1000, 900)],
        };
        assert_eq!(
            plan_custody_eviction(&meter, 1000).state,
            CustodyBudgetState::Ok
        );
    }

    #[test]
    fn over_budget_frees_exactly_the_overage() {
        let meter = CustodyMeter {
            scopes: vec![family("state", 1500, 1400)],
        };
        let plan = plan_custody_eviction(&meter, 1000);
        assert_eq!(plan.state, CustodyBudgetState::OverBudget);
        assert_eq!(plan.over_by, 500);
        assert_eq!(plan.planned_bytes(), 500);
        assert_eq!(plan.unreclaimable, 0);
    }

    #[test]
    fn the_largest_evictable_pool_is_drawn_from_first() {
        let meter = CustodyMeter {
            scopes: vec![
                family("state", 300, 300),
                family("state-fleet", 900, 900),
                family("conv:a", 300, 300),
            ],
        };
        let plan = plan_custody_eviction(&meter, 1000);
        assert_eq!(plan.over_by, 500);
        // One family covers the whole ask, so only it is disturbed.
        assert_eq!(plan.evictions.len(), 1);
        assert_eq!(plan.evictions[0].scope, "state-fleet");
        assert_eq!(plan.evictions[0].target_bytes, 500);
    }

    /// The ask spills into further families in the same order, and never asks a
    /// family for more than it has.
    #[test]
    fn a_spilling_ask_never_over_draws_a_family() {
        let meter = CustodyMeter {
            scopes: vec![family("a", 400, 400), family("b", 900, 900)],
        };
        let plan = plan_custody_eviction(&meter, 100);
        assert_eq!(plan.over_by, 1200);
        assert_eq!(plan.state, CustodyBudgetState::OverBudget);
        assert_eq!(plan.evictions.len(), 2);
        assert_eq!(plan.evictions[0].scope, "b");
        assert_eq!(plan.evictions[0].target_bytes, 900);
        assert_eq!(plan.evictions[1].scope, "a");
        assert_eq!(plan.evictions[1].target_bytes, 300);
        assert_eq!(plan.planned_bytes(), 1200);
    }

    /// T15's floor, the case that must never silently "succeed": a custody
    /// whose held bytes are almost all tombstones + coordinate rows is over its
    /// cap and *stays* over it. The plan takes every evictable byte, reports
    /// the rest as unreclaimable, and never touches the floor.
    #[test]
    fn a_floor_that_exceeds_the_cap_reports_rather_than_eats_itself() {
        let meter = CustodyMeter {
            scopes: vec![ScopeMeter {
                scope: "state".into(),
                item_class: "state-entry".into(),
                rows: 100,
                payload_bytes: 5000,
                evictable_rows: 5,
                evictable_bytes: 500,
            }],
        };
        let plan = plan_custody_eviction(&meter, 1000);
        assert_eq!(plan.state, CustodyBudgetState::AtFloor);
        assert_eq!(plan.over_by, 4000);
        assert_eq!(plan.planned_bytes(), 500, "every evictable byte is planned");
        assert_eq!(
            plan.unreclaimable, 3500,
            "and the floor's overage is reported, not absorbed"
        );
        assert_eq!(meter.floor_bytes(), 4500);
    }

    /// A custody with no evictable bytes at all plans nothing and is still
    /// honestly over budget — the degenerate `AtFloor`.
    #[test]
    fn an_all_floor_custody_plans_nothing_and_says_so() {
        let meter = CustodyMeter {
            scopes: vec![ScopeMeter {
                scope: "state".into(),
                item_class: "state-entry".into(),
                rows: 40,
                payload_bytes: 2000,
                evictable_rows: 0,
                evictable_bytes: 0,
            }],
        };
        let plan = plan_custody_eviction(&meter, 100);
        assert_eq!(plan.state, CustodyBudgetState::AtFloor);
        assert!(plan.evictions.is_empty());
        assert_eq!(plan.unreclaimable, 1900);
    }

    /// A zero cap is a real budget — drop all payload, keep the floor.
    #[test]
    fn a_zero_cap_evicts_every_evictable_byte() {
        let meter = CustodyMeter {
            scopes: vec![family("state", 800, 800)],
        };
        let plan = plan_custody_eviction(&meter, 0);
        assert_eq!(plan.state, CustodyBudgetState::OverBudget);
        assert_eq!(plan.planned_bytes(), 800);
        assert_eq!(plan.unreclaimable, 0);
    }

    #[test]
    fn the_meter_totals_are_saturating_and_consistent() {
        let meter = CustodyMeter {
            scopes: vec![family("a", 10, 4), family("b", 20, 5)],
        };
        assert_eq!(meter.held_bytes(), 30);
        assert_eq!(meter.evictable_bytes(), 9);
        assert_eq!(meter.floor_bytes(), 21);
        assert_eq!(meter.rows(), 20);
    }

    #[test]
    fn a_plan_is_deterministic_for_equal_pools() {
        let meter = CustodyMeter {
            scopes: vec![family("zzz", 500, 500), family("aaa", 500, 500)],
        };
        let first = plan_custody_eviction(&meter, 600);
        let second = plan_custody_eviction(&meter, 600);
        assert_eq!(first, second);
        assert_eq!(first.evictions[0].scope, "aaa", "ties break on coordinate");
    }
}
