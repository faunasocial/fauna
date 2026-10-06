//! Tips — per-post payments with **no entitlement consequence**.
//!
//! `docs/goal/behavior/monetization.md` § Tips (ratified 2026-07-22): "a tip
//! names a *(payee, post)* and its consequence is attribution/display/
//! notification, never a grant, so it carries no tier and no validity
//! window."
//!
//! That last clause is why this module sits beside the waist rather than
//! inside it. [`crate::PaymentEntitlement`] is the value a payment reduces to
//! when it *grants* something; a tip grants nothing, so reducing it to an
//! entitlement would require inventing a tier and a window that the model
//! deliberately says it does not have. The two consequence classes are
//! siblings — same crate, same mechanism-blindness discipline, different
//! values — exactly as the goal doc presents them.
//!
//! **Mechanism-independence is the point.** Zap receipts are the Nostr-native
//! tip mechanism and today the only one, but nothing here is Nostr-shaped: a
//! [`Tip`] names its carrier in [`Tip::mechanism`] and every consumer reads
//! the same fields whichever mechanism produced it. A tip arriving over a
//! future provider webhook adds a [`TipMechanism`] arm and no consumer change
//! — the same rule Pillar 3 states for the entitlement side ("nothing
//! downstream may special-case them").

use fauna_core::identity::ActorId;

/// Which mechanism carried a tip.
///
/// Additive by construction: a new arm is a new carrier, never a new
/// consequence — the consequence class is fixed by *being a tip* (attribution
/// and display, never a grant), which is what keeps consumers mechanism-blind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipMechanism {
    /// A NIP-57 kind-9735 zap receipt the payee's designated-signer gate
    /// believed (`monetization.md` § Zap receipts — the trust model). A
    /// receipt that did not pass that gate never becomes a `Tip` at all: the
    /// gate is applied at ingest, never at read, so every value in this
    /// module has already been judged.
    NostrZap,
}

impl TipMechanism {
    /// Stable wire/display token. Kept explicit rather than derived from the
    /// variant name so renaming the Rust arm can never silently change what
    /// a shipped client reads.
    pub fn as_str(self) -> &'static str {
        match self {
            TipMechanism::NostrZap => "nostr_zap",
        }
    }
}

impl std::fmt::Display for TipMechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One tip: a payment naming *(payee, post)* whose only consequence is
/// attribution, display and notification.
///
/// Deliberately absent, and load-bearing by their absence: **no tier and no
/// validity window**. A reviewer who finds either field growing here should
/// read § Tips first — a tip that carries a tier is a purchase, and a
/// purchase belongs on the entitlement waist ([`crate::PaymentEntitlement`]),
/// not here. The disambiguation between the two is made once, at ingest, by
/// the target post's `unlocks_post` designation (§ Per-post pay-to-unlock).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tip {
    /// The tipped author — the *(payee, post)* pair's first half.
    pub payee: ActorId,
    /// The tipped post: lowercase-hex 32-byte Fauna post id
    /// (`blake3(body)`, the `PostCreateReply::post_id` shape). Fauna-addressed
    /// on purpose — a mechanism's own identifier for the same post (a Nostr
    /// event id, say) is the mechanism's business and is resolved to this
    /// before a `Tip` exists.
    pub post_id: String,
    /// Amount in millisatoshis, when the mechanism reported one.
    ///
    /// `None` is a real and expected state, not a parse failure: a NIP-57
    /// receipt's `bolt11` tag is optional in the wild, and an amount-less
    /// receipt still attributes ("X tipped this") even though it cannot
    /// total. Consumers that sum MUST skip `None` rather than coerce it to 0
    /// — a zero-valued tip and an unknown-valued tip are different facts.
    ///
    /// Millisatoshi is the unit every mechanism reports today. A future
    /// non-Lightning mechanism gets a unit tag added *additively* beside this
    /// field (defaulting to msat, so shipped readers keep their meaning)
    /// rather than a second amount field.
    pub amount_msats: Option<u64>,
    /// The tipper, when the mechanism's sender identity resolved to a local
    /// actor. `None` covers both "the mechanism named no sender" and "the
    /// sender is not a Fauna actor this box can resolve" — a tip from outside
    /// still counts and still displays, it just displays unattributed.
    pub sender: Option<ActorId>,
    /// Which mechanism carried it.
    pub mechanism: TipMechanism,
    /// Arrival time on this box, seconds since epoch. Deliberately the box's
    /// own observation rather than any sender-controlled timestamp the
    /// mechanism carried: display ordering must not be steerable by whoever
    /// minted the payment.
    pub received_at: i64,
}

/// Aggregate over one post's tips — what a post card renders without
/// enumerating every tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TipTotals {
    /// Sum of the tips that reported an amount, in millisatoshis.
    pub total_msats: u64,
    /// How many tips there are **in total**, including amount-less ones.
    ///
    /// So `tip_count` can exceed the number of tips contributing to
    /// `total_msats`. That is deliberate: "3 people tipped" stays true when
    /// one of the three carried no parseable invoice, whereas counting only
    /// summable tips would silently under-report attribution.
    pub tip_count: u64,
}

impl TipTotals {
    /// Fold tips into totals, skipping amount-less ones for the sum but
    /// counting them all.
    ///
    /// Saturating on the sum: msat totals cannot realistically approach
    /// `u64::MAX` (that is ~1.8e19 msat against a 2.1e15 msat money supply),
    /// but a hostile or broken mechanism reporting absurd amounts must not be
    /// able to panic a nest handler in a release build's debug-assert twin.
    pub fn of<'a>(tips: impl IntoIterator<Item = &'a Tip>) -> Self {
        let mut totals = TipTotals::default();
        for tip in tips {
            totals.tip_count = totals.tip_count.saturating_add(1);
            if let Some(msats) = tip.amount_msats {
                totals.total_msats = totals.total_msats.saturating_add(msats);
            }
        }
        totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(byte: u8) -> ActorId {
        ActorId([byte; 32])
    }

    fn tip(amount: Option<u64>) -> Tip {
        Tip {
            payee: actor(1),
            post_id: "ab".repeat(32),
            amount_msats: amount,
            sender: Some(actor(2)),
            mechanism: TipMechanism::NostrZap,
            received_at: 1_700_000_000,
        }
    }

    #[test]
    fn totals_sum_amounts_and_count_every_tip() {
        let tips = [tip(Some(1_000)), tip(Some(2_500)), tip(Some(500))];
        let totals = TipTotals::of(&tips);
        assert_eq!(totals.total_msats, 4_000);
        assert_eq!(totals.tip_count, 3);
    }

    /// The distinction § Tips's attribution consequence depends on: an
    /// amount-less tip still says "someone tipped this", so it counts even
    /// though it cannot sum. Coercing `None` to 0 would keep the count right
    /// by accident; this pins that the count is not derived from the sum.
    #[test]
    fn an_amountless_tip_counts_but_does_not_sum() {
        let tips = [tip(Some(1_000)), tip(None)];
        let totals = TipTotals::of(&tips);
        assert_eq!(
            totals.total_msats, 1_000,
            "the amount-less tip adds nothing"
        );
        assert_eq!(totals.tip_count, 2, "but it is still a tip that happened");
    }

    #[test]
    fn no_tips_is_zero_not_absent() {
        let totals = TipTotals::of(&[]);
        assert_eq!(totals, TipTotals::default());
        assert_eq!((totals.total_msats, totals.tip_count), (0, 0));
    }

    /// A mechanism reporting an absurd amount must not panic the fold — the
    /// nest sums these inside a request handler.
    #[test]
    fn an_absurd_amount_saturates_rather_than_overflowing() {
        let tips = [tip(Some(u64::MAX)), tip(Some(u64::MAX))];
        let totals = TipTotals::of(&tips);
        assert_eq!(totals.total_msats, u64::MAX);
        assert_eq!(totals.tip_count, 2);
    }

    /// The wire token is pinned explicitly: renaming the Rust arm must not
    /// change what a shipped client reads.
    #[test]
    fn mechanism_token_is_stable() {
        assert_eq!(TipMechanism::NostrZap.as_str(), "nostr_zap");
        assert_eq!(TipMechanism::NostrZap.to_string(), "nostr_zap");
    }
}
