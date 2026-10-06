import Foundation

/// The `fauna://` deep-link vocabulary for File Provider context actions
/// (`file-sync.md` § On-Demand Files → Apple File Provider binding — *context
/// actions*): the FP UI action extension builds these URLs and the app routes
/// them. Deliberately a tiny FFI-free module: the UI appex links ONLY this (a
/// FaunaKit dependency would ship the whole Rust xcframework inside a
/// menu-action binary), while FaunaKit re-exports it for the app-side router —
/// one vocabulary, two lightweight consumers (priority #2).
///
/// The `fauna://identity?…` and `fauna://peer?…` URI forms are parsed by their
/// own onboarding/pairing surfaces, not here; `parse` returns `nil` for them so
/// a caller can fall through.
public enum FaunaDeepLink: Equatable, Sendable {
    /// Settings → Folders with `set` in view — the share affordance's home
    /// (`folder-share-button`), the windows shell submenu's **Share** leaf.
    case folderShare(set: String)
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
        case "share", nil:
            return .folderShare(set: set)
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
        case .folderShare(let set):
            components.path = "/\(set)"
            components.queryItems = [URLQueryItem(name: "action", value: "share")]
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
    /// on folders (a set is the natural share target), the per-file version
    /// leaf hides (a per-file read would render permanently unavailable).
    /// Mirrored by each action's Info.plist activation rule; this is the
    /// testable statement of record.
    public var hiddenForFolder: Bool {
        switch self {
        case .share: return false
        case .versions: return true
        }
    }

    /// The deep link this action opens the app with. `set` is the FP domain
    /// identifier (= folder name); `rels` the selected item identifiers
    /// (rel paths; the root container arrives as ""). Share is set-level and
    /// ignores the selection; versions is single-file (first rel).
    public func deepLink(set: String, rels: [String]) -> FaunaDeepLink? {
        switch self {
        case .share:
            return .folderShare(set: set)
        case .versions:
            guard let rel = rels.first(where: { !$0.isEmpty }) else { return nil }
            return .fileVersions(set: set, rel: rel)
        }
    }
}
