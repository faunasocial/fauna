import SwiftUI

/// The community room's search strip (`docs/goal/behavior/room-search.md` § The
/// surface): the field and its submit, painted while the strip is open on the
/// thread (`ThreadDetail.room.search` is non-nil). Submit-driven — typing alone
/// asks nothing — and the draft is local view state that goes with the strip when
/// it closes. The query, the leg, the rows and every word on them are the shared
/// manager's (`room_search`); this paints them and hands the typed query back.
/// Shared by the macOS + iOS conversations detail views, the lift of tui's
/// `room_search_elements` and android's `RoomSearchStrip`.
public struct RoomSearchStrip: View {
    public var onSubmit: (String) -> Void

    public init(onSubmit: @escaping (String) -> Void) {
        self.onSubmit = onSubmit
    }

    @State private var draft = ""

    public var body: some View {
        HStack(spacing: 8) {
            TextField(L.conversations.unified.roomSearchPlaceholder, text: $draft)
                .textFieldStyle(.roundedBorder)
                .onSubmit { onSubmit(draft) }
                .accessibilityIdentifier(Ids.roomSearchField)
                // The in-process driver only sees `automation*` modifiers; the
                // typed value must reach the same `@State` a keystroke fills.
                .automationField(Ids.roomSearchField, text: $draft)
            Button {
                onSubmit(draft)
            } label: { Image(systemName: "magnifyingglass") }
                .buttonStyle(.borderless)
                .help(L.common.search)
                .accessibilityIdentifier(Ids.roomSearchSubmitButton)
                .automationActivate(Ids.roomSearchSubmitButton) { onSubmit(draft) }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
    }
}

/// Whether a submitted query's rows take the message stream's place
/// (`room-search.md` § The surface): from the moment one is out until the strip
/// closes. An open strip with nothing submitted leaves the stream where it is.
public func roomSearchReplacesStream(_ search: RoomSearchSnapshot?) -> Bool {
    guard let search else { return false }
    return search.status != .idle
}

/// What the submitted query painted, in place of the message stream: one row per
/// hit in the order served, or the empty state once the home nest answered. In
/// flight and failed paint no result region at all — a driver waits on a row, the
/// empty state or `error-message`, never on a "searching" element.
///
/// A row's words are the shared snapshot's: sender, time and the opened first
/// line (or the shared *not on this device yet* copy), joined as the other apps
/// join them. The `kind` attribute (`message | post`) is for drivers only.
public struct RoomSearchResults: View {
    public let search: RoomSearchSnapshot
    public var onOpenHit: (RoomSearchNav) -> Void

    public init(search: RoomSearchSnapshot, onOpenHit: @escaping (RoomSearchNav) -> Void) {
        self.search = search
        self.onOpenHit = onOpenHit
    }

    public var body: some View {
        if search.status == .done {
            if search.results.isEmpty {
                Text(L.conversations.unified.roomSearchNoResults)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    .padding(16)
                    .accessibilityIdentifier(Ids.roomSearchNoResults)
                    .automationValue(
                        Ids.roomSearchNoResults, text: { L.conversations.unified.roomSearchNoResults })
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(Array(search.results.enumerated()), id: \.offset) { _, row in
                            let text = roomSearchRowText(row)
                            let kind = roomSearchKindToken(row.kind)
                            Text(text)
                                .font(.callout)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 16)
                                .padding(.vertical, 10)
                                .contentShape(Rectangle())
                                .onTapGesture { onOpenHit(row.navigation) }
                                .accessibilityIdentifier(Ids.roomSearchResultItem)
                                .automationActivate(
                                    Ids.roomSearchResultItem,
                                    text: { text },
                                    attributes: { ["kind": kind] }
                                ) { onOpenHit(row.navigation) }
                        }
                    }
                }
            }
        }
    }
}

/// A row's visible words — sender, time, first line — joined with a middle dot,
/// the parts the snapshot has (`room-search.md` § The surface → *What a hit row
/// paints*).
func roomSearchRowText(_ row: RoomSearchResultSnapshot) -> String {
    [
        row.senderDisplay,
        row.sentAtMs.map { ValueFormat.conversationTimestamp(thenMs: $0) },
        row.text.isEmpty ? nil : row.text,
    ]
    .compactMap { $0 }
    .joined(separator: " · ")
}

/// The row's `kind` attribute token, the one `RoomSearchResultKind::attr_token`
/// spells (`message | post`) — a driver-facing label, not a decision.
func roomSearchKindToken(_ kind: RoomSearchResultKind) -> String {
    switch kind {
    case .message: return "message"
    case .post: return "post"
    }
}
