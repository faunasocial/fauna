import SwiftUI

/// Shared view-model for the user-facing `mail-list-members` page (macOS + iOS,
/// one FaunaKit VM), scoped to one mailing list. A thin
/// proxy over the shared `MailListMembersMachine` (UniFFI): the machine owns the
/// member roster + add / batch-import / unsubscribe / resubscribe sequencing;
/// this VM holds the latest `MailListMembersSnapshot` as `@Observable` state and
/// re-reads it after each `hydrate` / `dispatch`. Pull-based (no observer
/// callback), like `MailAliasesMachine`.
///
/// The page is opened scoped to a specific list (via the `mail-lists` row's
/// "Members" button → `configure(api:listIdHex:listName:)`). Opened directly
/// with nothing selected (the settings rail's own `mail-list-members` slot),
/// it falls back to the caller's first owned list, or the placeholder id's
/// honest empty page when there are none (`mail-mass-mailing.md` § Per-app
/// render status — the tui/windows shape, not linux's). Target behavior:
/// `docs/goal/behavior/mail-mass-mailing.md`; reference renderers linux
/// (`apps/fauna-linux/src/settings/mail_list_members.rs`, placeholder-only)
/// and windows (`MailListMembersPanel.EnsureVmAsync`, the fallback shape this
/// mirrors — apple likewise has no already-hydrated `MailListsMachine` any
/// settings page can peek at, unlike tui). `hydrate()`/`dispatch(_:)` come from
/// `MachineBackedVM`'s shared default.
@MainActor @Observable
public final class MailListMembersVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`.
    public internal(set) var snapshot: MailListMembersSnapshot?
    /// Page-level error surface (`error-message`).
    public var errorMessage: String?
    public internal(set) var isLoading = false
    /// True once a `hydrate()` round trip has actually completed — the
    /// loading-is-not-empty gate (`ui/README.md` § List pages: loading is not
    /// empty; rule-5 render lift). Monotonic: never reset on a failed
    /// hydrate, so a transient error doesn't flip an already-loaded roster
    /// back to the loading state.
    public private(set) var loaded = false

    var machine: MailListMembersMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?
    /// The list the cached ``machine`` was vended for. Unlike its siblings this machine
    /// is bound to a *list* as well as a client (`mailListMembersMachine(listIdHex:…)`),
    /// so the cache key is `(api, listIdHex)` — keying on the client alone would serve
    /// one list's members under another list's name if the view were ever reused.
    private var configuredListIdHex: String?

    public init() {}

    /// Vend the list-scoped machine from APIClient and load the first snapshot.
    /// Idempotent. `listIdHex` + `listName` come from the opened `mail-lists` row,
    /// or the placeholder id on a direct settings-rail visit with nothing
    /// selected — resolved below to the caller's first owned list
    /// (`mail-mass-mailing.md` § Per-app render status, the tui/windows shape)
    /// before vending the scoped machine.
    public func configure(api: APIClient, listIdHex: String, listName: String) async {
        guard machine == nil || configuredApi !== api || configuredListIdHex != listIdHex
        else { return }
        var resolvedListIdHex = listIdHex
        var resolvedListName = listName
        if listIdHex == MailListMembersView.placeholderListId {
            (resolvedListIdHex, resolvedListName) = await Self.resolveFirstOwnedList(
                api: api, fallbackName: listName)
        }
        do {
            let m = try await api.mailListMembersMachine(listIdHex: resolvedListIdHex, listName: resolvedListName)
            machine = m
            configuredApi = api
            configuredListIdHex = resolvedListIdHex
            snapshot = m.snapshot()
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
    }

    /// "List my lists, take the first" — a direct rail visit with nothing
    /// selected has no already-hydrated `MailListsMachine` to peek at (unlike
    /// tui, this VM is scoped to one page mount, not a session-lifetime app
    /// state), so it needs its own throwaway read, mirroring windows'
    /// `MailListMembersPanel.EnsureVmAsync`. Reserves the placeholder id (and
    /// `fallbackName`) for the genuinely-zero-lists case or a failed read —
    /// the honest empty state, not a fabricated selection.
    private static func resolveFirstOwnedList(
        api: APIClient, fallbackName: String
    ) async -> (listIdHex: String, listName: String) {
        do {
            let listsMachine = try await api.mailListsMachine()
            try await listsMachine.hydrate()
            if let first = listsMachine.snapshot().lists.first {
                return (first.listIdHex, first.friendlyName)
            }
        } catch {
            // No lists to fall back to, or the read failed — fall through to
            // the placeholder empty state rather than surface this as a
            // page-level error the user did nothing to cause.
        }
        return (MailListMembersView.placeholderListId, fallbackName)
    }

    /// Refresh the member roster from the nest (page mount).
    public func hydrate() async { await hydrateFromMachine() }

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: MailListMembersAction) async { await dispatchToMachine(action) }

    func didHydrateSuccessfully() { loaded = true }
}
