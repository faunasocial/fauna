import SwiftUI

@MainActor @Observable
public class PrivacySettingsVM {
    // Inbox mode. `nil` until `loadAll()`'s fetch resolves — the selector must
    // show the account's stored mode, never a default (settings.md § Privacy
    // sub-page item 6); a guessed value is a false statement about who can
    // reach the user, and painting one before the real mode is known is
    // forbidden even though it's never written back.
    public var inboxMode: String?
    public var inboxModeLoading = false
    public var inboxModeError: String?

    // Email filters
    public var emailFilters: [FfiEmailFilter] = []
    public var emailFilterError: String?
    public var showFilterForm = false
    public var creatingFilter = false
    /// Non-nil while the shared create/edit form is editing an existing
    /// filter rather than composing a new one — `filter-edit` sets it,
    /// `save-filter`/cancel clear it. The View's `create-filter`/`save-filter`
    /// buttons are mutually exclusive on this (distinct ui.yaml ids, per the
    /// linux `Ctx::editing_id` reference).
    public var editingFilterId: Int64?

    // Spam preferences
    public var spamThreshold: Double = 0.5
    public var phishingThreshold: Double = 0.3
    public var spamPrefsLoading = false
    public var spamPrefsSaved = false
    public var spamPrefsError: String?

    private var api: APIClient?
    private var actorId: String?
    /// The session whose ``FaunaClient/inheritedFilterMarks`` cache this list
    /// paints its post-succession marks from and re-reads after every verdict —
    /// the one cache the Account section's inherited-filters line counts, so
    /// both surfaces move together. `nil` for a bare view (no marks painted).
    private weak var client: FaunaClient?

    /// The ids of the rules the aftermath carried across that the owner has
    /// not answered — `filter-unattested-mark` + `filter-review-keep-button`
    /// render on exactly these rows.
    public var filterMarks: [Int64] { client?.inheritedFilterMarks ?? [] }
    /// Writeback so the per-platform app-state cache the e2e snapshot reads
    /// (`AppState.inboxMode` on iOS, `MacAppState.inboxMode` on macOS) tracks the
    /// mode the user picks here — mirrors linux, where the radio handler updates
    /// the local `settings::set_inbox_mode` cache the snapshot serializer reads
    /// (`apps/fauna-linux/src/settings/mod.rs`). Decoupled from the server
    /// round-trip: fired on both load and change. Optional (nil for surfaces that
    /// don't feed a snapshot, e.g. the macOS Moderation page is wired too but a
    /// bare `PrivacySettingsView()` stays valid).
    private var onInboxModeChanged: ((String) -> Void)?

    public init() {}

    public func configure(api: APIClient, actorId: String, client: FaunaClient? = nil,
                          onInboxModeChanged: ((String) -> Void)? = nil) {
        self.api = api
        self.actorId = actorId
        self.client = client
        self.onInboxModeChanged = onInboxModeChanged
    }

    public func loadAll() async {
        guard let api, let actorId else { return }
        // The Privacy visit re-reads the marks (linux's nav-edge re-read): a
        // mark raised after this session's last read must paint on arrival.
        await client?.reloadInheritedFilterMarks()
        do {
            let mode = try await api.getInboxMode(actorId: actorId)
            inboxMode = mode
            onInboxModeChanged?(mode)
        } catch {}
        do { emailFilters = try await api.listEmailFilters() } catch {}
        do {
            let prefs = try await api.getSpamPreferences()
            spamThreshold = prefs.spamThreshold
            phishingThreshold = prefs.phishingThreshold
        } catch {}
    }

    public func updateInboxMode(_ mode: String) async {
        guard let api, let actorId else { return }
        inboxModeLoading = true
        inboxModeError = nil
        defer { inboxModeLoading = false }
        do {
            try await api.setInboxMode(actorId: actorId, mode: mode)
            inboxMode = mode
            onInboxModeChanged?(mode)
        } catch {
            inboxModeError = DisplayError.http(error)
        }
    }

    public func createFilter(name: String, ruleType: String, ruleValue: String,
                      action: FfiFilterActionInputs) async {
        guard let api else { return }
        creatingFilter = true
        emailFilterError = nil
        defer { creatingFilter = false }
        do {
            // Single source of truth: the shared `fauna_protocol::email` encoder
            // (UniFFI free funcs) builds the typed rule/action from the whole
            // form inputs. Empty `rejectReason` → the canonical "Rejected by
            // filter" default; an unknown rule kind or an invalid Forward
            // destination throws (caught below) instead of a malformed rule.
            let rule = try encodeEmailFilterRule(kind: ruleType, value: ruleValue)
            let act = try encodeEmailFilterActionInputs(inputs: action)
            try await api.createEmailFilter(name: name, rules: [rule], combination: "all",
                                            action: act, priority: 0)
            emailFilters = try await api.listEmailFilters()
            showFilterForm = false
        } catch {
            emailFilterError = DisplayError.http(error)
        }
    }

    public func deleteFilter(id: Int64) async {
        guard let api else { return }
        let wasMarked = filterMarks.contains(id)
        do {
            try await api.deleteEmailFilter(id: id)
            emailFilters.removeAll { $0.id == id }
        } catch {
            emailFilterError = DisplayError.http(error)
            return
        }
        // `filter-delete` IS the Remove half of an inherited rule's review —
        // there is no second removal mechanism — so a marked row's verdict is
        // recorded here, strictly AFTER the deletion: a failure leaves a
        // re-asked question about a rule that is gone, never a silenced armed
        // rule. Best-effort for the same reason (the delete already landed).
        if wasMarked {
            do {
                _ = try await api.filterMarkRemoved(id: id)
            } catch {
                logMessage(level: .warn, target: "fauna.app",
                           message: "recording the inherited-rule removal failed: \(error)")
            }
            await client?.reloadInheritedFilterMarks()
        }
    }

    /// `filter-review-keep-button` — the owner recognises this inherited rule:
    /// it stays, and its mark (and its share of the Account line) clears.
    /// Surfaced rather than swallowed: a Keep that silently did not land leaves
    /// the owner believing they answered a question that will be asked again.
    public func keepFilterMark(id: Int64) async {
        guard let api else { return }
        emailFilterError = nil
        do {
            _ = try await api.filterMarkKeep(id: id)
        } catch {
            emailFilterError = DisplayError.http(error)
        }
        await client?.reloadInheritedFilterMarks()
    }

    /// Open the shared form pre-populated for editing `id` (`filter-edit`).
    /// Fetches fresh (not the cached row) via `filtersGet`, then decodes back
    /// to the create-dialog's `(kind, value)` pair and the whole action inputs
    /// (a Forward's destination and copy mode, a Reject's reason) via the
    /// shared `describeEmailFilterRule`/`describeEmailFilterActionInputs` — the
    /// exact reverse of `createFilter`'s encode. The caller's `filter-edit` gate
    /// (`emailFilterIsEditableFor`) should already guarantee a decodable shape;
    /// a `nil` here surfaces an error rather than opening a form that would
    /// silently narrow an unsupported filter on save.
    public func beginEditFilter(id: Int64) async -> (name: String, ruleType: String, ruleValue: String, action: FfiFilterActionInputs)? {
        guard let api else { return nil }
        emailFilterError = nil
        do {
            let filter = try await api.getEmailFilter(id: id)
            guard let rule = filter.rules.first,
                  let describedRule = describeEmailFilterRule(rule: rule),
                  let actionInputs = describeEmailFilterActionInputs(action: filter.action)
            else {
                emailFilterError = L.errors.httpError(detail: "unsupported filter shape")
                return nil
            }
            editingFilterId = id
            showFilterForm = true
            return (name: filter.name, ruleType: describedRule.kind,
                    ruleValue: describedRule.value, action: actionInputs)
        } catch {
            emailFilterError = DisplayError.http(error)
            return nil
        }
    }

    /// Submit an edit to `editingFilterId` (`save-filter`) — same
    /// kind/value/action composition as `createFilter`, single source of
    /// truth in the shared encoder.
    public func updateFilter(id: Int64, name: String, ruleType: String, ruleValue: String,
                      action: FfiFilterActionInputs) async {
        guard let api else { return }
        creatingFilter = true
        emailFilterError = nil
        defer { creatingFilter = false }
        do {
            let rule = try encodeEmailFilterRule(kind: ruleType, value: ruleValue)
            let act = try encodeEmailFilterActionInputs(inputs: action)
            try await api.updateEmailFilter(id: id, name: name, rules: [rule], combination: "all",
                                            action: act, priority: 0)
            emailFilters = try await api.listEmailFilters()
            showFilterForm = false
            editingFilterId = nil
        } catch {
            emailFilterError = DisplayError.http(error)
        }
    }

    public func saveSpamPreferences() async {
        guard let api else { return }
        spamPrefsLoading = true
        spamPrefsError = nil
        spamPrefsSaved = false
        defer { spamPrefsLoading = false }
        do {
            let prefs = SpamPreferences(
                spamThreshold: spamThreshold,
                phishingThreshold: phishingThreshold
            )
            try await api.updateSpamPreferences(prefs)
            spamPrefsSaved = true
        } catch {
            spamPrefsError = DisplayError.http(error)
        }
    }
}
