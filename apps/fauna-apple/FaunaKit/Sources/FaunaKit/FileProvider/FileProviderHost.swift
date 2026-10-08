import FaunaFFISwift
import Foundation

/// Why a File Provider host could not be built.
public enum FileProviderHostError: Error, Sendable {
    /// The app has not written the capability into the shared Keychain yet (the user
    /// is signed out, or no domain has been created). The extension fails closed.
    case notProvisioned
    /// The app-group container is unreachable (missing `application-groups`
    /// entitlement — a packaging bug, not a runtime state).
    case noAppGroupContainer
    /// The domain identifier scopes no set to any account
    /// (`FileProviderDomainIdentity.parse` refused it): a domain registered
    /// under a bare ref, or by its set name, or with
    /// the bare `:` in the ref half. Never
    /// served — the next reconcile removes it as undesired (`on-demand-files.md`
    /// § Apple File Provider binding, *the actor-scoped device identity*).
    case unscopedDomainIdentifier
    /// The domain belongs to a DIFFERENT account than the provisioned
    /// capability — the account in the identifier itself, or the recorded
    /// owner (`FileProviderDomainOwners`, defense in depth): a dirty domain
    /// lingering across an account switch (the iOS drain-hold) must never be
    /// served — much less have its queued edits ingested — under the incoming
    /// account's credentials. The extension fails closed; the drain resumes
    /// when the owning account signs back in.
    case foreignDomainOwner
}

/// Build the app-dead File Provider host for one folder. Reads the
/// least-privilege capability the app wrote into the shared app-group Keychain
/// (`FileProviderCredentialStore`), lays out the extension's private working +
/// state directories in the app-group container, and hands both — plus a fresh-read
/// bearer provider and change-signer provider — to the Rust
/// `FfiFileProviderHost.appDead` constructor.
///
/// This is shared FaunaKit (not appex-local) so macOS and iOS build the host the
/// same way (priority #2). It takes the registered **domain identifier** — the
/// set's actor-scoped identity, `FileProviderDomainIdentity` — not an
/// `NSFileProviderDomain`, so it carries no `import FileProvider` — FaunaKit also
/// builds for watchOS, where that framework is absent; the appex passes
/// `domain.identifier.rawValue`, which is what `FileProviderDomains.add`
/// registered. The Rust constructor takes the bare ref recovered from it and
/// refuses anything else.
///
/// Paths (both private to the extension; the OS owns the visible
/// `~/Library/CloudStorage/` replica in the replicated model, so the engine never
/// writes there):
/// - **state** — the account's scoped container dir holds the per-set
///   `fsid-<ref>.db` + shared `device.db` (`SyncDb` names them per set, so one
///   shared state dir is a single writer per set).
/// - **root** — `<group>/FileProvider/roots/<actor-id-hex>/<ref-component>` is
///   the engine's `watch_dir` staging area (no watcher runs; the OS drives the
///   callbacks); the component is the identifier's own ref half.
public func makeFileProviderHost(domainId: String) throws -> FfiFileProviderHost {
    guard let creds = FileProviderCredentialStore.load() else {
        throw FileProviderHostError.notProvisioned
    }
    let identity: FileProviderDomainIdentity
    switch hostAdmission(
        domainId: domainId,
        provisionedActorHex: data_to_hex(creds.actorId),
        recordedOwner: FileProviderDomainOwners.owner(domainId: domainId))
    {
    case .serve(let admitted): identity = admitted
    case .unscoped: throw FileProviderHostError.unscopedDomainIdentifier
    case .foreign: throw FileProviderHostError.foreignDomainOwner
    }
    let stateDir = try fileProviderStateDir(actorIdHex: identity.actorIdHex)
    let rootDir = try fileProviderRootDir(for: identity)

    return try FfiFileProviderHost.appDead(
        nestUrl: creds.nestURL,
        actorId: creds.actorId,
        deviceId: creds.deviceId,
        deviceLabel: creds.deviceLabel,
        backupKey: creds.backupKey,
        bearer: KeychainBearerProvider(),
        signer: KeychainChangeSignerProvider(),
        stateDir: stateDir.path,
        folderId: identity.folderId,
        rootDir: rootDir.path,
        predecessorChain: creds.predecessorChain
    )
}

/// The File Provider host's cross-account admission decision, pure so each arm
/// is pinned headlessly (`FileProviderDomainIdentityTests`).
public enum HostAdmission: Equatable, Sendable {
    /// Own account's scoped domain: serve it under the parsed identity.
    case serve(FileProviderDomainIdentity)
    /// The identifier scopes no set to any account (fail closed).
    case unscoped
    /// The domain belongs to a different account than the provisioned one.
    case foreign
}

/// Evaluate the guard in `on-demand-files.md` § Apple File Provider binding
/// (*the actor-scoped device identity*)'s order: parse the identifier, then the
/// structural check (the identifier's account against the provisioned one —
/// BEFORE the record is consulted, so a foreign-scoped domain is refused
/// whatever the record says), then the owner record as defense in depth. An
/// absent record (a crash between the add and the record write, or a lost
/// record file) is served, backfilled by the next reconcile.
func hostAdmission(
    domainId: String, provisionedActorHex: String, recordedOwner: String?
) -> HostAdmission {
    guard let identity = FileProviderDomainIdentity.parse(domainId) else {
        return .unscoped
    }
    if identity.actorIdHex != provisionedActorHex {
        return .foreign
    }
    if let recordedOwner, recordedOwner != provisionedActorHex {
        return .foreign
    }
    return .serve(identity)
}

/// The shared per-set engine **staging root** (`upload_file` reads from it, so the
/// M3 write path materializes OS-provided bytes here — at the file's `rel` —
/// before calling `ingest`/`rename`). Created on first use; same derivation
/// `makeFileProviderHost` hands the Rust host as `root_dir`. The layout
/// (`FileProvider/roots/<actor-id-hex>/<ref-component>`) is the identity's pure
/// `stagingRootRelativePath`, pinned headlessly.
public func fileProviderRootDir(for identity: FileProviderDomainIdentity) throws -> URL {
    let rootDir = try appGroupContainer()
        .appendingPathComponent(identity.stagingRootRelativePath, isDirectory: true)
    try FileManager.default.createDirectory(at: rootDir, withIntermediateDirectories: true)
    return rootDir
}

/// The extension's engine state dir (`fsid-<ref>.db` + `device.db`; one dir for all
/// sets — `SyncDb` names per set): the account's **scoped container dir**
/// (`SyncStateDir` — the container domain, the extension's only root; per-
/// actor scoping so the app reads the same path as the container's steward).
/// Nothing outside the container is reachable from this sandbox — and, on
/// macOS, the user domain is another consent domain entirely.
private func fileProviderStateDir(actorIdHex: String) throws -> URL {
    guard let stateDir = SyncStateDir.resolve(actorIdHex: actorIdHex, in: .container) else {
        throw FileProviderHostError.noAppGroupContainer
    }
    return stateDir
}

private func appGroupContainer() throws -> URL {
    guard let container = FileProviderCredentialStore.containerURL() else {
        throw FileProviderHostError.noAppGroupContainer
    }
    return container
}
