import Foundation
import Testing

@testable import FaunaKit

// ⚠ SUPERSEDED as a safety argument (2026-09-29). The goal doc's premise below
// — that the caller's actor decides which stack the Rust poll runs on — was
// REFUTED by a crash on a `@MainActor` view model's call (the generated
// `uniffiRustCallAsync` is nonisolated), and every async export is now declared
// with `#[fauna_uniffi_async::export]`, which runs the work on a tokio worker
// whatever the caller's isolation (`native-async-execution.md` § The rule →
// *Every async export runs on a tokio worker*). The two assertions below are
// still true Swift facts; they no longer decide any Rust stack's safety.
//
// What this pins — and why it is not language trivia.
//
// `../../../../../docs/goal/architecture/apps/native-async-execution.md`
// § Implementation status today records the measurement that decides this whole
// crash class on Apple: **what makes an FFI-reachable call safe is WHICH ACTOR
// drives it.** `#[uniffi::export(async_runtime = "tokio")]` leaves the future
// polled on whichever thread the foreign side drives it from, so a chain that
// reaches ML-KEM-768 (~548 KB of synchronous frames) survives on the main
// thread's ~8 MB stack and dies on a Swift cooperative-pool thread's **544 KB**
// — `EXC_BAD_ACCESS` on the guard page, no Rust panic, nothing on any sink.
//
// The two measured 2026-08-23 data points were a *contrast*, and the difference
// between them was isolation, nothing else:
//
//   * `SuccessionAftermath` is a bare `enum` with no global actor, so the
//     `Task {}` in its `static func run` inherits **no** isolation, lands on the
//     cooperative pool, and **crashed**.
//   * `FaunaClient` is `@MainActor`, so the `Task {}` in its fire-and-forget
//     statics (`refreshMailEpochSchedule`, `runSealBackfill`) inherits main-actor
//     isolation and gets the big stack.
//
// That second clause is the load-bearing one, and it is a *language* guarantee
// nothing in this repo asserted: an unstructured `Task {}` inherits the actor
// isolation of its enclosing declaration, and a global-actor attribute on a type
// isolates that type's **static** members too, not only its instance members.
// The entire "the fire-and-forget passes on `FaunaClient` are safe" argument
// rests on it, and if it were false those passes would be reaching PQ keygen on
// a 544 KB stack on every authenticated start.
//
// So it is asserted here rather than reasoned about. If a Swift release ever
// changed static-member isolation inheritance, or someone dropped `@MainActor`
// from `FaunaClient`, this fails loudly at build time instead of becoming a
// crash with no panic and no log in a user's hands.
//
// ⚠ These are deliberately about the *isolation rule*, not about any one call
// site: a test that drove the real `FaunaClient` helper would need a live
// `APIClient` and would prove nothing extra, since what it would be exercising
// is exactly the rule below. The per-call-site question — which helpers actually
// reach a PQ leaf — is a static call-graph question and belongs in the goal doc's
// enumeration, not in a unit test.
//
// Neither test is timing-dependent (e2e-conventions convention 14): both assert
// a deterministic isolation property and use a continuation rather than any
// wall-clock wait.

/// Mirrors `FaunaClient`'s shape: a fire-and-forget `static func` on a
/// `@MainActor` type. The `Task {}` should inherit main-actor isolation.
@MainActor
private enum MainActorIsolatedHost {
    static func fireAndForget(_ report: @escaping @Sendable (Bool) -> Void) {
        Task { report(Thread.isMainThread) }
    }
}

/// Mirrors `SuccessionAftermath`'s shape: a fire-and-forget `static func` on a
/// type carrying no global actor. The `Task {}` inherits no isolation and runs
/// on the cooperative pool — the 544 KB stack that crashed.
private enum UnisolatedHost {
    static func fireAndForget(_ report: @escaping @Sendable (Bool) -> Void) {
        Task { report(Thread.isMainThread) }
    }
}

@Suite("Foreign-stack actor isolation (native-async-execution.md)")
struct ForeignStackIsolationTests {

    @MainActor
    @Test("a fire-and-forget Task on a @MainActor type keeps the main thread's big stack")
    func mainActorStaticKeepsTheMainThread() async {
        let ranOnMain: Bool = await withCheckedContinuation { continuation in
            MainActorIsolatedHost.fireAndForget { continuation.resume(returning: $0) }
        }

        #expect(
            ranOnMain,
            """
            A `Task {}` inside a static member of a @MainActor type left the main \
            thread. That breaks the safety argument for FaunaClient's \
            fire-and-forget passes (refreshMailEpochSchedule runs on EVERY \
            authenticated start and reaches fauna_pq_kem::derive_keypair): they \
            would now be driving ML-KEM-768 on a 544 KB cooperative-pool stack. \
            See native-async-execution.md § The rule — the remedy is a Rust-side \
            tokio::spawn at the FFI export, never a Swift-side actor convention.
            """
        )
    }

    @Test("a fire-and-forget Task on a non-isolated type does NOT get the main thread")
    func unisolatedStaticLeavesTheMainThread() async {
        let ranOnMain: Bool = await withCheckedContinuation { continuation in
            UnisolatedHost.fireAndForget { continuation.resume(returning: $0) }
        }

        #expect(
            !ranOnMain,
            """
            A `Task {}` inside a static member of a NON-isolated type ran on the \
            main thread. If that were generally true, the SuccessionAftermath \
            crash of 2026-08-23 could not have happened as diagnosed — so either \
            the diagnosis in native-async-execution.md § The second measured \
            incident needs revisiting, or this test no longer models it.
            """
        )
    }
}
