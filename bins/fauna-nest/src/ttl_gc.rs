//! Shared "drop expired entries" ceremony for the nest's small in-memory TTL
//! stores ([`crate::challenge_auth`]'s `ChallengeStore`, [`crate::token_store`]'s
//! `TokenStore`, [`crate::age_attest`]'s replay-nonce map): each hand-copied
//! the identical "count before, retain live entries, diff" `gc()` body,
//! differing only in the map's value shape and whether a grace period is
//! folded into the caller's cutoff.

use std::collections::HashMap;

/// Drop every `(k, v)` in `map` whose `expires_at(v)` is at or before
/// `cutoff`. Returns the number removed.
pub(crate) fn gc_before_cutoff<K, V>(
    map: &mut HashMap<K, V>,
    expires_at: impl Fn(&V) -> u64,
    cutoff: u64,
) -> usize {
    let before = map.len();
    map.retain(|_, v| expires_at(v) > cutoff);
    before - map.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_only_entries_at_or_before_the_cutoff() {
        let mut map: HashMap<u32, u64> = HashMap::new();
        map.insert(1, 100);
        map.insert(2, 200);
        map.insert(3, 300);
        let removed = gc_before_cutoff(&mut map, |v| *v, 200);
        assert_eq!(removed, 2);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&3), Some(&300));
    }

    #[test]
    fn an_empty_map_removes_nothing() {
        let mut map: HashMap<u32, u64> = HashMap::new();
        assert_eq!(gc_before_cutoff(&mut map, |v| *v, 0), 0);
    }

    #[test]
    fn works_over_a_struct_value_via_the_projection_closure() {
        struct Entry {
            expires_at: u64,
        }
        let mut map: HashMap<u32, Entry> = HashMap::new();
        map.insert(1, Entry { expires_at: 50 });
        map.insert(2, Entry { expires_at: 150 });
        let removed = gc_before_cutoff(&mut map, |e| e.expires_at, 100);
        assert_eq!(removed, 1);
        assert!(map.contains_key(&2));
    }
}
