import Foundation
import Testing
@testable import FaunaKit

// The iOS store-age arm (`family-safety.md` § The account age band, D3 + D5):
// best-effort throughout, never attests with no App ID to sign over or for a nest
// whose nonce reply does not list `ios`, and never presents a spent or stale nonce.

private struct Unavailable: Error {}

private final class FakeSignals: StoreAgeSignals {
    var range: StoreAgeRange?
    var rangeThrows = false
    var attestThrows = false
    var attestedHashes: [Data] = []

    init(range: StoreAgeRange?) { self.range = range }

    func ageRange() async throws -> StoreAgeRange? {
        if rangeThrows { throw Unavailable() }
        return range
    }

    func attest(clientDataHash: Data) async throws -> AppAttestResult {
        if attestThrows { throw Unavailable() }
        attestedHashes.append(clientDataHash)
        return AppAttestResult(keyId: Data([0xab, 0x01]), attestationObject: Data([0xa1, UInt8(attestedHashes.count)]))
    }
}

private final class FakeMachine: AgeClaimMachine {
    var claim: AgeClaimPlain?
    var noncesMinted = 0
    var digestArgs: [(String, String, String)] = []
    /// What the fake nest's nonce reply lists as verifiable (`attestationPlatforms`).
    var platforms: [String] = ["ios", "android"]

    func requestAgeNonce() async throws -> AgeNoncePlain {
        noncesMinted += 1
        return AgeNoncePlain(nonceHex: String(repeating: String(format: "%02x", noncesMinted), count: 32),
                             expiresInSecs: 300, attestationPlatforms: platforms)
    }

    func ageClaimDigest(nonceHex: String, band: String, applicationId: String) throws -> Data {
        digestArgs.append((nonceHex, band, applicationId))
        return Data(nonceHex.utf8.prefix(4))
    }

    func ageClaim() -> AgeClaimPlain? { claim }
    func setAgeClaim(claim: AgeClaimPlain?) { self.claim = claim }

    func bandFromAgeRange(lower: UInt32?, upper: UInt32?) -> String? {
        // The real fold is shared Rust (`fauna_protocol::age::AgeBand::from_age_range`,
        // Rust-tested); the fake only needs to be deterministic.
        guard lower != nil || upper != nil else { return nil }
        return (upper ?? 99) < 16 ? "13-15" : "18+"
    }
}

private let appId = "ABCDE12345.social.fauna.fauna"

@Suite @MainActor struct StoreAgeClaimRoundTests {
    @Test func noStoreSignalSetsNoClaim() async {
        let machine = FakeMachine()
        await StoreAgeClaimRound(signals: FakeSignals(range: nil), applicationId: appId).attach(to: machine)
        #expect(machine.claim == nil)
        #expect(machine.noncesMinted == 0)
    }

    @Test func aFailingStoreLeavesAnExistingClaimAlone() async {
        let machine = FakeMachine()
        let existing = AgeClaimPlain(band: "16-17", attestation: nil)
        machine.claim = existing
        let signals = FakeSignals(range: StoreAgeRange(lower: 13, upper: 15))
        signals.rangeThrows = true
        await StoreAgeClaimRound(signals: signals, applicationId: appId).attach(to: machine)
        #expect(machine.claim == existing)
    }

    @Test func anUnarmedBuildSendsDeclaredOnlyAndNeverAttests() async {
        let machine = FakeMachine()
        let signals = FakeSignals(range: StoreAgeRange(lower: 13, upper: 15))
        let round = StoreAgeClaimRound(signals: signals, applicationId: nil)
        await round.attach(to: machine)
        #expect(machine.claim == AgeClaimPlain(band: "13-15", attestation: nil))
        #expect(machine.noncesMinted == 0, "no nonce is minted for an attestation that will never be made")
        #expect(signals.attestedHashes.isEmpty)
        await round.prepareAdmission(on: machine)
        #expect(machine.claim == AgeClaimPlain(band: "13-15", attestation: nil))
    }

    @Test(arguments: [["android"], []] as [[String]])
    func aNestThatDoesNotListIosGetsADeclaredOnlyClaimAndNoAttestRound(listed: [String]) async {
        let machine = FakeMachine()
        machine.platforms = listed
        let signals = FakeSignals(range: StoreAgeRange(lower: 13, upper: 15))
        let round = StoreAgeClaimRound(signals: signals, applicationId: appId)
        await round.attach(to: machine)
        #expect(machine.claim == AgeClaimPlain(band: "13-15", attestation: nil))
        #expect(signals.attestedHashes.isEmpty, "no App Attest round for an attestation no nest will read")
        #expect(machine.digestArgs.isEmpty)
        await round.prepareAdmission(on: machine)
        #expect(machine.claim == AgeClaimPlain(band: "13-15", attestation: nil))
        #expect(machine.noncesMinted == 1, "a declared-only claim is never re-attested")
        #expect(signals.attestedHashes.isEmpty)
    }

    @Test func aNestListingIosGetsTheAttestedClaim() async throws {
        let machine = FakeMachine()
        machine.platforms = ["ios"]
        let signals = FakeSignals(range: StoreAgeRange(lower: 13, upper: 15))
        await StoreAgeClaimRound(signals: signals, applicationId: appId).attach(to: machine)
        #expect(try #require(machine.claim?.attestation).platform == "ios")
        #expect(signals.attestedHashes.count == 1)
    }

    @Test func anArmedBuildAttestsOverTheMachineDigest() async throws {
        let machine = FakeMachine()
        let signals = FakeSignals(range: StoreAgeRange(lower: 13, upper: 15))
        await StoreAgeClaimRound(signals: signals, applicationId: appId).attach(to: machine)
        let attestation = try #require(machine.claim?.attestation)
        #expect(machine.claim?.band == "13-15")
        #expect(attestation.platform == "ios")
        #expect(attestation.nonceHex == String(repeating: "01", count: 32))
        #expect(attestation.keyIdHex == "ab01")
        #expect(machine.digestArgs.first?.2 == appId, "the team-prefixed App ID is the signed application id")
        #expect(signals.attestedHashes == [Data(attestation.nonceHex.utf8.prefix(4))])
    }

    @Test func aFailedAttestationDegradesToDeclaredOnly() async {
        let machine = FakeMachine()
        let signals = FakeSignals(range: StoreAgeRange(lower: 18, upper: nil))
        signals.attestThrows = true
        await StoreAgeClaimRound(signals: signals, applicationId: appId).attach(to: machine)
        #expect(machine.claim == AgeClaimPlain(band: "18+", attestation: nil))
    }

    @Test func aReturnToThePageNeverReprompts() async {
        let machine = FakeMachine()
        let signals = FakeSignals(range: StoreAgeRange(lower: 13, upper: 15))
        let round = StoreAgeClaimRound(signals: signals, applicationId: appId)
        await round.attach(to: machine)
        await round.attach(to: machine)
        #expect(machine.noncesMinted == 1)
    }

    @Test func theFirstAdmissionUsesTheFreshAttestationTheSecondReMints() async throws {
        let machine = FakeMachine()
        let round = StoreAgeClaimRound(signals: FakeSignals(range: StoreAgeRange(lower: 13, upper: 15)),
                                       applicationId: appId)
        await round.attach(to: machine)
        let first = try #require(machine.claim?.attestation)
        await round.prepareAdmission(on: machine)
        #expect(machine.claim?.attestation == first, "a fresh, unspent attestation rides the first admission")
        await round.prepareAdmission(on: machine)
        let second = try #require(machine.claim?.attestation)
        #expect(second.nonceHex != first.nonceHex, "the nest consumed the first nonce; the redeem needs its own")
        #expect(machine.claim?.band == "13-15")
    }

    @Test func aStaleAttestationIsReMintedBeforeTheNonceLapses() async throws {
        let machine = FakeMachine()
        var clock = Date(timeIntervalSince1970: 1_000)
        let round = StoreAgeClaimRound(signals: FakeSignals(range: StoreAgeRange(lower: 13, upper: 15)),
                                       applicationId: appId, now: { clock })
        await round.attach(to: machine)
        let first = try #require(machine.claim?.attestation)
        clock = clock.addingTimeInterval(300 - StoreAgeClaimRound.expiryMargin + 1)
        await round.prepareAdmission(on: machine)
        #expect(machine.claim?.attestation?.nonceHex != first.nonceHex)
        #expect(machine.noncesMinted == 2)
    }

    @Test func aFailedReMintDowngradesRatherThanPresentingASpentNonce() async throws {
        let machine = FakeMachine()
        let signals = FakeSignals(range: StoreAgeRange(lower: 13, upper: 15))
        let round = StoreAgeClaimRound(signals: signals, applicationId: appId)
        await round.attach(to: machine)
        await round.prepareAdmission(on: machine)
        signals.attestThrows = true
        await round.prepareAdmission(on: machine)
        #expect(machine.claim == AgeClaimPlain(band: "13-15", attestation: nil))
    }
}
