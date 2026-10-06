import SwiftUI

/// Thin SwiftUI-friendly proxy over the shared `ConnectedAppsMachine`
/// (UniFFI, `libs/fauna-client-connected-apps` via `libs/fauna-ffi`) — backs the
/// Settings → **Connected apps** page (`docs/goal/ui/connected-apps.md`), macOS +
/// iOS, one FaunaKit VM. The roster's composition, the scope words, the class
/// badge key, *lasts-until* and **which verb revokes a row** are all the
/// machine's: a row's `key` is opaque here and the app never picks a revoke
/// verb. This VM owns the machine, mirrors its snapshot, forwards each gesture,
/// and holds only the page-local state a gesture needs (the code field, the
/// armed revoke, the revealed mail secrets). Reference painter: tui
/// `apps/fauna-tui/src/settings/connected_apps.rs`; the closest port is
/// android's `ConnectedAppsVM`.
///
/// **The mail app passwords are rows of this roster.** The machine takes the
/// session's Mail & Calendar machine and reads, revokes and reveals them through
/// it; that machine's `credential_management_reachable` gate is only known
/// after its own `hydrate()`, so ``visit(api:handle:)`` hydrates a fresh one
/// before it builds the connected-apps machine over it.
///
/// **A visit starts unread** (`connected-apps.md` § Errors & edge cases): rows
/// are nest state read on every open, so ``visit(api:handle:)`` builds a fresh
/// machine, which paints neither rows nor the empty state until its own read has
/// returned — never the previous visit's list while the fresh one is in flight —
/// and takes every revealed secret off the screen.
@MainActor @Observable
public final class ConnectedAppsVM {
    /// The last snapshot painted; `nil` until the machine is built.
    public private(set) var snapshot: ConnectedAppsSnapshot?

    /// One-time connect/build failure (`api.connectedAppsMachine` threw); page
    /// read/write failures live on the snapshot's `error`. Both reach the page's
    /// `error-message` through ``errorMessage``.
    public private(set) var connectError: String?

    /// The *Connect an app* field's draft.
    public var code = ""

    /// The row key whose inline revoke confirm is open.
    public private(set) var revokeArmed: String?

    /// The mail app-password secrets currently shown, by row key. Empty by
    /// default: the secret is never in the snapshot, so a row shows one only
    /// after the user asks and the on-demand read resolves. Keyed by row key,
    /// never by index, so a roster that re-orders under a fresh snapshot cannot
    /// show one password's secret against another row.
    public private(set) var revealed: [String: String] = [:]

    private var machine: ConnectedAppsMachine?
    private var handle = ""
    private let observerBox = ConnectedAppsObserverBox()

    /// Which visit is current. A re-selected rail page (or a `fauna://consent`
    /// route landing on the page already on screen) restarts the visit through
    /// `.task(id:)`, but SwiftUI's cancellation does not stop a UniFFI `await`
    /// already in flight — so an older visit would otherwise keep writing the
    /// machine and snapshot a newer visit owns. Every await below re-checks.
    private var visitGeneration = 0

    #if DEBUG
    /// The live instance the page is rendering off, for the `open_route` TestAgent
    /// command's wait. **Weak** — the slot must never keep a dismissed page's VM
    /// alive. `#if DEBUG` per convention 15.
    @MainActor
    public static weak var liveInstanceForTest: ConnectedAppsVM?
    #endif

    public init() {}

    /// The page-level `error-message`: the connect failure first, else the
    /// snapshot's localized `error`.
    public var errorMessage: String? {
        firstNonNil(connectError, snapshot?.error.map(renderLocalizedText))
    }

    /// Start a visit: drop the page-local drafts and the last visit's snapshot,
    /// build a fresh machine over a freshly hydrated mail machine, and read the
    /// roster. `handle` is the session's logged-in handle, substituted into each
    /// mail row's login by the shared `resolveMuaUsername`.
    public func visit(api: APIClient, handle: String) async {
        #if DEBUG
        Self.liveInstanceForTest = self
        #endif
        visitGeneration += 1
        let generation = visitGeneration
        self.handle = handle
        code = ""
        revokeArmed = nil
        revealed = [:]
        snapshot = nil
        connectError = nil
        machine = nil

        // A mail machine that fails to hydrate costs the roster its mail rows,
        // not the page — the machine records a real failure in its own snapshot.
        let mail = try? await api.mailSettingsMachine()
        if let mail { try? await mail.hydrate() }
        guard generation == visitGeneration else { return }

        observerBox.target = self
        let built: ConnectedAppsMachine
        do {
            built = try await api.connectedAppsMachine(observer: observerBox, mail: mail)
        } catch {
            guard generation == visitGeneration else { return }
            connectError = DisplayError.message(error)
            return
        }
        guard generation == visitGeneration else { return }
        machine = built
        snapshot = built.snapshot()
        await built.refresh()
        guard generation == visitGeneration else { return }
        snapshot = built.snapshot()
        await openStagedHandoff()
    }

    fileprivate func onMachineChanged() {
        if let machine { snapshot = machine.snapshot() }
    }

    /// Open the staged `fauna://consent/<request_uri>` route, if any, through the
    /// shared machine (`ConsentHandoff`), and clear it only after the open has
    /// returned — a waiter (the e2e `open_route` command) then knows the card is
    /// painted. Safe to call whenever the page is on screen: with nothing staged
    /// it is a no-op.
    public func openStagedHandoff() async {
        guard let machine, let requestUri = ConsentHandoff.shared.peekPending() else { return }
        let generation = visitGeneration
        await machine.openHandoff(requestUri: requestUri)
        // A newer visit owns the page now: leave the request staged so ITS open
        // (a fresh machine) reveals the card and clears it.
        guard generation == visitGeneration else { return }
        snapshot = machine.snapshot()
        ConsentHandoff.shared.finishOpen(requestUri)
    }

    // MARK: - Gestures

    public func submitCode() async {
        let typed = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !typed.isEmpty else { return }
        code = ""
        await run { await $0.submitCode(code: typed) }
    }

    public func resolveRequest(consentIdHex: String, approved: Bool) async {
        await run { await $0.resolveRequest(consentIdHex: consentIdHex, approved: approved) }
    }

    public func blockRequest(consentIdHex: String) async {
        await run { await $0.blockRequest(consentIdHex: consentIdHex) }
    }

    public func unblock(clientId: String) async {
        await run { await $0.unblock(clientId: clientId) }
    }

    public func armRevoke(_ key: String) { revokeArmed = key }
    public func cancelRevoke() { revokeArmed = nil }

    public func confirmRevoke(_ key: String) async {
        revokeArmed = nil
        revealed[key] = nil
        await run { await $0.revoke(key: key) }
    }

    /// Show the row's mail secret, or hide it if shown.
    public func toggleReveal(_ key: String) async {
        if revealed[key] != nil {
            revealed[key] = nil
            return
        }
        guard let secret = await readSecret(key) else { return }
        revealed[key] = secret
    }

    /// Read a mail secret on demand for the clipboard, without painting it: Copy
    /// is independent of the reveal toggle, so the secret reaches the clipboard
    /// without being drawn on a screen someone else can read. A failed read
    /// leaves the machine's own error on the snapshot.
    public func readSecret(_ key: String) async -> String? {
        guard let machine else { return nil }
        let secret = await machine.revealSecret(key: key)
        snapshot = machine.snapshot()
        return secret
    }

    /// The concrete login for a mail row, with only `{handle}` left for the
    /// shared `resolveMuaUsername` to substitute (never a locally built address).
    public func muaUsername(_ mail: MailAppPassword) -> String {
        resolveMuaUsername(muaUsername: mail.muaUsername, handle: handle)
    }

    private func run(_ gesture: (ConnectedAppsMachine) async -> Void) async {
        guard let machine else { return }
        await gesture(machine)
        snapshot = machine.snapshot()
    }
}

/// Trampoline conforming to UniFFI's `ConnectedAppsObserver`. The machine takes
/// its observer at construction time, so late-binding via `target` lets the VM
/// register itself once the page's visit starts. Mirrors `AtprotoSettingsObserverBox`.
final class ConnectedAppsObserverBox: ConnectedAppsObserver, @unchecked Sendable {
    weak var target: ConnectedAppsVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onMachineChanged() }
    }
}
