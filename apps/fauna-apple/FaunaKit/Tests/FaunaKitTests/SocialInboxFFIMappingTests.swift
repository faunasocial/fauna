import Testing
import Foundation
@testable import FaunaKit

// Pure, nest-free tests for the social-inbox WS-RPC migration's type plumbing:
// the `Ffi*` (contacts / notifications) → FaunaKit-struct mappings consumed by
// APIClient's `fetchKnocks` / `fetchContacts` / `getInboxMode` /
// `getNotifications` call sites. The round-trip wire conformance lives
// nest-side (`conformance_{contacts,notifications}.rs`); these guard the Swift
// seam.

// MARK: - Knock

@Test func knockMapsFromFfi() {
    let ffi = FfiKnockItem(id: 7, sender: String(repeating: "ab", count: 32),
                           senderNode: "node-a", summary: "alice wants to connect",
                           createdAt: 1000)
    let knock = Knock(ffi: ffi)
    #expect(knock.id == 7)
    #expect(knock.sender == String(repeating: "ab", count: 32))
    #expect(knock.senderNode == "node-a")
    #expect(knock.summary == "alice wants to connect")
    #expect(knock.createdAt == 1000)
}

// MARK: - Contact

@Test func contactMapsFromFfiWithAcceptedAt() {
    // Local peer: handle/domain present, ride through for the roster filter.
    let ffi = FfiContactItem(peerId: String(repeating: "11", count: 32),
                             status: "accepted", acceptedAt: 1234, createdAt: 1200,
                             handle: "alice", domain: "fauna.social")
    let contact = Contact(ffi: ffi)
    #expect(contact.peerId == String(repeating: "11", count: 32))
    #expect(contact.status == "accepted")
    #expect(contact.updatedAt == 1234)
    #expect(contact.handle == "alice")
    #expect(contact.domain == "fauna.social")
    #expect(contact.id == contact.peerId)
}

@Test func contactMapsFromFfiWithNilAcceptedAt() {
    // Federated peer: handle/domain are nil (no cached Profile nest-side).
    let ffi = FfiContactItem(peerId: String(repeating: "22", count: 32),
                             status: "blocked", acceptedAt: nil, createdAt: 1100,
                             handle: nil, domain: nil)
    let contact = Contact(ffi: ffi)
    #expect(contact.status == "blocked")
    #expect(contact.updatedAt == nil)
    #expect(contact.handle == nil)
    #expect(contact.domain == nil)
}

// MARK: - NotificationItem

// The body/summary pair deliberately DISAGREE in both tests below — the same
// shape the e2e's `POST /api/v1/test/push/notify` hook seeds
// (`test_notifications_localized_body.py`) — so a test that painted the
// English `summary` on its own say-so, or resolved an unknown key's raw
// name, could not pass by accident.

@Test func notificationItemPaintsTheLocalizedBodyForAKeyItsCatalogKnows() {
    let ffi = FfiNotifItem(id: 7, notifType: "like", source: "fauna",
                           senderId: String(repeating: "ab", count: 32),
                           contentId: String(repeating: "cd", count: 32),
                           subjectUri: nil,
                           summary: "english fallback (must not be painted)",
                           isRead: false, createdAt: 1_700_000_000_000_000,
                           body: LocalizedText(key: "notifications.row_like",
                                                args: ["sender": "alice"]))
    let item = NotificationItem(ffi: ffi)
    #expect(item.id == "7")
    // The catalog sentence wins over the summary — `notifications.md` §
    // Localized body, compat table's third row.
    #expect(item.body == "alice liked your post")
    // The raw summary still rides through, unpainted, for the router.
    #expect(item.summary == "english fallback (must not be painted)")
    #expect(item.read == false)
    #expect(item.createdAt == 1_700_000_000_000_000)
    // The wire type must survive the mapping: it is what drives the row's
    // per-kind icon (`NotificationTypeIcon` -> shared `notificationGlyphForType`).
    // Dropped here until 2026-09-20, which is why both apple apps painted one
    // fixed bell for every notification.
    #expect(item.notifType == "like")
}

@Test func notificationItemFallsBackToSummaryForAKeyItsCatalogLacks() {
    // The shape of a row a still-newer nest minted: this build's `L` table has
    // no such key. Must paint `summary`, never the raw key — `notifications.md`
    // § Localized body, compat table's fourth row.
    let ffi = FfiNotifItem(id: 9, notifType: "like", source: "fauna",
                           senderId: nil, contentId: nil, subjectUri: nil,
                           summary: "english fallback for a key this app lacks",
                           isRead: false, createdAt: 1_700_000_000_000_000,
                           body: LocalizedText(key: "notifications.row_not_minted_yet",
                                                args: ["sender": "alice"]))
    let item = NotificationItem(ffi: ffi)
    #expect(item.body == "english fallback for a key this app lacks")
    #expect(!item.body.contains("row_not_minted"))
    #expect(item.summary == item.body, "no real body: summary and the fallback body agree")
}

// A bridged row's `subjectUri` must survive the mapping AND reach the router
// through `NotificationOpen.swift`'s reconstructed `FfiNotifItem`: it is the
// one field the `External` arm reads (`notifications.md` § Deep-link
// destinations — a Bluesky row opens its post on bsky.app, never inside the
// app, and never from `contentId`, which is a dedup token). Dropped by the
// mapping until 2026-09-25, which is why no apple row could carry the
// off-app destination.
@Test func aBridgedRowCarriesItsSubjectThroughToTheExternalDestination() {
    let ffi = FfiNotifItem(id: 11, notifType: "like", source: "bluesky",
                           senderId: nil, contentId: "6174",
                           subjectUri: "at://did:plc:xyz/app.bsky.feed.post/3kpost",
                           summary: "alice liked your post",
                           isRead: false, createdAt: 1_700_000_000_000_000,
                           body: nil)
    let item = NotificationItem(ffi: ffi)
    #expect(item.subjectUri == "at://did:plc:xyz/app.bsky.feed.post/3kpost")
    #expect(notificationDestination(for: item, hasFamily: false)
            == .external(url: "https://bsky.app/profile/did:plc:xyz/post/3kpost"))

    // Without a subject the same row is honestly inert — its contentId is not
    // an address of any kind.
    let bare = NotificationItem(id: "12", body: "alice liked your post", createdAt: 1, read: false,
                                notifType: "like", source: "bluesky", contentId: "6174")
    #expect(notificationDestination(for: bare, hasFamily: false) == nil)
}

// The ward-side doorbell goes where its three guardian-side siblings go
// (user-ruled 2026-09-25), under the same `hasFamily` gate.
@Test func theWardSideFeedSourceApprovalOpensTheFamilyPageWhenGatedIn() {
    let item = NotificationItem(id: "13", body: "approved — try again", createdAt: 1, read: false,
                                notifType: "family.feed_source_approved", source: "fauna",
                                contentId: "3432")
    #expect(notificationDestination(for: item, hasFamily: true) == .family)
    #expect(notificationDestination(for: item, hasFamily: false) == nil)
}

// MARK: - Knock toast

// Mirrors `libs/fauna-client-notifications/src/text.rs`'s own
// `a_knock_with_a_known_key_says_the_rows_sentence` /
// `a_knock_with_no_body_names_the_sender` fixtures (same sender id, same
// expected sentences) — these pin the APPLE resolution
// (`knockToastBody(for:)` through `renderLocalizedText`) the shared-Rust
// tests cannot see, not the `knock_text_for` decision itself.

private let knockSenderId = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90"

@Test func knockToastBodyPaintsTheRowSentenceForAKnownKey() {
    let knock = FfiKnock(
        senderId: knockSenderId, summary: "hi, it's me",
        body: LocalizedText(key: "notifications.row_knock",
                             args: ["sender": "a1b2c3d4", "message": "hi, it's me"])
    )
    #expect(knockToastBody(for: knock) == "a1b2c3d4 wants to connect: hi, it's me")
}

@Test func knockToastBodyFallsBackToTheSendersEightHexPrefixWithNoBody() {
    let knock = FfiKnock(senderId: knockSenderId, summary: "hi, it's me", body: nil)
    #expect(knockToastBody(for: knock) == "a1b2c3d4 wants to connect")
}

@Test func notificationGlyphClassifiesTheWireTypeInSharedRust() {
    // The classification is shared Rust's, not this app's
    // (`notifications.md` § Where logic lives). Pinned here because the apple
    // icon picker switches exhaustively over these cases.
    #expect(notificationGlyphForType(notifType: "like") == .like)
    #expect(notificationGlyphForType(notifType: "reply") == .reply)
    #expect(notificationGlyphForType(notifType: "follow") == .follow)
    #expect(notificationGlyphForType(notifType: "mention") == .mention)
    #expect(notificationGlyphForType(notifType: "message") == .message)
    #expect(notificationGlyphForType(notifType: "event_invite") == .eventInvite)
    #expect(notificationGlyphForType(notifType: "group_invite") == .groupInvite)
    #expect(notificationGlyphForType(notifType: "knock") == .knock)
    #expect(notificationGlyphForType(notifType: "abuse_report.received") == .report)
    #expect(notificationGlyphForType(notifType: "abuse_report.resolved") == .report)
    // Anything outside the known set — including the e2e's own `test` type and
    // the empty string — is the generic bell, never a crash.
    #expect(notificationGlyphForType(notifType: "test") == .unknown)
    #expect(notificationGlyphForType(notifType: "") == .unknown)
}
