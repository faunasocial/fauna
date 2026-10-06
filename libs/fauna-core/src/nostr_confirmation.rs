//! The Nostr key-succession aftermath's **npub confirmation stamp** — the
//! value of the `fauna.state.nostr-confirmation` account-plane kind
//! (`config-dissolution.md`, the kinds table's row) and its merge rule
//! (before the `__config` rail retired at closure step (6), the stamp lived on
//! the whole-record blob; see `docs/goal/architecture/config-dissolution.md`).
//!
//! **One row per account, at [`NOSTR_CONFIRMATION_ROW_KEY`]**: a single
//! monotone stamp, bounded by its shape (*Bounded rows*). The join is
//! **max** — `Option<i64>`'s derived `Ord` sorts `None` below every `Some`, so
//! a later confirmation on either device always wins and an unconfirmed side
//! never clobbers a confirmed one. Nothing here is a thief-plantable row: the
//! sole writer is the owner's own "yes, that's my npub" gesture
//! (`fauna_client_config::nostr_npub_confirm`), so a plain max needs no
//! adjudication.
//!
//! **Strict decode** (the CrdtPerField posture, `deny_unknown_fields`): a
//! field a newer writer added makes the plane arm answer `BadValue`, and the
//! entry re-presents on the next reconcile once this build learns it.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The one logical key a `fauna.state.nostr-confirmation` row lives at.
pub const NOSTR_CONFIRMATION_ROW_KEY: &str = "self";

/// When the owner last confirmed the linked npub, in epoch seconds — `None`
/// when they never have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NostrConfirmation {
    /// Epoch seconds of the latest confirmation.
    pub confirmed_at: Option<i64>,
}

impl NostrConfirmation {
    /// The join: the later confirmation (`None` below every `Some`). The one
    /// statement of the rule, which the plane arm runs.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        Self {
            confirmed_at: std::cmp::max(self.confirmed_at, other.confirmed_at),
        }
    }

    /// The row's canonical dag-cbor bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        crate::encoding::canonical_encode(self)
    }
}

/// Decode one `fauna.state.nostr-confirmation` row: the key must be
/// [`NOSTR_CONFIRMATION_ROW_KEY`] and the value must decode strictly (an
/// unknown field is refused).
pub fn decode_nostr_confirmation_row(key: &str, value: &[u8]) -> Result<NostrConfirmation> {
    if key != NOSTR_CONFIRMATION_ROW_KEY {
        return Err(Error::Encoding(format!(
            "not a nostr-confirmation key: {key:?}"
        )));
    }
    crate::encoding::canonical_decode(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(v: Option<i64>) -> NostrConfirmation {
        NostrConfirmation { confirmed_at: v }
    }

    #[test]
    fn the_later_confirmation_wins_and_none_never_clobbers() {
        assert_eq!(at(Some(5)).merge(&at(Some(9))), at(Some(9)));
        assert_eq!(at(Some(9)).merge(&at(Some(5))), at(Some(9)));
        assert_eq!(at(None).merge(&at(Some(5))), at(Some(5)));
        assert_eq!(at(Some(5)).merge(&at(None)), at(Some(5)));
        assert_eq!(at(None).merge(&at(None)), at(None));
    }

    #[test]
    fn a_row_round_trips_and_refuses_another_key() {
        let bytes = at(Some(42)).encode().unwrap();
        assert_eq!(
            decode_nostr_confirmation_row(NOSTR_CONFIRMATION_ROW_KEY, &bytes).unwrap(),
            at(Some(42))
        );
        assert!(decode_nostr_confirmation_row("other", &bytes).is_err());
    }
}
