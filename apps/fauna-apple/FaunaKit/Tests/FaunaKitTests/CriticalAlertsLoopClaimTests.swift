import Testing
import Foundation
@testable import FaunaKit

// The loop/one-pass split every app's post-auth hook makes
// (`critical-alerts.md` § Implementation status today, the 2026-09-24 windows
// entry: a first or identity-changing sign-in starts the re-sweep LOOP, a
// same-identity re-establish runs ONE pass instead of stacking a second loop).
//
// Apple needs the record for a reason windows does not: a Swift `Task.cancel()`
// never reaches a UniFFI future (`uniffiRustCallAsync` polls to completion), so
// cancelling the previous loop's task — what `startSweepLoop` used to do —
// left that loop running, and every same-identity re-auth stacked one more.
// Windows' `CriticalAlertsSweepTests` pins the same three moves.

@Test func aSameIdentityReAuthClaimsNoSecondLoop() {
    var claim = CriticalAlertsLoopClaim()
    #expect(claim.claim("actor@nest") != nil, "the first sign-in starts the loop")
    #expect(claim.claim("actor@nest") == nil, "a same-identity re-establish runs one pass")
}

@Test func anotherIdentityClaimsItsOwnLoop() {
    var claim = CriticalAlertsLoopClaim()
    _ = claim.claim("a@nest")
    #expect(claim.claim("b@nest") != nil)
    // The identity is the actor AT a nest: a re-point needs a loop sweeping that nest.
    #expect(claim.claim("b@other-nest") != nil)
}

@Test func anEndedLoopNoLongerCoversItsIdentity() {
    var claim = CriticalAlertsLoopClaim()
    let token = claim.claim("actor@nest")!
    claim.release(token)
    #expect(claim.claim("actor@nest") != nil)
}

@Test func aDepartedIdentitysLateExitLeavesItsSuccessorsRecord() {
    var claim = CriticalAlertsLoopClaim()
    let departed = claim.claim("a@nest")!
    _ = claim.claim("b@nest")
    claim.release(departed)
    #expect(claim.claim("b@nest") == nil, "b's loop is still live")
}

@MainActor
@Test func clearingTheRegistryForgetsTheLoop() {
    // `clearAll` is the identity-teardown signal: its epoch bump is what stops
    // the Rust loop, so the record must drop in the same call or the next
    // sign-in as the same actor would run one pass with no loop behind it.
    let host = CriticalAlertsHost()
    #expect(host.loopClaim.claim("actor@nest") != nil)
    host.clearAll()
    #expect(host.loopClaim.claim("actor@nest") != nil)
}
