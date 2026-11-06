import Foundation

/// The `fauna://` deep-link vocabulary for File Provider context actions
/// (`on-demand-files-apple.md` § On-Demand Files → Apple File Provider binding — *context
/// actions*): the FP UI action extension builds these URLs and the app routes
/// them. The **Version history** leaf keeps this module's own grammar; the
/// **Share** leaf spells the shared in-app routes instead (``FaunaShareRoute``).
/// Deliberately a tiny FFI-free module: the UI appex links ONLY this (a
/// FaunaKit dependency would ship the whole Rust xcframework inside a
/// menu-action binary), while FaunaKit re-exports it for the app-side router —
/// one vocabulary, two lightweight consumers (priority #2).
///
/// The `fauna://identity?…` and `fauna://peer?…` URI forms are parsed by their
/// own onboarding/pairing surfaces, not here; `parse` returns `nil` for them so
/// a caller can fall through.
public enum FaunaDeepLink: Equatable, Sendable {
    /// One file's version history (the `file-version-history` surface) — the
    /// windows shell submenu's **Version history** leaf. `rel` is the
    /// folder-relative forward-slash path (= the FP item identifier).
    case fileVersions(set: String, rel: String)

    private static let scheme = "fauna"
    private static let host = "folder"

    /// Parse an incoming open-URL. `nil` = not a folder action link (possibly
    /// an identity/peer URI — the caller's other parsers own those).
    public static func parse(_ url: URL) -> FaunaDeepLink? {
        guard url.scheme == scheme, url.host == host else { return nil }
        let segments = url.pathComponents.filter { $0 != "/" }
        guard let set = segments.first, !set.isEmpty else { return nil }
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems
        switch query?.first(where: { $0.name == "action" })?.value {
        case "versions":
            guard let rel = query?.first(where: { $0.name == "path" })?.value, !rel.isEmpty
            else { return nil }
            return .fileVersions(set: set, rel: rel)
        default:
            return nil
        }
    }

    /// The URL for this link (inverse of `parse`; components-built so set names
    /// and rels with spaces/unicode round-trip).
    public var url: URL? {
        var components = URLComponents()
        components.scheme = Self.scheme
        components.host = Self.host
        switch self {
        case .fileVersions(let set, let rel):
            components.path = "/\(set)"
            components.queryItems = [
                URLQueryItem(name: "action", value: "versions"),
                URLQueryItem(name: "path", value: rel),
            ]
        }
        return components.url
    }
}

/// The File Provider context-action leaf set — the windows shell-submenu
/// vocabulary (`windows.md` § Shell Extension: Share / Version history;
/// the read-only device-info row is windows-only, since FP action names are
/// static Info.plist strings and cannot render a dynamic informational line).
/// The raw values are the `NSExtensionFileProviderActionIdentifier`s declared
/// in the FP appex's Info.plist; `prepare(forAction:…)` maps back through
/// `init(rawValue:)`, so the two surfaces cannot drift on the identifier
/// strings themselves (the plist's copies are pinned by FaunaKitTests).
public enum FileProviderAction: String, CaseIterable, Sendable {
    case share = "social.fauna.fp.action.share"
    case versions = "social.fauna.fp.action.versions"

    /// The windows `context_menu::leaf_hidden_for_folder` twin: **Share stays**
    /// on a folder — the set's own root, whose member sharing it opens (a
    /// sub-folder carries no `faunaShare` entry, so the activation rule hides
    /// it there) — and the per-file version leaf hides (a per-file read would
    /// render permanently unavailable). Mirrored by each action's Info.plist
    /// activation rule; this is the testable statement of record.
    public var hiddenForFolder: Bool {
        switch self {
        case .share: return false
        case .versions: return true
        }
    }

    /// The URL this action opens the app with. `domainIdentifier` is the FP
    /// domain identifier (`local%3A<id>@<actor-id-hex>`); `rels` the selected
    /// item identifiers (rel paths; the root container arrives as ""). Share
    /// spells the shared route for its one selected item
    /// (``FaunaShareRoute/forItem(domainIdentifier:rel:)``); versions is
    /// single-file (first rel).
    public func url(domainIdentifier: String, rels: [String]) -> URL? {
        switch self {
        case .share:
            guard rels.count == 1 else { return nil }
            return FaunaShareRoute.forItem(domainIdentifier: domainIdentifier, rel: rels[0])?.url
        case .versions:
            guard let rel = rels.first(where: { !$0.isEmpty }) else { return nil }
            return FaunaDeepLink.fileVersions(set: domainIdentifier, rel: rel).url
        }
    }
}

/// The two share routes of the shared in-app route grammar
/// (`fauna_core::app_route`): `fauna://share-link?folder=<id>&path=<rel>` and
/// `fauna://folder-share?folder=<id>`. The UI appex is FFI-free by design (a
/// menu action must not ship the Rust xcframework), so this is a declared
/// second speller of that grammar — build only, never parse: the app parses
/// every route through the shared `parseAppRoute`. Pinned round-trip against
/// that parser in FaunaKitTests (`macos.md` § Sync → *Finder's Share entry*,
/// link 2).
public enum FaunaShareRoute: Equatable, Sendable {
    /// A file of the set: its detail with the share-link create surface open.
    case shareLink(folderId: Int64, path: String)
    /// The set's own root: its member sharing.
    case folderShare(folderId: Int64)

    /// The route for one selected item of the domain: the root container
    /// (`rel == ""`) → ``folderShare(folderId:)``, any other item →
    /// ``shareLink(folderId:path:)`` (the item identifier IS the
    /// folder-relative path). `nil` when the domain names no set of this nest
    /// (``localFolderId(domainIdentifier:)``) — a cross-nest set is never a
    /// target. Which items get here at all is the activation rule's.
    public static func forItem(domainIdentifier: String, rel: String) -> FaunaShareRoute? {
        guard let folderId = localFolderId(domainIdentifier: domainIdentifier) else { return nil }
        return rel.isEmpty
            ? .folderShare(folderId: folderId) : .shareLink(folderId: folderId, path: rel)
    }

    /// The set id a domain identifier names, when that set is this nest's own:
    /// the identifier's ref half (before the `@`) percent-decodes to
    /// `local:<id>`. `nil` for a foreign ref or a malformed identifier.
    public static func localFolderId(domainIdentifier: String) -> Int64? {
        guard let at = domainIdentifier.lastIndex(of: "@"),
              let ref = domainIdentifier[..<at].removingPercentEncoding,
              ref.hasPrefix("local:")
        else { return nil }
        return Int64(ref.dropFirst("local:".count))
    }

    /// The route's URI, encoded exactly as `AppRoute::to_uri` encodes it: the
    /// path's RFC 3986 unreserved characters and `/` pass through, every other
    /// UTF-8 byte is `%XX`.
    public var url: URL? {
        switch self {
        case .shareLink(let folderId, let path):
            guard let encoded = path.addingPercentEncoding(withAllowedCharacters: Self.pathAllowed)
            else { return nil }
            return URL(string: "fauna://share-link?folder=\(folderId)&path=\(encoded)")
        case .folderShare(let folderId):
            return URL(string: "fauna://folder-share?folder=\(folderId)")
        }
    }

    /// ASCII letters and digits, `-._~` and `/` — `app_route`'s `percent_encode`.
    private static let pathAllowed: CharacterSet = {
        var set = CharacterSet(charactersIn: "-._~/")
        set.insert(charactersIn: Unicode.Scalar("A")...Unicode.Scalar("Z"))
        set.insert(charactersIn: Unicode.Scalar("a")...Unicode.Scalar("z"))
        set.insert(charactersIn: Unicode.Scalar("0")...Unicode.Scalar("9"))
        return set
    }()
}
