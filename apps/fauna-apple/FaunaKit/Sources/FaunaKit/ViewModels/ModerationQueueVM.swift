import Foundation

/// Drives the shared **Moderation queue** (`ModerationQueueView`) over the
/// existing `fauna.moderation.*` wire — the user's view onto why their *own*
/// content was labeled / quarantined / rejected, and their lever to correct it
/// (moderation.md § Goal). Both apple apps share this one VM (priority #2),
/// mirroring linux's `views/moderation.rs` presentation over the same RPCs.
///
/// The queue is the **union** of two sources (moderation.md § Layout & flow): the
/// server `FfiModerationClient.actions()` obligation rows and the client's own
/// post-decrypt **local detections** (`ConversationsSession.moderationLocalDetections()`
/// — the encrypted-mode social-content signal the nest can't produce), merged +
/// deduped by `content_id` through the shared `moderationQueue(server:local:)`
/// façade so the rule never drifts per client. A server row's
/// `train-correction-button` submits a training correction through the one shared
/// flow `MailSettingsMachine.trainModerationCorrection` (sealed tier-1 model write
/// when the nest advertises `spam-model-sealed-at-rest`, then `fauna.moderation.train`
/// always — moderation.md § State & data shape → *Training correction*); a local
/// row's removes the client-side flag and feeds the retained text to the sealed
/// model. Mirrors linux `client.rs::correct_moderation_row` and windows'
/// `ModerationViewModel`.
@MainActor @Observable
public final class ModerationQueueVM {
    /// One merged queue row (server obligation ∪ local detection), newest-first,
    /// deduped by `content_id`. Empty `[]` is the empty state, not an error.
    public private(set) var rows: [QueueRow] = []
    public private(set) var isLoading = false
    public var errorMessage: String?
    /// The reporter's own ledger — every abuse report this user filed, newest
    /// first (`moderation-reports-section`; moderation.md § User-initiated
    /// reporting → *What the reporter is told*).
    public private(set) var reports: [FfiReportLedgerRow] = []
    /// Whether the ledger has been read at least once — the empty line paints
    /// only off this bit, so a slow first read never claims "You have not
    /// reported anything" (`ui/README.md` § List pages: loading is not empty).
    public private(set) var reportsLoaded = false
    /// The line the last withdraw painted (`""` before one).
    public private(set) var reportStatus = ""

    // ── Appeal (moderation.md § Legal takedown — the transparency triple's second
    //    leg). Every decision is pre-made in shared Rust: `rowIsAppealable` picks
    //    the rows that offer a handle, `appealFormView` owns the guard and the
    //    wording, `appealVerdict` the outcome sentence. This VM paints that fold and
    //    dispatches; it decides and words nothing. Mirrors windows'
    //    `ModerationViewModel` and android's `ModerationQueueVM.openAppeal`/`submitAppeal`.

    /// The decision being appealed, captured when the form OPENED. The queue
    /// refetches, so a dispatch keyed on the row's index could appeal the wrong
    /// decision.
    public private(set) var appealContentId: String?
    /// Whether the appeal form is open (`appeal-reason-input`,
    /// `appeal-submit-button` and `appeal-cancel-button` paint only then).
    public var appealOpen: Bool { appealContentId != nil }
    /// `appeal-reason-input` — the reason as typed.
    public var appealReason = ""
    /// `appeal-status` — the last attempt's outcome. Page-level, so it survives the
    /// form closing on success, and deliberately NOT `errorMessage`: a refused appeal
    /// is form feedback, and the page error would blank the queue the user is reading.
    public private(set) var appealStatus: String?
    private var appealBusy = false

    private var moderation: FfiModerationClient?
    /// The client half of the queue — `nil` when no conversations session is
    /// active yet, matching linux's `active_session()` → `None` (the queue is then
    /// the server rows alone).
    private var conversationsSession: ConversationsSession?
    /// The sealed spam-model client-write path (1d — mail-spam.md § Encrypted-mode
    /// interaction). `nil` ⇒ no sealed-write path ⇒ every correction takes the
    /// server-side / flag-removal route unchanged (e.g. mail not enabled).
    #if !FAUNA_KIDS
    private var mailSettings: (any ModerationTraining)?
    #endif
    private var api: APIClient?

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary), on `SearchVM.reset()`'s shape. Called by ``configure(api:)`` on an
    /// api-identity change **before** it rebuilds, and by the page's nil-client phase:
    /// More → Moderation is not unmounted by the iOS switch teardown
    /// .
    ///
    /// ⚠ This was the worst of the surviving set, because the three `== nil` guards
    /// below made the survival PERMANENT while `configure`'s first line re-pointed
    /// `api` regardless — `SearchVM`'s own finding in triplicate. After a
    /// switch the page held the incoming account's `api` paired with the OUTGOING
    /// account's `FfiModerationClient`, its live `ConversationsSession` (the MLS
    /// engine behind the local post-decrypt detections) and its `MailSettingsMachine`
    /// (the sealed tier-1 spam model). That is not only a read of another account's
    /// enforcement history: ``correct(row:)`` would train the OUTGOING account's
    /// sealed spam model and remove its client-side flags while the app is
    /// authenticated as the incoming one — a cross-account WRITE through three
    /// handles the incoming session never owned.
    ///
    /// Dropping the three handles is corollary 2's second rule met as well (the loops
    /// that write the state are retired with it): the conversations session is the one
    /// whose MLS receive loop keeps running until it is released.
    public func reset() {
        api = nil
        moderation = nil
        conversationsSession = nil
        #if !FAUNA_KIDS
        mailSettings = nil
        #endif
        rows = []
        isLoading = false
        errorMessage = nil
        reports = []
        reportsLoaded = false
        reportStatus = ""
        appealContentId = nil
        appealReason = ""
        appealStatus = nil
        appealBusy = false
    }

    #if DEBUG && !FAUNA_KIDS
    /// Unit-test seam: stand in for the mail-settings machine `configure` vends.
    func seedMailSettingsForTest(_ trainer: (any ModerationTraining)?) {
        mailSettings = trainer
    }
    #endif

    #if DEBUG
    /// Unit-test seam: stand in for a completed load, which a real one cannot be
    /// offline (it dials the nest). Never called by production — mirrors
    /// `AddressBookVM.seedForTest`.
    func seedForTest(rows: [QueueRow]) {
        self.rows = rows
    }
    #endif

    /// Vend the `fauna.moderation.*` client + best-effort the shared conversations
    /// session + the mail-settings machine over the shared connection, then load
    /// the queue. Called from the view's `.task`, keyed on the session.
    public func configure(api: APIClient) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        if moderation == nil {
            do {
                let client = try await api.moderationClient()
                guard self.api === api else { return }   // the in-flight clause
                moderation = client
            } catch {
                guard self.api === api else { return }
                errorMessage = DisplayError.message(error)
                return
            }
        }
        if conversationsSession == nil {
            let session = await api.sharedConversationsSession()
            guard self.api === api else { return }   // the in-flight clause
            conversationsSession = session
        }
        #if !FAUNA_KIDS
        if mailSettings == nil {
            let settings = try? await api.mailSettingsMachine()
            guard self.api === api else { return }   // the in-flight clause
            mailSettings = settings
        }
        #endif
        await load()
    }

    /// Refresh the queue: the server `fauna.moderation.actions` rows merged with
    /// the conversations session's retained local detections. An empty reply
    /// renders the empty state, not an error (moderation.md § Errors & edge cases).
    public func load() async {
        guard let moderation, let api else { return }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            let server = try await moderation.actions()
            // The in-flight clause: a read suspended for the outgoing account still
            // returns after the drop, so neither its server rows nor the local
            // detections merged with them may land on the incoming account's page.
            guard self.api === api else { return }
            let local = conversationsSession?.moderationLocalDetections() ?? []
            rows = moderationQueue(server: server, local: local)
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
        // The reporter's ledger rides every Moderation entry beside the queue —
        // its own failure never hides the queue rows above.
        await loadReports()
    }

    /// Read the reporter's ledger (`fauna.moderation.abuse_report.mine`).
    public func loadReports() async {
        guard let moderation, let api else { return }
        do {
            let mine = try await moderation.abuseReportMine()
            guard self.api === api else { return }   // the in-flight clause
            reports = mine
            reportsLoaded = true
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    /// Withdraw one open report and re-read the ledger. The verdict line comes
    /// from the shared fold (success names what was deleted everywhere).
    public func withdrawReport(reportId: String) async {
        guard let moderation, let api else { return }
        do {
            try await moderation.abuseReportWithdraw(reportId: reportId)
            guard self.api === api else { return }
            reportStatus = renderLocalizedText(reportWithdrawVerdict(error: nil))
        } catch {
            guard self.api === api else { return }
            reportStatus = renderLocalizedText(reportWithdrawVerdict(error: String(describing: error)))
        }
        await loadReports()
    }

    /// One ledger row's line — `reason · status · short subject id · outcome —
    /// where it went` (web's `ledgerLine`, tui's `ledger_elements`: the e2e
    /// reads this text on every app).
    public nonisolated static func ledgerLine(_ row: FfiReportLedgerRow) -> String {
        var parts = [renderLocalizedText(row.reason), renderLocalizedText(row.status)]
        if let id = ReportSheetStore.subjectId(of: row.subject) {
            parts.append(shortId(hex: id))
        }
        if let outcome = row.outcome { parts.append(renderLocalizedText(outcome)) }
        return "\(parts.joined(separator: " · ")) — \(renderLocalizedText(row.routedTo))"
    }

    /// Submit a `train-correction-button` correction for one merged row. A
    /// **server** row (carries an enforcement `action`) submits "false positive →
    /// not spam" through the shared `trainModerationCorrection` flow — the sealed
    /// model write when available, then the nest's `fauna.moderation.train`
    /// always — so every correction reaches report capture. The Kids build has no
    /// mail machine and runs the nest half alone. A **local** row (blank action)
    /// has no server obligation to train against — the content is client-only —
    /// so the correction removes the client-side flag and, when the sealed write
    /// path is available, additionally feeds the retained post-decrypt text to the
    /// tier-1 model as a ham correction. The queue is enforcement *history*, so the
    /// row stays server-side; only a failure surfaces.
    public func correct(row: QueueRow) async {
        // The identity this correction is issued under. Every await below can suspend
        // across a switch, and a correction is a WRITE — so each step re-checks before
        // taking the next one, not merely before painting (the in-flight clause).
        let issuedApi = api
        if row.source == .local {
            let text = conversationsSession?.moderationMessageBody(contentId: row.contentId)
            _ = conversationsSession?.moderationRemoveLocalDetection(contentId: row.contentId)
            #if !FAUNA_KIDS
            if let mailSettings, let text, !text.isEmpty {
                _ = try? await mailSettings.trainSpamModelClient(text: text, isSpam: false)
            }
            #endif
            guard self.api === issuedApi else { return }
            await load()
            return
        }
        do {
            #if FAUNA_KIDS
            try await moderation?.train(contentId: row.contentId, verdict: "ham")
            #else
            guard let mailSettings else {
                throw ModerationCorrectionError.noMailSettingsMachine
            }
            _ = try await mailSettings.trainModerationCorrection(contentId: row.contentId, isSpam: false)
            #endif
        } catch {
            guard self.api === issuedApi else { return }
            errorMessage = DisplayError.message(error)
        }
        guard self.api === issuedApi else { return }
        await load()
    }

    /// Whether `row` offers an `appeal-button` — the shared fold's call (a server
    /// row only; a local detection's lever is the train-correction button).
    public nonisolated static func isAppealable(_ row: QueueRow) -> Bool {
        rowIsAppealable(row: row)
    }

    /// The form's per-keystroke fold over the captured content id and the reason as
    /// typed (`canSubmit`, `blockedReason`, `submitLabel`, `summary`).
    public var appealForm: FfiAppealFormView {
        appealFormView(contentId: appealContentId ?? "", reason: appealReason)
    }

    /// `appeal-button`: open the form on `row`, capturing its `content_id` now.
    public func openAppeal(row: QueueRow) {
        appealContentId = row.contentId
        appealReason = ""
        appealStatus = nil
    }

    /// `appeal-cancel-button`: drop the draft and the stale status.
    public func cancelAppeal() {
        appealContentId = nil
        appealReason = ""
        appealStatus = nil
    }

    /// `appeal-submit-button`: dispatch the captured draft. Re-gates on the shared
    /// `canSubmit` rather than trusting the control's enabled state. There is no
    /// offline gate: `fauna.moderation.appeal` is `OfflineQueued`. Success CLOSES the
    /// form (the decision is with an administrator now; the row stays — appeals are
    /// additive history); failure KEEPS it open with the reason intact so it can be
    /// fixed and resent.
    public func submitAppeal() async {
        guard !appealBusy, let moderation, let api, let contentId = appealContentId else { return }
        let reason = appealReason
        guard appealFormView(contentId: contentId, reason: reason).canSubmit else { return }
        appealBusy = true
        defer { appealBusy = false }
        var failure: String?
        do {
            _ = try await moderation.appeal(contentId: contentId, reason: reason)
        } catch {
            failure = DisplayError.message(error)
        }
        guard self.api === api else { return }   // the in-flight clause
        appealStatus = renderLocalizedText(appealVerdict(error: failure))
        if failure == nil {
            appealContentId = nil
            appealReason = ""
        }
    }

    /// Whole-percent confidence from the wire's per-mille `u16` (0–1000; the
    /// dag-cbor wire forbids floats), rounded. The rounding decision lives once in
    /// shared Rust (`fauna_core::format::confidence_percent`, half-up); this is a
    /// thin passthrough over the UniFFI face so no client hand-rolls `(m + 5) / 10`
    /// (value-formatting.md § Confidence percent; priority #2/#4).
    /// `nonisolated` — a pure stateless helper, callable from any context.
    public nonisolated static func confidencePercent(_ perMille: UInt16) -> Int {
        Int(FaunaFFISwift.confidencePercent(perMille: perMille))
    }

    /// The queue row's `content-label-badge` automation facts (moderation.md
    /// § Element IDs → *Queue row facts*): `confidence`, the whole percent with its
    /// `%` exactly as the row paints it, and `action`, the resolved action label —
    /// absent on a local detection, which carries none. The shared category key
    /// the badge styles from rides with them, the fleet-uniform fact set windows
    /// and android pack into one carrier.
    public nonisolated static func badgeFacts(
        category: String, confidencePerMille: UInt16, action: UInt8?
    ) -> [String: String] {
        var facts = [
            "category": category,
            "confidence": "\(confidencePercent(confidencePerMille))%",
        ]
        if let action {
            facts["action"] = renderLocalizedText(obligationActionLabel(action: action))
        }
        return facts
    }
}

#if !FAUNA_KIDS
/// The slice of `MailSettingsMachine` the queue drives — a seam so the VM's
/// routing is unit-testable without a live machine.
protocol ModerationTraining: Sendable {
    func trainModerationCorrection(contentId: String, isSpam: Bool) async throws -> SpamModelClientWrite
    func trainSpamModelClient(text: String, isSpam: Bool) async throws -> SpamModelClientWrite
}

extension MailSettingsMachine: ModerationTraining {}

/// A server-row correction with no mail-settings machine to run the shared flow.
enum ModerationCorrectionError: Error, CustomStringConvertible {
    case noMailSettingsMachine

    // `DisplayError.message` paints `"\(error)"` for a non-Ffi error.
    var description: String { "no mail settings machine to run the training correction" }
}
#endif
