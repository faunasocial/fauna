import Testing
import Foundation
@testable import FaunaKit

// Deterministic upload-sidecar tests — the apple half of the cross-app
// blob-upload wire-up (`docs/goal/ui/media.md` § Encryption at rest). Mirrors
// windows `MediaUploadTests.cs`: the multipart wire shape, the audience map,
// and a real-FFI `processAndSealUpload` round-trip (the FaunaFFI dylib loads in
// the swift-test host, so the sidecar bytes are the genuine cross-language
// wire — same packer the nest's `blob_ingest_sidecar.rs` verifies).

// MARK: - Audience mapping

@Test func mapUploadAudiencePublicPost() {
    #expect(mapUploadAudience(.publicPost) == FfiUploadAudience.publicPost)
}

@Test func mapUploadAudienceLibraryCarriesBackupKey() {
    let key = Data(repeating: 7, count: 32)
    #expect(mapUploadAudience(.library(backupKey: key)) == FfiUploadAudience.library(backupKey: key))
}

// MARK: - Multipart wire shape

@Test func buildBlobMultipartHasSidecarAndBytesParts() {
    let sidecar = Data("SIDECAR-CBOR".utf8)
    let bytes = Data("SEALED-BYTES".utf8)
    let body = buildBlobMultipart(sidecarCbor: sidecar, sealedBytes: bytes, boundary: "BOUND")
    let text = String(decoding: body, as: UTF8.self)

    let expected =
        "--BOUND\r\n"
        + "Content-Disposition: form-data; name=\"sidecar\"\r\n"
        + "Content-Type: application/cbor\r\n\r\n"
        + "SIDECAR-CBOR\r\n"
        + "--BOUND\r\n"
        + "Content-Disposition: form-data; name=\"bytes\"\r\n"
        + "Content-Type: application/octet-stream\r\n\r\n"
        + "SEALED-BYTES\r\n"
        + "--BOUND--\r\n"
    #expect(text == expected)
}

@Test func buildBlobMultipartEmbedsBinaryPayloadsVerbatim() {
    // Sealed bytes are binary (not UTF-8); assert the parts carry them verbatim
    // by locating each payload as a sub-range of the assembled body.
    let sidecar = Data([0xa1, 0x00, 0xff, 0x7f])
    let bytes = Data([0x00, 0x01, 0x02, 0xfe, 0xff])
    let body = buildBlobMultipart(sidecarCbor: sidecar, sealedBytes: bytes, boundary: "B")
    #expect(body.range(of: sidecar) != nil)
    #expect(body.range(of: bytes) != nil)
}

// MARK: - Real-FFI round-trip (the genuine cross-language wire)

@Test func processAndSealUploadPublicPostPassesBytesThrough() throws {
    let raw = Data("not an image, just attachment bytes".utf8)
    let payload = try processAndSealUpload(raw: raw, audience: .publicPost)
    // PublicPost: signed plaintext, byte-identical (no seal); stub process_media
    // derives no thumbnail.
    #expect(payload.primary.bytes == raw)
    #expect(payload.thumbnail == nil)
    #expect(!payload.primary.sidecarCbor.isEmpty)
}

@Test func processAndSealUploadLibrarySealsUnderBackupKey() throws {
    let raw = Data("owner-only library media bytes".utf8)
    let key = Data(repeating: 7, count: 32)
    let payload = try processAndSealUpload(raw: raw, audience: .library(backupKey: key))
    // Sealed: an AEAD envelope, never byte-identical, carrying at least the
    // 12-byte nonce + 16-byte tag floor over the plaintext.
    #expect(payload.primary.bytes != raw)
    #expect(payload.primary.bytes.count >= raw.count + 28)
    #expect(payload.thumbnail == nil)
}

@Test func processAndSealUploadLibraryRejectsWrongLengthKey() {
    let key = Data(repeating: 0, count: 16) // not 32
    #expect(throws: (any Error).self) {
        _ = try processAndSealUpload(raw: Data("x".utf8), audience: .library(backupKey: key))
    }
}

// MARK: - End-to-end: a sealed payload flows into a valid multipart body

@Test func sealedLibraryPayloadAssemblesIntoMultipart() throws {
    let raw = Data("library item".utf8)
    let key = Data(repeating: 3, count: 32)
    let payload = try processAndSealUpload(raw: raw, audience: .library(backupKey: key))
    let body = buildBlobMultipart(sidecarCbor: payload.primary.sidecarCbor,
                                  sealedBytes: payload.primary.bytes,
                                  boundary: "X")
    // The assembled body carries both the genuine sidecar CBOR and the sealed
    // bytes verbatim — the bytes the nest's multipart parser will see.
    #expect(body.range(of: payload.primary.sidecarCbor) != nil)
    #expect(body.range(of: payload.primary.bytes) != nil)
}

// MARK: - Echo check (`security.md` § Signed-envelope shape, client-computed media hash)

/// The content hash the shared check computes over `bytes`, read off its own
/// mismatch error (`blob upload hash mismatch: nest echoed …, local <hash>`).
private func localHash(of bytes: Data) throws -> String {
    do {
        _ = try verifyBlobUploadEcho(echoed: "", bytes: bytes)
    } catch {
        let text = "\(error)"
        if let range = text.range(of: "local ") {
            return String(text[range.upperBound...].prefix(64))
        }
    }
    throw CocoaError(.coderReadCorrupt)
}

@Test func checkedBlobResponseReturnsTheLocallyComputedHash() throws {
    let bytes = Data("public attachment bytes".utf8)
    let local = try localHash(of: bytes)
    #expect(local.count == 64)
    let reply = Data("{\"hash\":\"\(local.uppercased())\"}".utf8)
    #expect(try checkedBlobResponse(reply: reply, primaryBytes: bytes).hash == local)
}

@Test func checkedBlobResponseRefusesAnEchoNamingAnotherBlob() {
    let bytes = Data("public attachment bytes".utf8)
    let other = String(repeating: "ab", count: 32)
    let reply = Data("{\"hash\":\"\(other)\"}".utf8)
    #expect(throws: (any Error).self) {
        _ = try checkedBlobResponse(reply: reply, primaryBytes: bytes)
    }
}

// MARK: - Echo check at the call site (`APIClient`'s primary blob POSTs)
//
// The pins above hold `checkedBlobResponse` on its own; these hold that
// `uploadBlob(data:audience:)` and `uploadPreparedBlob` route the primary
// POST's reply through it. The client is built on the internal
// `init(nodeUrl:session:)` seam with a `URLProtocol` stub answering the blob
// POST — the twin of windows' `DirectNestClient` handler seam and android's
// `ApiClientPublicUploadEchoTest` interceptor fake.

/// Answers every request to a registered host with that host's canned JSON
/// body. Keyed by host so parallel tests never read each other's reply.
private final class BlobEchoStub: URLProtocol {
    private static let lock = NSLock()
    nonisolated(unsafe) private static var replies: [String: Data] = [:]

    static func register(_ reply: Data) -> URL {
        let host = "blob-\(UUID().uuidString.lowercased()).stub.invalid"
        lock.lock(); replies[host] = reply; lock.unlock()
        return URL(string: "https://\(host)/")!
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let host = request.url?.host ?? ""
        Self.lock.lock(); let reply = Self.replies[host]; Self.lock.unlock()
        guard let reply, let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.cannotFindHost))
            return
        }
        let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1",
                                       headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: reply)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

/// An authed `APIClient` whose residual-HTTP leg answers every POST with
/// `{"hash": echoed}`.
private func stubbedClient(echoing echoed: String) -> APIClient {
    let config = URLSessionConfiguration.ephemeral
    config.protocolClasses = [BlobEchoStub.self]
    let url = BlobEchoStub.register(Data("{\"hash\":\"\(echoed)\"}".utf8))
    let api = APIClient(nodeUrl: url, session: URLSession(configuration: config))
    api.token = "stub-bearer"
    api.tokenExpiresAt = Date().addingTimeInterval(3600)
    return api
}

@Test func uploadBlobRefusesAnEchoNamingAnotherBlob() async {
    let api = stubbedClient(echoing: String(repeating: "ab", count: 32))
    await #expect(throws: (any Error).self) {
        _ = try await api.uploadBlob(data: Data("public attachment bytes".utf8),
                                     audience: .publicPost)
    }
}

@Test func uploadBlobReturnsTheLocallyComputedHash() async throws {
    let raw = Data("public attachment bytes".utf8)
    let primary = try processAndSealUpload(raw: raw, audience: .publicPost).primary.bytes
    let local = try localHash(of: primary)
    let api = stubbedClient(echoing: local.uppercased())
    #expect(try await api.uploadBlob(data: raw, audience: .publicPost).hash == local)
}

#if !FAUNA_KIDS
@Test func uploadPreparedBlobRefusesAnEchoNamingAnotherBlob() async {
    let api = stubbedClient(echoing: String(repeating: "cd", count: 32))
    let primary = ComposeUploadPart(sidecarCbor: Data([0xa0]), bytes: Data("prepared bytes".utf8))
    await #expect(throws: (any Error).self) {
        _ = try await api.uploadPreparedBlob(primary: primary, thumbnail: nil)
    }
}

@Test func uploadPreparedBlobReturnsTheLocallyComputedHash() async throws {
    let primary = ComposeUploadPart(sidecarCbor: Data([0xa0]), bytes: Data("prepared bytes".utf8))
    let local = try localHash(of: primary.bytes)
    let api = stubbedClient(echoing: local.uppercased())
    #expect(try await api.uploadPreparedBlob(primary: primary, thumbnail: nil) == local)
}
#endif
