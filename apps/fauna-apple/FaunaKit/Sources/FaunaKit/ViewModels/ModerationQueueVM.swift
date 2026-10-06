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
/// `train-correction-button` submits a training correction; a local row's removes
/// the client-side flag. Both routes additionally prefer the **sealed tier-1
/// client-write path** (`MailSettingsMachine.trainSpamModelClient`) over the
/// server-side train when the nest advertises `spam-model-sealed-at-rest`
/// (mail-spam.md § Encrypted-mode interaction — the 1d surface switch), mirroring
/// linux `client.rs::{correct_moderation_row, train_moderation_flow}` and windows'
/// `ModerationViewModel`.
@MainActor @Observable
public final class ModerationQueueVM {
    /// One merged queue row (server obligation ∪ local detection), newest-first,
    /// deduped by `content_id`. Empty `[]` is the empty state, not an error.
    public private(set) var rows: [QueueRow] = []
    public private(set) var isLoading = false
    public var errorMessage: String?

    private var moderation: FfiModerationClient?
    /// The client half of the queue — `nil` when no conversations session is
    /// active yet, matching linux's `active_session()` → `None` (the queue is then
    /// the server rows alone).
    private var conversationsSession: ConversationsSession?
    /// The sealed spam-model client-write path (1d — mail-spam.md § Encrypted-mode
    /// interaction). `nil` ⇒ no sealed-write path ⇒ every correction takes the
    /// server-side / flag-removal route unchanged (e.g. mail not enabled).
    private var mailSettings: MailSettingsMachine?
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
        mailSettings = nil
        rows = []
        isLoading = false
        errorMessage = nil
    }

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
        if mailSettings == nil {
            let settings = try? await api.mailSettingsMachine()
            guard self.api === api else { return }   // the in-flight clause
            mailSettings = settings
        }
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
    }

    /// Submit a `train-correction-button` correction for one merged row. A
    /// **server** row (carries an enforcement `action`) submits "false positive →
    /// not spam" (`verdict: "ham"`) — preferring the sealed client-write path over
    /// `fauna.moderation.train` when available. A **local** row (blank action) has
    /// no server obligation to train against — the content is client-only — so the
    /// correction removes the client-side flag and, when the sealed write path is
    /// available, additionally feeds the retained post-decrypt text to the tier-1
    /// model as a ham correction. The queue is enforcement *history*, so the row
    /// stays server-side; only a failure surfaces.
    public func correct(row: QueueRow) async {
        // The identity this correction is issued under. Every await below can suspend
        // across a switch, and a correction is a WRITE — so each step re-checks before
        // taking the next one, not merely before painting (the in-flight clause).
        let issuedApi = api
        if row.source == .local {
            let text = conversationsSession?.moderationMessageBody(contentId: row.contentId)
            _ = conversationsSession?.moderationRemoveLocalDetection(contentId: row.contentId)
            if let mailSettings, let text, !text.isEmpty {
                _ = try? await mailSettings.trainSpamModelClient(text: text, isSpam: false)
            }
            guard self.api === issuedApi else { return }
            await load()
            return
        }
        var trainedClientSide = false
        if let mailSettings, (try? await mailSettings.sealedSpamWriteAvailable()) == true,
            let text = await api?.postBodyText(contentId: row.contentId)
        {
            guard self.api === issuedApi else { return }
            if let result = try? await mailSettings.trainSpamModelClient(text: text, isSpam: false) {
                trainedClientSide = result.sealed
            }
        }
        guard self.api === issuedApi else { return }
        if !trainedClientSide {
            do {
                try await moderation?.train(contentId: row.contentId, verdict: "ham")
            } catch {
                guard self.api === issuedApi else { return }
                errorMessage = DisplayError.message(error)
            }
        }
        guard self.api === issuedApi else { return }
        await load()
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
}
