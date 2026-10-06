import Testing
@testable import FaunaKit

// `snapshot-prune-preview`'s own automation text (`docs/goal/ui/backups.md`
// § Errors & edge cases — *Prune with no candidates*: the three states are typed
// off the `prune_set_policy` reply and no app infers them).
//
// The preview container used to declare an EMPTY automation value, so from
// outside "nothing to prune" and "no retention policy configured for this set"
// were the same observation: the preview present, the execute button absent.
// These pin the verdict onto the id's own text as `"<title>  <verdict>"` — the
// shape tui's `prune_preview_elements` declares — and that the verdict keys on
// `wouldPrune`, exactly as the body's painted line does, so the two cannot
// disagree. The app-level witness is the e2e
// `test_a_prune_with_nothing_to_remove_says_which_kind_of_nothing` (macos, ios).

@Suite struct PrunePreviewVerdictTests {
    private func preview(_ state: PolicyState, wouldPrune: Int64 = 0,
                         remaining: Int64 = 3,
                         candidates: [PruneCandidate] = []) -> PrunePreview {
        PrunePreview(wouldPrune: wouldPrune, remaining: remaining,
                     candidates: candidates, policyState: state)
    }

    private func text(_ p: PrunePreview) -> String {
        PrunePreviewView.automationText(of: p)
    }

    @Test func aSetWithNoPolicySaysSoRatherThanNothingToPrune() {
        #expect(text(preview(.notSet))
                == "\(L.backups.prunePreviewTitle)  \(L.backups.prunePolicyNotSet)")
    }

    @Test func anUnreadablePolicySaysSo() {
        #expect(text(preview(.unparseable))
                == "\(L.backups.prunePreviewTitle)  \(L.backups.prunePolicyUnparseable)")
    }

    @Test func anAppliedPolicyWithinBoundsSaysNothingToPrune() {
        #expect(text(preview(.applied))
                == "\(L.backups.prunePreviewTitle)  \(L.backups.prunePreviewNothing)")
    }

    @Test func anAppliedPolicyWithCandidatesCarriesTheCounts() {
        let p = preview(.applied, wouldPrune: 2, remaining: 5, candidates: [
            PruneCandidate(id: 1, createdAt: 0, tags: []),
            PruneCandidate(id: 2, createdAt: 0, tags: []),
        ])
        #expect(text(p) == "\(L.backups.prunePreviewTitle)  "
                + L.backups.prunePreviewCounts(wouldPrune: "2", remaining: "5"))
    }

    @Test func theNothingVerdictKeysOnWouldPruneAsThePaintedLineDoes() {
        // The body paints "nothing" off `wouldPrune == 0`; a verdict keyed on
        // `candidates` instead would read counts here while the screen said
        // nothing to prune.
        let p = preview(.applied, wouldPrune: 0, candidates: [
            PruneCandidate(id: 1, createdAt: 0, tags: []),
        ])
        #expect(PrunePreviewView.verdict(of: p) == L.backups.prunePreviewNothing)
    }
}

// `snapshot-item[i]`'s own text — the *Row content contract* line
// (`docs/goal/ui/backups.md` § Snapshot-list shape). apple used to answer
// `get_text` with the bare snapshot id (its `value`), so no row-content witness
// could read the file count or a lifecycle state off an apple row.
@Suite struct SnapshotRowLineTests {
    private func row(_ state: SnapshotState = .active) -> SnapshotRow {
        SnapshotRow(id: 14, createdAt: 1_700_000_000, fileCount: 3, totalBytes: 2048,
                    deviceId: nil, tags: [], state: state, integrity: .unknown)
    }

    @Test func theLineCarriesTheFileCountAndIsNotTheBareId() {
        let line = SnapshotRowText.line(row())
        #expect(line.contains(L.backups.fileCount(count: "3")))
        #expect(line != "14")
    }

    @Test func aNonActiveRowCarriesItsStateSuffix() {
        let state = SnapshotState.deletionPending(executeAfter: 1_700_172_800)
        let suffix = try! #require(SnapshotRowText.state(state))
        #expect(SnapshotRowText.line(row(state)).hasSuffix("  " + suffix))
    }
}
