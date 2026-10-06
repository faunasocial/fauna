// Built for and USED BY every Apple platform: macOS registers for APNs through
// this same type as of the macOS push leg (settings.md § Push notifications),
// so the "the macOS app doesn't use it" caveat this header used to carry is
// gone. The per-platform halves — which framework asks for a device token, and
// where the OS notification settings live — are the two statics at the bottom;
// everything else is platform-agnostic. The delegate callbacks that feed
// `didRegisterForRemoteNotifications` are each app's own `AppDelegate`
// (`UIApplicationDelegate` on iOS, `NSApplicationDelegate` on macOS).
import Foundation
import CryptoKit
import Security
import UserNotifications
#if os(macOS)
import AppKit
#else
import UIKit
#endif

// MARK: - Data base64URL extension

extension Data {
    /// URL-safe base64 encoding (no padding) per RFC 4648 Section 5.
    func base64URLEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

// MARK: - PushRegistering

/// The shared push-registration machine as `PushManager` drives it
/// (`fauna_client_push::registration` over `FfiPushRegistration`). A protocol
/// only so the unit tests can stand a recorder in for the nest; the one
/// production conformer is the FFI handle.
public protocol PushRegistering: Sendable {
    func enable(subscription: FfiPushEndpoint) async throws
    func rearm(subscription: FfiPushEndpoint) async throws -> Bool
    func disable() async throws
    func dropActorRow() async throws
    func announcePresence() async
}

extension FfiPushRegistration: PushRegistering {}

// MARK: - PushManager

/// The Apple half of push registration — the OS permission, the APNs device
/// token and the Web Push key pair — on macOS and iOS alike. Everything
/// stateful about *whether this install is opted in* is the shared machine's
/// (``PushRegistering``): this type hands it the `apns` subscription it built
/// in the token callback and renders what it stored.
///
/// Keys are stored in the shared keychain (App Group) so the Notification Service Extension
/// can decrypt incoming payloads.
@MainActor @Observable
public class PushManager {
    public enum Permission: String {
        case notDetermined
        case authorized
        case denied
        case provisional
    }

    public private(set) var permission: Permission = .notDetermined

    /// Did the user opt this install in? The stored bit, re-read from the
    /// shared record after every toggle — never set from the click, so it is
    /// evidence of what persisted. What the Settings toggle renders, and the
    /// *only* input that may open the launch-time re-registration gate
    /// (``shouldRegisterAtLaunch``).
    public private(set) var isOptedIn: Bool

    /// A toggle is in flight (its synchronous half — the permission prompt, a
    /// Disable's nest call); the control disables itself meanwhile.
    public private(set) var isWorking = false

    public private(set) var lastError: String?

    private let api: APIClient
    private let deviceId: String
    private let intent: PushIntentStore
    private let hostAvailable: Bool

    /// The device token now on its way answers the user's Enable, not a launch
    /// re-arm — the one thing that decides whether the token callback may set
    /// the bit (`enable`) or only re-register under a bit already set
    /// (`rearm`, which never opts a device in).
    private var enablePending = false

    // Shared keychain constants (owner: `AppleIdentifiers`)
    private static let accessGroup = AppleIdentifiers.appGroup
    private static let keychainService = AppleIdentifiers.KeychainService.push
    private static let privateKeyAccount = "push_p256_private"
    private static let authSecretAccount = "push_auth_secret"

    /// `hostAvailable` is `NotificationHost.isAvailable` everywhere but the
    /// unit tests, which cannot be a provisioned `.app`.
    public init(
        api: APIClient, deviceId: String, intent: PushIntentStore = PushIntentStore(),
        hostAvailable: Bool = NotificationHost.isAvailable
    ) {
        self.api = api
        self.deviceId = deviceId
        self.intent = intent
        self.hostAvailable = hostAvailable
        self.isOptedIn = intent.isOptedIn
    }

    private func registration() async throws -> any PushRegistering {
        try await api.pushRegistration(deviceId: deviceId, intentPath: intent.path)
    }

    // MARK: - Session start

    /// The signed-in session's manager — the instance each app's delegate
    /// forwards the APNs token callbacks to. The Settings control must drive
    /// THIS one: its Enable completes in that callback, so a private instance
    /// would wait for an answer delivered elsewhere. Set by
    /// ``onSessionStart()``; weak, so a torn-down session leaves nothing
    /// behind.
    public private(set) static weak var sessionManager: PushManager?

    /// Authenticated launch and every identity settle (an in-app account
    /// switch re-runs it): re-read the stored bit, announce which device this
    /// connection serves, and re-ask APNs for a token only when this install
    /// already opted in — the token callback then re-arms the row under
    /// whoever is signed in. Best-effort throughout.
    public func onSessionStart() async {
        Self.sessionManager = self
        isOptedIn = intent.isOptedIn
        // The id announced is the id the row is keyed under: both are
        // `deviceId`, handed to the one `FfiPushRegistration` (`common.md`
        // § Registration → *Every connection announces*).
        if let registration = try? await registration() {
            await registration.announcePresence()
        }
        await checkPermission()
        if shouldRegisterAtLaunch {
            Self.registerForRemoteNotifications()
        }
    }

    // MARK: - The launch gate

    /// Should this launch ask APNs for a device token again?
    ///
    /// **Only when the user already opted this install in.** The OS permission
    /// alone is not the question and never was: it says a banner may be posted,
    /// not that the nest may push here, and it survives a Disable untouched
    /// (revoking it is System Settings' job, not ours). Gating on permission
    /// alone therefore undid every Disable at the next launch. The shared
    /// machine holds the same line a second time: the token callback of a
    /// launch calls `rearm`, which subscribes nothing while the bit is clear.
    ///
    /// One named decision rather than two hand-rolled `&&`s in two app targets,
    /// for the reason `PushSectionState` gives for the render: a `#if
    /// os(iOS)`-shaped divergence between `FaunaMacApp` and `FaunaApp` is
    /// compiled by nothing that would notice it drifting.
    public var shouldRegisterAtLaunch: Bool {
        Self.shouldRegisterAtLaunch(isOptedIn: isOptedIn, permission: permission)
    }

    /// The pure half, so `PushManagerLaunchGateTests` can assert it without an
    /// `APIClient` or a notification centre.
    nonisolated public static func shouldRegisterAtLaunch(
        isOptedIn: Bool,
        permission: Permission
    ) -> Bool {
        isOptedIn && permission == .authorized
    }

    // MARK: - The toggle

    /// The Settings toggle (`settings.md` § Push notifications). On: ask the OS
    /// for permission if needed, then for a device token — the row is
    /// registered and the bit set in the token callback, so a failure anywhere
    /// leaves the toggle off with ``lastError`` saying why. Off: unregister;
    /// the OS permission is left alone.
    public func setOptIn(_ on: Bool) async {
        guard !isWorking else { return }
        isWorking = true
        defer { isWorking = false }
        lastError = nil
        if on {
            await beginEnable()
        } else {
            await disable()
        }
    }

    private func beginEnable() async {
        // A build that cannot reach the notification centre at all — the bare
        // debug binary — still renders the toggle and answers with the same
        // inline line every other failure gets ("the platform's push APIs
        // unavailable", settings.md).
        guard hostAvailable else {
            lastError = L.status.notifications.unavailable
            return
        }
        guard await requestPermission() else {
            if lastError == nil { lastError = L.status.notifications.permissionDenied }
            return
        }
        enablePending = true
        Self.registerForRemoteNotifications()
    }

    private func disable() async {
        do {
            try await registration().disable()
        } catch {
            // *Off* stays off even when the nest — or the connection itself —
            // cannot be reached: the shared machine cleared the bit before its
            // unsubscribe, and with no connection to build it over, clear it
            // here. The actor record survives, so the next leave-drop still
            // removes the row.
            try? pushClearOptIn(intentPath: intent.path)
            lastError = "Unregister failed: \(error.localizedDescription)"
        }
        deleteKeys()
        isOptedIn = intent.isOptedIn
    }

    // MARK: - Permission

    /// Check current notification authorization status. A no-op on a host that
    /// cannot serve one — see `NotificationHost`.
    public func checkPermission() async {
        guard hostAvailable else { return }
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        switch settings.authorizationStatus {
        case .authorized: permission = .authorized
        case .denied: permission = .denied
        case .provisional: permission = .provisional
        case .notDetermined: permission = .notDetermined
        @unknown default: permission = .notDetermined
        }
    }

    /// Request notification permission. Returns true if granted.
    @discardableResult
    public func requestPermission() async -> Bool {
        guard hostAvailable else { return false }
        do {
            let granted = try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .badge, .sound])
            permission = granted ? .authorized : .denied
            return granted
        } catch {
            lastError = error.localizedDescription
            permission = .denied
            return false
        }
    }

    // MARK: - Registration

    /// Called from AppDelegate when APNs returns a device token.
    public func didRegisterForRemoteNotifications(deviceToken: Data) {
        let tokenHex = deviceToken.hexString

        // Generate or load existing keys
        let (privateKey, authSecret) = loadOrCreateKeys()
        let publicKeyData = privateKey.publicKey.x963Representation
        let publicKeyB64 = publicKeyData.base64URLEncodedString()
        let authSecretB64 = authSecret.base64URLEncodedString()

        // The `apns` row: the hex device token is its endpoint. The device id
        // is not named here — the registration keys the row under its own.
        let subscription = FfiPushEndpoint(
            transport: "apns", endpoint: tokenHex,
            keyP256dh: publicKeyB64, keyAuth: authSecretB64)
        Task { await register(subscription) }
    }

    /// Hand the shared machine the subscription the token callback built. The
    /// user's Enable sets the bit (`enable`); a launch only re-registers under
    /// a bit already set (`rearm` — never opts a device in). A registration
    /// that FAILS — today every one of them, no `aps-environment` entitlement
    /// exists yet (`common.md` § Push Notifications → Implementation status) —
    /// leaves a fresh install un-opted-in.
    func register(_ subscription: FfiPushEndpoint) async {
        let answersEnable = enablePending
        enablePending = false
        do {
            let registration = try await registration()
            if answersEnable {
                try await registration.enable(subscription: subscription)
            } else {
                _ = try await registration.rearm(subscription: subscription)
            }
            lastError = nil
        } catch {
            lastError = "Registration failed: \(error.localizedDescription)"
        }
        isOptedIn = intent.isOptedIn
    }

    /// Called from AppDelegate when APNs registration fails.
    public func didFailToRegisterForRemoteNotifications(error: Error) {
        enablePending = false
        lastError = "APNs registration failed: \(error.localizedDescription)"
    }

    /// Drop THIS actor's `push_subscriptions` row only — the leave-gesture
    /// half of "the subscription follows the signed-in identity" (`common.md`
    /// § Registration, ruled 2026-08-30). The split from the user's Disable
    /// is the shared machine's: its drop never touches the install intent
    /// bit, and this never touches the shared keys — clearing either here
    /// would turn every switch/sign-out into the silent opt-out the re-arm
    /// rule forbids. On an install with no push history it issues nothing.
    ///
    /// Call while the OUTGOING actor's session authority is still in hand —
    /// the switch flow before it commits, sign-out before the credential
    /// erase — never on account removal (a non-active row holds nothing to
    /// drop under this contract, `common.md` § Registration's removal
    /// bullet). Best-effort by ruling: a leave gesture must complete
    /// offline, so a failed unsubscribe strands the row for the ruling's
    /// three reapers (re-adopt, succession burn, endpoint death) rather
    /// than blocking the user.
    public func dropActorRow() async {
        try? await registration().dropActorRow()
    }

    // MARK: - Key Management (Shared Keychain)

    /// Load existing P-256 key pair and auth secret, or generate new ones.
    private func loadOrCreateKeys() -> (P256.KeyAgreement.PrivateKey, Data) {
        if let existingKey = loadPrivateKey(), let existingAuth = loadAuthSecret() {
            return (existingKey, existingAuth)
        }

        // Generate new key pair
        let privateKey = P256.KeyAgreement.PrivateKey()
        let authSecret = SymmetricKey(size: .init(bitCount: 128))
        let authData = authSecret.withUnsafeBytes { Data($0) }

        // Store in shared keychain
        saveToSharedKeychain(
            account: Self.privateKeyAccount,
            data: privateKey.rawRepresentation
        )
        saveToSharedKeychain(
            account: Self.authSecretAccount,
            data: authData
        )

        return (privateKey, authData)
    }

    private func loadPrivateKey() -> P256.KeyAgreement.PrivateKey? {
        guard let data = loadFromSharedKeychain(account: Self.privateKeyAccount) else {
            return nil
        }
        return try? P256.KeyAgreement.PrivateKey(rawRepresentation: data)
    }

    private func loadAuthSecret() -> Data? {
        loadFromSharedKeychain(account: Self.authSecretAccount)
    }

    private func deleteKeys() {
        deleteFromSharedKeychain(account: Self.privateKeyAccount)
        deleteFromSharedKeychain(account: Self.authSecretAccount)
    }

    // MARK: - Shared Keychain Helpers

    private func saveToSharedKeychain(account: String, data: Data) {
        SharedKeychainItem.save(
            service: Self.keychainService, account: account, accessGroup: Self.accessGroup,
            data: data)
    }

    private func loadFromSharedKeychain(account: String) -> Data? {
        SharedKeychainItem.load(
            service: Self.keychainService, account: account, accessGroup: Self.accessGroup)
    }

    private func deleteFromSharedKeychain(account: String) {
        SharedKeychainItem.delete(
            service: Self.keychainService, account: account, accessGroup: Self.accessGroup)
    }

    // MARK: - Platform seams

    /// Ask the OS for a device token. The one line that genuinely differs
    /// between the two Apple targets, kept to a line on purpose: a `#if
    /// os(iOS)` branch is compiled by NOTHING on this machine's gates (the
    /// justfile's `apple-swift-build-check` comment has the four-and-a-half
    /// month case study), so the less that lives inside one, the better.
    public static func registerForRemoteNotifications() {
        guard NotificationHost.isAvailable else { return }
        #if os(macOS)
        NSApplication.shared.registerForRemoteNotifications()
        #else
        UIApplication.shared.registerForRemoteNotifications()
        #endif
    }

    /// The OS's own notification settings, for the denied state's shortcut.
    /// macOS has no `openNotificationSettingsURLString` constant, so it names
    /// the System Settings pane directly.
    public static var systemNotificationSettingsURL: URL? {
        #if os(macOS)
        URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension")
        #else
        URL(string: UIApplication.openNotificationSettingsURLString)
        #endif
    }
}
