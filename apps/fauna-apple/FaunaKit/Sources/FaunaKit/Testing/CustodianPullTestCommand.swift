import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it. Also required
// for correctness: it drives `FaunaClient.custodianPassForTest`, itself
// `#if DEBUG`, over a UniFFI call (`custodianRunPassNow`) that only the
// `test-helpers` flavor exports.
#if DEBUG

/// What one poked custodian pass did, on whichever side this platform hosts its
/// replica — the same split `CustodianStoreAccess` makes for the store itself.
public enum CustodianPassOutcome: Equatable, Sendable {
    /// A desktop shell (macOS): the external `fauna-sync-agent` ran the pass, and
    /// answered its own six-field report.
    case agent(FfiCustodianPassReport)
    /// A shell that hosts in the app process (iOS): this process ran the pass
    /// through the shared host's test-only `runAllKindsAtOffset`, which answers
    /// the same six-field report. `nil` is **not an enrolled custodian** —
    /// `build_custodian_host` answered `None`, which is the ordinary state until
    /// the enrollment reaches the registry, never an error.
    case inApp(FfiCustodianPassReport?)
}

/// Shared handler for the cross-app `custodian_pull_run_now` TestAgent command —
/// the causal barrier under `test_backups.py`'s enroll → pull → check-in → status
/// proof and its orphaned-store witness (`ui/backups.md` § Manage backup
/// destinations → *Reclaim this device's copy*).
///
/// Runs **one** real custodian pull pass and reports what it did as
/// `state.machine_method_result` — the slot `HttpBridgeDriver.call_command` reads
/// back — so a caller asserts state, never timing (convention 14). Without the
/// poke the first production pass is `PERIODIC_INTERVAL` (15 min) away.
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation,
/// mirroring `BackupAuditTestCommand`. The platform choice is `FaunaClient`'s and
/// is the SAME runtime one it already makes for the store itself
/// (`custodianStoreFootprint`) — a provisioner means the agent hosts, none means
/// this process does — so no `#if os(...)` appears above the seam.
///
/// **Both sides answer the same six fields** — `hosting` / `kinds_run` /
/// `held_bytes` / `cap_state` / `audit_state` / `checked_in`, key-for-key with
/// tui's and android's arms — plus `via`, so a failing assertion prints WHICH
/// side ran the pass.
public enum CustodianPullTestCommand {
    /// What the shell does with the command's result. Kept as data rather than
    /// two callbacks so both shells' switch arms are the same three lines and the
    /// slot-clearing rule below lives in ONE place.
    public enum Outcome: Equatable {
        /// The pass ran; the JSON is the reply the shell stashes in
        /// `machineMethodResultBox`.
        case report(String)
        /// The command cannot be honoured; the shell must surface the reason as a
        /// **loud** TestAgent failure (convention 11) and leave the result slot
        /// empty — never a stale neighbour's report.
        case refused(String)
    }

    @MainActor
    public static func apply(_ command: [String: Any], client: FaunaClient?) async -> Outcome {
        // Absent/unparseable is `0`: an ordinary pass at the real clock. The same
        // spelling and default as `backup_audit_run_now` and tui's arm.
        let offset = TestCommandArgs.nowOffsetSecs(command)
        guard let client else {
            return .refused(
                "custodian_pull_run_now: no authenticated session (no live FaunaClient), "
                    + "so there is no device to host a replica")
        }
        do {
            let outcome = try await client.custodianPassForTest(nowOffsetSecs: offset)
            return .report(try reportJSON(outcome))
        } catch {
            return .refused("custodian_pull_run_now: \(error)")
        }
    }

    /// The reply body. Keys are snake_case and match tui's arm
    /// (`apps/fauna-tui/src/automation.rs`) exactly for the fields both sides
    /// share — `actions/backups.py::custodian_pull_run_now` parses this shape.
    /// An absent verdict is `null`, as tui serializes it.
    static func reportJSON(_ outcome: CustodianPassOutcome) throws -> String {
        let body: [String: Any]
        switch outcome {
        case .inApp(nil):
            // Not hosting yet. `held_bytes` stays present at 0, as the agent's
            // not-hosting report has it, so a caller reading the shared key
            // never has to branch on `hosting` first.
            body = ["via": "in-app", "hosting": false, "held_bytes": 0]
        case .agent(let report):
            body = fields(report, via: "agent")
        case .inApp(let report?):
            body = fields(report, via: "in-app")
        }
        let data = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])
        return String(decoding: data, as: UTF8.self)
    }

    private static func fields(_ report: FfiCustodianPassReport, via: String) -> [String: Any] {
        [
            "via": via,
            "hosting": report.hosting,
            "kinds_run": report.kindsRun,
            "held_bytes": report.heldBytes,
            "cap_state": report.capState ?? NSNull(),
            "audit_state": report.auditState ?? NSNull(),
            "checked_in": report.checkedIn,
        ]
    }
}

#endif
