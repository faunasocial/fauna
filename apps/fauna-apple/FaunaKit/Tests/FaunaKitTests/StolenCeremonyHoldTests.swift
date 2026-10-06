import Testing
@testable import FaunaKit

/// `docs/goal/ui/settings.md` § Recovery kit → *The persist-failure message
/// survives the page*: a supersession this device's own stolen-identity
/// ceremony caused is held back while the ceremony runs or its persist-failure
/// message is pending, and performed once the user leaves Account — or at once,
/// when the ceremony ends while the user is elsewhere and nothing is parked.
/// The apple twin of tui's `the_ceremonys_own_supersession_waits_for_the_parked_key`.
@Suite @MainActor
struct StolenCeremonyHoldTests {
    /// Counts escalations performed — the teardown + `runLaunch()` stand-in.
    final class Escalations {
        var count = 0
        func perform() async { count += 1 }
    }

    /// No ceremony owns Account: an ordinary mid-session supersession goes to
    /// the launch surface at once.
    @Test func anOrdinarySupersessionEscalatesAtOnce() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()

        await escalateSessionEnding(.superseded, ceremonyHold: hold) {
            await escalations.perform()
        }

        #expect(escalations.count == 1)
        #expect(!hold.isOwed)
    }

    /// The whole path the outcome-17 journey drives: the ceremony's own
    /// supersession arrives mid-ceremony, the seed cannot be stored, the key is
    /// parked on Account — the escalation waits through all of it and runs on
    /// the leave edge.
    ///
    /// Mutation check: dropping `messagePending` from `escalate`'s condition, or
    /// `ceremonyEnded` performing without checking `onAccount`, reds this.
    @Test func theCeremonysOwnSupersessionWaitsForTheParkedKey() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()
        hold.accountAppeared()
        hold.ceremonyStarted()

        await escalateSessionEnding(.superseded, ceremonyHold: hold) {
            await escalations.perform()
        }
        #expect(escalations.count == 0, "the ceremony's result has not been handled yet")
        #expect(hold.isOwed)

        await hold.ceremonyEnded(adopted: false, messageParked: true)
        #expect(escalations.count == 0, "the parked key is the only copy — it must stay on screen")

        await hold.accountLeft()
        #expect(escalations.count == 1, "leaving Account performs the held-back escalation")
        #expect(!hold.isOwed)
    }

    /// A parked message holds the escalation even when the user is not on
    /// Account at the moment the ceremony ends.
    @Test func aParkedMessageHoldsTheEscalationWhereverTheUserIs() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()
        hold.ceremonyStarted()
        await hold.escalate { await escalations.perform() }

        await hold.ceremonyEnded(adopted: false, messageParked: true)

        #expect(escalations.count == 0)
        #expect(hold.isOwed)
    }

    /// The ceremony failed off Account with nothing parked: the owed escalation
    /// runs at once — no leave edge is coming.
    @Test func aCeremonyEndingOffAccountWithNothingParkedEscalatesAtOnce() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()
        hold.ceremonyStarted()
        await hold.escalate { await escalations.perform() }

        await hold.ceremonyEnded(adopted: false, messageParked: false)

        #expect(escalations.count == 1)
    }

    /// On Account with nothing parked, the user reads the ceremony's result
    /// first: the leave edge performs the escalation, not the ceremony's end.
    @Test func onAccountTheLeaveEdgePerformsItAfterTheResultIsRead() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()
        hold.accountAppeared()
        hold.ceremonyStarted()
        await hold.escalate { await escalations.perform() }

        await hold.ceremonyEnded(adopted: false, messageParked: false)
        #expect(escalations.count == 0)

        await hold.accountLeft()
        #expect(escalations.count == 1)
    }

    /// iOS re-entering Recovery kit mounts the new section BEFORE the old one
    /// unmounts — the two-mounted-sections shape `SuccessionHandoff`'s
    /// `rearmUnshownKit` records. The old one's leave edge is not the user
    /// leaving Account. Measured 2026-09-26 on
    /// `test_a_lost_reply_you_cannot_check_says_so_and_reopening_signs_you_in[ios]`:
    /// a boolean read "off Account" at the ceremony's end, escalated at once,
    /// and the import screen replaced the undecided-outcome message.
    ///
    /// Mutation check: collapsing the mount count back to a boolean reds this.
    @Test func anOverlappingRemountIsStillOnAccount() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()
        hold.accountAppeared()          // the section the user opened first
        hold.accountAppeared()          // the re-entered one, mounted first…
        await hold.accountLeft()        // …then the first one goes
        hold.ceremonyStarted()
        await hold.escalate { await escalations.perform() }

        await hold.ceremonyEnded(adopted: false, messageParked: false)
        #expect(escalations.count == 0,
                "the user is still on Account reading the outcome — hold the escalation")

        await hold.accountLeft()        // the user really leaves
        #expect(escalations.count == 1)
    }

    /// The refusal can land AFTER the ceremony's result (tui measured ~30 ms
    /// after an undecidable one). A ceremony that adopted nothing while the user
    /// was on Account keeps owning its supersession until they leave, so its
    /// message is not replaced by an import screen for a key never shown —
    /// tui's `App::stolen_outcome_on_screen`.
    ///
    /// Mutation check: dropping `outcomeOnScreen` from `escalate`'s condition
    /// reds this.
    @Test func aRefusalLandingAfterTheResultStillWaitsForTheLeaveEdge() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()
        hold.accountAppeared()
        hold.ceremonyStarted()
        await hold.ceremonyEnded(adopted: false, messageParked: false)

        await escalateSessionEnding(.superseded, ceremonyHold: hold) {
            await escalations.perform()
        }
        #expect(escalations.count == 0, "the outcome message is still what Account shows")

        await hold.accountLeft()
        #expect(escalations.count == 1)

        await escalateSessionEnding(.superseded, ceremonyHold: hold) {
            await escalations.perform()
        }
        #expect(escalations.count == 2, "once left, a later supersession is ordinary again")
    }

    /// The ceremony adopted its successor: the account switch is itself the
    /// relaunch, so the owed escalation is spent, never performed.
    @Test func anAdoptedSuccessorSpendsTheOwedEscalation() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()
        hold.accountAppeared()
        hold.ceremonyStarted()
        await hold.escalate { await escalations.perform() }

        await hold.ceremonyEnded(adopted: true, messageParked: false)
        await hold.accountLeft()

        #expect(escalations.count == 0)
        #expect(!hold.isOwed)
    }

    /// Only the supersession is the ceremony's own doing: a changed nest
    /// identity or a refused sign-in escalates at once even mid-ceremony.
    @Test func otherSessionEndingVerdictsAreNeverHeldBack() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()
        hold.accountAppeared()
        hold.ceremonyStarted()

        await escalateSessionEnding(.nestIdentityChanged, ceremonyHold: hold) {
            await escalations.perform()
        }
        await escalateSessionEnding(.signInRefused, ceremonyHold: hold) {
            await escalations.perform()
        }

        #expect(escalations.count == 2)
        #expect(!hold.isOwed)
    }

    /// The page's other error writers share `error-message` with the parked key
    /// — the export's refused 401 on the dead session is the journey's witness.
    /// While the message is pending their errors are dropped; once the user has
    /// left Account they land again.
    ///
    /// Mutation check: `report` assigning without asking `admits`, or `admits`
    /// ignoring `messagePending`, reds this.
    @Test func aPendingMessageDropsEveryOtherAccountPageError() async {
        let hold = StolenCeremonyHold()
        let vm = AccountSettingsVM(ceremonyHold: hold)
        hold.accountAppeared()
        hold.ceremonyStarted()
        await hold.ceremonyEnded(adopted: false, messageParked: true)

        vm.report(\.exportError, "HTTP 401")
        vm.report(\.deleteAccountError, "delete failed")
        #expect(vm.exportError == nil, "the only copy of the new key must stay on screen")
        #expect(vm.deleteAccountError == nil)

        await hold.accountLeft()
        vm.report(\.exportError, "HTTP 401")
        #expect(vm.exportError == "HTTP 401", "the acknowledged message no longer holds the page")
    }

    /// Leaving Account while the ceremony is still in flight does not perform
    /// the escalation — the ceremony's result has not been handled yet.
    @Test func leavingAccountMidCeremonyStillWaitsForTheResult() async {
        let hold = StolenCeremonyHold()
        let escalations = Escalations()
        hold.accountAppeared()
        hold.ceremonyStarted()
        await hold.escalate { await escalations.perform() }

        await hold.accountLeft()
        #expect(escalations.count == 0)

        await hold.ceremonyEnded(adopted: false, messageParked: false)
        #expect(escalations.count == 1, "off Account with nothing parked, the end performs it")
    }
}
