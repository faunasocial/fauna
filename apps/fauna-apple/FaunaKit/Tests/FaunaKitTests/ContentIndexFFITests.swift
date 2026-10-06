import Testing
import Foundation
@testable import FaunaKit

// MARK: - ContentIndex FFI smoke test (no nest required)

@Test func indexHandleRoundTrip() throws {
    let handle = try IndexHandle.createInRam()

    let doc = IndexedDoc(
        kind: .mail,
        contentId: Data(repeating: 0xAA, count: 4),
        timestampNs: 1_000,
        senderActorId: nil,
        secondaryId: nil,
        fields: [IndexedField(kind: .body, text: "hello from swift")]
    )
    try handle.addDoc(doc: doc)
    try handle.commit()

    let hits = try handle.query(
        query: "swift",
        kinds: [.mail],
        range: nil,
        limit: 10
    )
    #expect(hits.count == 1)
    #expect(hits[0].kind == .mail)
    #expect(hits[0].contentId == Data(repeating: 0xAA, count: 4))
}

@Test func indexHandleEmptyQueryYieldsNoHits() throws {
    let handle = try IndexHandle.createInRam()
    let doc = IndexedDoc(
        kind: .post,
        contentId: Data([1, 2, 3]),
        timestampNs: 0,
        senderActorId: nil,
        secondaryId: nil,
        fields: [IndexedField(kind: .body, text: "anything")]
    )
    try handle.addDoc(doc: doc)
    try handle.commit()

    let hits = try handle.query(query: "", kinds: [.post], range: nil, limit: 10)
    #expect(hits.isEmpty)
}
