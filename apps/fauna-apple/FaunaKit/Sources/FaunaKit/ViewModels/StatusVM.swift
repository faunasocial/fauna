import SwiftUI
import SwiftData

/// The app's own i18n catalog handed to `statusText` — `L.lookup` returns the
/// key itself when missing, which is exactly the fallback the shared projection
/// would apply, so a catalog miss surfaces as the raw key rather than as blank.
private final class StatusCatalogLookup: FfiStatusLookup {
    func lookup(key: String) -> String? { L.lookup(key) }
}

@MainActor @Observable
public class StatusVM {
    public var quota: QuotaResponse?
    /// The `feature-limits-section` read (`dynamic-features.md` § Transparency
    /// & auditability) — `nil` until the fetch resolves, matching `quota`'s
    /// hydrate-gates-paint contract (the Status views render nothing for the
    /// section until this is non-nil).
    public var featureRows: [FfiFeatureRow]?
    /// The Status snapshot's node / sync / MLS legs as loaded so far
    /// (`docs/goal/ui/status.md` § State & data shape) — a `nil` leg is not
    /// loaded or not applicable here (iOS has no local sync agent), and its
    /// section is not painted. Read the finished element texts through
    /// ``sectionText(now:)``; this VM derives none of them.
    public private(set) var statusLegs = FfiStatusLegs(node: nil, sync: nil, mls: nil)
    public var isLoading = false
    public var errorMessage: String?

    private var api: APIClient?

    public init() {}

    /// `fauna_client_status::render` over the loaded legs — the seven `status-*`
    /// element texts, resolved through the app's own catalog. The build leg is
    /// the commit the shared Rust was stamped with (`build.rs`).
    public func sectionText(now: Date = Date()) -> FfiStatusText {
        statusText(
            legs: statusLegs,
            nowMs: Int64(now.timeIntervalSince1970 * 1000),
            lookup: StatusCatalogLookup())
    }

    /// The node and MLS legs — the two nest reads the landing needs. Each leg
    /// lands as it resolves; a read that fails leaves its leg `nil` (the
    /// section is simply not painted — an unreachable nest is `connection-status`'s
    /// to say, not a zero here). `secureChannelCount` is the conversations
    /// manager's own count (`ConversationsManager.secureChannelCount()`), read
    /// on the main actor.
    public func fetchStatusLegs(
        actorId: String?, secureChannelCount: @MainActor () -> UInt64
    ) async {
        guard let api else { return }
        if let node = try? await api.statusClient().node() {
            guard self.api === api else { return }   // the in-flight clause
            statusLegs.node = node
        }
        if let actorId, let count = try? await api.getKeyPackageCount(actorId: actorId) {
            guard self.api === api else { return }
            statusLegs.mls = FfiMlsLeg(
                keyPackages: UInt64(max(count, 0)), channels: secureChannelCount())
        }
    }

    /// The sync leg, from the local agent's per-device signal — desktop only.
    /// `nil` (the agent has not answered) removes the section rather than
    /// painting a zero backlog.
    public func setSyncLeg(_ leg: FfiStatusSyncLeg?) {
        statusLegs.sync = leg
    }

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary), on `SearchVM.reset()`'s shape. Called by
    /// ``configure(api:)`` on an api-identity change **before** it re-points, and by
    /// the page's nil-client phase.
    ///
    /// iOS hosts this on the Settings **root**, which the switch teardown does NOT
    /// unmount — it clears `selectedSettingsPage`, which pops the value-based
    /// sub-pages, while `moreSelectedView` stays `"settings"`. So without this the
    /// outgoing account's storage quota and feature limits stay rendered under the
    /// incoming one . (macOS's `MacStatusView` sits inside
    /// the window shell the teardown unmounts wholesale, so there it is redundant —
    /// carried for uniformity, as `SearchVM`'s is.)
    ///
    /// ``signOut(sessionState:)`` is deliberately not part of this: it is an action,
    /// not state, and it reads no field this drops.
    public func reset() {
        api = nil
        quota = nil
        featureRows = nil
        statusLegs = FfiStatusLegs(node: nil, sync: nil, mls: nil)
        isLoading = false
        errorMessage = nil
    }

    public func configure(api: APIClient) {
        if let current = self.api, current !== api { reset() }
        self.api = api
    }

    public func fetchQuota() async {
        guard let api else { return }
        isLoading = true
        defer { isLoading = false }

        do {
            let loaded = try await api.fetchQuota()
            // The in-flight clause: a read suspended for the outgoing account still
            // returns after the drop, so its numbers must not land on the new page.
            guard self.api === api else { return }
            quota = loaded
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    /// `FfiFeaturesClient.rows()` — the whole feature-limits surface, ready to
    /// render (the row shape, per-cell tier attribution, and two-level
    /// magnitude composition are all `fauna_client_features`' output; this
    /// call decides nothing). Mirrors `fetchQuota`'s shape.
    public func fetchFeatureLimits() async {
        guard let api else { return }
        do {
            let loaded = try await api.featuresClient().rows()
            guard self.api === api else { return }   // the in-flight clause
            featureRows = loaded
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    /// Both fetches, in the order every consumer (macOS's `MacStatusView`, iOS's
    /// `SettingsView`) always calls them — initial load and WS-RPC reconnect alike.
    public func refreshQuotaAndLimits() async {
        await fetchQuota()
        await fetchFeatureLimits()
    }

    /// Sign out = erase the **whole credential namespace**, every account of it — not the
    /// active identity's two keys (`docs/goal/architecture/long-term-store.md`
    /// § Cleanup contract: *"Under multi-account, 'all three slots' means the whole
    /// credential namespace"*). Removing a single identity while staying signed in is a
    /// different affordance — the switcher's per-row remove (`AccountRegistry::remove`).
    ///
    /// Deleting only `secret_key` + `device_id`, as this used to, would leave two rows the
    /// registry cutover made load-bearing, and each is its own bug:
    ///   - a surviving `fauna/{actor}/secret` means the user's private key is still on the
    ///     device after "Sign Out" — they were not signed out;
    ///   - a surviving `fauna/index` still names the signed-out account *active*, so the
    ///     next boot routes on it — silently signing them back in as the identity they
    ///     just left, and shadowing any identity onboarded since.
    ///
    /// `clearAll()` is delete-only, so a crash mid-wipe cannot resurrect the identity being
    /// erased.
    ///
    /// And erasure follows SCOPE, not just the credential namespace
    /// (`account-scoping.md` § The scoping taxonomy, *Erasure follows scope*): every
    /// account-scoped store goes with it — the MLS conversation state above all. Leaving
    /// `Fauna/<actor>/mls.db` behind after "Sign Out" is the same class of bug as leaving
    /// `fauna/{actor}/secret` behind: the user asked for their data off this device, and
    /// what stays is decryptable conversation history. Install-scoped state (device UI
    /// preferences, logs, host-keyed TOFU pins) survives, by the same taxonomy.
    ///
    /// ⚠ **The account runtime is stopped FIRST, and the order is the whole
    /// point.** It is a process global that keeps pumping until told otherwise
    /// (`account-data-plane.md` § The account store → *The client-side
    /// lifecycle*), so erasing the store out from under a live runtime lets the
    /// next pass re-create it — and the credential wipe on the line above has
    /// already taken its Ed25519 writer key with it, so the resurrected store
    /// refuses every later sign-in ("account store belongs to a different
    /// writer") and the app runs with no account runtime, silently, for good.
    /// `stopAccountRuntimeForSignOut` awaits the in-flight pass, so by the time
    /// the erase runs nothing is still writing — and, first, retires this
    /// machine's enrollment nest-side while the runtime still holds the writer
    /// key the erase below is about to take (`sync-agent-credentials.md`
    /// § Credential model → *The signed-out reconcile*). (The shell's own
    /// teardown stops the runtime too, via `FaunaClient.shutdown()` — the
    /// switch-shaped stop, a safe no-op here since the runtime is already
    /// stopped — but that closure runs *after* this one returns, which is
    /// exactly one pass too late for either shape.)
    ///
    /// ⚠ `releaseAccountScopedStores` runs SECOND, right before the erase —
    /// stopping the account runtime does not touch the conversations engine,
    /// which keeps its own SQLite connection and role lock open
    /// (`account-scoping.md` § Erasure follows scope — *An OPEN store is an
    /// unerasable store*) until this hands it over explicitly. POSIX `unlink`
    /// tolerates the still-open handle, so skipping this leaves the engine
    /// writing to an unreachable inode rather than causing a visible failure
    /// here — the same uniformity debt windows' `os error 32` made visible.
    public func signOut(sessionState: SessionState) async {
        await api?.stopAccountRuntimeForSignOut()
        await api?.releaseAccountScopedStores()
        // What still reads back after the credential erase — the only witness this
        // seat has, since `KeychainStore`'s delete drops its `OSStatus`. It is
        // recorded beside the filesystem sweep: a sign-out that recorded only the
        // sweep read as clean over a keychain still holding the identity seed
        // (`account-scoping.md` § Erasure follows scope). The record is kept under
        // the install base, so a relaunch still tells the user (§ *the residue
        // surface*).
        let credentials = FaunaAccounts.registry().clearAll()
        let sweep = AccountStateDir.eraseAll()
        sessionState.signOutResidue = SignOutResidueSurface.record(
            sweep: sweep, credentials: credentials)
        sessionState.clearAuthenticatedOverride()
        sessionState.secretHex = nil
        sessionState.actorId = nil
        sessionState.nodeUrl = nil
        sessionState.deviceId = nil
    }

    public func clearCache() {
        let cacheDir = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first?
            .appendingPathComponent("fauna")
        if let dir = cacheDir {
            try? FileManager.default.removeItem(at: dir)
        }
    }
}
