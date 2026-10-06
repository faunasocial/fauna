import SwiftUI

/// Shared view-model for the user-facing `mail-lists` page (macOS + iOS, one
/// FaunaKit VM). A thin proxy over the shared
/// `MailListsMachine` (UniFFI): the machine owns the list CRUD + the owned-domain
/// projection; this VM holds the latest `MailListsSnapshot` as `@Observable`
/// state and re-reads it after each `hydrate` / `dispatch`. Pull-based (no
/// observer callback), like `MailAliasesMachine`.
///
/// The list backend is live (`mail-mass-mailing.md` § Implementation status
/// today) — actions round-trip through the real `fauna.bridges.*_list_*` RPCs;
/// a failure surfaces on `snapshot.error`, shown via `error-message`. Target
/// behavior: `docs/goal/behavior/mail-mass-mailing.md`; lead-client renderer is
/// linux (`apps/fauna-linux/src/settings/mail_lists.rs`). `hydrate()`/
/// `dispatch(_:)` come from `MachineBackedVM`'s shared default (row 7 harvest
/// pass 68 dedup).
@MainActor @Observable
public final class MailListsVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`.
    public internal(set) var snapshot: MailListsSnapshot?
    /// Page-level error surface (`error-message`).
    public var errorMessage: String?
    public internal(set) var isLoading = false
    /// True once a `hydrate()` round trip has actually completed — the
    /// loading-is-not-empty gate (`ui/README.md` § List pages: loading is not
    /// empty; rule-5 render lift). Monotonic: never reset on a failed
    /// hydrate, so a transient error doesn't flip an already-loaded list back
    /// to the loading state.
    public private(set) var loaded = false

    var machine: MailListsMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?

    public init() {}

    /// The user's owned domains; the add control is disabled when empty (a list
    /// needs a send-from domain).
    public var localDomains: [String] { snapshot?.localDomains ?? [] }

    /// Vend the machine from APIClient and load the first snapshot. Idempotent.
    public func configure(api: APIClient) async {
        guard machine == nil || configuredApi !== api else { return }
        do {
            let m = try await api.mailListsMachine()
            machine = m
            configuredApi = api
            snapshot = m.snapshot()
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
    }

    /// Refresh the list roster + owned domains from the nest (page mount).
    public func hydrate() async { await hydrateFromMachine() }

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: MailListsAction) async { await dispatchToMachine(action) }

    func didHydrateSuccessfully() { loaded = true }
}
