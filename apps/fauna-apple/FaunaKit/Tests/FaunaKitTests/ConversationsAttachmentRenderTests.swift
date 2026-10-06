import Testing
import Foundation
@testable import FaunaKit

// Conversation-attachment render lift (`docs/goal/ui/conversations.md`
// § Attachments). The render *path* — inbound MIME → `MessageSnapshot.attachments`
// + `cache_attachment_bytes`, and `attachment_bytes(blob_hash)` resolution — is
// covered by `fauna-conversations`' Rust tests. These guard the two Swift seams
// the apple lift adds:
//   1. `FaunaImage.decode` — raw bytes → platform image (the `dm-attachment-image`
//      `Image(platformImage:)` source); must accept real image bytes and reject junk.
//   2. `ConversationsVM.attachmentBytes` — the `Data?` glue over the manager's
//      `[UInt8]?` loader the bubble's `loadAttachment` closure calls.
// The bubble's image-vs-icon branch itself is a `DmMessageBubble` view concern,
// exercised by the cross-app e2e (`test_conversations_attachments.py`).

// A valid 1×1 PNG (transparent). `NSImage(data:)` / `UIImage(data:)` decode it.
private let onePixelPNGBase64 =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="

@Test func faunaImageDecodeAcceptsRealImageRejectsGarbage() {
    // Non-image bytes (the loader returning, say, a text/plain part) → nil, so
    // the bubble falls back to an icon rather than crashing.
    #expect(FaunaImage.decode(Data([0x00, 0x01, 0x02, 0x03])) == nil)
    #expect(FaunaImage.decode(Data()) == nil)

    // Real PNG bytes decode to a platform image — the `dm-attachment-image` source.
    let png = Data(base64Encoded: onePixelPNGBase64)!
    #expect(FaunaImage.decode(png) != nil)
}

@MainActor
@Test func vmAttachmentBytesResolvesCachedBytesViaManager() {
    let vm = ConversationsVM()
    let bytes: [UInt8] = [0xDE, 0xAD, 0xBE, 0xEF]

    // The inbound parse / send echo caches an attachment's plaintext under its
    // content hash; the bubble resolves it back through the VM as `Data`.
    //
    // The hash must be the REAL lowercase-hex BLAKE3 of `bytes`: the store is
    // content-addressed and the shared door verifies the pair before caching
    // (`docs/goal/ui/conversations.md` § Attachments — an unverified key let any
    // co-member overwrite another conversation's bytes). This read `"deadbeef"`
    // until 2026-09-09, which was never that BLAKE3 — it only passed because
    // nothing checked.
    let blobHash = "53147f3ce49ed4f60dfa5b9654c36ba6103c11f5737df3dabd4cbd296c4161bd"
    vm.manager.cacheAttachmentBytes(blobHash: blobHash, bytes: Data(bytes))
    #expect(vm.attachmentBytes(blobHash) == Data(bytes))

    // The refusal itself, from the app side of the FFI: bytes that do not hash to
    // their declared handle are dropped, not cached.
    vm.manager.cacheAttachmentBytes(blobHash: blobHash, bytes: Data([0x00, 0x01]))
    #expect(vm.attachmentBytes(blobHash) == Data(bytes))

    // An unknown / not-yet-fetched hash resolves to nil (icon fallback).
    #expect(vm.attachmentBytes("never-cached") == nil)
}
