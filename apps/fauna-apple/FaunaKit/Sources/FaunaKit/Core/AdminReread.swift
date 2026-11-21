import SwiftUI

/// When an admin page reads again (`docs/goal/behavior/admin.md` § Errors &
/// edge cases, *Controls wait for their read*): on every entry to the page, and
/// — for a read that has not answered — when the nest connection comes back,
/// with no navigation. One trigger for every admin page with a read gate, on
/// both apple targets; the twin of tui's `admin::reconnect_reread_op` and web's
/// `readOnEntryAndReconnect` (`admin-reread.ts`).
///
/// An answered read is not asked again on a reconnect, so a draft typed over
/// an answered page survives; a re-read that fails after one succeeded keeps
/// the earlier answer (the shared machines' and ``PageRead``'s own rule).

/// A page view model the trigger drives: its load, and whether the read
/// deciding its read-gated controls has answered.
@MainActor
protocol AdminPageReading: AnyObject {
    /// Whether the read the page's read-gated controls wait for has resolved
    /// — the same reads they are painted from (tui's `read_answered`).
    var readAnswered: Bool { get }
    /// The page's load: build what it needs, then read.
    func configure(api: APIClient) async
}

/// Whether a connection-state change asks the page's read again: the
/// connection came back (any other state → `.connected`) and the read has not
/// answered. The first state seen (`was == nil`) only seeds. The twin of web's
/// `onReconnect`.
func adminReconnectRereads(
    was: FfiConnectionState?, now: FfiConnectionState, answered: Bool
) -> Bool {
    guard let was, was != .connected, now == .connected else { return false }
    return !answered
}

/// The trigger's state: the last connection state seen, and one serial read
/// — a page never has two reads on their way at once: a read asked while one
/// runs waits for it and runs once after it (web's `serialRead`).
@MainActor
final class AdminPageReread {
    private var last: FfiConnectionState?
    private var running = false
    private var again = false

    init() {}

    /// Every entry to the page (mount, and a navigation to the page already
    /// shown).
    func entry(_ read: @escaping @MainActor () async -> Void) async {
        await run(read)
    }

    /// A connection-state change (the first one seeds).
    func connection(
        _ now: FfiConnectionState, answered: Bool,
        _ read: @escaping @MainActor () async -> Void
    ) async {
        let was = last
        last = now
        guard adminReconnectRereads(was: was, now: now, answered: answered) else { return }
        await run(read)
    }

    private func run(_ read: @escaping @MainActor () async -> Void) async {
        if running {
            again = true
            return
        }
        running = true
        defer { running = false }
        repeat {
            again = false
            await read()
        } while again
    }
}

private struct AdminReadModifier<VM: AdminPageReading>: ViewModifier {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    let vm: VM
    let reloadToken: Int
    let after: (@MainActor () -> Void)?
    @State private var reread = AdminPageReread()

    private func read() async {
        guard let client else { return }
        await vm.configure(api: client.api)
        after?()
    }

    func body(content: Content) -> some View {
        content
            .task(id: reloadToken) { await reread.entry(read) }
            .onChange(of: client?.connectionState, initial: true) { _, now in
                guard let now else { return }
                Task { await reread.connection(now, answered: vm.readAnswered, read) }
            }
    }
}

extension View {
    /// Load `vm`'s page on every entry (`reloadToken`: macOS passes the shell's
    /// `navGeneration`; iOS leaves it 0, the NavigationLink re-mounting the
    /// view) and read it again when the nest connection comes back while its
    /// read has not answered. `after` runs once each load returns.
    func adminReadOnEntryAndReconnect<VM: AdminPageReading>(
        _ vm: VM, reloadToken: Int, after: (@MainActor () -> Void)? = nil
    ) -> some View {
        modifier(AdminReadModifier(vm: vm, reloadToken: reloadToken, after: after))
    }
}

// MARK: - Each read-gated page's answer

// The Nest and Tiers pages compile in the Fauna Kids build too, so the trigger
// above and their two conformances stay outside the excision; every other
// page's view model is a plane the kids flavor excises.
extension AdminNestVM: AdminPageReading {}
extension AdminTiersVM: AdminPageReading {}

#if !FAUNA_KIDS
extension ReadGatedToggleVM {
    /// The DAV page's `get_mail_config` read — the shared machine's `read_state`.
    var readAnswered: Bool { machine?.readState() == .resolved }
}

extension AdminCalendarVM: AdminPageReading {}
extension AdminContactsVM: AdminPageReading {}
extension AdminFilesVM: AdminPageReading {}

extension AdminMailVM: AdminPageReading {}
extension ForwardersVM: AdminPageReading {}
extension BridgeApprovalVM: AdminPageReading {}
extension AdminWebVM: AdminPageReading {}
#endif // !FAUNA_KIDS
