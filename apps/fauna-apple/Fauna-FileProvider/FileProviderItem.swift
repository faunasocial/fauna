import FaunaKit
import FileProvider
import UniformTypeIdentifiers

/// An `NSFileProviderItem` projected from a tracked engine row (`FfiFileProviderItem`)
/// or a synthesized container. The item identifier is the folder-relative,
/// forward-slash `rel` path — the same string the callbacks pass back to the host —
/// with the root mapped to `.rootContainer`.
///
/// Writable since M3 slice 1: capabilities advertise the create/modify/delete/
/// rename surface the extension's write callbacks serve (`serve_ingest`/`serve_
/// delete`/`serve_rename`, ack only on `UploadOutcome::recorded`) — except on a
/// set the account holds as a reader, whose items advertise reading only
/// (`FileProviderItemCapabilities`; `on-demand-files.md` § Shared sets on a
/// capability host, decision 3).
final class FileProviderItem: NSObject, NSFileProviderItem {
    let identifier: NSFileProviderItemIdentifier
    let parent: NSFileProviderItemIdentifier
    let name: String
    let isFolder: Bool
    let size: Int64
    /// Unix seconds; `nil` for a synthesized container with no row.
    let mtime: Int64?
    /// The FP `contentVersion` bytes (empty for a directory).
    let contentVersionBytes: Data
    /// The set is served read-only (the account holds it as a reader).
    let readOnly: Bool

    init(
        identifier: NSFileProviderItemIdentifier,
        parent: NSFileProviderItemIdentifier,
        name: String,
        isFolder: Bool,
        size: Int64 = 0,
        mtime: Int64? = nil,
        contentVersion: Data = Data(),
        readOnly: Bool
    ) {
        self.identifier = identifier
        self.parent = parent
        self.name = name
        self.isFolder = isFolder
        self.size = size
        self.mtime = mtime
        self.contentVersionBytes = contentVersion
        self.readOnly = readOnly
        super.init()
    }

    /// Project one engine row. `overrideContentVersion`, when non-nil, stamps the
    /// `contentVersion` the host just returned from `fetch` (authoritative for the
    /// materialized bytes the OS is about to store); otherwise the row's own
    /// `contentVersion` is used. Parent is derived from `rel` (the segment before the
    /// last `/`, or the root for a top-level item) so `item(for:)` and enumeration
    /// agree without threading the parent through separately.
    convenience init(
        ffi: FfiFileProviderItem, overrideContentVersion: Data? = nil, readOnly: Bool
    ) {
        self.init(
            identifier: NSFileProviderItemIdentifier(ffi.rel),
            parent: Self.parentIdentifier(forRel: ffi.rel),
            name: ffi.name,
            isFolder: ffi.isDir,
            size: ffi.sizeBytes,
            mtime: ffi.mtime,
            contentVersion: overrideContentVersion ?? ffi.contentVersion,
            readOnly: readOnly
        )
    }

    /// The parent container identifier for a path `rel`. The derivation itself is
    /// the shared, unit-tested `FileProviderPathMapping.parentRel` (FaunaKit —
    /// `FaunaKitTests/FileProviderPathMappingTests`); this only maps the top-level
    /// `nil` onto `.rootContainer`, which needs the FileProvider framework.
    static func parentIdentifier(forRel rel: String) -> NSFileProviderItemIdentifier {
        FileProviderPathMapping.parentRel(forRel: rel)
            .map { NSFileProviderItemIdentifier($0) } ?? .rootContainer
    }

    /// The Fauna root container the domain presents in Finder / the Files app.
    static func rootContainer(named name: String, readOnly: Bool) -> FileProviderItem {
        FileProviderItem(
            identifier: .rootContainer,
            parent: .rootContainer,
            name: name,
            isFolder: true,
            readOnly: readOnly
        )
    }

    var itemIdentifier: NSFileProviderItemIdentifier { identifier }
    var parentItemIdentifier: NSFileProviderItemIdentifier { parent }
    var filename: String { name }

    var capabilities: NSFileProviderItemCapabilities {
        FileProviderItemCapabilities.capabilities(isFolder: isFolder, readOnly: readOnly)
    }

    var contentType: UTType {
        guard !isFolder else { return .folder }
        let ext = (name as NSString).pathExtension
        return UTType(filenameExtension: ext) ?? .data
    }

    var documentSize: NSNumber? { isFolder ? nil : NSNumber(value: size) }

    var contentModificationDate: Date? {
        mtime.map { Date(timeIntervalSince1970: TimeInterval($0)) }
    }

    /// Content identity drives the OS's dehydration gate: the `contentVersion` is the
    /// engine's `recorded_content_hash` (or the manifest head for an un-hydrated
    /// placeholder). Metadata version folds size + mtime so a metadata-only change
    /// still bumps it.
    var itemVersion: NSFileProviderItemVersion {
        NSFileProviderItemVersion(
            contentVersion: contentVersionBytes,
            metadataVersion: Data("\(size)-\(mtime ?? 0)".utf8)
        )
    }
}
