import Foundation

// Social-inbox WS-RPC seam: `Ffi*` (UniFFI mirrors of
// `fauna_protocol::{contacts,notifications}`) → the FaunaKit structs the
// SwiftUI knocks / contacts / notifications surfaces already bind to. The
// Swift structs are faithful (camelCase) mirrors for the fields their views
// consume, so these maps are lossless *for those fields* and the view models
// keep their signatures — the migration is confined to APIClient.swift's call
// sites. (`Contact.updatedAt` is still bound by no current view; surfacing it
// is a separate cross-app UI track, not this transport migration.
// `FfiNotifItem`'s `notifType` joined 2026-09-20 for the per-kind icon, and
// `source` / `contentId` / `senderId` joined 2026-09-21 for the deep-link
// router — see `NotificationOpen.swift`.)

extension Knock {
    init(ffi: FfiKnockItem) {
        self.init(id: Int(ffi.id), sender: ffi.sender, senderNode: ffi.senderNode,
                  summary: ffi.summary, createdAt: Int(ffi.createdAt))
    }
}

extension Contact {
    init(ffi: FfiContactItem) {
        // The roster row's acceptance time maps to the view-model's
        // `updatedAt` (the only timestamp it tracks); `nil` until accepted.
        // `handle`/`domain` ride through so the roster filter can match them
        // (contacts.md § Contact roster filter) — `nil` for a federated peer.
        self.init(peerId: ffi.peerId, status: ffi.status,
                  updatedAt: ffi.acceptedAt.map(Int.init),
                  handle: ffi.handle, domain: ffi.domain)
    }
}

extension NotificationItem {
    init(ffi: FfiNotifItem) {
        // `body` paints the shared `notification_text_for` decision
        // (`notifications.md` § Localized body): the catalog sentence for a
        // `.localized` key this build's `L` table carries, resolved through
        // `renderLocalizedText` — never `renderLocalizedTextNested`, a
        // notification's args are relayed data (a display name, a count),
        // never themselves translatable keys — or the nest's own English
        // `.verbatim` text as-is. `summary` rides through raw alongside it
        // (never painted) so `NotificationOpen.swift`'s reconstructed
        // `FfiNotifItem` can hand the router the row's real summary rather
        // than `body`'s already-resolved text.
        //
        // `notifType` drives the row's per-kind icon through shared Rust's
        // `notificationGlyphForType` (it was dropped here until 2026-09-20, which
        // is why both apple apps painted one fixed bell). `source` / `contentId`
        // / `senderId` / `subjectUri` ride through RAW for the deep-link router,
        // which keys on `source` then `notifType` and reads the ids — dropping
        // the first three here until 2026-09-21 is why both apple apps painted
        // an inert row, and dropping `subjectUri` until 2026-09-25 is why a
        // bridged row could not carry its off-app destination.
        let text: String
        switch notificationTextFor(item: ffi) {
        case let .localized(localizedText): text = renderLocalizedText(localizedText)
        case let .verbatim(verbatimText): text = verbatimText
        }
        self.init(id: String(ffi.id), body: text, summary: ffi.summary,
                  createdAt: Int(ffi.createdAt), read: ffi.isRead,
                  notifType: ffi.notifType,
                  source: ffi.source, contentId: ffi.contentId,
                  senderId: ffi.senderId, subjectUri: ffi.subjectUri)
    }
}

/// `knockTextFor`'s decision (`notifications.md` § The knock toast), resolved
/// through this app's i18n pipeline exactly like `NotificationItem.init(ffi:)`
/// resolves `notificationTextFor` above: the knock row's own sentence for a
/// body this build's catalog knows, else the toast's own sentence naming the
/// sender. Pulled out of `NotificationManager.postKnockNotification` as its
/// own function so the resolution is testable without a notification host
/// (`notificationsAvailable` is always false in a unit-test binary).
func knockToastBody(for knock: FfiKnock) -> String {
    switch knockTextFor(knock: knock) {
    case let .localized(localizedText): return renderLocalizedText(localizedText)
    case let .verbatim(verbatimText): return verbatimText
    }
}
