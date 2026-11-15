import Testing
import Foundation
import CoreGraphics
@testable import FaunaKit

/// Tests for the engagement-cue observer's *probe* half
/// (`Feed/CueViewportObserver.swift`) — the pure frames+postsWindow → `[CueRow]`
/// step that sits between SwiftUI geometry and the shared `FfiCueTracker`
/// (capture-shell boundary revised 2026-07-29; all bucketing/credit/hold-vs-leave
/// logic now lives in `fauna_feed::CueTracker`, reached through `FfiCueTracker`).
///
/// The rest of that file is genuinely untestable glue (a timer, a `.task`, FFI
/// calls), but this step is not: it owns the **omit-unarranged-rows** rule, whose
/// violation would synthesize a phantom row for a card that was never even laid
/// out. A card the SwiftUI preference plumbing DID publish a frame for is
/// included as-is, non-positive height and all — the shared tracker itself
/// decides what an unmeasurable row means, so this probe no longer computes a
/// visibility fraction or filters on height (the two probe-side simplifications
/// `cue_tracker.rs`'s own doc comment describes).
///
/// Mirrors the split android and windows already ship (`CueViewportTest` /
/// its C# twin): every rule the shell can get wrong is pinned over primitives;
/// only the toolkit wiring is left to the e2e.

// A 100pt-tall viewport starting at y=0, so a card's `minY` reads directly as
// its offset into the viewport (retained from the pre-migration fixtures —
// `buildRows` itself does not read the viewport at all now; it is passed
// straight through to `FfiCueTracker.sample` instead).
private func frame(y: CGFloat, height: CGFloat) -> CGRect {
    CGRect(x: 0, y: y, width: 400, height: height)
}

// MARK: - buildRows: omit-unarranged-rows

@Test func aCardNeverPublishedInFramesProducesNoRow() {
    // Nothing published a frame for "b" at all (never laid out, not yet in the
    // view tree) — it must be OMITTED from the result, never synthesized from
    // `postsWindow` alone (which lists every loaded post, not just realized rows).
    let rows = CueViewportObserver.buildRows(
        frames: ["a": frame(y: 0, height: 50)],
        postsWindow: ["a": false, "b": false], subjects: ["a", "b"])
    #expect(rows.map(\.postId) == ["a"])
}

@Test func aPublishedRowWithNonPositiveHeightIsIncludedAsIs() {
    // Mid-layout: SwiftUI published a frame for "a" but it hasn't grown to its
    // final size yet. The old code filtered this out to avoid fabricating a
    // 0-visibility reading; the shared tracker now owns that decision (it holds
    // a non-positive-height row as unmeasurable), so the probe just passes the
    // real height through unfiltered.
    let rows = CueViewportObserver.buildRows(
        frames: ["a": frame(y: 0, height: 0)], postsWindow: ["a": false], subjects: ["a"])
    #expect(rows.count == 1)
    #expect(rows[0].height == 0)
}

@Test func negativeHeightRectsAreStandardizedByCoreGraphics() {
    // CoreGraphics STANDARDIZES a negative-height rect on read: `.height` comes
    // back positive and `.minY` shifts to the true top edge. So a raw negative
    // height is not representable here (unlike linux's raw f64), and `top`
    // always reflects the row's true leading edge — worth pinning because it is
    // a platform quirk the shared tracker's contract does not itself guarantee.
    let raw = frame(y: 10, height: -20)
    #expect(raw.height == 20)
    #expect(raw.minY == -10)

    let rows = CueViewportObserver.buildRows(frames: ["a": raw], postsWindow: ["a": false], subjects: ["a"])
    #expect(rows[0].top == -10)
    #expect(rows[0].height == 20)
}

// MARK: - buildRows: only a cue subject is measured

@Test func aCardThatShowsAPlaceholderInsteadOfThePostProducesNoRow() {
    // "b"'s row published a frame, but its card is a muted, blocked or
    // collapsed placeholder: only `PostCardBody`'s post-body arm marks a post
    // as a cue subject, so "b" is absent from `subjects` and must not be
    // measured — lingering on a placeholder is no exposure of the post it hides
    // (`engagement-cues.md` § Cue vocabulary & derivation). Twin of linux's
    // `cue-subject` class and android's `CueSubjectMark`.
    let rows = CueViewportObserver.buildRows(
        frames: ["a": frame(y: 0, height: 50), "b": frame(y: 50, height: 50)],
        postsWindow: ["a": false, "b": false],
        subjects: ["a"])
    #expect(rows.map(\.postId) == ["a"])
}

@Test func aSubjectWithNoPublishedFrameStillProducesNoRow() {
    // The mark alone is not a measurement: the row's frame is.
    let rows = CueViewportObserver.buildRows(
        frames: [:], postsWindow: ["a": false], subjects: ["a"])
    #expect(rows.isEmpty)
}

// MARK: - one delivery carries the frame and its subject mark

/// The preference key's `reduce`, as SwiftUI folds sibling and descendant
/// values: `marks` in order.
private func reduced(_ marks: [[String: CueCardMark]]) -> [String: CueCardMark] {
    var value = CueFramePreferenceKey.defaultValue
    for mark in marks { CueFramePreferenceKey.reduce(value: &value) { mark } }
    return value
}

@Test func aRowWhoseFrameArrivesWithoutItsSubjectMarkIsNeverMeasured() {
    // The row's `cueCard` background published its frame; the card paints a
    // placeholder, so no `cueSubject` mark rides with it. One delivery, no
    // row — never a frame measured against a subject set from an earlier pass.
    let marks = reduced([["a": CueCardMark(frame: frame(y: 0, height: 50))]])
    #expect(CueViewportObserver.buildRows(marks: marks, postsWindow: ["a": false]).isEmpty)
}

@Test func theFrameAndTheSubjectMarkMergeInEitherOrder() {
    let framed = ["a": CueCardMark(frame: frame(y: 10, height: 50))]
    let marked = ["a": CueCardMark(subject: true)]
    for order in [[framed, marked], [marked, framed]] {
        let rows = CueViewportObserver.buildRows(marks: reduced(order), postsWindow: ["a": false])
        #expect(rows.map(\.postId) == ["a"])
        #expect(rows.first?.top == 10)
    }
}

@Test func aSubjectMarkAloneIsNoMeasurement() {
    let marks = reduced([["a": CueCardMark(subject: true)]])
    #expect(CueViewportObserver.buildRows(marks: marks, postsWindow: ["a": false]).isEmpty)
}

// MARK: - buildRows: field mapping

@Test func topAndHeightRideStraightThroughFromTheFramesGlobalRect() {
    let rows = CueViewportObserver.buildRows(
        frames: ["a": frame(y: 42, height: 100)], postsWindow: ["a": false], subjects: ["a"])
    #expect(rows[0].top == 42)
    #expect(rows[0].height == 100)
}

@Test func isMediaIsResolvedFromThePostsWindow() {
    let rows = CueViewportObserver.buildRows(
        frames: ["a": frame(y: 0, height: 50), "b": frame(y: 0, height: 50)],
        postsWindow: ["a": true, "b": false], subjects: ["a", "b"])
    let byId = Dictionary(uniqueKeysWithValues: rows.map { ($0.postId, $0.isMedia) })
    #expect(byId["a"] == true)
    #expect(byId["b"] == false)
}

@Test func aRowMissingFromThePostsWindowDefaultsToNotMedia() {
    // Should not happen in practice (a card can only be tagged from a post
    // that's in the loaded window), but the lookup is a plain dictionary read,
    // so the fallback is worth pinning rather than left to force-unwrap.
    let rows = CueViewportObserver.buildRows(
        frames: ["a": frame(y: 0, height: 50)], postsWindow: [:], subjects: ["a"])
    #expect(rows[0].isMedia == false)
}

@Test func mediaPlayedPmIsAlwaysNil() {
    // The player notes its progress into the shared tracker, which takes the
    // peak itself — the probe never carries one (`CueViewportObserver.emit`).
    let rows = CueViewportObserver.buildRows(
        frames: ["a": frame(y: 0, height: 50)], postsWindow: ["a": true], subjects: ["a"])
    #expect(rows[0].mediaPlayedPm == nil)
}

// `postsWindow` is deliberately not fixture-tested: it is a one-field map over
// `PostSummary`, whose only real hazard (keying by position rather than post id)
// is structurally impossible — the function takes `[PostSummary]` and reads
// `.postId`, with no index in scope to get wrong. A `PostSummary` fixture would
// have to carry a `RenderDocument` + `VerificationStatus` and would break on
// every unrelated field the shared snapshot grows. The end-to-end path is
// covered by the tier_3 dwell test instead.

// A feed switch ends every exposure through the shared
// `FfiCueTracker.noteSelection`; the key's spelling and the first-sample and
// same-key rules are unit-proven in Rust (`fauna-feed`, `fauna-ffi`).
