import SwiftUI

/// Shared view-model for the user-facing `mail-aliases` page (macOS + iOS, one
/// FaunaKit VM). A thin proxy over the shared
/// `MailAliasesMachine` (UniFFI): the machine owns all state + RPC orchestration
/// over the User-class `fauna.bridges.{list,create,update,revoke,delete}_account_alias`
/// + `generate_disposable_alias` surface; this VM holds the latest
/// `MailAliasesSnapshot` as `@Observable` state and re-reads it after each
/// `hydrate` / `dispatch`. The machine is **pull-based** (no observer callback,
/// like `MailSettingsMachine` — unlike `OnboardingMachine`), so re-assigning
/// `snapshot` is what drives the SwiftUI re-render. `hydrate()`/`dispatch(_:)`
/// come from `MachineBackedVM`'s shared default.
/// Target state: `docs/goal/behavior/mail-aliases.md`; the lead-client
/// renderer is linux (`apps/fauna-linux/src/settings/mail_aliases.rs`).
@MainActor @Observable
public final class MailAliasesVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`. The view reads its fields
    /// (`aliases`, `defaultDomain`, `lastMintedAddress`, `status`, `error`).
    public internal(set) var snapshot: MailAliasesSnapshot?
    /// Page-level error surface (`error-message`) — carries both connect/build
    /// failures and the machine's own `snapshot.error`.
    public var errorMessage: String?
    public internal(set) var isLoading = false
    /// True once a `hydrate()` round trip has actually completed — the
    /// loading-is-not-empty gate (`ui/README.md` § List pages: loading is not
    /// empty; rule-5 render lift). Monotonic: never reset on a failed
    /// hydrate, so a transient error doesn't flip an already-loaded list back
    /// to the loading state.
    public private(set) var loaded = false

    var machine: MailAliasesMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?

    public init() {}

    /// The canonical default domain new aliases are minted on; `nil` until the
    /// actor has at least one exact alias (add/generate disabled in that case).
    public var defaultDomain: String? { snapshot?.defaultDomain }

    /// Vend the machine from APIClient and load the first snapshot. Idempotent
    /// (the machine is built once).
    public func configure(api: APIClient) async {
        guard machine == nil || configuredApi !== api else { return }
        do {
            let m = try await api.mailAliasesMachine()
            machine = m
            configuredApi = api
            snapshot = m.snapshot()
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
    }

    /// Refresh the alias list from the nest (page mount).
    public func hydrate() async { await hydrateFromMachine() }

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: MailAliasesAction) async { await dispatchToMachine(action) }

    func didHydrateSuccessfully() { loaded = true }
}
