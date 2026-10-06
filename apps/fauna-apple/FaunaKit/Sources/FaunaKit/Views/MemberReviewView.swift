import SwiftUI

/// The permanent **Members To Review** Settings sub-page (`ui.yaml` page
/// `member_review`; rail placement directly after Account —
/// `docs/goal/ui/settings.md` § Navigation model). Item (iv) of
/// `succession-aftermath.md` § Propagation's two-surface ruling: renders
/// whatever a review sweep left unanswered. Shared by both apple targets, one
/// FaunaKit view. Zero logic owed here (priority #2) — every seam is
/// `libs/fauna-ffi/src/member_review.rs`. Reference: tui
/// `settings/member_review.rs`, android `MemberReviewScreen.kt`.
///
/// ⚠ **There is no sweep gate here, and that is the entire point.** The
/// (not-yet-built-on-apple) ephemeral kit-side pass gates on a live succession
/// sweep so it appears only inside the ceremony; this page has no such gate —
/// a deferred backlog must stay reachable after the ceremony that raised it
/// scrolls away. The only condition here is whether there is anything open.
///
/// ⚠ **`member-review-empty` is not a safety verdict** and its copy must never
/// read as one: an empty review list says nothing about whether the account
/// is safe, only that nothing is currently awaiting a decision.
public struct MemberReviewView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MemberReviewVM()

    public init() {}

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.settings.memberReviewPage.title)
                    .font(.title2)

                // Absent from the tree when nil — a registered-but-empty
                // element would read as present.
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                // `loaded` gates the empty state so a read still in flight
                // never paints "nothing to review" (docs/goal/ui/README.md §
                // *List pages: loading is not empty*).
                if vm.loaded && vm.rows.isEmpty {
                    automationText(Ids.memberReviewEmpty, L.settings.memberReviewPage.empty)
                        .foregroundStyle(.secondary)
                } else if !vm.rows.isEmpty {
                    // The lead line — the one thing the ephemeral pass never
                    // has to say: a user opening this page weeks later has no
                    // ceremony around them to explain why these names are here.
                    Text(L.settings.memberReviewPage.intro)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)

                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(vm.rows) { row in
                            memberReviewRow(row)
                        }
                    }
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.settings.memberReviewPage.title)
        .task {
            guard let client else { return }
            vm.configure(api: client.api, onRosterChanged: { [weak client] in
                await client?.reloadMemberReviewRoster()
            })
            await vm.load()
        }
    }

    /// One `member-review-row` per still-open PERSON, with that person's
    /// `member-review-keep-button`/`member-review-remove-button` pair scoped
    /// **inside** it — the same row family the (not-yet-built) ephemeral pass
    /// will share, one collapse per person off
    /// `SuccessionLedger::open_member_reviews`, never one row per item.
    ///
    /// Neither button carries a `.faunaGate`: Keep rides `fauna.account.state.put`
    /// (OfflineSafe) and Remove rides `fauna.conversations.channel.send`
    /// (OfflineQueued) — neither kind is `OnlineOnly`, so a gate on either
    /// could never desensitize anything (offline-gate-check ruling 1).
    @ViewBuilder
    private func memberReviewRow(_ row: MemberReviewRow) -> some View {
        let who = renderLocalizedText(row.text.who)
        let reasonText = row.text.reasons.map(renderLocalizedText).joined(separator: ", ")
        let rowText = L.settings.recoveryKit.reviewRow(who: who, reason: reasonText)
        VStack(alignment: .leading, spacing: 8) {
            Text(rowText)
            HStack(spacing: 8) {
                Button(L.settings.recoveryKit.reviewKeep) {
                    Task { await vm.keep(person: row.person) }
                }
                .accessibilityIdentifier(Ids.memberReviewKeepButton)
                .automationActivate(Ids.memberReviewKeepButton) {
                    Task { await vm.keep(person: row.person) }
                }
                Button(L.settings.recoveryKit.reviewRemove, role: .destructive) {
                    Task { await vm.remove(person: row.person) }
                }
                .accessibilityIdentifier(Ids.memberReviewRemoveButton)
                .automationActivate(Ids.memberReviewRemoveButton) {
                    Task { await vm.remove(person: row.person) }
                }
            }
        }
        // Keep BOTH the row id AND the child button ids queryable (a bare
        // container id would clobber children — memory note / MailListMembersView).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.memberReviewRow)
        // Per-row presence entry so the flat in-process registry can `count`
        // `member-review-row` rows, its text the row's own display text.
        .automationValue(Ids.memberReviewRow, text: { rowText })
    }
}
