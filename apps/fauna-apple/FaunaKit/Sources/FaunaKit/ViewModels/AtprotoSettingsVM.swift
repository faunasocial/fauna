import SwiftUI

/// Thin SwiftUI-friendly proxy over the page-level `AtprotoSettingsMachine`
/// (UniFFI, `libs/fauna-atproto-settings-machine` via `libs/fauna-ffi`) — backs
/// the shared **Bluesky** settings sub-page (`docs/goal/ui/atproto.md`), macOS +
/// iOS. Mirrors `LabelerCatalogVM`'s observer-box pattern:
///   1. builds + owns the machine instance (over the session `FfiNestClient`),
///   2. implements `AtprotoSettingsObserver` to translate machine
///      notifications into `@Observable` invalidations on the main actor,
///   3. exposes the latest `AtprotoSettingsSnapshot` + gesture wrappers.
///
/// **Reveal/mint secrets never ride the snapshot** (D3 — the machine's own
/// contract) — this VM keeps them in a local, per-session, never-persisted
/// dictionary keyed by `credentialId`, exactly like linux's
/// `settings/atproto.rs` `ctx.revealed`.
@MainActor @Observable
public final class AtprotoSettingsVM {
    /// `nil` until `configure` succeeds.
    public private(set) var machine: AtprotoSettingsMachine?

    /// One-time connect/build failure (`api.atprotoSettingsMachine` threw).
    /// Page read/write failures live on the machine snapshot's `error` instead;
    /// both are surfaced through `errorMessage`.
    public private(set) var connectError: String?

    /// Secrets revealed (by mint or explicit reveal) this session, keyed by
    /// `credentialId`. Never persisted, never part of the snapshot.
    public private(set) var revealedSecrets: [String: String] = [:]

    private let observerBox = AtprotoSettingsObserverBox()

    #if DEBUG
    /// The live instance the AT Protocol page is currently rendering off, for the
    /// `atproto_delegation_advance_clock` TestAgent command.
    ///
    /// The page's VM is view-local (`@State` in `AtprotoSettingsView`), unlike
    /// the app-level `feedVM` the feed's inject command reaches directly — so
    /// the agent needs a late-bound handle, the same shape `observerBox.target`
    /// already uses. **Weak**: the slot must never keep a dismissed page's VM
    /// alive, and a command arriving with no page open must fail loudly rather
    /// than mutate a dead one (convention 11).
    ///
    /// `#if DEBUG` per convention 15 — the automation surface is compiled out of
    /// release artifacts, never merely gated at runtime.
    @MainActor
    public static weak var liveInstanceForTest: AtprotoSettingsVM?
    #endif

    public init() {}

    /// Vend the machine from `APIClient` and load the first snapshot. Idempotent
    /// — the machine is built once per VM instance; later calls only re-`refresh()`.
    public func configure(api: APIClient) async {
        #if DEBUG
        // Late-bind on every configure (not just the first): re-entering the
        // page builds a fresh VM, and the agent must reach THAT one.
        Self.liveInstanceForTest = self
        #endif
        guard machine == nil else {
            await refresh()
            return
        }
        observerBox.target = self
        do {
            machine = try await api.atprotoSettingsMachine(observer: observerBox)
        } catch {
            connectError = DisplayError.message(error)
            return
        }
        await refresh()
    }

    fileprivate func onMachineChanged() {
        _observerTick &+= 1
    }
    private var _observerTick: UInt64 = 0

    /// The Rust-ratified pre-fetch default (`ui/README.md` § Copy comprehensibility
    /// rule 5) — seeds `snapshot` before `configure` resolves so the page never
    /// paints an open-looking gate during the pre-fetch window. Reachable from
    /// Rust on both seams now (`row 260`); this VM keeps no local stand-in.
    private static let prefetchSnapshot = atprotoSettingsPrefetchSnapshot()

    /// The whole renderable ATProto settings surface in one record. The
    /// Rust-derived pre-fetch default until `configure` resolves a real one.
    public var snapshot: AtprotoSettingsSnapshot? {
        _ = _observerTick
        return machine?.snapshot() ?? Self.prefetchSnapshot
    }

    /// The page-level `error-message`: the connect failure first, else the
    /// machine snapshot's localized `error`.
    public var errorMessage: String? {
        _ = _observerTick
        return firstNonNil(connectError, machine?.snapshot().error.map(renderLocalizedText))
    }

    public func refresh() async { await machine?.refresh() }

    /// Select a target level. Never mutates the level directly — the machine
    /// either stages the transition card, or (Off→Linked, the one effect-free
    /// move) applies immediately.
    public func selectLevel(_ targetLevel: String) async {
        await machine?.selectLevel(targetLevel: targetLevel)
    }

    public func confirmTransition() async { await machine?.confirmTransition() }
    public func cancelTransition() { machine?.cancelTransition() }
    public func setDidMethod(_ method: String) { machine?.setDidMethod(method: method) }
    public func setHistoryBackfill(_ enabled: Bool) { machine?.setHistoryBackfill(enabled: enabled) }

    /// The 72 h recovery-fork contest ceremony (`atproto-identity-custody.md`
    /// § The 72 h recovery-fork contest). `openContestConfirm`/`cancelContest`
    /// are SYNC pure-local machine mutations that fire the observer internally
    /// (no manual repaint call needed, mirrors `cancelTransition`);
    /// `requestContest` is the one async round trip — client-direct HTTPS to
    /// the public PLC directory, never a nest call.
    public func openContestConfirm() { machine?.openContestConfirm() }
    public func cancelContest() { machine?.cancelContest() }
    public func requestContest() async { await machine?.requestContest() }

    /// The "Delete my Bluesky presence" ceremony (`ui/atproto.md` § User
    /// actions row 4; `atproto-pds-bridge.md` § Disable & revocation layer 2).
    /// `openDeleteConfirm`/`cancelDelete` are SYNC pure-local machine
    /// mutations that fire the observer internally (mirrors
    /// `openContestConfirm`/`cancelContest`); `confirmDelete` is the one async
    /// round trip, `fauna.bridges.atproto.delete_presence`.
    public func openDeleteConfirm() { machine?.openDeleteConfirm() }
    public func cancelDelete() { machine?.cancelDelete() }
    public func confirmDelete() async { await machine?.confirmDelete() }

    /// The delete card's terminal opt-in (`atproto-delete-tombstone`, S5 slice
    /// 5b; `ui/atproto.md` § User actions row 4). A SYNC pure-local tick on the
    /// open card that fires the observer internally — nothing is sent until
    /// `confirmDelete`, which runs the sweep and only then records the
    /// retirement, so the page holds no entry point to the terminal act.
    public func setDeleteRetireIdentity(_ enabled: Bool) {
        machine?.setDeleteRetireIdentity(enabled: enabled)
    }

    /// Mint a new app credential and remember its one-time secret locally.
    /// Auto-labels ("App credential N") and defaults `dmAllowed` false
    /// (least privilege) — F1 collects no label/dm_allowed input at mint time,
    /// mirrors linux's `settings/atproto.rs`.
    public func mint() async {
        guard let machine else { return }
        let before = Set(machine.snapshot().credentials.map(\.credentialId))
        let label = L.atprotoSettings.defaultCredentialLabel(count: "\(before.count + 1)")
        guard let secret = try? await machine.mint(label: label, dmAllowed: false) else { return }
        let newId = machine.snapshot().credentials.map(\.credentialId).first { !before.contains($0) }
        if let newId, !newId.isEmpty {
            revealedSecrets[newId] = secret
        }
    }

    public func revealSecret(credentialId: String) async {
        guard let machine else { return }
        guard let secret = try? await machine.revealSecret(credentialId: credentialId) else { return }
        revealedSecrets[credentialId] = secret
    }

    public func revoke(credentialId: String) async { await machine?.revoke(credentialId: credentialId) }
    public func setExternalAppsEnabled(_ enabled: Bool) async {
        await machine?.setExternalAppsEnabled(enabled: enabled)
    }

    // The OAuth consent card (resolve / poll) and the connected-app session rows
    // (revoke) moved to Settings → Connected apps (`ConnectedAppsVM`,
    // `connected-apps.md` § Architectural rules): no per-page copy survives.

    /// D10 authoring delegation (`atproto-pds-full.md` § Problem 1 → D10).
    /// Authorizing is ALSO the renewal gesture — provisioning overwrites the
    /// stored cert with a freshly dated one, so a lapsed grant recovers in one
    /// gesture with no revoke first. The machine re-reads and re-verifies the
    /// stored cert itself, so neither wrapper assumes success; the page repaints
    /// off the refreshed snapshot (or `error-message`, which the machine sets).
    public func authorizeDelegation() async { await machine?.authorizeExternalApps() }

    /// Destroys the signing sub-key nest-side, so external apps can no longer
    /// author. Already-published posts stay verifiable forever — their cert is
    /// embedded in their own wire — so this stops FUTURE authoring, not history.
    public func revokeDelegation() async { await machine?.deauthorizeExternalApps() }
}

/// Trampoline conforming to UniFFI's `AtprotoSettingsObserver`. The machine
/// takes the observer at construction time (`build_atproto_settings_machine`),
/// so late-binding via `target` lets the VM register itself after `configure`.
/// Mirrors `LabelerCatalogObserverBox`.
final class AtprotoSettingsObserverBox: AtprotoSettingsObserver, @unchecked Sendable {
    weak var target: AtprotoSettingsVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onMachineChanged() }
    }
}
