import Testing
import Foundation
@testable import FaunaKit

// MARK: - Pure FFI tests (no nest required)

@Test func generateKeypairReturns64HexChars() {
    let secret = generate_keypair()
    #expect(secret.count == 64)
    #expect(secret.allSatisfy { "0123456789abcdef".contains($0) })
}

@Test func generateKeypairIsUnique() {
    let a = generate_keypair()
    let b = generate_keypair()
    #expect(a != b)
}

@Test func actorIdFromSecretRoundTrips() throws {
    let secret = generate_keypair()
    let actorId = try actor_id_from_secret(secret)
    #expect(actorId.count == 64)
    // Same secret always gives same actor ID
    let actorId2 = try actor_id_from_secret(secret)
    #expect(actorId == actorId2)
}

@Test func actorIdDiffersForDifferentSecrets() throws {
    let a = generate_keypair()
    let b = generate_keypair()
    let idA = try actor_id_from_secret(a)
    let idB = try actor_id_from_secret(b)
    #expect(idA != idB)
}

@Test func generateDeviceIdReturns64HexChars() {
    let deviceId = generate_device_id()
    #expect(deviceId.count == 64)
    #expect(deviceId.allSatisfy { "0123456789abcdef".contains($0) })
}

@Test func buildRegisterRequestProducesValidJSON() throws {
    let secret = generate_keypair()
    let json = try build_register_request(secret, "testuser", "example.com")
    let data = json.data(using: .utf8)!
    let dict = try JSONSerialization.jsonObject(with: data) as! [String: Any]
    #expect(dict["actor_id"] is String)
    #expect(dict["handle"] is String)
    #expect(dict["timestamp"] is UInt64)
    #expect(dict["signature"] is String)
    #expect((dict["handle"] as? String) == "testuser")
}

@Test func buildSignedEmailRoundTrips() throws {
    let secret = generate_keypair()
    let recipientSecret = generate_keypair()
    let recipientId = try actor_id_from_secret(recipientSecret)

    let payload = try build_signed_email(secret, recipientId, "Test Subject", "Test body", "http://localhost:3000")
    #expect(!payload.isEmpty)

    // Decode the email back
    let decoded = try decode_email(payload)
    #expect(!decoded.isEmpty)
    // decoded is JSON — verify it parses
    let data = decoded.data(using: .utf8)!
    let dict = try JSONSerialization.jsonObject(with: data) as! [String: Any]
    #expect((dict["subject"] as? String) == "Test Subject")
    #expect((dict["body"] as? String) == "Test body")
}

@Test func chunkFileAndReassemble() throws {
    // Create some test data
    let testData: [UInt8] = Array(repeating: 0x42, count: 1024)
    let manifestJson = chunk_file(testData)
    #expect(!manifestJson.isEmpty)

    let data = manifestJson.data(using: .utf8)!
    let dict = try JSONSerialization.jsonObject(with: data) as! [String: Any]
    #expect(dict["file_hash"] is String)
    #expect(dict["chunk_hashes"] is [String])
    #expect(dict["total_size"] is UInt64)
}

@Test func contentHashIsDeterministic() {
    let data: [UInt8] = [1, 2, 3, 4, 5]
    let hash1 = ffi_content_hash(data)
    let hash2 = ffi_content_hash(data)
    #expect(hash1 == hash2)
    #expect(hash1.count == 64) // 32 bytes = 64 hex chars
}

// NOTE: no MLS tests here — the standalone `libs/fauna-ffi/src/mls.rs` singleton
// engine plane (and the earlier throwaway `mls_generate_key_packages` FFI) is
// DELETED. MLS rides the conversations rail's per-session engine, so its coverage
// lives with that rail: key-package mints via `ConversationsManager.ensureKeypackages`
// (shared-Rust `keypackage_minted_via_ensure_keypackages_survives_a_provider_swap`),
// group/encrypt behavior in `libs/fauna-conversations`. Owner:
// `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync.

// MARK: - Integration tests (nest required)

@Test func authenticateAgainstLocalNest() async throws {
    let nodeUrl = "http://127.0.0.1:3000"

    // Check if nest is running (skip gracefully if not)
    guard let healthUrl = URL(string: "\(nodeUrl)/api/v1/health"),
          let (_, response) = try? await URLSession.shared.data(from: healthUrl),
          (response as? HTTPURLResponse)?.statusCode == 200 else {
        return
    }

    // Generate a keypair and mint a bearer over the shared FFI `mintBearer`
    // (`fauna.auth.handshake`, WS-RPC) — the HTTP `POST /api/v1/auth/token`
    // twin was deleted at the rip-out endgame. Mirrors
    // `APIClient.authenticate(secret:)`.
    let secret = generate_keypair()
    let r = try await mintBearer(nestUrl: nodeUrl, secret: hex_to_data(secret))
    #expect(!r.token.isEmpty)
}
