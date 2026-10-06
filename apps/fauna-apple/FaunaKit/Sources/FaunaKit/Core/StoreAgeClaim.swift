import Foundation
import os

/// The iOS **store-age arm** of the account age band (`family-safety.md`
/// § The account age band, D3 store signals + D5 attested age-at-admission):
/// ask the store for the user's Declared Age Range, fold it to the band
/// (shared Rust — `ageBandFromAgeRange`), harden it with an App Attest
/// attestation over the machine-minted nonce, and hand the claim to the
/// onboarding machine, which carries it on both admission bodies. The android
/// twin is `StoreAgeClaim.kt`; the shape is the same on purpose.
///
/// Every step is **best-effort by design**: no store signal, no attestation,
/// a build with no App ID to sign over (`iosAgeAttestationAppId()` is `nil`),
/// or a nest whose nonce reply does not list `ios` in `attestationPlatforms`
/// (it cannot check an App Attest object, so the round would be discarded)
/// means the admission simply carries less — a declared-only claim, or none —
/// and the nest's provenance records exactly that (the claim corroborates the
/// admitting adult; it never decides). The platform list is an economy, never
/// the guarantee: the shared onboarding machine strips an attestation the nest
/// did not offer to verify regardless (`family-safety.md` § The account age
/// band → *An attestation the nest cannot check*).
///
/// Two seams keep the round unit-testable on macOS: `StoreAgeSignals` is the
/// platform half (`AppleStoreAgeSignals`, iOS 26+ only), and `AgeClaimMachine`
/// is the shared-Rust calls it makes. Nothing here assembles a wire struct.

/// The store's age range as the platform shares it — either bound may be absent.
public struct StoreAgeRange: Equatable, Sendable {
    public let lower: UInt32?
    public let upper: UInt32?
    public init(lower: UInt32?, upper: UInt32?) {
        self.lower = lower
        self.upper = upper
    }
}

/// One App Attest round's output: the raw key id (the SHA-256 of the attested
/// public key) and the CBOR attestation object.
public struct AppAttestResult: Equatable, Sendable {
    public let keyId: Data
    public let attestationObject: Data
    public init(keyId: Data, attestationObject: Data) {
        self.keyId = keyId
        self.attestationObject = attestationObject
    }
}

/// The two platform calls the arm needs.
public protocol StoreAgeSignals {
    /// The user's shared age range, or nil when the store shares none (the
    /// user or parent declined, the OS predates Declared Age Range, or the
    /// entitlement is absent).
    func ageRange() async throws -> StoreAgeRange?
    /// Generate a fresh App Attest key and attest it over `clientDataHash`
    /// (the machine's `ageClaimDigest`). Throws when the device cannot attest —
    /// the caller degrades to a declared-only claim.
    func attest(clientDataHash: Data) async throws -> AppAttestResult
}

/// The shared-Rust calls the round makes — a seam over `OnboardingMachine`
/// (which conforms as generated) plus the free `ageBandFromAgeRange` fold.
public protocol AgeClaimMachine: AnyObject {
    func requestAgeNonce() async throws -> AgeNoncePlain
    func ageClaimDigest(nonceHex: String, band: String, applicationId: String) throws -> Data
    func ageClaim() -> AgeClaimPlain?
    func setAgeClaim(claim: AgeClaimPlain?)
    func bandFromAgeRange(lower: UInt32?, upper: UInt32?) -> String?
}

extension OnboardingMachine: AgeClaimMachine {
    public func bandFromAgeRange(lower: UInt32?, upper: UInt32?) -> String? {
        ageBandFromAgeRange(lower: lower, upper: upper)
    }
}

/// The store-age round for one onboarding wizard. `attach` runs when the
/// `invite_request` page appears, so the shared notice paints BEFORE the user
/// submits or redeems; `prepareAdmission` runs right before each admission
/// call. An attestation's nonce is single-use (the nest consumes it first, even
/// on a refusal) and lives `expiresInSecs`, so every admission after the first
/// — or after the TTL — re-mints nonce + attestation over the same band.
@MainActor
public final class StoreAgeClaimRound {
    /// `AgeAttestationPlain.platform` for this arm — the nest's `"ios"` seam.
    public static let platform = "ios"
    /// Re-attest this long before the nonce's own expiry, so a slow submit
    /// never presents a nonce that lapses in flight.
    static let expiryMargin: TimeInterval = 30

    private let signals: StoreAgeSignals
    /// The team-prefixed App ID (`application_id` of the signed claim), or nil
    /// when there is nothing to sign over — then the round never attests. It is
    /// an input to the signed message, not a gate on the admission.
    private let applicationId: String?
    private let now: () -> Date
    private let log = Logger(subsystem: "social.fauna", category: "store-age")

    /// When the current attestation's nonce stops being usable; nil = spent
    /// (or no attestation at all).
    private var attestationUsableUntil: Date?

    public init(signals: StoreAgeSignals,
                applicationId: String? = iosAgeAttestationAppId(),
                now: @escaping () -> Date = Date.init) {
        self.signals = signals
        self.applicationId = applicationId
        self.now = now
    }

    /// Ask the store once and set the claim on the machine. A claim already on
    /// the machine is kept (a return to the page never re-prompts), and a round
    /// that yields nothing leaves the machine untouched.
    public func attach(to machine: AgeClaimMachine) async {
        guard machine.ageClaim() == nil else { return }
        let range: StoreAgeRange?
        do {
            range = try await signals.ageRange()
        } catch {
            warn("store age range unavailable, no claim: \(error)")
            return
        }
        guard let range, let band = machine.bandFromAgeRange(lower: range.lower, upper: range.upper) else {
            return
        }
        let attestation = await attestOrNil(band: band, machine: machine)
        machine.setAgeClaim(claim: AgeClaimPlain(band: band, attestation: attestation))
    }

    /// Right before an admission call: re-mint a spent or stale attestation
    /// (a failure downgrades the claim to declared-only rather than sending a
    /// nonce the nest will refuse), then mark it spent — the call about to run
    /// consumes it.
    public func prepareAdmission(on machine: AgeClaimMachine) async {
        guard let claim = machine.ageClaim(), claim.attestation != nil else { return }
        if let until = attestationUsableUntil, now() < until {
            attestationUsableUntil = nil
            return
        }
        let attestation = await attestOrNil(band: claim.band, machine: machine)
        machine.setAgeClaim(claim: AgeClaimPlain(band: claim.band, attestation: attestation))
        attestationUsableUntil = nil
    }

    private func attestOrNil(band: String, machine: AgeClaimMachine) async -> AgeAttestationPlain? {
        guard let applicationId else { return nil }
        do {
            let nonce = try await machine.requestAgeNonce()
            guard nonce.attestationPlatforms.contains(Self.platform) else {
                attestationUsableUntil = nil
                warn("nest does not verify \(Self.platform) attestations, claim is declared-only")
                return nil
            }
            let minted = now()
            let digest = try machine.ageClaimDigest(nonceHex: nonce.nonceHex, band: band,
                                                    applicationId: applicationId)
            let result = try await signals.attest(clientDataHash: digest)
            attestationUsableUntil = minted.addingTimeInterval(
                TimeInterval(nonce.expiresInSecs) - Self.expiryMargin)
            return AgeAttestationPlain(
                platform: Self.platform,
                nonceHex: nonce.nonceHex,
                keyIdHex: result.keyId.map { String(format: "%02x", $0) }.joined(),
                attestationObject: result.attestationObject)
        } catch {
            attestationUsableUntil = nil
            warn("attestation unavailable, claim is declared-only: \(error)")
            return nil
        }
    }

    private func warn(_ reason: String) {
        log.warning("store-age claim degraded: \(reason, privacy: .public)")
    }
}
