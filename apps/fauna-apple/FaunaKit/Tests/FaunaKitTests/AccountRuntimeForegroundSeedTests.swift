import Foundation
import Testing
@testable import FaunaKit

// The account runtime's foreground seed (`p2p-shared-set-build.md` § Phone
// peers — design, decision 2; `account-sync-plane.md` § Implementation status
// today): before the runtime starts, a phone tells shared Rust whether it is
// in front, so a launch that reaches the start in the background never binds
// the same-account peer leg on the Rust default.
@Suite struct AccountRuntimeForegroundSeedTests {
    @Test func aLaunchInTheBackgroundStartsStoodDown() {
        #expect(FaunaClient.accountRuntimeForegroundSeed(.background) == false)
    }

    @Test func aLaunchOnItsWayToTheFrontKeepsTheLeg() {
        // No `.active` edge fires for the phase a launch starts in, so an
        // `.inactive` cold launch read as out of front would leave the leg
        // down in front for good.
        #expect(FaunaClient.accountRuntimeForegroundSeed(.inactive) == true)
        #expect(FaunaClient.accountRuntimeForegroundSeed(.active) == true)
    }

    @MainActor @Test func aDesktopSignalsNothing() {
        #expect(FaunaClient.accountRuntimeForegroundSeed(nil) == nil)
        #if os(macOS)
        #expect(FaunaClient.appRunStateNow() == nil)
        #endif
    }
}
