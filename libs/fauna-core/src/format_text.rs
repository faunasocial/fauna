//! L1 format adapter: line-based 3-way merge for text files.

use anyhow::{Result, bail};

use crate::format::{FormatAdapter, MergeResult, SemanticChunk};

/// L1 text adapter: handles .txt, .md, .csv, and common source code files.
/// Uses line-based 3-way merge (similar to git's merge strategy).
pub struct TextAdapter;

/// Cost bound on the positional merge — the [`diff3_merge`] CALL, on every path into it
/// (`docs/goal/behavior/conflicts.md` § Concurrent resolution). `diff3_merge` runs
/// `similar::capture_diff_slices(Algorithm::Patience, ..)` twice, which degrades to its
/// Myers fallback — O(n·d), d ∝ n — on low line cardinality: measured 291ms at 4,000
/// lines, 16.69s at 32,000, ~4.8h fitted at 3 MiB.
///
/// Above this many lines on ANY of base/ours/theirs (`max`, never one side alone —
/// symmetric, so both seats resolving one divergence take the same arm), the positional
/// attempt is not made at all. What stands instead depends on the arm, and the two answers
/// are genuinely different:
///
/// - **`both_extend_base`** (both heads pure appends) → the linear [`line_multiset_union`],
///   already the ratified lossless answer for that arm regardless of size, so nothing is
///   traded but ordering fidelity on oversized inputs.
/// - **Anything else** → `merge` returns `Err`: the adapter DECLINES, and
///   `fauna_sync_engine::conflict_resolver::resolve_conflict` folds that into
///   latest-writer-wins with the loser retained. The union is not available here — with a
///   deletion on either side it would resurrect a line its author deleted, a file state no
///   author ever had.
///
/// The gate sits after the containment short-circuits (linear, and meaningful regardless of
/// size) and immediately before `diff3_merge`, whose single call site is what makes the
/// complement empty. It was scoped to the `both_extend_base` branch when first written,
/// which left an ordinary peer edit — one deleted line away from that arm — reaching the
/// quadratic pass unguarded.
const POSITIONAL_MERGE_LINE_CEILING: usize = 4096;

impl FormatAdapter for TextAdapter {
    fn extensions(&self) -> &[&str] {
        &[
            "txt", "md", "csv", "rs", "py", "js", "ts", "toml", "yaml", "yml", "json",
        ]
    }

    fn semantic_chunks(&self, content: &[u8]) -> Result<Option<Vec<SemanticChunk>>> {
        let text = std::str::from_utf8(content)?;
        let chunks: Vec<SemanticChunk> = text
            .lines()
            .enumerate()
            .map(|(i, line)| SemanticChunk {
                id: format!("L{i}"),
                data: line.as_bytes().to_vec(),
            })
            .collect();
        Ok(Some(chunks))
    }

    fn can_merge(&self, base: &[u8], ours: &[u8], theirs: &[u8]) -> bool {
        std::str::from_utf8(base).is_ok()
            && std::str::from_utf8(ours).is_ok()
            && std::str::from_utf8(theirs).is_ok()
    }

    fn merge(&self, base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<MergeResult> {
        let base_str = std::str::from_utf8(base)?;
        let ours_str = std::str::from_utf8(ours)?;
        let theirs_str = std::str::from_utf8(theirs)?;

        // Line-multiset containment pre-checks (the same-anchor ruling,
        // 2026-08-05, `conflicts.md` § Concurrent resolution) — active only
        // when BOTH sides extend the base (no deletions to honor, so
        // containment is meaningful). A side whose lines the other side
        // already contains adds nothing: the superset stands, byte-exact.
        // This is what a positional diff structurally cannot see — two
        // heads holding one line-set in different orders (earlier merge
        // rounds union ours-first per seat) read as moves, and a move is a
        // delete+insert that duplicates or drops on a deep ancestor. Order
        // across seats converges via nest-log supersession, as ratified —
        // this chooses no order, it only refuses to re-merge content.
        let base_ms = line_multiset(base_str);
        let ours_ms = line_multiset(ours_str);
        let theirs_ms = line_multiset(theirs_str);
        let both_extend_base =
            multiset_contains(&ours_ms, &base_ms) && multiset_contains(&theirs_ms, &base_ms);
        if both_extend_base {
            if multiset_contains(&ours_ms, &theirs_ms) {
                return Ok(MergeResult::Merged(ours.to_vec()));
            }
            if multiset_contains(&theirs_ms, &ours_ms) {
                return Ok(MergeResult::Merged(theirs.to_vec()));
            }
        }

        // Affordability gate — see POSITIONAL_MERGE_LINE_CEILING's doc. It guards the CALL,
        // not one arm: `diff3_merge` has exactly one call site and this is immediately
        // before it, so the complement is empty and no input reaches the quadratic pass
        // ungated. Above the ceiling the arm decides the answer, and only the
        // both-extending arm has a lossless linear one.
        let base_lines: usize = base_ms.values().sum();
        let ours_lines: usize = ours_ms.values().sum();
        let theirs_lines: usize = theirs_ms.values().sum();
        let widest = base_lines.max(ours_lines).max(theirs_lines);
        if widest > POSITIONAL_MERGE_LINE_CEILING {
            if both_extend_base {
                return Ok(MergeResult::Merged(line_multiset_union(
                    ours_str, theirs_str, ours_ms,
                )));
            }
            bail!(
                "text merge declined: {widest} lines exceeds the positional-merge ceiling of \
                 {POSITIONAL_MERGE_LINE_CEILING}, and this divergence is not a both-extending \
                 pair, so no linear lossless merge exists for it — latest-writer-wins stands, \
                 with the loser retained"
            );
        }

        let (merged, conflicts) = diff3_merge(base_str, ours_str, theirs_str);
        let merged_bytes = merged.into_bytes();

        // Both sides extend the base and each holds novelty the other lacks.
        // The POSITIONAL merge is preferred here and the line-multiset UNION
        // is its FALLBACK — that order is the whole correctness of this arm.
        //
        // The union answers a real defect (the same-anchor ruling, 2026-08-05):
        // two permuted supersets read to a positional diff as conflicting
        // replacements, so it fell to latest-wins and destroyed the losing
        // side's novelty. But the union was installed as a PRE-CHECK, ahead of
        // `diff3_merge`, so it also swallowed every case the positional merge
        // already handled cleanly — and it keeps ours' order, appending theirs'
        // uncovered lines at the END. On the ratified central case
        // (`conflicts.md` § Conflicts, ruled 2026-07-10: non-overlapping hunks
        // three-way merge from the cached ancestor) that silently relocates the
        // peer's inserted line to the bottom of the file, and it does so
        // ASYMMETRICALLY: with ours = the appending seat, base `hello/there`
        // plus `dog` against `cat` yields `hello/there/dog/cat`, while the same
        // divergence merged from the other seat's vantage yields
        // `hello/cat/there/dog`. Two seats resolving one divergence therefore
        // produced different bytes.
        //
        // The union is a lossless answer, not a positional one, so it may only
        // stand where the positional merge is NOT both clean and lossless.
        // [`is_lossless_union`] is that test, and it is what makes the
        // preference safe: whenever the union's own guarantee (`max(ours,
        // theirs)` copies of every line — nothing dropped, nothing duplicated)
        // already holds of the positional result, the positional result is
        // strictly better, because it also honours where the authors put their
        // lines.
        //
        // Measured by `tests/platform/sync/test_text_merge.py::
        // test_text_merge_concurrent_edits`;
        // the tier_1 twin `merge_concurrent_insertions` had passed throughout
        // only because it assigns the inserting side to `ours`, which is the
        // one side assignment the union happens to get right.
        if both_extend_base {
            if conflicts == 0 && is_lossless_union(&merged_bytes, &ours_ms, &theirs_ms) {
                return Ok(MergeResult::Merged(merged_bytes));
            }
            return Ok(MergeResult::Merged(line_multiset_union(
                ours_str, theirs_str, ours_ms,
            )));
        }

        if conflicts > 0 {
            Ok(MergeResult::Conflicts {
                merged: merged_bytes,
                conflict_count: conflicts,
            })
        } else {
            Ok(MergeResult::Merged(merged_bytes))
        }
    }
}

/// Perform a 3-way merge using LCS-based diffing.
///
/// Returns `(merged_text, conflict_count)`.
fn diff3_merge(base: &str, ours: &str, theirs: &str) -> (String, usize) {
    let base_lines: Vec<&str> = base.lines().collect();
    let our_lines: Vec<&str> = ours.lines().collect();
    let their_lines: Vec<&str> = theirs.lines().collect();

    // Compute edit scripts: base→ours and base→theirs
    let our_ops =
        similar::capture_diff_slices(similar::Algorithm::Patience, &base_lines, &our_lines);
    let their_ops =
        similar::capture_diff_slices(similar::Algorithm::Patience, &base_lines, &their_lines);

    // Flatten ops into per-base-line edit descriptors
    let our_edits = flatten_ops(&our_ops, &our_lines);
    let their_edits = flatten_ops(&their_ops, &their_lines);

    let mut result = Vec::new();
    let mut conflicts = 0;

    // The cross-anchor union budget (the same-anchor ruling, 2026-08-05): a
    // line BOTH sides inserted — at any anchor each; permuted heads from
    // earlier merge rounds place shared lines at different anchors, which a
    // per-anchor comparison structurally cannot see — is emitted from ours'
    // placement only: `min(ours, theirs)` of its theirs-side occurrences are
    // skipped, so a legitimately-duplicated user line still keeps
    // `max(ours, theirs)` copies and `union(x, x) == x`.
    let mut theirs_skip: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    {
        let mut ours_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for e in our_edits.values() {
            for l in &e.inserts_before {
                *ours_counts.entry(l.as_str()).or_default() += 1;
            }
        }
        let mut theirs_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for e in their_edits.values() {
            for l in &e.inserts_before {
                *theirs_counts.entry(l.as_str()).or_default() += 1;
            }
        }
        for (l, t) in theirs_counts {
            let m = t.min(ours_counts.get(l).copied().unwrap_or(0));
            if m > 0 {
                theirs_skip.insert(l.to_string(), m);
            }
        }
    }

    for (i, _base_line) in base_lines.iter().enumerate() {
        let our_edit = our_edits.get(&i);
        let their_edit = their_edits.get(&i);

        // Insert any lines added BEFORE this base line
        let our_inserts = our_edit.map(|e| &e.inserts_before[..]).unwrap_or(&[]);
        let their_inserts = their_edit.map(|e| &e.inserts_before[..]).unwrap_or(&[]);

        // Concurrent inserts merge as an ordered UNION — never a conflict,
        // never a blind concatenation (the same-anchor ruling, 2026-08-05,
        // `conflicts.md` § Concurrent resolution). The old arms were the two
        // live leg-4a signatures: different-content inserts fell to
        // latest-wins, where a SUBSET row (a late watcher upload of a file
        // the union already contains) destroyed its own superset fleet-wide;
        // and partially-shared inserts concatenated both sides whole,
        // duplicating every shared line. The union keeps ours' order and
        // appends theirs' unseen lines (the cross-anchor `theirs_skip`
        // budget above); sequences still converge across seats via nest-log
        // supersession (clause 1), exactly as distinct-append permutations
        // always have. (A canonical ORDER stays rejected — this chooses no
        // order, it only refuses to lose or double a side.)
        emit_insert_union(&mut result, our_inserts, their_inserts, &mut theirs_skip);

        // Handle the base line itself
        let our_deleted = our_edit.map(|e| e.deleted).unwrap_or(false);
        let their_deleted = their_edit.map(|e| e.deleted).unwrap_or(false);
        let our_replacement = our_edit.and_then(|e| e.replacement.as_deref());
        let their_replacement = their_edit.and_then(|e| e.replacement.as_deref());

        match (
            our_deleted,
            their_deleted,
            our_replacement,
            their_replacement,
        ) {
            // Neither side touched this line
            (false, false, None, None) => {
                result.push(base_lines[i].to_string());
            }
            // Both deleted
            (true, true, _, _) => { /* line removed by both — skip */ }
            // Only one side deleted, the other didn't touch it
            (true, false, _, None) | (false, true, None, _) => {
                // Deletion wins when other side is unchanged
            }
            // One deleted, other modified — conflict
            (true, false, _, Some(tr)) => {
                result.push("<<<<<<< ours".to_string());
                // ours deleted the line (nothing to show)
                result.push("=======".to_string());
                result.push(tr.to_string());
                result.push(">>>>>>> theirs".to_string());
                conflicts += 1;
            }
            (false, true, Some(or), _) => {
                result.push("<<<<<<< ours".to_string());
                result.push(or.to_string());
                result.push("=======".to_string());
                // theirs deleted the line (nothing to show)
                result.push(">>>>>>> theirs".to_string());
                conflicts += 1;
            }
            // Only ours replaced
            (false, false, Some(or), None) => {
                result.push(or.to_string());
            }
            // Only theirs replaced
            (false, false, None, Some(tr)) => {
                result.push(tr.to_string());
            }
            // Both replaced
            (false, false, Some(or), Some(tr)) => {
                if or == tr {
                    result.push(or.to_string());
                } else {
                    result.push("<<<<<<< ours".to_string());
                    result.push(or.to_string());
                    result.push("=======".to_string());
                    result.push(tr.to_string());
                    result.push(">>>>>>> theirs".to_string());
                    conflicts += 1;
                }
            }
        }
    }

    // Handle trailing insertions (lines added after the last base line)
    let our_trailing = our_edits
        .get(&base_lines.len())
        .map(|e| &e.inserts_before[..])
        .unwrap_or(&[]);
    let their_trailing = their_edits
        .get(&base_lines.len())
        .map(|e| &e.inserts_before[..])
        .unwrap_or(&[]);

    // The same ordered union as the interior anchors (the same-anchor ruling,
    // 2026-08-05): the old "take both, whole" concatenated shared lines twice.
    emit_insert_union(&mut result, our_trailing, their_trailing, &mut theirs_skip);

    let mut merged = result.join("\n");

    // Preserve trailing newline: if any input ended with \n, the output should too.
    // str::lines() strips trailing newlines, so we must restore them explicitly.
    let has_trailing = (!ours.is_empty() && ours.ends_with('\n'))
        || (!theirs.is_empty() && theirs.ends_with('\n'));
    if has_trailing && !merged.is_empty() {
        merged.push('\n');
    }

    (merged, conflicts)
}

/// The line multiset any lossless merge of two base-extending heads must have:
/// `max(ours, theirs)` copies of every line — no side's novelty dropped, no
/// shared line duplicated, and nothing invented. It is exactly
/// [`line_multiset_union`]'s own guarantee, which is what makes it the right
/// test for whether the POSITIONAL merge may stand in the union's place: a
/// positional result that already meets the union's bar is strictly better than
/// the union, because it also honours where each author put their lines.
fn is_lossless_union(
    merged: &[u8],
    ours_ms: &std::collections::HashMap<&str, usize>,
    theirs_ms: &std::collections::HashMap<&str, usize>,
) -> bool {
    let Ok(text) = std::str::from_utf8(merged) else {
        return false;
    };
    let got = line_multiset(text);
    let mut expected_distinct = 0usize;
    for (line, n) in ours_ms {
        let want = (*n).max(theirs_ms.get(line).copied().unwrap_or(0));
        if got.get(line).copied().unwrap_or(0) != want {
            return false;
        }
        expected_distinct += 1;
    }
    for (line, n) in theirs_ms {
        if ours_ms.contains_key(line) {
            continue;
        }
        if got.get(line).copied().unwrap_or(0) != *n {
            return false;
        }
        expected_distinct += 1;
    }
    // ...and no line the merge invented out of nothing.
    got.len() == expected_distinct
}

/// The line-multiset UNION of two heads that both extend their base: ours'
/// order first, theirs' uncovered lines appended in their order, so every line
/// keeps `max(ours, theirs)` copies and `union(x, x) == x`.
///
/// The lossless answer when the positional merge is not available — never a
/// conflict, and never latest-wins, which is what destroyed the losing side's
/// novelty before the same-anchor ruling (2026-08-05). It chooses no canonical
/// order; order across seats converges via nest-log supersession, as ratified.
fn line_multiset_union<'a>(
    ours: &'a str,
    theirs: &'a str,
    ours_ms: std::collections::HashMap<&'a str, usize>,
) -> Vec<u8> {
    let mut taken = ours_ms;
    let mut merged: Vec<&str> = ours.lines().collect();
    for line in theirs.lines() {
        let n = taken.entry(line).or_default();
        if *n == 0 {
            merged.push(line);
        } else {
            *n -= 1;
        }
    }
    let mut out = merged.join("\n");
    if (!ours.is_empty() && ours.ends_with('\n')) || (!theirs.is_empty() && theirs.ends_with('\n'))
    {
        out.push('\n');
    }
    out.into_bytes()
}

/// A text's lines as a content multiset — the containment pre-checks'
/// vocabulary (the same-anchor ruling, 2026-08-05).
fn line_multiset(text: &str) -> std::collections::HashMap<&str, usize> {
    let mut ms: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for l in text.lines() {
        *ms.entry(l).or_default() += 1;
    }
    ms
}

/// Does `outer` hold at least every line of `inner`, with multiplicity?
fn multiset_contains(
    outer: &std::collections::HashMap<&str, usize>,
    inner: &std::collections::HashMap<&str, usize>,
) -> bool {
    inner
        .iter()
        .all(|(l, n)| outer.get(l).copied().unwrap_or(0) >= *n)
}

/// Emit the ordered UNION of two sides' insertions at one anchor: ours in
/// order, then each of theirs not covered by the cross-anchor `theirs_skip`
/// budget (see its construction in [`diff3_merge`] — the same-anchor ruling,
/// 2026-08-05).
fn emit_insert_union(
    result: &mut Vec<String>,
    ours: &[String],
    theirs: &[String],
    theirs_skip: &mut std::collections::HashMap<String, usize>,
) {
    result.extend(ours.iter().cloned());
    for line in theirs {
        if let Some(n) = theirs_skip.get_mut(line.as_str())
            && *n > 0
        {
            *n -= 1;
            continue;
        }
        result.push(line.clone());
    }
}

/// Per-base-line edit descriptor.
struct LineEdit {
    /// Lines inserted BEFORE this base line.
    inserts_before: Vec<String>,
    /// Whether this base line was deleted.
    deleted: bool,
    /// If the base line was replaced (not deleted), the replacement line.
    replacement: Option<String>,
}

/// Convert `similar` diff ops into a map from base-line-index to [`LineEdit`].
fn flatten_ops(
    ops: &[similar::DiffOp],
    new_lines: &[&str],
) -> std::collections::HashMap<usize, LineEdit> {
    let mut edits: std::collections::HashMap<usize, LineEdit> = std::collections::HashMap::new();

    for op in ops {
        match *op {
            similar::DiffOp::Equal { .. } => { /* no edit */ }
            similar::DiffOp::Delete {
                old_index,
                old_len,
                new_index: _,
            } => {
                for i in old_index..old_index + old_len {
                    let entry = edits.entry(i).or_insert(LineEdit {
                        inserts_before: Vec::new(),
                        deleted: false,
                        replacement: None,
                    });
                    entry.deleted = true;
                }
            }
            similar::DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => {
                // Insertions go before base line `old_index`
                let entry = edits.entry(old_index).or_insert(LineEdit {
                    inserts_before: Vec::new(),
                    deleted: false,
                    replacement: None,
                });
                for line in new_lines.iter().skip(new_index).take(new_len) {
                    entry.inserts_before.push(line.to_string());
                }
            }
            similar::DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                // First base line gets the replacement lines; rest are deleted
                for (k, i) in (old_index..old_index + old_len).enumerate() {
                    let entry = edits.entry(i).or_insert(LineEdit {
                        inserts_before: Vec::new(),
                        deleted: false,
                        replacement: None,
                    });
                    if k == 0 {
                        // Join all replacement lines with \n so they stay in
                        // order when emitted as a single replacement string.
                        let replacement: String = (new_index..new_index + new_len)
                            .map(|j| new_lines[j])
                            .collect::<Vec<&str>>()
                            .join("\n");
                        entry.replacement = Some(replacement);
                    } else {
                        entry.deleted = true;
                    }
                }
            }
        }
    }

    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::FormatAdapter;

    #[test]
    fn no_conflict_same_content() {
        let adapter = TextAdapter;
        let base = b"line1\nline2\nline3";
        let ours = b"line1\nline2\nline3";
        let theirs = b"line1\nline2\nline3";

        assert!(adapter.can_merge(base, ours, theirs));
        let result = adapter.merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Merged(data) => {
                assert_eq!(std::str::from_utf8(&data).unwrap(), "line1\nline2\nline3");
            }
            _ => panic!("expected clean merge"),
        }
    }

    #[test]
    fn no_conflict_different_lines() {
        let adapter = TextAdapter;
        let base = b"line1\nline2\nline3";
        let ours = b"line1\nOUR CHANGE\nline3";
        let theirs = b"line1\nline2\nTHEIR CHANGE";

        let result = adapter.merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Merged(data) => {
                let text = std::str::from_utf8(&data).unwrap();
                assert!(text.contains("OUR CHANGE"));
                assert!(text.contains("THEIR CHANGE"));
                assert!(!text.contains("<<<<<<<"));
            }
            _ => panic!("expected clean merge"),
        }
    }

    #[test]
    fn conflict_same_line() {
        let adapter = TextAdapter;
        let base = b"line1\nline2\nline3";
        let ours = b"line1\nOUR VERSION\nline3";
        let theirs = b"line1\nTHEIR VERSION\nline3";

        let result = adapter.merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Conflicts {
                merged,
                conflict_count,
            } => {
                let text = std::str::from_utf8(&merged).unwrap();
                assert!(text.contains("<<<<<<< ours"));
                assert!(text.contains("OUR VERSION"));
                assert!(text.contains("THEIR VERSION"));
                assert!(text.contains(">>>>>>> theirs"));
                assert_eq!(conflict_count, 1);
            }
            _ => panic!("expected conflict"),
        }
    }

    #[test]
    fn semantic_chunks_per_line() {
        let adapter = TextAdapter;
        let content = b"first\nsecond\nthird";
        let chunks = adapter.semantic_chunks(content).unwrap().unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].id, "L0");
        assert_eq!(chunks[1].id, "L1");
        assert_eq!(chunks[2].id, "L2");
    }

    #[test]
    fn rejects_non_utf8() {
        let adapter = TextAdapter;
        let binary = &[0xFF, 0xFE, 0x00, 0x01];
        assert!(!adapter.can_merge(binary, binary, binary));
    }

    #[test]
    fn fast_forward_single_line_replacement_takes_theirs() {
        // DIAGNOSTIC (F-bug repro): receiver unchanged (ours == base), remote
        // replaced the only line. A correct 3-way merge must fast-forward to
        // theirs. Mirrors the sync e2e: B has "version 1", A updates to
        // "version 2 with more content"; B must end with theirs.
        let adapter = TextAdapter;
        let base = b"version 1";
        let ours = b"version 1";
        let theirs = b"version 2 with more content";
        let result = adapter.merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Merged(data) => {
                assert_eq!(
                    std::str::from_utf8(&data).unwrap(),
                    "version 2 with more content"
                );
            }
            MergeResult::Conflicts {
                merged,
                conflict_count,
            } => {
                panic!(
                    "expected clean fast-forward, got {conflict_count} conflicts: {:?}",
                    std::str::from_utf8(&merged)
                );
            }
        }
    }

    #[test]
    fn merge_multi_line_replacement() {
        // One side replaces a single line with multiple lines.
        let adapter = TextAdapter;
        let base = b"aaa\nbbb\nccc";
        let ours = b"aaa\nB1\nB2\nB3\nccc"; // replaced "bbb" with 3 lines
        let theirs = b"aaa\nbbb\nccc"; // unchanged

        let result = adapter.merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Merged(data) => {
                assert_eq!(std::str::from_utf8(&data).unwrap(), "aaa\nB1\nB2\nB3\nccc");
            }
            MergeResult::Conflicts { merged, .. } => {
                panic!(
                    "expected clean merge, got conflict:\n{}",
                    std::str::from_utf8(&merged).unwrap_or("<non-utf8>")
                );
            }
        }
    }

    #[test]
    fn merge_preserves_trailing_newline() {
        let adapter = TextAdapter;
        let base = b"hello\nthere\n";
        let ours = b"hello\ncat\nthere\n";
        let theirs = b"hello\nthere\ndog\n";

        let result = adapter.merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Merged(data) => {
                assert_eq!(
                    std::str::from_utf8(&data).unwrap(),
                    "hello\ncat\nthere\ndog\n"
                );
            }
            MergeResult::Conflicts { merged, .. } => {
                panic!(
                    "expected clean merge, got conflict:\n{}",
                    std::str::from_utf8(&merged).unwrap_or("<non-utf8>")
                );
            }
        }
    }

    /// The SAME divergence as [`merge_concurrent_insertions`], merged from the
    /// other seat's vantage — the side assignment that pin never covered.
    ///
    /// `merge` is not symmetric in general (ours' order leads, by the ratified
    /// no-canonical-order rule), but a NON-OVERLAPPING pair of hunks has one
    /// positional answer and both seats must reach it, or the two devices
    /// resolving one conflict write different bytes. Between 2026-08-05 and
    /// this pin the line-multiset union ran as a PRE-CHECK ahead of
    /// `diff3_merge` and this direction produced `hello/there/dog/cat` —
    /// the peer's inserted line relocated to the end — while its twin above
    /// produced `hello/cat/there/dog`. It went unmeasured for that whole window
    /// because the only coverage was the twin's single side assignment and the
    /// tier_3 test that would have caught it was
    /// itself dead from 2026-08-18.
    #[test]
    fn merge_concurrent_insertions_is_symmetric_across_seats() {
        let adapter = TextAdapter;
        let base = b"hello\nthere";
        let ours = b"hello\nthere\ndog"; // this seat appended
        let theirs = b"hello\ncat\nthere"; // the peer inserted

        let result = adapter.merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Merged(data) => {
                assert_eq!(
                    std::str::from_utf8(&data).unwrap(),
                    "hello\ncat\nthere\ndog",
                    "both seats must converge on the positional merge"
                );
            }
            MergeResult::Conflicts { merged, .. } => {
                panic!(
                    "expected clean merge, got conflict:\n{}",
                    std::str::from_utf8(&merged).unwrap_or("<non-utf8>")
                );
            }
        }
    }

    /// The same-anchor ruling's union still stands where it was ratified for:
    /// two PERMUTED supersets, which a positional diff reads as conflicting
    /// replacements. Preferring the positional merge (above) must not reach
    /// this case — the union is its fallback, not its replacement.
    #[test]
    fn permuted_supersets_still_take_the_union_not_latest_wins() {
        let adapter = TextAdapter;
        let base = b"a\n";
        // Each side holds the other's line at a different anchor plus one of
        // its own — the shape earlier union rounds produce per seat.
        let ours = b"a\nours\nshared\n";
        let theirs = b"a\nshared\ntheirs\n";

        let MergeResult::Merged(data) = adapter.merge(base, ours, theirs).unwrap() else {
            panic!("the union arm must never return conflict markers");
        };
        let text = String::from_utf8(data).unwrap();
        assert!(!text.contains("<<<<<<<"), "markers must never be written");
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort_unstable();
        assert_eq!(
            lines,
            vec!["a", "ours", "shared", "theirs"],
            "every line survives exactly once — no drop, no duplicate: {text:?}"
        );
    }

    #[test]
    fn merge_concurrent_insertions() {
        // A inserts "cat" between "hello" and "there"; B appends "dog" after "there".
        let adapter = TextAdapter;
        let base = b"hello\nthere";
        let ours = b"hello\ncat\nthere"; // inserted line between
        let theirs = b"hello\nthere\ndog"; // appended line at end

        let result = adapter.merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Merged(data) => {
                assert_eq!(
                    std::str::from_utf8(&data).unwrap(),
                    "hello\ncat\nthere\ndog"
                );
            }
            MergeResult::Conflicts { merged, .. } => {
                panic!(
                    "expected clean merge, got conflict:\n{}",
                    std::str::from_utf8(&merged).unwrap_or("<non-utf8>")
                );
            }
        }
    }

    /// Pins the OBSERVABLE ARM taken above the ceiling, never a duration (convention 14
    /// forbids timing asserts). Both sides insert a novel line at
    /// the very start (anchor 0), so the two arms are byte-distinguishable by construction:
    /// the positional merge (if it ran) would anchor THEIRS_EXTRA right after OURS_EXTRA,
    /// before any base line — the same "insert-before-index-0, ours-first" shape
    /// [`merge_concurrent_insertions`] pins at small scale. The linear union instead always
    /// appends an uncovered line at the true end, regardless of where it was inserted. Red-
    /// verify by temporarily lowering the ceiling below the test's `n`, or removing the gate
    /// — `got` then equals `positional_would_be` instead.
    #[test]
    fn both_extend_base_positional_merge_yields_to_the_union_above_the_line_ceiling() {
        let adapter = TextAdapter;
        // Comfortably past the ceiling on both sides so the test does not pin the exact
        // boundary value.
        let n = POSITIONAL_MERGE_LINE_CEILING + 100;
        let base_lines: Vec<String> = (0..n).map(|i| format!("line{i}")).collect();
        let base = base_lines.join("\n");
        let ours = format!("OURS_EXTRA\n{base}");
        let theirs = format!("THEIRS_EXTRA\n{base}");

        let MergeResult::Merged(data) = adapter
            .merge(base.as_bytes(), ours.as_bytes(), theirs.as_bytes())
            .unwrap()
        else {
            panic!("both_extend_base never returns conflict markers");
        };
        let got = String::from_utf8(data).unwrap();

        let positional_would_be = format!("OURS_EXTRA\nTHEIRS_EXTRA\n{base}");
        let union_is = format!("{ours}\nTHEIRS_EXTRA");
        assert_ne!(
            positional_would_be, union_is,
            "test construction bug: the two arms must be distinguishable"
        );

        assert_eq!(
            got, union_is,
            "above the line ceiling, both_extend_base must take the linear union arm \
             directly rather than attempt the positional merge"
        );
    }
    /// The ceiling governs the `diff3_merge` CALL, not the `both_extend_base` arm — a
    /// divergence with a single deleted line takes the general path, and that path used
    /// to reach the quadratic positional diff unguarded.
    ///
    /// `ours` drops one base line, so `multiset_contains(ours, base)` fails and
    /// `both_extend_base` is false — one line of input away from
    /// [`both_extend_base_positional_merge_yields_to_the_union_above_the_line_ceiling`]'s
    /// shape, and the whole difference between the gated and ungated paths before the
    /// hoist. The general path has no lossless linear answer to fall to (the union would
    /// resurrect the deleted line), so above the ceiling the adapter DECLINES: `merge`
    /// returns `Err`, and `resolve_conflict` folds that into latest-writer-wins, whose
    /// loser is retained (`conflicts.md` § Target model). The same shape below the ceiling
    /// still merges positionally — that control is what makes this a test of the CEILING
    /// and not of "deletions never merge".
    ///
    /// Red-verify by removing the general-path arm of the hoisted gate: `merge` then
    /// returns `Ok` for the oversized input.
    #[test]
    fn a_deleted_line_takes_the_general_path_and_is_declined_above_the_line_ceiling() {
        let adapter = TextAdapter;

        // Distinct lines keep this test fast: the quadratic degradation needs LOW line
        // cardinality, and the arm — not the duration — is what is pinned (convention 14).
        let build = |n: usize| {
            let base_lines: Vec<String> = (0..n).map(|i| format!("line{i}")).collect();
            let base = base_lines.join("\n");
            // `ours` DELETES line0 and appends its own novelty; `theirs` only appends.
            let ours = format!("{}\nOURS_EXTRA", base_lines[1..].join("\n"));
            let theirs = format!("{base}\nTHEIRS_EXTRA");
            (base, ours, theirs)
        };

        let n = POSITIONAL_MERGE_LINE_CEILING + 100;
        let (base, ours, theirs) = build(n);

        // Premise of the whole test: this input is NOT the both_extend_base shape.
        let base_ms = line_multiset(&base);
        let ours_ms = line_multiset(&ours);
        let theirs_ms = line_multiset(&theirs);
        assert!(
            !(multiset_contains(&ours_ms, &base_ms) && multiset_contains(&theirs_ms, &base_ms)),
            "test construction bug: the deleted line must break both_extend_base"
        );

        assert!(
            adapter
                .merge(base.as_bytes(), ours.as_bytes(), theirs.as_bytes())
                .is_err(),
            "above the line ceiling the general path must decline the positional merge, \
             leaving resolve_conflict's latest-writer-wins fallback to stand"
        );

        // Control: the identical shape below the ceiling still merges. Without this the
        // assertion above would also pass if deletions never merged at any size.
        let (base, ours, theirs) = build(16);
        let MergeResult::Merged(data) = adapter
            .merge(base.as_bytes(), ours.as_bytes(), theirs.as_bytes())
            .unwrap()
        else {
            panic!("below the ceiling this shape merges cleanly");
        };
        let got = String::from_utf8(data).unwrap();
        assert!(
            !got.contains("line0\n") && got.contains("OURS_EXTRA") && got.contains("THEIRS_EXTRA"),
            "below the ceiling the positional merge honours the deletion and both \
             novelties, got: {got:?}"
        );
    }
}
