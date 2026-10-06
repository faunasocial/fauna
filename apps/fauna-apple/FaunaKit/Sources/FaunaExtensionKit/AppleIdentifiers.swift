import Foundation

/// Platform-registered identifiers — the strings macOS/iOS, not Fauna, key on.
///
/// An identifier belongs here when **the OS registers it** and it therefore
/// **persists on the user's machine** independently of our code: the app-group
/// container name, the keychain services living inside it, the launchd label the
/// app kickstarts, the `BGTaskScheduler` ids the system wakes us on. Those are
/// exactly the strings a rename cannot fix retroactively.
///
/// The Rust twin is `fauna_core::platform_ids`; the plists that must repeat these
/// literals (four entitlements files, the appex's
/// `NSExtensionFileProviderDocumentGroup`, the iOS
/// `BGTaskSchedulerPermittedIdentifiers` array) cannot reference a constant, so
/// `tests/e2e-unified/tests/test_apple_identifier_pins.py` is what keeps every
/// copy equal. Add any new site there.
///
/// Owner docs: `docs/goal/architecture/apps/macos.md` § Entitlements (the
/// app-group tier) and `docs/goal/architecture/installers/macos.md` § launchd
/// jobs (the label tier).
public enum AppleIdentifiers {
    /// The shared app-group container id — the ONE state-unification home the
    /// app, the `Fauna-FileProvider` extension, and the per-user
    /// `fauna-sync-agent` converge on, and the keychain access group their
    /// shared credential items live in.
    ///
    /// **Per-OS by ratified design** (`installers/macos.md` § Identifier
    /// domain, decided by the first-signed-build TCC matrix 2026-08-23): since
    /// macOS 15 the group container is TCC-protected and a Developer-ID-signed
    /// process reaches it prompt-free only under a **Team-ID-prefixed** group
    /// id — the one condition that also covers the `.pkg`'s bare, bundle-less
    /// `fauna-sync-agent`. iOS never takes the prefix. The Rust twin forks the
    /// same way (`platform_ids::APPLE_MACOS_APP_GROUP` / `APPLE_APP_GROUP`).
    #if os(macOS)
    public static let appGroup = "7457N3M72H.group.social.fauna.shared"
    #else
    public static let appGroup = "group.social.fauna.shared"
    #endif

    /// The watchOS widget/complication app group. Separate provisioning
    /// container, same domain.
    public static let watchAppGroup = "group.social.fauna.watchkit"

    #if os(macOS)
        /// The **macOS account-credential keychain access group** —
        /// `KeychainStore`'s data-protection-keychain rows live in it, and ONLY
        /// the app is entitled to it (`Fauna-macOS.entitlements`; never an
        /// appex). A second, app-only group rather than ``appGroup`` because
        /// the shared group is also the sandboxed File Provider extension's,
        /// and the extension must never be able to read the identity seed —
        /// least privilege by entitlement, the boundary iOS gets for free from
        /// its per-app default access group. Keychain-only; no container dir
        /// is ever resolved for it. Rust twin:
        /// `platform_ids::APPLE_MACOS_ACCOUNT_KEYCHAIN_GROUP`; pinned by
        /// `test_apple_identifier_pins.py`.
        public static let accountKeychainGroup = "7457N3M72H.group.social.fauna.account"
    #endif

    /// The home-screen widget's WidgetKit `kind`. The OS keys every widget a
    /// user has placed on this string, so a rename silently empties their home
    /// screen — hence it lives here, not beside the widget. The app's
    /// `WidgetCenter.reloadTimelines(ofKind:)` and the appex's
    /// `StaticConfiguration(kind:)` both read it.
    public static let unreadWidgetKind = "social.fauna.widget.unread"

    /// The per-user sync-agent LaunchAgent label the app kickstarts and the
    /// `.pkg` installs. Must equal the `Label` the sync postinstall writes.
    public static let syncAgentLaunchAgent = "social.fauna.sync-agent"

    /// The macOS app's own auto-start LaunchAgent label (`apps/macos.md` § App
    /// Lifecycle → *Auto-start at sign-in*). Must equal the `Label` of the
    /// bundle-shipped `Contents/Library/LaunchAgents/<label>.plist` that
    /// `SMAppService.agent(plistName:)` registers — a different job from
    /// ``syncAgentLaunchAgent``.
    public static let appLaunchAgent = "social.fauna.FaunaMacOS"

    /// Keychain `kSecAttrService` values, all inside ``appGroup``.
    public enum KeychainService {
        /// Web-push key material (`PushManager`).
        public static let push = "social.fauna.push"
        /// The File Provider capability (`FileProviderCredentialStore`).
        public static let fileProvider = "social.fauna.fileprovider"
        /// The account credential store (`KeychainStore`) — the actor secret key,
        /// node URL, device id, and the other account-scoped rows every app surface
        /// reads through `KeychainStore`/`KeychainSecretStore`. A purpose leaf, not
        /// the product name (`installers/macos.md` § Identifier domain — the
        /// keychain-service tier follows android's/push's/fileProvider's
        /// what-is-stored-here convention, never a bundle-id echo); ONE value for
        /// macOS, iOS, **and** watchOS — different devices, so the shared name never
        /// collides, exactly like ``push``/``fileProvider`` already are for
        /// macOS+iOS.
        ///
        /// The spellings it replaced (`social.fauna.desktop`/`.ios`/`.watch`, then
        /// `social.fauna.fauna`) are never read: the compat-remnant sweep removed their
        /// read-forward (`version-compatibility.md` § Dimension 2, the fourth ratified
        /// exception — no row under a retired spelling exists).
        public static let account = "social.fauna.account"
    }

    /// `BGTaskScheduler` identifiers. Every one of these must also appear in the
    /// iOS app's `BGTaskSchedulerPermittedIdentifiers` array, or the system
    /// refuses to register the handler at launch.
    public enum BackgroundTask {
        public static let upload = "social.fauna.sync.upload"
        /// The client-device backup custodian's periodic pull pass
        /// (`docs/goal/behavior/backup-destinations.md` § Third destination
        /// kind; the iOS twin of android's `CustodianHostWorker`).
        public static let custodianPull = "social.fauna.sync.custodian"
        /// The home-screen widget's background unread-count refresh — a
        /// `BGAppRefreshTask`, the iOS twin of android's periodic
        /// `WidgetDataWorker` (`apps/common.md` § Home-screen widget).
        public static let widgetRefresh = "social.fauna.widget.refresh"
        /// The background `URLSession` id used for out-of-process uploads.
        public static let backgroundUploadSession = "social.fauna.sync.bg-upload"
    }
}
