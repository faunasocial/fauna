import Foundation

/// The home-screen widget's data: the unread count the app's own conversations
/// list shows (`apps/common.md` § Home-screen widget), as the app last published
/// it, and when.
///
/// **Why a snapshot and not a fetch in the widget.** A WidgetKit widget is a
/// separate sandboxed process, and a sandboxed extension must never reach the
/// identity seed (`installers/macos.md` § Identifier domain) — so the widget
/// holds no credential and opens no connection — nor could it open the sealed
/// messages the count is folded from. The app writes this snapshot into the
/// shared app-group container; the widget only reads it. That is android's
/// shape (the app writes Glance state, `FaunaWidget` renders it), with the
/// app-group container standing in for Glance's per-widget DataStore.
public struct UnreadSnapshot: Codable, Equatable, Sendable {
    public let count: Int
    public let updatedAt: Date

    public init(count: Int, updatedAt: Date) {
        self.count = count
        self.updatedAt = updatedAt
    }
}

/// The one file the app writes and the widget reads, in a directory the caller
/// names. The shipped location is ``appGroup()``; an e2e launch names its own
/// (FaunaKit's `WidgetUnreadPublisher` — the app-group container resolves to the
/// real home even under `CFFIXED_USER_HOME`, so a test must never write there).
public struct UnreadSnapshotStore: Sendable {
    public static let fileName = "unread.json"

    public let directory: URL

    public init(directory: URL) {
        self.directory = directory
    }

    public var fileURL: URL { directory.appendingPathComponent(Self.fileName) }

    /// `<shared app-group container>/Widget`, or `nil` when the container is
    /// unreachable (a missing `application-groups` entitlement — a packaging
    /// bug; the widget then shows the empty state rather than crashing).
    public static func appGroup() -> UnreadSnapshotStore? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: AppleIdentifiers.appGroup)
            .map { UnreadSnapshotStore(directory: $0.appendingPathComponent("Widget")) }
    }

    /// The last snapshot written, or `nil` when there is none (never published,
    /// signed out, or an unreadable file — all render as the empty state).
    public func load() -> UnreadSnapshot? {
        guard let data = try? Data(contentsOf: fileURL) else { return nil }
        return try? Self.decoder.decode(UnreadSnapshot.self, from: data)
    }

    /// Replace the snapshot. Atomic, so the widget never reads a torn file.
    public func save(_ snapshot: UnreadSnapshot) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Self.encoder.encode(snapshot).write(to: fileURL, options: .atomic)
    }

    /// Forget the snapshot — the outgoing account's count must never stay on
    /// the home screen (`account-scoping.md` § The scoping taxonomy: the count
    /// is an account-scoped replica). A missing file is already cleared.
    public func clear() {
        try? FileManager.default.removeItem(at: fileURL)
    }

    private static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .iso8601
        return e
    }()

    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .iso8601
        return d
    }()
}
