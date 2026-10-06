import Foundation

#if DEBUG
/// The registry half of the cross-app E2E bridge on macOS + iOS
/// (`onboarding.md` § E2E bridge contract). The name table and the semantics
/// are shared Rust (`fauna_client_accounts::call_registry_method_for_test`,
/// reached through the `test-helpers` export
/// `FfiAccountRegistry.callRegistryMethodForTest`); apple contributes only its
/// registry. Each target's `callMachineMethod` tries this before the machine's
/// dispatcher, exactly as tui's automation does.
///
/// A fresh registry per call is fine: a registry is a stateless view over the
/// keychain, and the one stateful arm — `refuse_secret_writes_for_test` — keeps
/// its fault in the store's backing, which every view shares.
///
/// `#if DEBUG` because the export exists only in the test FFI flavor's bindings
/// (convention 15; pinned by `test_apple_seam_gating.py`).
@MainActor
public enum RegistryTestBridge {
    /// `.handled(resultJson:)` when `name` is a registry method — the reader's
    /// JSON, or `nil` for a setter — else `.notMine`.
    public static func call(name: String, jsonArg: String) -> FfiRegistryMethodOutcome {
        FaunaAccounts.registry().callRegistryMethodForTest(name: name, jsonArg: jsonArg)
    }
}
#endif
