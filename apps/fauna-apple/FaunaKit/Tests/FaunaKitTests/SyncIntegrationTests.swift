import Testing
import Foundation
@testable import FaunaKit

/// Integration tests for the sync/chunk upload path.
/// Requires a local nest running on port 3000 with --no-require-registration.

private let nestUrl = "http://127.0.0.1:3000"

private func nestIsRunning() async -> Bool {
    guard let url = URL(string: "\(nestUrl)/api/v1/health"),
          let (_, response) = try? await URLSession.shared.data(from: url),
          (response as? HTTPURLResponse)?.statusCode == 200 else {
        return false
    }
    return true
}

private func authenticate(secret: String) async throws -> String {
    // Mint over the shared FFI `mintBearer` (`fauna.auth.handshake`, WS-RPC) —
    // the HTTP `POST /api/v1/auth/token` twin was deleted at the rip-out
    // endgame. Mirrors `APIClient.authenticate(secret:)`.
    let r = try await mintBearer(nestUrl: nestUrl, secret: hex_to_data(secret))
    return r.token
}

private enum SyncTestError: Error {
    case blobUploadFailed(Int, String)
}

// MARK: - Tests

@Test func chunkUploadAndDownloadBlob() async throws {
    guard await nestIsRunning() else { return }

    let secret = generate_keypair()
    let token = try await authenticate(secret: secret)

    // Chunk test data
    let testContent = "Hello from sync integration test! \(UUID().uuidString)"
    let testData = Array(testContent.data(using: .utf8)!)

    let manifestJson = chunk_file(testData)
    let manifest = try JSONSerialization.jsonObject(
        with: manifestJson.data(using: .utf8)!) as! [String: Any]

    let chunkHashes = manifest["chunk_hashes"] as! [String]
    let fileHash = manifest["file_hash"] as! String
    #expect(!chunkHashes.isEmpty)
    #expect(!fileHash.isEmpty)

    // Extract chunks
    let chunksJson = extract_chunks(testData, manifestJson)
    let chunks = try JSONSerialization.jsonObject(
        with: chunksJson.data(using: .utf8)!) as! [[String: Any]]
    #expect(chunks.count == chunkHashes.count)

    // Upload each chunk as a blob
    for chunk in chunks {
        let hash = chunk["hash"] as! String
        let hexData = chunk["data"] as! String
        let chunkBytes = hexToData(hexData)

        var req = URLRequest(url: URL(string: "\(nestUrl)/api/v1/blob")!)
        req.httpMethod = "POST"
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        req.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        req.setValue(hash, forHTTPHeaderField: "X-Fauna-Hash")
        req.httpBody = chunkBytes

        let (respData, resp) = try await URLSession.shared.data(for: req)
        let status = (resp as? HTTPURLResponse)?.statusCode ?? 0
        if status != 200 && status != 201 {
            throw SyncTestError.blobUploadFailed(status, String(data: respData, encoding: .utf8) ?? "")
        }
    }

    // Download and verify each chunk
    for chunk in chunks {
        let hash = chunk["hash"] as! String
        let hexData = chunk["data"] as! String
        let expectedBytes = hexToData(hexData)

        var req = URLRequest(url: URL(string: "\(nestUrl)/api/v1/blob/\(hash)")!)
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")

        let (downloadedData, resp) = try await URLSession.shared.data(for: req)
        let status = (resp as? HTTPURLResponse)?.statusCode ?? 0
        #expect(status == 200, "Blob download failed with \(status)")
        #expect(downloadedData == expectedBytes, "Downloaded chunk doesn't match")
    }
}

@Test func contentHashMatchesBetweenChunkAndDirect() {
    let data: [UInt8] = Array("deterministic test content".data(using: .utf8)!)
    let hash = ffi_content_hash(data)
    let manifestJson = chunk_file(data)
    let manifest = try! JSONSerialization.jsonObject(
        with: manifestJson.data(using: .utf8)!) as! [String: Any]
    let fileHash = manifest["file_hash"] as! String
    #expect(hash == fileHash)
}

@Test func serializeAndDeserializeManifest() throws {
    let data: [UInt8] = Array(repeating: 0xAB, count: 2048)
    let manifestJson = chunk_file(data)

    // Serialize to bare bytes
    let bareBytes = serialize_manifest(manifestJson)
    #expect(!bareBytes.isEmpty)

    // Deserialize back to JSON
    let restored = try deserialize_manifest(bareBytes)
    let original = try JSONSerialization.jsonObject(with: manifestJson.data(using: .utf8)!) as! [String: Any]
    let restoredDict = try JSONSerialization.jsonObject(with: restored.data(using: .utf8)!) as! [String: Any]

    #expect((original["file_hash"] as? String) == (restoredDict["file_hash"] as? String))
    #expect((original["chunk_hashes"] as? [String]) == (restoredDict["chunk_hashes"] as? [String]))
}

private func hexToData(_ hex: String) -> Data {
    var data = Data()
    var idx = hex.startIndex
    while idx < hex.endIndex {
        let next = hex.index(idx, offsetBy: 2, limitedBy: hex.endIndex) ?? hex.endIndex
        if let byte = UInt8(hex[idx..<next], radix: 16) {
            data.append(byte)
        }
        idx = next
    }
    return data
}
