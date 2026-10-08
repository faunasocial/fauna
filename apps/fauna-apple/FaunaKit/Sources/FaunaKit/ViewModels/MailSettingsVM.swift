import SwiftUI

/// Shared view-model for the user-facing `mail-settings` page (macOS + iOS, one
/// FaunaKit VM). A thin proxy over the shared
/// `MailSettingsMachine` (UniFFI): the machine owns all state, RPC orchestration,
/// rotation resume, and MSEK persistence; this VM holds the latest
/// `MailSettingsSnapshot` as `@Observable` state and re-reads it after each
/// `hydrate` / `dispatch`. The machine is **pull-based** (no observer callback,
/// unlike `OnboardingMachine`), so there is no observer-box — re-assigning
/// `snapshot` is what drives SwiftUI re-render. Target state:
/// `docs/goal/ui/mail-settings.md`; behavior: `docs/goal/behavior/mail-credentials.md`.
@MainActor @Observable
public final class MailSettingsVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`. The view reads its fields
    /// (`enabled`, `credentials`, `mua`, `status`, `pendingRotation`, …).
    public internal(set) var snapshot: MailSettingsSnapshot?
    /// Page-level error surface (`error-message`) — carries both connect/build
    /// failures and the machine's own `snapshot.error`.
    public var errorMessage: String?
    public internal(set) var isLoading = false

    var machine: MailSettingsMachine?
    /// The `APIClient` the cached ``machine`` was built from. A machine is bound for
    /// life to the nest + identity of the `APIClient` that vended it, so caching it
    /// on `machine == nil` alone silently outlives a session change: the app builds a
    /// **fresh** `APIClient` whenever the session is re-pointed (a different nest, or
    /// a different account on the same nest), and a machine held across that keeps
    /// talking to the OLD nest — succeeding, against the wrong box. Re-vend whenever
    /// the identity differs (`!==` — reference identity covers both axes at once).
    private var configuredApi: APIClient?
    /// Logged-in handle, substituted into each credential's `mua_username`
    /// template via the shared `resolveMuaUsername` (priority #2).
    private var handle: String = ""

    public init() {}

    public var enabled: Bool { snapshot?.enabled ?? false }

    /// CalDAV (calendar) enablement — independent of `enabled` (email). Gates only the
    /// CalDAV *connection-detail rows* in the MUA block; the page's shared surfaces gate
    /// on `credentialManagementReachable` below. Mirrors the linux `snap.caldav_enabled`
    /// / windows `snap.caldavEnabled` reads.
    public var caldavEnabled: Bool { snapshot?.caldavEnabled ?? false }

    /// Whether the credential-management surfaces are reachable —
    /// `enabled || caldav_enabled || carddav_enabled || serves_webdav_set`, computed
    /// **once in shared Rust** and simply READ here: every app reads the field rather
    /// than re-deriving the disjunction, so a future DAV sibling widens it in one place
    /// (`mail-settings.md` § Credential-management reachability; priority #2/#4).
    ///
    /// Every protocol rides the one shared MSEK + `default` bridge credential, so an
    /// actor reaching Fauna over *any* of them needs this page's credential section.
    public var credentialManagementReachable: Bool {
        snapshot?.credentialManagementReachable ?? false
    }

    /// Whether THIS actor serves ≥1 folder over WebDAV — the per-actor serve state that
    /// gates the `mail-settings-mua-webdav-url` row. Deliberately NOT the deployment-wide
    /// `webdav_enabled` toggle, which defaults ON for a real-domain box (so gating on it
    /// would show a dead mount URL to every actor) and is `Admin`-only to read, while this
    /// is a `User`-class page (`mail-settings.md` § WebDAV files).
    public var servesWebdavSet: Bool { snapshot?.servesWebdavSet ?? false }

    /// Vend the machine from APIClient and load the latest snapshot. The machine
    /// is built once (idempotent); **`hydrate()` runs on every call** so the page
    /// re-fetches the `fauna.state.mail` entries each time it (re)appears — the view drives this
    /// from `.task`, which SwiftUI re-runs on every appear. This refresh-on-show
    /// is load-bearing: a mail-state change made *after* the first build —
    /// notably the background non-admin first-setup auto-mint
    /// (`provision_mail_at_first_setup`, which mints on a *separate* machine
    /// instance) — is invisible to a stale build-time snapshot until the page
    /// re-fetches; without it the auto-minted credential and its one-time
    /// generated password never surface in the minting session
    /// (`mail-credentials.md` § Auto-enable). Mirrors the linux `connect_map`
    /// refresh-on-show (`apps/fauna-linux/src/settings/mail.rs`).
    public func configure(api: APIClient, handle: String) async {
        self.handle = handle
        if machine == nil || configuredApi !== api {
            do {
                let m = try await api.mailSettingsMachine()
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

    /// Refresh from the nest (page mount + after the account-plane watcher fires).
    /// Shared plumbing — `MachineBackedVM.hydrateFromMachine()`.
    public func hydrate() async { await hydrateFromMachine() }

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: MailSettingsAction) async {
        // No machine ⇒ `configure` never ran (its view's `.task` guard-returns when the
        // app has no `FaunaClient`) or it threw. Either way the user's click does
        // NOTHING, and returning quietly renders that as a no-op behind an empty error
        // banner — the failure mode that made this page's mint look like a nest bug for
        // two sessions. A gesture that cannot reach the nest must SAY so.
        guard let machine else {
            errorMessage = L.errors.notConnectedToNest
            return
        }
        isLoading = true
        defer { isLoading = false }
        // A throw here must never be silent: the machine records most failures on the
        // snapshot, but a transport-level throw leaves `snapshot.error` nil, so
        // discarding it (`try?`) rendered a failed enable/add as a no-op with an empty
        // error banner — the mint just never happened and nothing said why.
        var thrown: Error?
        // The status line paints the machine's LIVE status (`mail-settings.md`
        // § Status indicator), so "Syncing mail credentials…" and the rotate
        // button's disabled state show while the change is still running. A
        // snapshot folded only when the dispatch returns would never show them,
        // so re-read it on a short tick for the dispatch's whole life.
        let livePoll = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 150_000_000)
                if Task.isCancelled { break }
                self?.snapshot = machine.snapshot()
            }
        }
        do { try await machine.dispatch(action: action) }
        catch { thrown = error }
        livePoll.cancel()
        let snap = machine.snapshot()
        snapshot = snap
        errorMessage = snap.error ?? thrown.map { "\($0)" }
    }

    // The credentials list's row reads (the secret reveal, the substituted MUA
    // login) moved with the rows to the Connected apps page (`ConnectedAppsVM`,
    // mail-settings.md § Where the credential rows render): the machine's own
    // `revealCredentialSecret` is reached there through `ConnectedAppsMachine`.
}
