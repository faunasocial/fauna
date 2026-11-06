import SwiftUI

/// The admin block's notice (`takedown-and-block.md` § Admin block of a reported
/// item): the shared `admin_block_notice(reason)` face worded for the user, painted
/// by every surface that carries an `adminBlockReason` — the quoted-post embed, the
/// post detail and the DM bubble. No dedicated id (the notice rides the body's own
/// position, as the legal-takedown tombstone does).
public func adminBlockNoticeText(_ reason: String) -> String {
    renderLocalizedTextNested(adminBlockNotice(reason: reason))
}

/// Whether a blocked record's notice stands IN PLACE of the body (decision 4's
/// withheld shape: the document is empty — Rust `MessageSnapshot::admin_block_in_place`)
/// or paints BESIDE the author's own kept body.
public func adminBlockInPlace(_ document: RenderDocument) -> Bool {
    document.blocks.isEmpty
}

/// The notice line painted beside an author's kept body.
public struct AdminBlockNoticeLine: View {
    let reason: String

    public init(reason: String) { self.reason = reason }

    public var body: some View {
        Text(adminBlockNoticeText(reason))
            .font(.caption)
            .italic()
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}
