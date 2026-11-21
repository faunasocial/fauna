// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Shared view-model for the flat admin **`admin-mail`** policy page (macOS + iOS,
/// one FaunaKit VM). A thin proxy over the shared
/// `MailPolicyMachine` (UniFFI): the machine owns the effective-config hydrate +
/// the per-sub-struct full-PUT save sequencing (re-reading after each write); this
/// VM holds the latest `MailPolicySnapshot` as `@Observable` state and re-reads it
/// after each `hydrate` / `dispatch`. The machine is **pull-based** (no observer
/// callback, like `ForwardersVM` / `AdminDnsVM`), so re-assigning `snapshot` drives
/// the SwiftUI re-render.
///
/// `MailPolicyMachine::refresh()` issues **three** admin reads — `get_mail_config`
/// for the five projected groups + the mail-enable toggle, the separate
/// `get_alias_policy` twin for the alias group, and `fauna.setup.status` for the
/// new-user auto-enable policy (the latter two are not in `FetchConfigReply`). All
/// of that lives in the shared machine; this VM just surfaces the snapshot.
///
/// Target behavior + the policy catalog: `docs/goal/behavior/mail-policy-config.md`
/// § Policy catalog (Tier 2) + § Implementation status today. Page §/IDs:
/// `docs/goal/behavior/admin.md` § 6 Mail + `tests/e2e-unified/ui.yaml` `admin-mail`.
/// Reference renderer: linux (`apps/fauna-linux/src/settings/admin_mail.rs`).
@MainActor @Observable
public final class AdminMailVM {
    /// Latest snapshot; `nil` until `configure`. The view reads the seven policy
    /// groups + the two deployment-wide toggles + `status` from it.
    public private(set) var snapshot: MailPolicySnapshot?
    /// The single page error surface (`error-message`) — both connect/build
    /// failures and the machine's `snapshot.error` (e.g. an out-of-order
    /// spam-threshold write rejected `fauna.protocol.malformed`) route here.
    public var errorMessage: String?
    public private(set) var isLoading = false

    /// Bumped on every snapshot re-read so the per-group form sections re-seed
    /// their edit buffers from the freshly persisted values (mirrors linux's
    /// full re-render on every dispatch). Keyed by the sections' `.task(id:)`.
    public private(set) var snapshotVersion = 0

    /// The answer every control that writes what the policy read decides is
    /// painted from — the mail-enable and new-user auto-enable toggles, every
    /// group's save, the standing-publish switch, the Microsoft save and the
    /// rotate and publish confirms: the shared
    /// `MailPolicyMachine::write_possibility` (admin.md § Errors & edge cases,
    /// *Controls wait for their read*). Still loading before the machine
    /// exists, failed to load when it could not be built. Re-read with every
    /// snapshot; a press asks the machine live.
    public private(set) var writeAnswer: Possibility = readPossibility(read: .pending)

    private var machine: MailPolicyMachine?
    private var buildFailed = false
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?

    public init() {}

    /// True while a hydrate / save round-trip is in flight — gates every Save.
    /// Whether the read this page's read-gated controls wait for has resolved
    /// (``AdminPageReading``): the shared `MailPolicyMachine::read_state`. An unanswered one is asked again when the
    /// nest connection comes back.
    public var readAnswered: Bool { machine?.readState() == .resolved }

    public var isBusy: Bool {
        isLoading || (snapshot?.isWorking ?? false)
    }

    // MARK: - Lifecycle (pull-based; machine built once, hydrate on every appear)

    /// Vend the machine from APIClient and load the effective config. Idempotent.
    public func configure(api: APIClient) async {
        if machine == nil || configuredApi !== api {
            let built: MailPolicyMachine
            do {
                built = try await api.mailPolicyMachine()
            } catch {
                // A concurrent build for this api that succeeded owns the page.
                if machine == nil || configuredApi !== api {
                    errorMessage = DisplayError.message(error)
                    buildFailed = true
                    writeAnswer = livePossibility
                }
                return
            }
            // Re-entrant `configure`: the build that finishes first wins; a
            // later one for the same api hydrates it, never replaces it (same
            // shape as `LabelerCatalogVM`). A re-point to another api still
            // re-vends.
            if machine == nil || configuredApi !== api {
                machine = built
                configuredApi = api
                applySnapshot()
            }
        }
        await hydrate()
    }

    /// Re-read the effective config (page mount / refresh).
    public func hydrate() async {
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        do { try await machine.hydrate() }
        catch { errorMessage = DisplayError.message(error) }
        applySnapshot()
    }

    public func refresh() async { await hydrate() }

    // MARK: - Deployment-wide toggles (dispatch-on-change)

    /// `admin-mail-enabled-toggle`'s answer — the shared
    /// `MailPolicySnapshot::enable_toggle_possibility` (`admin.md` § Errors &
    /// edge cases, *Role and state gates answer one `Possibility` too*): turning
    /// mail on is blocked by the `admin-mail-unavailable-notice` sentence on a
    /// box that cannot enable it; mail already on stays possible so it can be
    /// turned off. `nil` snapshot = the read has not landed.
    public var enableTogglePossibility: Possibility {
        guard let snapshot else { return readPossibility(read: .pending) }
        return mailEnableTogglePossibility(snapshot: snapshot)
    }

    public func setMailEnabled(_ enabled: Bool) async {
        // The press asks what the paint asked (rule 7): a turn-on the box
        // cannot take is refused with the notice's own sentence.
        if enabled, refusePress(enableTogglePossibility, errorMessage: &errorMessage) { return }
        await dispatch(.setMailEnabled(enabled: enabled))
    }

    public func setAutoEnableMailForNewUsers(_ enabled: Bool) async {
        await dispatch(.setAutoEnableMailForNewUsers(enabled: enabled))
    }

    // MARK: - Per-sub-struct full-PUT saves

    public func saveSpam(_ policy: SpamPolicyView) async { await dispatch(.saveSpam(policy: policy)) }
    public func saveAuth(_ policy: AuthPolicyView) async { await dispatch(.saveAuth(policy: policy)) }
    public func saveSubmission(_ policy: SubmissionPolicyView) async {
        await dispatch(.saveSubmission(policy: policy))
    }
    public func saveImap(_ policy: ImapPolicyView) async { await dispatch(.saveImap(policy: policy)) }
    public func saveOutbound(_ policy: OutboundPolicyView) async {
        await dispatch(.saveOutbound(policy: policy))
    }
    public func saveAlias(_ policy: AliasPolicyView) async {
        await dispatch(.saveAlias(policy: policy))
    }

    public func saveDmarc(_ policy: DmarcPolicyView) async { await dispatch(.saveDmarc(policy: policy)) }

    /// `admin-mail-microsoft-save-button`: the two drafts go out as typed; the
    /// shared machine checks them (both blank clears) and refuses a bad pair onto
    /// the error banner before any round trip.
    public func saveMicrosoftRegistration(clientId: String, tenant: String) async {
        await dispatch(.setMicrosoftRegistration(clientId: clientId, tenant: tenant))
    }

    /// The standing-publish switch — its own gesture and its own dispatch, off the
    /// PERSISTED group, never a Spam-group draft (mail-spam.md § Cold start Path 2
    /// → *Standing publish*).
    public func setBaselineStandingPublish(_ enabled: Bool) async {
        await dispatch(.setBaselineStandingPublish(enabled: enabled))
    }

    // MARK: - Mail health readout (read-only section + its two buttons)

    /// A fresh blocklist self-check + diagnostics run, then a re-read of the
    /// readout (mail-deliverability.md § The mail health readout).
    public func recheckHealth() async { await dispatch(.recheckHealth, gated: false) }

    /// One press of a confirm button — the shared machine holds the same-button
    /// two-click: the first press arms it (`snapshot.armed`), the second
    /// dispatches the guarded action (the baseline publish's outcome lands in
    /// `snapshot.baselinePublishResult`), and any re-read disarms. The view only
    /// relabels the armed button; nothing is armed on the Swift side.
    public func press(_ button: MailPolicyConfirm) async {
        await dispatch(.press(button: button))
    }

    // MARK: - Plumbing

    /// The write answer asked of the machine NOW (the press's half of rule 7).
    private var livePossibility: Possibility {
        machinePossibility(machine, buildFailed: buildFailed) { $0.writePossibility() }
    }

    /// Run one action. Every action but the health re-check writes what the
    /// policy read decides, so it is refused — reason on `error-message`,
    /// nothing sent — while the write answer is not possible.
    private func dispatch(_ action: MailPolicyAction, gated: Bool = true) async {
        if gated, refusePress(livePossibility, errorMessage: &errorMessage) { return }
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        do { try await machine.dispatch(action: action) }
        catch { /* the machine records the error on the snapshot below */ }
        applySnapshot()
    }

    /// Re-read the machine snapshot, route `snapshot.error` → `errorMessage`, and
    /// bump `snapshotVersion` so the form sections re-seed.
    private func applySnapshot() {
        guard let machine else { return }
        let snap = machine.snapshot()
        snapshot = snap
        if let e = snap.error { errorMessage = e } else { errorMessage = nil }
        writeAnswer = machine.writePossibility()
        snapshotVersion += 1
    }
}
#endif // !FAUNA_KIDS
