import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for apple's convention 11 refusal surface
// (`e2e-conventions.md` § convention 11 and its apple build-out bullet).
//
// ⚠ **The whole point of this file is the SLOT, not the message.** Convention
// 11's build-out record is explicit that a pin which only checks "the text is
// set" is vacuous — it passes against both wrong slots, and that is precisely
// how tui's first fix shipped believing itself correct, and how android shipped
// the wrong slot for eleven days. Apple was the THIRD app to get this wrong,
// and it had the trap in its documented, literal form: `testAgentFailure` wrote
// `AppMessages.error`, and `applyNavPatch` — which EVERY `navigate_to` issues,
// and every action-layer helper issues a `navigate_to` — begins with
// `AppMessages.error = nil` under the comment *"Clear messages on navigation —
// errors belong to the page that raised them"*. Which is true of a page's
// errors and false of an agent's refusal. A refusal was therefore wiped
// microseconds after being set, and the driver read `error=''`: the exact
// sentence convention 11's build-out record uses for tui and android.
// `ErrorBanner.onAppear`/`onDisappear` clobber it a second way.
//
// So every case below drives the slot through the events that define it: **a
// banner rendering over it**, **the nav clear**, and **`reset`** (which must be
// the only thing that empties it). Asserting the text alone would grade none of
// that.
//
// 🧪 **Mutation-graded by RUNNING the mutant (2026-08-28), not by predicting
// it.** Pointing `reportRefusedAgentCommand` at `AppMessages.error` — the
// pre-2026-08-28 body, i.e. the real bug restored — kills **4 of these 6
// cases**: `aBannerRenderingAfterARefusalDoesNotEraseIt` (`→ false`),
// `aBannerDisappearingDoesNotEraseARefusal` (`→ nil`, the nav-clear shape),
// `resetClearsTheRefusal` and `clearingTheRefusalLeavesThePageBannerAlone`.
//
// The last two were NOT predicted when this file was written, and what they
// caught is worth keeping: with both messages in one slot, `reset` could not
// clear the refusal without also erasing the page's real error, and could not
// spare the page's error without also leaking the refusal into the next test.
// **The old code chose the leak** — `resetToFactory` never touched
// `AppMessages` at all, so a refusal survived `reset` unless some later banner
// or nav happened to overwrite it. One slot cannot serve two lifetimes; that is
// the argument for the split, and it was found by the mutant rather than by
// reasoning.
//
// The two survivors are honest survivors, not gaps: `aRefusalOutranksThePageBanner`
// asserts a precedence that is trivially true when both writes land in the same
// slot, and the two message-shape cases do not touch slot identity at all.
//
// `.serialized` because every case mutates the SAME process-wide statics,
// mirroring `BarrierTestCommandTests`.
#if DEBUG

@MainActor
@Suite("Test-agent refusal slot (convention 11's surface)", .serialized)
struct AgentRefusalSlotTests {
    /// Put the process-wide message statics back to a clean page, the way the
    /// agent's `reset` arm does.
    private func cleanSlate() {
        AppMessages.clearRefusedAgentCommand()
        AppMessages.error = nil
    }

    // MARK: - The slot survives what the page's own banner does

    /// The grading case. A refusal is stamped, then a *product* error banner
    /// renders — the ordinary consequence of navigating to a page that has
    /// something to say. The refusal must still be what the harness reads.
    @Test func aBannerRenderingAfterARefusalDoesNotEraseIt() {
        cleanSlate()

        AppMessages.reportRefusedAgentCommand("unknown command: no_such_command")
        // Exactly what `ErrorBanner.onAppear` does.
        AppMessages.error = "Could not reach the nest"

        #expect(AppMessages.errorForDisplay?.contains("no_such_command") == true, """
            the refusal was clobbered by a page banner. This is the wrong-slot \
            bug in its live form: `AppMessages.error` is the banner's mirror, so \
            anything parked there is overwritten by the next page that renders \
            an error, and the driver reads that page's message instead of the \
            reason its command was refused. Got \
            \(String(describing: AppMessages.errorForDisplay)).
            """)
    }

    /// The nav clear, which is the mechanism that actually did the damage.
    /// `applyNavPatch` opens with `AppMessages.error = nil` on every single
    /// `navigate_to`, and `ErrorBanner.onDisappear` does the same when a banner
    /// goes away. Either one wiped a refusal parked in the shared slot before
    /// the driver's next poll could read it.
    @Test func aBannerDisappearingDoesNotEraseARefusal() {
        cleanSlate()

        AppMessages.reportRefusedAgentCommand("conversations_accept_recipient: nothing committed")
        // A banner renders, then the harness navigates: `applyNavPatch`'s
        // `AppMessages.error = nil`, verbatim.
        AppMessages.error = "Draft not saved"
        AppMessages.error = nil

        #expect(AppMessages.errorForDisplay?.contains("conversations_accept_recipient") == true, """
            the refusal did not survive a nav. Convention 11 requires a slot \
            that is page-independent AND nav-independent; a per-page or \
            transient slot is wiped before the driver's next poll can read it, \
            so the driver reads `error=''` and the command looks honoured.
            """)
    }

    /// Precedence: the refusal is the reason the page is in whatever state it is
    /// in, so it outranks the page's own message rather than queueing behind it.
    @Test func aRefusalOutranksThePageBanner() {
        cleanSlate()

        AppMessages.error = "Could not reach the nest"
        #expect(AppMessages.errorForDisplay == "Could not reach the nest")

        AppMessages.reportRefusedAgentCommand("silent_sign_in (no active account)")

        #expect(AppMessages.errorForDisplay?.contains("silent_sign_in") == true, """
            with both slots full the harness must read the REFUSAL. Reading the \
            page banner instead is the silent-drop failure wearing a product \
            error's clothes: the command was refused, and the driver is told \
            about an unrelated network problem.
            """)
    }

    // MARK: - …and is cleared at reset, and only there

    /// `reset` is the per-test boundary every `app` fixture drives. A refusal
    /// that outlived it would fail the NEXT test's precondition assertion, which
    /// is the trap `BarrierTestCommand.clear()` exists to avoid for its own
    /// slots.
    @Test func resetClearsTheRefusal() {
        cleanSlate()

        AppMessages.reportRefusedAgentCommand("unknown command: leftover")
        #expect(AppMessages.errorForDisplay != nil)

        AppMessages.clearRefusedAgentCommand()

        #expect(AppMessages.refusedAgentCommand == nil)
        #expect(AppMessages.errorForDisplay == nil, """
            a refusal survived `reset` and will surface as the next test's \
            pre-existing error, failing an unrelated assertion in a way that \
            points nowhere near this command.
            """)
    }

    /// Clearing the refusal must not take the page's own banner with it — the
    /// two slots are independent in both directions.
    @Test func clearingTheRefusalLeavesThePageBannerAlone() {
        cleanSlate()

        AppMessages.error = "Could not reach the nest"
        AppMessages.reportRefusedAgentCommand("unknown command: x")
        AppMessages.clearRefusedAgentCommand()

        #expect(AppMessages.errorForDisplay == "Could not reach the nest", """
            clearing the agent's slot erased the page's real error too. The \
            harness would then see a clean page where the product is actually \
            failing — a false green.
            """)
    }

    // MARK: - The message itself

    /// Convention 6: a refusal that does not name its command sends the next
    /// session looking in the wrong place. The `[TestAgent]` prefix is what tells
    /// a reader the message came from the agent and not the product.
    @Test func aRefusalNamesTheAgentAndTheCommand() {
        cleanSlate()

        AppMessages.reportRefusedAgentCommand("unknown command: no_such_command")

        let surfaced = AppMessages.errorForDisplay ?? ""
        #expect(surfaced.contains("[TestAgent]"))
        #expect(surfaced.contains("no_such_command"))
    }

    /// The cross-app half of convention 11: `_assert_command_honoured`
    /// (`actions/conversations.py::_AGENT_FAILURE_MARKERS`) greps every
    /// sibling's refusal for one of two literal marker substrings. A refusal
    /// that carries `[TestAgent]` but not the marker is written, logged and
    /// rendered — and still invisible to that assert (the bug: pinned
    /// here so the two contracts can never drift apart silently again).
    @Test func aRefusalCarriesTheCrossAppMarker() {
        cleanSlate()

        AppMessages.reportRefusedAgentCommand("unknown command: no_such_command")

        let surfaced = AppMessages.errorForDisplay ?? ""
        #expect(surfaced.contains("test agent refused command"), """
            the refusal is missing the marker `_assert_command_honoured` greps \
            for (`test agent command` / `test agent refused command`) — this \
            command's refusal would pass that assert clean, the exact\
             bug.
            """)
    }

    /// Apple carries shared Rust's refusal sentence by hand — on that constant's
    /// own instruction, since no FFI export is warranted for a debug-only
    /// string. This pins the copy against drift, because the cross-app e2e can
    /// only assert text every app agrees on.
    @Test func theSharedRefusalSentenceHasNotDrifted() {
        #expect(acceptRecipientNoChipReason == """
            nothing committed — the active picker had no resolvable recipient \
            (empty input, or an address that resolved to no chip)
            """, """
            apple's copy of `fauna_conversations::manager::\
            ACCEPT_RECIPIENT_NO_CHIP_REASON` has drifted from the Rust \
            constant. The cross-app pin asserts the command NAME rather than \
            this sentence, so drift here reds nothing there — it just leaves \
            apple quietly telling the user a different story than tui and linux.
            """)
    }
}

#endif
