//! The **whole-record latest-wins** rule every stamped join in the crate
//! shares — the backup state (`backup_state`), the mail config's rotation arms
//! (`data`) and the plane's whole-record kinds (`mail_rows` and the
//! account-plane row modules). The rule itself is pure and needs nothing but
//! the canonical encoding's tiebreak key.

use std::cmp::Ordering;

use crate::data::Timestamp;
use crate::encoding::canonical_tiebreak_key as tiebreak_key;

/// Does the `their` side win a **whole-record latest-wins** field?
///
/// Strictly-newer stamp wins; an equal instant is broken by the side with
/// the smaller
/// [`canonical_tiebreak_key`](crate::encoding::canonical_tiebreak_key). The old
/// rule broke that tie toward "ours", which is a *local* preference — the one
/// thing a join cannot have, since each replica then answers the question
/// differently and the two never compare equal again.
///
/// Selecting the max of `(updated_at, Reverse(tiebreak key))` is a max over a
/// total order, so the fold is commutative, associative and idempotent — and
/// because the key is computed per field, an unrelated field's difference can
/// never flip which side this one takes.
pub(crate) fn theirs_wins<T: serde::Serialize>(
    our: &T,
    our_at: Timestamp,
    their: &T,
    their_at: Timestamp,
) -> bool {
    match their_at.cmp(&our_at) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => tiebreak_key(their) < tiebreak_key(our),
    }
}

/// [`theirs_wins`] for values that are already totally ordered (key bytes,
/// strings) — same rule, without the encoding round-trip.
pub(crate) fn theirs_wins_ord<T: Ord + ?Sized>(
    our: &T,
    our_at: Timestamp,
    their: &T,
    their_at: Timestamp,
) -> bool {
    match their_at.cmp(&our_at) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => their < our,
    }
}
