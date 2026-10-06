import Foundation
#if os(iOS)
import DeviceCheck
import UIKit
#if canImport(DeclaredAgeRange)
import DeclaredAgeRange
#endif
#endif

/// The real `StoreAgeSignals` — Declared Age Range (iOS 26+) and App Attest.
/// Live on iOS only: macOS ships outside the App Store and has no store age
/// signal (`family-safety.md` § The account age band, D3 — the mobile apps), so
/// off iOS the type exists (the iOS views also build for the macOS host) but
/// shares no range and never attests.
public struct AppleStoreAgeSignals: StoreAgeSignals {
    /// The age gates asked of the store — the band boundaries, so the shared
    /// range folds exactly to `U13 | 13-15 | 16-17 | 18+`.
    static let ageGates = (13, 16, 18)

    public init() {}

    public func ageRange() async throws -> StoreAgeRange? {
        #if os(iOS) && canImport(DeclaredAgeRange)
        guard #available(iOS 26, *) else { return nil }
        guard let presenter = await Self.presenter() else { return nil }
        let response = try await AgeRangeService.shared.requestAgeRange(
            ageGates: Self.ageGates.0, Self.ageGates.1, Self.ageGates.2, in: presenter)
        switch response {
        case .sharing(let range):
            return StoreAgeRange(lower: range.lowerBound.map { UInt32(clamping: $0) },
                                 upper: range.upperBound.map { UInt32(clamping: $0) })
        case .declinedSharing:
            return nil
        @unknown default:
            return nil
        }
        #else
        return nil
        #endif
    }

    public func attest(clientDataHash: Data) async throws -> AppAttestResult {
        #if os(iOS)
        let service = DCAppAttestService.shared
        guard service.isSupported else { throw AppAttestUnavailable.unsupported }
        // A fresh key per attestation: a key attests once, and admissions are rare.
        let keyId = try await service.generateKey()
        let object = try await service.attestKey(keyId, clientDataHash: clientDataHash)
        guard let rawKeyId = Data(base64Encoded: keyId) else { throw AppAttestUnavailable.malformedKeyId }
        return AppAttestResult(keyId: rawKeyId, attestationObject: object)
        #else
        throw AppAttestUnavailable.unsupported
        #endif
    }

    enum AppAttestUnavailable: Error { case unsupported, malformedKeyId }

    #if os(iOS)
    /// The top-most presented controller of the key window — what the system
    /// age-range prompt presents over.
    @MainActor
    static func presenter() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let window = scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? scenes.first?.windows.first
        var top = window?.rootViewController
        while let presented = top?.presentedViewController { top = presented }
        return top
    }
    #endif
}
