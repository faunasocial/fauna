import SwiftUI

/// The consumer-side **subscription-settings** page (`monetization.md` § Pillar 1
/// — "what this user subscribes to across all creators, with unsubscribe"; a
/// Settings sub-page, sibling of mail-settings/web-settings), shared by macOS +
/// iOS (one FaunaKit view, thin per-target mount points).
/// A dumb renderer of `SubscriptionSettingsVM` over the caller-scoped
/// `fauna.subscriptions.mine.list` read; unsubscribe re-reads (observer-free).
/// Element IDs match `tests/e2e-unified/ui.yaml` `subscription-settings` /
/// `subscription-mine-list` exactly. Reference renderer: linux
/// `apps/fauna-linux/src/settings/subscriptions.rs` + android
/// `SubscriptionSettingsScreen.kt` (same ui.yaml IDs).
public struct SubscriptionSettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The owner of the private-overlay projection the creator's name is read
    /// through (`contacts.md` § The private overlay).
    @Environment(ConversationsVM.self) private var conversationsVM
    @State private var vm = SubscriptionSettingsVM()

    public init() {}

    public var body: some View {
        bodyContainer
            .pageTitle(L.subscriptions.title)
            .task {
                guard let client else { return }
                await vm.configure(api: client.api)
            }
    }

    /// The page body. Production: a grouped `Form`. Under e2e (`FaunaE2E.isActive`):
    /// a non-lazy `ScrollView { VStack }`. Rationale (apple TRACK 1, harness-
    /// confirmed): on iOS a `Form`/`List` is `UITableView`-backed and
    /// lazily instantiates cells, so a row below the fold never `.onAppear`s → its
    /// `automation*` ids never register in-process. A `ScrollView` of a non-lazy
    /// `VStack` mounts every row eagerly. macOS `Form`s render eagerly already, so
    /// the swap is iOS-motivated but platform-uniform + behaviour-/ID-neutral —
    /// only the container changes. `MailSettingsView` is the reference impl.
    @ViewBuilder
    private var bodyContainer: some View {
        if FaunaE2E.isActive {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    sectionsContent
                }
                .padding()
            }
        } else {
            Form {
                sectionsContent
            }
            .formStyle(.grouped)
        }
    }

    /// The page's sections, shared by the production `Form` and the e2e
    /// `ScrollView`/`VStack` so the two never diverge.
    @ViewBuilder
    private var sectionsContent: some View {
        mineSection
        // The buy-side gate surface excises with the money plane; the caller's
        // own subscription rows above are the subscriptions plane and stay
        // (`dynamic-features.md` § Platform-family surface excision).
        #if !FAUNA_EXCISE_PAYMENTS
        claimRedeemSection
        #endif
        if let error = vm.errorMessage {
            Section { ErrorBanner(message: error) }
        }
    }

    /// `subscription-mine-section` + `subscription-mine-list` — one row per
    /// `(creator, tier)` the caller subscribes to.
    private var mineSection: some View {
        Section(L.subscriptions.mySubscriptions) {
            if vm.subscriptions.isEmpty {
                Text(L.subscriptions.noSubscriptions)
                    .font(.caption).foregroundStyle(.secondary)
            }
            // Index-keyed (not by `authorId`): a caller may subscribe to one
            // creator at multiple tiers (ui.yaml "one row per (creator, tier)"),
            // so the author id alone is not unique. The e2e reads rows by index.
            // What the viewer calls each creator: their nickname for them, else
            // the handle the nest resolved, else the hex actor id — the shared
            // projection's answer (value-formatting.md § Subscription author
            // label), read once per render.
            let overlays = conversationsVM.contactOverlays
            ForEach(Array(vm.subscriptions.enumerated()), id: \.offset) { _, sub in
                let author = overlays.subscriptionAuthorLabel(
                    handle: sub.handle, authorId: sub.authorId)
                SubscriptionMineRow(subscription: sub, author: author) {
                    Task { await vm.unsubscribe(authorId: sub.authorId) }
                }
                // `.contain` keeps both this row id AND its child cell ids
                // queryable (a bare container id otherwise clobbers children —
                // memory apple-section-accessibilityid-clobbers-children).
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.subscriptionMineRow)
                .automationValue(Ids.subscriptionMineRow, text: { author })
            }
        }
        // `.contain` keeps the section container id AND the child row ids
        // queryable; the read exposes the row count (mirrors ProfileView's
        // §-section pattern). The e2e waits on this id, then counts the rows.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.subscriptionMineSection)
        .automationValue(Ids.subscriptionMineSection, text: { String(vm.subscriptions.count) })
    }

    #if !FAUNA_EXCISE_PAYMENTS
    /// Claim redemption (`monetization.md` § Pillar 3 Q4's universal fallback
    /// binding): paste a post-payment claim code → the entitlement binds to
    /// this actor and lands as a queued grant in the section above. Mirrors
    /// linux `apps/fauna-linux/src/settings/subscriptions.rs`'s claim section.
    private var claimRedeemSection: some View {
        Section(L.subscriptions.redeemClaimTitle) {
            HStack {
                TextField(L.subscriptions.claimCode, text: $vm.claimCodeInput)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.subscriptionClaimRedeemInput)
                    .automationField(Ids.subscriptionClaimRedeemInput, text: $vm.claimCodeInput)
                Button(L.subscriptions.redeem) { Task { await vm.redeemClaim() } }
                    .accessibilityIdentifier(Ids.subscriptionClaimRedeemButton)
                    .automationActivate(Ids.subscriptionClaimRedeemButton) {
                        Task { await vm.redeemClaim() }
                    }
                    // The code field beside it is buffer and stays typeable.
                    .faunaGate("fauna.payments.claims.redeem")
            }
        }
    }
    #endif
}

// MARK: - Row

private struct SubscriptionMineRow: View {
    let subscription: FfiMineSubscription
    /// What the viewer calls the creator (the shared projection's label).
    let author: String
    let onUnsubscribe: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            automationText(Ids.subscriptionMineAuthor, author)
                .lineLimit(1).truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            automationText(Ids.subscriptionMineTier, subscription.tier)
            // Raw wire status ("active" | "pending"), rendered verbatim — uniform
            // with linux/android/windows (a tier_3 e2e asserts the literal text).
            automationText(Ids.subscriptionMineStatus, subscription.status)

            Button(L.subscriptions.unsubscribe) { onUnsubscribe() }
                .accessibilityIdentifier(Ids.subscriptionMineUnsubscribeButton)
                .automationActivate(Ids.subscriptionMineUnsubscribeButton) { onUnsubscribe() }
                .faunaGate("fauna.subscriptions.unsubscribe")
        }
    }
}
