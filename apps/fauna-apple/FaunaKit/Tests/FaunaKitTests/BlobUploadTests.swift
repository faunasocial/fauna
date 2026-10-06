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
