import FaunaFFISwift
import Foundation
import Testing

@testable import FaunaKit

/// Headless pins for the extension's **change-signer provisioning**
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records* (1),
/// *The capability host*): the app writes the machine principal's writer secret
/// and its `DeviceAuthorization` as two app-group Keychain items beside the
/// bearer, and the extension's provider re-reads them at every write. What the
/// Rust host does with the pair — the delegation-chain check, signing every
/// record, holding a write that finds none — is proven against a real nest in
/// `bins/fauna-nest/tests/conformance_file_provider_client.rs`; the Keychain
/// primitive itself is the one the bearer already rides.
@Suite struct FileProviderChangeSignerTests {
    private let secret = Data(repeating: 0x4B, count: 32)
    private let authorization = Data([0xA1, 0x02, 0x03])

    /// Both items make a signer; either one alone — a crash between the two
    /// writes — makes none, never a key paired with some other key's grant.
    @Test func theTwoItemsPairIntoOneSignerOrNone() {
        let paired = FileProviderCredentialStore.signerCarriage(
            writerSecret: secret, deviceAuthorization: authorization)
        #expect(paired?.writerSecret == secret)
        #expect(paired?.deviceAuthorization == authorization)

        #expect(
            FileProviderCredentialStore.signerCarriage(
                writerSecret: secret, deviceAuthorization: nil) == nil)
        #expect(
            FileProviderCredentialStore.signerCarriage(
                writerSecret: nil, deviceAuthorization: authorization) == nil)
        #expect(
            FileProviderCredentialStore.signerCarriage(
                writerSecret: Data(), deviceAuthorization: authorization) == nil)
    }

    /// The provider holds nothing: every call is a fresh read, so a principal
    /// the app re-provisioned — or cleared — is what the next write sees.
    @Test func theProviderReReadsOnEveryCall() {
        let stored = Stored()
        let provider = KeychainChangeSignerProvider(read: { stored.value })
        #expect(provider.currentSigner() == nil, "nothing provisioned yet")

        let first = FfiChangeSignerCarriage(writerSecret: secret, deviceAuthorization: authorization)
        stored.value = first
        #expect(provider.currentSigner() == first)

        let successor = FfiChangeSignerCarriage(
            writerSecret: Data(repeating: 0x5C, count: 32), deviceAuthorization: authorization)
        stored.value = successor
        #expect(provider.currentSigner() == successor, "a re-minted principal, no rebuild")

        stored.value = nil
        #expect(provider.currentSigner() == nil, "a cleared signer reads as none")
    }
}

/// The Keychain stand-in the provider reads through.
private final class Stored: @unchecked Sendable {
    private let lock = NSLock()
    private var carriage: FfiChangeSignerCarriage?

    var value: FfiChangeSignerCarriage? {
        get { lock.withLock { carriage } }
        set { lock.withLock { carriage = newValue } }
    }
}
