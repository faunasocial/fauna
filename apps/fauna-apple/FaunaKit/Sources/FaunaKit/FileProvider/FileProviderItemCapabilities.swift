import FaunaFFISwift
import Foundation

#if canImport(FileProvider)
    import FileProvider

    /// What a File Provider item advertises, and the error a write on a
    /// read-only set answers with — shared FaunaKit (not appex-local) so the
    /// rule is pinned headlessly and macOS and iOS serve it alike.
    ///
    /// A set the account holds as a reader (`on-demand-files-shared-sets.md` § Shared sets
    /// on a capability host, decision 3) advertises no write, create, delete or
    /// rename capability on any of its items: the host says which it is
    /// (`FfiFileProviderHost.isReadOnly`), and refuses a write that arrives
    /// anyway — the extension refuses it first, before any byte is staged.
    ///
    /// On iOS the same word carries the keep: a file the holder-keeps gate does
    /// not hold (`kept`) advertises `.allowsEvicting`, without which the OS
    /// refuses every eviction, and a kept one does not. macOS keeps by content
    /// policy instead (`FileProviderKeep`), the capability deprecated there.
    public enum FileProviderItemCapabilities {
        public static func capabilities(isFolder: Bool, readOnly: Bool, kept: Bool)
            -> NSFileProviderItemCapabilities
        {
            var capabilities: NSFileProviderItemCapabilities
            if readOnly {
                capabilities = isFolder ? [.allowsContentEnumerating, .allowsReading] : [.allowsReading]
            } else {
                capabilities = isFolder
                    ? [
                        .allowsContentEnumerating, .allowsReading, .allowsAddingSubItems,
                        .allowsRenaming, .allowsDeleting,
                    ]
                    : [.allowsReading, .allowsWriting, .allowsRenaming, .allowsDeleting, .allowsReparenting]
            }
            #if os(iOS)
                if !isFolder && !kept { capabilities.insert(.allowsEvicting) }
            #endif
            return capabilities
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
