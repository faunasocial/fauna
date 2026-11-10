import SwiftUI

/// The two render placeholders the content pillar paints in place of a flagged
/// item's body (`family-safety.md` § Content policy, Slice C) — shared by all
/// three apple render surfaces (the macOS + iOS feed post-cards and the
/// conversation bubble) so they cannot drift on what enforcement looks like
/// (priority #2, the apple twins of linux's `build_content_block`/
/// `build_content_collapse` and android's `ContentPolicyBlockedBody`).

/// The **block** placeholder — a policy-naming notice in place of the whole
/// body, with **no reveal**. A `block` verdict is always a guardian floor (a
/// viewer's own threshold only ever collapses), so the notice names the family
/// policy; the ward sees *that* something was hidden and why, never a silent
/// disappearance (§ The trust shape invariant 4).
///
/// `content-policy-blocked-notice` is the one ui.yaml id this pillar renders
/// (indexed, per card/bubble).
///
/// `reported` is the same notice for the viewer's OWN report
/// (`moderation.md` § Corollary — block also hides): "You reported this" in
/// place of the family words, `source="reported"` on the element, no body and no
/// reveal. A personal filter, so it names the viewer's own act — not a policy.
///
/// `reason` is the guardian rule's own reason, verbatim beside the frame
/// (`content-policy-blocked-reason`; `nest-rules.md` § The shared rule editor →
/// *The reason is the member's to read*) — present only when a rule of the
/// guardian's document drove the verdict.
public struct ContentPolicyBlockedNotice: View {
    private let reported: Bool
    private let reason: String?
    private let showAgain: FfiShowAgain?
    private let onShowAgain: ([String]) -> Void

    /// `showAgain` is the shared `reported_show_again` fold for this item — kept
    /// only on the `reported` arm, where it paints
    /// `content-policy-show-again-button` (`moderation.md` § Corollary — the
    /// reporter's hide is theirs to undo); `onShowAgain` receives its ids.
    public init(
        reported: Bool = false, reason: String? = nil,
        showAgain: FfiShowAgain? = nil, onShowAgain: @escaping ([String]) -> Void = { _ in }
    ) {
        self.reported = reported
        self.reason = reason
        self.showAgain = reported ? showAgain : nil
        self.onShowAgain = onShowAgain
    }

    private var text: String {
        reported ? L.moderation.report.hiddenPlaceholder : L.family.contentBlockedNotice
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(text)
                .accessibilityIdentifier(Ids.contentPolicyBlockedNotice)
                .automationValue(
                    Ids.contentPolicyBlockedNotice, text: { text },
                    attributes: { ["source": reported ? "reported" : "policy", "state": "blocked"] })
                .font(.caption)
                .italic()
                .foregroundStyle(.secondary)
            if let showAgain {
                let label = renderLocalizedText(showAgain.label)
                let press = { onShowAgain(showAgain.ids) }
                Button(label, action: press)
                    .buttonStyle(.borderless)
                    .font(.caption2)
                    .accessibilityIdentifier(Ids.contentPolicyShowAgainButton)
                    .automationActivate(
                        Ids.contentPolicyShowAgainButton, text: { label }, perform: press)
            }
            GuardianReasonText(reason: reason)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The guardian rule's reason, verbatim, beside a family-policy placeholder's
/// fixed frame (`content-policy-blocked-reason`). Paints nothing when no rule of
/// the guardian's document drove the verdict (`reason` nil). The block and
/// collapse arms of the feed card, the post detail and the conversation bubble
/// all paint through here, so the surfaces cannot drift on the id or the
/// placement (android's `GuardianReason`).
public struct GuardianReasonText: View {
    private let reason: String?

    public init(reason: String?) {
        self.reason = reason
    }

    public var body: some View {
        if let reason {
            Text(reason)
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier(Ids.contentPolicyBlockedReason)
                .automationValue(Ids.contentPolicyBlockedReason, text: { reason })
        }
    }
}

/// The **collapse** placeholder + one-tap reveal. Session-local: revealing
/// un-collapses this one item for the rest of the session and never writes
/// back — the floor (or the viewer's own threshold) persists, and relaxing it is
/// what stops future collapse. A `collapse` can come from either source, so the
/// text stays neutral rather than naming the family policy.
///
/// The block arm's `content-policy-blocked-notice` in `state="collapsed"` beside
/// `content-policy-collapsed-reveal-button` (ids user-approved 2026-10-16; tui
/// leads, android's `ContentPolicyCollapsedBody` is the twin).
public struct ContentPolicyCollapsedPlaceholder: View {
    private let reason: String?
    private let onReveal: () -> Void

    public init(reason: String? = nil, onReveal: @escaping () -> Void) {
        self.reason = reason
        self.onReveal = onReveal
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Text(L.family.contentCollapsedNotice)
                    .accessibilityIdentifier(Ids.contentPolicyBlockedNotice)
                    .automationValue(
                        Ids.contentPolicyBlockedNotice, text: { L.family.contentCollapsedNotice },
                        attributes: { ["source": "policy", "state": "collapsed"] })
                    .font(.caption)
                    .italic()
                    .foregroundStyle(.secondary)
                Spacer(minLength: 4)
                Button(L.family.contentRevealButton) {
                    onReveal()
                }
                .buttonStyle(.borderless)
                .font(.caption2)
                .accessibilityIdentifier(Ids.contentPolicyCollapsedRevealButton)
                .automationActivate(Ids.contentPolicyCollapsedRevealButton) { onReveal() }
            }
            GuardianReasonText(reason: reason)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
