import Foundation

/// Apple's ONE call site for restoring the persisted last-known supervision
/// snapshot into every enforcement surface (`family-safety.md` § Content
/// policy, clause 2). Both shells' `ContentView`
/// (`Fauna-macOS/Views/MainWindow/ContentView.swift`,
/// `Fauna-iOS/App/ContentView.swift`) call this at session establish
/// (`.task(id: client != nil)`), AHEAD of the first `familyStatus.refresh` /
/// `contentPolicy.refresh` / `screenTime.refresh` landing — the write is
/// `APIClient.familyStatus()`'s own success path, and the erase rides
/// `remove`/`clear_all` with the account's other stores; this is the third
/// and last of the ruling's three call sites, no design of its own.
///
/// `api == nil` clears every store's fallback to its baseline rather than
/// skipping the call — this is what makes it safe to call unconditionally at
/// EVERY session-establish firing, including the bare teardown phase
/// (`client` going non-nil → nil during sign-out / an account switch): a
/// departing account's restored guardian, floor, or lock can never bleed into
/// the next account's first paint. android's per-store construction-time seed
/// is the reference shape; apple's stores are app-scoped (one `AppState` /
/// `MacAppState` per process, reused across account switches, not rebuilt per
/// session), so the seed lives at this call site instead of each store's
/// `init`.
@MainActor
public func seedSupervisionSnapshot(
    api: APIClient?,
    familyStatus: FamilyStatusStore,
    contentPolicy: ContentPolicyStore,
    screenTime: ScreenTimeStore
) {
    let snapshot = api?.supervisionSnapshot()
    familyStatus.seed(from: snapshot)
    contentPolicy.seed(from: snapshot)
    screenTime.seed(from: snapshot)
}
