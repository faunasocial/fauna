import Testing
import Foundation
@testable import FaunaKit

// The reporting paint's Swift seam (moderation.md § User-initiated reporting):
// the sheet store's open/cancel/reset lifecycle, the hide key per subject kind,
// the reporter-side hide in the render path, and the keep-on-failure merge
// carrying the hidden list. Every decision itself is shared Rust's and pinned
// there; these guard only what Swift sequences.

private let author = String(repeating: "ab", count: 32)
private let cid = String(repeating: "cd", count: 32)

@Test func subjectIdIsTheHideKeyPerSubjectKind() {
    #expect(ReportSheetStore.subjectId(of: .post(cid: cid)) == cid)
    #expect(ReportSheetStore.subjectId(of: .message(channel: "ch", recordCid: cid)) == cid)
    #expect(ReportSheetStore.subjectId(of: .actor(actorId: author)) == author)
    #expect(ReportSheetStore.subjectId(of: .unknown(cbor: Data([0x80]))) == nil)
}

@MainActor
@Test func openStartsFromAnEmptyDraftAndCancelCloses() {
    let store = ReportSheetStore()
    #expect(store.view == nil)
    store.form.note = "stale"
    store.open(reportActorTarget(actorId: author))
    #expect(store.target != nil)
    #expect(store.form.note.isEmpty)
    #expect(store.form.reason == nil)
    // No reason picked → the shared fold refuses the send.
    #expect(store.view?.canSubmit == false)
    store.cancel()
    #expect(store.target == nil)
    #expect(store.view == nil)
}

@MainActor
@Test func resetDropsTheDraftAndTheAcknowledgement() {
    let store = ReportSheetStore()
    store.open(reportPostTarget(cid: cid, author: author, plaintext: "x", gated: false))
    store.setError("boom")
    store.reset()
    #expect(store.target == nil)
    #expect(store.errorMessage.isEmpty)
    #expect(store.status.isEmpty)
}

@Test func anAccountReportOffersNoIncludeTextButASealedPostDoes() {
    let account = reportSheetView(
        target: reportActorTarget(actorId: author), form: ReportSheetStore.emptyForm)
    #expect(account.showIncludeText == false)
    let gated = reportSheetView(
        target: reportPostTarget(cid: cid, author: author, plaintext: "x", gated: true),
        form: ReportSheetStore.emptyForm)
    #expect(gated.showIncludeText == true)
}

@Test func aReportedItemOrAuthorIsHiddenAndAnEmptyListNeverHides() {
    let none = ContentPolicyInputs()
    #expect(none.isReported(itemId: cid, authorId: author) == false)

    let byItem = ContentPolicyInputs(hiddenContent: [cid])
    #expect(byItem.isReported(itemId: cid, authorId: nil))
    #expect(byItem.isReported(itemId: String(repeating: "ef", count: 32), authorId: author) == false)

    let byAuthor = ContentPolicyInputs(hiddenContent: [author])
    #expect(byAuthor.isReported(itemId: cid, authorId: author))
}

@Test func theKeepOnFailureMergeCarriesTheHiddenList() {
    let previous = ContentPolicyInputs(hiddenContent: [cid])
    let merged = ContentPolicyStore.merged(previous: previous, policy: .failed, own: .failed)
    #expect(merged.hiddenContent == [cid])
}

// The abuse-report composer (account-offline-mutation.md § Implementation
// status today, the knock-accept and abuse-report composers). With no nest
// `abuseReportSubmitOrQueue` answers `nil` (queued in the device outbox — the
// queue-then-send half is pinned in Rust, `libs/fauna-ffi/src/abuse_report.rs`);
// these pin the sheet's half. Mirrors android's `ReportSheetStoreTest`.

private func tickedPost() -> (FfiReportTarget, FfiReportForm) {
    let target = reportPostTarget(cid: cid, author: author, plaintext: "x", gated: false)
    let form = FfiReportForm(reason: nil, note: "", includeText: false, blockAuthor: true)
    return (target, form)
}

@Test func aReportWithNoNestIsQueuedAndSaysSo() {
    let (target, form) = tickedPost()
    let outcome = ReportSheetStore.outcome(of: nil, draft: form, target: target)
    #expect(outcome.status == renderLocalizedText(reportQueued()))
    #expect(outcome.blockAuthor == nil, "a queued report never tries the block — it needs the nest")
    #expect(outcome.notice == renderLocalizedText(reportBlockNotSent()))

    var unticked = form
    unticked.blockAuthor = false
    let quiet = ReportSheetStore.outcome(of: nil, draft: unticked, target: target)
    #expect(quiet.notice == nil, "no block asked, nothing to say was not sent")
}

@Test func aReportWithTheNestConnectedIsSentNotQueued() {
    let (target, form) = tickedPost()
    let ack = reportQueued()   // any LocalizedText stands in for the nest's acknowledgement
    let reply = FfiReportSent(reportId: "r1", routedTo: [], acknowledgement: ack)
    let outcome = ReportSheetStore.outcome(of: reply, draft: form, target: target)
    #expect(outcome.status == renderLocalizedText(ack))
    #expect(outcome.blockAuthor == author, "a sent report with the box ticked blocks the author")
    #expect(outcome.notice == nil)
}
