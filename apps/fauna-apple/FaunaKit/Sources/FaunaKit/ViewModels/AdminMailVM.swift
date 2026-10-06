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

    private var machine: MailPolicyMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?

    public init() {}

    /// True while a hydrate / save round-trip is in flight — gates every Save.
    public var isBusy: Bool {
        isLoading || (snapshot?.isWorking ?? false)
    }

    // MARK: - Lifecycle (pull-based; machine built once, hydrate on every appear)

    /// Vend the machine from APIClient and load the effective config. Idempotent.
    public func configure(api: APIClient) async {
        if machine == nil || configuredApi !== api {
            do {
                machine = try await api.mailPolicyMachine()
                configuredApi = api
                applySnapshot()
            } catch {
                errorMessage = DisplayError.message(error)
                return
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

    public func setMailEnabled(_ enabled: Bool) async {
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

    /// Publish the opt-in aggregate as the deployment baseline; the outcome lands
    /// in `snapshot.baselinePublishResult`.
    public func publishSpamBaseline() async { await dispatch(.publishSpamBaseline) }

    // MARK: - Plumbing

    private func dispatch(_ action: MailPolicyAction) async {
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
        snapshotVersion += 1
    }
}
