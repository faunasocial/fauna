import Foundation
import WidgetKit

/// Publishes the home-screen widget's unread count (macOS + iOS, shared FaunaKit —
/// priority #2): the apple half of `apps/common.md` § Home-screen widget, mechanism
/// in `apps/ios.md` § Home-screen widget.
///
/// **The number is the conversations list's own number.** `ConversationsVM`'s
/// manager observer — the one place every render of the list reads through —
/// calls ``publish(unread:)`` with ``total(of:)`` over the shared
/// `fauna_conversations` snapshot's threads, the per-thread `unreadCount` the list
/// renders, summed. That is linux's `sum_unread` feeding its launcher badge on each
/// snapshot tick: one computation, so the widget can never show a number the app
/// would not, and never a second count query.
///
/// **Why a snapshot and not a fetch in the widget.** The widget is a sandboxed
/// extension that must never reach the identity seed, so it holds no credential and
/// could not open the sealed messages the count is folded from. The app writes an
/// ``UnreadSnapshot`` into the shared app-group container; the widget only reads it.
///
/// **Background currency.** Nothing here polls: the snapshot moves whenever the
/// conversations snapshot does. On macOS that is whenever the app runs — its
/// receive loop keeps the manager current with the window closed. iOS suspends a
/// backgrounded app, so there `BackgroundScheduler`'s widget `BGAppRefreshTask`
/// runs one receive pass, whose ingest ticks the same observer. A QUIT macOS app
/// publishes nothing: the declared gap in `apps/macos.md` § Implementation status
/// today.
@MainActor
public final class WidgetUnreadPublisher {
    public static let shared = WidgetUnreadPublisher()

    private let store: () -> UnreadSnapshotStore?
    private let reload: () -> Void
    private let now: () -> Date
    /// The count last written — the observer fires on every manager change (a
    /// compose keystroke included), and only a changed count is worth a write
    /// and a WidgetKit reload.
    private var lastPublished: Int?

    init(
        store: @escaping () -> UnreadSnapshotStore? = WidgetUnreadPublisher.resolvedStore,
        reload: @escaping () -> Void = WidgetUnreadPublisher.reloadWidget,
        now: @escaping () -> Date = Date.init
    ) {
        self.store = store
        self.reload = reload
        self.now = now
    }

    /// The conversations list's unread total — the number the widget shows.
    public nonisolated static func total(of threads: [ThreadSummary]) -> Int {
        threads.reduce(0) { $0 + Int($1.unreadCount) }
    }

    /// Write `unread` for the widget and ask WidgetKit to redraw, unless it is
    /// the count already written.
    public func publish(unread: Int) {
        guard unread != lastPublished, let store = store() else { return }
        do {
            try store.save(UnreadSnapshot(count: unread, updatedAt: now()))
        } catch {
            logMessage(level: .warn, target: "fauna.widget",
                       message: "unread snapshot write failed: \(error)")
            return
        }
        lastPublished = unread
        reload()
    }

    /// Forget the count: the outgoing account's number must not stay on the home
    /// screen after a switch or sign-out (`ActorScope.resetSharedState`). Zeroed
    /// now rather than left for the incoming account's first tick — android's
    /// `refoldForAccountSwitch` ("a stale number is worse than none").
    public func clear() {
        lastPublished = nil
        store()?.clear()
        reload()
    }

    /// The shipped location is the shared app-group container. An e2e launch
    /// writes only where the harness named (`FAUNA_E2E_WIDGET_DIR`), and nowhere
    /// if it named nothing — the container resolves to the real home even under
    /// `CFFIXED_USER_HOME`, i.e. machine-global state a test must never touch.
    static func resolvedStore() -> UnreadSnapshotStore? {
        #if DEBUG
        if FaunaE2E.isActive {
            return E2eEnv.widgetDir.map { UnreadSnapshotStore(directory: URL(fileURLWithPath: $0)) }
        }
        #endif
        return UnreadSnapshotStore.appGroup()
    }

    static func reloadWidget() {
        WidgetCenter.shared.reloadTimelines(ofKind: AppleIdentifiers.unreadWidgetKind)
    }
}
