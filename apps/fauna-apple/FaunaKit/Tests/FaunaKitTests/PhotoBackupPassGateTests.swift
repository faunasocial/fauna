import Testing
import Foundation
@testable import FaunaKit

// The photo-backup single-flight gate (`PhotoBackupPassGate`, driven by
// `PhotoBackupEngine.syncNewPhotos()`). Five triggers can call a pass — the enable
// toggle's grant branch, the PhotoKit change observer, the `BGProcessingTask` body,
// the Sync-now button, and a catch-up pass at launch on each shell — and they are
// not mutually exclusive, so a photo arriving mid-pass wakes the observer while
// another pass is still running.
//
// **Why the gate is tested here rather than through the engine.** A real pass needs
// a live `FfiSyncEngineHost`, a `NetworkMonitor` and a SwiftData `ModelContext`,
// none of them injectable (`syncNewPhotos()` early-returns on any being nil), so no
// unit test can drive one. The gate is the seam where the coalescing *decision* is
// separable from the I/O, which is what makes it testable at all; the wiring — that
// a mid-pass photo really is picked up by one trailing pass — is witnessed by the
// ios e2e over the `passes_started`/`passes_completed` counters.
//
// The property under test is the one the naive fix gets wrong. `guard !isBackingUp
// else { return 0 }` — which android's twin ships — drops the request instead of
// remembering it, and the running pass has already taken its `PHAsset.fetchAssets`
// snapshot, so the mid-pass photo is picked up by nothing until the next OS slice.
// Every test below is therefore about what happens to a REFUSED caller's intent.

@Test func theFirstClaimWinsAndASecondIsRefused() async {
    let gate = PhotoBackupPassGate()
    #expect(await gate.claim() == true, "a gate with no pass running must admit the first caller")
    #expect(await gate.claim() == false, "a second caller must not start a concurrent pass")
    #expect(await gate.isRunning == true)
}

@Test func arefusedClaimIsRememberedAndBecomesExactlyOneTrailingPass() async {
    let gate = PhotoBackupPassGate()
    #expect(await gate.claim() == true)
    // A photo arrives mid-pass: the observer's call is refused…
    #expect(await gate.claim() == false)
    // …and the finishing pass is told to run again. THIS is the property the bare
    // re-entry guard loses: the refused caller's intent survives its refusal.
    #expect(await gate.finish() == true, "a request refused mid-pass must produce a trailing pass")
    // The trailing pass is the last one — nothing asked for another.
    #expect(await gate.finish() == false)
    #expect(await gate.isRunning == false, "the gate must be open again once the trailing pass ends")
}

@Test func aBurstOfRefusalsCollapsesToOneTrailingPassNotN() async {
    let gate = PhotoBackupPassGate()
    #expect(await gate.claim() == true)
    // Ten photos land while one pass runs — a plausible import, and the reason the
    // flag is a flag rather than a counter: the trailing pass re-walks the whole
    // library, so it subsumes all ten. Queueing ten passes would re-walk it ten
    // times for nothing.
    for _ in 0..<10 {
        #expect(await gate.claim() == false)
    }
    #expect(await gate.finish() == true, "the burst must still earn a trailing pass")
    #expect(await gate.finish() == false, "…but exactly one, however large the burst")
}

@Test func theGateIsNeverOpenBetweenAPassAndItsTrailingRun() async {
    let gate = PhotoBackupPassGate()
    #expect(await gate.claim() == true)
    #expect(await gate.claim() == false)
    #expect(await gate.finish() == true)
    // Still running: the trailing pass belongs to the caller `finish()` just
    // answered, so a fresh trigger arriving now must not start a third concurrent
    // pass — it must be remembered like any other mid-pass request.
    #expect(await gate.isRunning == true, "the trailing pass holds the gate")
    #expect(await gate.claim() == false)
    #expect(await gate.finish() == true, "a request during the trailing pass earns its own re-run")
}

@Test func aQuietPassEndsWithNoTrailingRun() async {
    let gate = PhotoBackupPassGate()
    #expect(await gate.claim() == true)
    #expect(await gate.finish() == false, "nothing asked for a second pass, so there must not be one")
    #expect(await gate.isRunning == false)
    // And the gate is reusable — the next trigger runs an ordinary first pass.
    #expect(await gate.claim() == true)
}

@Test func releaseDiscardsARememberedRequestAndOpensTheGate() async {
    let gate = PhotoBackupPassGate()
    #expect(await gate.claim() == true)
    #expect(await gate.claim() == false)   // remembered
    // The throwing path: a pass that failed would almost certainly fail again, so
    // the trailing re-run is deliberately dropped rather than spun. Nothing is lost
    // — a failed asset has no `PhotoBackupRecord`, so the next trigger re-attempts
    // it — but the gate must be genuinely open afterwards, or backup would wedge
    // for the process's lifetime on one thrown pass.
    await gate.release()
    #expect(await gate.isRunning == false, "a released gate must not stay latched")
    #expect(await gate.claim() == true, "the next trigger must be able to run a pass")
    #expect(await gate.finish() == false, "release must have discarded the remembered request")
}

@Test func concurrentClaimsAdmitExactlyOnePass() async {
    // The race the actor exists for, driven rather than reasoned about: 64 tasks
    // calling `claim()` at once must yield exactly one winner. On a plain `Bool`
    // this is the check-then-set that lets several through, and the compiler will
    // not flag it under `swiftLanguageMode(.v5)`.
    let gate = PhotoBackupPassGate()
    let winners = await withTaskGroup(of: Bool.self, returning: Int.self) { group in
        for _ in 0..<64 {
            group.addTask { await gate.claim() }
        }
        var count = 0
        for await won in group where won { count += 1 }
        return count
    }
    #expect(winners == 1, "exactly one of 64 concurrent callers may run a pass")
    // …and the 63 refusals are remembered as one trailing pass, not 63.
    #expect(await gate.finish() == true)
    #expect(await gate.finish() == false)
}
