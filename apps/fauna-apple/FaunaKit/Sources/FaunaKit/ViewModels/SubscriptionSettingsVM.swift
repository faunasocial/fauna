import SwiftUI

/// Shared view-model for the consumer-side `subscription-settings` page
/// (subscriptions Slice B, monetization.md § Pillar 1 — "what this user
/// subscribes to across all creators, with unsubscribe"), shared by macOS + iOS
/// (one FaunaKit VM). A dumb renderer of the
/// caller-scoped `fauna.subscriptions.mine.list` read over the thin
/// `FfiSubscriptionsClient`; unsubscribe re-reads (observer-free, no client-side
/// caching — `feed.md` § Architectural rules). Mirrors linux
/// `apps/fauna-linux/src/settings/subscriptions.rs` + android
/// `SubscriptionSettingsVM` (same calls, same row shape — priority #1/#3).
///
/// Distinct from `SubscriptionsVM` (the author-side Slice-A Tiers-tab management):
/// this is the *consumer* enumeration of the rows the caller subscribes *to*.
@MainActor @Observable
public final class SubscriptionSettingsVM {
    /// The caller's subscriptions across every creator (active + pending),
    /// rendered one row per `(creator, tier)` (`subscription-mine-list`).
    public private(set) var subscriptions: [FfiMineSubscription] = []

    /// Claim redemption input (`subscription-claim-redeem-input` —
    /// `monetization.md` § Pillar 3 Q4's universal fallback binding).
    ///
    /// Excised with the money plane (`FAUNA_EXCISE_PAYMENTS`) — redeeming a
    /// post-payment code is a buy-side gate surface. The rest of this page (the
    /// caller's own subscription rows, unsubscribe) is the subscriptions plane
    /// and survives.
    #if !FAUNA_EXCISE_PAYMENTS
    public var claimCodeInput = ""

    /// The features courtesy read the redeem waits on: `nil` until it answered.
    public private(set) var featureRows: [FfiFeatureRow]?
    /// The features read threw (and none has answered since).
    public private(set) var featuresReadFailed = false

    /// `subscription-claim-redeem-button`'s answer — the shared features gate
    /// over the `payments` row (``featureGateAnswer(rows:failed:feature:)``,
    /// `profile.md` § Errors & edge cases: the same mapping on Settings →
    /// Subscriptions as on the profile). Absent leaves the redeem section out;
    /// ``redeemClaim()`` asks it again.
    public var redeemAnswer: Possibility {
        featureGateAnswer(rows: featureRows, failed: featuresReadFailed, feature: "payments")
    }
    #endif

    /// Shared page-level error (`error-message`).
    public var errorMessage: String?

    private var api: APIClient?

    public init() {}

    /// Wire the API client and load the consumer list.
    public func configure(api: APIClient) async {
        self.api = api
        #if !FAUNA_EXCISE_PAYMENTS
        featureRows = nil
        featuresReadFailed = false
        #endif
        await hydrate()
        #if !FAUNA_EXCISE_PAYMENTS
        await refreshFeatures()
        #endif
    }

    /// Re-read `mine.list`. Used on mount and after each unsubscribe.
    public func hydrate() async {
        guard let api else { return }
        do {
            subscriptions = try await api.subscriptionMineList()
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Unsubscribe from `authorId`, then re-read. In plaintext mode the row
    /// vanishes (`Removed`); in encrypted mode the nest returns `Queued` and the
    /// row persists until the author commits the removal — mirrors linux/android.
    ///
    /// Takes the `mine.list` row's tier and `homeNestUrl`, so a follow carried
    /// across nests is left at the creator's home (`federated-follow.md` § Decision 2).
    public func unsubscribe(authorId: Data, tier: String, homeNestUrl: String?) async {
        guard let api else { return }
        do {
            _ = try await api.subscriptionUnsubscribe(
                authorId: authorId, tier: tier, homeNestUrl: homeNestUrl)
            errorMessage = nil
            await hydrate()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    #if !FAUNA_EXCISE_PAYMENTS
    /// Redeem the pasted claim code (`fauna.payments.claims.redeem`) — binds the
    /// entitlement to this actor; success clears the input and re-reads
    /// `mine.list`, where the queued grant renders exactly like a queued
    /// subscribe (a "pending" row). Typed `fauna.payments.claim_*` errors
    /// surface via `error-message`. Mirrors linux `redeem_claim`.
    ///
    /// Rides the across-nests redeem with no home — the one call shape both
    /// redeem doors share (the OTHER profile Tiers tab names the creator's home).
    ///
    /// A blank code is refused first with the shared
    /// `redeem_code_possibility`'s reason on `error-message`.
    public func redeemClaim() async {
        // Rule 6: the press asks what the button was painted from — the
        // payments gate first (the plane's reason outranks the draft's, as
        // `Possibility.and` orders them), then the shared
        // `redeem_code_possibility`.
        if let refusal = redeemAnswer.and(subscriptionRedeemCodePossibility(code: claimCodeInput)).refusal {
            errorMessage = refusal
            return
        }
        guard let api else { return }
        let code = claimCodeInput.trimmingCharacters(in: .whitespaces)
        do {
            // No home → the plain local redeem, which never reads the author
            // slot: this page has no creator in hand, so by convention it
            // carries the redeemer's own id.
            guard let me = api.boundActorIdHex else {
                throw APIError.ffiError("No secret for claim redeem")
            }
            _ = try await api.redeemPaymentClaimAcrossNests(
                code: code, authorIdHex: me, authorHomeUrl: nil)
            errorMessage = nil
            claimCodeInput = ""
            await hydrate()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// The features courtesy read ``redeemAnswer`` waits on.
    public func refreshFeatures() async {
        guard let api else { return }
        do {
            let rows = try await api.featureRows()
            guard self.api === api else { return }
            applyFeaturesRead(rows)
        } catch {
            guard self.api === api else { return }
            applyFeaturesRead(nil)
        }
    }

    /// The features read landed (`rows`) or threw (`nil`). Internal so the
    /// unit tests drive it without a nest.
    func applyFeaturesRead(_ rows: [FfiFeatureRow]?) {
        if let rows {
            featureRows = rows
            featuresReadFailed = false
        } else if featureRows == nil {
            featuresReadFailed = true
        }
    }
    #endif
}
