import SwiftUI

/// Shared view-model for the profile **Tiers tab — SELF author management**
/// (subscriptions Slice A), shared by macOS + iOS (one FaunaKit VM). Three
/// sections render over the shared Rust:
///
/// - **§1 My tiers** — `listSubscriptionTiers` (the authenticated `tiers.list`
///   own-read) + the inline create/edit form (`createSubscriptionTier` via the
///   author orchestration / `updateSubscriptionTier` thin) + `deleteSubscriptionTier`.
/// - **§2 Pending requests** — `listSubscriptionRequests`; approve runs the
///   transparent mint+upload (`approveSubscriber`, showing the busy indicator);
///   reject is the thin `rejectSubscriptionRequest`.
/// - **§3 Subscribers** — `listSubscribers` for the selected tier; remove rotates
///   + re-mints (`removeSubscriber`).
/// - **§4 Payment providers** (`monetization.md` § Pillar 3, IDs reserved
///   2026-07-12) — `listPaymentProviders`; an add/edit form (kind from the
///   shared `fauna-payments` registry, the webhook-verification secret — never
///   echoed back by the nest, so never pre-filled — and the entitled tier from
///   the author's own §1 tiers) over `setPaymentProvider` / `removePaymentProvider`.
/// - **§5 Manual claim codes** (`monetization.md` § Pillar 3) — `listPaymentClaims`
///   (the audit surface for BOTH manually- and webhook-minted codes); mint a new
///   one for a tier via `mintPaymentClaim` (provider always `"manual"`, never
///   expires in this first cut — mirrors linux's `claims_mint(tier, None)`).
///
/// Observer-free: a manual re-read after each mutation (no client-side caching —
/// `feed.md` § Architectural rules), exactly like linux
/// `apps/fauna-linux/src/views/profile/tiers.rs`. The mint orchestration is
/// encrypted-mode only at the crypto layer, but — mirroring the linux reference —
/// the UI drives the same calls in both modes (plaintext mints nest-side).
@MainActor @Observable
public final class SubscriptionsVM {
    // ── §1 My tiers ─────────────────────────────────────────────────────
    public private(set) var tiers: [FfiTierItem] = []
    /// §1's RENDERED list — `tiers` minus any per-post pay-to-unlock designated
    /// tier (`monetization.md:128`). The unlock affordance renders on the sold
    /// post, not in a tier-management list; the author's `tiers.list` read still
    /// carries it (needed to gate the post and audit sales via §5, so `tiers`
    /// itself — and the §3/§5 pickers derived from it — stay unfiltered), so the
    /// exclusion is client-side, scoped to just this list, same shape as linux
    /// `tiers.rs::render_tier_rows`'s `shown`.
    public var myTiers: [FfiTierItem] { tiers.filter { $0.unlocksPost == nil } }
    /// The inline create/edit form is visible.
    public var showForm = false
    /// `Some(name)` while editing an existing tier; `nil` while creating. The tier
    /// name is the server key, so the name field is read-only while editing.
    public private(set) var editingTier: String?
    public var formName = ""
    public var formRank = ""
    public var formDescription = ""
    public var formPriceHint = ""
    /// The machine-comparable threshold (`monetization.md` § The asking price),
    /// typed in SATS — the wire/FFI face converts to msat internally
    /// (`FfiNestClient`'s `subscriptionsCreateTier`/`tiersUpdate`), so this VM
    /// parses text -> `UInt64` and nothing more; independent of `formPriceHint`
    /// (never inferred from it — the two fields are separate by design).
    public var formAskingPrice = ""
    public var formPaymentUrl = ""
    public var formAutoApprove = false

    // ── §2 Pending requests ─────────────────────────────────────────────
    public private(set) var requests: [FfiPendingRequest] = []
    /// `subscription-request-busy` — true while an approve mints + uploads.
    public private(set) var isApproving = false

    // ── §3 Subscribers roster ───────────────────────────────────────────
    /// Tier names index-aligned with the §3 picker; a selection maps to a tier.
    public private(set) var tierNames: [String] = []
    public var selectedTier = ""
    public private(set) var subscribers: [FfiSubscriberEntry] = []

    // ── §4 Payment providers ────────────────────────────────────────────
    //
    // §§4-5 are the money plane's author half and excise with it
    // (`FAUNA_EXCISE_PAYMENTS`; dynamic-features.md § Platform-family surface
    // excision). §§1-3 stay: a tier, its roster and its pending requests are the
    // subscriptions plane, which is not a registry member — and a priced tier a
    // full client authored keeps round-tripping through an excised build
    // untouched (§ Wire-compat posture).
    #if !FAUNA_EXCISE_PAYMENTS
    public private(set) var providers: [FfiProviderItem] = []
    /// The shared `fauna-payments` adapter registry, unlocalized (a kind is an
    /// identifier, not user-facing copy) — populated once at `configure`.
    public private(set) var providerKinds: [String] = []
    public var showProviderForm = false
    public var formProviderKind = ""
    public var formProviderTier = ""
    public var formProviderSecret = ""

    // ── §5 Manual claim codes ───────────────────────────────────────────
    public private(set) var claims: [FfiClaimItem] = []
    /// The §5 mint form's tier picker — index-aligned with `tierNames`, same
    /// convention as §3's `selectedTier`.
    public var selectedClaimTier = ""
    /// `true` while a mint round-trip is in flight (disables the button).
    public private(set) var mintingClaim = false
    #endif

    // ── shared page-level error (`error-message`) ───────────────────────
    public var errorMessage: String?

    private var api: APIClient?
    /// This author's own hex actor id — the §4 webhook-URL preview's
    /// `payments_webhook_url(base_url, author_id_hex, kind)` input. Set once at
    /// `configure` (the caller passes the SELF profile's `viewedActorId`).
    private var authorIdHex = ""

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary), on `SearchVM.reset()`'s shape. Called by
    /// ``configure(api:authorIdHex:)`` on an api-identity change **before** it
    /// re-hydrates, and by the page's nil-client phase: More → Profile is not
    /// unmounted by the iOS switch teardown, and the page's `.task` keyed on the
    /// viewed actor id rather than on the client .
    ///
    /// ⚠ The sharp field is `formProviderSecret` — the §4 payment-provider form's
    /// API secret, typed by the author. Like the bridge link form's credentials
    /// (`BridgeManagerVM.reset()`), a survivor would be submitted under the incoming
    /// account's api. The §3 subscriber roster and the §2 pending requests are other
    /// accounts' own identities, and `claims` are mintable codes.
    ///
    /// `providerKinds` goes too even though the adapter registry itself is
    /// install-scoped: ``configure(api:authorIdHex:)`` re-seeds it from the incoming
    /// api on the next line, so leaving it would be the one field whose staleness a
    /// reader of this drop could not explain.
    public func reset() {
        api = nil
        authorIdHex = ""
        tiers = []
        showForm = false
        editingTier = nil
        formName = ""
        formRank = ""
        formDescription = ""
        formPriceHint = ""
        formAskingPrice = ""
        formPaymentUrl = ""
        formAutoApprove = false
        requests = []
        isApproving = false
        tierNames = []
        selectedTier = ""
        subscribers = []
        #if !FAUNA_EXCISE_PAYMENTS
        providers = []
        providerKinds = []
        showProviderForm = false
        formProviderKind = ""
        formProviderTier = ""
        formProviderSecret = ""
        claims = []
        selectedClaimTier = ""
        mintingClaim = false
        #endif
        errorMessage = nil
    }

    /// Wire the API client + this author's own actor id, seed the §4 kind
    /// registry (pure/sync), and load §1 + §2 + the §3 roster + §4 + §5.
    public func configure(api: APIClient, authorIdHex: String) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        self.authorIdHex = authorIdHex
        #if !FAUNA_EXCISE_PAYMENTS
        providerKinds = api.paymentProviderKinds()
        #endif
        await hydrate()
    }

    #if !FAUNA_EXCISE_PAYMENTS
    /// The exact webhook URL to register at the selected §4 provider's
    /// dashboard (`subscription-provider-form-webhook-url`) — the shared
    /// `payments_webhook_url`, recomputed live as `formProviderKind` changes.
    /// Empty until `configure` has run or while the form has no kind selected.
    public var providerWebhookUrl: String {
        guard let api, !formProviderKind.isEmpty else { return "" }
        return paymentsWebhookUrl(
            baseUrl: api.nodeUrl.absoluteString, authorIdHex: authorIdHex, kind: formProviderKind)
    }
    #endif

    /// Re-read the tier list + pending requests + configured providers + claim
    /// codes, repopulate the §3/§5 pickers (preserving the prior selection by
    /// name), and refresh the roster. Used on mount and after every mutation.
    public func hydrate() async {
        guard let api else { return }
        do {
            let loadedTiers = try await api.listSubscriptionTiers()
            let loadedRequests = try await api.listSubscriptionRequests()
            #if !FAUNA_EXCISE_PAYMENTS
            let loadedProviders = try await api.listPaymentProviders()
            let loadedClaims = try await api.listPaymentClaims()
            #endif
            // The in-flight clause (`account-scoping.md` § The scoping taxonomy):
            // four reads, so the widest window on this page — and what they carry is
            // the subscriber roster and the mintable claim codes.
            guard self.api === api else { return }
            tiers = loadedTiers
            requests = loadedRequests
            #if !FAUNA_EXCISE_PAYMENTS
            providers = loadedProviders
            claims = loadedClaims
            #endif
            errorMessage = nil
            // Repopulate §3/§5, preserving the prior selection by name.
            let names = loadedTiers.map { $0.name }
            tierNames = names
            if !names.contains(selectedTier) {
                selectedTier = names.first ?? ""
            }
            #if !FAUNA_EXCISE_PAYMENTS
            if !names.contains(selectedClaimTier) {
                selectedClaimTier = names.first ?? ""
            }
            #endif
            await refreshRoster()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    /// Read the selected tier's roster and re-render §3.
    public func refreshRoster() async {
        guard let api, !selectedTier.isEmpty else {
            subscribers = []
            return
        }
        let tier = selectedTier
        do {
            subscribers = try await api.listSubscribers(tierName: tier)
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    // MARK: - §1 form

    public func openCreateForm() {
        editingTier = nil
        formName = ""
        formRank = ""
        formDescription = ""
        formPriceHint = ""
        formAskingPrice = ""
        formPaymentUrl = ""
        formAutoApprove = false
        errorMessage = nil
        showForm = true
    }

    public func openEditForm(_ tier: FfiTierItem) {
        editingTier = tier.name
        formName = tier.name
        formRank = String(tier.rank)
        formDescription = tier.description ?? ""
        formPriceHint = tier.priceHint ?? ""
        formAskingPrice = tier.askingPriceSats.map(String.init) ?? ""
        formPaymentUrl = tier.paymentUrl ?? ""
        formAutoApprove = tier.autoApprove
        errorMessage = nil
        showForm = true
    }

    public func cancelForm() {
        editingTier = nil
        showForm = false
    }

    /// Create or update the tier depending on `editingTier`, then refresh.
    public func saveForm() async {
        guard let api else { return }
        let name = formName.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else { return }
        let rank = UInt32(formRank.trimmingCharacters(in: .whitespaces)) ?? 0
        let description = optTrim(formDescription)
        let priceHint = optTrim(formPriceHint)
        let paymentUrl = optTrim(formPaymentUrl)
        let auto = formAutoApprove
        // Empty OR unparseable -> nil, same shape as the fields above and the
        // lead app's own form (`apps/fauna-tui/src/profile/mod.rs`): on an
        // update, nil means KEEP the current price, never clear it
        // (`monetization.md` § The asking price — no per-app clear affordance
        // ships here, only the dedicated `tiers.clear_field` verb can unset).
        let askingPrice = optTrim(formAskingPrice).flatMap { UInt64($0) }
        do {
            if editingTier != nil {
                _ = try await api.updateSubscriptionTier(
                    name: name, rank: rank, description: description,
                    priceHint: priceHint, paymentUrl: paymentUrl, autoApprove: auto,
                    askingPriceSats: askingPrice)
            } else {
                _ = try await api.createSubscriptionTier(
                    name: name, rank: rank, description: description,
                    priceHint: priceHint, paymentUrl: paymentUrl, autoApprove: auto,
                    askingPriceSats: askingPrice)
            }
            errorMessage = nil
            showForm = false
            editingTier = nil
            await hydrate()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    public func deleteTier(_ name: String) async {
        guard let api else { return }
        do {
            try await api.deleteSubscriptionTier(name: name)
            errorMessage = nil
            await hydrate()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    // MARK: - §2 requests

    /// Approve a pending request — the transparent mint+upload; shows the busy
    /// indicator for its duration.
    public func approve(_ request: FfiPendingRequest) async {
        guard let api else { return }
        isApproving = true
        do {
            try await api.approveSubscriber(request: request)
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
        isApproving = false
        await hydrate()
    }

    public func reject(requestId: Int64) async {
        guard let api else { return }
        do {
            try await api.rejectSubscriptionRequest(requestId: requestId)
            errorMessage = nil
            await hydrate()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    // MARK: - §3 roster

    public func selectTier(_ name: String) async {
        selectedTier = name
        await refreshRoster()
    }

    /// Remove a subscriber from `tierName` — rotates the period key + re-mints.
    public func remove(subscriberId: Data, tierName: String) async {
        guard let api else { return }
        do {
            try await api.removeSubscriber(tierName: tierName, subscriberId: subscriberId)
            errorMessage = nil
            await refreshRoster()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    // MARK: - §4 Payment providers

    #if !FAUNA_EXCISE_PAYMENTS
    /// Open the add-provider form, defaulting the kind/tier selects to their
    /// first option (mirrors linux's `DropDown` default-selects-index-0). The
    /// secret starts empty — it is write-only, never pre-filled even when
    /// re-adding a kind that already has a saved config (that's an overwrite,
    /// not an edit-in-place; the nest has no read-back to prefill from).
    public func openProviderForm() {
        formProviderKind = providerKinds.first ?? ""
        formProviderTier = tierNames.first ?? ""
        formProviderSecret = ""
        errorMessage = nil
        showProviderForm = true
    }

    public func cancelProviderForm() {
        showProviderForm = false
        formProviderSecret = ""
    }

    /// Save the provider config (`fauna.payments.providers.set`), then refresh.
    /// The secret is cleared from the form immediately after the call succeeds
    /// or fails — it is a credential, not state to linger in a view model.
    public func saveProviderForm() async {
        guard let api else { return }
        let kind = formProviderKind
        let tier = formProviderTier
        let secret = formProviderSecret
        guard !kind.isEmpty, !tier.isEmpty, !secret.isEmpty else { return }
        do {
            _ = try await api.setPaymentProvider(kind: kind, webhookSecret: secret, tier: tier)
            errorMessage = nil
            showProviderForm = false
            formProviderSecret = ""
            await hydrate()
        } catch {
            formProviderSecret = ""
            errorMessage = DisplayError.message(error)
        }
    }

    /// Remove a configured provider by kind (`fauna.payments.providers.remove`),
    /// then refresh — a webhook to the removed provider then 404s nest-side.
    public func removeProvider(kind: String) async {
        guard let api else { return }
        do {
            _ = try await api.removePaymentProvider(kind: kind)
            errorMessage = nil
            await hydrate()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    // MARK: - §5 Manual claim codes

    /// Manually mint a claim code for `selectedClaimTier` (provider always
    /// `"manual"`, no expiry — mirrors linux `claims_mint(tier, None)`), then
    /// refresh so the new code appears in the audit list.
    public func mintClaim() async {
        guard let api, !selectedClaimTier.isEmpty else { return }
        mintingClaim = true
        do {
            _ = try await api.mintPaymentClaim(tier: selectedClaimTier, validUntil: nil)
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
        mintingClaim = false
        await hydrate()
    }
    #endif
}

/// Trim + collapse an empty field to `nil` (the optional wire shape), mirroring
/// linux `opt` in `tiers.rs`.
private func optTrim(_ s: String) -> String? {
    let t = s.trimmingCharacters(in: .whitespaces)
    return t.isEmpty ? nil : t
}
