import Foundation

/// Where THIS install's push record lives — the user's own opt-in and which
/// actor's nest row is live.
///
/// The record itself, and every rule about it, is the shared
/// `fauna_client_push::registration` machine's (`common.md` § Push Notifications
/// → *Registration*): a re-arm never opts a device in, a Disable clears the bit
/// the moment it is issued, a leave gesture drops the row and keeps the bit.
/// This type only names the file and reads it; nothing here decides anything.
///
/// **Install-scoped by declared design**, not account-scoped
/// (`docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy,
/// class 2, which requires the reason be stated): the record describes this
/// install's push transport — one device token, one APNs registration, one
/// shared-keychain P-256 key pair — and not any one signed-in identity. So the
/// file sits directly under the install base (`AccountStateDir.base`), outside
/// every actor scope, and `ActorScope`'s teardown does not erase it: switching
/// accounts must not silently re-open a push channel the user closed, nor close
/// one they left open.
///
/// The shared machine records the actor beside the bit, so a leave-drop issues
/// nothing on an install that never subscribed and forgets the row it removed.
public struct PushIntentStore: Sendable {
    /// The record's file name under the install base — the same name tui uses
    /// under its own config base.
    public static let fileName = "push-intent.cbor"

    /// The record's path.
    public let path: String

    public init(path: String = AccountStateDir.base.appendingPathComponent(fileName).path) {
        self.path = path
    }

    /// The stored record. Reads no connection; an absent or unreadable file is
    /// "not opted in" — the safe direction, since a re-arm never opts in.
    public var intent: FfiPushIntent { pushIntent(intentPath: path) }

    /// Did the user opt this install in, and not since turn it off? What the
    /// Settings toggle renders.
    public var isOptedIn: Bool { intent.optedIn }
}
