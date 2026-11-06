import Foundation

// Compiled out of release artifacts (testing.md convention 15): both UniFFI
// exports it drives (`custodyOwnerResendNow`, `custodyCeremonyIngestJson`) exist
// only in the `test-helpers` flavor.
#if DEBUG

/// Shared handler for the cross-app `custody_owner_resend_now` TestAgent command
/// (`fauna_e2e_agent::CUSTODY_OWNER_RESEND_NOW`) — the owner's send-again poke
/// under `devices` outcome 15 ("a removed or declined custody offer stays gone
/// when its owner sends again"). The witness reads the HOST's
/// `custody_ceremony_ingest` state value
/// (`AppStateObservables.commonState`), never this ack.
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation,
/// mirroring `CustodianPullTestCommand`. Fire-and-forget like `account_pump_now`:
/// awaiting would put a whole network pass on the agent's dispatch path.
public enum CustodyOwnerResendTestCommand {
    @MainActor
    public static func apply(client: FaunaClient?) {
        guard let api = client?.api else {
            logMessage(
                level: .info, target: "fauna.app",
                message: "[e2e] custody_owner_resend_now: no client yet (pre-auth)")
            return
        }
        Task {
            if await api.custodyOwnerResendNow() == false {
                logMessage(
                    level: .info, target: "fauna.app",
                    message: "[e2e] custody_owner_resend_now: no custody runtime to poke")
            }
        }
    }
}

#endif
