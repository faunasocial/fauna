import Foundation
import os
#if os(iOS)
import StoreKit
import UIKit
#if canImport(DeclaredAgeRange)
import DeclaredAgeRange
#endif
#if canImport(PermissionKit)
import PermissionKit
#endif
#endif

/// The real `StoreConsentSignals` (`account-age-band.md` § The account age band
/// → the store-law developer duties bullet, items (3) and (4)): DeclaredAgeRange's
/// `requiredRegulatoryFeatures` (iOS 26.4+) and StoreKit's
/// `AppStore.ageRatingCode` (iOS 26.2+). Live on iOS only — macOS ships outside
/// the App Store and declares the store-consent lock absent, so off iOS the
/// type exists (the iOS views also build for the macOS host) and reads nothing,
/// which never gates. Older iOS reads nothing too: the duty's own API is the
/// only way to know it applies.
public struct AppleStoreConsentSignals: StoreConsentSignals {
    public init() {}

    public func regulatoryFeatures() async throws -> StoreRegulatoryFeatures? {
        #if os(iOS) && canImport(DeclaredAgeRange)
        guard #available(iOS 26.4, *) else { return nil }
        let features = try await AgeRangeService.shared.requiredRegulatoryFeatures
        return StoreRegulatoryFeatures(
            parentalConsentRequired: features.contains(.significantAppChangeRequiresParentalConsent),
            adultNotificationRequired: features.contains(.significantAppChangeRequiresAdultNotification))
        #else
        return nil
        #endif
    }

    public func ageRatingCode() async -> Int64? {
        #if os(iOS)
        guard #available(iOS 26.2, *) else { return nil }
        return await AppStore.ageRatingCode.map { Int64($0) }
        #else
        return nil
        #endif
    }
}

/// The real `StoreConsentAsker`: PermissionKit's significant-update ask
/// (iOS 26.2+) and DeclaredAgeRange's adult acknowledgment sheet (iOS 26.4+),
/// both presented over the top-most controller. Inert off iOS.
public struct AppleStoreConsentAsker: StoreConsentAsker {
    public init() {}

    public func ask(updateDescription: String) async throws {
        #if os(iOS) && canImport(PermissionKit)
        guard #available(iOS 26.2, *) else { throw Unavailable.osTooOld }
        guard let presenter = await AppleStoreAgeSignals.presenter() else { throw Unavailable.noPresenter }
        let question = PermissionQuestion(significantAppUpdateTopic: SignificantAppUpdateTopic(
            description: updateDescription))
        try await AskCenter.shared.ask(question, in: presenter)
        #else
        throw Unavailable.osTooOld
        #endif
    }

    public func acknowledge(updateDescription: String) async {
        #if os(iOS) && canImport(DeclaredAgeRange)
        guard #available(iOS 26.4, *),
              let scene = await AppleStoreAgeSignals.presenter()?.view.window?.windowScene else { return }
        do {
            try await AgeRangeService.shared.showSignificantUpdateAcknowledgment(
                in: scene, updateDescription: updateDescription)
        } catch {
            Logger(subsystem: "social.fauna", category: "store-consent")
                .warning("adult acknowledgment sheet not shown: \(String(describing: error), privacy: .public)")
        }
        #endif
    }

    enum Unavailable: Error { case osTooOld, noPresenter }
}

extension StoreConsentStore {
    /// Hand every PermissionKit answer to a significant-update ask to the
    /// round, for the life of the process. Answers arrive on this device
    /// (`AskCenter.responses(for:)`) whenever the parent decides — the app may
    /// have been backgrounded and relaunched in between. Call once at launch.
    @MainActor
    public func listenForAnswers() {
        #if os(iOS) && canImport(PermissionKit)
        guard #available(iOS 26.2, *) else { return }
        Task { @MainActor [weak self] in
            for await response in AskCenter.shared.responses(for: SignificantAppUpdateTopic.self) {
                switch response.choice.answer {
                case .approval: await self?.receive(answer: .approved)
                case .denial: await self?.receive(answer: .declined)
                @unknown default: break
                }
            }
        }
        #endif
    }
}
