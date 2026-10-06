import SwiftUI

@MainActor @Observable
public class AccountSettingsVM {
    // Handle change
    public var newHandle = ""
    public var changingHandle = false
    public var changeHandleError: String?

    // Bluesky
    public var bskyAvailable = false
    public var bskyLinked = false
    public var bskyHandle = ""
    public var bskyInputHandle = ""
    public var bskyLoading = false
    public var bskyError: String?

    // Export
    public var exporting = false
    public var exportError: String?

    // Account deletion. Both exist because the confirm is now an ordinary inline
    // control rather than a system `.alert`: an alert dismissed itself on tap and
    // owned its own in-flight affordance, whereas an inline button stays on screen
    // and would otherwise accept a second click and fail silently. `deletingAccount`
    // is the in-flight latch windows already keeps ("the field reads exactly DELETE
    // AND no delete is already in flight" — `SettingsAccountPage.xaml.cs`), and
    // `deleteAccountError` is why a swallowed failure no longer looks like success.
    public var deletingAccount = false
    public var deleteAccountError: String?

    // Pending actions (`settings.md` § Pending actions) — the STANDING
    // account-page section listing the cancellable window the three delayed
    // verbs (handle change, account delete, snapshot delete) open. tui/linux/
    // web/android shipped this first; mirrors their shape. `nil` = not yet
    // hydrated (bare title); `[]` = hydrated and empty; non-empty = counted
    // title with rows — never a settled "nothing scheduled" claim before the
    // first list read lands.
    public var pendingActions: [FfiPendingActionSummary]?
    public var pendingActionsError: String?

    // iCloud Keychain backup (apple-only; apps/ios.md § Credential Storage). Default OFF
    // = device-bound: the identity secret never silently migrates via iCloud Keychain or an
    // encrypted-device restore. Toggling rewrites the keychain items (see
    // `KeychainStore.setICloudBackup`).
    public var iCloudBackupEnabled = false
    private let keychain = KeychainStore()

    private var api: APIClient?
    /// Every error banner here shares Account's `error-message` with the stolen
    /// ceremony's persist-failure message, so each error write goes through
    /// ``report(_:_:)``, which drops it while that message is pending
    /// (``StolenCeremonyHold``).
    private let ceremonyHold: StolenCeremonyHold

    public init(ceremonyHold: StolenCeremonyHold? = nil) {
        self.ceremonyHold = ceremonyHold ?? .shared
    }

    /// Write one of this page's error banners — unless it is an error and the
    /// stolen ceremony's persist-failure message is pending on the page
    /// (`settings.md` § Recovery kit → *The persist-failure message survives
    /// the page*). Every non-`nil` banner write in this view model goes through
    /// here; a clear is always admitted.
    func report(_ banner: ReferenceWritableKeyPath<AccountSettingsVM, String?>, _ text: String?) {
        guard ceremonyHold.admits(text) else { return }
        self[keyPath: banner] = text
    }

    /// Reflect the on-device preference into the toggle. Call when the Account view appears.
    public func loadICloudBackupState() {
        iCloudBackupEnabled = keychain.iCloudBackupEnabled()
    }

    /// Flip iCloud backup and re-read the persisted state, so the toggle always mirrors what
    /// actually landed in the keychain rather than an optimistic UI guess.
    public func setICloudBackup(_ enabled: Bool) {
        keychain.setICloudBackup(enabled: enabled)
        iCloudBackupEnabled = keychain.iCloudBackupEnabled()
    }

    public func configure(api: APIClient) {
        self.api = api
    }

    public func checkBlueskyStatus() async {
        guard let api else { return }
        do {
            // `fauna.bridges.list` over the unified bridges client; the Bluesky
            // entry carries `available`/`linked`/`identity` (identity.display is
            // the handle, e.g. "alice.bsky.social"). The bluesky-specific
            // `auth/status` HTTP twin was deleted nest-side.
            let bridges = try await api.listBridges()
            if let bsky = bridges.first(where: { $0.id == "bluesky" }) {
                bskyAvailable = bsky.available
                bskyLinked = bsky.linked
                bskyHandle = bsky.identity?.display ?? ""
            } else {
                bskyAvailable = false
            }
        } catch {
            bskyAvailable = false
        }
    }

    public func changeHandle(session: SessionState) async {
        guard let api, !newHandle.isEmpty else { return }
        let trimmed = newHandle.trimmingCharacters(in: .whitespaces)
        // Client-side format validation first (the shared
        // `fauna_protocol::handle::validate_handle` over UniFFI; nil ⇒ valid) —
        // a too-short/invalid handle surfaces in `error-message` without a
        // round-trip, the same shared validator + message the nest enforces and
        // every app shows (priority #3). Mirrors linux `settings/account.rs`.
        if let validationError = validateHandle(handle: trimmed) {
            report(\.changeHandleError, validationError)
            return
        }
        changingHandle = true
        changeHandleError = nil
        defer { changingHandle = false }
        do {
            // Discard the echoed new handle — the change has not applied yet
            // (`settings.md` § Pending actions' never-cache-the-echo trap).
            _ = try await api.changeHandle(newHandle: trimmed)
            newHandle = ""
            await loadPendingActions()
        } catch {
            // `message`, not `http`: the refusal is a boundary `FfiError` whose
            // text is already the nest's reason in the user's language (a taken
            // handle — `settings.md` § User actions), and `http` wrapped it as
            // "HTTP error: FfiError.General(msg: …)".
            report(\.changeHandleError, DisplayError.message(error))
        }
    }

    /// Fired on load, and after either delayed verb this screen hosts
    /// (`changeHandle`/`deleteAccount`) completes, and after a cancel — never
    /// trust a stale read.
    public func loadPendingActions() async {
        guard let api else { return }
        do {
            pendingActions = try await api.pendingActionsList()
            pendingActionsError = nil
        } catch {
            // The section keeps its bare title on a failed read — no basis
            // for any other claim.
            report(\.pendingActionsError, DisplayError.http(error))
        }
    }

    /// `pending-action-cancel-button` — one click, no confirm: cancelling is
    /// the safe direction. Ends on a fresh list read, never a local removal,
    /// so the row count always reflects the nest's own state.
    public func cancelPendingAction(id: Int64) async {
        guard let api else { return }
        do {
            try await api.pendingActionCancel(id: id)
            await loadPendingActions()
        } catch {
            report(\.pendingActionsError, DisplayError.http(error))
        }
    }

    public func linkBluesky() async {
        guard let api, !bskyInputHandle.isEmpty else { return }
        bskyLoading = true
        bskyError = nil
        do {
            // `fauna.bridges.link` with mode "oauth" + params {handle}; the reply
            // carries the OAuth `redirect_url` to open in the browser (the OAuth
            // callback stays HTTP — far end is Bluesky's OAuth server).
            let result = try await api.linkBridge(
                bridgeId: "bluesky", mode: "oauth",
                fields: ["handle": bskyInputHandle.trimmingCharacters(in: .whitespaces)])
            // Open OAuth URL in default browser
            if let redirect = result.redirectUrl, let url = URL(string: redirect) {
                OpenURL.open(url)
            }
        } catch {
            report(\.bskyError, DisplayError.http(error))
        }
        bskyLoading = false
    }

    public func unlinkBluesky() async {
        guard let api else { return }
        bskyLoading = true
        bskyError = nil
        do {
            // `fauna.bridges.unlink` (idempotent); the bluesky `auth` DELETE
            // HTTP twin was deleted nest-side.
            try await api.unlinkBridge(bridgeId: "bluesky")
            bskyLinked = false
            bskyHandle = ""
            bskyInputHandle = ""
        } catch {
            report(\.bskyError, DisplayError.http(error))
        }
        bskyLoading = false
    }

    public func exportData() async -> Data? {
        guard let api else { return nil }
        exporting = true
        exportError = nil
        defer { exporting = false }
        do {
            return try await api.exportData()
        } catch {
            report(\.exportError, DisplayError.http(error))
            return nil
        }
    }

    /// `fauna.account.delete` only SCHEDULES a 14-day cancellable pending
    /// action (`account-scoping.md` § *Erasure follows scope* — erasure binds
    /// at execution, when the account is actually gone, never at request
    /// time). The nest-side account is NOT gone yet, so this must not sign
    /// the user out, stop the runtime, or erase any credential/store: doing
    /// so strands the user outside the 14-day cancel window with only an
    /// exported identity secret as the way back in (`settings.md` § User
    /// actions, ruled 2026-08-26 — tui/linux/windows already only make the
    /// RPC call and leave everything else standing).
    public func deleteAccount() async -> Bool {
        guard let api else { return false }
        deletingAccount = true
        deleteAccountError = nil
        defer { deletingAccount = false }
        do {
            try await api.deleteAccount()
            await loadPendingActions()
            return true
        } catch {
            // Previously swallowed. A failed deletion left the old `.alert`
            // dismissed with nothing said, so the user read "nothing happened"
            // as "done" — the one reading that is never true. Web surfaces the
            // same failure (`doDeleteAccount`'s catch), and the page's own
            // `changeHandleError`/`exportError` banners are the shape.
            report(\.deleteAccountError, L.settings.errors.deleteAccount)
            return false
        }
    }
}
