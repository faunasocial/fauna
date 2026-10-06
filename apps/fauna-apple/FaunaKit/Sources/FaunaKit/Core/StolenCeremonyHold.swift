import Foundation
import Observation

/// The `.faunaSessionEnding` notification's payload key.
public enum FaunaSessionEnding {
    public static let verdictKey = "verdict"
}

/// What this device's stolen-identity ceremony holds while it owns the Account
/// page (`docs/goal/ui/settings.md` § Recovery kit → *The persist-failure
/// message survives the page*): the page's other error writers, and the
/// supersession escalation the ceremony itself causes. tui's
/// `App::defer_own_supersession` / `escalate_deferred_supersession` and its
/// `PageErrors::stolen_failed_pending` guard are the model.
///
/// **The writers.** When the successor's seed could not be stored, the
/// ceremony parks the new key on Account's `error-message` — the only copy in
/// existence. Every other writer on that page (``AccountSettingsVM``'s banners
/// share the id) drops its error while ``messagePending`` is set; clears still
/// land, since they cannot hide the key.
///
/// **The escalation.** The ceremony is what supersedes the identity, so the running session's next
/// auth refresh is refused the moment the nest commits — typically before the
/// ceremony's own result is handled. Escalating then (tear down + `runLaunch()`)
/// would take the page, and the ceremony's result, down with it; when the
/// successor's seed could not be stored, that result is the only copy of the
/// new key. So while the ceremony runs, or its persist-failure message is still
/// pending, the escalation is recorded rather than performed, and it runs once
/// the user leaves Account — or at once, when the ceremony ends while the user is
/// elsewhere and no message is parked.
///
/// Only the supersession verdict goes through here: a nest-identity change or a
/// refused sign-in is never the ceremony's own doing, so it escalates at once.
///
/// One per process (``shared``), because its three reporters live at three
/// different altitudes — the connection observer (app root), the ceremony
/// (``RecoveryKitVM``) and the Account page's lifecycle (``RecoveryKitSection``)
/// — and the section's view model dies with the view. `init` is public so a
/// unit test owns a fresh one.
@MainActor @Observable
public final class StolenCeremonyHold {
    public static let shared = StolenCeremonyHold()

    public init() {}

    /// True from the stolen ceremony's dispatch until its result is handled.
    public private(set) var ceremonyInFlight = false
    /// True while the ceremony's persist-failure message is parked on Account.
    public private(set) var messagePending = false
    /// True while the Account page (the Recovery Kit section) is on screen.
    public var onAccount: Bool { accountMounts > 0 }
    /// How many Recovery Kit sections are mounted. A count, not a flag: iOS
    /// re-entering the page mounts the new section BEFORE the old one unmounts,
    /// so the old one's leave edge is not the user leaving Account — a boolean
    /// read "off Account" there and escalated over the ceremony's outcome
    /// (measured 2026-09-26, the lost-reply journey on iOS).
    @ObservationIgnored private var accountMounts = 0
    /// True when the ceremony ended WITHOUT adopting a successor while the user
    /// was on Account, so its outcome message is what the page shows — until the
    /// user leaves. It extends the hold past the result, because the refusal the
    /// ceremony causes has no fixed order against it (tui measured one landing
    /// ~30 ms after an undecidable result — `App::stolen_outcome_on_screen`), and
    /// escalating then would replace the message the user needs with an import
    /// screen for a key they were never shown.
    public private(set) var outcomeOnScreen = false
    /// The escalation held back, if one is owed.
    @ObservationIgnored private var owed: (@MainActor () async -> Void)?

    /// Whether another Account-page writer may put `text` on screen: a clear
    /// always, an error only while no persist-failure message is pending. The one
    /// gate every other Account-page error writer goes through.
    public func admits(_ text: String?) -> Bool {
        text == nil || !messagePending
    }

    /// Whether an escalation is owed — for tests and the diagnostic log.
    public var isOwed: Bool { owed != nil }

    /// Route a supersession escalation: performed now, unless the ceremony owns
    /// the Account page, in which case it is recorded (latest wins — every
    /// arrival is the same teardown).
    public func escalate(_ perform: @escaping @MainActor () async -> Void) async {
        if ceremonyInFlight || outcomeOnScreen || messagePending {
            logMessage(level: .info, target: "fauna.app",
                       message: "[supersession] held back: this device's own ceremony owns Account")
            owed = perform
            return
        }
        await perform()
    }

    /// `identity-stolen-button` dispatched the ceremony.
    public func ceremonyStarted() {
        ceremonyInFlight = true
    }

    /// The ceremony's result was handled.
    ///
    /// - `adopted`: the device switched to the successor — the switch is itself
    ///   the full relaunch, so an owed escalation is spent, not performed.
    /// - `messageParked`: the persist-failure message now sits on Account and
    ///   holds the escalation until the user leaves it, wherever they are.
    ///
    /// Otherwise an owed escalation runs now if the user is off Account; on
    /// Account the result stays on screen (``outcomeOnScreen``) and the leave
    /// edge performs the escalation, after the user has read it.
    public func ceremonyEnded(adopted: Bool, messageParked: Bool) async {
        ceremonyInFlight = false
        messagePending = messageParked
        if adopted {
            owed = nil
            return
        }
        outcomeOnScreen = onAccount
        if !onAccount && !messagePending {
            await performOwed()
        }
    }

    /// The Account page appeared.
    public func accountAppeared() {
        accountMounts += 1
    }

    /// A Recovery Kit section left the screen. When it was the last one, the
    /// user left the Account page — the persist-failure message's one
    /// acknowledgment gesture, so a parked message is discharged here and the
    /// held-back escalation, if any, goes the ordinary way.
    public func accountLeft() async {
        accountMounts = max(0, accountMounts - 1)
        guard accountMounts == 0 else { return }
        messagePending = false
        outcomeOnScreen = false
        guard !ceremonyInFlight else { return }
        await performOwed()
    }

    private func performOwed() async {
        guard let perform = owed else { return }
        owed = nil
        logMessage(level: .info, target: "fauna.app",
                   message: "[supersession] performing the held-back escalation")
        await perform()
    }
}
