import Foundation
import Testing

@testable import FaunaKit

#if canImport(FileProvider)

    /// Headless pins that the reconcile feeds apple's arbitration precedence
    /// into the shared-Rust presence plan (`FileProviderCoordinator.plan` —
    /// `on-demand-files.md` § Apple File Provider binding): the bound refs are
    /// the plan's *stronger* presence, so a bound set never gets a domain
    /// whatever its toggle, and the binding matches by ref, never by name. The
    /// rule itself (stronger-outranks-toggle) is pinned in Rust
    /// (`fauna_folders_machine::on_demand_presence`); this is the wiring.
    @Suite struct FileProviderArbitrationTests {
        private let actor = String(repeating: "aa", count: 32)
        private let docs = PresenceSet(name: "docs", folderId: "local:1", thisDeviceAccepts: true, role: .own)
        private let pics = PresenceSet(name: "pics", folderId: "local:2", thisDeviceAccepts: true, role: .own)

        private func desired(
            _ sets: [PresenceSet], bound: Set<String>, disabled: [PresenceSet] = []
        ) throws -> [String] {
            let dir = FileManager.default.temporaryDirectory
                .appendingPathComponent("fp-arb-\(UUID().uuidString)")
            defer { try? FileManager.default.removeItem(at: dir) }
            let prefs = FfiOnDemandPrefsStore(path: dir.appendingPathComponent("prefs.json").path)
            for set in disabled {
                try prefs.setEnabled(
                    scopedId: FileProviderDomainIdentity(actorIdHex: actor, folderId: set.folderId)!
                        .domainId,
                    enabled: false)
            }
            return try FileProviderCoordinator.plan(
                actorHex: actor, sets: sets, boundSets: bound, prefs: prefs, registered: [],
                ownersAt: dir.appendingPathComponent(FileProviderDomainOwners.filename)
            ).add.map(\.set.folderId)
        }

        @Test func toggleOnUnboundSetsGetDomains() throws {
            #expect(try desired([docs, pics], bound: []) == ["local:1", "local:2"])
        }

        /// The banned shape this kills: a bound set's resident engine AND an
        /// FP domain engine writing one set from one device id — even with
        /// its toggle ON.
        @Test func aBoundSetNeverGetsADomain() throws {
            #expect(try desired([docs, pics], bound: ["local:1"]) == ["local:2"])
        }

        @Test func theToggleStillGatesUnboundSets() throws {
            #expect(try desired([docs, pics], bound: ["local:1"], disabled: [pics]).isEmpty)
        }

        /// The name is a label two sets can share: binding one set never
        /// suppresses the domain of a same-named set with a different ref.
        @Test func aBindingMatchesByRefNotByName() throws {
            let otherDocs = PresenceSet(name: "docs", folderId: "local:9", thisDeviceAccepts: true, role: .own)
            #expect(try desired([docs, otherDocs], bound: ["local:9"]) == ["local:1"])
        }
    }

#endif
