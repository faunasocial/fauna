//! Pure conflict-resolution decision core (file-sync.md § Conflicts, ratified 2026-07-10).
//!
//! Every conflict auto-resolves immediately; the caller (the engine's apply path — the
//! only holder of base + local + incoming bytes) retains the losing version in version history before acting on the
//! outcome, so every arm here is risk-free by construction. A local loser is retained by the resolved report: the
//! nest mints it as an `is_retention` row the reporter signs (`writer-signed-change-records.md` ruling (10)(d)), and a
//! judged version history lists it by that signature.
//!
//! Decision rules:
//! - [`ConflictPolicy::Auto`] + a cached merge base + a merge-capable adapter +
//!   a **clean** three-way merge → [`ConflictResolution::Merged`]. A merge that would
//!   need conflict markers ([`MergeResult::Conflicts`]) is treated as unmergeable —
//!   markers are NEVER written into a user's file.
//! - Everything else (binary, no base, merge error, an adapter that declines the input as
//!   unaffordable, overlapping hunks, or
//!   [`ConflictPolicy::LatestWinsAlways`]) → latest-writer-wins by timestamp, with a
//!   deterministic content-hash tiebreak so two devices detecting the mirror-image
//!   conflict (A sees `local=x, incoming=y`; B sees `local=y, incoming=x`) pick the
//!   same winning *content* on equal stamps.
//!
//! This module is pure — no IO, no DB, no wire. Recording the loser, writing the
//! outcome, and reporting the resolved conflict are the caller's job (slice 3).

use fauna_core::format::{ConflictPolicy, FormatAdapter, MergeResult};

/// The auto-resolution outcome for one detected conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictResolution {
    /// A clean three-way merge succeeded: the merged bytes become the new head; both
    /// parents (local and incoming) are retained as versions.
    Merged(Vec<u8>),
    /// The local version is the latest writer: it becomes the head (the incoming
    /// version is already a recorded change, hence already retained).
    LocalWins,
    /// The incoming version is the latest writer: it is applied; the local version is
    /// uploaded first and retained as the resolved report's signed retention row.
    IncomingWins,
}

/// Resolve a detected conflict between the local file and an incoming version.
///
/// * `base` — the cached last-synced content for this path, if the caller has one.
///   Best-effort: after a both-offline divergence the single per-path slot may not be
///   the true common ancestor; a wrong-but-present base can only yield a *merge* whose
///   parents are both retained, so this stays safe.
/// * `local_modified_at_ms` / `incoming_created_at_ms` — wall-clock stamps for
///   latest-writer-wins (local file mtime vs. the incoming change's `created_at`).
///
/// **`spawn_blocking`, considered:** the call site —
/// `fauna_sync_engine::engine::SyncEngine::auto_resolve_conflict` — calls this synchronously from an `async fn`, with no
/// `spawn_blocking`. `fauna-core::format_text::POSITIONAL_MERGE_LINE_CEILING` bounds the
/// quadratic positional pass on EVERY path through `TextAdapter::merge`, not on one arm of
/// it: the gate sits immediately before `diff3_merge`, which has a single call site, so the
/// complement is empty. (It was scoped to the `both_extend_base` arm when first written, and
/// this sentence carried the unscoped conclusion anyway — the gap that made an ordinary
/// peer edit, one deleted line away from that arm, an unbounded synchronous stall.) A few
/// hundred ms at the ceiling, not hours, so `spawn_blocking` is deliberately NOT added here:
/// the remaining synchronous stall is small and capped. The bound belongs to
/// `TextAdapter`, not to this function — revisit if a raised ceiling, or a merge-capable
/// adapter carrying no such bound of its own, pushes it back up.
pub fn resolve_conflict(
    policy: ConflictPolicy,
    adapter: &dyn FormatAdapter,
    base: Option<&[u8]>,
    local: &[u8],
    local_modified_at_ms: i64,
    incoming: &[u8],
    incoming_created_at_ms: i64,
) -> ConflictResolution {
    if policy == ConflictPolicy::Auto
        && let Some(base) = base
        && adapter.can_merge(base, local, incoming)
        && let Ok(MergeResult::Merged(merged)) = adapter.merge(base, local, incoming)
    {
        return ConflictResolution::Merged(merged);
    }
    latest_writer_wins(
        local,
        local_modified_at_ms,
        incoming,
        incoming_created_at_ms,
    )
}

/// Timestamp comparison with a deterministic, symmetric content-hash tiebreak.
fn latest_writer_wins(
    local: &[u8],
    local_modified_at_ms: i64,
    incoming: &[u8],
    incoming_created_at_ms: i64,
) -> ConflictResolution {
    match local_modified_at_ms.cmp(&incoming_created_at_ms) {
        std::cmp::Ordering::Greater => ConflictResolution::LocalWins,
        std::cmp::Ordering::Less => ConflictResolution::IncomingWins,
        std::cmp::Ordering::Equal => {
            // Equal stamps: the lexicographically greater content hash wins. Symmetric
            // across the two mirror-detecting devices, so both converge on one content.
            if blake3::hash(local).as_bytes() > blake3::hash(incoming).as_bytes() {
                ConflictResolution::LocalWins
            } else {
                ConflictResolution::IncomingWins
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::format::OpaqueAdapter;

    // Timestamps: LOCAL_NEWER > INCOMING_T, LOCAL_OLDER < INCOMING_T.
    const INCOMING_T: i64 = 1_700_000_100_000;
    const LOCAL_NEWER: i64 = 1_700_000_200_000;
    const LOCAL_OLDER: i64 = 1_700_000_000_000;

    #[cfg(feature = "format_text")]
    mod text {
        use super::*;
        use fauna_core::format_text::TextAdapter;

        const BASE: &[u8] = b"alpha\nbravo\ncharlie\n";
        // Local inserts a line at the top; incoming appends one at the bottom —
        // non-overlapping, cleanly mergeable.
        const LOCAL_CLEAN: &[u8] = b"zero\nalpha\nbravo\ncharlie\n";
        const INCOMING_CLEAN: &[u8] = b"alpha\nbravo\ncharlie\ndelta\n";

        #[test]
        fn clean_merge_returns_merged_with_both_edits() {
            let out = resolve_conflict(
                ConflictPolicy::Auto,
                &TextAdapter,
                Some(BASE),
                LOCAL_CLEAN,
                LOCAL_OLDER,
                INCOMING_CLEAN,
                INCOMING_T,
            );
            let ConflictResolution::Merged(merged) = out else {
                panic!("expected Merged, got {out:?}");
            };
            let text = String::from_utf8(merged).unwrap();
            assert!(text.contains("zero"), "local edit survives: {text}");
            assert!(text.contains("delta"), "incoming edit survives: {text}");
            assert!(!text.contains("<<<<<<<"), "never markers: {text}");
        }

        #[test]
        fn overlapping_hunks_fall_to_latest_wins_never_markers() {
            // Both sides replace the same base line differently — overlapping.
            let local = b"alpha\nlocal-edit\ncharlie\n";
            let incoming = b"alpha\nincoming-edit\ncharlie\n";
            let out = resolve_conflict(
                ConflictPolicy::Auto,
                &TextAdapter,
                Some(BASE),
                local,
                LOCAL_NEWER,
                incoming,
                INCOMING_T,
            );
            assert_eq!(out, ConflictResolution::LocalWins);
            let out = resolve_conflict(
                ConflictPolicy::Auto,
                &TextAdapter,
                Some(BASE),
                local,
                LOCAL_OLDER,
                incoming,
                INCOMING_T,
            );
            assert_eq!(out, ConflictResolution::IncomingWins);
        }

        #[test]
        fn no_base_falls_to_latest_wins_even_for_text() {
            let out = resolve_conflict(
                ConflictPolicy::Auto,
                &TextAdapter,
                None,
                LOCAL_CLEAN,
                LOCAL_NEWER,
                INCOMING_CLEAN,
                INCOMING_T,
            );
            assert_eq!(out, ConflictResolution::LocalWins);
        }

        #[test]
        fn latest_wins_always_skips_the_merge_attempt() {
            // Cleanly mergeable inputs, but the per-set policy says never merge.
            let out = resolve_conflict(
                ConflictPolicy::LatestWinsAlways,
                &TextAdapter,
                Some(BASE),
                LOCAL_CLEAN,
                LOCAL_OLDER,
                INCOMING_CLEAN,
                INCOMING_T,
            );
            assert_eq!(out, ConflictResolution::IncomingWins);
        }

        #[test]
        fn invalid_utf8_falls_to_latest_wins() {
            // can_merge() rejects non-UTF-8 inputs even for a text extension.
            let local: &[u8] = &[0xff, 0xfe, 0x00, 0x01];
            let out = resolve_conflict(
                ConflictPolicy::Auto,
                &TextAdapter,
                Some(BASE),
                local,
                LOCAL_OLDER,
                INCOMING_CLEAN,
                INCOMING_T,
            );
            assert_eq!(out, ConflictResolution::IncomingWins);
        }

        /// The consequence, at this layer, of the text adapter's affordability gate
        /// covering the whole call: an oversized divergence
        /// that is NOT a pure-append pair on both sides is declined by the adapter, and a
        /// declining adapter lands on latest-writer-wins — the same arm binary files,
        /// a missing base and overlapping hunks already take, with the loser retained.
        ///
        /// One deleted line is the whole difference from the `both_extend_base` arm,
        /// which stays merged (via its linear union) at any size; the peer supplies the
        /// incoming side, so this is the shape a peer can choose freely.
        #[test]
        fn oversized_non_append_divergence_falls_to_latest_wins_both_directions() {
            let n = 5_000; // comfortably past POSITIONAL_MERGE_LINE_CEILING
            let base_lines: Vec<String> = (0..n).map(|i| format!("line{i}")).collect();
            let base = format!("{}\n", base_lines.join("\n"));
            // Local DELETES line0 (so neither side is a pure append) and adds its own.
            let local = format!("{}\nLOCAL_EXTRA\n", base_lines[1..].join("\n"));
            let incoming = format!("{}INCOMING_EXTRA\n", base);

            for (local_ms, expected) in [
                (LOCAL_NEWER, ConflictResolution::LocalWins),
                (LOCAL_OLDER, ConflictResolution::IncomingWins),
            ] {
                let out = resolve_conflict(
                    ConflictPolicy::Auto,
                    &TextAdapter,
                    Some(base.as_bytes()),
                    local.as_bytes(),
                    local_ms,
                    incoming.as_bytes(),
                    INCOMING_T,
                );
                assert_eq!(
                    out, expected,
                    "an oversized non-append divergence must resolve latest-writer-wins, \
                     not merge"
                );
            }

            // Control: the SAME size, both sides pure appends, still merges — so this
            // pins the declined arm and not merely "big inputs never merge".
            let local_append = format!("{}LOCAL_EXTRA\n", base);
            let out = resolve_conflict(
                ConflictPolicy::Auto,
                &TextAdapter,
                Some(base.as_bytes()),
                local_append.as_bytes(),
                LOCAL_NEWER,
                incoming.as_bytes(),
                INCOMING_T,
            );
            let ConflictResolution::Merged(merged) = out else {
                panic!("both_extend_base still merges above the ceiling, got {out:?}");
            };
            let text = String::from_utf8(merged).unwrap();
            assert!(
                text.contains("LOCAL_EXTRA") && text.contains("INCOMING_EXTRA"),
                "the union arm keeps both novelties"
            );
        }
    }

    #[test]
    fn binary_adapter_resolves_latest_wins_both_directions() {
        let out = resolve_conflict(
            ConflictPolicy::Auto,
            &OpaqueAdapter,
            Some(b"base"),
            b"local",
            LOCAL_NEWER,
            b"incoming",
            INCOMING_T,
        );
        assert_eq!(out, ConflictResolution::LocalWins);
        let out = resolve_conflict(
            ConflictPolicy::Auto,
            &OpaqueAdapter,
            Some(b"base"),
            b"local",
            LOCAL_OLDER,
            b"incoming",
            INCOMING_T,
        );
        assert_eq!(out, ConflictResolution::IncomingWins);
    }

    #[test]
    fn equal_timestamps_tiebreak_is_deterministic_and_symmetric() {
        let x = b"content-x";
        let y = b"content-y";
        // Device A: local=x, incoming=y. Device B (mirror): local=y, incoming=x.
        let a = resolve_conflict(
            ConflictPolicy::Auto,
            &OpaqueAdapter,
            None,
            x,
            INCOMING_T,
            y,
            INCOMING_T,
        );
        let b = resolve_conflict(
            ConflictPolicy::Auto,
            &OpaqueAdapter,
            None,
            y,
            INCOMING_T,
            x,
            INCOMING_T,
        );
        // Both devices must converge on the same winning CONTENT.
        let a_winner: &[u8] = match a {
            ConflictResolution::LocalWins => x,
            ConflictResolution::IncomingWins => y,
            ConflictResolution::Merged(_) => panic!("opaque cannot merge"),
        };
        let b_winner: &[u8] = match b {
            ConflictResolution::LocalWins => y,
            ConflictResolution::IncomingWins => x,
            ConflictResolution::Merged(_) => panic!("opaque cannot merge"),
        };
        assert_eq!(a_winner, b_winner);
    }
}
