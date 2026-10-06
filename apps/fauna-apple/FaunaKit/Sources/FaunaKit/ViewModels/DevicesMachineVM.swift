import SwiftUI

/// Thin SwiftUI-friendly proxy over the page-level `DevicesMachine` (UniFFI,
/// `libs/fauna-devices-machine` via `libs/fauna-ffi`). All device / folder /
/// conflict state, the page write gestures, and the embedded folder creation
/// wizard live in the shared-Rust machine; this class:
///
///   1. builds + owns the machine instance (over the session `FfiNestClient`),
///   2. implements `DevicesObserver` to translate machine notifications into
///      `@Observable` invalidations on the main actor,
///   3. exposes the latest `DevicesSnapshot` + small gesture wrappers so SwiftUI
///      views read `vm.snapshot` / drive `vm.removeDevice(index:)` instead of
///      reaching into `vm.machine` everywhere.
///
/// Shared by the macOS and iOS apps — one FaunaKit VM, identical behaviour on
/// both. **One instance per signed-in session**, not per page: the app shells
/// hold it as scene-level `@State` injected through `.environment` (like
/// `feedVM`), read by `DevicesView`, `MacFoldersView` and the iOS
/// `FoldersView`, and `ActorScope.dropAppOwnedState` drops it via `reset()`.
/// The machine's memory is load-bearing — the followed-folders source answers
/// an unreachable read with the LAST rows it read (`ui/folders.md` § Following
/// a public folder), and the refresh barrier counts per machine — so a machine
/// born per page visit emptied the followed list on a revisit during a dropped
/// connection. web's twin is `devices-session.ts`. Mirrors `OnboardingVM`'s observer-box pattern
/// (the machine is push-based: gestures mutate then notify, including the
/// embedded wizard whose own observer forwards to this page observer). Target
/// state: `docs/goal/ui/devices.md` § Broader DevicesSnapshot / § Where logic
/// lives.
@MainActor @Observable
public final class DevicesMachineVM {
    /// The page-level machine. `nil` until `configure` succeeds. Views forward
    /// page gestures through the wrappers below; the embedded wizard is reached
    /// via `wizardMachine`.
    public private(set) var machine: DevicesMachine?

    /// One-time connect/build failure (`api.devicesMachine` threw). Page read /
    /// write failures live on the machine snapshot's `error` instead; both are
    /// surfaced through `errorMessage`.
    public private(set) var connectError: String?

    private let observerBox = DevicesObserverBox()

    /// Held from `configure` so the owner-side folder Sharing gestures
    /// (`loadFolderActors` / `shareFolder` / `removeFolderMember`) can reach the
    /// thin `FfiFoldersClient` read + the `nest + session + ownerSecret` author fns
    /// (all live on `APIClient`). The device / conflict / wizard gestures ride the
    /// `machine` and don't need it.
    private var api: APIClient?

    /// The cross-user "Shared with" roster per folder (`folder-member-item`
    /// rows), keyed by set name — loaded lazily by `loadFolderActors` for shared
    /// sets (`FolderSummary.mlsGroupId != nil`). Not part of `DevicesSnapshot`: the
    /// actor roster is a separate `members.list_actors` read (distinct from the
    /// snapshot's *device* roster), so it lives on the VM.
    public private(set) var folderActors: [String: [FfiFolderActorMember]] = [:]

    /// The per-folder **device-place** roster (`folder-place-row` rows) keyed by
    /// folder name — the post-create place editor's model, loaded lazily on row
    /// expand by `FolderPlacesSection` and RE-READ after every flag write.
    ///
    /// Deliberately not on `DevicesSnapshot`: the per-seat flags are not on the
    /// page snapshot a write already refreshes, so the roster read is the only
    /// thing that can answer what the nest now holds. Distinct from
    /// `folderActors` (the cross-user roster) and `folderDeviceActivity` (the
    /// change signal).
    public private(set) var folderPlaces: [String: [FfiFolderMember]] = [:]

    /// Per-set device activity (`folder-device-activity-item` rows) keyed by set
    /// name — the ordinary sync-mode change signal (`fauna.folders.devices`,
    /// file-sync.md § Implementation status today), distinct from `folderActors`
    /// (the cross-user roster) and the *Backups* page's `cachedSnapshotCount`/
    /// `cachedTotalBytes` (backup-mode-only). Lazy-loaded on row expand by
    /// `FolderDeviceActivitySection`, and re-fetched on `fauna.sync.changed`
    /// pushes naming this exact set.
    public private(set) var folderDeviceActivity: [String: [DeviceInfo]] = [:]

    /// The "Destination places" list per folder (`folder-destination-row`
    /// rows) keyed by set name — the owner's enrolled backup destinations,
    /// each marked attached-or-not for this folder (`backup-destinations.md`
    /// § Ordinary-folder coverage). Lazy-loaded on
    /// first expand, same shape as `folderDeviceActivity`. Every mutation
    /// re-reads and stores the FFI call's own repaint answer — never an
    /// optimistic flip, mirroring linux/web/android's posture.
    public private(set) var folderDestinations: [String: [FfiFolderDestinationPlace]] = [:]

    /// Transient share / remove failure (`folder-share-button` / `-remove-button`),
    /// surfaced on the page `error-message`. A per-item **read** failure is NOT an
    /// error (owner-only sets answer `not_shared`) — those map to an empty roster.
    public private(set) var sharingError: String?

    /// A macOS-only local-folder-binding refusal (`folder-location-add-button`,
    /// `MacFolderBindingSection`) — the desktop local-folder binding is
    /// `LocationsModel`-driven, not a `DevicesMachine` gesture, so it has no
    /// other route to the page's shared `error-message`. A row whose
    /// `folder.folderRef` is nil is refused rather than bound by name
    /// (`on-demand-files.md` § Hosting multiple on-demand folders, the
    /// name-keyed bind retirement), mirroring tui/linux/windows. iOS never
    /// sets this — no local-folder binding surface there.
    public private(set) var bindLocationError: String?

    /// Set (or clear with `nil`) `bindLocationError` — the write side
    /// `MacFolderBindingSection` uses since the field itself is
    /// `private(set)`, matching every other gesture-error setter on this VM.
    public func setBindLocationError(_ message: String?) {
        bindLocationError = message
    }

    /// The owner-side custody facet rows (`custody-holder-*`, `devices.md` §
    /// Custody facet piece 2) — "who holds my data". NOT part of
    /// `DevicesSnapshot`: the facet folds the owner's `fauna.state.custody-ceremony` entries, which the
    /// keyless `DevicesMachine` cannot read, so it arrives as its own VM-held
    /// state loaded alongside `configure` (mirrors `folderActors`' own
    /// separate-fold reasoning).
    public private(set) var custodyRows: [CustodyHolderRowView] = []

    /// The host-side custody families from the SAME fold as `custodyRows`
    /// (`devices.md` § Custody facet, piece 3): one `custody-held-card` per
    /// custody this device holds for another account, one `custody-offer-card`
    /// per offer waiting on this account's consent.
    public private(set) var custodyHeld: [CustodyHeldRowView] = []
    public private(set) var custodyOffers: [CustodyOfferRowView] = []

    /// The offers (by grant id) whose consent card renders
    /// `custody-offer-target-select` — the shared
    /// `custody_offer_shows_target_select` answer, read on the same edge as the
    /// fold (it needs the nest client; the view never calls FFI).
    public private(set) var custodyOfferTargets: Set<Data> = []

    /// The enrolled principals the shared `devices_keyless_posture` rule marks
    /// relay-only (`device-keyless-posture-badge`, `devices.md` § Custody
    /// facet piece 1). Keyed by principal rather than row index so a roster
    /// re-order between the read and the paint can never move the marker onto
    /// another device. Re-read on the page's load edge, after the refresh.
    public private(set) var keylessPrincipals: Set<String> = []

    /// The standing device-cap refusal notice (`devices.md` § Errors & edge
    /// cases) — painted on `errorMessage` at the lowest precedence while it
    /// stands. NOT part of `DevicesSnapshot` (the keyless machine can't read
    /// the account runtime's credential slot), so it loads alongside
    /// `custodyRows` on the same edge. Only a later load actually returning
    /// `nil` clears it — a transient load failure keeps whatever notice
    /// already stood.
    public private(set) var enrollmentNotice: String?

    public init() {}

    /// The in-flight first build + first load, joined by any configure that
    /// arrives while it runs (two pages appearing at once) — never a second
    /// machine.
    private var pendingBuild: Task<Void, Never>?

    /// Bumped by `reset()`, so a build that was in flight across a reset
    /// discards its machine instead of installing it for the next session.
    private var sessionGeneration: UInt64 = 0

    // ── Lifecycle ──────────────────────────────────────────────────────────

    /// Vend the machine from `APIClient` and load the first snapshot. Idempotent
    /// — the machine is built once per session; later calls only re-`refresh()`.
    /// A call with a DIFFERENT `APIClient` than the one this VM was configured
    /// with is a new session: the old one's state is dropped first (the belt to
    /// `ActorScope.dropAppOwnedState`'s braces).
    public func configure(api: APIClient) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        // A gesture's error answers THAT visit's gesture. With one VM per
        // session it no longer dies with the view, so every visit starts
        // without the last one's — else a refusal greets the next visit and an
        // answer read after a new gesture may be a leftover.
        sharingError = nil
        bindLocationError = nil
        if let pendingBuild {
            // The build's own first load covers this visit too.
            await pendingBuild.value
            return
        }
        guard machine == nil else {
            // The re-entry arm. Logged for the same reason as the build arm
            // below: an empty Folders list must be attributable to a stage,
            // and "configure ran but the page still shows nothing" is a
            // different bug from "configure never ran at all".
            logMessage(level: .info, target: "fauna.devices.vm",
                       message: "configure: machine already built — re-refreshing")
            await refresh()
            await loadPendingShares()
            #if !FAUNA_EXCISE_P2P_SHARE
            await loadOfflineGroupShares()
            #endif
            // Re-read on EVERY appear, not just the first build: mail (and so the MSEK)
            // can be set up while this page is off-screen, and the toggle must open up
            // on the way back — webdav-server.md § Independent enablement pt 2 makes the
            // capability a per-page-visit read precisely for this. Same for the tier
            // names the paywall select offers (a tier can be created off-screen too).
            await loadCanServeWebdav()
            await loadOwnTierNames()
            await loadCustodyFacet()
            await loadEnrollmentNotice()
            return
        }
        let generation = sessionGeneration
        let build = Task { await self.buildAndLoad(api: api, generation: generation) }
        pendingBuild = build
        await build.value
        if sessionGeneration == generation { pendingBuild = nil }
    }

    private func buildAndLoad(api: APIClient, generation: UInt64) async {
        observerBox.target = self
        // Bracketed build: `devicesMachine` awaits the nest connect AND (on
        // apple, unlike tui) the shared ConversationsSession, so it is the one
        // step that can leave `machine == nil` with NO error and no page error
        // — which renders as an empty folder list indistinguishable from an
        // account that owns none.
        logMessage(level: .info, target: "fauna.devices.vm",
                   message: "configure: building the devices machine")
        let built: DevicesMachine
        do {
            built = try await api.devicesMachine(observer: observerBox)
        } catch {
            guard sessionGeneration == generation else { return }
            logMessage(level: .error, target: "fauna.devices.vm",
                       message: "configure: machine build FAILED: \(error)")
            connectError = DisplayError.message(error)
            return
        }
        guard sessionGeneration == generation else {
            logMessage(level: .info, target: "fauna.devices.vm",
                       message: "configure: session reset during the build — discarding its machine")
            return
        }
        connectError = nil
        machine = built
        logMessage(level: .info, target: "fauna.devices.vm",
                   message: "configure: machine built — first refresh")
        await refresh()
        await loadPendingShares()
        #if !FAUNA_EXCISE_P2P_SHARE
        await loadOfflineGroupShares()
        #endif
        await loadDefaultConflictPolicy()
        await loadCanServeWebdav()
        await loadOwnTierNames()
        await loadCustodyFacet()
        await loadEnrollmentNotice()
    }

    /// Drop everything this view model holds for the session it was scoped to —
    /// called by ``ActorScope/dropAppOwnedState(criticalAlertsHost:conversationsVM:feedVM:eventsVM:devicesVM:screenTime:modelContainer:appState:newModelContainer:)``,
    /// the one canonical drop (`account-scoping.md` § The scoping taxonomy),
    /// and by `configure` when handed another session's `APIClient`. Releasing
    /// the machine releases its followed-folders memory and refresh counts with
    /// it; the next `configure` builds the incoming session's own.
    public func reset() {
        sessionGeneration &+= 1
        pendingBuild = nil
        machine = nil
        api = nil
        connectError = nil
        folderActors = [:]
        folderPlaces = [:]
        folderDeviceActivity = [:]
        folderDestinations = [:]
        sharingError = nil
        bindLocationError = nil
        custodyRows = []
        custodyHeld = []
        custodyOffers = []
        custodyOfferTargets = []
        keylessPrincipals = []
        enrollmentNotice = nil
        thisDeviceRow = nil
        pendingShares = []
        #if !FAUNA_EXCISE_P2P_SHARE
        offlineSharePanel = .closed
        offlineSharePeerCodeInput = ""
        offlineShareStatus = .idle
        offlineShareSeat = nil
        groupShareViews = FfiGroupShareViews(invitations: [], scopes: [])
        #endif
        canServeWebdav = false
        ownTierNames = []
        defaultConflictPolicy = nil
        _observerTick &+= 1
    }

    // ── Observer ───────────────────────────────────────────────────────────
    fileprivate func onMachineChanged() {
        // @Observable picks up via the property accesses below; provoke a
        // tracked-property read on the main actor so SwiftUI re-renders.
        _observerTick &+= 1
    }
    private var _observerTick: UInt64 = 0

    // ── Read surface (read freshly on every access) ──────────────────────────

    /// The whole renderable Devices page in one record (devices, folders,
    /// conflicts, embedded wizard, page error). `nil` until `configure`.
    public var snapshot: DevicesSnapshot? {
        _ = _observerTick
        return machine?.snapshot()
    }

    /// The open folder wizard machine, if any — views forward wizard gestures
    /// (`setName` / `next` / `submit` / …) to it; its rendered state is read from
    /// `snapshot.wizard`.
    public var wizardMachine: FolderWizardMachine? {
        _ = _observerTick
        return machine?.wizard()
    }

    /// The page-level `error-message`: the connect failure, then the sharing
    /// error, then the macOS-only bind-location refusal, then the machine
    /// snapshot's localized `error`, then — lowest precedence — the standing
    /// device-cap refusal notice (a roster gesture's own error still wins
    /// while it stands). A computed property, so it recomputes fresh on
    /// every read; no explicit re-assert on hydrate is needed.
    public var errorMessage: String? {
        _ = _observerTick
        return firstNonNil(
            connectError, sharingError, bindLocationError,
            machine?.snapshot().error.map(renderLocalizedText), enrollmentNotice)
    }

    // ── Page gestures ────────────────────────────────────────────────────────

    /// Refresh, then log what the VIEW will actually read. This is the seam
    /// between "the shared machine holds N rows" (the Rust `refresh: snapshot
    /// committed` line) and "the page renders N rows" — a disagreement between
    /// the two is a Swift render/registry bug, an agreement at 0 is upstream.
    public func refresh() async {
        await machine?.refresh()
        let snap = machine?.snapshot()
        logMessage(level: .info, target: "fauna.devices.vm",
                   message: "refresh: vm sees folders=\(snap?.folders.count ?? -1) "
                          + "devices=\(snap?.devices.count ?? -1) "
                          + "machine=\(machine == nil ? "nil" : "built")")
    }

    /// Open the create wizard and stamp the owner's **default conflict policy** onto
    /// it (`FolderWizardMachine::set_default_conflict_policy`, injected right after
    /// open and before submit — the linux/windows idiom), so a newly created set
    /// inherits the `sync-default-conflict-policy-select` value. Best-effort: a
    /// defaults read failure just leaves the nest column default (`auto`) in force.
    public func openWizard() {
        machine?.openWizard()
        Task { [weak self] in
            guard let self else { return }
            if self.defaultConflictPolicy == nil { await self.loadDefaultConflictPolicy() }
            guard let policy = self.defaultConflictPolicy else { return }
            self.wizardMachine?.setDefaultConflictPolicy(policy: policy)
        }
    }
    public func closeWizard() { machine?.closeWizard() }

    public func removeDevice(index: Int) async {
        await machine?.removeDevice(index: UInt32(index))
    }

    /// `device-p2p-participation-toggle[index]` (`p2p.md` § Per-device
    /// participation): the machine decides the arm — this device's own switch,
    /// or an off-request to a sibling — and paints any refusal on the
    /// snapshot's `error`, so `errorMessage` carries it with no VM field. The
    /// machine is handed `thisDeviceRow` first, so a seat whose account runtime
    /// cannot name the row (iOS today) still takes the own arm and says why it
    /// cannot flip, rather than asking the nest to turn itself off.
    public func setP2pParticipation(index: Int, on: Bool) async {
        machine?.setThisDeviceRow(row: thisDeviceRow)
        await machine?.setP2pParticipation(index: UInt32(index), on: on)
    }

    // ── This device's row (devices.md § This-device marker) ────────────────

    /// The roster row `device-this-mark-badge` marks — the shared rule
    /// (`fauna_devices_machine::this_device_row` through the FFI door
    /// `devicesThisDeviceRow`): the row this machine's enrollment latched on,
    /// else the app's own device id. Not the app's id alone: where the
    /// enrollment converged onto another row, that id names no roster row.
    /// The participation toggle's own arm reads the same value, so the switch
    /// and the marker always sit on one row.
    public private(set) var thisDeviceRow: String?

    /// Re-read `thisDeviceRow` — a local slot read, never a network call — and
    /// hand it to the machine. Rides the page's load edge (every appear), since
    /// the latch lands after sign-in and can move on a re-mint.
    public func loadThisDeviceRow(ownDeviceId: String?) async {
        let row = await devicesThisDeviceRow(ownDeviceId: ownDeviceId)
        thisDeviceRow = row
        machine?.setThisDeviceRow(row: row)
    }

    /// The member-addressed door (`device-member-remove-confirm-button`,
    /// `devices.md` § Members without a matching entry): removes a verified
    /// fleet member BY ITS KEY, no nest row touched. `deviceId` is the armed
    /// card's `FleetMemberSummary.deviceId`, so a reshaped list between arm and
    /// confirm cannot retarget the removal.
    public func removeMember(deviceId: String) async {
        await machine?.removeMemberById(deviceId: deviceId)
    }

    public func deleteFolder(name: String) async {
        await machine?.deleteFolder(name: name)
    }

    public func resolveConflict(id: Int64, winningManifestHash: String?) async {
        await machine?.resolveConflict(id: id, winningManifestHash: winningManifestHash)
    }

    public func setFolderPaths(name: String, include: [String]?, exclude: [String]?) async {
        await machine?.setFolderPaths(name: name, includePaths: include, excludePaths: exclude)
    }

    /// Save a folder's **nest-place snapshot policy** — the four `folder-nest-*`
    /// controls, committed together by `folder-nest-save-button`
    /// (`backup-restore.md` § 8b). Drives the shared
    /// `DevicesMachine::set_folder_nest_place`, which refreshes on success.
    ///
    /// ⚠ **Whole-value, not a delta.** Callers pass the full policy they want to
    /// rest, which is what lets a user return a knob to "use the default" — so
    /// the only sanctioned way to build the argument is `nestPlaceWrite` over the
    /// four buffers (retention's `nil` means *leave unchanged*, the one knob that
    /// does not clear by omission; a hand-rolled call gets that backwards).
    ///
    /// `versionRetention` is the place's FOURTH knob, and apple does
    /// (landed 2026-08-25): the version-retention SIBLING pair rides
    /// the same whole-value rule as the four above — `versionRetention` is
    /// built by the caller from `version_retention_write` over the editor's
    /// two buffers and always rides non-`nil` on this call, since the boxes
    /// are on screen (mirrors android's `DevicesVM.setFolderNestPlace` doc:
    /// "the version-retention pair always rides as non-null since the boxes
    /// are on screen"). `nil` stays the documented shape only for an app that
    /// has not built the editor at all — `DevicesMachine::set_folder_nest_place`
    /// reads a `nil` there as **leave unchanged**, never as clear.
    public func setFolderNestPlace(
        name: String,
        snapshots: Bool?,
        quietSecs: Int64?,
        retention: String?,
        versionRetention: VersionRetentionWrite?
    ) async {
        await machine?.setFolderNestPlace(
            name: name,
            snapshots: snapshots,
            quietSecs: quietSecs,
            retention: retention,
            versionRetention: versionRetention
        )
    }

    // ── Owner-side folder Sharing gestures ─────────────────────────────────
    // (docs/goal/ui/folders.md § Sharing; the linux LEAD shape.)

    /// Load the cross-user "Shared with" roster for a shared set into
    /// `folderActors[name]`. Call only for a shared set (`mlsGroupId != nil`) — an
    /// owner-only set never needs the read. ANY failure maps to an empty roster (a
    /// per-item read never blanks the page — goal doc § Sharing), so this never sets
    /// `sharingError`.
    public func loadFolderActors(name: String) async {
        guard let api else { return }
        do {
            folderActors[name] = try await api.folderActorMembers(name: name)
        } catch {
            folderActors[name] = []
        }
    }

    /// Load one folder's device-place roster into `folderPlaces[name]`. ANY
    /// failure maps to an empty roster (a per-item read never blanks the page —
    /// mirrors `loadFolderActors`), so this never sets `sharingError`.
    public func loadFolderPlaces(name: String) async {
        guard let api else { return }
        do {
            folderPlaces[name] = try await api.folderDevicePlaces(
                name: name, devices: snapshot?.devices ?? [])
        } catch {
            folderPlaces[name] = []
        }
    }

    /// Write ONE device's place on a folder — `fauna.folders.places.set` through
    /// `DevicesMachine::set_folder_place`, then a roster RE-READ.
    ///
    /// ⚠ **The point applies WHOLE.** All three flags ride every write, so the
    /// two boxes the user did not touch cannot be dropped on the way to the
    /// wire; the caller composes the next triple from the row it painted.
    ///
    /// ⚠ **Repaint from the NEST, never from the local flip.** The per-seat
    /// flags are not on the page snapshot the write already refreshed, so the
    /// re-read is the only thing that can answer — and a FAILED write lands here
    /// too, re-reading back to the unchanged truth rather than leaving an
    /// optimistic box on screen.
    public func setFolderPlace(
        name: String, deviceId: String, originates: Bool, accepts: Bool, appliesDeletes: Bool
    ) async {
        await machine?.setFolderPlace(
            name: name,
            deviceId: deviceId,
            originates: originates,
            accepts: accepts,
            appliesDeletes: appliesDeletes
        )
        await loadFolderPlaces(name: name)
    }

    /// Enrol this device on a folder it holds no place in — the
    /// `folder-on-demand-toggle` ON gesture (`on-demand-files.md`, *Auto-appear
    /// default-ON*: turning the toggle on "is the **enrol gesture**"). Routes the
    /// shared `ensure_place` (the desktop bind runs the same helper agent-side),
    /// which writes the default place iff `deviceId` holds none and never
    /// rewrites a chosen one. Best-effort like the helper's other callers: a
    /// failure leaves the device place-less until the next gesture. Always
    /// ends on a roster RE-READ — the places section repaints from the nest,
    /// never from an optimistic flip.
    public func ensureFolderPlace(name: String, deviceId: String) async {
        guard let api else { return }
        _ = try? await api.ensureFolderPlace(name: name, deviceIdHex: deviceId)
        await loadFolderPlaces(name: name)
    }

    /// Load the per-set device activity into `folderDeviceActivity[name]`. ANY
    /// failure maps to an empty list (a per-item read never blanks the page —
    /// mirrors `loadFolderActors`), so this never sets `sharingError`.
    public func loadFolderDeviceActivity(name: String) async {
        guard let api else { return }
        do {
            folderDeviceActivity[name] = try await api.listFolderDevices(name: name)
        } catch {
            folderDeviceActivity[name] = []
        }
    }

    /// Share a set with the picked recipient (`folder-share-confirm`). Resolves the
    /// handle + creates the MLS group + delivers the Welcome, then refreshes the
    /// snapshot (so `mlsGroupId` flips to shared) and re-reads the roster. Returns
    /// `true` on success so the sheet can dismiss; a failure surfaces on the page
    /// `error-message` and returns `false`.
    @discardableResult
    public func shareFolder(name: String, recipientInput: String) async -> Bool {
        guard let api else { return false }
        sharingError = nil
        do {
            _ = try await api.shareFolder(name: name, recipientInput: recipientInput)
            await refresh()
            await loadFolderActors(name: name)
            return true
        } catch {
            sharingError = DisplayError.message(error).map { "\(L.devices.errorShareSet): \($0)" }
            return false
        }
    }

    /// Remove a member (`folder-member-remove-button`) — rotates the content key —
    /// then refresh + re-read the roster. The rotated bindings reach this device's
    /// sync agent inside `APIClient.removeFolderMember` (as a share's do inside
    /// `shareFolder`), so a bound owner engine stops sealing under the generation
    /// the removed member holds. `groupIdHex` is the set's `mlsGroupId`
    /// (the remove `ChannelId` is derived from it). Failure surfaces on `error-message`.
    public func removeFolderMember(name: String, memberActorIdHex: String, groupIdHex: String) async {
        guard let api else { return }
        sharingError = nil
        do {
            try await api.removeFolderMember(
                name: name, memberActorIdHex: memberActorIdHex, groupIdHex: groupIdHex)
            await refresh()
            await loadFolderActors(name: name)
        } catch {
            sharingError = DisplayError.message(error).map { "\(L.devices.errorRemoveMember): \($0)" }
        }
    }

    // ── Owner-side folder destination-places gestures ──────────────────────
    // (backup-destinations.md § Ordinary-folder coverage.)

    /// Load the destination-places list for a folder into
    /// `folderDestinations[name]`. ANY failure maps to an empty list (a
    /// per-item read never blanks the page — mirrors `loadFolderActors`), so
    /// this never sets `sharingError`.
    public func loadFolderDestinations(name: String, folderId: Int64) async {
        guard let api else { return }
        do {
            folderDestinations[name] = try await api.listFolderDestinations(folderId: folderId)
        } catch {
            folderDestinations[name] = []
        }
    }

    /// Attach `folderId` to `destinationId` (`folder-destination-attach-button`),
    /// then repaint from the re-read the FFI call already performed.
    public func attachFolderDestination(name: String, folderId: Int64, destinationId: String) async {
        guard let api else { return }
        sharingError = nil
        do {
            folderDestinations[name] = try await api.attachFolderDestination(
                folderId: folderId, destinationId: destinationId)
        } catch {
            sharingError = DisplayError.message(error).map { L.devices.errorFolderDestination(message: $0) }
        }
    }

    /// Detach `folderId` from `destinationId` (`folder-destination-detach-button`).
    /// `folderSet` is the attached row's own `__folder/<hex>/<id>` name,
    /// carried by the `FfiFolderDestinationPlace` the detach button's row was
    /// built from — never re-derived here.
    public func detachFolderDestination(
        name: String, folderId: Int64, destinationId: String, folderSet: String
    ) async {
        guard let api else { return }
        sharingError = nil
        do {
            folderDestinations[name] = try await api.detachFolderDestination(
                folderId: folderId, destinationId: destinationId, folderSet: folderSet)
        } catch {
            sharingError = DisplayError.message(error).map { L.devices.errorFolderDestination(message: $0) }
        }
    }

    // ── Recipient-side folder Sharing (pending-share knocks + shared-with-me) ──
    // (docs/goal/ui/folders.md § Sharing — Recipient side.)
    //
    // A joined shared-with-me set surfaces as an ordinary `FolderSummary` with
    // `role == "member"` — the machine's injected `MlsQuery` join-filter (wired in
    // `APIClient.devicesMachine`) drops any member row this client has NOT MLS-joined,
    // so a stranger's rostered-but-un-accepted knock can only ever appear as a
    // `folder-pending-share`. Leaving such a set rides `leaveFolder` below.

    /// Staged (knocked) cross-user folder shares from strangers — the
    /// `folder-pending-share` "Shared with you" list. A contact's share auto-joins
    /// (B2 gate) so it never appears here. Loaded by `loadPendingShares` (a durable-
    /// inbox **peek**, no ack) on page `configure` + after every accept/decline.
    public private(set) var pendingShares: [FfiPendingShare] = []

    /// Peek the durable inbox for staged folder shares. ANY failure maps to an
    /// empty list (a knock-list read never blanks the page), so this never sets
    /// `sharingError`.
    public func loadPendingShares() async {
        guard let api else { return }
        do {
            pendingShares = try await api.folderPendingShares()
        } catch {
            pendingShares = []
        }
    }

    /// Accept a staged share (`folder-share-accept-button`): join the MLS group off
    /// the chat rail + ack, then refresh the snapshot (the joined set may surface once
    /// member-list-visibility lands) and reload the knock list (the accepted knock
    /// drops out). Failure surfaces on `error-message`.
    public func acceptPendingShare(inboxId: Int64) async {
        guard let api else { return }
        sharingError = nil
        do {
            try await api.acceptFolderShare(inboxId: inboxId)
            await refresh()
            await loadPendingShares()
        } catch {
            sharingError = DisplayError.message(error).map { "\(L.devices.errorAcceptShare): \($0)" }
        }
    }

    /// Decline a staged share (`folder-share-decline-button`): ack-and-drop — never
    /// joins the group — then reload the knock list. Failure surfaces on `error-message`.
    public func declinePendingShare(inboxId: Int64) async {
        guard let api else { return }
        sharingError = nil
        do {
            try await api.declineFolderShare(inboxId: inboxId)
            await loadPendingShares()
        } catch {
            sharingError = DisplayError.message(error).map { "\(L.devices.errorDeclineShare): \($0)" }
        }
    }

    /// Leave a set shared WITH you (`folder-leave-button`, on a `role == "member"`
    /// row). Self-scoped: drops only our roster row + forgets the MLS group locally —
    /// no key rotation (folders.md § Sharing — Leave). On success the row drops out
    /// of the list on the next snapshot, so refresh. Failure surfaces on `error-message`.
    public func leaveFolder(groupIdHex: String) async {
        guard let api else { return }
        sharingError = nil
        do {
            try await api.leaveFolderShare(groupIdHex: groupIdHex)
            await refresh()
        } catch {
            sharingError = DisplayError.message(error).map { "\(L.devices.errorLeaveShare): \($0)" }
        }
    }

    // EXCISED BY `FAUNA_EXCISE_P2P_SHARE` — the `p2p-share` member's ceremony half. Its
    // FFI face still sits in the store-safe flavor, so the compiler cannot catch an
    // ungated reference here; the apple p2p pins in `test_payments_excision_spine.py`
    // do (the reason is written once, at the top of `SharePlaneModel.swift`).
    #if !FAUNA_EXCISE_P2P_SHARE
    // ── Offline co-present share ceremony (folders page) ────────────────────
    // (`offline-share-*`/`offline-receive-*`; docs/goal/behavior/p2p.md §
    // Offline share initiation. Reference: apps/fauna-linux/src/offline_share.rs
    // + its client.rs/app.rs/folders.rs wiring — closer structurally than
    // tui's direct struct access, since apple crosses the same UniFFI
    // boundary windows/android also cross.)

    /// Which panel (if any) is open. `.closed` is the resting state.
    public private(set) var offlineSharePanel: OfflineSharePanel = .closed

    /// The `offline-share-peer-code-input` buffer — a local draft, committed
    /// only by Begin/Expect.
    public var offlineSharePeerCodeInput: String = ""

    /// This side's progress, painted in `offline-share-status`.
    public private(set) var offlineShareStatus: CeremonyStatus = .idle

    /// The bound seat, once a panel opened and the brake allowed it — `nil`
    /// while closed, or if the `p2p-share` brake refused.
    public private(set) var offlineShareSeat: FfiCeremonySeat?

    /// The co-present ceremony's own group-scope listing + consent-card
    /// invitations — loaded alongside `configure` and refreshed after every
    /// accept/decline (mirrors `pendingShares`).
    public private(set) var groupShareViews = FfiGroupShareViews(invitations: [], scopes: [])

    /// The whole paint decision — every offline-share element reads from
    /// here, never from the raw fields above (mirrors linux's
    /// `OfflineShareState::view()`). `nil` only before `api` is set.
    public var offlineShareView: OfflineShareView? {
        guard let api else { return nil }
        return try? api.offlineShareViewSnapshot(
            panel: offlineSharePanel, seat: offlineShareSeat,
            peerCodeInput: offlineSharePeerCodeInput, status: offlineShareStatus)
    }

    /// The typed peer code's parse, re-derived live — `can_begin`/`can_expect`'s
    /// shared half. The FFI Record boundary carries `OfflineShareView`'s
    /// FIELDS but not its Rust-side helper methods (`shows_entry_buttons` /
    /// `can_begin` / …, `impl OfflineShareView` in `group_ceremony_view.rs`
    /// is not `#[uniffi::export]`ed) — this re-derives them from the exported
    /// `offline_share_parse_peer_code` door rather than duplicating the
    /// parser itself.
    public var offlineSharePeerCodeParsed: PeerCodeParsed? {
        try? api?.parseOfflinePeerCode(input: offlineSharePeerCodeInput)
    }

    /// `offline-share-button` / `offline-receive-button` — open the panel.
    /// Opening while ALREADY bound is a pure flip: binding hands the
    /// existing seat straight back rather than opening a second listener
    /// (one actor-keyed endpoint per session, mirrors linux's
    /// `bind_offline_share_seat`).
    public func openOfflineSharePanel(_ panel: OfflineSharePanel) {
        offlineSharePanel = panel
        offlineSharePeerCodeInput = ""
        offlineShareStatus = .idle
        guard offlineShareSeat == nil, let api else { return }
        Task { [weak self] in
            guard let self else { return }
            do {
                self.offlineShareSeat = try await api.bindOfflineShareSeat()
                await self.loadOfflineGroupShares()
            } catch {
                self.sharingError = DisplayError.message(error).map { L.folders.errorOfflineShare(message: $0) }
            }
        }
    }

    /// `offline-share-cancel-button` — close the panel, and on the recipient
    /// side withdraw the expectation (rule 6: the user changed their mind).
    /// The SEAT stays bound: the listener is this session's, not this
    /// ceremony's, and re-binding an endpoint per cancel would be churn.
    public func cancelOfflineSharePanel() {
        if offlineSharePanel == .receive, let seat = offlineShareSeat,
           let peer = offlineSharePeerCodeParsed, !peer.actor.isEmpty {
            try? seat.cancelExpectation(initiator: peer.actor)
        }
        offlineSharePanel = .closed
        offlineSharePeerCodeInput = ""
        offlineShareStatus = .idle
    }

    /// `offline-share-begin-button` — the whole initiator walk. No
    /// optimistic status change: `beginOfflineShare` drives the whole walk
    /// to its final status in one round trip. A deliver that crosses lands a
    /// scope this device can list — re-read the group listing on that edge so
    /// the shared set lists on the Folders page the initiator is looking at,
    /// not only after they navigate away and back (mirrors `consentToGroupShare`'s
    /// own reload; `p2p.md` § Offline share initiation).
    public func beginOfflineShare() async {
        guard let api, let seat = offlineShareSeat else { return }
        do {
            let status = try await api.beginOfflineShare(
                seat: seat, peerCodeInput: offlineSharePeerCodeInput)
            offlineShareStatus = status
            if offlineShareStatusLandsAScope(status: status) {
                await loadOfflineGroupShares()
            }
        } catch {
            offlineShareStatus = .failed
            sharingError = DisplayError.message(error).map { L.folders.errorOfflineShare(message: $0) }
        }
    }

    /// `offline-receive-expect-button` — the receive act. Synchronous:
    /// minting the expectation is an in-memory write on the live seat, true
    /// the instant the user says so, before the initiator's offer can
    /// arrive. The button is disabled while this can't succeed, so reaching
    /// the failure arm here is the belt to that braces — never a silent
    /// drop (e2e convention 11).
    public func expectOfflineShare() {
        guard let seat = offlineShareSeat, let peer = offlineSharePeerCodeParsed,
              !peer.actor.isEmpty
        else {
            offlineShareStatus = .failed
            return
        }
        do {
            try seat.expectFrom(initiator: peer.actor)
            offlineShareStatus = .expecting
        } catch {
            offlineShareStatus = .failed
            sharingError = DisplayError.message(error).map { L.folders.errorOfflineShare(message: $0) }
        }
    }

    /// Peek the co-present ceremony's group-scope listing + consent-card
    /// invitations. ANY failure maps to the empty default (a knock-list read
    /// never blanks the page — mirrors `loadPendingShares`).
    public func loadOfflineGroupShares() async {
        guard let api else { return }
        groupShareViews = await api.loadOfflineGroupShares(seat: offlineShareSeat)
    }

    /// The consent card's Accept (`folder-share-accept-button`, the group
    /// arm) — needs an already-bound seat (the row's own 2026-08-20 finding);
    /// a card rendered with none is a dead control, the same silence the two
    /// entry buttons keep in that state (mirrors linux's `Op::AcceptGroupShare`).
    public func consentToGroupShare(scopeId: Data) async {
        guard let api, let seat = offlineShareSeat else { return }
        do {
            offlineShareStatus = try await api.consentToOfflineGroupShare(
                seat: seat, scopeId: scopeId)
            await loadOfflineGroupShares()
        } catch {
            offlineShareStatus = .failed
            sharingError = DisplayError.message(error).map { L.folders.errorOfflineShare(message: $0) }
        }
    }

    /// The consent card's Decline (`folder-share-decline-button`, the group
    /// arm) — needs an already-bound seat, same as Accept.
    public func declineGroupShare(scopeId: Data) async {
        guard let api, let seat = offlineShareSeat else { return }
        do {
            try await api.declineOfflineGroupShare(seat: seat, scopeId: scopeId)
            await loadOfflineGroupShares()
        } catch {
            sharingError = DisplayError.message(error).map { L.folders.errorOfflineShare(message: $0) }
        }
    }
    #endif

    // ── WebDAV serving (the per-set opt-in) ──────────────────────────────────
    // (`folder-webdav-toggle`; webdav-server.md § Independent enablement point 2.)

    /// Whether this actor can serve ANY set over WebDAV (i.e. holds an MSEK). Read per
    /// page-visit off the **key-bearing** face and cached here, because it is NOT on the
    /// keyless `DevicesMachine` snapshot — so every row can gate its toggle without a
    /// per-row FFI read.
    ///
    /// **Fail-closed** (`false` until the face answers `true`): `serve_set` flips the nest
    /// flag BEFORE re-provisioning the keys blob, so an enable by an actor with no MSEK
    /// would commit the flag and only then fail `NoMsek`. A toggle that is merely
    /// error-on-click would therefore leave the nest inconsistent — it must be disabled.
    public private(set) var canServeWebdav: Bool = false

    /// Re-read the capability. A failed/absent read leaves it CLOSED (never opens the
    /// toggle on an unreadable answer). Driven from the page's `.task`, so it re-reads on
    /// every appear — mail can be set up while the page is off-screen.
    public func loadCanServeWebdav() async {
        guard let api else { return }
        canServeWebdav = (try? await api.canServeWebdav()) ?? false
    }

    /// Flip one set's WebDAV serve state (`folder-webdav-toggle`, every OWNER row).
    /// A flip rotates/migrates content-key custody, so the row's `webdavEnabled` only
    /// re-reads after a refresh. Failure surfaces on `error-message`.
    public func serveFolderWebdav(name: String, mlsGroupIdHex: String?, enable: Bool) async {
        guard let api else { return }
        sharingError = nil
        do {
            _ = try await api.serveFolderWebdav(
                name: name, mlsGroupIdHex: mlsGroupIdHex, enable: enable)
            await refresh()
        } catch {
            sharingError = DisplayError.message(error).map { L.devices.errorServeWebdav(message: $0) }
        }
    }

    // ── Web paywall (the per-set tier select) ────────────────────────────────
    // (`folder-paywall-tier-select`, website-enabled owner rows; folders.md § Web paywall.)

    /// The creator's own subscription tier NAMES — the option set each website-enabled row's
    /// `folder-paywall-tier-select` offers. Read per page-visit off the key-bearing
    /// face (the keyless `DevicesMachine` can't own it — same pattern as
    /// `canServeWebdav`); a failed read degrades to empty ⇒ the select renders
    /// disabled with the "create a tier first" hint. Never gates the page.
    public private(set) var ownTierNames: [String] = []

    /// Re-read the tier names (driven beside `loadCanServeWebdav` on every
    /// configure/appear — a tier can be created while this page is off-screen).
    public func loadOwnTierNames() async {
        guard let api else { return }
        ownTierNames = ((try? await api.listSubscriptionTiers()) ?? []).map(\.name)
    }

    /// Paywall one website-enabled set to `tier` (v1 SET-ONLY — no clear path). The row's
    /// `webPaywallTier` only re-reads after a refresh; failure surfaces on
    /// `error-message` via the same page error the other sharing writes use.
    public func paywallFolder(name: String, mlsGroupIdHex: String?, tier: String) async {
        guard let api else { return }
        sharingError = nil
        do {
            try await api.paywallFolder(name: name, mlsGroupIdHex: mlsGroupIdHex, tier: tier)
            await refresh()
        } catch {
            sharingError = DisplayError.message(error).map { "\(L.devices.errorPaywallSet): \($0)" }
        }
    }

    // ── T16 custody facet (devices.md § Custody facet, pieces 1–3 + the mint) ──

    /// Fire a ceremony drive pass, then refold the custody facet into all
    /// three row families, and re-read piece 1's keyless posture. Rides the
    /// page's own load edge (called alongside `configure`, on every appear,
    /// after the roster `refresh()`) — android's `loadCustodyFacet` twin. The
    /// drive comes FIRST and is fire-and-forget: it deposits nothing to await,
    /// so a receipt landed since the last visit is already in this pass's
    /// rows rather than the next one's. A failed/absent fold is a transient
    /// (unreadable config, not connected yet) and keeps the previous rows
    /// rather than blanking live ones — this never touches `sharingError`.
    public func loadCustodyFacet() async {
        guard let api else { return }
        await api.driveCustody()
        if let facet = try? await api.loadCustodyFacet() {
            await applyCustodyFacet(facet, api: api)
        }
        await loadKeylessPosture()
    }

    /// Paint one re-folded facet — the load edge's and every act's. The
    /// target-select answer is read per offer here, beside the rows it
    /// decorates, so a card never renders against a stale answer.
    private func applyCustodyFacet(_ facet: CustodyFacetView, api: APIClient) async {
        var targets: Set<Data> = []
        for offer in facet.offers where await api.custodyOfferShowsTarget(offer) {
            targets.insert(offer.grantId)
        }
        custodyRows = facet.rows
        custodyHeld = facet.held
        custodyOffers = facet.offers
        custodyOfferTargets = targets
    }

    /// Piece 1: which enrolled rows hold no keys, through the shared
    /// `fauna_devices_machine::keyless_posture` rule (every fail-safe — an
    /// unresolved tip, a row no principal has enrolled on — answers `false`, so the
    /// marker never rests on an unknown).
    private func loadKeylessPosture() async {
        let principals = (snapshot?.devices ?? []).map(\.principal)
        let keyless = await devicesKeylessPosture(principals: principals)
        keylessPrincipals = Set(zip(principals, keyless).compactMap { principal, isKeyless in
            isKeyless ? principal : nil
        })
    }

    /// Run one host-side or mint act and answer it: repaint from the re-folded
    /// facet (a `nil` facet keeps the painted rows) and put the act's error on
    /// `error-message` through `sharingError` (never swallowed — e2e
    /// convention 11). Returns whether the act succeeded.
    @discardableResult
    private func runCustodyAct(_ act: (APIClient) async throws -> FfiCustodyActOutcome) async -> Bool {
        guard let api else { return false }
        sharingError = nil
        do {
            let outcome = try await act(api)
            if let facet = outcome.facet { await applyCustodyFacet(facet, api: api) }
            sharingError = outcome.error
            return outcome.error == nil
        } catch {
            sharingError = DisplayError.message(error)
            return false
        }
    }

    /// `custody-offer-accept-button` — `onNest` is the target select's answer
    /// (always `false` where the select is absent).
    public func acceptCustody(grantId: Data, onNest: Bool) async {
        await runCustodyAct { try await $0.acceptCustody(grantId: grantId, onNest: onNest) }
    }

    /// `custody-offer-decline-button`.
    public func declineCustody(grantId: Data) async {
        await runCustodyAct { try await $0.declineCustody(grantId: grantId) }
    }

    /// `custody-held-budget-input` commit. The typed text is parsed by the
    /// shared `parseByteSize` — never an app-side parser — and an unparseable
    /// (or zero) budget makes no call and says so, tui's and linux's answer.
    public func setCustodyBudget(grantId: Data, typed: String) async {
        guard let cap = parseByteSize(input: typed), cap > 0 else {
            sharingError = L.backups.backupDestinationCapacityInvalid
            return
        }
        await runCustodyAct { try await $0.setCustodyBudget(grantId: grantId, cap: cap) }
    }

    /// `custody-held-stop-button`.
    public func stopCustody(grantId: Data) async {
        await runCustodyAct { try await $0.stopCustody(grantId: grantId) }
    }

    /// `custody-held-remove-button`.
    public func removeCustody(grantId: Data) async {
        await runCustodyAct { try await $0.removeCustody(grantId: grantId) }
    }

    /// `custody-mint-button` — the host options for the mint flow, or `nil`
    /// when there is no one to ask: the page then keeps the flow closed and
    /// `error-message` says to start a conversation first, rather than
    /// offering an empty picker whose confirm can never succeed.
    public func openCustodyMint() async -> [CustodyMintCandidateView]? {
        guard let api else { return nil }
        sharingError = nil
        do {
            let candidates = try await api.loadCustodyMintCandidates()
            if candidates.isEmpty {
                sharingError = L.devices.custodyMintNoContacts
                return nil
            }
            return candidates
        } catch {
            sharingError = DisplayError.message(error)
            return nil
        }
    }

    /// `custody-mint-confirm-button` over one `openCustodyMint` row, passed
    /// back unchanged. Returns whether the offer was recorded, so the page
    /// closes the flow only when something was sent.
    public func mintCustody(_ candidate: CustodyMintCandidateView) async -> Bool {
        await runCustodyAct { try await $0.mintCustody(host: candidate.host, channelHex: candidate.channelHex) }
    }

    // ── Device-cap refusal notice (devices.md § Errors & edge cases) ─────────

    /// Refold the standing device-cap refusal notice into `enrollmentNotice`.
    /// Rides the same load edge as `loadCustodyFacet` (called alongside
    /// `configure`, on every appear). `accountEnrollmentNotice()` is
    /// non-throwing, so a plain assignment is exactly the required semantics:
    /// a genuine `nil` (the refusal has cleared) actually clears the field —
    /// unlike `loadCustodyFacet`'s `try?` fold, there is no thrown-error case
    /// to distinguish from a successful empty read here.
    public func loadEnrollmentNotice() async {
        guard let api else { return }
        enrollmentNotice = await api.accountEnrollmentNotice()
    }

    /// Revoke a custody grant (`custody-holder-revoke-button`). Takes the
    /// row's **grant id and accept-bound custodian key** — never a row index,
    /// which a refold can re-point at a different custody. Error routes to
    /// `sharingError` (never swallowed — e2e convention 11); the re-folded
    /// facet lands straight into `custodyRows` so the revoked row leaves
    /// without waiting for a second read.
    public func revokeCustody(grantId: Data, holder: Data?) async {
        guard let api else { return }
        sharingError = nil
        do {
            let outcome = try await api.revokeCustody(grantId: grantId, holder: holder)
            if let facet = outcome.facet { await applyCustodyFacet(facet, api: api) }
            if let message = outcome.error {
                sharingError = L.devices.errorRevokeCustody(message: message)
            }
        } catch {
            sharingError = DisplayError.message(error).map { L.devices.errorRevokeCustody(message: $0) }
        }
    }

    // ── Conflicts (review list) ──────────────────────────────────────────────
    // (docs/goal/ui/folders.md § Conflicts; mechanism owner file-sync.md § Conflicts.)
    //
    // Conflicts AUTO-RESOLVE on the detecting device and the losing version is always
    // retained in version history, so this surface is a REVIEW LIST, never a blocking
    // chooser: one `conflict-resolve-button` = "use the other version", which is
    // exactly the version-restore re-point (and so is itself reversible).

    /// Re-point a resolved conflict at the OTHER version (`conflict-resolve-button`).
    /// `deviceId` is THIS client's recording device (the Media-restore idiom) — the
    /// restore is attributed to the device performing it, not to the losing candidate.
    /// A missing device id is a no-op rather than an error (nothing to attribute to).
    public func useOtherVersion(conflictId: Int64) async {
        guard let machine, let deviceId = FaunaAccounts.sessionMaterial()?.deviceId, !deviceId.isEmpty
        else { return }
        await machine.useOtherVersion(conflictId: conflictId, deviceId: deviceId)
    }

    // ── Conflict policy (per-set + the page-level default) ───────────────────

    /// Set one folder's conflict policy (`folder-conflict-policy-select`, every
    /// owner row): `"auto"` (merge text-like files, else latest-wins) | `"latest_wins_always"`.
    public func setFolderConflictPolicy(name: String, policy: String) async {
        await machine?.setFolderConflictPolicy(name: name, conflictPolicy: policy)
    }

    // ── Following a public folder (`folder-follow-*`, `folder-followed-*`,
    // `folder-unfollow-button`) ─────────────────────────────────────────────
    // (ui/folders.md § Following a public folder.)

    /// Follow a public folder by owner (handle or 64-hex actor id) + plaintext
    /// name, then `refresh()` so the row repaints WITH its availability — the
    /// `followed` rows and their `available` flag come from `DevicesSnapshot`,
    /// which the follow write does not itself move.
    ///
    /// A failure surfaces on the page's `error-message` through `sharingError`.
    /// The message is the shared catalog's single ratified not-found wording
    /// when the address did not resolve: absent, private and misspelled are
    /// folded by the home nest and stay folded here, so this must never try to
    /// tell them apart.
    ///
    /// Returns `true` on success, so the caller can close its form only when
    /// something was actually stored.
    @discardableResult
    public func followFolder(owner: String, folderName: String) async -> Bool {
        guard let api else { return false }
        sharingError = nil
        do {
            _ = try await api.followPublicFolder(owner: owner, folderName: folderName)
            await refresh()
            return true
        } catch {
            sharingError = DisplayError.message(error)
            return false
        }
    }

    /// Stop following — addressed by the follow's pinned identity, never its
    /// display name. Idempotent, and a local removal only.
    public func unfollowFolder(homeNestUrl: String, folderId: Int64) async {
        guard let api else { return }
        sharingError = nil
        do {
            try await api.unfollowPublicFolder(homeNestUrl: homeNestUrl, folderId: folderId)
            await refresh()
        } catch {
            sharingError = DisplayError.message(error)
        }
    }

    // ── Audience + website serving (`folder-audience-select`,
    // `folder-website-toggle`; every owner row) ─────────────────────────────
    // (folders re-model phase 4 slice 4d; ui/folders.md § Audience and website
    // serving owns the UX, behavior/folders.md § Target re-model the model.)

    /// Set one folder's audience — `"private"` | `"shared"` | `"public"`, the
    /// canonical wire values `audienceOptions()` mints and `fauna.folders.update`
    /// accepts.
    ///
    /// **Keyless for every direction the picker offers**, the bound `→shared`
    /// flip-back included: a plain `fauna.folders.update`, *not* the
    /// `FoldersAuthor` orchestration the WebDAV and paywall toggles beside it
    /// run — nothing here touches a content key. The back-catalogue is moved by
    /// each device's own engine at its next catch-up or rescan tick off the
    /// projected audience (`SyncEngine::converge_corpus_to_audience`), never by
    /// the caller. `DevicesMachine::set_folder_audience` refreshes on success,
    /// exactly as `setFolderResidency` below does.
    public func setFolderAudience(name: String, audience: String) async {
        await machine?.setFolderAudience(name: name, audience: audience)
    }

    /// Publish (or stop publishing) this folder's head as the actor's website.
    ///
    /// Orthogonal to the audience beside it — the flag publishes the HEAD, the
    /// audience decides who may READ it — so the toggle is never disabled on a
    /// folder that is neither public nor paywalled; it is merely inert there and
    /// the shared tri-state `websiteServeHint` says so. Keyless like the
    /// audience: a plain `fauna.folders.update`, NOT the `serve_set`
    /// orchestration `serveFolderWebdav` runs.
    public func setFolderWebsiteEnabled(name: String, enabled: Bool) async {
        await machine?.setFolderWebsiteEnabled(name: name, enabled: enabled)
    }

    // ── Content residency (`folder-nest-residency-select`, every row) ────────
    // (folders re-model phase 5; file-sync.md § Content residency owns the
    // model.)

    /// Set one folder's content residency — `"full"` (default; the nest keeps
    /// a copy) | `"metadata_only"` (consent-gated: the nest deletes its copy
    /// on this write). Applies ON CHANGE, its own `fauna.folders.update`
    /// field, deliberately outside the batched `setFolderNestPlace` — mirrors
    /// `setFolderConflictPolicy` exactly (`DevicesMachine.setFolderResidency`
    /// is the machine-level sibling UniFFI export).
    public func setFolderResidency(name: String, residency: String) async {
        await machine?.setFolderResidency(name: name, residency: residency)
    }

    /// The page-level default conflict policy stamped onto NEW sets
    /// (`sync-default-conflict-policy-select`), sealed into the owner's encrypted
    /// `fauna.state.sync-prefs`. `nil` until loaded / never set (nest default `auto`).
    public private(set) var defaultConflictPolicy: String?

    /// Load the stored default. A read failure maps to `nil` (the select falls back to
    /// `auto`) — a defaults read never blanks the page.
    public func loadDefaultConflictPolicy() async {
        guard let api else { return }
        defaultConflictPolicy = (try? await api.defaultConflictPolicy()) ?? nil
    }

    /// Persist the default for new sets. Failure surfaces on `error-message`.
    public func setDefaultConflictPolicy(_ policy: String) async {
        guard let api else { return }
        sharingError = nil
        do {
            defaultConflictPolicy = try await api.setDefaultConflictPolicy(policy)
        } catch {
            sharingError = DisplayError.message(error).map { "\(L.devices.errorSetDefaultConflictPolicy): \($0)" }
        }
    }
}

/// Trampoline conforming to UniFFI's `DevicesObserver`. The machine takes the
/// observer at construction time (`build_devices_machine`), so late-binding via
/// `target` lets the VM register itself after `configure`. Mirrors
/// `OnboardingVM`'s `ObserverBox`.
final class DevicesObserverBox: DevicesObserver, @unchecked Sendable {
    weak var target: DevicesMachineVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onMachineChanged() }
    }
}
