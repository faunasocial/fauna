//! The conversation read marker — the value of one `fauna.state.read-marker`
//! entry (`docs/goal/behavior/conversation-read-state.md` § The read-marker
//! record).
//!
//! One entry per fauna-native conversation channel, keyed
//! [`channel_key`]; the value says how far the user has read in it. This is
//! deliberately *not* the seen-set ([`crate::seen_set`]): that records what
//! the account has observed and feeds custody, this records what the user has
//! read and feeds a badge.
//!
//! The merge is a **max-register**: [`ReadMarker::join`] keeps the higher
//! position, so a marker only ever moves forward and a device holding a stale
//! value can never re-present read messages as unread on the others. Nothing
//! here can lower it — "mark unread again", if it is ever wanted, is a sibling
//! kind, never a change to this frozen shape.

use serde::{Deserialize, Serialize};

/// The rail keyspace prefix of a fauna-native channel's entry. A later rail
/// joins the same kind under a prefix of its own.
pub const CHANNEL_KEY_PREFIX: &str = "conv:";

/// The logical key of the entry for the channel whose id is
/// `channel_id_hex` — `conv:<hex>`, lowercased so every device spells it the
/// same way whatever case it holds the id in.
pub fn channel_key(channel_id_hex: &str) -> String {
    format!(
        "{CHANNEL_KEY_PREFIX}{}",
        channel_id_hex.to_ascii_lowercase()
    )
}

/// The channel id (hex) an entry key names, or `None` for a key outside the
/// fauna-native keyspace — another rail's, which this build leaves alone.
pub fn channel_of_key(key: &str) -> Option<&str> {
    key.strip_prefix(CHANNEL_KEY_PREFIX)
}

/// How far the user has read in one channel: every message whose channel
/// `seq` is at or below [`Self::through`] is read. The default — `0` — is
/// "nothing read", which is also what an absent entry means.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadMarker {
    pub through: u64,
}

impl ReadMarker {
    pub fn new(through: u64) -> Self {
        Self { through }
    }

    /// Whether the message at channel `seq` is read under this marker.
    pub fn covers(&self, seq: u64) -> bool {
        seq <= self.through
    }

    /// Raise the marker to `through`; whether that moved it. Never lowers.
    pub fn raise(&mut self, through: u64) -> bool {
        if through <= self.through {
            return false;
        }
        self.through = through;
        true
    }

    /// The join: the higher position. Commutative, associative, idempotent
    /// (`max` over a total order), so two replicas converge whichever merges.
    pub fn join(&self, other: &Self) -> Self {
        Self {
            through: self.through.max(other.through),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{canonical_decode, canonical_encode};

    const SAMPLES: [u64; 5] = [0, 1, 7, 41, u64::MAX];

    #[test]
    fn join_is_commutative_associative_idempotent() {
        for a in SAMPLES.map(ReadMarker::new) {
            assert_eq!(a.join(&a), a);
            for b in SAMPLES.map(ReadMarker::new) {
                assert_eq!(a.join(&b), b.join(&a));
                for c in SAMPLES.map(ReadMarker::new) {
                    assert_eq!(a.join(&b).join(&c), a.join(&b.join(&c)));
                }
            }
        }
    }

    #[test]
    fn join_never_lowers_either_side() {
        for a in SAMPLES.map(ReadMarker::new) {
            for b in SAMPLES.map(ReadMarker::new) {
                let joined = a.join(&b);
                assert!(joined.through >= a.through && joined.through >= b.through);
            }
        }
    }

    #[test]
    fn raise_moves_forward_only() {
        let mut marker = ReadMarker::new(7);
        assert!(!marker.raise(7));
        assert!(!marker.raise(3));
        assert_eq!(marker.through, 7);
        assert!(marker.raise(8));
        assert_eq!(marker.through, 8);
    }

    #[test]
    fn covers_is_inclusive_and_the_default_covers_nothing_real() {
        let marker = ReadMarker::new(7);
        assert!(marker.covers(7));
        assert!(!marker.covers(8));
        // Channel seqs start at 1, so an absent entry reads nothing.
        assert!(!ReadMarker::default().covers(1));
    }

    #[test]
    fn canonical_round_trip_and_unknown_fields_are_refused() {
        let marker = ReadMarker::new(41);
        let bytes = canonical_encode(&marker).unwrap();
        assert_eq!(canonical_decode::<ReadMarker>(&bytes).unwrap(), marker);

        #[derive(Serialize)]
        struct Wider {
            through: u64,
            extra: u64,
        }
        let wider = canonical_encode(&Wider {
            through: 41,
            extra: 1,
        })
        .unwrap();
        assert!(canonical_decode::<ReadMarker>(&wider).is_err());
    }

    #[test]
    fn the_channel_key_is_case_stable_and_round_trips() {
        assert_eq!(channel_key("AB12"), "conv:ab12");
        assert_eq!(channel_of_key(&channel_key("AB12")), Some("ab12"));
        assert_eq!(channel_of_key("mail:whatever"), None);
    }
}
