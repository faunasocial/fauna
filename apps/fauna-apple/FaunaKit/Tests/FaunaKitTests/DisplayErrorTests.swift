import Foundation
import Testing
@testable import FaunaKit

// `ui/README.md` § A cancellation is not an error. The measured leak was
// `URLError.cancelled`, whose `localizedDescription` is the bare word "cancelled" —
// which is exactly what an iOS admin shell painted in red after a launch verdict
// seeded the wizard and an authenticated session immediately replaced it.

@Test func aUrlSessionCancellationIsNotDisplayed() {
    #expect(DisplayError.isCancellation(URLError(.cancelled)))
    #expect(DisplayError.http(URLError(.cancelled)) == nil)
}

@Test func aSwiftConcurrencyCancellationIsNotDisplayed() {
    #expect(DisplayError.isCancellation(CancellationError()))
    #expect(DisplayError.http(CancellationError()) == nil)
}

@Test func aBridgedNSErrorCancellationIsNotDisplayed() {
    // The same condition arriving already bridged — what a call that crossed an
    // Objective-C boundary hands back.
    let bridged = NSError(domain: NSURLErrorDomain, code: NSURLErrorCancelled)
    #expect(DisplayError.isCancellation(bridged))
    #expect(DisplayError.http(bridged) == nil)
}

@Test func arealFailureStillGetsItsBanner() {
    // The rule removes the red sentence for cancellations ONLY — a genuine
    // failure must still reach the user, or the fix would be a swallowed error.
    let timedOut = URLError(.timedOut)
    #expect(DisplayError.isCancellation(timedOut) == false)
    let text = DisplayError.http(timedOut)
    #expect(text != nil)
    #expect(text?.contains(timedOut.localizedDescription) == true)
}

@Test func anOtherDomainNSErrorSharingTheCodeStillGetsItsBanner() {
    // `NSURLErrorCancelled` is -999; a different domain reusing that number is a
    // different error, so the domain is part of the test and not decoration.
    let unrelated = NSError(domain: "social.fauna.test", code: NSURLErrorCancelled)
    #expect(DisplayError.isCancellation(unrelated) == false)
    #expect(DisplayError.http(unrelated) != nil)
}

// A shared-Rust boundary error already carries display text — the catalog
// sentence the Rust side rendered. UniFFI's generated `errorDescription` is
// `String(reflecting:)`, so painting `"\(error)"` put the enum's debug shape on
// screen: `General(msg: "No public folder by that name …")`, measured on the
// macOS follow witness (`test_folder_follow_outcomes.py`).

@Test func aBoundaryErrorShowsItsOwnMessageNotItsDebugShape() {
    let sentence = "No public folder by that name for that person. Check the handle and the folder name."
    #expect(DisplayError.message(FfiError.General(msg: sentence)) == sentence)
    #expect(DisplayError.message(FfiError.NestOutdated(msg: sentence)) == sentence)
}

@Test func aCancelledBoundaryCallShowsNothing() {
    #expect(DisplayError.message(CancellationError()) == nil)
}

@Test func anErrorWithNoMessageOfItsOwnStillGetsText() {
    // No sentence to unwrap: the fallback keeps the error visible rather than
    // swallowing it.
    let text = DisplayError.message(FfiError.IdentitySuperseded(newActorIdHex: "ab"))
    #expect(text?.isEmpty == false)
}
