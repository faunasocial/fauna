import Foundation

/// The reach-policy option catalogs + fail-closed label/wire mapping are shared
/// Rust now (`unknownSenderOptions`/`feedSourcesOptions`/`unknownSenderLabel`/
/// `feedSourcesLabel` — family-safety.md § Implementation status today, "A knob
/// value a client cannot parse renders fail-closed, on every app"; previously
/// hand-rolled six times, apple included). Do not re-add a local catalog or
/// fail-closed map — call these directly.
///
/// Within a major version a client may be **older than its nest**
/// (`version-compatibility.md`), so a newer nest can store a knob value this
/// binary cannot name. Rendering it as the permissive `allow` would show the
/// guardian a policy weaker than the one actually enforced — and because save
/// writes the editor's current state back, the next save would genuinely
/// downgrade the ward's protection. So an unrecognized value resolves to the
/// **strictest** option (`hold` / `block`), never `allow`. This is a safety
/// rule, not a cosmetic one — enforced once in shared Rust via `from_wire`.
///
/// The e2e drives these with the **localized label** (`driver.select(id, "Hold
/// for review")`) and reads it back with `get_text` — so the label is the wire
/// of the *driver*, and the wire value is what reaches the nest.
public enum FamilyReachPolicyFormat {
    /// A stored wire value, normalized to one of `options`' exact `.value`s —
    /// fail-closed via the shared label lookup, never hardcoding "which option
    /// is fail-closed" locally (mirrors android's `normalizeUnknownSenderWire`/
    /// `normalizeFeedSourcesWire` — the identical label-key-matching trick).
    public static func normalizedWire(
        _ wire: String, options: [ReachPolicyOption], label: (String) -> LocalizedText
    ) -> String {
        let resolvedKey = label(wire).key
        return options.first { $0.label.key == resolvedKey }?.value ?? wire
    }

    /// The wire value for a picked **label** (the driver's `select(id, "Hold for
    /// review")` contract). Tolerates a raw wire value passed as the label (a
    /// caller that already speaks wire) as an apple-local fallback layered on top
    /// of the shared catalog — not a rule shared Rust knows about — then falls
    /// closed via the same label-key-matching trick as `normalizedWire`.
    public static func wire(
        forPickedLabel picked: String, options: [ReachPolicyOption], label: (String) -> LocalizedText
    ) -> String {
        if let match = options.first(where: { renderLocalizedText($0.label) == picked }) {
            return match.value
        }
        if options.contains(where: { $0.value == picked }) { return picked }
        return normalizedWire(picked, options: options, label: label)
    }
}

/// Drives the shared **Family** surface (`FamilyView`) + the global
/// `supervised-indicator` over the `fauna.family.*` kinds
/// (family-safety.md § App surface), via the UniFFI `FfiFamilyClient`
/// (`FfiNestClient.family()` → `APIClient.familyClient()`). Both apple apps
/// share this one VM (priority #2), mirroring linux's `views/family.rs` and
/// web's `/app/family` over the same shared `FamilyClient`.
///
/// Role is decided from the one `fauna.family.status` read, not a second call:
/// the **supervised** section renders when `supervisedBy != nil`, the
/// **guardian** section when `wards` is non-empty. Both can render at once.
///
/// Nothing here is optimistic: every mutation is followed by a full re-read, so
/// the render always comes from nest-confirmed state (linux `run_mutation`).
/// The one exception is the offline composer (account-offline-mutation.md §
/// Implementation status today, the family composer): with no nest a decision,
/// an un-deny or a contact pre-approval rests in the device's outbox and drains
/// when the nest is back. There is no nest to re-read, so a queued decision
/// leaves the local list through the shared fold and the other two settle
/// nothing (``settleDecision(_:queued:)``).
@MainActor @Observable
public final class FamilyVM {
    // ── The `fauna.family.status` projection ────────────────────────────
    /// Who supervises the caller (the supervised side + the global indicator).
    public private(set) var supervisedBy: FfiFamilyGuardianInfo?
    /// The caller's own active policy, read-only (supervised side).
    public private(set) var myPolicy: FfiReachPolicy?
    /// The caller's own cross-device screen-time usage for their current
    /// local day, when supervised under a daily budget (family-safety.md §
    /// Screen time — the ward's summary shows the same number the guardian
    /// sees). `nil` when unsupervised or no budget is set — no accounting
    /// without a declared policy.
    public private(set) var myUsageTodayMinutes: UInt32?
    /// The caller's OWN established age band (`FfiFamilyStatus.age_band` —
    /// family-safety.md § The account age band: the account always sees its
    /// own band, like its policy). `nil` = no band row.
    public private(set) var myAgeBand: FfiFamilyAgeBand?
    /// The accounts the caller guards (the guardian side).
    public private(set) var wards: [FfiFamilyWardInfo] = []
    /// Every ward's pending reach approvals — the queue is **not** ward-scoped
    /// (family-safety.md § Reach approvals: one `approvals.list` read returns
    /// them all).
    public internal(set) var approvals: [FfiFamilyApprovalEntry] = []
    /// Proposals awaiting THIS caller's consent as the proposed guardian
    /// (family-safety.md § Graduation & transfer) — rendered as its own group
    /// above the role sections, and what widens the `family-tab` gate for a
    /// caller with no other family relationship yet.
    public private(set) var incomingTransfers: [FfiFamilyIncomingTransfer] = []

    public private(set) var isLoading = false
    public private(set) var isSaving = false
    public var errorMessage: String?

    /// Any family relationship at all — the `family-tab` gate (guardian OR
    /// supervised OR a pending/incoming transfer). An outgoing pending proposal
    /// implies `wards` is non-empty already; `incomingTransfers` is the case a
    /// proposed guardian with no other family relationship must still reach the
    /// prompt (family-safety.md § Graduation & transfer → Visibility). The
    /// `supervised-indicator` gate is `supervisedBy != nil` alone: a guardian
    /// who is not themself supervised sees no indicator.
    public var hasRelationship: Bool {
        supervisedBy != nil || !wards.isEmpty || !incomingTransfers.isEmpty
    }

    // ── The ONE shared reach-policy editor (ui.yaml carries exactly one,
    //    non-indexed, element set — a guardian with several wards selects a
    //    `family-ward-item` row to load that ward into it) ─────────────────
    /// The selected ward's actor id; the editor renders iff this resolves.
    public private(set) var selectedWardId: Data?
    public var draftContactApproval = false
    public var draftUnknownSender = "allow"
    public var draftFederation = true
    public var draftFeedSources = "allow"
    /// The bridge-DM gate (family-safety.md § The bridge-DM gate): whether an
    /// inbound DM over an already-connected bridge account, from a peer the
    /// ward has never messaged, is held for guardian review. `Option<String>`
    /// on the wire — absent means "leave unchanged" — so it rides out
    /// **only when `unknownPeerDmTouched`**, set solely by the Picker's own
    /// binding setter (never by `loadEditor`'s plain assignment below), the
    /// same discipline as linux's `unknown_peer_dm_edited`.
    public var draftUnknownPeerDm = "allow"
    public var unknownPeerDmTouched = false
    /// The content pillar's Guardian Notify knob (family-safety.md § Guardian
    /// Notify). Same editor, same save — it rides the one
    /// `fauna.family.policy.update` the reach knobs already use; the guardian's
    /// content rules save apart, through the shared rule editor
    /// (`FamilyRulesSection`).
    public var draftContentNotify = false
    /// The v1.x screen-time pillar's three inputs (`family-safety.md` §
    /// Screen time), typed `"HH:MM"` / whole minutes exactly as the guardian
    /// enters them — parsed at Save time via `parseTimeOfDay`/
    /// `parseDailyMinutes` (shared Rust; **write no policy logic in Swift**).
    /// Empty = unset (clears the window/budget); bounds come in pairs, so a
    /// half-typed window is a save-time refusal, not a client-side guess.
    public var draftScreenWindowStart = ""
    public var draftScreenWindowEnd = ""
    public var draftScreenDailyMinutes = ""

    public var contactAddInput = ""
    /// The graduate flow is reveal-then-confirm: the confirm button appears only
    /// after the first button is pressed.
    public var graduateConfirmVisible = false
    /// The proposed new guardian's hex actor id (`family-transfer-input`) — the
    /// same convention as `contactAddInput`. Cleared whenever the editor swaps
    /// wards (`loadEditor`), matching the pending/none swap it drives.
    public var transferInput = ""

    private var family: FfiFamilyClient?
    /// Held alongside `family` so `load()` can route its `status` read
    /// through the shared choke point (`APIClient.familyStatus()`) rather
    /// than `family.status()` directly — every other call still goes through
    /// `family`, the raw client.
    private var api: APIClient?

    public init() {}

    /// The guardian host of the shared `feature-policy-editor`
    /// (family-safety.md § App surface → *Feature limits*): the ward-keyed arm
    /// of the same seam, read live from ``selectedWardId``. Built at
    /// ``configure(api:)``, nil before it; reset and re-read on every ward
    /// change, because an editor is one ward's.
    public private(set) var featureAuthoring: FeatureAuthoringHost?

    public var selectedWard: FfiFamilyWardInfo? {
        guard let selectedWardId else { return nil }
        return wards.first { $0.actorId == selectedWardId }
    }

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary), on `SearchVM.reset()`'s shape. Called by ``configure(api:)`` on an
    /// api-identity change **before** it rebuilds, and by the page's nil-client
    /// phase: More → Family is not unmounted by the iOS switch teardown, and the
    /// page's `.task` was keyed on `navGeneration` rather than on the client, so a
    /// switch left the outgoing account's guardian, policy and WARD ROSTER rendered
    /// — a family graph naming real people the incoming account has no relationship
    /// with . The `family == nil` guard below made the
    /// client itself survive alongside them.
    ///
    /// The `draft*` editor fields go too: they are the selected ward's policy staged
    /// for save, so a survivor would offer to write the OUTGOING account's ward
    /// policy through the incoming account's session.
    public func reset() {
        family = nil
        api = nil
        supervisedBy = nil
        myPolicy = nil
        myUsageTodayMinutes = nil
        myAgeBand = nil
        wards = []
        approvals = []
        incomingTransfers = []
        isLoading = false
        isSaving = false
        errorMessage = nil
        selectedWardId = nil
        featureAuthoring = nil
        draftContactApproval = false
        draftUnknownSender = "allow"
        draftFederation = true
        draftFeedSources = "allow"
        draftUnknownPeerDm = "allow"
        unknownPeerDmTouched = false
        draftContentNotify = false
        draftScreenWindowStart = ""
        draftScreenWindowEnd = ""
        draftScreenDailyMinutes = ""
        contactAddInput = ""
        graduateConfirmVisible = false
        transferInput = ""
    }

    /// Vend the `fauna.family.*` client over the shared connection, then load.
    /// Called from the view's `.task`; re-callable (idempotent).
    public func configure(api: APIClient) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        if family == nil {
            do {
                let client = try await api.familyClient()
                guard self.api === api else { return }   // the in-flight clause
                family = client
            } catch {
                guard self.api === api else { return }
                errorMessage = DisplayError.message(error)
                return
            }
        }
        if featureAuthoring == nil {
            featureAuthoring = FeatureAuthoringHost(
                readRows: { [weak self] in
                    guard let ward = self?.selectedWardId else { return [] }
                    return try await api.featuresClient().wardAuthoredRows(ward: ward)
                },
                openEditor: { [weak self] feature in
                    guard let ward = self?.selectedWardId else { throw CancellationError() }
                    return try await api.featuresClient().openEditorForWard(ward: ward, feature: feature)
                })
        }
        await load()
    }

    /// Re-read `fauna.family.status` + `fauna.family.approvals.list`. An empty
    /// reply is the empty state, not an error. The approvals read runs even for
    /// a supervised-only account (it yields `[]`), keeping one code path.
    public func load() async {
        guard let family, let api else { return }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            let status = try await api.familyStatus()
            // The in-flight clause: a status read suspended for the outgoing account
            // still returns after the drop, and its ward roster must not land here.
            guard self.api === api else { return }
            supervisedBy = status.supervisedBy
            myPolicy = status.policy
            myUsageTodayMinutes = status.usageTodayMinutes
            myAgeBand = status.ageBand
            wards = status.wards
            incomingTransfers = status.incomingTransfers
            let loadedApprovals = try await family.approvalsList()
            guard self.api === api else { return }
            approvals = loadedApprovals
            reconcileSelection()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    /// Keep the editor pointed at the same ward across a reload; fall back to
    /// the first ward, or clear the editor when there are none left (the
    /// post-graduation case).
    private func reconcileSelection() {
        if let selectedWardId, wards.contains(where: { $0.actorId == selectedWardId }) {
            loadEditor(wardId: selectedWardId)
            return
        }
        graduateConfirmVisible = false
        if let first = wards.first {
            loadEditor(wardId: first.actorId)
        } else {
            selectedWardId = nil
        }
    }

    /// Load a ward's stored policy into the shared editor. Knob values are
    /// **normalized fail-closed** on the way in, so an unparseable value renders
    /// — and saves back — as the strictest option, never `allow`.
    public func loadEditor(wardId: Data) {
        guard let ward = wards.first(where: { $0.actorId == wardId }) else { return }
        // A ward change drops the previous ward's rows and open editor before
        // the re-read; a reload of the SAME ward keeps its open editor.
        let wardChanged = selectedWardId != wardId
        selectedWardId = wardId
        if wardChanged { featureAuthoring?.reset() }
        if let host = featureAuthoring { Task { await host.refresh() } }
        graduateConfirmVisible = false
        transferInput = ""
        draftContactApproval = ward.policy.contactApproval
        draftFederation = ward.policy.federationContact
        draftUnknownSender = FamilyReachPolicyFormat.normalizedWire(
            ward.policy.unknownSenderMail, options: unknownSenderOptions(), label: unknownSenderLabel(value:))
        draftFeedSources = FamilyReachPolicyFormat.normalizedWire(
            ward.policy.feedSources, options: feedSourcesOptions(), label: feedSourcesLabel(value:))
        // Unlike every sibling knob, an ABSENT `unknown_peer_dm` renders the
        // `allow` DEFAULT, never the fail-closed `hold` (family-safety.md §
        // The bridge-DM gate; `fauna_core::format`'s
        // `an_absent_unknown_peer_dm_renders_its_allow_default_not_the_fail_closed_value`
        // — the nest omits a knob sitting at its default). A PRESENT value
        // still normalizes fail-closed like every other knob.
        draftUnknownPeerDm = FamilyReachPolicyFormat.normalizedWire(
            ward.policy.unknownPeerDm ?? "allow", options: unknownPeerDmOptions(), label: unknownPeerDmLabel(value:))
        unknownPeerDmTouched = false

        // The content rules are the rule editor's (`FamilyRulesSection` loads
        // them off the ward's stored document); only the Notify knob is a draft
        // of this form.
        draftContentNotify = ward.policy.contentNotify ?? false

        // The screen-time pillar's three inputs — a `nil` bound renders as an
        // empty field. `formatTimeOfDay` is the inverse of the save-time
        // `parseTimeOfDay` (shared Rust, both directions), so the editor
        // never spells the "HH:MM" format itself.
        let screenTime = ward.policy.screenTime
        draftScreenWindowStart = screenTime?.windowStart.map { formatTimeOfDay(minutesFromMidnight: $0) } ?? ""
        draftScreenWindowEnd = screenTime?.windowEnd.map { formatTimeOfDay(minutesFromMidnight: $0) } ?? ""
        draftScreenDailyMinutes = screenTime?.dailyMinutes.map { String($0) } ?? ""
    }

    /// `fauna.family.policy.update` — persist the shared editor onto the
    /// selected ward, then re-read (never optimistic).
    public func savePolicy() async {
        guard let family, let wardId = selectedWardId else { return }
        isSaving = true
        errorMessage = nil
        defer { isSaving = false }
        do {
            // Parse the three screen-time inputs FIRST, then pre-validate the
            // whole window with the nest's own `ScreenTimePolicy::validate`
            // (`family-safety.md` § Where logic lives, write validation): a
            // half-set or empty window is refused HERE, before any request,
            // and lands in `errorMessage` exactly like any other save failure.
            // **Write no policy logic here**: parsing, formatting and the
            // half-set-pair / range refusal all live in shared Rust.
            let windowStart = try parseTimeOfDay(input: draftScreenWindowStart)
            let windowEnd = try parseTimeOfDay(input: draftScreenWindowEnd)
            let dailyMinutes = try parseDailyMinutes(input: draftScreenDailyMinutes)
            let screenTime = FfiScreenTimePolicy(
                windowStart: windowStart, windowEnd: windowEnd, dailyMinutes: dailyMinutes)
            try validateScreenTime(policy: screenTime)
            try await family.policyUpdate(
                supervisedActorId: wardId,
                policy: FfiReachPolicy(
                    contactApproval: draftContactApproval,
                    unknownSenderMail: draftUnknownSender,
                    federationContact: draftFederation,
                    feedSources: draftFeedSources,
                    // The content pillar is the rule editor's, saved by its
                    // own button (`save_content_rules`); this form sends it
                    // **absent**, so a reach save never rewrites the rules.
                    contentPolicy: nil,
                    // Screen time is now edited here too, so it goes out
                    // **present** as well — an all-nil policy is an explicit
                    // "no window, no budget", not "leave unchanged".
                    screenTime: screenTime,
                    contentNotify: draftContentNotify,
                    // The bridge-DM gate stays `Option<String>` on the wire —
                    // absent means "leave unchanged" (family-safety.md §
                    // Policy-update compatibility) — so it rides out only
                    // when the guardian actually touched the Picker; an
                    // untouched editor must never silently rewrite the
                    // ward's knob back to whatever this render happened to
                    // show.
                    unknownPeerDm: unknownPeerDmTouched ? draftUnknownPeerDm : nil))
            await load()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// `fauna.family.approvals.decide` — approve or deny one pending item on the
    /// ward's behalf. The entry's keys are already mutually exclusive on the
    /// wire (a `contact` is named by `peerActorId`, a `mail_hold` by
    /// `messageId`), so both pass straight through — deciding is keyed on the
    /// **message**, never the address, so approving one held message never
    /// sweeps every other message from that sender.
    ///
    /// The entry travels whole: shared Rust passes every item key through, so
    /// this side never picks which key a kind uses. With no nest the decision
    /// queues in the device's outbox (``settleDecision(_:queued:)``).
    public func decide(_ entry: FfiFamilyApprovalEntry, approve: Bool) async {
        guard let family else { return }
        errorMessage = nil
        do {
            let queued = try await family.approvalsDecideOrQueue(entry: entry, approve: approve)
            if settleDecision(entry, queued: queued) { await load() }
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Settle one decision's outcome; returns whether the page re-reads. Sent
    /// (`queued == false`) → re-read from the nest, as every mutation here.
    /// Queued with no nest → the decided item leaves the local list through the
    /// shared fold (`approvalsAfterQueuedDecision`, matched by item key, never
    /// display fields) and nothing is re-read: there is no nest to read.
    /// Mirrors tui's `Outcome::Queued` and android's `decideApproval`.
    func settleDecision(_ entry: FfiFamilyApprovalEntry, queued: Bool) -> Bool {
        guard queued else { return true }
        approvals = approvalsAfterQueuedDecision(approvals: approvals, decided: entry)
        return false
    }

    /// `fauna.family.contact.add` — pre-approve a contact on the selected ward's
    /// behalf. v1 takes the peer's actor id as hex (handle resolution is future
    /// UX polish — `i18n family.contact_add_placeholder`).
    public func addContact() async {
        guard let family, let wardId = selectedWardId else { return }
        let hex = contactAddInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !hex.isEmpty, hex.count % 2 == 0, let peer = Data(hexString: hex) else {
            errorMessage = L.family.contactAddInvalidActorId
            return
        }
        errorMessage = nil
        do {
            // With no nest the pre-approval queues in the device's outbox and
            // folds nothing; only a sent one re-reads.
            let queued = try await family.contactAddOrQueue(supervisedActorId: wardId, peerActorId: peer)
            contactAddInput = ""
            if !queued { await load() }
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// `fauna.family.graduate` — supervised → full account, in place. The ward
    /// leaves the guardian's list on the following re-read.
    public func graduate() async {
        guard let family, let wardId = selectedWardId else { return }
        errorMessage = nil
        do {
            try await family.graduate(supervisedActorId: wardId)
            graduateConfirmVisible = false
            await load()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// `fauna.family.transfer` — propose a new guardian for the selected ward
    /// (family-safety.md § Graduation & transfer). Pending until the target
    /// accepts; the link is untouched until then. Same hex-actor-id convention
    /// (and the same invalid-id error) as `addContact`.
    public func submitTransfer() async {
        guard let family, let wardId = selectedWardId else { return }
        let hex = transferInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !hex.isEmpty, hex.count % 2 == 0, let target = Data(hexString: hex) else {
            errorMessage = L.family.contactAddInvalidActorId
            return
        }
        errorMessage = nil
        do {
            try await family.transfer(supervisedActorId: wardId, newGuardianActorId: target)
            transferInput = ""
            await load()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// `fauna.family.transfer.cancel` — withdraw the selected ward's pending
    /// proposal (initiator side: the current guardian or the admin).
    public func cancelTransfer() async {
        guard let family, let wardId = selectedWardId else { return }
        errorMessage = nil
        do {
            try await family.transferCancel(supervisedActorId: wardId)
            await load()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// `fauna.family.device.mark` — set/clear the guardian-enrolled-device
    /// marker on one of the selected ward's devices (family-safety.md § Full
    /// visibility for young children, Slice F). Not batched behind
    /// `family-policy-save-button`: `device.mark` is its own per-device RPC,
    /// so the flip lands immediately and the re-read re-renders every row
    /// from nest-confirmed state (mirrors linux `Mutation::DeviceMark`).
    ///
    /// Takes `wardId`/`deviceId` **by value**, never a row index: a
    /// concurrent refresh that reorders the ward's devices must not route a
    /// flip at the wrong device — the mark is a security promise ("the child
    /// cannot remove the guardian's device").
    public func deviceMark(wardId: Data, deviceId: String, marked: Bool) async {
        guard let family else { return }
        errorMessage = nil
        do {
            try await family.deviceMark(supervisedActorId: wardId, deviceId: deviceId, marked: marked)
            await load()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// The guardian's UN-DENY of one bridge-DM peer (`family-blocked-peer-allow-button`;
    /// `family-safety.md` § The bridge-DM gate → *The un-deny surface*). Takes the
    /// denied row **whole** — the `(bridgeId, peerId)` pair the guardian read off
    /// `blockedDmPeers` — so the call cannot address a different peer than the row it
    /// was tapped in; the `dm_hold` wire kind and the peer riding `peer_address` are
    /// the shared client's. Idempotent and not queue-scoped: it works long after the
    /// hold row is gone. Followed by a full re-read, like every mutation here, so the
    /// row leaves the list on the nest's word. With no nest the un-deny queues in
    /// the device's outbox and folds nothing: the row stays until the nest says so.
    public func allowBlockedPeer(wardId: Data, peer: FfiFamilyBlockedPeer) async {
        guard let family else { return }
        errorMessage = nil
        do {
            let queued = try await family.allowBlockedPeerOrQueue(supervisedActorId: wardId, peer: peer)
            if !queued { await load() }
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Accept/decline one incoming proposal (the proposed-guardian side).
    /// Accept re-points the link — the follow-up re-read lands the ward in this
    /// account's own `wards` list.
    public func decideIncoming(_ entry: FfiFamilyIncomingTransfer, accept: Bool) async {
        guard let family else { return }
        errorMessage = nil
        do {
            if accept {
                try await family.transferAccept(supervisedActorId: entry.supervisedActorId)
            } else {
                try await family.transferDecline(supervisedActorId: entry.supervisedActorId)
            }
            await load()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    // ── Presentation helpers (pure; shared by both apple shells) ─────────

    /// The supervised side's read-only policy summary (`family-policy-summary`)
    /// — the lines (order, labels, fail-closed value rendering) come from shared
    /// Rust via `reachPolicySummary`; this only owns the "{label}: {value}" join,
    /// apple's own line layout (mirrors linux's `policy_summary`).
    /// `usageTodayMinutes` folds the screen-time readout into this same
    /// summary rather than claiming a new ui.yaml ID (family-safety.md §
    /// Screen time — "the ward's summary shows the same number the guardian
    /// sees"): the supervised section's element set is
    /// `family-guardian-handle` + `family-policy-summary`, and the ward's
    /// usage *is* a line of "the active policy, read-only", the same shape
    /// as every other rule shown here. The number and its wording come from
    /// the shared `usageTodayLine` — the SAME call `FamilyVM.usageTodayText`
    /// makes for the guardian's own per-ward readout, so the two surfaces
    /// cannot show different figures. `nil` renders nothing at all: no
    /// budget, no accounting. Mirrors linux's `views/family.rs::policy_summary`.
    public nonisolated static func policySummary(
        _ policy: FfiReachPolicy, usageTodayMinutes: UInt32? = nil
    ) -> String {
        var lines = reachPolicySummary(policy: policy)
        if let used = usageTodayMinutes {
            lines.append(usageTodayLine(usedMinutes: used, budgetMinutes: policy.screenTime?.dailyMinutes))
        }
        // A line a rule of the guardian's document makes carries that rule's
        // reason, appended verbatim as "{label}: {value} — {reason}"
        // (`nest-rules.md` § The shared rule editor → *The reason is the
        // member's to read*): the entry for the UI language, else the required
        // `default` one.
        return lines
            .map { line in
                let text = "\(renderLocalizedText(line.label)): \(renderLocalizedText(line.value))"
                let reason = line.reason.flatMap { $0[RegionStore.uiLang] ?? $0["default"] }
                guard let reason, !reason.isEmpty else { return text }
                return "\(text) — \(reason)"
            }
            .joined(separator: "\n")
    }

    /// The guardian's per-ward `family-ward-content-notices` readout
    /// (`family-safety.md` § Guardian Notify) — one "{label}: {value}" line per
    /// `(category, count)` notice off `FfiFamilyWardInfo.contentNotices`, over
    /// the shared `contentNoticeLine`; mirrors `policySummary` above and
    /// android's `formatContentNotices`. Empty when Notify is off or nothing
    /// was reported today — the caller renders the element only when non-empty
    /// (mirrors android/linux/web).
    public nonisolated static func contentNoticesText(_ notices: [FfiFamilyContentNotice]) -> String {
        notices
            .map { contentNoticeLine(category: $0.category, count: $0.count) }
            .map { "\(renderLocalizedText($0.label)): \(renderLocalizedText($0.value))" }
            .joined(separator: "\n")
    }

    /// The guardian's per-ward `family-ward-usage-today` readout
    /// (`family-safety.md` § Screen time: "the ward's summary shows the same
    /// number the guardian sees" — transparency) — over the shared
    /// `usageTodayLine`, mirrors `contentNoticesText`/`policySummary`. The
    /// caller renders the element only when `usedMinutes` is non-nil (no
    /// accounting without a declared daily budget).
    /// The two age-band readouts' text (`family-ward-age-band` with `own =
    /// false`, `family-age-band-summary` with `own = true`) — the shared
    /// `age_band_line`, so the guardian's row and the ward's summary cannot
    /// disagree; `nil` when there is no band or the client cannot name it
    /// (absent, never placeholdered — family-safety.md § App surface →
    /// *Age-band surfaces*).
    public nonisolated static func ageBandText(_ band: FfiFamilyAgeBand?, own: Bool) -> String? {
        guard let band,
              let line = ageBandLine(band: band.band, provenance: band.provenance, own: own)
        else { return nil }
        return renderLocalizedTextNested(line)
    }

    public nonisolated static func usageTodayText(usedMinutes: UInt32, budgetMinutes: UInt16?) -> String {
        let line = usageTodayLine(usedMinutes: usedMinutes, budgetMinutes: budgetMinutes)
        return "\(renderLocalizedText(line.label)): \(renderLocalizedText(line.value))"
    }
}
