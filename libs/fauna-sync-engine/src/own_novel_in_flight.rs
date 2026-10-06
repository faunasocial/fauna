//! The IN-FLIGHT GUARD (`conflicts.md` clause 5, 2026-08-03): paths whose
//! local content holds NOVEL published content whose own row has not yet
//! echoed back — armed by the resolved-report outcomes and by the apply pass
//! at partition time for own non-resolution rows; cleared when an own row's
//! echo matches local. While armed, the licence's settled conjunct is false:
//! no fast-forward/adopt fires and divergences merge — the safe direction.
//!
//! Per-carrier since the same-anchor ruling (2026-08-05, conjunct 4): a
//! single per-path bit let an older report's rows clear the window while a
//! newer report's novelty-carrying winner was still unlisted, releasing the
//! covering adopt over it. A RETENTION row retires nothing (the loser is not
//! the carrier — the same-anchor ruling, conjunct 4).
//!
//! Held by [`crate::engine::SyncEngine`]; lifted into its own module while
//! the since-removed headless daemon independently carried this exact
//! mechanism (same field shape, same three operations).

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct OwnNovelInFlight {
    inner: Mutex<HashMap<String, Vec<String>>>,
}

impl OwnNovelInFlight {
    pub fn note(&self, relative_path: &str, manifest_hex: &str) {
        self.inner
            .lock()
            .unwrap()
            .entry(relative_path.to_string())
            .or_default()
            .push(manifest_hex.to_string());
    }

    /// Is an own novel publication still awaiting its listing on this path?
    pub fn is_in_flight(&self, relative_path: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(relative_path)
            .is_some_and(|v| !v.is_empty())
    }

    /// Retire ONE carrier from the path's in-flight window, by manifest —
    /// called when the carrier's row lists (the partition/catch-up pre-pass)
    /// or echoes.
    pub fn retire(&self, relative_path: &str, manifest_hex: &str) {
        let mut map = self.inner.lock().unwrap();
        if let Some(v) = map.get_mut(relative_path) {
            if let Some(k) = v.iter().position(|m| m == manifest_hex) {
                v.remove(k);
            }
            if v.is_empty() {
                map.remove(relative_path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_path_is_not_in_flight() {
        let g = OwnNovelInFlight::default();
        assert!(!g.is_in_flight("a"));
    }

    #[test]
    fn noting_a_carrier_arms_the_path() {
        let g = OwnNovelInFlight::default();
        g.note("a", "hex1");
        assert!(g.is_in_flight("a"));
    }

    #[test]
    fn retiring_the_only_carrier_disarms_the_path() {
        let g = OwnNovelInFlight::default();
        g.note("a", "hex1");
        g.retire("a", "hex1");
        assert!(!g.is_in_flight("a"));
    }

    #[test]
    fn retiring_one_of_several_carriers_leaves_the_path_armed() {
        let g = OwnNovelInFlight::default();
        g.note("a", "hex1");
        g.note("a", "hex2");
        g.retire("a", "hex1");
        assert!(g.is_in_flight("a"));
    }

    #[test]
    fn retiring_an_unknown_manifest_is_a_no_op() {
        let g = OwnNovelInFlight::default();
        g.note("a", "hex1");
        g.retire("a", "not-the-carrier");
        assert!(g.is_in_flight("a"));
    }

    #[test]
    fn retiring_on_an_unarmed_path_is_a_no_op() {
        let g = OwnNovelInFlight::default();
        g.retire("never-armed", "hex1");
        assert!(!g.is_in_flight("never-armed"));
    }

    #[test]
    fn paths_are_tracked_independently() {
        let g = OwnNovelInFlight::default();
        g.note("a", "hex1");
        assert!(g.is_in_flight("a"));
        assert!(!g.is_in_flight("b"));
    }
}
