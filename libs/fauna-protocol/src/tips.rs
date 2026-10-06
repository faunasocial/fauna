//! `fauna.tips.list` — the post-addressed tip attribution read.
//!
//! `docs/goal/behavior/monetization.md` § Tips (ratified 2026-07-22): a tip
//! names *(payee, post)* and its consequence is **attribution/display/
//! notification, never a grant**. This is the read that consequence is
//! delivered through, and it is deliberately **mechanism-independent**: the
//! wire says nothing about NIP-57, so a tip arriving over a future mechanism
//! surfaces here with no client change (§ *One model, many mechanisms, two
//! targets*).
//!
//! **Post-addressed, never mechanism-addressed.** Its predecessor
//! `nostr.zaps.total` is keyed by *Nostr event id* — the mechanism's own
//! identifier, which a Fauna app holding a `PostSummary` does not have.
//! That is why § Implementation status recorded "no live display/attribution
//! surface on any client" even though the totals existed: they were not
//! addressable from a post. Keying on `post_id` is what closes that, and it
//! carries the same anti-enumeration property
//! `fauna.subscriptions.post_unlock.get` relies on — `post_id = blake3(body)`
//! is unguessable without having seen the post, so possession of the id is
//! evidence of legitimate access.
//!
//! On any error (transport, undesignated id) the client renders the post with
//! no tip surface.

use crate::Value;
use fauna_core::identity::ActorId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Most tip items one reply will carry, whatever the request asks for.
///
/// The list is capped and the **totals are not**: a post with ten thousand
/// tips still reports its true `total_msats` and `tip_count` (SQL aggregates
/// over every row) while returning a bounded attribution window. An unpaged
/// list kind over an attacker-growable table is the storm shape this
/// codebase has already had to fix once elsewhere — the totals are what the
/// post card renders, and no display needs every tipper at once.
pub const MAX_TIP_ITEMS: u32 = 100;

/// Default window when the request names no limit.
pub const DEFAULT_TIP_ITEMS: u32 = 20;

/// List the tips on one post.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TipsListRequest {
    /// Hex 32-byte Fauna post id (the [`crate::posts::PostCreateReply::post_id`]
    /// shape). Malformed ⇒ refused, the same rule `tiers.create` applies to
    /// its designation.
    pub post_id: String,
    /// Attribution-window size. Absent ⇒ [`DEFAULT_TIP_ITEMS`]; clamped down
    /// to [`MAX_TIP_ITEMS`] rather than refused, so a newer client asking for
    /// more gets the cap instead of an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema and
    /// forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TipsListReply {
    /// Sum over **every** tip on the post that reported an amount, in
    /// millisatoshis — not just the returned window.
    pub total_msats: i64,
    /// How many tips there are on the post in total, including any that
    /// carried no amount. So this can exceed the number of tips contributing
    /// to `total_msats`: "3 people tipped" stays true when one receipt had no
    /// parseable invoice.
    pub tip_count: i64,
    /// Newest-first attribution window, at most the effective limit.
    #[serde(default)]
    pub tips: Vec<TipItem>,
    /// Whether the post has more tips than `tips` carries — so a client can
    /// render "and N others" honestly without inferring it from a length
    /// comparison against a cap it would have to hard-code.
    #[serde(default)]
    pub has_more: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema and
    /// forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One tip, as a display surface renders it.
///
/// Deliberately absent, and load-bearing by their absence: **no tier and no
/// validity window** (§ Tips — "it carries no tier and no validity window").
/// A field of either kind appearing here would mean a purchase had been
/// misrouted onto the tip surface; purchases go through the entitlement
/// waist instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TipItem {
    /// The tipper, when their mechanism identity resolved to an actor on this
    /// box. `None` covers both "the mechanism named no sender" and "the
    /// sender is not a local actor" — an outside tip still counts and still
    /// displays, unattributed.
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<ActorId>,
    /// The tipper's mechanism-native identifier when there is one (a Nostr
    /// pubkey today) — what a client shows for an unresolvable sender rather
    /// than showing nothing. Public data by construction: it is what the
    /// mechanism itself published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_ref: Option<String>,
    /// Millisatoshis, when the mechanism reported an amount. `None` is a real
    /// state, not a parse failure — consumers that sum MUST skip it rather
    /// than coerce to 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_msats: Option<i64>,
    /// Which mechanism carried it — the stable token from
    /// `fauna_payments::tips::TipMechanism` (`"nostr_zap"` today). Present so
    /// a display can attribute the carrier without any consumer branching on
    /// it for *behavior*: the consequence class is fixed by being a tip.
    pub mechanism: String,
    /// Arrival on this box, seconds since epoch. The box's own observation,
    /// never a sender-controlled timestamp — display ordering must not be
    /// steerable by whoever minted the payment.
    pub received_at: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema and
    /// forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl TipsListRequest {
    /// The window this request actually gets: its own limit clamped into
    /// `1..=MAX_TIP_ITEMS`, or the default when it named none.
    ///
    /// Clamping rather than refusing keeps a newer client that asks for more
    /// working against an older nest's cap, and a `0` — which would otherwise
    /// mean "return nothing", an answer no caller wants and every caller
    /// would have to special-case — reads as the default.
    pub fn effective_limit(&self) -> u32 {
        match self.limit {
            None | Some(0) => DEFAULT_TIP_ITEMS,
            Some(n) => n.min(MAX_TIP_ITEMS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_limit_is_the_default() {
        assert_eq!(
            TipsListRequest::default().effective_limit(),
            DEFAULT_TIP_ITEMS
        );
    }

    #[test]
    fn an_oversized_limit_clamps_rather_than_refusing() {
        let req = TipsListRequest {
            limit: Some(10_000),
            ..Default::default()
        };
        assert_eq!(req.effective_limit(), MAX_TIP_ITEMS);
    }

    /// Zero would mean "return nothing" — an answer no caller wants and every
    /// caller would have to special-case, so it reads as the default instead.
    #[test]
    fn a_zero_limit_reads_as_the_default() {
        let req = TipsListRequest {
            limit: Some(0),
            ..Default::default()
        };
        assert_eq!(req.effective_limit(), DEFAULT_TIP_ITEMS);
    }

    #[test]
    fn a_limit_under_the_cap_is_honoured() {
        let req = TipsListRequest {
            limit: Some(5),
            ..Default::default()
        };
        assert_eq!(req.effective_limit(), 5);
    }
}
