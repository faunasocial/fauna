import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pins for the apple store-change notice consumer
// (`account-runtime.md` § Multi-instance concurrency, the own-pump notice,
// part 4): the listener is registered once per process and never cleared; a
// notice reaches only the subscriptions that are still open; and a notice that
// lands mid-pass folds into exactly one more pass rather than being dropped or
// stacked.
@MainActor
@Suite("Store-change notice", .serialized)
struct StoreChangeNoticeTests {
    /// Let the main actor run the tasks a delivery spawned.
    private func settle() async {
        for _ in 0..<20 { await Task.yield() }
    }

    @Test("registering twice is the same registration")
    func registrationIsIdempotent() {
        StoreChangeNotice.registerOnce()
        StoreChangeNotice.registerOnce()
    }

    @Test("a delivered notice bumps the generation once")
    func deliveryBumpsGeneration() {
        let notice = StoreChangeNotice()
        let before = notice.generation
        notice.deliver()
        #expect(notice.generation == before + 1)
    }

    @Test("an open watch re-drives its resync on a notice, a cancelled one does not")
    func watchFiresUntilCancelled() async {
        let notice = StoreChangeNotice()
        var calls = 0
        let watch = notice.watch { calls += 1 }
        notice.deliver()
        await settle()
        #expect(calls == 1)
        watch.cancel()
        notice.deliver()
        await settle()
        #expect(calls == 1)
    }

    @Test("the handler re-drives only the subscription that is open")
    func onlyTheOpenSurfaceResyncs() async {
        let notice = StoreChangeNotice()
        var open = 0
        var left = 0
        let openWatch = notice.watch { open += 1 }
        let leftWatch = notice.watch { left += 1 }
        leftWatch.cancel()
        notice.deliver()
        await settle()
        #expect(open == 1)
        #expect(left == 0)
        openWatch.cancel()
    }

    @Test("a notice mid-pass folds into exactly one more pass")
    func midPassNoticesCoalesce() async {
        var passes = 0
        var driver: StoreResyncDriver!
        driver = StoreResyncDriver {
            passes += 1
            if passes == 1 {
                // Three notices land while the first pass is running.
                driver.notify()
                driver.notify()
                driver.notify()
            }
        }
        await driver.notify()?.value
        #expect(passes == 2)
    }

    @Test("a notice after the drain starts a fresh pass")
    func laterNoticeRunsAgain() async {
        var passes = 0
        let driver = StoreResyncDriver { passes += 1 }
        await driver.notify()?.value
        await driver.notify()?.value
        #expect(passes == 2)
    }
}
