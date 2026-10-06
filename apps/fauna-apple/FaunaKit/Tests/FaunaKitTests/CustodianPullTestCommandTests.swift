import Testing
import Foundation
@testable import FaunaKit

// Non-e2e pins for the shared `custodian_pull_run_now` TestAgent command
// (`CustodianPullTestCommand`) — the reply SHAPE and the two refusals, the parts
// that go silently wrong without a nest in the loop. The pass itself (agent or
// in-app) is the e2e's: `test_backups.py::
// test_removing_a_custodian_without_the_opt_in_leaves_a_reclaimable_orphaned_store`
// is what runs it against a real nest and a real store.
//
// Why the shape needs its own pin: `actions/backups.py::custodian_pull_run_now`
// parses these keys straight off `state.machine_method_result`, and both apple
// shells hand the string over untouched. A renamed key is not a crash there — it
// is `hosting` reading as absent, which `wait_for_custodian_hosting` treats as
// "not hosting yet" and polls to its deadline, so the failure surfaces as a
// timeout that names the wrong cause.
#if DEBUG

@MainActor
@Suite("custodian_pull_run_now test-agent command")
struct CustodianPullTestCommandTests {
    private func decode(_ json: String) throws -> [String: Any] {
        let object = try JSONSerialization.jsonObject(with: Data(json.utf8))
        return try #require(object as? [String: Any])
    }

    /// The agent's six fields, key-for-key with tui's arm
    /// (`apps/fauna-tui/src/automation.rs`) — the shape every other app already
    /// answers, so the cross-app action layer reads all of them alike.
    @Test func anAgentReportCarriesTuisSixFieldsPlusItsVia() throws {
        let json = try CustodianPullTestCommand.reportJSON(
            .agent(
                FfiCustodianPassReport(
                    hosting: true, kindsRun: 3, heldBytes: 4096, capState: "ok",
                    auditState: "passed", checkedIn: true)))
        let body = try decode(json)
        #expect(Set(body.keys) == [
            "via", "hosting", "kinds_run", "held_bytes", "cap_state", "audit_state", "checked_in",
        ])
        #expect(body["via"] as? String == "agent")
        #expect(body["hosting"] as? Bool == true)
        #expect(body["kinds_run"] as? Int == 3)
        #expect(body["held_bytes"] as? Int == 4096)
        #expect(body["cap_state"] as? String == "ok")
        #expect(body["audit_state"] as? String == "passed")
        #expect(body["checked_in"] as? Bool == true)
    }

    /// `None` is "not audited on this pass", never a failed audit — it must reach
    /// the harness as JSON `null` (tui's `serde_json` spelling), not vanish.
    @Test func anAbsentVerdictIsNullNotMissing() throws {
        let body = try decode(
            CustodianPullTestCommand.reportJSON(
                .agent(
                    FfiCustodianPassReport(
                        hosting: false, kindsRun: 0, heldBytes: 0, capState: nil,
                        auditState: nil, checkedIn: false))))
        #expect(body["hosting"] as? Bool == false)
        #expect(body.keys.contains("cap_state") && body["cap_state"] is NSNull)
        #expect(body.keys.contains("audit_state") && body["audit_state"] is NSNull)
    }

    /// The in-app door has no `kinds_run` / `cap_state` / `audit_state` to
    /// report, so the reply must OMIT them. `checked_in` it does have — the
    /// summary computes it by the agent's rule — and it is passed through, never
    /// defaulted: `wait_for_custodian_hosting` requires `hosting` AND
    /// `checked_in`, so an in-app host that omitted it could never be seen
    /// hosting at all.
    @Test func anInAppReportOmitsWhatItHasNoHonestValueFor() throws {
        let body = try decode(
            CustodianPullTestCommand.reportJSON(
                .inApp(
                    FfiCustodianPullSummary(
                        storedSegments: 2, tombstonedSegments: 1, heldBytes: 900,
                        capReached: false, checkedIn: false))))
        #expect(body["via"] as? String == "in-app")
        #expect(body["hosting"] as? Bool == true)
        #expect(body["held_bytes"] as? Int == 900)
        #expect(body["stored_segments"] as? Int == 2)
        #expect(body["tombstoned_segments"] as? Int == 1)
        #expect(body["cap_reached"] as? Bool == false)
        #expect(body["checked_in"] as? Bool == false)
        for fabricated in ["kinds_run", "cap_state", "audit_state"] {
            #expect(!body.keys.contains(fabricated), "in-app must not invent \(fabricated)")
        }
    }

    /// A device that is not an enrolled custodian (`build_custodian_host` → `None`)
    /// is `hosting: false` — a report, not an error, which is exactly what lets
    /// `wait_for_custodian_hosting` poll instead of sleeping. `held_bytes` stays
    /// present so a caller never has to branch on `hosting` before reading it.
    @Test func notBeingAnEnrolledCustodianIsAReportNotAnError() throws {
        let body = try decode(CustodianPullTestCommand.reportJSON(.inApp(nil)))
        #expect(body["via"] as? String == "in-app")
        #expect(body["hosting"] as? Bool == false)
        #expect(body["held_bytes"] as? Int == 0)
    }

    /// No session → a NAMED refusal (convention 11), never a report. The shell
    /// leaves the result slot empty on this outcome, so the message is the only
    /// thing the caller gets to read.
    @Test func withNoLiveClientTheCommandRefusesByName() async {
        let outcome = await CustodianPullTestCommand.apply(
            ["action": "custodian_pull_run_now"], client: nil)
        guard case .refused(let reason) = outcome else {
            Issue.record("expected a refusal, got \(outcome)")
            return
        }
        #expect(reason.hasPrefix("custodian_pull_run_now:"), "the refusal must name the command: \(reason)")
        #expect(reason.contains("no authenticated session"))
    }

    /// The in-app pass reads the wall clock itself, so a shifted clock is refused
    /// with the value it was asked for rather than dropped: a caller that believed
    /// it had jumped a 24-hour audit debounce would otherwise pass against the
    /// real clock and prove nothing.
    @Test func aClockOffsetTheInAppPassCannotHonourIsRefusedWithItsValue() {
        let text = CustodianPassPokeError.clockOffsetNotHonoured(86_400).description
        #expect(text.contains("now_offset_secs=86400"))
        #expect(text.contains("cannot be honoured"))
    }
}

#endif
