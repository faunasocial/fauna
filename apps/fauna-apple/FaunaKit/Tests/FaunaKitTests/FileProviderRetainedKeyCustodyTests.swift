import FaunaFFISwift
import Foundation
import Testing

@testable import FaunaKit

/// Headless pins for the extension's **retained generation-key custody**
/// (decision 1′ of `on-demand-files-shared-sets.md`): the app mirrors the machine
/// slot's keys into one app-group Keychain item and the extension reads and
/// records through `KeychainRetainedKeyCustody`. The Keychain primitive is the
/// one the bearer rides; the FFI-level proof of the read itself is the Rust
/// conformance test.
@Suite struct FileProviderRetainedKeyCustodyTests {
    private let genA = Data(repeating: 0x01, count: 32)
    private let genB = Data(repeating: 0x02, count: 32)
    private let keyA = Data(repeating: 0xA1, count: 32)
    private let keyB = Data(repeating: 0xB2, count: 32)

    @Test func theItemRoundTripsAndMalformedBytesReadAsNone() {
        let map = [genA: keyA, genB: keyB]
        #expect(KeychainRetainedKeyCustody.decode(KeychainRetainedKeyCustody.encode(map)) == map)
        #expect(KeychainRetainedKeyCustody.decode(nil).isEmpty)
        #expect(KeychainRetainedKeyCustody.decode(Data([32, 1, 2])).isEmpty, "truncated")
        #expect(KeychainRetainedKeyCustody.decode(Data([0, 0])).isEmpty, "zero-length field")
    }

    @Test func putGetRemoveThroughTheItem() {
        let item = Item()
        let custody = item.custody()
        #expect(custody.get(generation: genA) == nil)

        custody.put(generation: genA, key: keyA)
        #expect(custody.get(generation: genA) == keyA)

        custody.put(generation: genA, key: keyB)
        #expect(custody.get(generation: genA) == keyA, "a held generation keeps its key")

        custody.remove(generation: genA)
        #expect(custody.get(generation: genA) == nil)
        #expect(item.bytes == nil, "the last removal clears the item")
    }

    /// The app's mirror and the extension's own puts share the one item: the
    /// mirror adds, never drops what the extension recorded.
    @Test func theMirrorAddsBesideTheExtensionsOwnPuts() {
        let item = Item()
        let custody = item.custody()
        custody.put(generation: genB, key: keyB)
        custody.record([
            FfiRetainedGenerationKey(generation: genA, key: keyA),
            FfiRetainedGenerationKey(generation: genB, key: keyB),
        ])
        #expect(custody.get(generation: genA) == keyA)
        #expect(custody.get(generation: genB) == keyB)
        #expect(
            KeychainRetainedKeyCustody.decode(item.bytes) == [genA: keyA, genB: keyB])
    }

    @Test func aFreshReadSeesAnotherWritersItem() {
        let item = Item()
        let reader = item.custody()
        #expect(reader.get(generation: genA) == nil)
        item.custody().put(generation: genA, key: keyA)
        #expect(reader.get(generation: genA) == keyA, "no cache between calls")
    }
}

/// The Keychain stand-in the custody reads and writes through.
private final class Item: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Data?

    var bytes: Data? { lock.withLock { stored } }

    private func set(_ data: Data?) { lock.withLock { stored = data } }

    func custody() -> KeychainRetainedKeyCustody {
        KeychainRetainedKeyCustody(
            read: { [self] in bytes },
            write: { [self] data in set(data) })
    }
}
