import Foundation
import Security
import Testing
@testable import FaunaKit

#if os(macOS)
/// Pins convention 10 (`e2e-launch-isolation.md`) at the shared-keychain primitive: under an e2e
/// launch's environment no `SharedKeychainItem` operation reaches the system keychain. The harness
/// relocates `HOME`, so a `SecItem*` call there finds no login keychain and macOS raises a real
/// dialog on the box's unwatched console ("A keychain cannot be found to store
/// push_p256_private"). The mechanical reading is the primitive's own count of system calls,
/// never a look at the screen.
@Suite(.sharedKeychain)
struct SharedKeychainItemE2ETests {
    private static let service = "social.fauna.e2e-test.shared-item"
    private static let group = "group.social.fauna.e2e-test"

    @Test func e2eLaunchNeverReachesTheSystemKeychain() {
        setenv("FAUNA_E2E_BRIDGE", "1", 1)
        defer { unsetenv("FAUNA_E2E_BRIDGE") }
        #expect(SharedKeychainItem.e2eHeldInMemory)

        let before = SharedKeychainItem.systemKeychainCalls
        let data = Data("key-material".utf8)
        SharedKeychainItem.save(
            service: Self.service, account: "push_p256_private", accessGroup: Self.group,
            data: data)
        #expect(
            SharedKeychainItem.load(
                service: Self.service, account: "push_p256_private", accessGroup: Self.group)
                == data)
        // The FileProvider capability's flags take the same road.
        SharedKeychainItem.save(
            service: Self.service, account: "capability", accessGroup: Self.group, data: data,
            useDataProtectionKeychain: true)
        let probe = SharedKeychainItem.probeRoundTrip(
            service: Self.service, probeAccount: "__probe__", accessGroup: Self.group,
            useDataProtectionKeychain: true)
        #expect(probe.add == errSecSuccess && probe.read == errSecSuccess)
        SharedKeychainItem.delete(
            service: Self.service, account: "push_p256_private", accessGroup: Self.group)
        SharedKeychainItem.delete(
            service: Self.service, account: "capability", accessGroup: Self.group,
            useDataProtectionKeychain: true)
        #expect(
            SharedKeychainItem.load(
                service: Self.service, account: "push_p256_private", accessGroup: Self.group)
                == nil)

        #expect(SharedKeychainItem.systemKeychainCalls == before)
    }
}
#endif
