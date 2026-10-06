import SwiftUI

/// Shared view-model for the user-facing `nests` page (macOS + iOS, one FaunaKit
/// VM; renamed from `linked-nests` 2026-07-07, `docs/goal/ui/nests.md`). A thin
/// proxy over the shared `LinkedNestsMachine` (UniFFI, `libs/fauna-client-pair`):
/// the machine owns all state + RPC orchestration over the User-class
/// `fauna.pair.{list,add,revoke}` surface (Refresh / Link / LinkBoth / Unlink)
/// plus the both-ends `connect_peer` sequencing, **and** the v1 nest-trust facet
/// (Mint / Renew / Revoke / SetLens over the home nest's content-processing
/// grants — `nests.md` § Where logic lives). This VM holds the latest
/// `LinkedNestsSnapshot` as `@Observable` state and re-reads it after each
/// `hydrate` / `dispatch`. The machine is **pull-based** (no observer callback,
/// like `MailAliasesMachine` / `MailSettingsMachine`), so re-assigning `snapshot`
/// drives the SwiftUI re-render. No pairing or trust logic here — the lift is
/// render-only (priority #1). Target state: `docs/goal/ui/nests.md` +
/// `docs/goal/behavior/linked-nests.md` (the linking half); the lead-client
/// renderer is linux (`apps/fauna-linux/src/settings/linked_nests.rs`).
/// `hydrate()`/`dispatch(_:)` come from `MachineBackedVM`'s shared default
/// (dedup — this also fixed a latent bug: the old local
/// `hydrate()` unconditionally overwrote `errorMessage` with `snap.error`,
/// clobbering a thrown-hydrate failure message back to nil whenever the
/// snapshot read itself succeeded with no `snapshot.error` of its own — the
/// same class of bug already fixed elsewhere, e.g. `BridgeApprovalVM`'s
/// `hydrate()` comment).
@MainActor @Observable
public final class LinkedNestsVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`. The view reads `pairings`,
    /// `status`, `error`.
    public internal(set) var snapshot: LinkedNestsSnapshot?
    /// Page-level error surface (`error-message`) — carries both connect/build
    /// failures and the machine's own `snapshot.error` (incl. the admin
    /// pairing-disabled rejection).
    public var errorMessage: String?
    public internal(set) var isLoading = false

    var machine: LinkedNestsMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?

    /// The custodian-**nest** custody rows (`nests.md` § Trust facet — custody
    /// rows): the `custodianNestUrl != nil` half of the shared facet fold, whose
    /// complement the Devices page's `custody-holder-card` family paints. Read on
    /// the page's hydrate edge; a failed/absent fold keeps the previous rows.
    public internal(set) var custodyNestRows: [CustodyHolderRowView] = []
    /// Lowercase-hex identities that hold this account's generation-key escrow
    /// (`custodyEscrowHolders()`, the shared `AccountStoreHandle::escrow_holders`
    /// read) — the `participant-escrow-holder-badge` source. `nil` from the FFI
    /// keeps the previous set rather than blanking the badge.
    public internal(set) var escrowHolders: Set<String> = []

    #if DEBUG
    /// The displayed page's VM, so the `trust_facet_advance_clock` TestAgent
    /// command (`TrustFacetClockTestCommand`) can re-fold on the SAME instance
    /// the Nests page renders. **Weak** and `#if DEBUG` — see
    /// `BackupDestinationsVM.liveInstanceForTest`.
    @MainActor
    public static weak var liveInstanceForTest: LinkedNestsVM?
    #endif

    public init() {}

    /// The owner's active pairings (empty until hydrated).
    public var pairings: [LinkedNestRow] { snapshot?.pairings ?? [] }

    /// Every nest row for the `nests-item` list — the home/connected nest first
    /// (identity + trust facet, no Unlink), then pairings (`nests.md` § Layout —
    /// mirrors the linux `home.iter().chain(pairings.iter())` render order).
    public var rows: [LinkedNestRow] {
        if let home = snapshot?.home { return [home] + pairings }
        return pairings
    }

    /// The user's post-forward queue on the connected nest (`nests-forward-queue`,
    /// `nests.md` § Forward queue); `nil` until the nest reports one. The shell
    /// paints the block only while `queued > 0`.
    public var forwardQueue: ForwardQueueStatus? { snapshot?.forwardQueue }

    /// A mutation is in flight — the add button disables so the user can't
    /// double-fire (mirrors the linux render).
    public var isWorking: Bool { snapshot?.status == .working }

    /// The page's last `RestoreGeneration` outcome (`nest-trust-generation-notice`,
    /// `nests.md` § Trust facet — generation recovery). Page-scoped, not
    /// per-row: a restore's outcome describes the last action, not any one
    /// generation row.
    public var restoreOutcome: TrustRestoreOutcome? { snapshot?.restoreOutcome }

    /// Vend the machine from APIClient (the mail-relay variant — see
    /// `APIClient.linkedNestsMachine()`) and load the first snapshot. Idempotent
    /// — re-vends only on a session re-point (`configuredApi` change), matching
    /// every sibling `MachineBackedVM` conformer (this VM was the one holdout still guarding on `machine == nil` alone).
    public func configure(api: APIClient) async {
        bindLiveInstanceForTest()
        guard await attach(api: api) else { return }
        await hydrate()
    }

    /// `configure`, but hydrating on every call rather than only when it builds
    /// the machine — the page's `.task(id: reloadToken)` body, so a
    /// re-navigation to the mounted page re-reads the rows, custody and badge.
    public func refresh(api: APIClient) async {
        bindLiveInstanceForTest()
        _ = await attach(api: api)
        await hydrate()
    }

    private func bindLiveInstanceForTest() {
        #if DEBUG
        // Late-bind on every configure: re-entering the page builds a fresh VM.
        Self.liveInstanceForTest = self
        #endif
    }

    /// Build the machine for `api` if it is not already the cached one. `true`
    /// when this call vended a fresh machine (the caller then hydrates).
    private func attach(api: APIClient) async -> Bool {
        guard machine == nil || configuredApi !== api else { return false }
        do {
            let m = try await api.linkedNestsMachine()
            machine = m
            configuredApi = api
            snapshot = m.snapshot()
            return true
        } catch {
            errorMessage = DisplayError.message(error)
            return false
        }
    }

    /// Refresh the pairing list from the nest (page mount / explicit refresh),
    /// then the custody rows and the escrow-holder set the page paints beside it.
    public func hydrate() async {
        await hydrateFromMachine()
        await loadCustody()
    }

    /// The app-level auto-renew tick (`nests.md` § Expiry / renewal → *Duration
    /// and blessing*): dispatch `AutoRenew` over a machine built without the
    /// page's hydrate — the sweep is silent and best-effort, so a failure never
    /// paints an error.
    public func autoRenewTick(api: APIClient) async {
        _ = await attach(api: api)
        guard let machine else { return }
        _ = try? await machine.dispatch(action: .autoRenew)
    }

    /// Drive one ceremony pass, re-fold the custody facet and re-read the escrow
    /// holders. The drive comes first, exactly as on the Devices page: apple has
    /// no app-level ceremony edge (tui and linux drive on session start and on
    /// every ceremony-moved notification), so a page load is the edge — without
    /// it an accept a custodian sent while the user sat on THIS page would never
    /// be minted against. Cheap when settled and fire-and-forget. The fold
    /// itself ingests the receipts custodian nests staged at this account's
    /// nest (`load_custody_facet`). Neither failure is a page error.
    func loadCustody() async {
        guard let api = configuredApi else { return }
        await api.driveCustody()
        if let facet = try? await api.loadCustodyFacet() { applyCustodyFacet(facet) }
        if let holders = await custodyEscrowHolders() {
            escrowHolders = Set(holders.map { $0.lowercased() })
        }
    }

    private func applyCustodyFacet(_ facet: CustodyFacetView) {
        custodyNestRows = Self.nestAnchored(facet.rows)
    }

    /// The custody rows this page owns: exactly those whose accept bound a
    /// NEST (`custodianNestUrl != nil`). The Devices page renders the
    /// complement, so one custody never paints in both places.
    static func nestAnchored(_ rows: [CustodyHolderRowView]) -> [CustodyHolderRowView] {
        rows.filter { $0.custodianNestUrl != nil }
    }

    /// Whether `nestId` holds the account's escrow (the badge's per-row gate).
    public func holdsEscrow(_ nestId: String) -> Bool {
        escrowHolders.contains(nestId.lowercased())
    }

    /// Revoke a custodian-nest custody (`nest-trust-custody-revoke-button`).
    /// Carries the row's grant id + accept-bound custodian key, never an index;
    /// the act's error reaches `error-message` (convention 11) and the re-folded
    /// facet repaints the rows.
    public func revokeCustody(grantId: Data, holder: Data?) async {
        guard let api = configuredApi else { return }
        errorMessage = nil
        do {
            let outcome = try await api.revokeCustody(grantId: grantId, holder: holder)
            if let facet = outcome.facet { applyCustodyFacet(facet) }
            if let message = outcome.error {
                errorMessage = L.devices.errorRevokeCustody(message: message)
            }
        } catch {
            errorMessage = DisplayError.message(error).map { L.devices.errorRevokeCustody(message: $0) }
        }
    }

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: LinkedNestsAction) async { await dispatchToMachine(action) }

    /// Classify the link-form value and dispatch the action it resolves to (a
    /// nest **address** seeds both ends in one action via `LinkBoth`; a bare
    /// 64-hex **identity** authorizes that one nest via `Link`, the out-of-band
    /// path). Routing lives in shared Rust (`classifyLinkInput`) so every app
    /// resolves the same input identically (priority #2). Empty `capabilities`
    /// → the machine fills the default full self-sync set.
    public func submitLink(_ raw: String) async {
        let action: LinkedNestsAction
        switch classifyLinkInput(raw: raw) {
        case .nestUrl(let nestUrl):
            action = .linkBoth(
                otherNestUrl: nestUrl, capabilities: [], expiresAt: nil, label: nil)
        case .nestId(let nestId):
            action = .link(
                nestId: nestId, capabilities: [], expiresAt: nil, label: nil, nestUrl: nil)
        }
        await dispatch(action)
    }
}
