import SwiftUI

/// Shared view-model for the flat admin **`admin-files`** page (macOS + iOS, one
/// FaunaKit VM). The WebDAV-enable sibling of `AdminCalendarVM`: a thin proxy over the
/// shared `WebdavPolicyMachine` (UniFFI, `libs/fauna-client-mail-settings::webdav_policy`).
/// The machine owns the hydrate (`get_mail_config` → `webdav_enabled`) + the
/// write-then-re-read (`set_webdav_enabled`); this VM holds the latest
/// `WebdavPolicySnapshot` as `@Observable` state and re-reads it after each `configure` /
/// `dispatch`. Pull-based (no observer callback), like `AdminCalendarVM` / `AdminMailVM`.
///
/// **No port knob** — WebDAV rides the shared DAV listener that the calendar page's
/// `admin-calendar-caldav-port-input` already governs, so unlike `AdminCalendarVM` there
/// is no `setCaldavPort` twin here (`admin.md` § Files — "No port field").
///
/// Target behavior: `docs/goal/behavior/admin.md` § Files + `webdav-server.md`
/// § Independent enablement point 1 (the admin shell is one of the two setters of
/// `set_webdav_enabled`; onboarding is the other). Page §/IDs: ui.yaml `admin-files`.
/// Reference renderers: linux (`settings/admin_files.rs`), android (`AdminFilesScreen.kt`).
/// `hydrate()`/`dispatch(_:)` come from `MachineBackedVM`'s shared default
/// (dedup — this also fixed a latent bug: the old
/// local `applySnapshot()` unconditionally overwrote `errorMessage` with
/// `snap.error`, clobbering a thrown-hydrate failure message back to nil
/// whenever the snapshot read itself succeeded with no `snapshot.error` of
/// its own — the same class of bug pass 68 fixed for `LinkedNestsVM`).
@MainActor @Observable
public final class AdminFilesVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`. The view reads `webdavEnabled` + `status`.
    public internal(set) var snapshot: WebdavPolicySnapshot?
    /// The single page error surface (`error-message`) — both connect/build failures and
    /// the machine's `snapshot.error` route here.
    public var errorMessage: String?
    public internal(set) var isLoading = false

    var machine: WebdavPolicyMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?

    public init() {}

    // `isBusy` (true while a hydrate/save round-trip is in flight, gating the
    // Toggle) comes from `MachineBackedVM`'s `MachineSnapshotWithWorkStatus`
    // extension — `WebdavPolicySnapshot` conforms in `MachineBackedVM.swift`.

    // MARK: - Lifecycle (pull-based; machine built once, hydrate on every appear)

    /// Vend the machine from APIClient and load the effective config. Idempotent.
    public func configure(api: APIClient) async {
        if machine == nil || configuredApi !== api {
            do {
                let m = try await api.webdavPolicyMachine()
                machine = m
                configuredApi = api
                snapshot = m.snapshot()
            } catch {
                errorMessage = DisplayError.message(error)
                return
            }
        }
        await hydrate()
    }

    /// Re-read the effective config (page mount / refresh).
    public func hydrate() async { await hydrateFromMachine() }

    public func refresh() async { await hydrate() }

    // MARK: - Deployment-wide toggle (dispatch-on-change)

    /// Flip the deployment-wide `webdav_enabled` master switch via
    /// `fauna.bridges.set_webdav_enabled` (Admin-class), then re-read persisted state.
    ///
    /// **Harmless-on**: enabling WebDAV deployment-wide exposes *nothing* by itself — the
    /// actual exposure gate is the user's per-set `folder-webdav-toggle` (default OFF).
    /// The MDA runs iff `mail || caldav || carddav || webdav`, so this starts the bridge
    /// (adding `/webdav` to the shared DAV listener); flipping all four off stops it.
    public func setWebdavEnabled(_ enabled: Bool) async {
        await dispatch(.setWebdavEnabled(enabled: enabled))
    }

    // MARK: - Plumbing

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: WebdavPolicyAction) async { await dispatchToMachine(action) }
}
