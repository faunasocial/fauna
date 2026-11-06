import FaunaDeepLink
import Foundation

/// The per-item `userInfo` entry Finder's (and the Files app's) **Share** action
/// gates on (`docs/goal/architecture/apps/macos.md` § Sync → *Finder's Share
/// entry*, link 1): the action's activation rule in `Fauna-FileProviderUI/Info.plist`
/// is "one selected item carrying `faunaShare`", so the OS itself hides Share
/// wherever it has no meaning.
///
/// - `"link"` on a file that can carry a share link — the shared provider face's
///   `share_link_eligible` (a file of a public-audience set of this nest);
/// - `"folder"` on the domain's root container, when the domain names a set of
///   this nest (its member sharing);
/// - no entry on a sub-folder, on any other file, or on any item of a set this
///   nest does not own.
///
/// Framework-free so its pin runs under plain `swift test`.
public enum FileProviderShareEntry {
    /// The `userInfo` key the activation rule reads.
    public static let key = "faunaShare"

    /// A file's entry, from the provider face's fact.
    public static func forFile(shareLinkEligible: Bool) -> String? {
        shareLinkEligible ? "link" : nil
    }

    /// The root container's entry: the set's member sharing, for a set of this
    /// nest (``FaunaShareRoute/localFolderId(domainIdentifier:)``).
    public static func forRoot(domainIdentifier: String) -> String? {
        FaunaShareRoute.localFolderId(domainIdentifier: domainIdentifier) != nil ? "folder" : nil
    }

    /// The `userInfo` dictionary for an entry (`nil` = no entry).
    public static func userInfo(_ entry: String?) -> [AnyHashable: Any]? {
        entry.map { [key: $0] }
    }
}
