//! The asking price — the machine-comparable purchase threshold.
//!
//! `docs/goal/behavior/monetization.md` § The asking price (ratified
//! 2026-08-02, Q10): "A tier MAY carry an `asking_price` — an optional,
//! unit-tagged, machine-comparable amount: `{ value: u64, unit: string }`, the
//! first ratified unit `"msat"`."
//!
//! This module is the **one comparison site** that section names. Every
//! intent-inferring, amount-bearing mechanism asks the question here, so no
//! mechanism can drift into its own threshold rule — which is the same
//! discipline `zap_ingest` applies to the trust question one layer up.
//!
//! **Why the unit is a `String` and not an enum.** An enum would make an
//! unknown unit unrepresentable, and that is exactly wrong here: a *newer*
//! client may set a unit this build has never heard of, the wire must carry it
//! unchanged (`version-compatibility.md` — additive everywhere, bidirectional
//! within a major), and the ratified rule then says that unit "is *not met*,
//! fail-closed". Making it unrepresentable would force the wire layer to drop
//! or reject the field instead of storing it; keeping it a string is what lets
//! [`AskingPrice::is_met_by`] answer "no" for it honestly. [`unit_is_known`]
//! is the predicate that draws that line, and it is deliberately the *only*
//! place the known-unit set is written down.
//!
//! **What deliberately does not live here.** No exchange rates and no
//! cross-unit arithmetic: cross-unit comparison is undefined by design (an
//! oracle is an off-box dependency and a configuration surface this model
//! refuses). And no mechanism vocabulary — `unit` is a **denomination** tag,
//! never a mechanism tag, so a zap, an eCash note and a Lightning-invoice
//! webhook denominated the same way compare identically.

/// Millisatoshis — the first ratified unit, and the one every current
/// amount-bearing mechanism (today: NIP-57 zap receipts) reports in.
///
/// Re-exported from the wire layer, which **owns** the token: it is a string a
/// shipped nest and a shipped client compare, so it must stay readable in a
/// build where this whole crate is compiled away (`dynamic-features.md`
/// § Compile-time excision — [`fauna_protocol::subscriptions::TierAskingPrice`]
/// keeps round-tripping a priced tier an excised build cannot buy). Re-exported
/// rather than duplicated so the comparison rule below and the wire shape can
/// never drift onto two different tokens.
pub use fauna_protocol::subscriptions::UNIT_MSAT;

/// Does this build know how to compare amounts denominated in `unit`?
///
/// The whole known-unit set, in one place. A future fiat-denominated
/// mechanism adds its arm here **and** an amount source that reports it —
/// adding a unit alone changes no verdict, because a unit nothing produces is
/// never on the left of a comparison.
pub fn unit_is_known(unit: &str) -> bool {
    unit == UNIT_MSAT
}

/// What a tier asks for, as a machine-comparable amount.
///
/// Lives beside [`crate::PaymentEntitlement`] rather than inside it: an asking
/// price is a property of the *tier* (what it costs), while the entitlement is
/// the value a completed payment reduces to (what was bought). Keeping them
/// apart is what lets the waist stay amount-blind — nothing downstream of
/// `PaymentEntitlement` ever sees a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskingPrice {
    /// The amount asked, in [`AskingPrice::unit`]'s denomination.
    ///
    /// `0` is legal and means "any amount in this unit buys it" — a coherent
    /// author choice (name your price), not a sentinel. It is *not* the same
    /// as having no asking price at all, which means the tier cannot be bought
    /// by an intent-inferring mechanism whatever the amount.
    pub value: u64,
    /// The denomination tag — [`UNIT_MSAT`] today. A unit this build does not
    /// know is carried faithfully and never compares as met.
    pub unit: String,
}

/// Millisatoshis per satoshi — the one conversion the sat-denominated author
/// input needs to reach the msat-denominated wire. Owned by the wire layer for
/// the same reason as [`UNIT_MSAT`]; re-exported here so this module still
/// reads as the whole asking-price story.
pub use fauna_protocol::subscriptions::MSATS_PER_SAT;

// The author types **sats**; the wire carries **msats** (`monetization.md`
// § The asking price: "the author types the price in sats and one shared
// parse/format pair converts to the msat wire value").
//
// That pair's forward half MOVED TO THE WIRE LAYER on 2026-08-10 — it is
// `fauna_protocol::subscriptions::TierAskingPrice::from_sats`. Every one of its
// four callers (the FFI tier editor ×2, the wasm one, the feed's sell-gated
// compose) built a `TierAskingPrice` from the result, so the hop through this
// crate's type was pure ceremony — and it was the one thing tying priced-tier
// *authoring* to a crate the `payments` excision compiles away
// (`dynamic-features.md` § The cargo feature spine). Collapsing it left one
// conversion site instead of four map-and-rebuild blocks, which is what
// § The asking price asked for in the first place.
//
// No re-export shim was left behind: four callers, all migrated in the same
// commit, and a deprecated alias here would be a second name for one
// conversion — the drift this move exists to remove.
//
// This crate keeps what is genuinely the money plane: `AskingPrice` and its
// ratified `is_met_by` comparison rule.

impl AskingPrice {
    /// Convenience constructor for the ratified millisatoshi unit.
    pub fn msats(value: u64) -> Self {
        Self {
            value,
            unit: UNIT_MSAT.to_string(),
        }
    }

    /// **The ratified comparison rule**, verbatim from § The asking price: an
    /// amount-bearing verified payment event without explicit tier intent
    /// meets this price **iff the event's amount unit equals the asking
    /// price's unit AND its value ≥ the asking price's value**. Unit mismatch
    /// — *including a unit this build does not know* — is not met, fail-closed.
    ///
    /// Each event compares alone: there is deliberately no accumulation across
    /// events (a partial-credit ledger was rejected — new nest state with
    /// refund semantics, gameable across arbitrary time windows). Over-payment
    /// is simply met; the surplus is appreciation, not credit.
    ///
    /// Note the known-unit conjunct is not redundant with the equality one. A
    /// tier whose price a newer client set in some future unit, zapped by a
    /// mechanism reporting that *same* future unit, would compare met on
    /// string equality alone — on a build that has no idea what the unit
    /// means. Fail-closed means the build that cannot reason about a
    /// denomination does not get to decide a sale in it.
    pub fn is_met_by(&self, amount_value: u64, amount_unit: &str) -> bool {
        unit_is_known(&self.unit) && self.unit == amount_unit && amount_value >= self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_equal_amount_in_the_same_unit_meets_the_price() {
        // `>=`, not `>` — paying exactly the asking price buys it.
        assert!(AskingPrice::msats(1_000).is_met_by(1_000, UNIT_MSAT));
    }

    #[test]
    fn over_payment_meets_the_price_and_buys_nothing_extra() {
        // "Over-payment is simply met; the surplus is appreciation, not
        // credit" — there is no second entitlement and nothing to carry over,
        // which is visible here as the verdict being a plain bool.
        assert!(AskingPrice::msats(1_000).is_met_by(50_000, UNIT_MSAT));
    }

    #[test]
    fn under_payment_does_not_meet_the_price() {
        // The ratified consequence of this `false` is *not* a refusal: the
        // receipt stays a tip (§ The asking price — a zap is irrevocable, so
        // refusing returns no sats and only destroys owed attribution).
        assert!(!AskingPrice::msats(1_000).is_met_by(999, UNIT_MSAT));
    }

    #[test]
    fn a_foreign_amount_unit_never_meets_a_known_price() {
        // Cross-unit comparison is undefined by design — no exchange-rate
        // oracle. Even a wildly larger number in another denomination is not
        // met, because the question "is 5000 of X at least 1000 of Y" has no
        // on-box answer.
        //
        // The price side is deliberately the KNOWN unit. An earlier draft
        // priced in "cent" and paid in msat, which passes for the wrong
        // reason — the known-unit conjunct alone answers it, so deleting the
        // equality conjunct left the test green. Here only equality can
        // decide it.
        assert!(!AskingPrice::msats(1_000).is_met_by(5_000, "cent"));
    }

    #[test]
    fn a_unit_this_build_does_not_know_never_meets_it_even_when_both_sides_agree() {
        // The conjunct that string equality alone would lose. A newer client
        // priced the tier in a unit this build has never heard of and the
        // amount arrives in that same unit: equality holds, and the verdict
        // must still be "not met".
        let price = AskingPrice {
            value: 10,
            unit: "future-coin".to_string(),
        };
        assert!(!price.is_met_by(1_000_000, "future-coin"));
    }

    #[test]
    fn unit_comparison_is_exact_never_case_or_whitespace_folded() {
        // The unit is a wire token, not user prose. Folding here would be a
        // second, looser definition of equality living outside
        // `unit_is_known`, which is the one place the vocabulary is written.
        assert!(!AskingPrice::msats(1).is_met_by(1_000, "MSAT"));
        assert!(!AskingPrice::msats(1).is_met_by(1_000, " msat"));
    }

    #[test]
    fn a_zero_price_is_met_by_any_amount_in_that_unit() {
        // "Name your price": legal, and deliberately distinct from having no
        // asking price at all. A tier with NO price is unbuyable by an
        // inferring mechanism; a tier priced at zero is buyable by any zap.
        // Only the caller can tell those apart, because `Option::None` never
        // reaches this function.
        assert!(AskingPrice::msats(0).is_met_by(0, UNIT_MSAT));
        assert!(AskingPrice::msats(0).is_met_by(1, UNIT_MSAT));
    }

    #[test]
    fn a_zero_price_still_fails_closed_on_a_foreign_unit() {
        // The degenerate price must not become a unit-blind "yes" — the
        // known-unit and equality conjuncts still bind.
        assert!(!AskingPrice::msats(0).is_met_by(0, "cent"));
    }

    // The sats→msats conversion's own tests moved with it, to
    // `fauna_protocol::subscriptions`'s `mod tests` — they must run in the
    // excised flavor, where this crate does not exist.

    #[test]
    fn the_comparison_unit_is_the_same_token_the_wire_carries() {
        // The whole point of re-exporting rather than duplicating: a rename on
        // either side would have to break this equality first.
        assert_eq!(UNIT_MSAT, fauna_protocol::subscriptions::UNIT_MSAT);
        assert!(unit_is_known(fauna_protocol::subscriptions::UNIT_MSAT));
        // And a price built by the wire-layer constructor compares as met by
        // the rule that lives here — the two halves of the split still meet.
        let wire = fauna_protocol::subscriptions::TierAskingPrice::from_sats(21).unwrap();
        assert!(
            AskingPrice {
                value: wire.value,
                unit: wire.unit,
            }
            .is_met_by(21_000, UNIT_MSAT)
        );
    }

    #[test]
    fn the_msat_token_is_the_stable_wire_spelling() {
        // Pinned because shipped nests and shipped clients compare this
        // string: renaming it is a wire break, not a refactor.
        assert_eq!(UNIT_MSAT, "msat");
        assert!(unit_is_known("msat"));
        assert!(!unit_is_known("msats"));
        assert!(!unit_is_known("sat"));
        assert!(!unit_is_known(""));
    }
}
