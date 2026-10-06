import Foundation
import Testing

@testable import FaunaKit

#if canImport(FileProvider)

    /// Headless pins for the actor-scoped device identity
    /// (`on-demand-files.md` § Apple File Provider binding, *the actor-scoped
    /// device identity*): the three device registries the File Provider
    /// binding keeps per set — the domain identifier, the staging root and
    /// the on-demand-toggle preference — are keyed by the set's ref scoped to
    /// the account, so two accounts' `local:1` on one device never share any
    /// of them; the grammar lives in shared Rust and the Swift wrapper only
    /// round-trips it. The iOS upload-drain gate's decision is pinned here
    /// too: a scoped identifier is gated on ITS account's state for ITS set,
    /// never skipped because the raw identifier is not itself a `FolderRef`.
    /// The toggle pins run against a temp-path shared-Rust store, never the
    /// app's own (testing.md § conventions point 10).
    @Suite struct FileProviderDomainIdentityTests {
        private let a = String(repeating: "aa", count: 32)
        private let b = String(repeating: "bb", count: 32)

        /// The ratified shape: `<ref-component>@<actor-id-hex>` — the ref half
        /// percent-encoded so the identifier carries neither `/` nor `:` (iOS
        /// refuses both) — round-tripping through the shared parser, the bare
        /// ref recoverable for the Rust seams, and the same nest row under two
        /// accounts two distinct identifiers.
        @Test func twoAccountsSameRefAreTwoIdentities() throws {
            let ia = try #require(FileProviderDomainIdentity(actorIdHex: a, folderId: "local:1"))
            let ib = try #require(FileProviderDomainIdentity(actorIdHex: b, folderId: "local:1"))
            #expect(ia.domainId == "local%3A1@\(a)")
            #expect(!ia.domainId.contains("/") && !ia.domainId.contains(":"))
            #expect(ia.domainId != ib.domainId)
            #expect(FileProviderDomainIdentity.parse(ia.domainId) == ia)
            #expect(ia.folderId == "local:1", "the Rust host and drain seams still take the bare ref")
            #expect(ia.folderComponent == "local%3A1")
            #expect(ia.actorIdHex == a)
            let upper = try #require(
                FileProviderDomainIdentity(actorIdHex: a.uppercased(), folderId: "local:1"))
            #expect(upper == ia, "the account half is case-normalized, like the owner record")
        }

        /// A bare ref, a set name, the pre-2026-09-29 spelling (the bare `:`
        /// in the ref half), a malformed account or a non-ref set is no
        /// identity — nothing is registered, keyed or served by it.
        @Test func anUnscopedOrMalformedIdentifierIsNoIdentity() {
            #expect(FileProviderDomainIdentity.parse("local:1") == nil, "pre-scoping bare ref")
            #expect(FileProviderDomainIdentity.parse("docs") == nil, "pre-baseline set name")
            #expect(
                FileProviderDomainIdentity.parse("local:1@\(a)") == nil,
                "the pre-encoding spelling iOS refuses is not an identity either")
            #expect(FileProviderDomainIdentity(actorIdHex: "abcd", folderId: "local:1") == nil)
            #expect(FileProviderDomainIdentity(actorIdHex: a, folderId: "docs") == nil)
        }

        /// The staging root nests account-outermost — the identifier's `@`
        /// boundary is the directory boundary — so two accounts' `local:1`
        /// stage into two directories, the ref a single percent-encoded
        /// component under the account's subtree, spelled exactly as the
        /// identifier's ref half.
        @Test func stagingRootsNestPerAccount() throws {
            let ia = try #require(FileProviderDomainIdentity(actorIdHex: a, folderId: "local:1"))
            let ib = try #require(FileProviderDomainIdentity(actorIdHex: b, folderId: "local:1"))
            #expect(ia.stagingRootRelativePath == "FileProvider/roots/\(a)/local%3A1")
            #expect(ia.stagingRootRelativePath.hasSuffix("/\(ia.folderComponent)"))
            #expect(ia.domainId.hasPrefix("\(ia.folderComponent)@"), "one spelling, identifier and directory")
            #expect(ia.stagingRootRelativePath != ib.stagingRootRelativePath)
            #expect(ib.stagingRootRelativePath.hasPrefix("FileProvider/roots/\(b)/"))
            let foreign = try #require(
                FileProviderDomainIdentity(actorIdHex: a, folderId: "foreign:\(String(repeating: "cd", count: 32))"))
            #expect(
                foreign.stagingRootRelativePath.split(separator: "/").count == 4,
                "the ref never adds a path level of its own")
        }

        /// The on-demand toggle is per set per ACCOUNT per device: one
        /// account flipping its `local:1` off leaves the other's `local:1` at
        /// the default ON.
        @Test func togglePreferenceIsPerAccount() throws {
            let dir = scratch()
            defer { try? FileManager.default.removeItem(at: dir) }
            let store = FileProviderDomainPrefs.open(at: dir.appendingPathComponent(FileProviderDomainPrefs.filename))
            let ia = try #require(FileProviderDomainIdentity(actorIdHex: a, folderId: "local:1"))
            let ib = try #require(FileProviderDomainIdentity(actorIdHex: b, folderId: "local:1"))

            #expect(FileProviderDomainPrefs.isEnabled(domainId: ia.domainId, store: store), "default ON")
            FileProviderDomainPrefs.setEnabled(false, domainId: ia.domainId, store: store)
            #expect(!FileProviderDomainPrefs.isEnabled(domainId: ia.domainId, store: store))
            #expect(
                FileProviderDomainPrefs.isEnabled(domainId: ib.domainId, store: store),
                "A's flip never hides B's local:1")
            FileProviderDomainPrefs.setEnabled(true, domainId: ia.domainId, store: store)
            #expect(FileProviderDomainPrefs.isEnabled(domainId: ia.domainId, store: store))
        }

        /// The iOS upload-drain gate keys on the PARSED identity: a removal
        /// outside a plan (sign-out, the bind-time yield) is scoped to its own
        /// account and set exactly as the plan's removals are (so a scoped
        /// domain with un-uploaded edits still refuses removal), while an
        /// identifier that scopes no set to any account — never served, so
        /// never drainable — carries no scope and is not gated; a domain
        /// macOS registered under the pre-2026-09-29 spelling takes that
        /// un-gated path.
        @Test func removalGateConsultsTheScopedAccountAndSet() throws {
            let ia = try #require(FileProviderDomainIdentity(actorIdHex: a, folderId: "local:1"))
            #expect(
                FileProviderDomains.removal(domainId: ia.domainId).scope
                    == PresenceScope(actorIdHex: a, folderId: "local:1"))
            #expect(FileProviderDomains.removal(domainId: "local:1").scope == nil)
            #expect(FileProviderDomains.removal(domainId: "local:1@\(a)").scope == nil)
            #expect(FileProviderDomains.removal(domainId: "docs").scope == nil)
        }

        /// The host's cross-account guard, arm by arm (`on-demand-files.md`
        /// § Apple File Provider binding, *the actor-scoped device identity*):
        /// parse first, then the identifier's own account against the
        /// provisioned one — independent of the owner record — then the record
        /// as defense in depth. The Rust host cross-checks none of it, so these
        /// arms are the only control.
        @Test func hostAdmissionServesOnlyTheProvisionedAccountsScopedDomain() throws {
            let ia = try #require(FileProviderDomainIdentity(actorIdHex: a, folderId: "local:1"))
            let ib = try #require(FileProviderDomainIdentity(actorIdHex: b, folderId: "local:1"))
            #expect(
                hostAdmission(domainId: ia.domainId, provisionedActorHex: a, recordedOwner: nil)
                    == .serve(ia), "no record yet (crash before the record write) is served")
            #expect(
                hostAdmission(domainId: ia.domainId, provisionedActorHex: a, recordedOwner: a)
                    == .serve(ia))
            #expect(
                hostAdmission(domainId: ib.domainId, provisionedActorHex: a, recordedOwner: nil)
                    == .foreign, "structural: another account's identifier, no record")
            #expect(
                hostAdmission(domainId: ib.domainId, provisionedActorHex: a, recordedOwner: a)
                    == .foreign, "structural, independent of a record naming the provisioned account")
            #expect(
                hostAdmission(domainId: ia.domainId, provisionedActorHex: a, recordedOwner: b)
                    == .foreign, "record: own scoped id whose recorded owner is another account")
        }

        /// An identifier that scopes no set to any account is never served —
        /// even when a record names the provisioned account.
        @Test func hostAdmissionRefusesAnUnscopedIdentifier() {
            for id in ["local:1", "docs", "local%3A1@abcd", "local:1@\(a)"] {
                #expect(
                    hostAdmission(domainId: id, provisionedActorHex: a, recordedOwner: a)
                        == .unscoped, "\(id)")
            }
        }

        private func scratch() -> URL {
            FileManager.default.temporaryDirectory.appendingPathComponent("fp-prefs-\(UUID().uuidString)")
        }
    }

#endif
