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
    /// A shell that hosts in the app process (iOS): this process ran the pass, and
    /// the only door it has answers the four-field summary. `nil` is **not an
    /// enrolled custodian** — `build_custodian_host` answered `None`, which is the
    /// ordinary state until the enrollment reaches the registry, never an error.
    case inApp(FfiCustodianPullSummary?)
}

/// A poke this platform cannot honour, named so the harness reads the reason
/// instead of a generic failure (testing.md convention 11).
public enum CustodianPassPokeError: Error, Equatable, CustomStringConvertible {
    /// The in-app pass (`FfiCustodianHost.runAllKinds`) reads the wall clock
    /// itself and takes no offset, so a shifted clock would be silently ignored —
    /// a test that asked for a 24-hour jump would then pass against the real
    /// clock and prove nothing.
    case clockOffsetNotHonoured(Int64)

    public var description: String {
        switch self {
        case .clockOffsetNotHonoured(let offset):
            return "this device hosts its replica in the app process, whose pass "
                + "(`FfiCustodianHost.runAllKinds`) takes no clock offset, so "
                + "now_offset_secs=\(offset) cannot be honoured — only 0 is"
        }
    }
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
/// **The report is not the same six fields on both sides, and that is stated
/// rather than papered over.** The agent's `custodianRunPassNow` answers
/// `hosting` / `kinds_run` / `held_bytes` / `cap_state` / `audit_state` /
/// `checked_in`; the in-app door answers only what `FfiCustodianPullSummary`
/// carries. So an in-app reply omits the fields it has no honest value for
/// rather than fabricating them, and carries `via` so a failing assertion prints
/// WHICH side ran the pass. `hosting`, `checked_in` and `held_bytes` — all
/// `actions/backups.py::wait_for_custodian_hosting` and the orphaned-store
/// witness read — are on both; the summary's `checked_in` is computed by the
/// agent's own rule, never assumed.
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
        let offset = (command["now_offset_secs"] as? NSNumber)?.int64Value
            ?? (command["now_offset_secs"] as? Int).map(Int64.init)
            ?? 0
        guard let client else {
            return .refused(
                "custodian_pull_run_now: no authenticated session (no live FaunaClient), "
                    + "so there is no device to host a replica")
        }
        do {
            let outcome = try await client.custodianPassForTest(nowOffsetSecs: offset)
            return .report(try reportJSON(outcome))
        } catch let refusal as CustodianPassPokeError {
            return .refused("custodian_pull_run_now: \(refusal)")
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
        case .agent(let report):
            body = [
                "via": "agent",
                "hosting": report.hosting,
                "kinds_run": report.kindsRun,
                "held_bytes": report.heldBytes,
                "cap_state": report.capState ?? NSNull(),
                "audit_state": report.auditState ?? NSNull(),
                "checked_in": report.checkedIn,
            ]
        case .inApp(nil):
            // Not hosting yet. `held_bytes` stays present at 0, as the agent's
            // not-hosting report has it, so a caller reading the shared key
            // never has to branch on `hosting` first.
            body = ["via": "in-app", "hosting": false, "held_bytes": 0]
        case .inApp(let summary?):
            body = [
                "via": "in-app",
                "hosting": true,
                "held_bytes": summary.heldBytes,
                "stored_segments": summary.storedSegments,
                "tombstoned_segments": summary.tombstonedSegments,
                "cap_reached": summary.capReached,
                "checked_in": summary.checkedIn,
            ]
        }
        let data = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])
        return String(decoding: data, as: UTF8.self)
    }
}

#endif
