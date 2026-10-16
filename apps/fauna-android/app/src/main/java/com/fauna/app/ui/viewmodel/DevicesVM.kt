package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.app.core.ResolveService
import com.fauna.app.ui.util.firstListingRefusal
import com.fauna.app.core.SecureStorage
import com.fauna.app.p2pshare.GroupShareRows
import com.fauna.app.p2pshare.OfflinePanel
import com.fauna.app.p2pshare.OfflineShareDecision
import com.fauna.app.p2pshare.OfflineShareHost
import com.fauna.app.provider.OnDemandToggle
import com.fauna.ffi.FfiFolderActorMember
import com.fauna.ffi.FfiFolderDevice
import com.fauna.ffi.FfiPendingShare
import com.fauna.ffi.actorIdFromSecret
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.fauna_client_capabilities.CustodyFacetView
import uniffi.fauna_client_capabilities.CustodyHeldRowView
import uniffi.fauna_client_capabilities.CustodyMintCandidateView
import uniffi.fauna_client_capabilities.CustodyOfferRowView
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_devices_machine.DevicesMachine
import uniffi.fauna_devices_machine.DevicesObserver
import uniffi.fauna_devices_machine.DevicesSnapshot
import uniffi.fauna_devices_machine.FolderSummary
import uniffi.fauna_folders_machine.FolderWizardStep
import javax.inject.Inject

/**
 * View-model for the Devices page (`docs/goal/ui/devices.md`). Holds the shared
 * page-level [DevicesMachine] (built over the session's WS-RPC connection via
 * [ApiClient.buildDevicesMachine]) and renders the whole page off its
 * [DevicesSnapshot] — devices, folders, conflicts, and the embedded folder
 * creation wizard. **No page or wizard logic client-side**: every read is a
 * snapshot getter and every gesture forwards to the machine (priority #2,
 * observer-driven rendering per the doc's § Architectural rules).
 *
 * Mirrors the [com.fauna.app.core.OnboardingHost] observer→state pattern: a
 * registered [DevicesObserver] republishes `machine.snapshot()` to [snapshot] on
 * every tick (including the wizard's own ticks, which the machine bridges to the
 * page observer), so Compose recomposes the page and the embedded wizard
 * together. The stateless `DevicesContent` renders [snapshot]; this VM is the
 * thin wrapper the navigation graph mounts.
 */
@HiltViewModel
class DevicesVM @Inject constructor(
    private val api: ApiClient,
    private val resolveService: ResolveService,
    private val secureStorage: SecureStorage,
    @ApplicationContext private val appContext: Context,
    // The co-present offline-share ceremony's session-long state (seat, panel,
    // acts in flight) — app-scoped, never this VM's: see [OfflineShareHost].
    private val offlineShare: OfflineShareHost,
    // The on-demand presence the `folder-on-demand-toggle` steers (the shared
    // preference store + plan, behind the provider's source).
    private val onDemandToggle: OnDemandToggle,
) : ViewModel() {

    private val _snapshot = MutableStateFlow<DevicesSnapshot?>(null)
    /** The whole renderable Devices page; null until the machine is built + first refresh. */
    val snapshot: StateFlow<DevicesSnapshot?> = _snapshot

    /**
     * This app's own locally-stored device id — drives the `device-this-mark-badge`
     * row match (`devices.md` § This-device marker). [SecureStorage.resolveDeviceId] mints
     * and persists on first call, so this is stable for the VM's lifetime.
     */
    val localDeviceId: String? get() = secureStorage.resolveDeviceId()

    /**
     * This client's own actor ID — drives the page-level `peer-actor-id-copy-btn`
     * (`devices.md` § Layout & flow point 2: a single, non-per-row copy button for
     * handing this id to a new device being paired). Same derivation as
     * [AccountSettingsVM.actorId].
     */
    val actorId: String?
        get() = secureStorage.secretHex?.let {
            try { HexUtil.bytesToHex(actorIdFromSecret(HexUtil.hexToBytes(it))) }
            catch (_: Exception) { null }
        }

    // ── Cross-user sharing (owner side) — folders.md § Sharing ──
    //
    // The per-set "Shared with" roster (fauna.folders.members.list_actors) is NOT
    // part of DevicesSnapshot — it's a separate read living on the VM, keyed by set
    // name (owner + members; the view filters to role == "member"). Mirrors apple
    // DevicesMachineVM.folderActors.
    private val _folderActors = MutableStateFlow<Map<String, List<FfiFolderActorMember>>>(emptyMap())
    val folderActors: StateFlow<Map<String, List<FfiFolderActorMember>>> = _folderActors

    // ── Per-set device activity — file-sync.md § Implementation status today ──
    //
    // The ordinary sync per-device change signal (fauna.folders.devices),
    // distinct from folderActors above (cross-USER sharing) — this is per-DEVICE.
    // Same "separate read living on the VM, keyed by set name" shape as
    // folderActors: NOT part of DevicesSnapshot (the keyless DevicesMachine can't
    // own a per-device read either). Mirrors web's `deviceActivity` /
    // linux's/tui's `device_activity` state.
    private val _folderDeviceActivity = MutableStateFlow<Map<String, List<FfiFolderDevice>>>(emptyMap())
    val folderDeviceActivity: StateFlow<Map<String, List<FfiFolderDevice>>> = _folderDeviceActivity

    // ── Destination places (backup-destinations.md § Ordinary-folder coverage) ──
    //
    // Which of the owner's enrolled backup destinations are attached to each
    // ordinary folder — the folders page's *Destination places* section. Same
    // "separate read living on the VM, keyed by set name" shape as
    // folderDeviceActivity above (not part of DevicesSnapshot). Mirrors web's
    // `destinationPlaces` / linux's `populate_folder_destinations`.
    private val _folderDestinationPlaces =
        MutableStateFlow<Map<String, List<com.fauna.ffi.FfiFolderDestinationPlace>>>(emptyMap())
    val folderDestinationPlaces: StateFlow<Map<String, List<com.fauna.ffi.FfiFolderDestinationPlace>>> =
        _folderDestinationPlaces

    // ── On-demand presence (`folder-on-demand-toggle`, on-demand-files.md
    // § Android SAF DocumentsProvider binding) ──
    //
    // Each row's "Show in Files" reading — the device-local preference (default
    // ON) and whether a watched-directory ingress presents the set instead —
    // keyed by [OnDemandRowKey]. Not part of DevicesSnapshot: the preference is
    // this device's, never nest state. A row absent here renders no toggle (no
    // session, or a row that resolves no set ref).
    private val _onDemand = MutableStateFlow<Map<OnDemandRowKey, OnDemandRow>>(emptyMap())
    val onDemand: StateFlow<Map<OnDemandRowKey, OnDemandRow>> = _onDemand

    // Which rows the Compose screen currently has expanded — a VM-side shadow of
    // FolderRow's own local `expanded` boolean (Compose state the VM has no other
    // visibility into), fed by [setFolderExpanded]. Its ONLY purpose is gating the
    // fauna.sync.changed tick handler below so a collapsed row's device-activity
    // read is never wastefully re-fired — the android twin of linux's
    // `folder_row_is_expanded` (which reads the real GTK widget state) / web's
    // `if (expandedFs) void loadDeviceActivity(expandedFs)` (a component-local
    // reactive var the push handler closes over directly). Android's push handler
    // lives on the VM, not the Composable (every other `*Tick` collector in this
    // app is VM-owned — ApiClient.kt's push-dispatch doc), so the VM needs its own
    // copy of "which rows are expanded" rather than reading Compose state directly.
    private val _expandedFolders = MutableStateFlow<Set<String>>(emptySet())

    // Transient share/remove failure (a plain FFI-raised string, prefixed by the VM);
    // the Screen surfaces it on the page error banner then it clears on next success.
    private val _sharingError = MutableStateFlow<String?>(null)
    val sharingError: StateFlow<String?> = _sharingError

    // The standing enrollment-refusal notice (devices.md § Errors & edge cases;
    // today's one refusal is the tier device cap, devices.md § Step 4) —
    // Some(sentence) while the nest refuses to enroll this machine. NOT part of
    // DevicesSnapshot (the keyless DevicesMachine can't read the account
    // runtime) — a separate read living on the VM, the same "rides start(),
    // beside the machine refresh" shape as folderActors/custodyRows above.
    private val _enrollmentNotice = MutableStateFlow<String?>(null)
    val enrollmentNotice: StateFlow<String?> = _enrollmentNotice

    // Whether this actor can serve a set over WebDAV at all — it needs the MSEK that
    // serving seals the WebdavKeysBlob under, minted when mail is first enabled. NOT a
    // DevicesSnapshot field: the shared DevicesMachine is deliberately keyless and
    // cannot read `fauna.state.mail`, so this is a separate read off the key-bearing
    // ApiClient (the shared owner_can_serve_webdav), like folderActors above.
    // Gates each owner row's folder-webdav-toggle — disabled + hinted rather than
    // clicking into a NoMsek that has already committed the nest flag
    // (webdav-server.md § Independent enablement point 2). Starts false: a control
    // that cannot succeed is not offered.
    private val _canServeWebdav = MutableStateFlow(false)
    val canServeWebdav: StateFlow<Boolean> = _canServeWebdav

    // The creator's own subscription tier names (ascending by rank) — the option set
    // `folder-paywall-tier-select` (website-enabled rows) offers. Same page-level-read shape
    // as canServeWebdav above (the keyless DevicesMachine can't own it): fetched off
    // the key-bearing ApiClient, re-read on every refresh() (folders.md § Web
    // paywall). A failed read degrades to empty — the select renders disabled with a
    // "create a tier first" hint, since there is nothing to paywall to.
    private val _ownTiers = MutableStateFlow<List<String>>(emptyList())
    val ownTiers: StateFlow<List<String>> = _ownTiers

    // The owner's default conflict policy for new folders
    // (`sync-default-conflict-policy-select`'s persisted value) — a page-level
    // `fauna.state.sync-prefs` read/write, NOT part of DevicesSnapshot (like ownTiers /
    // canServeWebdav above). Null = no preference (renders as "auto").
    private val _defaultConflictPolicy = MutableStateFlow<String?>(null)
    val defaultConflictPolicy: StateFlow<String?> = _defaultConflictPolicy

    // ── Cross-user sharing (recipient side) — folders.md § Sharing, Recipient side ──
    //
    // Staged (knocked) shares awaiting accept/decline — like folderActors, a separate
    // read living on the VM (not part of DevicesSnapshot).
    private val _pendingShares = MutableStateFlow<List<FfiPendingShare>>(emptyList())
    val pendingShares: StateFlow<List<FfiPendingShare>> = _pendingShares

    // ── T16 custody facet — devices.md § Custody facet, pieces 1–3 + the mint ──
    //
    // NOT part of DevicesSnapshot: the facet folds the `fauna.state.custody-ceremony` entries, which the
    // deliberately-keyless DevicesMachine cannot read — so it rides its own load
    // beside the machine refresh, the same "separate read living on the VM" shape
    // as folderActors / pendingShares above (and the same reasoning behind linux's
    // own `connect_map` hydrate rather than a DevicesMachine observer tick).
    //
    // Starts empty and stays whatever it last held on an unreadable-config pass:
    // `custodyFacetLoad` answering null is a transient, and blanking live rows over
    // it would tell the owner their custodians are gone.
    private val _custodyRows =
        MutableStateFlow<List<uniffi.fauna_client_capabilities.CustodyHolderRowView>>(emptyList())
    val custodyRows: StateFlow<List<uniffi.fauna_client_capabilities.CustodyHolderRowView>> =
        _custodyRows

    // Piece 3, from the SAME fold as [custodyRows]: one `custody-held-card` per
    // custody this device holds for another account, one `custody-offer-card`
    // per offer waiting on this account's consent. Same keep-on-transient rule.
    private val _custodyHeld = MutableStateFlow<List<CustodyHeldRowView>>(emptyList())
    val custodyHeld: StateFlow<List<CustodyHeldRowView>> = _custodyHeld
    private val _custodyOffers = MutableStateFlow<List<CustodyOfferRowView>>(emptyList())
    val custodyOffers: StateFlow<List<CustodyOfferRowView>> = _custodyOffers

    // The offers (by lowercase-hex grant id) whose consent card renders
    // `custody-offer-target-select` — the shared `custody_offer_shows_target_select`
    // answer, read beside the rows it decorates on every fold. Never derived from
    // the offer row's own fields.
    private val _custodyOfferTargets = MutableStateFlow<Set<String>>(emptySet())
    val custodyOfferTargets: StateFlow<Set<String>> = _custodyOfferTargets

    // Piece 1: the enrolled principals the shared `devices_keyless_posture` rule
    // marks relay-only (`device-keyless-posture-badge`).
    private val _keylessPrincipals = MutableStateFlow<Set<String>>(emptySet())
    val keylessPrincipals: StateFlow<Set<String>> = _keylessPrincipals

    // The `custody-mint-*` flow: `null` = closed; a non-empty list = open over
    // these host options. Never an open empty list — no one to ask is answered on
    // `error-message` instead of a picker whose confirm can never succeed.
    private val _mintCandidates = MutableStateFlow<List<CustodyMintCandidateView>?>(null)
    val mintCandidates: StateFlow<List<CustodyMintCandidateView>?> = _mintCandidates

    private val observer = object : DevicesObserver {
        override fun onChanged() {
            _snapshot.value = machine?.snapshot()
        }
    }

    private var machine: DevicesMachine? = null

    init {
        // `fauna.sync.changed` — a record landed in a folder this actor
        // participates in (own other device, or a fellow member). The push is
        // a nudge: re-read the machine snapshot so a collaborator's share/
        // membership/conflict change appears without waiting for the next
        // page visit (file-sync.md § Remote-change nudge; same tick idiom as
        // EventsVM.calendarChangedTick).
        viewModelScope.launch {
            api.folderChangedTick.collect {
                refresh()
                // The live-update half of the device-activity feature (file-sync.md
                // § Implementation status today) — the whole point of the feature,
                // per the same doc's tui/linux bullets: a client with no push arm
                // wired to this section never converges at any timeout. Re-read
                // ONLY the rows the user currently has expanded — never every set
                // this actor owns — mirroring linux's `folder_row_is_expanded`
                // gate / web's `if (expandedFs) void loadDeviceActivity(expandedFs)`.
                // `event.folder` still isn't threaded through the tick itself (same
                // blanket shape ApiClient.kt's SyncChanged arm already had, and
                // CalendarChanged's) — this only narrows WHICH already-expanded
                // rows re-read on it, not which push fires it.
                _expandedFolders.value.forEach { loadFolderDeviceActivity(it) }
            }
        }
        // Reconnect backstop: a push fired while the socket was down is never
        // replayed, so a mounted Devices/Folders screen must re-pull on
        // reconnect too — `StaleSurfaces::on_reconnect()` stales `media`
        // (the surface `fauna.sync.changed` feeds) along with every other
        // surface (`transport.md` § Which surfaces a push invalidates). This
        // VM had no reconnect arm before adopting the shared classifier (the
        // same class of gap the seam's own audit found on web's Events page).
        viewModelScope.launch {
            api.reconnectTick.collect {
                refresh()
                _expandedFolders.value.forEach { loadFolderDeviceActivity(it) }
            }
        }
        // The store-change notice: followed folders and foreign sets ride the
        // machine's refresh, and the enrollment notice, the custody facet and
        // the sync-defaults select are store reads ([ApiClient.storeChangedTick]).
        // The custody facet re-FOLDS only: its visit edge drives the ceremony
        // first ([loadCustodyFacet]), and a notice must not become a drive.
        viewModelScope.launch {
            api.storeChangedTick.collect {
                refresh()
                loadEnrollmentNotice()
                foldCustodyFacet()
                loadDefaultConflictPolicy()
            }
        }
        // Piece 1 follows the roster: the keyless posture is a read over the rows'
        // enrolled principals, so it re-runs whenever that list changes — a row
        // enrolling or leaving — rather than only on the page's load edge.
        viewModelScope.launch {
            _snapshot.map { snap -> snap?.devices.orEmpty().map { it.principal } }
                .distinctUntilChanged()
                .collect { loadKeylessPosture(it) }
        }
    }

    /**
     * Build the machine over the current connection if not already built, then
     * refresh. A null return (nest not yet connected) leaves an empty page; the
     * next [refresh] retries the build.
     */
    fun start() {
        // A fresh visit disarms any member removal left armed by a prior visit.
        _memberRemovePending.value = null
        refresh()
        loadPendingShares()
        loadOfflineGroupShares()
        loadDefaultConflictPolicy()
        loadCustodyFacet()
        loadEnrollmentNotice()
    }

    fun refresh() {
        val m = ensureMachine() ?: return
        viewModelScope.launch { m.refresh() }
        refreshWebdavCapability()
        refreshOwnTiers()
    }

    /**
     * Re-read the per-actor WebDAV serve capability. Rides [refresh] (every nav to the
     * page), so enabling mail elsewhere and returning re-enables the toggles without a
     * restart. A failed read degrades to `false` — the toggle stays disabled + hinted,
     * because not offering a control is the safe failure and offering one that cannot
     * succeed is not.
     */
    private fun refreshWebdavCapability() {
        viewModelScope.launch {
            _canServeWebdav.value = try {
                api.canServeWebdav()
            } catch (e: Exception) {
                false
            }
        }
    }

    /**
     * Re-read the creator's own subscription tier names. Rides [refresh] (every nav
     * to the page), so a tier created elsewhere shows up in the paywall select without
     * a restart. A failed read degrades to an empty list — same safe-failure shape as
     * [refreshWebdavCapability].
     */
    private fun refreshOwnTiers() {
        viewModelScope.launch {
            _ownTiers.value = try {
                api.subscriptionTiersList().sortedBy { it.rank }.map { it.name }
            } catch (e: Exception) {
                emptyList()
            }
        }
    }

    // ── Page gestures (forward to the machine; it refreshes + notifies) ──────

    fun removeDevice(index: Int) {
        val m = machine ?: return
        viewModelScope.launch { m.removeDevice(index.toUInt()) }
    }

    // ── Members without a matching entry — devices.md § Members without a matching entry ──
    //
    // The armed card's fleet id (`FleetMemberSummary.deviceId`), or null while no card is
    // armed. Keyed by the member's id, never its position: a refresh between arm and
    // confirm that inserts a card earlier in the list must not retarget the removal
    // (`fleet-scope-reclamation.md` § Fleet-scope reclamation, clause (4)). Mirrors
    // tui's `DevicesState::member_remove_pending`.
    private val _memberRemovePending = MutableStateFlow<String?>(null)
    val memberRemovePending: StateFlow<String?> = _memberRemovePending

    /** `device-member-remove-button`: arm THAT card only (arming another moves the arm). */
    fun armMemberRemoval(deviceId: String) { _memberRemovePending.value = deviceId }

    /** `device-member-remove-cancel-button`: disarm; nothing is written. */
    fun cancelMemberRemoval() { _memberRemovePending.value = null }

    /**
     * `device-member-remove-confirm-button`: disarm, then remove the member by its
     * fleet id through the key-addressed door. A refusal paints on `error-message`
     * (the machine's own copy) and the card stays.
     */
    fun confirmMemberRemoval(deviceId: String) {
        _memberRemovePending.value = null
        val m = machine ?: return
        viewModelScope.launch { m.removeMemberById(deviceId) }
    }

    /**
     * `device-p2p-participation-toggle[index]` (`p2p.md` § Per-device participation):
     * the machine picks the arm — this device's own switch, or a request that a
     * sibling turn off — then refreshes, or paints the refusal on `error-message`.
     */
    fun setP2pParticipation(index: Int, on: Boolean) {
        val m = machine ?: return
        viewModelScope.launch { m.setP2pParticipation(index.toUInt(), on) }
    }

    fun deleteFolder(name: String) {
        val m = machine ?: return
        viewModelScope.launch { m.deleteFolder(name) }
    }

    /**
     * The conflict review-row re-point (`conflict-resolve-button`): re-points the file
     * at the latest retained non-winning candidate via `DevicesMachine.use_other_version`
     * (`folders.md` § Conflicts). `device_id` is this device's own sync id — the
     * restore record is attributed to it, mirroring the Media-restore idiom; a
     * missing id (never provisioned) no-ops rather than sending a bogus attribution.
     */
    fun useOtherVersion(conflictId: Long) {
        val m = machine ?: return
        val deviceId = secureStorage.resolveDeviceId() ?: return
        viewModelScope.launch { m.useOtherVersion(conflictId, deviceId) }
    }

    /**
     * Save selective-sync paths — the RAW field text. Parses via the shared
     * `parsePathsField` (comma-split, trimmed, blanks dropped; an emptied field
     * parses to `[]` — ALWAYS a list, never `null`, so a cleared field actually
     * clears the stored filter instead of the wire's `null` = "leave unchanged"
     * silently no-opping — folders.md § Where logic lives, selective-sync bullet).
     * A `null` text is a list this device could not open: it goes out as `null`
     * ("leave unchanged"), never as a blank over the stored seal.
     */
    fun saveFolderPaths(name: String, includeText: String?, excludeText: String?) {
        val m = machine ?: return
        viewModelScope.launch {
            m.setFolderPaths(
                name,
                includeText?.let { com.fauna.ffi.parsePathsField(it) },
                excludeText?.let { com.fauna.ffi.parsePathsField(it) },
            )
        }
    }

    /**
     * Commit the nest place's snapshot policy (`folder-nest-save-button`) —
     * `backup-restore.md` § 8b — PLUS the version-retention SIBLING pair (apps row
     * 323). Takes the six controls' RAW text, exactly like [saveFolderPaths] takes
     * raw field text: the shared `nestPlaceWrite`/`versionRetentionWrite` own every
     * trap, so no per-app glue re-derives them — every snapshot knob rides on every
     * save (the nest applies the policy whole, so an emptied box must arrive as
     * *unset*), a cleared snapshot retention rides as the canonical binds-nothing
     * policy, never `null` (which the wire reads as "leave unchanged"), and the
     * version-retention pair always rides as non-null since the boxes are on screen.
     */
    fun setFolderNestPlace(
        name: String,
        snapshots: String,
        quietSecs: String,
        retentionSnapshots: String,
        retentionDays: String,
        versionRetentionCount: String,
        versionRetentionDays: String,
    ) {
        val m = machine ?: return
        val write = com.fauna.ffi.nestPlaceWrite(
            uniffi.fauna_folders_machine.NestPlaceEdit(
                snapshots = snapshots,
                quietSecs = quietSecs,
                retentionSnapshots = retentionSnapshots,
                retentionDays = retentionDays,
            ),
        )
        val versionRetention = com.fauna.ffi.versionRetentionWrite(
            uniffi.fauna_folders_machine.VersionRetentionEdit(
                count = versionRetentionCount,
                days = versionRetentionDays,
            ),
        )
        viewModelScope.launch {
            m.setFolderNestPlace(name, write.snapshots, write.quietSecs, write.retention, versionRetention)
        }
    }

    /**
     * Set a folder's conflict policy in place (`folder-conflict-policy-select`,
     * every owner row) — the value the *resolving device* reads to decide auto-merge
     * vs. latest-wins (file-sync.md § Conflicts). The nest folder row is the single
     * authoritative source; the machine refreshes the list on success so the picker
     * reflects the new value.
     */
    fun setFolderConflictPolicy(name: String, policy: String) {
        val m = machine ?: return
        viewModelScope.launch { m.setFolderConflictPolicy(name, policy) }
    }

    /**
     * Set a folder's content residency (`folder-nest-residency-select` /
     * `folder-residency-confirm`) — folders re-model phase 5, file-sync.md §
     * Content residency. Its own `folders.update` field, deliberately never
     * folded into the batched [setFolderNestPlace] write.
     *
     * **The flip to `metadata_only` is confirm-gated in the composable** — the
     * nest deletes its chunk bytes for the folder on that write, so the row
     * arms `folder-residency-confirm` and calls this only once answered; the
     * flip back to `full` calls straight through. The machine refreshes the
     * list on success so the picker reflects the new value.
     */
    fun setFolderResidency(name: String, residency: String) {
        val m = machine ?: return
        viewModelScope.launch { m.setFolderResidency(name, residency) }
    }

    /**
     * Turn a folder's exclusive editing on or off (`folder-exclusive-editing-toggle`,
     * owner row only; `ui/folders.md` § Exclusive editing). **Non-optimistic**: the
     * machine refreshes on success and the toggle repaints `exclusiveEditing` from
     * the new snapshot — a refused write never leaves it showing a state the folder
     * does not have.
     */
    fun setFolderExclusiveEditing(name: String, on: Boolean) {
        val m = machine ?: return
        viewModelScope.launch { m.setFolderExclusiveEditing(name, on) }
    }

    /**
     * The `folder-lease-status` line for [folder] (`null` while the folder is not
     * lease-governed), resolved once in shared Rust — read off the snapshot, never
     * probed (asking for the lease takes it). "This device" is the same
     * [localDeviceId] `device-this-mark-badge` matches.
     */
    fun folderLeaseStatus(folder: FolderSummary): LocalizedText? =
        com.fauna.ffi.devicesFolderLeaseStatus(
            folder,
            snapshot.value?.devices.orEmpty(),
            localDeviceId,
        )

    /**
     * Load the persisted default-conflict-policy preference for new sets
     * (`sync-default-conflict-policy-select`'s current value). Absent (no
     * preference) leaves [defaultConflictPolicy] null, rendered as "auto". A
     * failed read keeps the painted value — the store-change notice re-runs
     * this while the page is open, and a transient must never flip a chosen
     * policy back to "auto" — except the first-listing gate's refusal, whose
     * reason rides [sharingError] (`account-client-lifecycle.md` § The first
     * listing, clauses (4) and (5)).
     */
    fun loadDefaultConflictPolicy() {
        viewModelScope.launch {
            runCatching { api.defaultConflictPolicyGet() }
                .onSuccess { _defaultConflictPolicy.value = it }
                .onFailure { e ->
                    (e as? Exception)?.firstListingRefusal()?.let { _sharingError.value = it }
                }
        }
    }

    /**
     * Save the default-conflict-policy preference for new sets. Existing sets are
     * untouched — only the wizard reads this at open to stamp new creates.
     */
    fun setDefaultConflictPolicy(policy: String) {
        viewModelScope.launch {
            _sharingError.value = null
            try {
                _defaultConflictPolicy.value = api.defaultConflictPolicySet(policy)
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_set_default_conflict_policy)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    // ── Cross-user sharing (owner side) — folders.md § Sharing ─────────────

    /**
     * Load a set's cross-user "Shared with" roster (fauna.folders.members.list_actors).
     * ANY failure (an owner-only set answers `not_shared`, a transient WS error) maps to
     * an empty roster for that set — never a page error (parity with apple).
     */
    fun loadFolderActors(name: String) {
        viewModelScope.launch {
            val members = runCatching { api.folderActorMembers(name) }.getOrDefault(emptyList())
            _folderActors.update { it + (name to members) }
        }
    }

    // ── Per-set device activity — file-sync.md § Implementation status today ──

    /**
     * Read a set's per-device sync-activity list (`fauna.folders.devices` —
     * `folder-device-activity-item`/-label/-count). ANY failure (a transient WS
     * error) maps to an empty list for that set — never a page error, same
     * degrade-to-empty shape as [loadFolderActors].
     */
    fun loadFolderDeviceActivity(name: String) {
        viewModelScope.launch {
            val devices = runCatching { api.folderDevices(name) }.getOrDefault(emptyList())
            _folderDeviceActivity.update { it + (name to devices) }
        }
    }

    /**
     * The `FolderRow` composable calls this on every transition of its own local
     * `expanded` boolean (mirrors [loadFolderActors]'s on-expand trigger). Expanding
     * ALWAYS re-reads (matches web calling `loadDeviceActivity` on every
     * toggle-to-expand, not just the first — a stale count is exactly what the
     * live-update half of this feature exists to prevent) and records the set as
     * "expanded" so [init]'s `fauna.sync.changed` tick handler re-fires for it;
     * collapsing drops it from that set so a hidden row's activity is never
     * wastefully re-fetched on a later push.
     */
    fun setFolderExpanded(name: String, folderId: Long, expanded: Boolean) {
        if (expanded) {
            _expandedFolders.update { it + name }
            loadFolderDeviceActivity(name)
            loadFolderDestinationPlaces(name, folderId)
        } else {
            _expandedFolders.update { it - name }
        }
    }

    // ── Destination places (backup-destinations.md § Ordinary-folder coverage) ──

    /**
     * Read a folder's destination places (`fauna.backup.destination.list` joined
     * with the sealed config's display names). ANY failure degrades to an empty
     * list for that set — never a page error, same posture as
     * [loadFolderDeviceActivity]: an absent section is not a user-facing error.
     */
    fun loadFolderDestinationPlaces(name: String, folderId: Long) {
        viewModelScope.launch {
            val places = runCatching { api.folderDestinationsList(folderId) }.getOrDefault(emptyList())
            _folderDestinationPlaces.update { it + (name to places) }
        }
    }

    /**
     * Attach `folderId` to `destinationId` — the section always repaints from
     * the mutation's own re-read, never an optimistic flip (mirrors linux's
     * `attach_folder_destination`).
     */
    fun attachFolderDestination(name: String, folderId: Long, destinationId: String) {
        viewModelScope.launch {
            try {
                val places = api.folderDestinationAttach(folderId, destinationId)
                _folderDestinationPlaces.update { it + (name to places) }
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_folder_destination)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    /**
     * Detach one attached place — `place.folderSet` is the row's own
     * `__folder/<hex>/<id>` name, never re-derived here.
     */
    fun detachFolderDestination(name: String, folderId: Long, place: com.fauna.ffi.FfiFolderDestinationPlace) {
        viewModelScope.launch {
            try {
                val places = api.folderDestinationDetach(folderId, place.destinationId, place.folderSet ?: "")
                _folderDestinationPlaces.update { it + (name to places) }
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_folder_destination)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    /**
     * Share the set with a typed recipient. Resolves the handle / actor-id in shared
     * Rust ([ResolveService]), calls the author `folders_share`, then re-reads the
     * snapshot (so `mls_group_id` flips shared) and reloads the roster. `onResult(true)`
     * on success closes the picker; `false` (with [sharingError] set) keeps it open.
     * `access` is the share-time grant (`"writer"` | `null` for the reader default —
     * `folder-share-role-select`, multi-writer Phase 1).
     */
    fun shareFolder(name: String, recipientInput: String, access: String?, onResult: (Boolean) -> Unit) {
        val m = machine ?: run { onResult(false); return }
        viewModelScope.launch {
            _sharingError.value = null
            try {
                val resolved = resolveService.resolve(recipientInput)
                api.shareFolder(name, resolved.actorId, access)
                m.refresh()
                loadFolderActors(name)
                onResult(true)
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_share_set)
                    .replace("{message}", e.message ?: "")
                onResult(false)
            }
        }
    }

    /**
     * Grant or edit a shared-set member's access (`folder-member-role-select` /
     * `folder-member-cap-input`, owner-editable in place; multi-writer Phase 1,
     * folders.md § Sharing). Reloads the roster on success so the row reflects the
     * nest's authoritative value; any failure surfaces on [sharingError].
     */
    fun setFolderMemberAccess(name: String, memberActorIdHex: String, access: String, byteCap: Long?) {
        viewModelScope.launch {
            _sharingError.value = null
            try {
                api.setFolderMemberAccess(name, memberActorIdHex, access, byteCap)
                loadFolderActors(name)
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_set_member_access)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    /**
     * Remove a member from a shared set (rotates the content key for forward secrecy).
     * `groupIdHex` = the set's `mls_group_id`; the channel-id derivation + author
     * `folders_remove_member` run in shared Rust. Re-reads + reloads on success.
     */
    fun removeFolderMember(name: String, memberActorIdHex: String, groupIdHex: String) {
        val m = machine ?: return
        viewModelScope.launch {
            _sharingError.value = null
            try {
                api.removeFolderMember(name, memberActorIdHex, groupIdHex)
                m.refresh()
                loadFolderActors(name)
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_remove_member)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    /**
     * Flip a folder's WebDAV serve flag — the production caller of
     * `folder-webdav-toggle`. NOT a `DevicesMachine` config write: it runs the
     * author's `serve_set` orchestration then refreshes the snapshot so the toggle
     * reflects the persisted state (mirrors linux `FaunaClient::serve_set_folder` /
     * web `changeWebdav`). Errors (e.g. no MSEK — mail not set up yet) surface on
     * [sharingError] (the shared error-message banner; disable-with-hint is 6b-2).
     */
    fun serveSetFolder(name: String, mlsGroupIdHex: String?, enable: Boolean) {
        val m = machine ?: return
        viewModelScope.launch {
            _sharingError.value = null
            try {
                api.serveSetFolder(name, mlsGroupIdHex, enable)
                m.refresh()
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_serve_webdav)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    /**
     * Paywall a website-enabled folder to a subscription tier — the production caller of
     * `folder-paywall-tier-select`. v1 is set-only (no clear affordance). Same
     * author-orchestration-then-refresh shape as [serveSetFolder]; errors (e.g. no
     * enrolled web-serve holder) surface on [sharingError].
     */
    fun paywallSetFolder(name: String, mlsGroupIdHex: String?, tier: String) {
        val m = machine ?: return
        viewModelScope.launch {
            _sharingError.value = null
            try {
                api.paywallSetFolder(name, mlsGroupIdHex, tier)
                m.refresh()
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_paywall_set)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    /**
     * Read every row's `folder-on-demand-toggle` state (the screen calls this
     * whenever the folder list changes). Re-read from the store, never held in
     * view state alone, so a flip reads the same after the user leaves the page
     * and returns, and across a relaunch (`on-demand-files.md` § On-Demand
     * Files: the choice is remembered).
     */
    fun loadOnDemand(folders: List<FolderSummary>) {
        viewModelScope.launch {
            val identities = folders.mapNotNull { folder ->
                onDemandToggle.identify(folder)?.let { OnDemandRowKey.of(folder) to it }
            }
            val states = runCatching { onDemandToggle.read(identities.map { it.second }) }
                .getOrDefault(emptyMap())
            _onDemand.value = identities.mapNotNull { (key, id) ->
                states[id.scopedId]?.let { key to OnDemandRow(id.scopedId, it.shown, it.stronger) }
            }.toMap()
        }
    }

    /**
     * Flip a row's `folder-on-demand-toggle` — apple's `FolderOnDemandToggle.apply`
     * twin: persist the preference, then (ON, on an OWNER row) the enrol gesture
     * — the shared `ensure_place` writes this device's place at the default
     * point iff it holds none (`file-sync.md` § 4, *A local presence writes the
     * place it needs*) — and only then re-plan, so the plan sees the place the
     * presence needs. **A member's row is hide-only** (`on-demand-files.md`
     * § Shared sets on a capability host, decision 3): the membership is the
     * seat, and the roster kinds resolve a folder by name among the caller's
     * OWN folders, so no place is read or written there. A failed enrol leaves
     * the device place-less until the next gesture, as on apple.
     */
    fun setFolderOnDemand(folder: FolderSummary, shown: Boolean) {
        val key = OnDemandRowKey.of(folder)
        val row = _onDemand.value[key] ?: return
        _onDemand.update { it + (key to row.copy(shown = shown)) }
        viewModelScope.launch {
            val persisted = runCatching { onDemandToggle.setShown(row.scopedId, shown) }
            if (persisted.isFailure) {
                _onDemand.update { it + (key to row) }
                return@launch
            }
            val deviceId = secureStorage.resolveDeviceId()
            if (shown && folder.role != "member" && !deviceId.isNullOrEmpty()) {
                runCatching { api.ensureFolderPlace(folder.name, deviceId) }
            }
            runCatching { onDemandToggle.replan() }
        }
    }

    // ── Cross-user sharing (recipient side) — folders.md § Sharing, Recipient side ──

    /**
     * Peek the staged (knocked) folder shares. ANY failure maps to an empty list
     * (never a page error), matching [loadFolderActors]'s degrade-to-empty shape.
     */
    fun loadPendingShares() {
        viewModelScope.launch {
            _pendingShares.value = runCatching { api.folderPendingShares() }.getOrDefault(emptyList())
        }
    }

    /**
     * Accept a staged share (`folder-share-accept-button`): join the MLS group off
     * the chat rail + ack. Re-lists pending shares and refreshes the page (a newly
     * joined shared-with-me set should appear in the folders list on success).
     */
    fun acceptPendingShare(inboxId: Long) {
        val m = machine
        viewModelScope.launch {
            _sharingError.value = null
            try {
                api.acceptFolderShare(inboxId)
                loadPendingShares()
                m?.refresh()
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_accept_share)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    /** Decline a staged share (`folder-share-decline-button`): ack-and-drop, never joins. */
    fun declinePendingShare(inboxId: Long) {
        viewModelScope.launch {
            _sharingError.value = null
            try {
                api.declineFolderShare(inboxId)
                loadPendingShares()
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_decline_share)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    /**
     * Leave a set shared *with* us (`folder-leave-button`) — self-scoped, drops only
     * our own roster row. Refreshes so the row drops out of the folders list.
     */
    fun leaveFolder(groupIdHex: String) {
        val m = machine ?: return
        viewModelScope.launch {
            _sharingError.value = null
            try {
                api.leaveFolder(groupIdHex)
                m.refresh()
            } catch (e: Exception) {
                _sharingError.value = appContext.getString(R.string.devices_error_leave_share)
                    .replace("{message}", e.message ?: "")
            }
        }
    }

    // ── Co-present offline share ceremony — p2p.md § Offline share initiation ──
    //
    // The android leg of the affordance tui leads (`offline-share-*` /
    // `offline-receive-*`, the consent card as the knock trio's group arm, a
    // landed scope as an ordinary `folder-row`). Every decision is shared Rust
    // and every act runs on [OfflineShareHost]'s session-long scope, never
    // this VM's, because a ceremony outlives the page visit that started it.
    // The host is the `p2p-share` glue twin (`src/p2pShare` / `src/noP2pShare`),
    // so this VM names no ceremony FFI type — the store-safe build has none.

    /** Bumps on every write the paint decision reads; the Screen recomputes on it. */
    val offlineShareChanges: StateFlow<Int> = offlineShare.changes
    val groupShares: StateFlow<GroupShareRows> = offlineShare.groupShares
    val offlineShareError: StateFlow<String?> = offlineShare.error

    /** The shared paint decision for the current state; `null` hides the section. */
    fun offlineShareDecision(): OfflineShareDecision? = offlineShare.decision(api)

    /** `offline-share-button` / `offline-receive-button` — open a panel, binding the seat if needed. */
    fun openOfflineSharePanel(panel: OfflinePanel) = offlineShare.open(api, panel)

    fun setOfflinePeerCode(text: String) = offlineShare.setPeerCode(text)

    /** `offline-share-begin-button` — the initiator's whole walk. */
    fun beginOfflineShare() = offlineShare.begin(api)

    /** `offline-receive-expect-button` — the receive act. */
    fun expectOfflineShare() = offlineShare.expect(api)

    /** `offline-share-cancel-button` — close the panel, withdrawing any expectation. */
    fun cancelOfflineSharePanel() = offlineShare.cancel()

    /** The consent card's Accept (`folder-share-accept-button`, the group arm). */
    fun consentToGroupShare(scopeId: ByteArray) = offlineShare.consent(api, scopeId)

    /** The consent card's Decline (`folder-share-decline-button`, the group arm) — terminal. */
    fun declineGroupShare(scopeId: ByteArray) = offlineShare.decline(api, scopeId)

    /**
     * Re-read the consent cards and the landed scopes. Rides [start] — every
     * edge this page becomes visible on — so an offer that arrived while the
     * user was elsewhere knocks when they come back.
     */
    fun loadOfflineGroupShares() = offlineShare.loadGroupShares(api)

    // ── T16 custody facet — devices.md § Custody facet, pieces 1–3 + the mint ──

    /**
     * Fire a ceremony drive pass, then refold the custody facet into all three
     * row families.
     *
     * Rides [start] (every nav to the page), the android twin of linux's
     * `devices_page.connect_map` hydrate. The drive comes FIRST and is
     * fire-and-forget: it deposits nothing to await, and folding after it means a
     * receipt that landed since the last visit is already in this pass's rows
     * instead of the next one's.
     *
     * A null fold is a transient (unreadable config) — [_custodyRows] keeps its
     * previous contents rather than blanking live rows, which is why this assigns
     * only inside the non-null branch. Any FFI failure (not connected yet) degrades
     * to the same "keep what we have" outcome and never a page error: an owner with
     * no custodians and an owner whose config could not be read both have nothing
     * to be told here.
     */
    fun loadCustodyFacet() {
        viewModelScope.launch {
            api.custodyDrive()
            runCatching { api.custodyFacetLoad() }.getOrNull()?.let { applyCustodyFacet(it) }
        }
    }

    /**
     * Refold the custody facet WITHOUT a drive pass — the store-change notice's
     * re-read. Same keep-what-we-have contract as [loadCustodyFacet]'s fold.
     */
    fun foldCustodyFacet() {
        viewModelScope.launch {
            runCatching { api.custodyFacetLoad() }.getOrNull()?.let { applyCustodyFacet(it) }
        }
    }

    /**
     * Paint one re-folded facet — the load edge's and every act's. The
     * target-select answer is read per offer here, beside the rows it decorates,
     * so a card never renders against a stale answer.
     */
    private fun applyCustodyFacet(facet: CustodyFacetView) {
        _custodyOfferTargets.value = facet.offers
            .filter { api.custodyOfferShowsTarget(it) }
            .map { HexUtil.bytesToHex(it.grantId) }
            .toSet()
        _custodyRows.value = facet.rows
        _custodyHeld.value = facet.held
        _custodyOffers.value = facet.offers
    }

    /**
     * Piece 1: which enrolled rows hold no keys, through the shared
     * `fauna_devices_machine::keyless_posture` rule. A failed read keeps the
     * previous set (a transient must not flicker the marker); every unknown the
     * rule meets answers `false`, so the marker never rests on a guess.
     */
    private suspend fun loadKeylessPosture(principals: List<String?>) {
        val keyless = runCatching { api.devicesKeylessPosture(principals) }.getOrNull() ?: return
        _keylessPrincipals.value = principals.zip(keyless)
            .mapNotNull { (principal, isKeyless) -> principal?.takeIf { isKeyless } }
            .toSet()
    }

    /**
     * Run one custody gesture from the page — the android twin of linux's
     * `run_custody_gesture`. Every act runs through the shared
     * `run_custody_act`; its outcome repaints from the re-folded facet (a `null`
     * facet keeps the painted rows) and its error goes on `error-message`, never
     * swallowed (e2e convention 11).
     */
    fun runCustodyGesture(gesture: CustodyGesture) {
        when (gesture) {
            is CustodyGesture.Revoke -> revokeCustody(gesture.grantId, gesture.holder)
            is CustodyGesture.Accept ->
                runCustodyAct { api.custodyAccept(gesture.grantId, gesture.onNest) }
            is CustodyGesture.Decline -> runCustodyAct { api.custodyDecline(gesture.grantId) }
            is CustodyGesture.SetBudget -> {
                // The shared parser — never an app-side one — and an unparseable or
                // zero budget makes no call and says so (tui's and linux's answer).
                val cap = runCatching { com.fauna.ffi.parseByteSize(gesture.typed) }.getOrNull()
                if (cap == null || cap == 0UL) {
                    _sharingError.value =
                        appContext.getString(R.string.backups_backup_destination_capacity_invalid)
                    return
                }
                runCustodyAct { api.custodySetBudget(gesture.grantId, cap) }
            }
            is CustodyGesture.Stop -> runCustodyAct { api.custodyStop(gesture.grantId) }
            is CustodyGesture.Remove -> runCustodyAct { api.custodyRemove(gesture.grantId) }
            CustodyGesture.MintOpen -> openCustodyMint()
            CustodyGesture.MintCancel -> _mintCandidates.value = null
            is CustodyGesture.Mint -> runCustodyAct(closesMint = true) {
                api.custodyMint(gesture.host, gesture.channelHex)
            }
        }
    }

    /**
     * Run one host-side or mint act and answer it. A mint that recorded its offer
     * closes the flow; a failed one leaves it open beside its error, so the user
     * can pick again.
     */
    private fun runCustodyAct(
        closesMint: Boolean = false,
        act: suspend () -> com.fauna.ffi.FfiCustodyActOutcome,
    ) {
        viewModelScope.launch {
            _sharingError.value = null
            val outcome = runCatching { act() }.getOrElse { e ->
                _sharingError.value = e.message ?: e.toString()
                return@launch
            }
            outcome.facet?.let { applyCustodyFacet(it) }
            _sharingError.value = outcome.error
            if (closesMint && outcome.error == null) _mintCandidates.value = null
        }
    }

    /**
     * `custody-mint-button`: open the flow over this account's 1:1
     * conversations, or — with no one to ask — keep it closed and say on
     * `error-message` to start a conversation first.
     */
    private fun openCustodyMint() {
        _sharingError.value = null
        val candidates = runCatching { api.custodyMintCandidates() }.getOrElse { e ->
            _sharingError.value = e.message ?: e.toString()
            return
        }
        if (candidates.isEmpty()) {
            _mintCandidates.value = null
            _sharingError.value = appContext.getString(R.string.devices_custody_mint_no_contacts)
        } else {
            _mintCandidates.value = candidates
        }
    }

    /**
     * Revoke a custody grant (`custody-holder-revoke-button`).
     *
     * Takes the row's **grant id and accept-bound custodian key** — never a row
     * index, which a refold can re-point at a different custody. The act's own
     * nest-before-record ordering lives in shared Rust; this only routes the
     * outcome: the error string onto the page's error banner (never swallowed —
     * e2e convention 11), and the re-folded facet straight into [custodyRows] so
     * the revoked row leaves without waiting for a second read.
     */
    fun revokeCustody(grantId: ByteArray, holder: ByteArray?) {
        viewModelScope.launch {
            _sharingError.value = null
            val outcome = runCatching { api.custodyRevoke(grantId, holder) }.getOrElse { e ->
                _sharingError.value = appContext.getString(R.string.devices_error_revoke_custody)
                    .replace("{message}", e.message ?: "")
                return@launch
            }
            outcome.facet?.let { applyCustodyFacet(it) }
            outcome.error?.let { message ->
                _sharingError.value = appContext.getString(R.string.devices_error_revoke_custody)
                    .replace("{message}", message)
            }
        }
    }

    // ── Wizard lifecycle + gestures (forward to machine.wizard()) ────────────

    /**
     * Open a fresh wizard, then inject the user's global default conflict policy
     * (`fauna.state.sync-prefs`, cached in [defaultConflictPolicy]) so `submit()`
     * stamps it onto the create — the client-glue half of the Sync-defaults
     * contract (file-sync.md § Conflicts, policy; mirrors linux's
     * `open_wizard` → `set_default_conflict_policy` pairing). Best-effort: no
     * cached preference just leaves the wizard un-injected (new set lands on the
     * nest column default, `auto`).
     */
    fun openWizard() {
        val m = ensureMachine() ?: return
        m.openWizard()
        _defaultConflictPolicy.value?.let { m.wizard()?.setDefaultConflictPolicy(it) }
    }

    fun closeWizard() {
        machine?.closeWizard()
    }

    fun wizardSetName(name: String) { machine?.wizard()?.setName(name) }
    fun wizardToggleDevice(index: Int) { machine?.wizard()?.toggleDeviceMember(index.toUInt()) }

    /**
     * Set device `index`'s three place flags — the
     * `wizard-device-{originates,accepts,applies-deletes}` checkboxes (phase 2
     * slice e). Whole-value, like the `fauna.folders.places.set` write the
     * shared machine sends — the three flags ARE a device's place; there is no
     * role.
     */
    fun wizardSetFlags(index: Int, originates: Boolean, accepts: Boolean, appliesDeletes: Boolean) {
        machine?.wizard()?.setDeviceFlags(index.toUInt(), originates, accepts, appliesDeletes)
    }
    fun wizardNext() { machine?.wizard()?.next() }
    fun wizardBack() { machine?.wizard()?.back() }

    /** Commit the folder; on the terminal Done step, close the wizard and refresh. */
    fun wizardSubmit() {
        val m = machine ?: return
        viewModelScope.launch {
            val wizard = m.wizard() ?: return@launch
            val step = wizard.submit()
            if (step == FolderWizardStep.DONE) {
                m.closeWizard()
                m.refresh()
            }
        }
    }

    // ── The device-cap refusal notice — devices.md § Errors & edge cases ─────

    /**
     * Re-read the standing enrollment-refusal notice. Rides [start] (every nav
     * to the page), the same edge [loadCustodyFacet] rides — a local slot read,
     * never a network call. A failed read is a transient (not connected yet, a
     * JNI hiccup) and keeps whatever notice already stood, the same "keep what
     * we have" shape as [loadCustodyFacet]'s null-fold: a standing refusal must
     * not flicker off on a blip, and only a genuine fresh `null` (the nest
     * really has stopped refusing) clears it. [DevicesRosterScreen] gives a
     * roster gesture's own error precedence over this while it stands.
     */
    fun loadEnrollmentNotice() {
        viewModelScope.launch {
            runCatching { api.accountEnrollmentNotice() }
                .onSuccess { _enrollmentNotice.value = it }
        }
    }

    private fun ensureMachine(): DevicesMachine? {
        machine?.let { return it }
        val built = api.buildDevicesMachine(observer) ?: return null
        // Wire the B3 member-row join-filter BEFORE the first refresh() — fail-safe
        // otherwise (every role == "member" row stays hidden until a fresh machine
        // build, since `machine` is cached for the VM's lifetime).
        api.wireDevicesMlsQuery(built)
        // Wire the foreign-set (cross-nest) list source beside it — its long-missing
        // twin (docs/goal/ui/folders.md § Implementation status today): unwired, a
        // cross-nest shared set renders as absent rather than stale.
        api.wireDevicesForeignSets(built)
        // The participation rule's last fallback: the row `device-this-mark-badge`
        // marks is the one the toggle paints and acts on as this device's while the
        // runtime has not yet named its enrolled row (`p2p.md` § Per-device
        // participation → *Which row is this device's*).
        built.setThisDeviceRow(localDeviceId)
        machine = built
        return built
    }
}

/**
 * A folder row's identity on the Folders page — the row id plus the nest it
 * lives on (a cross-nest member row's id is its home nest's), so an own row and
 * a shared row never collide even when their names do.
 */
data class OnDemandRowKey(val id: Long, val homeNestUrl: String?) {
    companion object {
        fun of(folder: FolderSummary) = OnDemandRowKey(folder.id, folder.homeNestUrl)
    }
}

/** One row's `folder-on-demand-toggle` paint: its scoped id, the preference, and the stronger presence. */
data class OnDemandRow(val scopedId: String, val shown: Boolean, val stronger: Boolean)
