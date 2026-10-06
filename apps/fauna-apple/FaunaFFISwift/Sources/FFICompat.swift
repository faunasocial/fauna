// FFICompat.swift — snake_case wrappers around UniFFI-generated camelCase API.
// FaunaKit was written against a hand-rolled FFI layer that used string hex params
// and returned JSON strings. These wrappers bridge to the typed UniFFI bindings.

import Foundation

// MARK: - Hex helpers
//
// The canonical `Data.hexString` / `Data(hexString:)` door — moved here
// (from FaunaKit's `HexEncoding.swift`) because `FaunaFFISwift` sits BELOW
// `FaunaKit` in the dependency graph (`Package.swift`) and can't import it
// the other way; `FaunaKit`'s `@_exported import FaunaFFISwift`
// (`FFIImport.swift`) keeps it visible to every existing FaunaKit caller
// unchanged. This file is the one hand-written survivor of the
// `apple-ffi-host` Sources wipe (justfile's `_apple-ffi-host-flavor`), so it
// — not a sibling file — is where hand-written Swift in this target lives.

public extension Data {
    var hexString: String {
        map { String(format: "%02x", $0) }.joined()
    }

    init?(hexString: String) {
        let len = hexString.count / 2
        var data = Data(capacity: len)
        var index = hexString.startIndex
        for _ in 0..<len {
            let nextIndex = hexString.index(index, offsetBy: 2)
            guard let byte = UInt8(hexString[index..<nextIndex], radix: 16) else { return nil }
            data.append(byte)
            index = nextIndex
        }
        self = data
    }
}

private func hexToData(_ hex: String) -> Data {
    Data(hexString: hex) ?? Data()
}

private func dataToHex(_ data: Data) -> String {
    data.hexString
}

/// Public wrappers around the same hex helpers used elsewhere in this
/// file. Exposed so callers (e.g. `APIClient.silentSignIn`) can build
/// signed payloads without re-implementing hex parsing locally.
public func hex_to_data(_ hex: String) -> Data {
    hexToData(hex)
}

public func data_to_hex(_ data: Data) -> String {
    dataToHex(data)
}

/// Sign arbitrary bytes with the given Ed25519 secret. Returns a
/// 64-byte signature. Snake_case wrapper around the UniFFI-generated
/// `signMessage` binding for `libs/fauna-ffi/src/auth.rs::sign_message`.
public func sign_message(_ secret: Data, _ msg: Data) throws -> Data {
    try signMessage(secret: secret, msg: msg)
}

// MARK: - Transport trust

/// Install the disk-backed nest-identity pin store at startup so TOFU pins
/// (self-signed / LAN nests) survive restarts. Snake_case wrapper around the
/// UniFFI-generated `installNestIdentityPinStore` binding for
/// `libs/fauna-ffi/src/trust.rs::install_nest_identity_pin_store`. `dataDir` is
/// the client's config dir; the canonical pin filename is appended inside Rust.
public func install_nest_identity_pin_store(_ dataDir: String) {
    installNestIdentityPinStore(dataDir: dataDir)
}

// MARK: - Identity

public func generate_keypair() -> String {
    dataToHex(generateKeypair())
}

public func actor_id_from_secret(_ secretHex: String) throws -> String {
    let data = hexToData(secretHex)
    return dataToHex(try actorIdFromSecret(secret: data))
}

public func generate_device_id() -> String {
    dataToHex(generateDeviceId())
}

// MARK: - Identity QR

public func identity_qr_encode(_ secretHex: String, handle: String? = nil) -> String {
    identityQrEncode(secretHex: secretHex, handle: handle)
}

public func identity_qr_decode(_ uri: String) throws -> String {
    try identityQrDecode(uri: uri)
}

public func build_register_request(_ secretHex: String, _ handle: String, _ domain: String) throws -> String {
    let secret = hexToData(secretHex)
    let req = try buildRegisterRequest(secret: secret, handle: handle, domain: domain)
    let dict: [String: Any] = [
        "actor_id": dataToHex(req.actorId),
        "handle": req.handle,
        "timestamp": req.timestamp,
        "signature": dataToHex(req.signature),
    ]
    let jsonData = try JSONSerialization.data(withJSONObject: dict)
    return String(data: jsonData, encoding: .utf8) ?? "{}"
}

// MARK: - Email

public func build_signed_email(_ secretHex: String, _ recipientId: String, _ subject: String, _ body: String, _ nodeUrl: String) throws -> [UInt8] {
    let secret = hexToData(secretHex)
    let to = hexToData(recipientId)
    let result = try buildSignedEmail(secret: secret, to: to, subject: subject, body: body, nodeUrl: nodeUrl)
    return Array(result)
}

public func decode_email(_ payload: [UInt8]) throws -> String {
    let data = Data(payload)
    return try decodeEmail(payload: data)
}

// MARK: - Posts / Feed

public func build_post(_ secret: Data, _ body: String) throws -> Data {
    try buildPost(secret: secret, body: body)
}

public func build_post_tagged(_ secret: Data, _ body: String, _ tags: [String]) throws -> Data {
    try buildPostTagged(secret: secret, body: body, tags: tags)
}

public func build_post_reply(_ secret: Data, _ body: String, _ replyToHex: String) throws -> Data {
    try buildPostReply(secret: secret, body: body, replyTo: hexToData(replyToHex))
}

public func decode_post_full(_ data: Data) throws -> DecodedPost {
    try decodePostFull(data: data)
}

// MARK: - Chunking / Sync

public func ffi_content_hash(_ data: [UInt8]) -> String {
    let result = ffiContentHash(data: Data(data))
    return dataToHex(result)
}

public func content_hash_at_path(_ path: String) throws -> String {
    let result = try contentHashAtPath(path: path)
    return dataToHex(result)
}

public func chunk_file(_ data: [UInt8]) -> String {
    let manifest = chunkFile(data: Data(data))
    return manifestToJson(manifest, chunkDir: nil)
}

public func chunk_file_at_path(_ path: String) throws -> String {
    let result = try chunkFileAtPath(path: path)
    return manifestToJson(result.manifest, chunkDir: result.chunkDir)
}

public func extract_chunks(_ data: [UInt8], _ manifestJson: String) -> String {
    guard let manifest = manifestFromJson(manifestJson) else { return "[]" }
    guard let chunks = try? extractChunks(data: Data(data), manifest: manifest) else { return "[]" }
    return chunkItemsToJson(chunks)
}

public func serialize_manifest(_ manifestJson: String) -> [UInt8] {
    guard let manifest = manifestFromJson(manifestJson),
          let data = try? serializeManifest(manifest: manifest) else { return [] }
    return Array(data)
}

public func deserialize_manifest(_ data: [UInt8]) throws -> String {
    let manifest = try deserializeManifest(data: Data(data))
    return manifestToJson(manifest, chunkDir: nil)
}

public func reassemble_chunks_to_path(_ chunkDir: String, _ manifestJson: String, _ outputPath: String) throws {
    guard let manifest = manifestFromJson(manifestJson) else {
        throw FFICompatError.invalidManifest
    }
    try reassembleChunksToPath(chunkDir: chunkDir, manifest: manifest, outputPath: outputPath)
}

// (Domain search removed: the nest never had a verifier or route for it — the
// whole client-side chain was dead code, deleted in the sweep.)

// MARK: - MLS
//
// No `mls_*` wrappers here any more. The standalone `libs/fauna-ffi/src/mls.rs`
// singleton engine plane was DELETED 2026-07-22 once apple's `MlsManager` — its
// last production caller anywhere — went away (2026-07-19). Every app's MLS is
// the conversations rail's per-session engine, built by `build_conversations_session`
// (`libs/fauna-ffi/src/nest_client.rs`); nothing may re-derive a standalone-engine
// path. Owner: `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync.

// MARK: - Internal helpers

public enum FFICompatError: Error {
    case invalidManifest
}

private func manifestToJson(_ m: FfiManifest, chunkDir: String?) -> String {
    var dict: [String: Any] = [
        "file_hash": dataToHex(m.fileHash),
        "chunk_hashes": m.chunkHashes.map { dataToHex($0) },
        "chunk_sizes": m.chunkSizes.map { Int($0) },
        "total_size": m.totalSize,
    ]
    // Sealed content addresses its chunks by CIPHERTEXT hash; absent means the
    // chunks are stored under their plaintext hash. Dropping this on the JSON
    // round-trip would make a sealed manifest reassemble against the wrong
    // storage keys (`fauna-core::chunk` § storage_key).
    if let stored = m.storedHashes { dict["stored_hashes"] = stored.map { dataToHex($0) } }
    if let dir = chunkDir { dict["chunk_dir"] = dir }
    guard let data = try? JSONSerialization.data(withJSONObject: dict) else { return "{}" }
    return String(data: data, encoding: .utf8) ?? "{}"
}

private func chunkItemsToJson(_ items: [FfiChunkItem]) -> String {
    let arr = items.map { item -> [String: Any] in
        [
            "hash": dataToHex(item.hash),
            "data": dataToHex(item.data),
        ]
    }
    guard let data = try? JSONSerialization.data(withJSONObject: arr) else { return "[]" }
    return String(data: data, encoding: .utf8) ?? "[]"
}

private func manifestFromJson(_ json: String) -> FfiManifest? {
    guard let data = json.data(using: .utf8),
          let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
    let fileHash = hexToData(dict["file_hash"] as? String ?? "")
    let chunkHashes = (dict["chunk_hashes"] as? [String] ?? []).map { hexToData($0) }
    let chunkSizes = (dict["chunk_sizes"] as? [Int] ?? []).map { UInt64($0) }
    let totalSize = dict["total_size"] as? UInt64 ?? 0
    let storedHashes = (dict["stored_hashes"] as? [String])?.map { hexToData($0) }
    return FfiManifest(fileHash: fileHash, totalSize: totalSize, chunkHashes: chunkHashes,
                       chunkSizes: chunkSizes, storedHashes: storedHashes)
}
