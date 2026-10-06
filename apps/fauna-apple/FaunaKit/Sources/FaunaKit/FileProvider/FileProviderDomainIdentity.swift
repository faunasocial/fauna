import FaunaFFISwift
import Foundation

/// A set's identity on this device for one account — the File Provider domain
/// identifier, and the key of the device registries that ride on it (the
/// staging root, the on-demand-toggle preference, the domain-owner record).
/// `on-demand-files.md` § Apple File Provider binding, *the actor-scoped
/// device identity*: a bare `FolderRef` is unique per NEST, not per device —
/// two accounts on two nests routinely both own `local:1` — so a device-wide
/// registry keyed by the bare ref hands one account's presence to the other.
///
/// The grammar (`<ref-component>@<actor-id-hex>`, `local%3A1@…` — the ref half
/// percent-encoded as one component, because iOS refuses a domain identifier
/// carrying `/` or `:`) has one home, shared Rust's `ActorScopedFolderRef`;
/// this type is FaunaKit's only wrapper over its two FFI functions, so no
/// Swift ever spells the separator or the ref's encoding. The seam rule: the
/// apps feed the coordinator BARE refs (`FolderSummary.folderRef` — the same
/// key the binding surfaces use), the coordinator scopes each one exactly
/// once, and the extension recovers both halves from the domain it was built
/// for through `parse`. The Rust host keeps taking the bare ref (`folderId`).
///
/// Framework-free (no `import FileProvider`) so it exists on watchOS too and
/// its pins run under plain `swift test` (`FileProviderDomainIdentityTests`).
public struct FileProviderDomainIdentity: Hashable, Sendable {
    /// The account holding the set on this device (lowercase actor-id hex).
    public let actorIdHex: String
    /// The set's bare `FolderRef` wire string — what the Rust seams take.
    public let folderId: String
    /// The identifier's ref half — the set's `FolderRef` percent-encoded as
    /// one path component (`local%3A1`), which is also the staging root's
    /// directory component: one spelling for both.
    public let folderComponent: String
    /// The registered domain identifier: `<folderComponent>@<actorIdHex>`.
    public let domainId: String

    /// Scope one set to one account. `nil` when either half is malformed (a
    /// non-ref `folderId`, an actor that is not 32 bytes of hex) — the
    /// caller then has no device identity to register, key or remove.
    public init?(actorIdHex: String, folderId: String) {
        guard let scoped = actorScopedFolderRef(actorIdHex: actorIdHex, folderId: folderId)
        else { return nil }
        self.init(scoped)
    }

    /// Recover the halves of a registered identifier. `nil` for an identifier
    /// that scopes no set to any account — a bare ref, a set name, or a
    /// spelling whose ref half
    /// carries the bare `:` — which the extension never serves and the
    /// reconcile removes as undesired.
    public static func parse(_ domainId: String) -> FileProviderDomainIdentity? {
        actorScopedFolderRefParse(wire: domainId).map(FileProviderDomainIdentity.init)
    }

    private init(_ scoped: FfiActorScopedFolderRef) {
        self.actorIdHex = scoped.actorIdHex
        self.folderId = scoped.folderId
        self.folderComponent = scoped.folderComponent
        self.domainId = scoped.scopedId
    }

    /// The set's engine **staging root**, relative to the app-group container:
    /// `FileProvider/roots/<actor-id-hex>/<ref-component>` — the identifier's
    /// `@` boundary IS the directory boundary, account outermost, the same
    /// per-account nesting as the state dir (`<group>/sync/<actor-id-hex>/`),
    /// so two accounts' `local:1` stage into two directories; and the ref
    /// half of the identifier IS the directory's name (`local%3A1`), so no
    /// set escapes the account's `roots/` subtree and nothing here encodes
    /// anything. Pure; `fileProviderRootDir(for:)` joins it onto the
    /// container and creates it.
    public var stagingRootRelativePath: String {
        "FileProvider/roots/\(actorIdHex)/\(folderComponent)"
    }
}
