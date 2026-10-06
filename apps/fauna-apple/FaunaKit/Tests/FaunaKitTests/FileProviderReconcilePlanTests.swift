import Foundation
import Testing

#if canImport(FileProvider)
    import FileProvider
#endif

@testable import FaunaKit

#if canImport(FileProvider)

    /// Headless pins for the reconcile's wrapper over the shared-Rust presence
    /// plan (`FileProviderCoordinator.plan` → `onDemandPresencePlan` —
    /// `on-demand-files.md` § Apple File Provider binding, *the actor-scoped
    /// device identity* + § Multi-account × File Provider, consequence 2). The
    /// plan's rules are pinned in Rust (`fauna_folders_machine::on_demand_presence`,
    /// where these cases were ported one-for-one); what stays here is the Swift
    /// glue: the domain-owner record read from the real store into the plan,
    /// the backfill written back through it, a set shared with the account
    /// through the toggle store and the bound subtraction (the list itself is
    /// mapped in shared Rust — `presence_sets` — never in Swift), the nudge's
    /// push-to-domain match, and a reader's item capabilities. The
    /// owner record and the toggle store run at injected temp paths, never the
    /// app-group container or the user domain (testing.md § conventions point
    /// 10); the `NSFileProviderManager` add/remove calls are the only live inch.
    @Suite struct FileProviderReconcilePlanTests {
        private let a = String(repeating: "aa", count: 32)
        private let b = String(repeating: "bb", count: 32)
        private let docs = PresenceSet(name: "docs", folderId: "local:1", thisDeviceAccepts: true, role: .own)
        private let pics = PresenceSet(name: "pics", folderId: "local:2", thisDeviceAccepts: true, role: .own)
        /// A folder shared WITH the account, as the shared lister hands it:
        /// named with its owner, a seat by the membership, read-only.
        private let sharedDocs = PresenceSet(
            name: "docs (alice)", setName: "docs", folderId: "local:5", thisDeviceAccepts: true,
            role: .sharedWithMe, readOnly: true)

        private func scopedId(_ set: PresenceSet, _ actorHex: String) -> String {
            FileProviderDomainIdentity(actorIdHex: actorHex, folderId: set.folderId)!.domainId
        }

        private func tempDir() -> URL {
            FileManager.default.temporaryDirectory.appendingPathComponent("fp-plan-\(UUID().uuidString)")
        }

        private func plan(
            _ sets: [PresenceSet], registered: [String], actorHex: String, dir: URL,
            bound: Set<String> = []
        ) throws -> PresencePlan {
            try FileProviderCoordinator.plan(
                actorHex: actorHex, sets: sets, boundSets: bound,
                prefs: FfiOnDemandPrefsStore(path: dir.appendingPathComponent("prefs.json").path),
                registered: registered,
                ownersAt: dir.appendingPathComponent(FileProviderDomainOwners.filename))
        }

        /// Account A left its `local:1` registered; account B signs in wanting
        /// its own `local:1`. Two identifiers: B's is added, A's removed through
        /// the gated path scoped to A — and A's owner record, read from the
        /// real store, is untouched by B's backfill.
        @Test func twoAccountsSameRefNeverCollide() throws {
            let dir = tempDir()
            defer { try? FileManager.default.removeItem(at: dir) }
            let url = dir.appendingPathComponent(FileProviderDomainOwners.filename)
            FileProviderDomainOwners.record(domainId: scopedId(docs, a), actorIdHex: a, at: url)

            let p = try plan([docs], registered: [scopedId(docs, a)], actorHex: b, dir: dir)
            #expect(p.add.map(\.scopedId) == [scopedId(docs, b)])
            #expect(p.foreignHeld.isEmpty)
            #expect(
                p.remove == [
                    PresenceRemoval(
                        identifier: scopedId(docs, a),
                        scope: PresenceScope(actorIdHex: a, folderId: "local:1"))
                ])

            FileProviderCoordinator.backfillOwners(p, actorHex: b, at: url)
            #expect(FileProviderDomainOwners.owner(domainId: scopedId(docs, a), at: url) == a)
        }

        /// The owner record reaches the plan: a desired identifier recorded
        /// to another account is held, never re-recorded by the backfill.
        @Test func aForeignRecordedDomainIsNeitherAdoptedNorReRecorded() throws {
            let dir = tempDir()
            defer { try? FileManager.default.removeItem(at: dir) }
            let url = dir.appendingPathComponent(FileProviderDomainOwners.filename)
            FileProviderDomainOwners.record(domainId: scopedId(docs, b), actorIdHex: a, at: url)

            let p = try plan([docs], registered: [scopedId(docs, b)], actorHex: b, dir: dir)
            #expect(p.add.isEmpty && p.backfill.isEmpty && p.remove.isEmpty)
            #expect(p.foreignHeld.map(\.ownerHex) == [a])

            FileProviderCoordinator.backfillOwners(p, actorHex: b, at: url)
            #expect(FileProviderDomainOwners.owner(domainId: scopedId(docs, b), at: url) == a)
        }

        /// A registered domain with no owner on record is backfilled with the
        /// provisioned account — through the real store.
        @Test func anUnownedRegisteredDomainIsBackfilled() throws {
            let dir = tempDir()
            defer { try? FileManager.default.removeItem(at: dir) }
            let url = dir.appendingPathComponent(FileProviderDomainOwners.filename)

            let p = try plan([docs], registered: [scopedId(docs, b)], actorHex: b, dir: dir)
            #expect(p.backfill == [scopedId(docs, b)])

            FileProviderCoordinator.backfillOwners(p, actorHex: b, at: url)
            #expect(FileProviderDomainOwners.owner(domainId: scopedId(docs, b), at: url) == b)
        }

        /// An identifier from before actor scoping — or from before the ref
        /// half was percent-encoded (`local:1@…`, which iOS refuses and macOS
        /// may still hold) — is removed un-gated (no scope), and the
        /// account's scoped identity is added beside it.
        @Test func aPreScopingIdentifierIsRemovedUngated() throws {
            let dir = tempDir()
            defer { try? FileManager.default.removeItem(at: dir) }
            let p = try plan(
                [docs], registered: ["local:1", "docs", "local:1@\(b)"], actorHex: b, dir: dir)
            #expect(p.add.map(\.scopedId) == [scopedId(docs, b)])
            #expect(
                p.remove == [
                    PresenceRemoval(identifier: "local:1", scope: nil),
                    PresenceRemoval(identifier: "docs", scope: nil),
                    PresenceRemoval(identifier: "local:1@\(b)", scope: nil),
                ])
        }

        /// A set shared with the account joins the plan by default (the
        /// toggle store reads ON for an identifier it never saw), keyed by its
        /// actor-scoped ref and shown under its owner-qualified name.
        @Test func aSharedSetIsADefaultOnPresence() throws {
            let dir = tempDir()
            defer { try? FileManager.default.removeItem(at: dir) }
            let p = try plan([sharedDocs], registered: [], actorHex: b, dir: dir)
            #expect(p.add.map(\.scopedId) == [scopedId(sharedDocs, b)])
            #expect(p.add.map(\.set.name) == ["docs (alice)"])
            #expect(p.add.allSatisfy { $0.set.readOnly })
        }

        /// The member's toggle turned off hides the shared set — and a domain
        /// it already had is removed.
        @Test func aSharedSetToggledOffHasNoPresence() throws {
            let dir = tempDir()
            defer { try? FileManager.default.removeItem(at: dir) }
            let prefs = FfiOnDemandPrefsStore(path: dir.appendingPathComponent("prefs.json").path)
            FileProviderDomainPrefs.setEnabled(
                false, domainId: scopedId(sharedDocs, b), store: prefs)
            let p = try plan(
                [sharedDocs], registered: [scopedId(sharedDocs, b)], actorHex: b, dir: dir)
            #expect(p.add.isEmpty && p.keep.isEmpty)
            #expect(p.remove.map(\.identifier) == [scopedId(sharedDocs, b)])
        }

        /// A writer member's local-folder binding outranks the shared set's
        /// domain, exactly as an owner's does (one local presence per set).
        @Test func aBoundSharedSetIsSubtracted() throws {
            let dir = tempDir()
            defer { try? FileManager.default.removeItem(at: dir) }
            let p = try plan(
                [sharedDocs, docs], registered: [], actorHex: b, dir: dir, bound: ["local:5"])
            #expect(p.add.map(\.scopedId) == [scopedId(docs, b)])
        }

        /// The nudge finds a shared set's domain by the set's own name, never
        /// by the owner-qualified display name, and a push naming a set the
        /// account holds as its own AND as a member's share signals both —
        /// each by its own ref.
        @Test func aPushNamesDomainsByTheSetsRef() {
            let ownDocs = docs
            #expect(
                FileProviderDomains.domainIds(
                    namedBy: "docs", folderHash: nil, in: [sharedDocs], actorHex: b)
                    == [scopedId(sharedDocs, b)])
            #expect(
                FileProviderDomains.domainIds(
                    namedBy: "docs (alice)", folderHash: nil, in: [sharedDocs], actorHex: b)
                    .isEmpty)
            #expect(
                FileProviderDomains.domainIds(
                    namedBy: "docs", folderHash: nil,
                    in: [sharedDocs, PresenceSet(
                        name: "docs", setName: "docs", folderId: ownDocs.folderId,
                        thisDeviceAccepts: true, role: .own)],
                    actorHex: b)
                    == [scopedId(sharedDocs, b), scopedId(ownDocs, b)])
            #expect(
                FileProviderDomains.domainIds(
                    namedBy: "pics", folderHash: nil, in: [sharedDocs], actorHex: b)
                    .isEmpty)
        }

        /// A reader's items advertise reading only — no write, create, delete,
        /// rename or move — while a writable set's keep the full surface.
        @Test func aReadersItemsAdvertiseNoWrite() {
            let write: NSFileProviderItemCapabilities = [
                .allowsWriting, .allowsAddingSubItems, .allowsRenaming, .allowsDeleting,
                .allowsReparenting,
            ]
            let readerFile = FileProviderItemCapabilities.capabilities(isFolder: false, readOnly: true)
            let readerDir = FileProviderItemCapabilities.capabilities(isFolder: true, readOnly: true)
            #expect(readerFile.intersection(write).isEmpty)
            #expect(readerDir.intersection(write).isEmpty)
            #expect(readerFile.contains(.allowsReading))
            #expect(readerDir.contains([.allowsReading, .allowsContentEnumerating]))
            #expect(
                FileProviderItemCapabilities.capabilities(isFolder: false, readOnly: false)
                    .contains([.allowsWriting, .allowsDeleting, .allowsRenaming]))
            #expect(
                FileProviderItemCapabilities.capabilities(isFolder: true, readOnly: false)
                    .contains(.allowsAddingSubItems))
        }

        /// A malformed account fails the plan (the reconcile logs and skips)
        /// rather than planning under a bogus identity.
        @Test func aMalformedActorFailsThePlan() {
            let dir = tempDir()
            defer { try? FileManager.default.removeItem(at: dir) }
            #expect(throws: (any Error).self) {
                try plan([docs, pics], registered: [], actorHex: "zz", dir: dir)
            }
        }
    }

#endif
