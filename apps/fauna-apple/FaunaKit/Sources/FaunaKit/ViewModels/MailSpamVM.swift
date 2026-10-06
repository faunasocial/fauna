import SwiftUI

/// Shared view-model for the user-facing `mail-spam` page (macOS + iOS, one
/// FaunaKit VM). A thin proxy over the shared
/// `MailSpamMachine` (UniFFI): the machine owns the per-account spam-classifier
/// state + RPC orchestration (`reset_spam_model` / `set_contribute_baseline` /
/// the client-side undo / training-history pagination); this VM holds the latest
/// `MailSpamSnapshot` as `@Observable` state and re-reads it after each `hydrate`
/// / `dispatch`. The machine is **pull-based** (no observer callback, like
/// `MailAliasesMachine`), so re-assigning `snapshot` is what drives the SwiftUI
/// re-render.
///
/// **UI precedes backend:** the per-user Bayesian feedback loop is unbuilt today
/// (`mail-spam.md` § Implementation status today) — every action surfaces the
/// seam's honest `unimplemented` rejection on `snapshot.error`, which the view
/// shows via `error-message`. No fabricated training rows. Target behavior:
/// `docs/goal/behavior/mail-spam.md`; lead-client renderer is linux
/// (`apps/fauna-linux/src/settings/mail_spam.rs`).
/// `hydrate()`/`dispatch(_:)` come from `MachineBackedVM`'s shared default.
@MainActor @Observable
public final class MailSpamVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`. The view reads its fields
    /// (`events`, `contributeBaseline`, `error`).
    public internal(set) var snapshot: MailSpamSnapshot?
    /// Page-level error surface (`error-message`) — carries both connect/build
    /// failures and the machine's own `snapshot.error`.
    public var errorMessage: String?
    public internal(set) var isLoading = false

    var machine: MailSpamMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?

    /// Report sharing (`report-sharing.md` § Client wire + transparency
    /// surface) — a bool + read-only list over `FfiModerationClient` directly,
    /// **not** routed through `MailSpamMachine`/`MailSpamAction` (it is a
    /// distinct k-anonymity mechanism with no state machine of its own; mirrors
    /// linux `hydrate_report_share`/`set_report_share`, which likewise bypass
    /// `MailSpamMachine`).
    private var moderationClient: FfiModerationClient?
    /// Whether this actor opts in to report sharing. Default off; `nil` until
    /// the first `report_share.status` read.
    public private(set) var reportShare: Bool = false
    /// Exactly the ≥k aggregates this nest exports to peers — the transparency
    /// guarantee ("what this nest publishes"). Empty until hydrated or before
    /// any content clears the k floor.
    public private(set) var reportSharePublished: [FfiReportShareEntry] = []

    /// The per-account `mail-spam-threshold-override-input` draft text — always
    /// the last **nest-confirmed** value (never the in-progress keystroke, same
    /// non-optimistic shape as `reportShare` above); empty means "follow the
    /// admin default". Mirrors linux `render_threshold_override`.
    public private(set) var thresholdOverrideText: String = ""

    public init() {}

    /// Vend the machine from APIClient and load the first snapshot. Idempotent
    /// (the machine is built once).
    public func configure(api: APIClient) async {
        guard machine == nil || configuredApi !== api else { return }
        do {
            let m = try await api.mailSpamMachine()
            machine = m
            configuredApi = api
            snapshot = m.snapshot()
            moderationClient = try await api.moderationClient()
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
        await hydrateReportShare()
        await hydrateThresholdOverride()
    }

    /// Refresh the training history + contribution flag from the nest (page mount).
    public func hydrate() async { await hydrateFromMachine() }

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: MailSpamAction) async { await dispatchToMachine(action) }

    // MARK: - Report sharing (report-sharing.md § Client wire + transparency surface)

    /// Read the opt-in + published list (`fauna.moderation.report_share.status`,
    /// page mount). Mirrors linux `hydrate_report_share`.
    public func hydrateReportShare() async {
        guard let moderationClient else { return }
        do {
            let status = try await moderationClient.reportShareStatus()
            reportShare = status.share
            reportSharePublished = status.published
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Set the opt-in (`fauna.moderation.report_share.set`), then re-read
    /// status so the toggle + list reflect the persisted value — opting out
    /// withdraws this actor's reports, which may shrink the list. Mirrors
    /// linux `set_report_share`. Unlike `dispatch(_:)` above, SwiftUI's
    /// `Toggle` binding `set` closure fires only on real user interaction
    /// (never on a programmatic re-render), so this needs no linux-style
    /// `rs_syncing` echo-suppress flag.
    public func setReportShare(_ share: Bool) async {
        guard let moderationClient else { return }
        do {
            _ = try await moderationClient.reportShareSet(share: share)
            let status = try await moderationClient.reportShareStatus()
            reportShare = status.share
            reportSharePublished = status.published
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    // MARK: - Per-account spam-folder threshold override

    /// Read `spam_threshold_override_get` (page mount). Mirrors linux
    /// `hydrate_threshold_override`.
    public func hydrateThresholdOverride() async {
        guard let configuredApi else { return }
        do {
            let value = try await configuredApi.spamThresholdOverrideGet()
            thresholdOverrideText = value.map(String.init) ?? ""
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Parse the committed text (`parseCount` — empty or unparseable both
    /// yield `nil`, i.e. "follow the admin default"; `0` is a real setting,
    /// never collapsed into unset), set it, then re-read so the field reflects
    /// the **persisted** value, never the local keystroke. Mirrors linux
    /// `commit_threshold_override` — unconditional, always dispatches.
    public func commitThresholdOverride(text: String) async {
        guard let configuredApi else { return }
        let value = parseCount(input: text)
        do {
            let result = try await configuredApi.spamThresholdOverrideSet(value: value)
            thresholdOverrideText = result.map(String.init) ?? ""
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }
}
