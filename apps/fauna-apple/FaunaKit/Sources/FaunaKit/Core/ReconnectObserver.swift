import SwiftUI
import Combine

public extension View {
    /// Shared body for the single-notification, no-payload modifiers below —
    /// each keeps its own name and doc comment (a distinct semantic seam, not
    /// interchangeable), only the Combine plumbing was byte-identical five times.
    private func onFaunaNotification(_ name: Notification.Name, action: @escaping () async -> Void) -> some View {
        onReceive(NotificationCenter.default.publisher(for: name)) { _ in
            Task { await action() }
        }
    }

    /// Re-run `action` whenever the WS-RPC client reconnects (`.faunaReconnected`,
    /// posted by `FaunaClient`'s reconnect observer). Each live surface attaches
    /// this to re-pull its snapshot on reconnect — the feed has no poll backstop,
    /// so a post that arrived while disconnected would otherwise stay invisible
    /// until a manual refresh (`transport.md` § Push events; mirrors linux
    /// `WsEvent::Reconnected`). Centralises the notification name + Combine bridge
    /// in one place so adding a new re-hydrating surface is a single `.onReconnect`
    /// line. Only fires while the view is on screen; off-screen surfaces re-pull
    /// via their own `.task`/`.onAppear` when navigated to.
    func onReconnect(_ action: @escaping () async -> Void) -> some View {
        onFaunaNotification(.faunaReconnected, action: action)
    }

    /// Run `action` when a client's WS-RPC supervisor stops on a session-ending
    /// verdict (`.faunaSessionEnding`, posted by `FaunaClient`'s connection-state
    /// observer). Attached once, at the app root, where the per-platform
    /// teardown and `runLaunch()` live. A torn-down client never posts — its
    /// observer is cancelled by `shutdown()` — and a verdict racing an account
    /// switch meets the switch's own `switchInFlight` guard.
    func onSessionEnding(
        _ action: @escaping @MainActor (FfiSessionEndingVerdict) async -> Void
    ) -> some View {
        onReceive(NotificationCenter.default.publisher(for: .faunaSessionEnding)) { note in
            guard let verdict = note.userInfo?[FaunaSessionEnding.verdictKey]
                    as? FfiSessionEndingVerdict else { return }
            Task { @MainActor in await action(verdict) }
        }
    }

    /// Re-run `action` whenever a `fauna.notification` push arrives
    /// (`.faunaNotificationReceived`, posted by `FaunaClient`'s push observer off
    /// the one authenticated socket). The notifications surface attaches this to
    /// grow a new row live, with no navigation — the apple twin of android's
    /// `notificationTick` collector and linux's `fetch_notifications()` on
    /// `PushEvent::Notification` (`transport.md` § Push events). Only fires while
    /// the view is on screen; an off-screen surface re-pulls via its own
    /// `.task`/`.onAppear` when navigated to (and a dropped push is recovered by
    /// the reconnect sweep — a push is a hint, never the only path to the value).
    func onPushNotification(_ action: @escaping () async -> Void) -> some View {
        onFaunaNotification(.faunaNotificationReceived, action: action)
    }

    /// Re-run `action` whenever a `fauna.knock` (contact-request) push arrives
    /// (`.faunaKnockReceived`, posted by `FaunaClient`'s dedicated knock observer off
    /// the one authenticated socket). The contacts surface attaches this to grow a
    /// pending-knock row live, with no navigation — the apple twin of windows'
    /// `KnockReceived` event and android's `knockTick` collector (`transport.md`
    /// § Push events). `fauna.knock` is a dedicated broker kind, not a push-event
    /// variant, so it needs its own seam (`onPushNotification` never sees it). Only
    /// fires while the view is on screen; an off-screen surface re-pulls via its own
    /// `.task`/`.onAppear`, and a dropped knock is recovered by the reconnect sweep —
    /// a push is a hint, never the only path to the value.
    func onKnockReceived(_ action: @escaping () async -> Void) -> some View {
        onFaunaNotification(.faunaKnockReceived, action: action)
    }

    /// Re-run `action` whenever a `fauna.calendar.changed` push arrives
    /// (`.faunaCalendarChanged`, posted by `FaunaClient`'s push observer). The
    /// Events surface attaches this to re-pull live, cutting the
    /// quick-appearance poll's latency down to push latency — the apple twin
    /// of android's `calendarChangedTick` collector (`transport.md` § Push
    /// events). Only fires while the view is on screen; the poll stays as
    /// backstop for a dropped push or an off-screen write.
    func onCalendarChanged(_ action: @escaping () async -> Void) -> some View {
        onFaunaNotification(.faunaCalendarChanged, action: action)
    }

    /// Re-run `action` whenever a `fauna.addressbook.changed` push arrives
    /// (`.faunaAddressBookChanged`, posted by `FaunaClient`'s push observer). The
    /// Address Book segment attaches this to re-read the book list and the open
    /// book's cards live — the apple twin of android's `addressBookChangedTick`
    /// collector (`transport.md` § Push events). Only fires while the view is on
    /// screen, which IS the segment gate here: `AddressBookView` is mounted only
    /// while the Address Book segment is showing.
    func onAddressBookChanged(_ action: @escaping () async -> Void) -> some View {
        onFaunaNotification(.faunaAddressBookChanged, action: action)
    }

    /// Re-run `action` with the changed folder's name whenever a `fauna.sync.changed`
    /// push arrives (`.faunaFolderDeviceActivityChanged`, posted by `FaunaClient`'s
    /// push observer with the folder name as `object`). The Folders page's per-set
    /// device-activity section attaches this and gates on the name matching its own
    /// (currently-expanded) set — never a blanket refetch for a collapsed row (the
    /// apple twin of tui's `device_activity_resync_op` / windows' `_expandedFolder
    /// == folder` guard; `transport.md` § Push events). Unlike the other modifiers
    /// here this one passes the payload through rather than discarding it, since the
    /// gate needs to know which set changed.
    func onFolderDeviceActivityChanged(_ action: @escaping (String) async -> Void) -> some View {
        onReceive(NotificationCenter.default.publisher(for: .faunaFolderDeviceActivityChanged)) { note in
            guard let folder = note.object as? String else { return }
            Task { await action(folder) }
        }
    }

    /// Re-run `action` whenever a `fauna.sync.changed` push arrives
    /// (`.faunaMediaChanged`, posted by `FaunaClient`'s push observer alongside
    /// `.faunaFolderDeviceActivityChanged`). The Media page attaches this to
    /// re-read its cross-set all-media aggregate live — the apple twin of linux's
    /// `media_page_is_visible`-gated `PushEvent::SyncChanged` arm and tui's
    /// `StaleSurfaces::media` (`transport.md` § Push events; `media.md` § Staying
    /// live while the page is open). Only fires while the view is on screen,
    /// which IS the page gate here (unlike `onFolderDeviceActivityChanged`,
    /// there's no per-folder name to also check — `fauna.media.list` is a
    /// cross-set aggregate).
    func onMediaChanged(_ action: @escaping () async -> Void) -> some View {
        onFaunaNotification(.faunaMediaChanged, action: action)
    }

    /// Re-run `action` whenever the account registry is mutated from outside the
    /// switcher's own view model (`.faunaAccountRegistryChanged` — today the admin
    /// auto-default). Unlike the four push seams above this is a purely LOCAL
    /// signal: the writer is in-process, so a missed post has no reconnect sweep to
    /// recover it, which is precisely why the write posts unconditionally rather
    /// than relying on the switcher's on-appear reload to catch up. Only fires
    /// while the view is on screen; an off-screen switcher re-reads in its own
    /// `.task { vm.reload() }` when navigated to.
    func onAccountRegistryChanged(_ action: @escaping () async -> Void) -> some View {
        onFaunaNotification(.faunaAccountRegistryChanged, action: action)
    }
}
