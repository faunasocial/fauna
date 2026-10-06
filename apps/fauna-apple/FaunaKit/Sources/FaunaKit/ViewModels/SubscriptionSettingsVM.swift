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
    #endif

    /// Shared page-level error (`error-message`).
    public var errorMessage: String?

    private var api: APIClient?

    public init() {}

    /// Wire the API client and load the consumer list.
    public func configure(api: APIClient) async {
        self.api = api
        await hydrate()
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
    public func unsubscribe(authorId: Data) async {
        guard let api else { return }
        do {
            _ = try await api.subscriptionUnsubscribe(authorId: authorId)
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
    public func redeemClaim() async {
        guard let api else { return }
        let code = claimCodeInput.trimmingCharacters(in: .whitespaces)
        guard !code.isEmpty else { return }
        do {
            _ = try await api.redeemPaymentClaim(code: code)
            errorMessage = nil
            claimCodeInput = ""
            await hydrate()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }
    #endif
}
