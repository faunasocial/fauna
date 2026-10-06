import Foundation
import LocalAuthentication

/// Stage-2 activation re-auth (`long-term-store.md` § Multi-account evolution,
/// ratified 2026-07-16): the platform's **native** re-auth prompt — `LAContext`
/// with `.deviceOwnerAuthentication` (biometric, falling back to the device
/// passcode / login password). Deliberately no in-app confirm dialog: the OS
/// sheet is the surface, so no ui.yaml element renders on apple
/// (`account-activate-reauth-prompt` is reserved for platforms without a
/// native prompt).
///
/// **Fail-closed.** A device where the policy cannot evaluate (no passcode
/// set) declines the activation rather than waving the gate through. That is
/// recoverable from the client UI: the flag's toggle
/// (`account-require-confirm-toggle`) sits on the CURRENT session's switcher
/// row, and *setting* the flag never demands re-auth — only activation does —
/// so the user can turn it off and switch.
///
/// **E2E seam — `#if DEBUG` ONLY, and that gate is load-bearing.** A real OS
/// prompt would hang a headless run, so in a DEBUG build, when the harness sets
/// `FAUNA_E2E_CREDENTIAL_DIR` (the same env the Keychain file backend seeds
/// from), the verdict is read from `<dir>/reauth-result` — literal `approve`
/// confirms; anything else, or no file, declines (fail-closed here too). The file
/// is read per prompt, so one app session can exercise both the approve and the
/// decline paths without a relaunch.
///
/// This branch **stands in for `LAContext.deviceOwnerAuthentication`**: ungated,
/// a Release build would let whoever controls the launch environment drop a file
/// on disk and walk through the device-owner re-auth that guards activating
/// another account. That is verbatim the windows defect — `FAUNA_E2E_CREDENTIAL_DIR`
/// in `AccountReauth.ConfirmActivationAsync` standing in for the Windows Hello
/// verification — which `e2e-automation-surface-gating.md` § Implementation status
/// today names as one of "the three worst", gated there 2026-08-10. Convention 15's
/// compile-time boundary is the gate; `E2eEnv` is only the inner switch.
enum AccountReauth {
    static func confirmActivation() async -> Bool {
        #if DEBUG
        if let dir = E2eEnv.credentialDir {
            let url = URL(fileURLWithPath: dir).appendingPathComponent("reauth-result")
            let verdict = (try? String(contentsOf: url, encoding: .utf8))?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let approved = verdict == "approve"
            logMessage(level: .info, target: "fauna.accounts",
                       message: "[account-switch] e2e reauth verdict: \(verdict ?? "<absent>") → \(approved ? "approve" : "decline")")
            return approved
        }
        #endif

        let ctx = LAContext()
        var unavailable: NSError?
        guard ctx.canEvaluatePolicy(.deviceOwnerAuthentication, error: &unavailable) else {
            logMessage(level: .warn, target: "fauna.accounts",
                       message: "[account-switch] re-auth unavailable (\(unavailable?.localizedDescription ?? "unknown")) — declining, fail-closed")
            return false
        }
        do {
            return try await ctx.evaluatePolicy(.deviceOwnerAuthentication,
                                                localizedReason: L.settings.accountPage.reauthReason)
        } catch {
            // User cancel, biometry lockout, policy failure — every non-success
            // reads as a decline; the switch is a pure no-op either way.
            return false
        }
    }
}
