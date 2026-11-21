// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Shared view-model for the flat admin **`admin-calendar`** page (macOS + iOS,
/// one FaunaKit VM). The CalDAV-enable sibling of
/// `AdminMailVM`: a thin proxy over the shared `CaldavPolicyMachine` (UniFFI,
/// `libs/fauna-client-mail-settings::caldav_policy`). The machine owns the
/// hydrate (`get_mail_config` → `caldav_enabled`) + the write-then-re-read
/// (`set_caldav_enabled`); this VM holds the latest `CaldavPolicySnapshot` as
/// `@Observable` state and re-reads it after each `configure` / `dispatch`. The
/// machine is **pull-based** (no observer callback, like `AdminMailVM`), so
/// re-assigning `snapshot` drives the SwiftUI re-render.
///
/// Target behavior: `docs/goal/behavior/admin.md` § 8 Calendar +
/// `docs/goal/behavior/caldav-server.md` § Independent enablement (the admin
/// shell is the 3rd setter of `set_caldav_enabled`). Page §/IDs: ui.yaml
/// `admin-calendar`. Reference renderer: linux
/// (`apps/fauna-linux/src/settings/admin_calendar.rs`).
/// `hydrate()`/`dispatch(_:)` come from `MachineBackedVM`'s shared default
/// (dedup — this also fixed a latent bug: the old
/// local `applySnapshot()` unconditionally overwrote `errorMessage` with
/// `snap.error`, clobbering a thrown-hydrate failure message back to nil
/// whenever the snapshot read itself succeeded with no `snapshot.error` of
/// its own — the same class of bug pass 68 fixed for `LinkedNestsVM`).
@MainActor @Observable
public final class AdminCalendarVM: ReadGatedToggleVM {
    /// Latest snapshot; `nil` until `configure`. The view reads `caldavEnabled`
    /// + `status` from it.
    public internal(set) var snapshot: CaldavPolicySnapshot?
    /// The single page error surface (`error-message`) — both connect/build
    /// failures and the machine's `snapshot.error` route here.
    public var errorMessage: String?
    public internal(set) var isLoading = false
    /// The machine could not be built (``ReadGatedToggleVM``): the toggle
    /// says *failed to load* rather than waiting for a read that never comes.
    var machineBuildFailed = false

    var machine: CaldavPolicyMachine?
    /// The `APIClient` the cached ``machine`` was built from. A machine is bound to that
    /// client's nest + identity, so a session re-point (a different nest, or a different
    /// account on the same nest — the app vends a fresh `APIClient` for either) must
    /// re-vend it; a machine held across one keeps talking to the OLD box, succeeding
    /// against the wrong nest. Same guard in every machine-backed VM (see `MailSettingsVM`).
    private var configuredApi: APIClient?

    public init() {}

    // `isBusy` (true while a hydrate/save round-trip is in flight, gating the
    // Toggle) comes from `MachineBackedVM`'s `MachineSnapshotWithWorkStatus`
    // extension — `CaldavPolicySnapshot` conforms in `MachineBackedVM.swift`.

    // MARK: - Lifecycle (pull-based; machine built once, hydrate on every appear)

    /// Vend the machine from APIClient and load the effective config. Idempotent.
    public func configure(api: APIClient) async {
        if machine == nil || configuredApi !== api {
            do {
                let m = try await api.caldavPolicyMachine()
                machine = m
                machineBuildFailed = false
                configuredApi = api
                snapshot = m.snapshot()
            } catch {
                errorMessage = DisplayError.message(error)
                machineBuildFailed = true
                return
            }
        }
        await hydrate()
    }

    /// Re-read the effective config (page mount / refresh).
    public func hydrate() async { await hydrateFromMachine() }

    public func refresh() async { await hydrate() }

    // MARK: - Deployment-wide toggle (dispatch-on-change)

    public func setCaldavEnabled(_ enabled: Bool) async {
        await dispatch(.setCaldavEnabled(enabled: enabled))
    }

    // MARK: - Admin-set CalDAV listener port (gathered on Save)

    /// Set the admin CalDAV listener port via `fauna.bridges.set_caldav_port`
    /// (Admin-class), then re-read persisted state (`get_mail_config` →
    /// `caldav_port`). The MDA rebinds on the config change. The View validates the
    /// u16 range before calling this (invalid → `error-message`, no dispatch).
    public func setCaldavPort(_ port: UInt16) async {
        await dispatch(.setCaldavPort(port: port))
    }

    /// `admin-calendar-caldav-port-save-button`'s answer over the typed port:
    /// the page's read gate first, then the shared `port_draft_possibility` (a
    /// port in `[1, 65535]`; admin.md § Errors & edge cases, *A form's submit
    /// is live only for a draft the nest would accept*). The Save is painted
    /// from it and the press asks it again; a nest whose CalDAV is served behind
    /// its domain still saves (§ 8 Calendar).
    public func portSaveAnswer(_ draft: String) -> Possibility {
        toggleAnswer.and(caldavPortDraftPossibility(draft: draft))
    }

    // MARK: - Plumbing

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: CaldavPolicyAction) async { await dispatchGated(action) }
}
#endif // !FAUNA_KIDS
