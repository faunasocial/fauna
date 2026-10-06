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
public struct ContentPolicyBlockedNotice: View {
    public init() {}

    public var body: some View {
        automationText(Ids.contentPolicyBlockedNotice, L.family.contentBlockedNotice)
            .font(.caption)
            .italic()
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The **collapse** placeholder + one-tap reveal. Session-local: revealing
/// un-collapses this one item for the rest of the session and never writes
/// back — the floor (or the viewer's own threshold) persists, and relaxing it is
/// what stops future collapse. A `collapse` can come from either source, so the
/// text stays neutral rather than naming the family policy.
///
/// Presentation-only, no test id — v1 e2e drives the `block` case (the
/// linux/web/android precedent).
public struct ContentPolicyCollapsedPlaceholder: View {
    private let onReveal: () -> Void

    public init(onReveal: @escaping () -> Void) {
        self.onReveal = onReveal
    }

    public var body: some View {
        HStack(spacing: 8) {
            Text(L.family.contentCollapsedNotice)
                .font(.caption)
                .italic()
                .foregroundStyle(.secondary)
            Spacer(minLength: 4)
            Button(L.family.contentRevealButton) {
                onReveal()
            }
            .buttonStyle(.borderless)
            .font(.caption2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
