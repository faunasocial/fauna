import FaunaFFISwift
import Foundation

#if canImport(FileProvider)
    import FileProvider

    /// What a File Provider item advertises, and the error a write on a
    /// read-only set answers with — shared FaunaKit (not appex-local) so the
    /// rule is pinned headlessly and macOS and iOS serve it alike.
    ///
    /// A set the account holds as a reader (`on-demand-files.md` § Shared sets
    /// on a capability host, decision 3) advertises no write, create, delete or
    /// rename capability on any of its items: the host says which it is
    /// (`FfiFileProviderHost.isReadOnly`), and refuses a write that arrives
    /// anyway — the extension refuses it first, before any byte is staged.
    public enum FileProviderItemCapabilities {
        public static func capabilities(isFolder: Bool, readOnly: Bool)
            -> NSFileProviderItemCapabilities
        {
            if readOnly {
                return isFolder ? [.allowsContentEnumerating, .allowsReading] : [.allowsReading]
            }
            return isFolder
                ? [
                    .allowsContentEnumerating, .allowsReading, .allowsAddingSubItems,
                    .allowsRenaming, .allowsDeleting,
                ]
                : [.allowsReading, .allowsWriting, .allowsRenaming, .allowsDeleting, .allowsReparenting]
        }

        /// The platform's refusal of a write on a read-only set: the user lacks
        /// write permission, so the OS reports the edit as not allowed rather
        /// than retrying it as a transient failure.
        public static func readOnlyRefusal() -> Error {
            CocoaError(.fileWriteNoPermission)
        }

        /// Whether `host` serves its set read-only. A host that cannot answer
        /// (its binding refused: no engine) is read-only — fail closed; it
        /// serves nothing to write into anyway.
        public static func isReadOnly(_ host: FfiFileProviderHost) async -> Bool {
            (try? await host.isReadOnly()) ?? true
        }
    }
#endif
