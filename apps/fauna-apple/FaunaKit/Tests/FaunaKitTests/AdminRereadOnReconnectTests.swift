#if !FAUNA_KIDS
import Foundation
import Testing
@testable import FaunaKit

// admin.md § Errors & edge cases, *Controls wait for their read*: a read that
// has not answered is read again when the nest connection comes back, with no
// navigation; every entry reads again; an answered read is not asked on a
// reconnect, so a typed draft survives. The apple leg of tui's
// `admin::reconnect_reread_op` and web's `admin-reread.ts`, one FaunaKit
// trigger (`AdminPageReread`) driving every read-gated admin page.

/// A page whose read fails until told otherwise, counting how often it is asked.
@MainActor
private final class FakePage {
    var answered = false
    var reads = 0
    func read() async { reads += 1 }
}

@Test func onlyAComebackToConnectedAsksAnUnansweredRead() {
    // The first state seen only seeds.
    #expect(!adminReconnectRereads(was: nil, now: .connected, answered: false))
    for was in [FfiConnectionState.connecting, .disconnected, .unreachable] {
        #expect(adminReconnectRereads(was: was, now: .connected, answered: false))
        #expect(!adminReconnectRereads(was: was, now: .connected, answered: true))
    }
    #expect(!adminReconnectRereads(was: .connected, now: .connected, answered: false))
    #expect(!adminReconnectRereads(was: .connected, now: .disconnected, answered: false))
}

@Test @MainActor func aFailedReadIsAskedAgainWhenTheConnectionComesBack() async {
    let page = FakePage()
    let reread = AdminPageReread()
    await reread.connection(.connected, answered: page.answered, page.read)
    await reread.entry(page.read)
    #expect(page.reads == 1)
    // The entry read failed; the connection drops and comes back.
    await reread.connection(.disconnected, answered: page.answered, page.read)
    #expect(page.reads == 1)
    await reread.connection(.connected, answered: page.answered, page.read)
    #expect(page.reads == 2)
}

@Test @MainActor func anAnsweredReadIsNotAskedOnAReconnectButEveryEntryReads() async {
    let page = FakePage()
    let reread = AdminPageReread()
    await reread.connection(.connected, answered: false, page.read)
    await reread.entry(page.read)
    page.answered = true
    await reread.connection(.connecting, answered: page.answered, page.read)
    await reread.connection(.connected, answered: page.answered, page.read)
    #expect(page.reads == 1, "an answered page keeps its draft")
    await reread.entry(page.read)
    #expect(page.reads == 2, "an entry reads again, answered or not")
}

/// A page whose first read holds until released.
@MainActor
private final class SlowPage {
    var reads = 0
    var started: CheckedContinuation<Void, Never>?
    var release: CheckedContinuation<Void, Never>?
    func read() async {
        reads += 1
        guard reads == 1 else { return }
        started?.resume()
        await withCheckedContinuation { release = $0 }
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor func aReadAskedWhileOneRunsRunsOnceAfterIt() async {
    let reread = AdminPageReread()
    let page = SlowPage()
    await reread.connection(.disconnected, answered: false, page.read)
    let entry = Task { @MainActor in await reread.entry(page.read) }
    await withCheckedContinuation { page.started = $0 }
    // Two reconnects while the entry read runs: neither starts a second read.
    await reread.connection(.connected, answered: false, page.read)
    await reread.connection(.unreachable, answered: false, page.read)
    await reread.connection(.connected, answered: false, page.read)
    #expect(page.reads == 1)
    page.release?.resume()
    await entry.value
    #expect(page.reads == 2, "the waiting asks run once, after the running read")
}

@Test @MainActor func everyReadGatedPageIsUnansweredBeforeItsRead() {
    let pages: [any AdminPageReading] = [
        AdminCalendarVM(), AdminContactsVM(), AdminFilesVM(), AdminMailVM(),
        ForwardersVM(), BridgeApprovalVM(), AdminNestVM(), AdminTiersVM(), AdminWebVM(),
    ]
    for page in pages {
        #expect(!page.readAnswered, "\(type(of: page)) reads again on a reconnect before its read")
    }
}
#endif
