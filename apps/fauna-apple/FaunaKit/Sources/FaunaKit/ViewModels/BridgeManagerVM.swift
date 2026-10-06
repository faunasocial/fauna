import SwiftUI

@MainActor @Observable
public class BridgeManagerVM {
    public var bridges: [BridgeInfo] = []
    public var follows: [String: [BridgeFollow]] = [:]
    public var isLoading = false
    public var errorMessage: String?

    // Link form state
    public var linkFields: [String: [String: String]] = [:]
    public var selectedMode: [String: String] = [:]

    // Follow form
    public var followId = ""
    public var followPetname = ""

    /// When set, `refresh()` scopes `bridges` to just this one bridge id and
    /// skips the unified-page exclusion filter — the AT Protocol settings page's
    /// Linked-account panel uses this to focus on the `"bluesky"` row (which
    /// `isUnifiedBridgesPageBridge` deliberately excludes from the generic
    /// list; `docs/goal/ui/atproto.md` § Migration). `nil` (default) preserves
    /// the unified Bridges page's existing behavior unchanged.
    public var singleBridgeId: String?

    /// One blocked feed source, as a ward's ask names it — the whole
    /// `(bridge, operation, target)` triple, because that is what a guardian's grant
    /// is scoped to (`family-safety.md` § Feed-source approvals): a `follow` ask for
    /// one account must not light up a different follow on the same bridge, and a
    /// `link` ask (empty target) matches neither.
    public struct SourceKey: Hashable, Sendable {
        public let bridgeId: String
        public let operation: FfiFeedSourceOperation
        /// The follow id / feed URI; **empty for a link**.
        public let target: String

        public init(bridgeId: String, operation: FfiFeedSourceOperation, target: String) {
            self.bridgeId = bridgeId
            self.operation = operation
            self.target = target
        }

        /// The shared wire spelling, for comparing against the durable list.
        var wire: String { feedSourceOperationWire(operation: operation) }
    }

    /// Sources the nest refused for a guardian this session — the TYPED refusal only
    /// (rule (a)). **Keyed on the ask data, never on the add-form buffers:**
    /// ``addFollow(bridgeId:)`` clears `followId` when it succeeds and a refusal can
    /// land after the form moved on, so a buffer-keyed row would never paint. Cleared
    /// with the rest of the account-scoped state by ``reset()``.
    public private(set) var guardianRefused: Set<SourceKey> = []
    /// Sources this session just asked about — the durable half is the store's list;
    /// this only makes the render answer before the re-read lands (and survives a
    /// failed re-read, which is not a failed ask: the guardian has been rung).
    private var askedSources: Set<SourceKey> = []

    private var api: APIClient?
    /// The app-root `fauna.family.status` projection the durable asks read from.
    private var familyStatus: FamilyStatusStore?

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary), on `SearchVM.reset()`'s shape. Called by ``configure(api:)`` on an
    /// api-identity change **before** it re-points, and by the page's nil-client
    /// phase: More → Bridges is not unmounted by the iOS switch teardown, so without
    /// it the outgoing account's bridges survive into the incoming one's page
    /// .
    ///
    /// ⚠ The sharp field is `linkFields` — the per-bridge link form's typed values,
    /// which for several bridges ARE the user's credentials for that provider (an app
    /// password, a token). Half-typed credentials of account A surviving into account
    /// B's session are worse than stale rows: ``link(bridge:)`` would submit them
    /// under B's api, handing A's provider secret to B's nest. `follows`,
    /// `selectedMode` and the `followId`/`followPetname` form go with them.
    ///
    /// `singleBridgeId` deliberately does NOT: it is the HOST view's scoping choice
    /// (`AtprotoSettingsView` sets `"bluesky"` once at mount), not an account-scoped
    /// datum — dropping it would silently widen that page's Linked-account panel to
    /// the whole unified bridge list.
    public func reset() {
        api = nil
        familyStatus = nil
        bridges = []
        follows = [:]
        isLoading = false
        errorMessage = nil
        linkFields = [:]
        selectedMode = [:]
        followId = ""
        followPetname = ""
        guardianRefused = []
        askedSources = []
    }

    public func configure(api: APIClient, familyStatus: FamilyStatusStore? = nil) {
        if let current = self.api, current !== api { reset() }
        self.api = api
        self.familyStatus = familyStatus
    }

    // MARK: - The ward's feed-source ask (`family-safety.md` § Feed-source approvals)

    /// Fold one link/follow failure. The guardian gate is told apart from every
    /// other failure on the typed ``FfiError/GuardianApprovalRequired(msg:)`` — the
    /// shared `RpcError::is_guardian_approval_required` verdict — never a string
    /// match; painting the ask on a transport failure would tell an unsupervised
    /// user their account is supervised. The refusal **stays on `error-message`**
    /// (clause (b)): the source was not added, so silencing it because an ask is now
    /// offered would make the page claim success.
    private func noteFailure(_ error: Error, source: SourceKey) {
        if let ffi = error as? FfiError, case .GuardianApprovalRequired = ffi {
            guardianRefused.insert(source)
            errorMessage = L.bridges.sourceBlocked
        } else {
            errorMessage = DisplayError.http(error)
        }
    }

    /// The live asks on one bridge — durable rows first, then any this session just
    /// made whose row has not come back yet — for the card's
    /// `bridge-source-request-state` labels. **No supervision test here and none is
    /// needed:** both inputs are supervised-only by construction (the store gates its
    /// list on `supervisedBy`; a refusal arrives only from a supervised account).
    public func sourceAskStates(bridgeId: String) -> [FamilyStatusStore.FeedRequestState] {
        let durable = familyStatus?.feedRequestStates(bridgeId: bridgeId) ?? []
        let local = askedSources
            .filter { $0.bridgeId == bridgeId && !hasDurableRow($0) }
            .map { _ in FamilyStatusStore.FeedRequestState.pending }
        return durable + local
    }

    /// The refused sources on one bridge that still deserve the ask button — those
    /// with no durable row and no ask made yet, so an answered ask shows its verdict
    /// rather than offering the button again. Ordered deterministically (a `Set`
    /// iterates arbitrarily, and a re-render must not shuffle the buttons).
    public func sourceAskOffers(bridgeId: String) -> [SourceKey] {
        guardianRefused
            .filter { $0.bridgeId == bridgeId && !askedSources.contains($0) && !hasDurableRow($0) }
            .sorted { ($0.wire, $0.target) < ($1.wire, $1.target) }
    }

    private func hasDurableRow(_ source: SourceKey) -> Bool {
        familyStatus?.feedRequestState(
            bridgeId: source.bridgeId, operation: source.wire, target: source.target) != nil
    }

    /// `bridge-source-request-button` — ask the guardian to approve `source`
    /// (`fauna.family.feed_source.request`), then re-read the ward's own asks so what
    /// paints is what the NEST holds. `label` is display-only (the bridge's name is
    /// the one label still available once the form's buffers have cleared). Its own
    /// typed refusals are the ward's to read verbatim, not the gate's sentence.
    ///
    /// An approved ask is a PROMPT to retry, never an auto-retry (rule (e)): the
    /// grant is single-use, so spending it on a render the user did not ask for
    /// would burn it.
    public func requestSource(_ source: SourceKey, label: String) async {
        guard let api else { return }
        do {
            try await api.requestFeedSource(
                bridgeId: source.bridgeId, operation: source.operation,
                target: source.target, label: label)
            guard self.api === api else { return }   // the in-flight clause
            askedSources.insert(source)
            errorMessage = nil
            await familyStatus?.refresh(api: api)
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    public func refresh() async {
        guard let api else { return }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        do {
            let all = try await api.listBridges()
            guard self.api === api else { return }   // the in-flight clause
            if let singleBridgeId {
                bridges = all.filter { $0.id == singleBridgeId }
            } else {
                // Nostr is NOT a Bridges-page bridge — it has its own dedicated page
                // (the shared `NostrSettingsView`; nostr.md § Page structure / bridges.md
                // § Scope, 2026-06-13). Bluesky likewise moved to its own dedicated
                // page (`ui/atproto.md` § Migration). Exclude both from the generic
                // Bridges surface via the shared predicate (lifted 2026-07-17 — linux/
                // web/apple all filtered nostr identically by hand; apple was the last
                // consumer; bluesky joined the predicate).
                bridges = all.filter { isUnifiedBridgesPageBridge(id: $0.id) }
            }
            var newFollows: [String: [BridgeFollow]] = [:]
            for bridge in bridges where bridge.linked && bridge.supportsFollows {
                newFollows[bridge.id] = try await api.listBridgeFollows(bridgeId: bridge.id)
            }
            // The in-flight clause, re-checked after the LOOP of per-bridge follow
            // reads — the widest window on this page for a switch to land mid-read.
            guard self.api === api else { return }
            follows = newFollows
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    public func link(bridge: BridgeInfo) async {
        guard let api else { return }
        let modes = bridge.linkModes?.filter { $0.platform == nil || $0.platform == "desktop" } ?? []
        guard let mode = modes.first(where: { $0.mode == selectedMode[bridge.id] }) ?? modes.first else { return }
        errorMessage = nil
        // A link ask carries an EMPTY target (approving a link approves connecting
        // that bridge; the OAuth mode is mechanism, not scope).
        let source = SourceKey(bridgeId: bridge.id, operation: .link, target: "")

        do {
            let fields = linkFields[bridge.id] ?? [:]
            let resp = try await api.linkBridge(bridgeId: bridge.id, mode: mode.mode, fields: fields)
            guard self.api === api else { return }   // the in-flight clause
            if let redirect = resp.redirectUrl, let url = URL(string: redirect) {
                // `oauth_redirect` modes (e.g. Bluesky) resume in the browser — open
                // the provider's authorize URL via the shared cross-platform helper
                // (NSWorkspace on macOS / UIApplication on iOS). bridges.md § User
                // actions: "for oauth_redirect the reply redirect_url opens in the
                // browser"; mirrors linux + the iOS Account-page Bluesky path.
                OpenURL.open(url)
            }
            await refresh()
        } catch {
            guard self.api === api else { return }
            noteFailure(error, source: source)
        }
    }

    public func unlink(bridge: BridgeInfo) async {
        guard let api else { return }
        do {
            try await api.unlinkBridge(bridgeId: bridge.id)
            guard self.api === api else { return }   // the in-flight clause
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    public func updateSetting(bridge: BridgeInfo, key: String, value: Any) async {
        guard let api else { return }
        do {
            try await api.updateBridgeSettings(bridgeId: bridge.id, settings: [key: value])
            guard self.api === api else { return }   // the in-flight clause
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    public func addFollow(bridgeId: String) async {
        guard let api, !followId.isEmpty else { return }
        // Captured BEFORE the call: the ask is keyed on the refused follow's own id,
        // not on the form buffer a success clears (and a refusal can outlive).
        let source = SourceKey(bridgeId: bridgeId, operation: .follow, target: followId)
        do {
            try await api.addBridgeFollow(bridgeId: bridgeId, followId: followId,
                                           petname: followPetname.isEmpty ? nil : followPetname)
            guard self.api === api else { return }   // the in-flight clause
            followId = ""
            followPetname = ""
            await refresh()
        } catch {
            guard self.api === api else { return }
            noteFailure(error, source: source)
        }
    }

    public func removeFollow(bridgeId: String, followId: String) async {
        guard let api else { return }
        do {
            try await api.removeBridgeFollow(bridgeId: bridgeId, followId: followId)
            guard self.api === api else { return }   // the in-flight clause
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }
}
