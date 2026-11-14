// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
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

    /// The visit whose machine has returned its first read — gestures act only
    /// on that machine (``readyMachine()``).
    private var readyGeneration = -1
    private var readyWaiters: [CheckedContinuation<Void, Never>] = []

    /// This visit's number at the route door (``ConsentHandoff/registerVisit()``),
    /// which hands a staged request only to the newest visit of any instance.
    private var doorVisit = 0

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
        doorVisit = ConsentHandoff.shared.registerVisit()
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
            wakeReadyWaiters()
            return
        }
        guard generation == visitGeneration else { return }
        machine = built
        // The fresh-visit reset of the trust facet's lenses, beside the page's
        // other drafts (`connected-apps.md` § The trust facet and history lens
        // → *Lens state*); a fresh machine starts on Now, and the reset keeps
        // that true should the machine ever outlive a visit.
        built.resetLenses()
        snapshot = built.snapshot()
        await built.refresh()
        guard generation == visitGeneration else { return }
        snapshot = built.snapshot()
        readyGeneration = generation
        wakeReadyWaiters()
        await openStagedHandoff()
    }

    /// The current visit's machine once its first read has returned, or `nil`
    /// when the visit could not build one. The page's fields render before the
    /// machine exists, so a gesture made in that window (a code typed the moment
    /// the field shows) waits for it here instead of being dropped; and an
    /// action taken after the read can never be overwritten by that older read.
    private func readyMachine() async -> ConnectedAppsMachine? {
        while true {
            if let machine, readyGeneration == visitGeneration { return machine }
            if connectError != nil { return nil }
            await withCheckedContinuation { readyWaiters.append($0) }
        }
    }

    private func wakeReadyWaiters() {
        let waiters = readyWaiters
        readyWaiters = []
        for waiter in waiters { waiter.resume() }
    }

    fileprivate func onMachineChanged() {
        if let machine { snapshot = machine.snapshot() }
    }

    /// Open the staged `fauna://consent/<request_uri>` route, if any, through the
    /// shared machine (`ConsentHandoff`), and clear it only once the card is
    /// painted — a waiter (the e2e `open_route` command) then knows it is on
    /// screen. Safe to call whenever the page is on screen: with nothing staged
    /// it is a no-op.
    public func openStagedHandoff() async {
        guard let machine else { return }
        let door = ConsentHandoff.shared
        let visit = doorVisit
        if let requestUri = door.beginOpen(visit: visit) {
            await machine.openHandoff(requestUri: requestUri)
            if door.finishOpen(requestUri, visit: visit) { snapshot = machine.snapshot() }
            return
        }
        // An older visit opened it — the nest has spent the request, so it is
        // never opened again: re-read this visit's tray, where the nest lists
        // the card.
        guard let requestUri = await door.awaitSpent(visit: visit) else { return }
        await machine.refresh()
        if doorVisit == visit { snapshot = machine.snapshot() }
        door.finishReread(requestUri)
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

    /// Choose the folder on a bare folder-plane request card
    /// (`connected-apps-request-folder`; `authorization-server.md` § Consent →
    /// *The card chooses the folder*). Local to this visit — the machine keeps
    /// the choice and the card's Approve mints over it — so this is synchronous:
    /// the card only exists once the machine has read, and a pick is painted in
    /// the same frame the user made it.
    public func setFolder(consentIdHex: String, id: Int64) {
        guard let machine else { return }
        machine.setFolder(consentIdHex: consentIdHex, id: id)
        snapshot = machine.snapshot()
    }

    public func blockRequest(consentIdHex: String) async {
        await run { await $0.blockRequest(consentIdHex: consentIdHex) }
    }

    public func unblock(clientId: String) async {
        await run { await $0.unblock(clientId: clientId) }
    }

    /// The account's external-apps switch (`atproto-external-apps-enable`). The
    /// machine writes it and the page paints the re-read snapshot, never a local
    /// guess.
    public func setExternalAppsEnabled(_ enabled: Bool) async {
        await run { await $0.setExternalAppsEnabled(enabled: enabled) }
    }

    public func armRevoke(_ key: String) { revokeArmed = key }
    public func cancelRevoke() { revokeArmed = nil }

    public func confirmRevoke(_ key: String) async {
        revokeArmed = nil
        revealed[key] = nil
        await run { await $0.revoke(key: key) }
    }

    /// Open the row's per-operation record, or close it if it is the one open
    /// (`connected-apps.md` § Layout & flow, region 3 — at most one is open;
    /// opening another row's closes this one). The open is the machine's read
    /// of the custodians' record; the app keeps none of its own. A fresh visit
    /// builds a fresh machine, so it always starts with the record closed.
    public func toggleRecord(_ key: String) async {
        if snapshot?.record?.key == key {
            await run { $0.closeRecord() }
        } else {
            await run { await $0.openRecord(key: key) }
        }
    }

    /// Show the Now or History lens of a keyed principal's trust facet
    /// (`connected-apps.md` § The trust facet and history lens) — page-local
    /// state on the machine, no nest call.
    public func setLens(_ key: String, _ lens: TrustLens) async {
        await run { $0.setLens(key: key, lens: lens) }
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
        guard let machine = await readyMachine() else { return nil }
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
        guard let machine = await readyMachine() else { return }
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
#endif // !FAUNA_KIDS
