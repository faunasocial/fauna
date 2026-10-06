import SwiftUI

/// Shared view-model for the flat admin **`admin-contacts`** page (macOS + iOS, one
/// FaunaKit VM). The CardDAV-enable sibling of `AdminCalendarVM`: a thin proxy over the
/// shared `CarddavPolicyMachine` (UniFFI, `libs/fauna-client-mail-settings::carddav_policy`).
/// The machine owns the hydrate (`get_mail_config` → `carddav_enabled`) + the
/// write-then-re-read (`set_carddav_enabled`); this VM holds the latest
/// `CarddavPolicySnapshot` as `@Observable` state and re-reads it after each `configure` /
/// `dispatch`. Pull-based (no observer callback), like `AdminCalendarVM` / `AdminFilesVM`.
///
/// **No port knob** — CardDAV rides the shared DAV listener that the calendar page's
/// `admin-calendar-caldav-port-input` already governs, so unlike `AdminCalendarVM` there
/// is no `setCaldavPort` twin here (`admin.md` § Contacts — "No port field").
///
/// Target behavior: `docs/goal/behavior/admin.md` § Contacts + `carddav-server.md`
/// § Independent enablement (the admin shell is one of the two setters of
/// `set_carddav_enabled`; onboarding is the other). Page §/IDs: ui.yaml `admin-contacts`.
/// Reference renderers: linux (`settings/admin_contacts.rs`), android (`AdminContactsScreen.kt`).
/// `hydrate()`/`dispatch(_:)` come from `MachineBackedVM`'s shared default
/// (dedup — this also fixed a latent bug: the old
/// local `applySnapshot()` unconditionally overwrote `errorMessage` with
/// `snap.error`, clobbering a thrown-hydrate failure message back to nil
/// whenever the snapshot read itself succeeded with no `snapshot.error` of
/// its own — the same class of bug pass 68 fixed for `LinkedNestsVM`).
@MainActor @Observable
public final class AdminContactsVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`. The view reads `carddavEnabled` + `status`.
    public internal(set) var snapshot: CarddavPolicySnapshot?
    /// The single page error surface (`error-message`) — both connect/build failures and
    /// the machine's `snapshot.error` route here.
    public var errorMessage: String?
    public internal(set) var isLoading = false

    var machine: CarddavPolicyMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?

    public init() {}

    // `isBusy` (true while a hydrate/save round-trip is in flight, gating the
    // Toggle) comes from `MachineBackedVM`'s `MachineSnapshotWithWorkStatus`
    // extension — `CarddavPolicySnapshot` conforms in `MachineBackedVM.swift`.

    // MARK: - Lifecycle (pull-based; machine built once, hydrate on every appear)

    /// Vend the machine from APIClient and load the effective config. Idempotent.
    public func configure(api: APIClient) async {
        if machine == nil || configuredApi !== api {
            do {
                let m = try await api.carddavPolicyMachine()
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

    /// Flip the deployment-wide `carddav_enabled` master switch via
    /// `fauna.bridges.set_carddav_enabled` (Admin-class), then re-read persisted state.
    ///
    /// The MDA runs iff `mail || caldav || carddav || webdav`, so this starts the bridge
    /// (adding `/carddav` to the shared DAV listener); flipping all four off stops it.
    public func setCarddavEnabled(_ enabled: Bool) async {
        await dispatch(.setCarddavEnabled(enabled: enabled))
    }

    // MARK: - Plumbing

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: CarddavPolicyAction) async { await dispatchToMachine(action) }
}
