/// Pure rel-path → File Provider identity mapping, shared by the macOS appex and
/// (at M4) the iOS appex. An item's identifier is its folder-relative,
/// forward-slash `rel` path; the parent is derived from it here — single source of
/// truth so `item(for:)`, enumeration, and the `fetchContents` metadata-less
/// fallback can never disagree (a hardcoded root fallback mis-parents a nested
/// `rel` like `sub/b.txt` — the bug fixed 2026-07-19).
///
/// Deliberately framework-free (no `import FileProvider`) so it exists on watchOS
/// too and stays unit-testable via plain `swift test` (`FaunaKitTests`); the appex
/// maps the `nil` root to `NSFileProviderItemIdentifier.rootContainer`.
public enum FileProviderPathMapping {
    /// The parent container's `rel` for `rel` — the segment before the last `/` —
    /// or `nil` for a top-level item (the caller maps `nil` to the root container).
    public static func parentRel(forRel rel: String) -> String? {
        guard let slash = rel.range(of: "/", options: .backwards) else { return nil }
        return String(rel[..<slash.lowerBound])
    }
}
