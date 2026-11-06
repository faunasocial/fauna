import SwiftUI

/// The post author line — `post-author` on the list card, `feed-post-detail-author`
/// on the detail: the author's name as the viewer reads it, and the door to that
/// author's profile (`ui/feed.md` § User actions; `profile.md` § Layout & flow →
/// *Reaching another actor's profile with their home carried*). Both ids stay
/// `type: text` in ui.yaml — the activation is the element's click, not a new id.
///
/// The activation opens the author's profile carrying `PostSummary.homeNestUrl`;
/// an id naming the viewer normalizes to SELF at the app's opener
/// (`profileNavTarget`). Text + activation register as ONE automation entry so the
/// id keeps answering `get_text` and `click` (no duplicate id).
public struct PostAuthorLine: View {
    let id: String
    let label: String
    let onOpen: () -> Void

    public init(id: String, label: String, onOpen: @escaping () -> Void) {
        self.id = id
        self.label = label
        self.onOpen = onOpen
    }

    public var body: some View {
        Text(label)
            .accessibilityIdentifier(id)
            .contentShape(Rectangle())
            .onTapGesture(perform: onOpen)
            .automationActivate(id, text: { label }, perform: onOpen)
    }
}
