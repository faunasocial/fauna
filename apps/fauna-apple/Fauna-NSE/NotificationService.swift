#if os(iOS)
import UserNotifications
import CryptoKit
import Security
import Foundation

// MARK: - NSE Error

enum NSEError: Error {
    case missingEncryptedPayload
    case invalidHeader
    case keychainLoadFailed
    case invalidSenderKey
    case decryptionFailed
    case invalidPayload
}

// MARK: - Notification Service Extension

class NotificationService: UNNotificationServiceExtension {
    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var bestAttemptContent: UNMutableNotificationContent?

    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        self.contentHandler = contentHandler
        bestAttemptContent = request.content.mutableCopy() as? UNMutableNotificationContent

        guard let bestAttemptContent else {
            contentHandler(request.content)
            return
        }

        guard let encryptedB64 = request.content.userInfo["encrypted_payload"] as? String,
              let encryptedData = base64URLDecode(encryptedB64) else {
            // No encrypted payload — deliver as-is
            contentHandler(bestAttemptContent)
            return
        }

        do {
            let plaintext = try decryptPayload(encryptedData)

            if let json = try JSONSerialization.jsonObject(with: plaintext) as? [String: Any] {
                if let title = json["title"] as? String {
                    bestAttemptContent.title = title
                }
                if let body = json["body"] as? String {
                    bestAttemptContent.body = body
                }
                if let url = json["url"] as? String {
                    bestAttemptContent.userInfo["url"] = url
                }
            }
        } catch {
            // Decryption failed — deliver with generic content
            bestAttemptContent.title = "New message"
            bestAttemptContent.body = "You have a new notification."
        }

        contentHandler(bestAttemptContent)
    }

    override func serviceExtensionTimeWillExpire() {
        if let contentHandler, let bestAttemptContent {
            bestAttemptContent.title = "New message"
            bestAttemptContent.body = "You have a new notification."
            contentHandler(bestAttemptContent)
        }
    }

    // MARK: - RFC 8291 aes128gcm Decryption

    /// Decrypt an RFC 8291 (Web Push) aes128gcm encrypted payload.
    ///
    /// Binary format:
    /// ```
    /// salt(16) || rs(4) || idlen(1) || sender_pub(idlen) || ciphertext(...)
    /// ```
    private func decryptPayload(_ data: Data) throws -> Data {
        // Parse header
        guard data.count >= 21 else { throw NSEError.invalidHeader }

        let salt = data[0..<16]
        // rs is data[16..<20] — record size, not needed for single-record payloads
        let idlen = Int(data[20])
        let headerSize = 21 + idlen

        guard data.count > headerSize else { throw NSEError.invalidHeader }
        guard idlen == 65 else { throw NSEError.invalidSenderKey } // uncompressed P-256 point

        let senderPubBytes = data[21..<headerSize]
        let ciphertext = data[headerSize...]

        // Load our private key and auth secret from shared keychain
        guard let privateKeyData = loadFromSharedKeychain(account: "push_p256_private"),
              let authSecret = loadFromSharedKeychain(account: "push_auth_secret") else {
            throw NSEError.keychainLoadFailed
        }

        let privateKey = try P256.KeyAgreement.PrivateKey(rawRepresentation: privateKeyData)
        let senderPubKey = try P256.KeyAgreement.PublicKey(x963Representation: senderPubBytes)

        // ECDH shared secret
        let sharedSecret = try privateKey.sharedSecretFromKeyAgreement(with: senderPubKey)

        // Receiver public key (our public key) in uncompressed form
        let receiverPub = privateKey.publicKey.x963Representation

        // Key derivation per RFC 8291 Section 3.4
        //
        // Combined HKDF:
        //   PRK_key = HKDF-Extract(salt=auth_secret, IKM=ecdh_secret)
        //   IKM     = HKDF-Expand(PRK_key, key_info, 32)
        //
        // CryptoKit's hkdfDerivedSymmetricKey does Extract+Expand in one call.
        var keyInfoData = Data("WebPush: info\0".utf8)
        keyInfoData.append(receiverPub)
        keyInfoData.append(contentsOf: senderPubBytes)

        let ikmKey = sharedSecret.hkdfDerivedSymmetricKey(
            using: SHA256.self,
            salt: authSecret,
            sharedInfo: keyInfoData,
            outputByteCount: 32
        )
        let ikm = ikmKey.withUnsafeBytes { Data($0) }

        // PRK = HKDF-Extract(salt=header_salt, IKM=ikm)
        let prk = HKDF<SHA256>.extract(inputKeyMaterial: SymmetricKey(data: ikm), salt: salt)

        // CEK = HKDF-Expand(PRK, "Content-Encoding: aes128gcm\0", 16)
        //
        // ⚠ `HKDF.extract` answers a `HashedAuthenticationCode<SHA256>` — the raw
        // 32 PRK bytes — not a `SymmetricKey`, so the wrapper below is required and
        // is NOT ceremony. It was dropped (2026-03-30), and because
        // this whole file is `#if os(iOS)` and every apple compile on the box targets
        // the macOS HOST, the file compiled to NOTHING for four and a half months and
        // no gate said a word (`apple-ios-typecheck` now covers it).
        let cekInfo = Data("Content-Encoding: aes128gcm\0".utf8)
        let cek = expandHKDF(prk: SymmetricKey(data: prk), info: cekInfo, length: 16)

        // Nonce = HKDF-Expand(PRK, "Content-Encoding: nonce\0", 12)
        let nonceInfo = Data("Content-Encoding: nonce\0".utf8)
        let nonce = expandHKDF(prk: SymmetricKey(data: prk), info: nonceInfo, length: 12)

        // AES-128-GCM decrypt
        let sealedBox = try AES.GCM.SealedBox(
            nonce: AES.GCM.Nonce(data: nonce),
            ciphertext: ciphertext.dropLast(16),
            tag: ciphertext.suffix(16)
        )
        var plaintext = try AES.GCM.open(sealedBox, using: SymmetricKey(data: cek))

        // Remove trailing 0x02 record delimiter (and any padding zeroes)
        if let delimiterIndex = plaintext.lastIndex(of: 0x02) {
            plaintext = plaintext[plaintext.startIndex..<delimiterIndex]
        }

        return plaintext
    }

    /// HKDF-Expand using SHA-256.
    private func expandHKDF(prk: SymmetricKey, info: Data, length: Int) -> Data {
        let derived = HKDF<SHA256>.expand(
            pseudoRandomKey: prk,
            info: info,
            outputByteCount: length
        )
        return derived.withUnsafeBytes { Data($0) }
    }

    // MARK: - Shared Keychain

    // These MUST equal `FaunaKit.AppleIdentifiers.appGroup` /
    // `.KeychainService.push` — they are the same keychain items `PushManager`
    // writes. They are repeated rather than imported because `FaunaNSE` is a
    // deliberately dependency-free SPM target (a notification service extension
    // gets ~30s and must not drag FaunaKit + the FFI xcframework in). The
    // equality is enforced instead by
    // `tests/e2e-unified/tests/test_apple_identifier_pins.py`.
    private static let accessGroup = "group.social.fauna.shared"
    private static let keychainService = "social.fauna.push"

    private func loadFromSharedKeychain(account: String) -> Data? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.keychainService,
            kSecAttrAccount as String: account,
            kSecAttrAccessGroup as String: Self.accessGroup,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess, let data = result as? Data else { return nil }
        return data
    }

    // MARK: - Base64URL

    private func base64URLDecode(_ string: String) -> Data? {
        var base64 = string
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = base64.count % 4
        if remainder > 0 {
            base64 += String(repeating: "=", count: 4 - remainder)
        }
        return Data(base64Encoded: base64)
    }
}
#endif
