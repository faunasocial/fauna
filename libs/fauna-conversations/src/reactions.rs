//! Per-message reaction aggregates and the pure fold helper.
//!
//! [`ReactionGroup`] is the snapshot type every app renders (one row per
//! emoji). [`fold_reactions`] folds a log of add/remove events into that view,
//! preserving first-add appearance order and handling idempotent re-adds and
//! no-op removes cleanly.
//!
//! ⚠ **It folds the SET of signed ops, not the sequence.** A community room's
//! record can be re-appended at a fresh log position by any member or by its
//! home nest — the author's signature does not cover the `seq`, because the
//! nest allocates it afterwards — so "the last op in the log wins" is a rule an
//! attacker controls. The ordering token is the author's own signed stamp
//! ([`StampedReactionEvent`]), and [`fold_reactions`] carries the reasoning.

use std::collections::HashSet;

use fauna_core::identity::ActorId;
use fauna_mls::types::ReactionOp;
use serde::{Deserialize, Serialize};

/// The aggregated reaction state for one emoji on a message.
///
/// Ordered by first-add appearance in the event sequence. `count` is the
/// number of distinct current reactors; `reacted_by_me` is true when the
/// local actor is among them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ReactionGroup {
    /// The emoji (Unicode scalar sequence, e.g. `"👍"`, `"❤️"`).
    pub emoji: String,
    /// Number of distinct actors currently reacted with this emoji.
    pub count: u32,
    /// Whether the local actor (`me`) is among the current reactors.
    pub reacted_by_me: bool,
}

/// One reaction event as logged per message, carrying the **author's own
/// signed stamp**.
///
/// # Why the stamp is here
///
/// A community room's reaction op is **replayable by construction**
/// (`community-rooms.md` § The three classes → *Community* → *Who wrote it*):
/// the author's signature covers `(room, generation, author, sent_at_ms,
/// body)` but *not* the log `seq` — the nest allocates that afterwards — so
/// the identical envelope bytes, re-appended by any member or by the home
/// nest itself, open and verify as that author's op again at a later
/// position. Under a fold that took the last op **in log order**, re-appending
/// somebody's already-retracted `Add` put their reaction back on every seat,
/// under their own valid signature.
///
/// The stamp is what the signature covers, so ordering by it is ordering by
/// something a replay cannot move. See [`fold_reactions`] for the rule and
/// the two skews it cannot see.
///
/// **Every event carries one.** The stamp-less `(actor, emoji, op)` shape a
/// replica slice once carried beside this one — a downgrade mirror kept only
/// so a pre-stamp build could read it — and the log-order fold such events
/// got were retired by the compat-remnant sweep (`version-compatibility.md`
/// § Dimension 2, program 4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StampedReactionEvent {
    /// The actor whose reaction this is — the **verified signed author** on
    /// the community class, the MLS-authenticated sender on the end-to-end
    /// class. Never a self-asserted field.
    pub reactor: ActorId,
    /// The emoji (Unicode scalar sequence).
    pub emoji: String,
    /// Add or retract.
    pub op: ReactionOp,
    /// The author's own stamp in **milliseconds**, as the community class's
    /// signature covers it
    /// (`fauna_mls::room_message::RoomMessageCore::sent_at_ms`); the
    /// end-to-end class divides its microsecond `ChannelMessage.timestamp`
    /// down to the same unit so one fold serves both (priority #2).
    /// Required at rest: an event without it is refused.
    pub sent_at_ms: i64,
}

impl StampedReactionEvent {
    /// A stamped event.
    pub fn new(reactor: ActorId, emoji: String, op: ReactionOp, sent_at_ms: i64) -> Self {
        Self {
            reactor,
            emoji,
            op,
            sent_at_ms,
        }
    }
}

/// The fixed quick-set of reaction emojis every app's
/// `dm-message-actions-menu` offers, in this exact order
/// (`docs/goal/ui/conversations.md` § Reactions & message delete — identical on
/// all apps; ui.yaml `dm-reaction-option`). The wire carries an arbitrary
/// emoji string, so this is the uniform must-have; the fuller picker
/// (`dm-reaction-more-button`) is the per-platform divergence.
///
/// **One definition for all 7 apps.** The Rust-native apps (linux, tui) use this
/// const directly; apple and android read it through [`quickset_emojis`] over UniFFI,
/// and web through the same function's wasm twin (`quicksetEmojis` in
/// `libs/fauna-wasm`). They used to mirror it in their own UI layers instead — four
/// hand-kept copies of a list whose *order* is part of the cross-app contract, so a
/// reorder here was silently a 4-app divergence with nothing to catch it
/// (priority #2/#4).
pub const QUICKSET_EMOJIS: [&str; 6] = [
    "\u{1f44d}",        // 👍
    "\u{2764}\u{fe0f}", // ❤️
    "\u{1f602}",        // 😂
    "\u{1f62e}",        // 😮
    "\u{1f622}",        // 😢
    "\u{1f64f}",        // 🙏
];

/// FFI face of [`QUICKSET_EMOJIS`] — the quick-set, in order, for the apps that
/// reach shared Rust across a binding rather than as a crate dep (apple, android,
/// windows; web via the wasm twin).
///
/// A `const` array of `&str` has no UniFFI representation, so the face is a
/// function returning owned `String`s — the same shape [`rail_glyph`](crate::rail_glyph)
/// takes for the other cross-binding constant-ish projection. Ordering is the
/// contract, not just the membership: `dm-reaction-option` is an indexed element,
/// so an e2e that taps index 0 is asserting "👍" on every app.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn quickset_emojis() -> Vec<String> {
    QUICKSET_EMOJIS.iter().map(|e| (*e).to_string()).collect()
}

/// The twenty-emoji **shortcut grid** web and windows show beside the fuller
/// picker's free-entry field (`docs/goal/ui/conversations.md` § Reactions & message
/// delete → *Rendering / picker glue*). The field is what reaches **any** emoji; the
/// grid is a one-click shortcut to common ones, so it leads with the quick-set in
/// its own order. Its cells carry no ui.yaml id (they must never inflate the fixed-6
/// `dm-reaction-option` count), so the order here is presentation, not an e2e contract
/// — but it is still one list, not a copy per app (priority #2).
pub const MORE_GRID_EMOJIS: [&str; 20] = [
    "\u{1f44d}",        // 👍
    "\u{2764}\u{fe0f}", // ❤️
    "\u{1f602}",        // 😂
    "\u{1f62e}",        // 😮
    "\u{1f622}",        // 😢
    "\u{1f64f}",        // 🙏
    "\u{1f389}",        // 🎉
    "\u{1f525}",        // 🔥
    "\u{1f44f}",        // 👏
    "\u{1f64c}",        // 🙌
    "\u{1f60d}",        // 😍
    "\u{1f914}",        // 🤔
    "\u{1f60e}",        // 😎
    "\u{1f621}",        // 😡
    "\u{1f440}",        // 👀
    "\u{2705}",         // ✅
    "\u{274c}",         // ❌
    "\u{1f4af}",        // 💯
    "\u{1f680}",        // 🚀
    "\u{2b50}",         // ⭐
];

/// FFI face of [`MORE_GRID_EMOJIS`] — the [`quickset_emojis`] shape, for windows over
/// UniFFI and web through the wasm twin (`moreGridEmojis` in `libs/fauna-wasm`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn more_grid_emojis() -> Vec<String> {
    MORE_GRID_EMOJIS.iter().map(|e| (*e).to_string()).collect()
}

/// Where one event ranks against the other events for the **same**
/// `(reactor, emoji)`. The highest rank wins that pair; nothing else about
/// the sequence decides it.
///
/// Two components, and neither is the log position — position is precisely
/// what a replay controls:
///
/// 1. **The author's signed stamp.**
/// 2. **`Remove` over `Add` at an equal stamp** — see [`fold_reactions`].
type Rank = (i64, u8);

/// The op currently winning one `(reactor, emoji)` pair, and the [`rank`] it
/// won with. Only the winner is carried: the fold's answer is a function of
/// the set of ops, so a beaten one has no further say.
struct Winner<'a> {
    reactor: ActorId,
    emoji: &'a str,
    op: &'a ReactionOp,
    rank: Rank,
}

fn rank(ev: &StampedReactionEvent) -> Rank {
    (
        ev.sent_at_ms,
        match ev.op {
            // An op this build does not name ranks as a retraction — the
            // restrictive reading: at an equal stamp it never lets an `Add`
            // win, and it never counts as one.
            ReactionOp::Remove | ReactionOp::Other(_) => 1,
            ReactionOp::Add => 0,
        },
    )
}

/// Fold a sequence of reaction add/remove events into per-emoji aggregates.
///
/// The output is ordered by **first-add appearance** in `events`
/// (`../ui/conversations.md` § Reactions & message delete). `count` is the
/// number of distinct actors whose *winning* op for that emoji is
/// [`ReactionOp::Add`]; `reacted_by_me` is true when `me` is among them.
/// Emojis whose reactor set ends empty are dropped.
///
/// # Which op wins a `(reactor, emoji)` pair
///
/// **The one with the highest [`rank`] — never "the last one in the log".**
/// That is the author's own signed `sent_at_ms`, with `Remove` ranked over
/// `Add` at an equal stamp.
///
/// ⚠ **The outcome is a pure function of the SET of distinct stamped ops**,
/// and that — not last-writer-wins as such — is what defeats replay. A
/// community room's op carries no `seq` inside its signature
/// (`community-rooms.md` § The three classes → *Community* → *Who wrote it*),
/// so any member or the home nest can re-append an author's own bytes at a
/// later position; a fold that read multiplicity or position from the log
/// would let a re-appended `Add` bury the retraction that followed it. Neither
/// "break the tie by `seq`" nor "break it by log position" would have closed
/// that: **a replay always lands later**, so on the equal stamp it shares with
/// its original either rule hands it the win — which is why the tie goes to
/// `Remove`. The stamp is millisecond-resolution
/// (`fauna_mls::room_message`, `Timestamp::now_millis`), so an add-then-retract
/// inside one millisecond is ordinary rather than exotic, and the tie rule is
/// load-bearing rather than decorative. Ranking `Remove` first also fails
/// **safe**: the state a tie resolves to is the absence of the reaction.
///
/// # The two skews this cannot see
///
/// - **A reactor's own clock running backwards**, and only its own: the rank
///   is compared within one `(target, reactor, emoji)` pair, and every op in
///   such a pair is signed by that one reactor. So no other member's clock —
///   honest, broken or hostile — can touch this reactor's outcome. What can
///   is that reactor's *own* later device stamping its retraction behind its
///   own earlier add; the retraction then loses until its next gesture.
/// - **A double-toggle inside one millisecond** — `Remove`, then `Add`, both
///   at the same stamp — resolves to `Remove`, so a re-add that fast is not
///   seen until the next gesture carries a later stamp. A tap pair inside one
///   millisecond is not a gesture a person makes.
pub fn fold_reactions(events: &[StampedReactionEvent], me: ActorId) -> Vec<ReactionGroup> {
    // The winning op per (reactor, emoji), and its rank. Linear scans
    // throughout: the per-message reaction count is small, and this keeps the
    // fold dependency-free and ordering-explicit.
    let mut winners: Vec<Winner<'_>> = Vec::new();
    // Emoji groups in first-ADD appearance order — the display contract. A
    // replay can never move a first appearance EARLIER (its original is
    // already in the log ahead of it), so this stays stable under replay even
    // though it reads the sequence rather than the set.
    let mut order: Vec<&str> = Vec::new();

    for ev in events {
        if matches!(ev.op, ReactionOp::Add) && !order.iter().any(|e| *e == ev.emoji) {
            order.push(&ev.emoji);
        }
        let r = rank(ev);
        match winners
            .iter_mut()
            .find(|w| w.reactor == ev.reactor && w.emoji == ev.emoji)
        {
            Some(held) if r > held.rank => {
                held.op = &ev.op;
                held.rank = r;
            }
            Some(_) => {}
            None => winners.push(Winner {
                reactor: ev.reactor,
                emoji: &ev.emoji,
                op: &ev.op,
                rank: r,
            }),
        }
    }

    order
        .into_iter()
        .filter_map(|emoji| {
            let reactors: HashSet<ActorId> = winners
                .iter()
                .filter(|w| w.emoji == emoji && matches!(w.op, ReactionOp::Add))
                .map(|w| w.reactor)
                .collect();
            (!reactors.is_empty()).then(|| ReactionGroup {
                count: reactors.len() as u32,
                reacted_by_me: reactors.contains(&me),
                emoji: emoji.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn a(n: u8) -> ActorId {
        ActorId([n; 32])
    }

    #[test]
    fn the_ffi_face_is_the_const_in_order() {
        // The face is what the FFI apps paint `dm-reaction-option` from, so it must
        // agree with the const the Rust-native apps use — same members AND same order
        // (the element is indexed; index 0 is 👍 on all 7 apps).
        assert_eq!(quickset_emojis(), QUICKSET_EMOJIS);
        assert_eq!(quickset_emojis()[0], "\u{1f44d}");
        assert_eq!(quickset_emojis().len(), 6);
    }

    #[test]
    fn the_more_grid_face_is_the_const_in_order_and_opens_with_the_quick_set() {
        // The shortcut grid web and windows paint beside the fuller picker's
        // free-entry field: the FFI/wasm face must agree with the const, and the
        // grid leads with the quick-set so the six stay where a hand expects them.
        assert_eq!(more_grid_emojis(), MORE_GRID_EMOJIS);
        assert_eq!(more_grid_emojis().len(), 20);
        assert_eq!(&more_grid_emojis()[..6], &quickset_emojis()[..]);
        let mut seen = std::collections::HashSet::new();
        assert!(more_grid_emojis().iter().all(|e| seen.insert(e.clone())));
    }

    /// A stamped event; `t` is the author's signed millisecond stamp.
    fn ev(n: u8, emoji: &str, op: ReactionOp, t: i64) -> StampedReactionEvent {
        StampedReactionEvent::new(a(n), emoji.to_string(), op, t)
    }

    fn pills(groups: &[ReactionGroup]) -> Vec<(&str, u32, bool)> {
        groups
            .iter()
            .map(|g| (g.emoji.as_str(), g.count, g.reacted_by_me))
            .collect()
    }

    #[test]
    fn folds_add_remove_dedup_and_self() {
        let me = a(1);
        let events = vec![
            ev(1, "👍", ReactionOp::Add, 10),
            ev(2, "👍", ReactionOp::Add, 20),
            ev(1, "👍", ReactionOp::Add, 30), // idempotent
            ev(2, "❤️", ReactionOp::Add, 40),
            ev(1, "👍", ReactionOp::Remove, 50), // me un-reacts 👍
        ];
        assert_eq!(
            pills(&fold_reactions(&events, me)),
            vec![("👍", 1, false), ("❤️", 1, false)]
        );
    }

    /// ⚠ The replay property, at the level of the fold itself: re-appending an
    /// op the log already holds — at ANY later position, any number of times —
    /// cannot change the outcome, because the outcome reads the set of ops and
    /// not the sequence. The end-to-end proof over real sealed bytes is
    /// `fauna_mls_backend_tests.rs`'s
    /// `a_replayed_reaction_op_cannot_override_a_later_retraction`.
    #[test]
    fn re_appending_an_op_the_log_already_holds_changes_nothing() {
        let me = a(9);
        let add = ev(2, "👎", ReactionOp::Add, 100);
        let retract = ev(2, "👎", ReactionOp::Remove, 200);

        let honest = vec![add.clone(), retract.clone()];
        assert!(
            fold_reactions(&honest, me).is_empty(),
            "the retraction stands on the honest log"
        );

        for replayed in [
            vec![add.clone(), retract.clone(), add.clone()],
            vec![add.clone(), retract.clone(), add.clone(), add.clone()],
            vec![add.clone(), add.clone(), retract.clone(), add.clone()],
            vec![add.clone(), retract.clone(), retract.clone(), add.clone()],
        ] {
            assert!(
                fold_reactions(&replayed, me).is_empty(),
                "a re-appended op must not resurrect a retracted reaction"
            );
        }
    }

    /// An add-then-retract inside ONE millisecond is ordinary at this stamp's
    /// resolution, and it is exactly where a `seq`/position tie-break would
    /// have handed the replay the win. `Remove` takes the tie.
    #[test]
    fn a_retraction_at_the_same_stamp_beats_the_add_it_retracts() {
        let me = a(9);
        let add = ev(2, "👎", ReactionOp::Add, 100);
        let retract = ev(2, "👎", ReactionOp::Remove, 100);
        assert!(fold_reactions(&[add.clone(), retract.clone()], me).is_empty());
        assert!(
            fold_reactions(&[add.clone(), retract, add], me).is_empty(),
            "and the replay of the add still loses at the equal stamp"
        );
    }

    /// Display order is first-ADD appearance, and a replay cannot move one
    /// earlier — the original always sits ahead of its own copy.
    #[test]
    fn groups_keep_first_add_appearance_order() {
        let me = a(1);
        let events = vec![
            ev(1, "❤️", ReactionOp::Add, 10),
            ev(2, "👍", ReactionOp::Add, 20),
            ev(1, "❤️", ReactionOp::Add, 30), // a replay of the first
        ];
        assert_eq!(
            pills(&fold_reactions(&events, me)),
            vec![("❤️", 1, true), ("👍", 1, false)]
        );
    }

    /// A stamp-less event — the `(actor, emoji, op)` shape a pre-stamp build
    /// logged — is refused at rest, never folded by log order. The stamp-less
    /// shape and its fold were retired by the compat-remnant sweep
    /// (`version-compatibility.md` § Dimension 2, program 4).
    #[test]
    fn a_stamp_less_event_is_refused_at_rest() {
        #[derive(Serialize)]
        struct StampLess {
            reactor: ActorId,
            emoji: String,
            op: ReactionOp,
        }
        let bytes = fauna_core::encoding::canonical_encode(&StampLess {
            reactor: a(1),
            emoji: "👍".to_string(),
            op: ReactionOp::Add,
        })
        .expect("encode");
        assert!(
            fauna_core::encoding::canonical_decode::<StampedReactionEvent>(&bytes).is_err(),
            "an event without its author's stamp must not decode"
        );

        let stamped = ev(1, "👍", ReactionOp::Add, 7);
        let round: StampedReactionEvent = fauna_core::encoding::canonical_decode(
            &fauna_core::encoding::canonical_encode(&stamped).expect("encode"),
        )
        .expect("decode");
        assert_eq!(round, stamped);
    }
}
