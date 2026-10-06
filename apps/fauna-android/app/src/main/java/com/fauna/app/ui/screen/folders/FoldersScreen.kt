package com.fauna.app.ui.screen.folders

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.components.TokenSelect
import com.fauna.app.service.SyncService
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.DevicesVM
import com.fauna.app.ui.viewmodel.PhotoBackupVM
import com.fauna.ffi.FfiFolderActorMember
import com.fauna.ffi.FfiFolderDevice
import com.fauna.app.BuildConfig
import com.fauna.app.p2pshare.GroupInvitation
import com.fauna.app.p2pshare.GroupScope
import com.fauna.app.p2pshare.OfflinePanel
import com.fauna.ffi.FfiPendingShare
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_devices_machine.ConflictSummary
import uniffi.fauna_devices_machine.DevicesSnapshot
import uniffi.fauna_devices_machine.FolderSummary
import uniffi.fauna_folders_machine.FolderWizardSnapshot
import uniffi.fauna_folders_machine.FolderWizardStep
import uniffi.fauna_folders_machine.MemberAccessOption
import uniffi.fauna_folders_machine.NestPlaceEdit
import uniffi.fauna_folders_machine.NestSnapshotsOption
import uniffi.fauna_folders_machine.VersionRetentionEdit
import uniffi.fauna_folders_machine.WizardDevice
import social.fauna.generated.Ids

/**
 * Settings → Folders (`docs/goal/ui/folders.md`): the **one control plane**
 * for the user's folders — the folder list, in-place per-set config
 * (selective-sync paths + the per-set conflict policy `folder-conflict-policy-select`),
 * the create wizard, and the conflict surface. Introduced by the 2026-06-28
 * sync/folder UI unification (the former `Settings → Sync` page, renamed +
 * expanded; the folder wizard/list/conflicts moved here from the retired
 * top-level Peers page — the roster stays on Settings → Devices). Android has
 * **no** local-folder binding (mobile sync model — `folder-location-*` is desktop
 * `platform_elements`), so this page is folder create/config + conflicts only.
 *
 * Drives off the same shared [DevicesVM]/[DevicesSnapshot] as the roster, rendering
 * the folder / conflict / wizard slices. Stateless [FoldersContent] is split out
 * so it renders under the Compose test harness with a seeded snapshot — **no page
 * or wizard logic client-side** (priority #2). The page + wizard errors flow to the
 * navigation shell's `error-message` banner via [LocalAppMessages].
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FoldersScreen(
    navController: NavController,
    vm: DevicesVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val folderActors by vm.folderActors.collectAsState()
    val folderDeviceActivity by vm.folderDeviceActivity.collectAsState()
    val folderDestinationPlaces by vm.folderDestinationPlaces.collectAsState()
    val sharingError by vm.sharingError.collectAsState()
    val canServeWebdav by vm.canServeWebdav.collectAsState()
    val ownTiers by vm.ownTiers.collectAsState()
    val defaultConflictPolicy by vm.defaultConflictPolicy.collectAsState()
    val pendingShares by vm.pendingShares.collectAsState()
    // Collected only so a ceremony write (panel, typed code, status, the seat
    // binding that gives the code its addressing) repaints the decision below.
    val offlineShareChanges by vm.offlineShareChanges.collectAsState()
    val groupShares by vm.groupShares.collectAsState()
    val offlineShareError by vm.offlineShareError.collectAsState()
    val appMessages = LocalAppMessages.current

    // The co-present affordance's whole paint, from shared Rust (the view, its
    // gates, the two readings) — decided behind the `p2p-share` glue seam and
    // handed to the Content as plain data. `null` hides the section. The
    // `BuildConfig.P2P_SHARE` condition is what lets the release shrinker fold the whole
    // ceremony render out of a store-safe APK (dynamic-features.md
    // § Platform-family surface excision).
    val offlineShare = if (BuildConfig.P2P_SHARE) {
        key(offlineShareChanges) { vm.offlineShareDecision() }?.let { decision ->
            OfflineSharePaint(
                panel = decision.panel,
                ownCode = decision.ownCode,
                peerCode = decision.peerCode,
                statusText = localized(decision.statusLabel).orEmpty(),
                codeHint = decision.codeError?.let { localized(it) },
                showsEntryButtons = decision.showsEntryButtons,
                showsCodeWidgets = decision.showsCodeWidgets,
                canBegin = decision.canBegin,
                canExpect = decision.canExpect,
                showsCancel = decision.showsCancel,
            )
        }
    } else {
        null
    }

    LaunchedEffect(Unit) { vm.start() }

    // Single `error-message` surface: the page error, falling back to the open
    // wizard's submit error. Both flow to the navigation shell's banner.
    val pageError = snapshot?.error
    val wizardError = snapshot?.wizard?.review?.error
    val errorText = localized(pageError) ?: localized(wizardError)
    LaunchedEffect(errorText) {
        if (errorText != null) appMessages.showError(errorText)
    }
    // Cross-user share/remove failures (plain strings the FFI raises, prefixed by
    // the VM) flow to the same banner — the owner-side Sharing surface has no
    // dedicated error element (folders.md § Sharing).
    LaunchedEffect(sharingError) {
        sharingError?.let { appMessages.showError(it) }
    }
    // Why a ceremony act stopped short. Held on the app-scoped host rather
    // than the VM, since the act may finish after the page that started it.
    LaunchedEffect(offlineShareError) {
        offlineShareError?.let { appMessages.showError(it) }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.folders_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = { navController.popBackStack() }) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        FoldersContent(
            modifier = Modifier.padding(padding),
            snapshot = snapshot,
            folderActors = folderActors,
            folderDeviceActivity = folderDeviceActivity,
            folderDestinationPlaces = folderDestinationPlaces,
            canServeWebdav = canServeWebdav,
            ownTiers = ownTiers,
            defaultConflictPolicy = defaultConflictPolicy,
            pendingShares = pendingShares,
            // Per-conflict localized badge text, computed here (FFI) and injected as
            // plain data — keeps FoldersContent FFI-free for Robolectric (mirrors
            // nestSnapshotsOptions below). Keyed by conflict id.
            conflictBadgeLabels = snapshot?.conflicts.orEmpty().associate {
                it.id to com.fauna.ffi.conflictBadgeLabel(it.resolution, it.resolvedAt, it.conflictType)
            },
            // The three-state select's catalog + each row's prefill, computed here
            // (FFI) and injected as plain data — same shape as conflictBadgeLabels
            // above. The prefill is the SHARED rule set (blank for unset, blank for a
            // zero retention bound), never re-derived per app.
            nestSnapshotsOptions = com.fauna.ffi.nestSnapshotsOptions(),
            nestPlaceEdits = snapshot?.folders.orEmpty().associate { folder ->
                folder.name to com.fauna.ffi.nestPlaceEditFromRow(
                    folder.nestSnapshots,
                    folder.nestSnapshotQuietSecs,
                    folder.retentionPolicy,
                )
            },
            // The version-retention SIBLING pair's prefill, same
            // "computed here (FFI), injected as plain data" shape as nestPlaceEdits.
            versionRetentionEdits = snapshot?.folders.orEmpty().associate { folder ->
                folder.name to com.fauna.ffi.versionRetentionEditFromBounds(
                    folder.versionRetentionMaxVersions,
                    folder.versionRetentionMaxAgeDays,
                )
            },
            actions = FoldersActions(
                onOpenWizard = vm::openWizard,
                onDeleteFolder = vm::deleteFolder,
                onSavePaths = vm::saveFolderPaths,
                onSaveNestPlace = vm::setFolderNestPlace,
                onUseOtherVersion = vm::useOtherVersion,
                onLoadMembers = vm::loadFolderActors,
                onFolderExpandedChanged = vm::setFolderExpanded,
                onAttachDestination = vm::attachFolderDestination,
                onDetachDestination = vm::detachFolderDestination,
                onShareSet = vm::shareFolder,
                onRemoveMember = vm::removeFolderMember,
                onSetMemberAccess = vm::setFolderMemberAccess,
                onServeWebdav = vm::serveSetFolder,
                onSetConflictPolicy = vm::setFolderConflictPolicy,
                onSetResidency = vm::setFolderResidency,
                onPaywallSet = vm::paywallSetFolder,
                onSetDefaultConflictPolicy = vm::setDefaultConflictPolicy,
                onAcceptShare = vm::acceptPendingShare,
                onDeclineShare = vm::declinePendingShare,
                onLeaveFolder = vm::leaveFolder,
                onOpenOfflineShare = { vm.openOfflineSharePanel(OfflinePanel.INITIATE) },
                onOpenOfflineReceive = { vm.openOfflineSharePanel(OfflinePanel.RECEIVE) },
                onOfflinePeerCodeChanged = vm::setOfflinePeerCode,
                onBeginOfflineShare = vm::beginOfflineShare,
                onExpectOfflineShare = vm::expectOfflineShare,
                onCancelOfflineShare = vm::cancelOfflineSharePanel,
                onAcceptGroupShare = vm::consentToGroupShare,
                onDeclineGroupShare = vm::declineGroupShare,
            ),
            wizardActions = WizardActions(
                onSetName = vm::wizardSetName,
                onToggleDevice = vm::wizardToggleDevice,
                onSetFlags = vm::wizardSetFlags,
                onNext = vm::wizardNext,
                onBack = vm::wizardBack,
                onCreate = vm::wizardSubmit,
                onCancel = vm::closeWizard,
            ),
            // photo-backup-controls (ui.yaml, used_in: [folders]) — injected as a
            // composable slot (not threaded through FoldersContent's plain-data
            // params) because it's a self-contained VM + Android permission-launcher
            // concern, not FFI/snapshot data (mirrors how MediaScreen wraps the pure
            // MediaContent rather than folding VM state into it).
            photoBackupSection = { PhotoBackupSection() },
            offlineShare = offlineShare,
            groupInvitations = groupShares.invitations,
            groupScopes = groupShares.scopes,
        )
    }
}

/** Page gesture callbacks. No-op defaults so the Compose test can seed any subset. */
data class FoldersActions(
    val onOpenWizard: () -> Unit = {},
    val onDeleteFolder: (String) -> Unit = {},
    /**
     * Save selective-sync paths — the RAW `folder-include-paths`/`-exclude-paths`
     * field text (not pre-split lists): parsing runs at the VM boundary via the
     * shared `parsePathsField` so this stays a pure UI callback (Content is FFI-free).
     */
    val onSavePaths: (name: String, includeText: String, excludeText: String) -> Unit = { _, _, _ -> },
    /**
     * Commit the nest place's snapshot policy (`folder-nest-save-button`) — the RAW
     * text of the six controls, exactly like [onSavePaths] passes raw field text:
     * `nestPlaceWrite`/`versionRetentionWrite` at the VM boundary own the parse and
     * every trap (the snapshot policy rides whole, so an emptied box arrives as
     * *unset*; a cleared snapshot retention rides as the binds-nothing policy, never
     * `None`, which the wire reads as "leave unchanged"; the version-retention pair
     * is its own SIBLING family, apps row 323). Keeping the parse out of here is
     * what keeps Content FFI-free.
     */
    val onSaveNestPlace: (
        name: String,
        snapshots: String,
        quietSecs: String,
        retentionSnapshots: String,
        retentionDays: String,
        versionRetentionCount: String,
        versionRetentionDays: String,
    ) -> Unit = { _, _, _, _, _, _, _ -> },
    /**
     * The conflict review-row re-point (`conflict-resolve-button`): re-point the
     * file at the latest retained non-winning candidate via
     * `DevicesMachine.use_other_version` (the VM resolves this device's id).
     */
    val onUseOtherVersion: (conflictId: Long) -> Unit = {},
    // ── Cross-user sharing (owner side) — folders.md § Sharing ──
    /** Load a set's "Shared with" actor roster (fauna.folders.members.list_actors). */
    val onLoadMembers: (name: String) -> Unit = {},
    /**
     * A row's local `expanded` boolean changed (`FolderRow`'s own Compose state) —
     * loads the set's device-activity list on every transition into `expanded`
     * (`folder-device-activity-item`/-label/-count, file-sync.md § Implementation
     * status today) and tells the VM which rows are currently expanded, so the
     * `fauna.sync.changed` push handler knows which already-loaded rows to
     * re-fetch (`DevicesVM.setFolderExpanded`).
     */
    val onFolderExpandedChanged: (name: String, folderId: Long, expanded: Boolean) -> Unit = { _, _, _ -> },
    // ── Destination places — backup-destinations.md § Ordinary-folder coverage ──
    /**
     * Attach `folderId` to `destinationId` (`folder-destination-attach-select` +
     * `-attach-button`). The section always repaints from the mutation's own
     * re-read, never an optimistic flip (mirrors linux's
     * `attach_folder_destination`).
     */
    val onAttachDestination: (name: String, folderId: Long, destinationId: String) -> Unit = { _, _, _ -> },
    /**
     * Detach one attached place (`folder-destination-detach-button`). The row
     * carries its own `folder_set` — never re-derived here.
     */
    val onDetachDestination: (name: String, folderId: Long, place: com.fauna.ffi.FfiFolderDestinationPlace) -> Unit =
        { _, _, _ -> },
    /**
     * Share the set with a typed recipient (handle / actor-id) + a share-time access
     * grant (`"writer"` | `null` for the reader default — `folder-share-role-select`,
     * multi-writer Phase 1). The VM resolves the recipient (shared Rust), calls the
     * author `folders_share`, then re-reads; `onResult(true)` on success closes the
     * picker, `false` surfaces the error.
     */
    val onShareSet: (name: String, recipientInput: String, access: String?, onResult: (Boolean) -> Unit) -> Unit =
        { _, _, _, cb -> cb(true) },
    /** Remove a member (rotates the content key); groupIdHex = the set's mls_group_id. */
    val onRemoveMember: (name: String, memberActorIdHex: String, groupIdHex: String) -> Unit = { _, _, _ -> },
    /**
     * Grant or edit a member's access + optional byte cap (`folder-member-role-select`
     * / `folder-member-cap-input`, owner-editable in place; multi-writer Phase 1).
     */
    val onSetMemberAccess: (name: String, memberActorIdHex: String, access: String, byteCap: Long?) -> Unit =
        { _, _, _, _ -> },
    /** Flip the per-set WebDAV serve flag (`folder-webdav-toggle`, every owner row). */
    val onServeWebdav: (name: String, mlsGroupIdHex: String?, enable: Boolean) -> Unit = { _, _, _ -> },
    /** Set a set's conflict policy in place (`folder-conflict-policy-select`, every owner row). */
    val onSetConflictPolicy: (name: String, policy: String) -> Unit = { _, _ -> },
    /**
     * Set a folder's content residency (`folder-nest-residency-select`,
     * every row) — folders re-model phase 5, file-sync.md § Content
     * residency. The flip to `"metadata_only"` is confirm-gated by the row
     * itself (`folder-residency-confirm`) before this is ever called.
     */
    val onSetResidency: (name: String, residency: String) -> Unit = { _, _ -> },
    /** Paywall a set to a subscription tier (`folder-paywall-tier-select`, website-enabled rows only; v1 set-only). */
    val onPaywallSet: (name: String, mlsGroupIdHex: String?, tier: String) -> Unit = { _, _, _ -> },
    /** Save the default conflict policy for NEW folders (`sync-default-conflict-policy-select`). */
    val onSetDefaultConflictPolicy: (policy: String) -> Unit = {},
    // ── Cross-user sharing (recipient side) — folders.md § Sharing, Recipient side ──
    /** Accept a staged share (`folder-share-accept-button`): join the MLS group + ack. */
    val onAcceptShare: (inboxId: Long) -> Unit = {},
    /** Decline a staged share (`folder-share-decline-button`): ack-and-drop, never joins. */
    val onDeclineShare: (inboxId: Long) -> Unit = {},
    /** Leave a set shared WITH us (`folder-leave-button`); groupIdHex = the set's mls_group_id. */
    val onLeaveFolder: (groupIdHex: String) -> Unit = {},
    // ── Co-present offline share — p2p.md § Offline share initiation ──
    /** `offline-share-button` — open the initiator panel. */
    val onOpenOfflineShare: () -> Unit = {},
    /** `offline-receive-button` — open the recipient panel. */
    val onOpenOfflineReceive: () -> Unit = {},
    /** `offline-share-peer-code-input` changed — the raw typed text; the parse is shared Rust's. */
    val onOfflinePeerCodeChanged: (String) -> Unit = {},
    /** `offline-share-begin-button` — the initiator's whole walk. */
    val onBeginOfflineShare: () -> Unit = {},
    /** `offline-receive-expect-button` — the receive act. */
    val onExpectOfflineShare: () -> Unit = {},
    /** `offline-share-cancel-button` — close the panel, withdrawing any expectation. */
    val onCancelOfflineShare: () -> Unit = {},
    /** The consent card's accept (group arm) — addressed by scope id, never row position. */
    val onAcceptGroupShare: (scopeId: ByteArray) -> Unit = {},
    /** The consent card's decline (group arm) — terminal. */
    val onDeclineGroupShare: (scopeId: ByteArray) -> Unit = {},
)

/**
 * The co-present offline-share affordance's paint (`p2p.md` § Offline share
 * initiation), decided before it reaches [FoldersContent]: the three strings
 * and [panel] are shared Rust's `OfflineShareView`, the booleans its
 * `offline_share_gates`, and [statusText] / [codeHint] its readings resolved
 * at the Screen — so the Content stays FFI-free and decides nothing.
 */
data class OfflineSharePaint(
    val panel: OfflinePanel,
    /** This device's compare code — shown verbatim; the in-person compare is the ceremony's only MITM defence. */
    val ownCode: String,
    val peerCode: String,
    val statusText: String,
    /** Why the typed code is refused — `null` for a usable or not-yet-typed one. */
    val codeHint: String?,
    val showsEntryButtons: Boolean,
    val showsCodeWidgets: Boolean,
    val canBegin: Boolean,
    val canExpect: Boolean,
    val showsCancel: Boolean,
)

/** Wizard gesture callbacks (forwarded to `machine.wizard()`). */
data class WizardActions(
    val onSetName: (String) -> Unit = {},
    val onToggleDevice: (Int) -> Unit = {},
    /**
     * Set device `index`'s three place flags — the
     * `wizard-device-{originates,accepts,applies-deletes}` checkboxes (phase 2
     * slice e). Whole-value, like the `fauna.folders.places.set` write the
     * machine sends — the three flags ARE a device's place; there is no role.
     */
    val onSetFlags: (index: Int, originates: Boolean, accepts: Boolean, appliesDeletes: Boolean) -> Unit =
        { _, _, _, _ -> },
    val onNext: () -> Unit = {},
    val onBack: () -> Unit = {},
    val onCreate: () -> Unit = {},
    val onCancel: () -> Unit = {},
)

@Composable
fun FoldersContent(
    snapshot: DevicesSnapshot?,
    actions: FoldersActions,
    wizardActions: WizardActions,
    // Per-set "Shared with" rosters (fauna.folders.members.list_actors), keyed by
    // set name — injected (like nestSnapshotsOptions) so the Content stays FFI-free. Each
    // list carries owner + members; the section filters to role == "member".
    folderActors: Map<String, List<FfiFolderActorMember>> = emptyMap(),
    // Per-set device activity (fauna.folders.devices), keyed by set name — the
    // ordinary sync per-DEVICE change signal, distinct from folderActors
    // above (cross-USER sharing). Same "injected so Content stays FFI-free" shape.
    folderDeviceActivity: Map<String, List<FfiFolderDevice>> = emptyMap(),
    // Per-folder destination places (backup-destinations.md § Ordinary-folder
    // coverage), keyed by set name — which of the owner's enrolled backup
    // destinations are attached to this ordinary folder. Same "injected so
    // Content stays FFI-free" shape as folderDeviceActivity. An absent/empty
    // list hides the whole section (no destination enrolled at all).
    folderDestinationPlaces: Map<String, List<com.fauna.ffi.FfiFolderDestinationPlace>> = emptyMap(),
    // Whether this actor holds the MSEK that serving a set over WebDAV seals the
    // WebdavKeysBlob under (minted when mail is first enabled). Not a DevicesSnapshot
    // field — the shared machine is keyless — so it is injected like folderActors.
    // Gates each owner row's folder-webdav-toggle; false ⇒ disabled + hinted.
    canServeWebdav: Boolean = false,
    // The creator's own subscription tier names — the option set each website-enabled row's
    // folder-paywall-tier-select offers. Not a DevicesSnapshot field (the keyless
    // machine can't own it) — injected like canServeWebdav. Empty ⇒ select disabled
    // + "create a tier first" hint.
    ownTiers: List<String> = emptyList(),
    // The persisted default-conflict-policy preference for new sets
    // (sync-default-conflict-policy-select's current value); null renders as "auto".
    defaultConflictPolicy: String? = null,
    // Recipient-side staged (knocked) shares — a peek of the un-acked folder
    // welcomes (folders.md § Sharing, Recipient side). Injected (like
    // folderActors) so the Content stays FFI-free.
    pendingShares: List<FfiPendingShare> = emptyList(),
    // Per-conflict localized `conflict-type-badge` text (the resolution when
    // resolved, else the conflict type), keyed by conflict id — computed by the
    // shared `conflictBadgeLabel` FFI fn at the Screen level and injected so this
    // Content stays FFI-free.
    conflictBadgeLabels: Map<Long, LocalizedText> = emptyMap(),
    // The three-state `folder-nest-snapshots-select` catalog (value + localized
    // label) from the shared `nestSnapshotsOptions()` — injected so the Content
    // stays FFI-free under Robolectric.
    nestSnapshotsOptions: List<NestSnapshotsOption> = emptyList(),
    // Per-folder prefill for the five `folder-nest-*` controls, keyed by folder
    // name — computed by the shared `nestPlaceEditFromRow` at the Screen level
    // (like conflictBadgeLabels) so this Content never re-derives the two rules it
    // owns: an unset knob shows BLANK, and so does a ZERO retention bound.
    nestPlaceEdits: Map<String, NestPlaceEdit> = emptyMap(),
    // Per-folder prefill for the version-retention SIBLING pair,
    // same "computed at the Screen level" shape as nestPlaceEdits above — never
    // folded into it (its own `folders.version_retention` wire field).
    versionRetentionEdits: Map<String, VersionRetentionEdit> = emptyMap(),
    // photo-backup-controls (ui.yaml `used_in: [folders]`; media.md § Photo-backup
    // reframe) — the "Photo Library" set's config, injected as a
    // composable slot so this Content stays FFI/VM-free (default no-op for tests
    // that don't care about it).
    photoBackupSection: @Composable () -> Unit = {},
    // The co-present offline-share affordance (p2p.md § Offline share
    // initiation), already decided at the Screen — `null` hides the section
    // (no usable identity: an affordance that cannot work is worse than none).
    offlineShare: OfflineSharePaint? = null,
    // Offered sets awaiting this account's consent — the knock trio's group
    // arm, painted into the SAME "Shared with you" list as [pendingShares].
    groupInvitations: List<GroupInvitation> = emptyList(),
    // Shared sets this device holds the machinery for — ordinary, read-only
    // `folder-row`s after the folder list.
    groupScopes: List<GroupScope> = emptyList(),
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(horizontal = 16.dp)
            .verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        val folders = snapshot?.folders.orEmpty()
        val conflicts = snapshot?.conflicts.orEmpty()

        photoBackupSection()

        // ── Shared with you (recipient-side pending knocks) — present only when
        // there are staged shares; a contact's share auto-joins and never knocks.
        // A co-present invitation is the same decision with a second source, so
        // it joins the SAME indexed list: one list of things awaiting an answer.
        if (pendingShares.isNotEmpty() || groupInvitations.isNotEmpty()) {
            SectionHeader(stringResource(R.string.devices_shared_with_you))
            pendingShares.forEach { share -> PendingShareRow(share, actions) }
            if (BuildConfig.P2P_SHARE) {
                groupInvitations.forEach { invitation -> GroupInvitationRow(invitation, actions) }
            }
            HorizontalDivider()
        }

        // ── Share with someone next to you (the co-present ceremony) ──────
        // `BuildConfig.P2P_SHARE` is the `p2p-share` member's android
        // condition: false on `storeSafe`, where the release shrinker folds this branch and the
        // `offline-share-*` / `offline-receive-*` ids leave the dex with it.
        if (BuildConfig.P2P_SHARE) {
            offlineShare?.let { paint ->
                OfflineShareSection(paint, actions)
                HorizontalDivider()
            }
        }

        // ── Conflicts (only when present) ────────────────────────────────
        if (conflicts.isNotEmpty()) {
            SectionHeader(stringResource(R.string.devices_conflicts_title))
            conflicts.forEach { conflict -> ConflictCard(conflict, conflictBadgeLabels[conflict.id], actions) }
            HorizontalDivider()
        }

        // ── Folders ────────────────────────────────────────────────────
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth(),
        ) {
            SectionHeader(
                stringResource(R.string.devices_folders),
                modifier = Modifier.weight(1f),
            )
            FilledTonalButton(
                onClick = actions.onOpenWizard,
                modifier = Modifier.testTag(Ids.FOLDER_ADD_BUTTON),
            ) {
                Icon(Icons.Default.Add, contentDescription = null)
                Spacer(Modifier.width(4.dp))
                Text(stringResource(R.string.devices_add_folder))
            }
        }
        if (folders.isEmpty() && groupScopes.isEmpty()) {
            EmptyHint(stringResource(R.string.devices_no_folders))
        } else {
            folders.forEach { folder ->
                // A set shared WITH us (`role == "member"`) is READ-ONLY — a separate,
                // non-expanding row with no owner affordances. Only sets this client
                // has actually MLS-joined reach here: the machine's injected MlsQuery
                // join-filter drops rostered-but-un-joined rows server-side
                // (folders.md § Sharing) — this Content just renders what it's given.
                if (folder.role == "member") {
                    MemberFolderRow(folder, actions)
                } else {
                    FolderRow(
                        folder,
                        actions,
                        members = folderActors[folder.name].orEmpty(),
                        deviceActivity = folderDeviceActivity[folder.name].orEmpty(),
                        destinationPlaces = folderDestinationPlaces[folder.name].orEmpty(),
                        canServeWebdav = canServeWebdav,
                        ownTiers = ownTiers,
                        nestSnapshotsOptions = nestSnapshotsOptions,
                        nestPlaceEdit = nestPlaceEdits[folder.name],
                        versionRetentionEdit = versionRetentionEdits[folder.name],
                    )
                }
            }
            // A set a co-present ceremony landed lists as an ordinary set row —
            // a ceremony-born set IS a set (p2p.md § Offline share initiation).
            if (BuildConfig.P2P_SHARE) groupScopes.forEach { scope -> GroupScopeRow(scope) }
        }

        // ── Sync defaults (page-level, below the per-set list) ──────────────
        // Today one control: the global default conflict policy for NEW folders
        // (file-sync.md § Conflicts, policy). Existing sets are untouched — each
        // row's folder-conflict-policy-select stays authoritative.
        HorizontalDivider()
        SyncDefaultsSection(defaultConflictPolicy, actions)
    }

    // ── Folder creation wizard ─────────────────────────────────────────
    snapshot?.wizard?.let { wizard ->
        FolderWizardDialog(wizard, wizardActions)
    }
}

// ── Conflicts (auto-resolve review list — folders.md § Conflicts) ────────

/**
 * One conflict review row. `badgeLabel` is the localized resolution text
 * (`conflictBadgeLabel`, injected from the Screen — see the `FoldersContent`
 * `conflictBadgeLabels` param); falls back to the raw conflict type if no
 * label was supplied (defensive — every production caller supplies one).
 * `conflict-file-info` renders the precomputed `fileInfo` field verbatim.
 * The `conflict-resolve-button` one-tap re-point (`use_other_version`) shows
 * only on a resolved row that retains a non-winning candidate
 * (`hasOtherVersion`); a still-unresolved row (the sync engine's report when its local-version
 * upload failed, awaiting a resolving device) renders informationally — no button, an "awaiting device"
 * caption; a resolved row with nothing to re-point at (candidate-free (mark-only)
 * resolve) renders neither.
 */
@Composable
private fun ConflictCard(conflict: ConflictSummary, badgeLabel: LocalizedText?, actions: FoldersActions) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                localized(badgeLabel) ?: conflict.conflictType,
                style = MaterialTheme.typography.labelMedium,
                modifier = Modifier.testTag(Ids.CONFLICT_TYPE_BADGE),
            )
            Text(
                conflict.fileInfo,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.CONFLICT_FILE_INFO),
            )
            when {
                conflict.winningManifestHash != null && conflict.hasOtherVersion -> {
                    Button(
                        onClick = { actions.onUseOtherVersion(conflict.id) },
                        modifier = Modifier.testTag(Ids.CONFLICT_RESOLVE_BUTTON),
                    ) { Text(stringResource(R.string.devices_conflicts_use_other_version)) }
                }
                conflict.winningManifestHash == null -> {
                    Text(
                        stringResource(R.string.devices_conflicts_awaiting_device),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                // Resolved, nothing to re-point at (candidate-free (mark-only) resolve) — nothing
                // actionable; version history is the finer surface.
                else -> {}
            }
        }
    }
}

// ── Folders ────────────────────────────────────────────────────────────────

@Composable
private fun FolderRow(
    folder: FolderSummary,
    actions: FoldersActions,
    members: List<FfiFolderActorMember>,
    deviceActivity: List<FfiFolderDevice>,
    destinationPlaces: List<com.fauna.ffi.FfiFolderDestinationPlace>,
    canServeWebdav: Boolean,
    ownTiers: List<String>,
    nestSnapshotsOptions: List<NestSnapshotsOption> = emptyList(),
    nestPlaceEdit: NestPlaceEdit? = null,
    versionRetentionEdit: VersionRetentionEdit? = null,
) {
    var expanded by remember(folder.name) { mutableStateOf(false) }
    var include by remember(folder.name) {
        mutableStateOf(folder.includePaths.orEmpty().joinToString(", "))
    }
    var exclude by remember(folder.name) {
        mutableStateOf(folder.excludePaths.orEmpty().joinToString(", "))
    }
    var showDeleteDialog by remember(folder.name) { mutableStateOf(false) }
    // The armed content-residency confirm (`folder-residency-confirm`, folders
    // re-model phase 5 — file-sync.md § Content residency). Picking
    // Metadata-only does NOT commit — it arms this, mirroring `showDeleteDialog`:
    // the flip deletes the nest's copy of the folder's content, and v1 has no
    // custody-inferred softening. Unlike the GTK apps, Compose recomposes the
    // select off `folder.residency` itself, so no "put the control back" step is
    // needed on cancel — nothing was ever committed.
    var showResidencyConfirmDialog by remember(folder.name) { mutableStateOf(false) }

    // Eager-load the "Shared with" roster for a shared set (mls_group_id present) so
    // the `folder-shared-badge` count shows even while the row is collapsed — the
    // android twin of apple's `.task(id: mlsGroupId)` (FoldersContent.swift).
    LaunchedEffect(folder.mlsGroupId) {
        if (folder.mlsGroupId != null) actions.onLoadMembers(folder.name)
    }
    // Owner-side "Shared with" = only the members (owner excluded from the roster row).
    val memberActors = com.fauna.ffi.folderMemberActors(members)

    // Per-set device activity (`folder-device-activity-item`/-label/-count, file-sync.md
    // § Implementation status today) — loaded on every transition into `expanded`
    // (mirrors the member roster's on-expand load; matches web calling
    // `loadDeviceActivity` on every toggle-to-expand, not just the first — a stale
    // count is exactly what the live-update half of this feature exists to
    // prevent). Also tells the VM which rows are currently expanded so the
    // `fauna.sync.changed` tick handler (`DevicesVM.init`) knows which
    // already-loaded rows to re-fetch on a push — android's twin of linux's
    // `folder_row_is_expanded` gate: `expanded` itself is Compose-local to this
    // row, so the VM keeps its own shadow of it rather than blanket-refreshing
    // every set's activity on every push.
    LaunchedEffect(expanded, folder.name) {
        actions.onFolderExpandedChanged(folder.name, folder.id, expanded)
    }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_ROW)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(Modifier.weight(1f)) {
                    Text(folder.name, style = MaterialTheme.typography.titleSmall)
                    Text(
                        "${folder.cachedSnapshotCount} snapshots",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    // "Shared · N" badge — a set is shared iff mls_group_id != null;
                    // shown only once ≥1 member is surfaced (parity with apple).
                    if (folder.mlsGroupId != null && memberActors.isNotEmpty()) {
                        Text(
                            stringResourceFmt(R.string.devices_shared_badge, memberActors.size),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.testTag(Ids.FOLDER_SHARED_BADGE),
                        )
                    }
                }
                TextButton(onClick = { expanded = !expanded }) {
                    Text(
                        stringResource(
                            if (expanded) R.string.common_cancel else R.string.devices_selective_sync
                        )
                    )
                }
            }

            // (The per-set scan-frequency editor that used to sit here retired
            // with phase 5 of the folders re-model: the cadence is a constant,
            // not a choice — file-sync.md § Config, the phase-5 block.)

            // Per-set conflict policy (`folder-conflict-policy-select`) on every
            // owner row — a folder has no type (`ui/folders.md` § Conflicts); the
            // resolving device reads the policy off the authoritative nest row.
            // Values + labels from the shared catalog.
            ConflictPolicySelect(
                currentPolicy = folder.conflictPolicy,
                onSelect = { policy -> actions.onSetConflictPolicy(folder.name, policy) },
            )

            // Per-set "serve over WebDAV" opt-in (`folder-webdav-toggle`) on every
            // owner row — a folder has no type (webdav-server.md § What the namespace
            // is; § Independent enablement point 2). NOT a
            // `DevicesMachine` config write: it runs the full serve orchestration
            // (content-key genesis/rotation + the nest flag + the MSEK-sealed
            // `WebdavKeysBlob` re-provision) via the shared `FoldersAuthor::serve_set`,
            // mirroring the web reference (`FoldersSection.svelte::changeWebdav`).
            //
            // Serving seals that blob under the actor's MSEK, so an actor who has not
            // set up mail cannot serve at all: the Switch is DISABLED and the hint says
            // why. Not cosmetic — `serve_set` flips the nest flag before it
            // re-provisions, so a doomed enable would commit the flag and only then
            // fail `NoMsek`, leaving the set served-but-blobless until a later reconcile
            // heals it. Any *other* failure still surfaces on the error banner.
            // ⚠ Grade this one from the ORACLE's gesture, not from the Rust
            // issuer's name — the rule apple's `folder-webdav-toggle` paid
            // for. The issuer is `reconcile_webdav_keys_blob`, which reads
            // like a background job; tui's gesture is `ToggleFolderWebdav`
            // and this is exactly that toggle. Serving is what seals the
            // blob, so the toggle IS the commit. Its own `canServeWebdav`
            // predicate (no MSEK → cannot serve at all) composes with the
            // gate rather than being replaced by it.
            val serveGate = faunaGate(
                "fauna.bridges.provision_webdav_keys_blob",
                enabled = canServeWebdav,
            )
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.devices_serve_webdav),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Text(
                        stringResource(
                            if (canServeWebdav) {
                                R.string.devices_serve_webdav_hint
                            } else {
                                R.string.devices_serve_webdav_needs_mail
                            },
                        ),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    DisabledControlReasonText(serveGate.reason)
                }
                Switch(
                    checked = folder.webdavEnabled,
                    enabled = serveGate.enabled,
                    onCheckedChange = { checked ->
                        actions.onServeWebdav(folder.name, folder.mlsGroupId, checked)
                    },
                    modifier = Modifier.testTag(Ids.FOLDER_WEBDAV_TOGGLE),
                )
            }

            // Per-set "paywall to tier" control (`folder-paywall-tier-select`,
            // website-enabled rows only) — folders.md § Web paywall / monetization.md
            // § Pillar 2. Structural sibling of the webdav toggle above:
            // NOT a `DevicesMachine` config write, it runs the full paywall
            // orchestration via the shared author `paywall_set`. v1 is SET-ONLY
            // (ratified 2026-07-13) — no clear affordance, so an empty own-tiers
            // list disables the select with a "create a tier first" hint.
            // Website-enabled rows only — keyed on the toggle, never on the
            // retired `mode = "web"` spelling. Not in Fauna Kids: a paywalled
            // web audience is web publishing AND monetization, both excised
            // (family-safety.md § The account age band, the kids-app bullet).
            if (!BuildConfig.KIDS && folder.websiteEnabled) {
                PaywallTierSelect(
                    currentTier = folder.webPaywallTier,
                    ownTiers = ownTiers,
                    onSelect = { tier -> actions.onPaywallSet(folder.name, folder.mlsGroupId, tier) },
                )
            }

            if (expanded) {
                OutlinedTextField(
                    value = include,
                    onValueChange = { include = it },
                    label = { Text(stringResource(R.string.devices_include_paths)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_INCLUDE_PATHS),
                )
                OutlinedTextField(
                    value = exclude,
                    onValueChange = { exclude = it },
                    label = { Text(stringResource(R.string.devices_exclude_paths)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_EXCLUDE_PATHS),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = {
                            actions.onSavePaths(folder.name, include, exclude)
                        },
                        modifier = Modifier.testTag(Ids.FOLDER_SAVE_PATHS),
                    ) { Text(stringResource(R.string.devices_save_paths)) }
                    OutlinedButton(
                        onClick = { showDeleteDialog = true },
                        modifier = Modifier.testTag(Ids.FOLDER_DELETE_BUTTON),
                    ) { Text(stringResource(R.string.devices_delete_folder)) }
                }

                // ── Destination places (backup-destinations.md § Ordinary-folder
                // coverage) — attach/detach an enrolled backup destination to
                // THIS ordinary folder. Hidden entirely while no destination is
                // enrolled at all (an affordance that cannot work must not
                // paint), same posture as linux's `build_destination_places_section`.
                if (destinationPlaces.isNotEmpty()) {
                    HorizontalDivider()
                    DestinationPlacesSection(
                        places = destinationPlaces,
                        onAttach = { destinationId ->
                            actions.onAttachDestination(folder.name, folder.id, destinationId)
                        },
                        onDetach = { place ->
                            actions.onDetachDestination(folder.name, folder.id, place)
                        },
                    )
                }

                // ── The nest place's snapshot policy (backup-restore.md § 8b) ──
                HorizontalDivider()
                NestPlaceSection(
                    folderName = folder.name,
                    options = nestSnapshotsOptions,
                    seed = nestPlaceEdit,
                    versionRetentionSeed = versionRetentionEdit,
                    onSave = actions.onSaveNestPlace,
                )

                // ── The nest place's content residency (folders re-model
                // phase 5 — file-sync.md § Content residency). Applies ON
                // CHANGE unlike the batched section above — its own
                // `folders.update` field, deliberately outside that save.
                HorizontalDivider()
                ResidencySelect(
                    currentResidency = folder.residency,
                    onSelect = { residency ->
                        if (residency == "metadata_only") {
                            showResidencyConfirmDialog = true
                        } else {
                            actions.onSetResidency(folder.name, residency)
                        }
                    },
                )

                // ── Per-set device activity (ordinary sync change signal) ──
                HorizontalDivider()
                DeviceActivitySection(deviceActivity)

                // ── Cross-user "Shared with" section (owner side) ────────────
                HorizontalDivider()
                SharedWithSection(folder, memberActors, actions)
            }
        }
    }

    if (showDeleteDialog) {
        AlertDialog(
            onDismissRequest = { showDeleteDialog = false },
            title = { Text(stringResource(R.string.devices_delete_confirm_title)) },
            text = { Text(stringResourceFmt(R.string.devices_delete_confirm_body, folder.name)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        showDeleteDialog = false
                        actions.onDeleteFolder(folder.name)
                    },
                    modifier = Modifier.testTag(Ids.FOLDER_DELETE_CONFIRM),
                ) { Text(stringResource(R.string.common_delete)) }
            },
            dismissButton = {
                TextButton(onClick = { showDeleteDialog = false }) {
                    Text(stringResource(R.string.common_cancel))
                }
            },
        )
    }

    // The content-residency confirm (folders re-model phase 5 — file-sync.md §
    // Content residency) — the delete confirm's twin. Picking Metadata-only
    // arms this; answering it is the one place the flip is committed. Nothing
    // was ever written on the arming pick, so a dismiss needs no cleanup beyond
    // closing the dialog — the select still paints `folder.residency` itself.
    if (showResidencyConfirmDialog) {
        AlertDialog(
            onDismissRequest = { showResidencyConfirmDialog = false },
            title = { Text(stringResource(R.string.devices_residency_confirm_title)) },
            text = { Text(stringResource(R.string.devices_residency_confirm_body)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        showResidencyConfirmDialog = false
                        actions.onSetResidency(folder.name, "metadata_only")
                    },
                    modifier = Modifier.testTag(Ids.FOLDER_RESIDENCY_CONFIRM),
                ) { Text(stringResource(R.string.devices_residency_confirm)) }
            },
            dismissButton = {
                TextButton(onClick = { showResidencyConfirmDialog = false }) {
                    Text(stringResource(R.string.common_cancel))
                }
            },
        )
    }
}

/**
 * The nest place's snapshot policy on an expanded `folder-row`
 * (`folder-nest-snapshots-select` / `-quiet-input` / `-retention-snapshots` /
 * `-retention-days` / `-save-button`) — `backup-restore.md` § 8b.
 *
 * On EVERY folder: "what the nest keeps" is a property of the one place every
 * folder has (a folder has no type, `ui/folders.md` § Modes).
 *
 * Three knobs, each three-state. The select spells its third state out; for the
 * two retention boxes and the quiet period, a BLANK box IS that third state — so
 * all four are staged locally and committed together by `folder-nest-save-button`,
 * never applied on change like the conflict-policy select above. An apply-on-change knob
 * here would have to send its three siblings with it, committing half-typed values
 * the user had not saved.
 *
 * The `seed` comes from the shared `nestPlaceEditFromRow`, which owns the rules
 * this render must not re-derive: an unset knob shows BLANK, and so does a ZERO
 * retention bound (zero is the nest's own spelling of unset, so rendering it would
 * turn "nothing chosen" into a bound the owner appears to have picked). The four
 * raw values go back out through `onSave` unparsed — `nestPlaceWrite` at the VM
 * boundary owns the write half.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NestPlaceSection(
    folderName: String,
    options: List<NestSnapshotsOption>,
    seed: NestPlaceEdit?,
    versionRetentionSeed: VersionRetentionEdit?,
    onSave: (String, String, String, String, String, String, String) -> Unit,
) {
    // Re-key on the seed itself, not just the folder: a push refresh carrying the
    // SAME persisted policy leaves half-typed edits alone (data-class equality),
    // while a policy that actually changed under us re-seeds the boxes.
    var snapshots by remember(folderName, seed) { mutableStateOf(seed?.snapshots.orEmpty()) }
    var quiet by remember(folderName, seed) { mutableStateOf(seed?.quietSecs.orEmpty()) }
    var retentionSnapshots by remember(folderName, seed) {
        mutableStateOf(seed?.retentionSnapshots.orEmpty())
    }
    var retentionDays by remember(folderName, seed) { mutableStateOf(seed?.retentionDays.orEmpty()) }
    // The version-retention SIBLING pair — its own family, sent
    // whole on the SAME folder-nest-save-button click (file-versions.md §
    // Retention ruling 1), never folded into the snapshot retention above.
    var versionRetentionCount by remember(folderName, versionRetentionSeed) {
        mutableStateOf(versionRetentionSeed?.count.orEmpty())
    }
    var versionRetentionDays by remember(folderName, versionRetentionSeed) {
        mutableStateOf(versionRetentionSeed?.days.orEmpty())
    }
    var expanded by remember { mutableStateOf(false) }

    Text(
        stringResource(R.string.devices_nest_place_section),
        style = MaterialTheme.typography.labelLarge,
    )

    // Same value/label split as the conflict selects: the MODEL string
    // is the wire value ("default" / "on" / "off") the cross-app `select(id, value)`
    // contract drives, and the display expression is its localized label — both from
    // the shared catalog.
    val currentLabel = options.firstOrNull { it.value == snapshots }
        ?.let { localized(it.label) ?: it.label.key }
        ?: snapshots
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = !expanded }) {
        OutlinedTextField(
            value = currentLabel,
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(R.string.devices_nest_snapshots)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .fillMaxWidth()
                .menuAnchor()
                .testTag(Ids.FOLDER_NEST_SNAPSHOTS_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(localized(option.label) ?: option.label.key) },
                    onClick = {
                        expanded = false
                        snapshots = option.value
                    },
                )
            }
        }
    }

    OutlinedTextField(
        value = quiet,
        onValueChange = { quiet = it },
        label = { Text(stringResource(R.string.devices_nest_quiet)) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_NEST_QUIET_INPUT),
    )
    OutlinedTextField(
        value = retentionSnapshots,
        onValueChange = { retentionSnapshots = it },
        label = { Text(stringResource(R.string.devices_nest_retention_snapshots)) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_NEST_RETENTION_SNAPSHOTS),
    )
    OutlinedTextField(
        value = retentionDays,
        onValueChange = { retentionDays = it },
        label = { Text(stringResource(R.string.devices_nest_retention_days)) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_NEST_RETENTION_DAYS),
    )
    // The version-retention SIBLING pair — bounds file-version
    // history, never snapshots (file-versions.md § Retention ruling 1); its own
    // folders.version_retention wire field, riding the SAME save button.
    OutlinedTextField(
        value = versionRetentionCount,
        onValueChange = { versionRetentionCount = it },
        label = { Text(stringResource(R.string.devices_version_retention_count)) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_VERSION_RETENTION_COUNT),
    )
    OutlinedTextField(
        value = versionRetentionDays,
        onValueChange = { versionRetentionDays = it },
        label = { Text(stringResource(R.string.devices_version_retention_days)) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_VERSION_RETENTION_DAYS),
    )
    // Not decoration: the only on-screen statement that emptying a box is a real
    // choice rather than a no-op.
    Text(
        stringResource(R.string.devices_nest_place_blank_hint),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Button(
        onClick = {
            onSave(
                folderName,
                snapshots,
                quiet,
                retentionSnapshots,
                retentionDays,
                versionRetentionCount,
                versionRetentionDays,
            )
        },
        modifier = Modifier.testTag(Ids.FOLDER_NEST_SAVE_BUTTON),
    ) { Text(stringResource(R.string.devices_nest_save)) }
}

/**
 * Per-set "Device activity" section on an expanded `folder-row`
 * (`folder-device-activity-item`/-label/-count) — the ordinary sync change
 * signal (`fauna.folders.devices`), distinct from `cachedSnapshotCount`/
 * `cachedTotalBytes` (the nest place's snapshot tally). Loaded on expand and re-fetched on every
 * `fauna.sync.changed` push while the row stays expanded — the live-update half of
 * the feature (`FolderRow`'s `LaunchedEffect(expanded, ...)` above,
 * `DevicesVM.setFolderExpanded`/`loadFolderDeviceActivity`, file-sync.md §
 * Implementation status today). An empty list renders the "no activity yet" hint,
 * not an error — a freshly created set (or one with no reads back yet) has
 * recorded nothing, same degrade-to-empty shape as [SharedWithSection]'s roster.
 */
@Composable
private fun DeviceActivitySection(devices: List<FfiFolderDevice>) {
    Text(
        stringResource(R.string.devices_device_activity),
        style = MaterialTheme.typography.labelLarge,
    )
    if (devices.isEmpty()) {
        EmptyHint(stringResource(R.string.devices_no_device_activity))
    } else {
        devices.forEach { device ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_DEVICE_ACTIVITY_ITEM),
            ) {
                Text(
                    device.label,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.weight(1f).testTag(Ids.FOLDER_DEVICE_ACTIVITY_LABEL),
                )
                // "Changes" caption — declares what the number means; the inline
                // peer of web's `<th>{t.devices.col_changes}</th>` column header
                // (this row style has no shared table header, so the caption rides
                // per-row instead, mirroring linux's per-row caption).
                Text(
                    stringResource(R.string.devices_col_changes),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    device.changeCount.toString(),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.testTag(Ids.FOLDER_DEVICE_ACTIVITY_COUNT),
                )
            }
        }
    }
}

/**
 * Per-folder "Destination places" section on an expanded owner `folder-row`
 * (`folder-destination-row`/-detach-button/-attach-select/-attach-button —
 * `docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
 * Renders one `folder-destination-row` per ATTACHED destination (label +
 * detach button), then — while at least one enrolled destination remains
 * unattached — a select + attach button pair. Only called while
 * `places.isNotEmpty()` (an affordance that cannot work must not paint —
 * `FolderRow`'s call site guards this), so an empty-state hint is never
 * needed here, unlike [DeviceActivitySection].
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DestinationPlacesSection(
    places: List<com.fauna.ffi.FfiFolderDestinationPlace>,
    onAttach: (destinationId: String) -> Unit,
    onDetach: (com.fauna.ffi.FfiFolderDestinationPlace) -> Unit,
) {
    Text(
        stringResource(R.string.devices_folder_destinations_title),
        style = MaterialTheme.typography.labelLarge,
    )
    val attached = places.filter { it.attached }
    val attachable = places.filter { !it.attached }

    // Both of these are gate-only gaps on already-rendered controls — the
    // section was built, the declarations were simply never added.
    val detachGate = faunaGate("fauna.backup.destination.detach_folder")
    attached.forEach { place ->
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_DESTINATION_ROW),
        ) {
            Text(
                place.label,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f),
            )
            OutlinedButton(
                onClick = { onDetach(place) },
                enabled = detachGate.enabled,
                modifier = Modifier.testTag(Ids.FOLDER_DESTINATION_DETACH_BUTTON),
            ) { Text(stringResource(R.string.devices_folder_destination_detach)) }
        }
    }
    if (attached.isNotEmpty()) DisabledControlReasonText(detachGate.reason)

    if (attachable.isNotEmpty()) {
        var expanded by remember { mutableStateOf(false) }
        // Keyed on `attachable` (structural equality): the moment a mutation
        // shrinks/reorders the attachable set, the stale selection is dropped
        // and this re-seeds to the fresh list's first entry — the same "drop
        // the stale selection" rule web's `destinationAttachSelect` reset
        // enforces after a successful attach.
        var selected by remember(attachable) { mutableStateOf(attachable.first()) }
        // The picker beside it is a pure buffer and stays live; only the attach
        // commits. Hoisted out of the Row so its reason renders BELOW the pair
        // rather than competing with the button for horizontal space.
        val attachGate = faunaGate("fauna.backup.destination.attach_folder")
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            ExposedDropdownMenuBox(
                expanded = expanded,
                onExpandedChange = { expanded = !expanded },
                modifier = Modifier.weight(1f),
            ) {
                OutlinedTextField(
                    value = selected.label,
                    onValueChange = {},
                    readOnly = true,
                    trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
                    modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.FOLDER_DESTINATION_ATTACH_SELECT),
                )
                ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                    attachable.forEach { place ->
                        DropdownMenuItem(
                            text = { Text(place.label) },
                            onClick = {
                                selected = place
                                expanded = false
                            },
                        )
                    }
                }
            }
            Button(
                onClick = { onAttach(selected.destinationId) },
                enabled = attachGate.enabled,
                modifier = Modifier.testTag(Ids.FOLDER_DESTINATION_ATTACH_BUTTON),
            ) { Text(stringResource(R.string.devices_folder_destination_attach)) }
        }
        DisabledControlReasonText(attachGate.reason)
    }
}

/**
 * Owner-side cross-user *Shared with* section on an expanded `folder-row`
 * (`docs/goal/ui/folders.md` § Sharing). A "Shared…" affordance
 * (`folder-share-button`) opens a recipient picker; below it, the set's member
 * roster renders as `folder-member-item` rows, or a "Not shared with anyone yet."
 * hint. Renders unconditionally on any expanded row (even an owner-only set), so
 * the owner can start sharing — the android twin of apple's `FolderSharedWithSection`.
 */
@Composable
private fun SharedWithSection(
    folder: FolderSummary,
    members: List<FfiFolderActorMember>,
    actions: FoldersActions,
) {
    var showShareSheet by remember(folder.name) { mutableStateOf(false) }

    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            stringResource(R.string.devices_shared_with),
            style = MaterialTheme.typography.labelLarge,
            modifier = Modifier.weight(1f),
        )
        TextButton(
            onClick = { showShareSheet = true },
            modifier = Modifier.testTag(Ids.FOLDER_SHARE_BUTTON),
        ) { Text(stringResource(R.string.devices_share_button)) }
    }
    if (members.isEmpty()) {
        EmptyHint(stringResource(R.string.devices_not_shared_yet))
    } else {
        members.forEach { member -> MemberRow(folder, member, actions) }
    }

    if (showShareSheet) {
        ShareSheet(
            folder = folder,
            onShare = { input, access, onResult -> actions.onShareSet(folder.name, input, access, onResult) },
            onDismiss = { showShareSheet = false },
        )
    }
}

/**
 * One member the set is shared with (`folder-member-item`): the member's handle
 * (or hex actor-id when the handle is unknown), a client-derived status, and a
 * `folder-member-remove-button` that rotates the content key. Status is always
 * "Active" — the nest cannot observe an MLS group join, so it reports only *who*
 * the set reached; "Pending" is a future optimistic state (folders.md § Sharing).
 * The second row is the member's owner-editable-in-place **access** (multi-writer
 * Phase 1): `folder-member-role-select` (Reader | Writer, from the shared
 * `member_access_options()` catalog) + `folder-member-cap-input` (blank = uncapped,
 * commits on the keyboard "Done" action) + `folder-writer-uncapped-warning` when
 * Writer is granted with no cap + `folder-writer-published-warning` when Writer is
 * granted on a folder whose content is readable beyond its members (see
 * [publishedWriterWarning]). Both edits write the FULL (access, cap) pair —
 * `fauna.folders.members.set_access` upserts the whole row, so sending one half
 * would silently clear the other (mirrors linux).
 */
@Composable
private fun MemberRow(
    folder: FolderSummary,
    member: FfiFolderActorMember,
    actions: FoldersActions,
) {
    var access by remember(member.actorId) { mutableStateOf(member.access ?: "reader") }
    var capText by remember(member.actorId) { mutableStateOf(member.byteCap?.toString() ?: "") }

    fun commit(newAccess: String = access, newCapText: String = capText) {
        access = newAccess
        capText = newCapText
        actions.onSetMemberAccess(folder.name, member.actorId, newAccess, com.fauna.ffi.parseCountI64(newCapText))
    }

    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_MEMBER_ITEM),
        ) {
            Text(
                member.display,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f).testTag(Ids.FOLDER_MEMBER_HANDLE),
            )
            Text(
                stringResource(R.string.common_active),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.FOLDER_MEMBER_STATUS),
            )
            TextButton(
                onClick = {
                    // A member row only exists on a shared set, so mls_group_id is present.
                    folder.mlsGroupId?.let { actions.onRemoveMember(folder.name, member.actorId, it) }
                },
                modifier = Modifier.testTag(Ids.FOLDER_MEMBER_REMOVE_BUTTON),
            ) { Text(stringResource(R.string.common_remove)) }
        }
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            MemberAccessSelect(
                currentAccess = access,
                testTagValue = "folder-member-role-select",
                modifier = Modifier.weight(1f),
                onSelect = { commit(newAccess = it) },
            )
            OutlinedTextField(
                value = capText,
                onValueChange = { capText = it },
                label = { Text(stringResource(R.string.devices_member_byte_cap)) },
                placeholder = { Text(stringResource(R.string.devices_member_byte_cap_placeholder)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { commit() }),
                modifier = Modifier.weight(1f).testTag(Ids.FOLDER_MEMBER_CAP_INPUT),
            )
        }
        if (access == "writer" && com.fauna.ffi.parseCountI64(capText) == null) {
            Text(
                stringResource(R.string.devices_writer_uncapped_warning),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.testTag(Ids.FOLDER_WRITER_UNCAPPED_WARNING),
            )
        }
        // Independent of the cap — a byte cap bounds the owner's quota, not what the
        // writer can change for the people outside the set — and stacked with the
        // quota warning above rather than replacing it.
        publishedWriterWarning(folder, access)?.let { published ->
            Text(
                published,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.testTag(Ids.FOLDER_WRITER_PUBLISHED_WARNING),
            )
        }
    }
}

/**
 * The `folder-writer-published-warning` copy for one grant of `access` on `folder`,
 * or null when the folder reaches nobody outside its members (folders.md § Sharing).
 *
 * The reach test is the shared `writerGrantReach` (`public` or paywalled ⇒ a writer
 * changes what people OUTSIDE the set read; public ⇒ "anyone", paywalled ⇒
 * "subscribers") — never re-derived here — fed the NORMALIZED audience exactly as the
 * audience select on the other apps is, so an unparseable column can never claim the
 * set is world-readable. State-based: it follows the grant and the publish in either
 * order, and it stacks with the uncapped-quota warning.
 */
@Composable
private fun publishedWriterWarning(folder: FolderSummary, access: String): String? =
    localized(
        com.fauna.ffi.writerGrantReach(
            access,
            com.fauna.ffi.normalizeAudience(folder.audience, folder.mlsGroupId != null),
            folder.webPaywallTier != null,
        ),
    )

/**
 * The member-access picker (`folder-share-role-select` / `folder-member-role-select`,
 * Reader | Writer) — values + labels come from the shared `memberAccessOptions()`
 * catalog (folders.md § Sharing), never hand-rolled. Same shared-catalog dropdown
 * shape as [ConflictPolicySelect] / [PaywallTierSelect].
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MemberAccessSelect(
    currentAccess: String,
    testTagValue: String,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    // The shared member-access catalog (`fauna_core::format::member_access_options`)
    // over the token-round-tripping select every raw-value picker on this
    // app shares: the driver reads/picks `reader`/`writer`, the human the label.
    val options = remember { com.fauna.ffi.memberAccessOptions() }
    val context = LocalContext.current
    val choices = remember(options, context) {
        options.map { it.value to (resolveLocalized(context, it.label) ?: it.label.key) }
    }
    Column(modifier = modifier) {
        Text(
            stringResource(R.string.devices_member_access),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        TokenSelect(
            testTagValue = testTagValue,
            selected = currentAccess,
            options = choices,
            onSelect = onSelect,
        )
    }
}

/**
 * The share recipient picker (a dialog). Reuses the conversations recipient-picker
 * IDs (`recipient-picker-input` / `recipient-resolve-status`) verbatim — priority
 * #2, no new picker IDs — but NOT the stateful conversations picker component
 * (which is coupled to the conversations manager; co-opting it would drag folder
 * concepts into that manager). Resolution runs on confirm in the VM (shared Rust
 * `classify_recipient` → `resolve_handle`), mirroring apple's `FolderShareSheet`.
 * The `folder-share-confirm` commit is disabled while the input is blank and shows
 * a resolve-status token while the share is in flight. Carries the share-time
 * `folder-share-role-select` access grant (Reader default) + the
 * `folder-writer-uncapped-warning` (a share-time writer grant always carries no
 * cap, so picking Writer always shows it — advisory, the owner can cap it on the
 * member row afterwards) and, stacked with it, the `folder-writer-published-warning`
 * when [folder] is readable beyond its members (see [publishedWriterWarning]).
 */
@Composable
private fun ShareSheet(
    folder: FolderSummary,
    onShare: (recipientInput: String, access: String?, onResult: (Boolean) -> Unit) -> Unit,
    onDismiss: () -> Unit,
) {
    var input by remember { mutableStateOf("") }
    var access by remember { mutableStateOf("reader") }
    // idle | resolving | error — mirrors the conversations recipient-resolve-status.
    var resolveState by remember { mutableStateOf("idle") }

    val (statusToken, statusText) = when (resolveState) {
        "resolving" -> "resolving" to stringResource(R.string.conversations_unified_recipient_resolve_resolving)
        "error" -> "error" to stringResource(R.string.conversations_unified_recipient_resolve_error)
        else -> "idle" to ""
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.devices_share_button)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = input,
                    onValueChange = { input = it },
                    placeholder = {
                        Text(stringResource(R.string.conversations_unified_recipient_picker_placeholder))
                    },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.RECIPIENT_PICKER_INPUT),
                )
                Text(
                    statusText,
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier
                        .testTag(Ids.RECIPIENT_RESOLVE_STATUS)
                        .semantics { stateDescription = statusToken },
                )
                MemberAccessSelect(
                    currentAccess = access,
                    testTagValue = "folder-share-role-select",
                    onSelect = { access = it },
                )
                if (access == "writer") {
                    Text(
                        stringResource(R.string.devices_writer_uncapped_warning),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.testTag(Ids.FOLDER_WRITER_UNCAPPED_WARNING),
                    )
                }
                publishedWriterWarning(folder, access)?.let { published ->
                    Text(
                        published,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.testTag(Ids.FOLDER_WRITER_PUBLISHED_WARNING),
                    )
                }
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    resolveState = "resolving"
                    val grantedAccess = access.takeIf { it == "writer" }
                    onShare(input.trim(), grantedAccess) { ok ->
                        if (ok) onDismiss() else resolveState = "error"
                    }
                },
                enabled = input.isNotBlank(),
                modifier = Modifier.testTag(Ids.FOLDER_SHARE_CONFIRM),
            ) { Text(stringResource(R.string.devices_share_button)) }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.common_cancel)) }
        },
    )
}

// ── Cross-user sharing (recipient side) — folders.md § Sharing, Recipient side ──

/**
 * One staged, not-yet-accepted cross-user folder share (`folder-pending-share`):
 * the sharer identity ("Shared by ‹who›") + `folder-share-accept-button` (join the
 * MLS group off the chat rail) + `folder-share-decline-button` (ack-and-drop, never
 * joins). Rendered inside the page-level "Shared with you" section — the android twin
 * of apple's `FolderPendingShareRow`. `who` reads the precomputed `sharedByDisplay`
 * (handle, else the sharer's canonical short id; empty only for a fully unstamped
 * cross-nest share — value-formatting.md § Account display label).
 */
@Composable
private fun PendingShareRow(share: FfiPendingShare, actions: FoldersActions) {
    val who = share.sharedByDisplay.ifEmpty { stringResource(R.string.common_unknown) }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_PENDING_SHARE),
    ) {
        Text(
            stringResourceFmt(R.string.devices_shared_by, who),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f),
        )
        TextButton(
            onClick = { actions.onAcceptShare(share.inboxId) },
            modifier = Modifier.testTag(Ids.FOLDER_SHARE_ACCEPT_BUTTON),
        ) { Text(stringResource(R.string.common_accept)) }
        TextButton(
            onClick = { actions.onDeclineShare(share.inboxId) },
            modifier = Modifier.testTag(Ids.FOLDER_SHARE_DECLINE_BUTTON),
        ) { Text(stringResource(R.string.common_decline)) }
    }
}

/**
 * One set shared WITH us — the recipient counterpart of [FolderRow]. Renders the
 * SAME flat row for every access level, and that is the ratified android shape, not
 * a gap: since multi-writer Phase 1 a `writer` member DOES write (folders.md
 * § Sharing), but the only affordance that access buys is **location binding**, and
 * android has none at all — for an owned set or a shared one (`folder-location-*` is
 * desktop `platform_elements`). So a writer row has nothing to differ on from a
 * reader's — the same N/A-by-construction disposition as iOS and web, ruled
 * 2026-08-15 in folders.md § Sharing. (`WatchedDirectoryManager`'s SAF trees are NOT
 * that affordance — a manual one-shot scan, no watcher; see the ruling.) The row
 * therefore does not expand: no path editors, no delete, no share, no
 * location binding, no webdav toggle. Carries the recipient `folder-shared-badge` variant
 * ("Shared by ‹handle›", the precomputed `FolderSummary.ownerDisplay`) and the
 * `folder-leave-button` — the android twin of apple's `memberFolderRow`.
 */
@Composable
private fun MemberFolderRow(folder: FolderSummary, actions: FoldersActions) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_ROW)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().padding(12.dp),
        ) {
            Text(
                folder.name,
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.weight(1f),
            )
            Text(
                stringResourceFmt(R.string.devices_shared_by, folder.ownerDisplay),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.testTag(Ids.FOLDER_SHARED_BADGE),
            )
            // `mls_group_id` is always present on a member row (the join-filter
            // proves we joined the group); the guard is belt-and-braces.
            folder.mlsGroupId?.takeIf { it.isNotEmpty() }?.let { groupId ->
                TextButton(
                    onClick = { actions.onLeaveFolder(groupId) },
                    modifier = Modifier.testTag(Ids.FOLDER_LEAVE_BUTTON),
                ) { Text(stringResource(R.string.common_leave)) }
            }
        }
    }
}

// ── Co-present offline share — p2p.md § Offline share initiation ─────────────

/**
 * The co-present share affordance: the two entry buttons while closed, the
 * open initiator/recipient panel otherwise (this device's code, theirs, the one
 * act button the open panel owns, status, cancel). Paints [paint] and decides
 * nothing — every gate on it is shared Rust's. The android twin of apple's
 * `OfflineShareSectionView`, linux's `render_offline_share` and windows'
 * `RenderOfflineShare`.
 */
@Composable
private fun OfflineShareSection(paint: OfflineSharePaint, actions: FoldersActions) {
    SectionHeader(stringResource(R.string.folders_offline_share_section))
    if (paint.showsEntryButtons) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(
                onClick = actions.onOpenOfflineShare,
                modifier = Modifier.testTag(Ids.OFFLINE_SHARE_BUTTON),
            ) { Text(stringResource(R.string.folders_offline_share_start)) }
            OutlinedButton(
                onClick = actions.onOpenOfflineReceive,
                modifier = Modifier.testTag(Ids.OFFLINE_RECEIVE_BUTTON),
            ) { Text(stringResource(R.string.folders_offline_share_receive)) }
        }
    }
    if (!paint.showsCodeWidgets) return
    Text(
        stringResource(R.string.folders_offline_share_own_code_label),
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    // Verbatim and selectable: a human reads this out and the other person
    // checks it, so it is never truncated or reflowed into a different string.
    SelectionContainer {
        Text(
            paint.ownCode,
            style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
            modifier = Modifier.testTag(Ids.OFFLINE_SHARE_OWN_CODE),
        )
    }
    // The safety sentence — handing the code over IN PERSON is the mechanism.
    Text(
        stringResource(R.string.folders_offline_share_own_code_help),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    OutlinedTextField(
        value = paint.peerCode,
        onValueChange = actions.onOfflinePeerCodeChanged,
        label = { Text(stringResource(R.string.folders_offline_share_peer_code_label)) },
        modifier = Modifier.fillMaxWidth().testTag(Ids.OFFLINE_SHARE_PEER_CODE_INPUT),
    )
    // Typing guidance beside the input — never `error-message`, which is
    // reserved for what an act actually did (e2e convention 2).
    paint.codeHint?.let { hint ->
        Text(hint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
    }
    // Exactly one act button — the open panel's.
    when (paint.panel) {
        OfflinePanel.INITIATE -> Button(
            onClick = actions.onBeginOfflineShare,
            enabled = paint.canBegin,
            modifier = Modifier.testTag(Ids.OFFLINE_SHARE_BEGIN_BUTTON),
        ) { Text(stringResource(R.string.folders_offline_share_begin)) }
        OfflinePanel.RECEIVE -> Button(
            onClick = actions.onExpectOfflineShare,
            enabled = paint.canExpect,
            modifier = Modifier.testTag(Ids.OFFLINE_RECEIVE_EXPECT_BUTTON),
        ) { Text(stringResource(R.string.folders_offline_share_expect)) }
        OfflinePanel.CLOSED -> Unit
    }
    Text(
        paint.statusText,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.testTag(Ids.OFFLINE_SHARE_STATUS),
    )
    if (paint.showsCancel) {
        TextButton(
            onClick = actions.onCancelOfflineShare,
            modifier = Modifier.testTag(Ids.OFFLINE_SHARE_CANCEL_BUTTON),
        ) { Text(stringResource(R.string.common_cancel)) }
    }
}

/**
 * One offered set awaiting consent — the `folder-pending-share` knock trio's
 * group arm, minting no ids of its own. The set is nameless in v1, so the card
 * names what IS known: who is handing it over, and the short scope id both
 * people can see on their own screens. Both gestures carry the SCOPE ID — an
 * offer can land while the user's finger is moving. The twin of apple's
 * `GroupInvitationRow` and linux's `build_group_invitation_row`.
 */
@Composable
private fun GroupInvitationRow(invitation: GroupInvitation, actions: FoldersActions) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_PENDING_SHARE),
    ) {
        Text(
            stringResourceFmt(R.string.folders_offline_share_from, invitation.initiator, invitation.shortId),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f),
        )
        TextButton(
            onClick = { actions.onAcceptGroupShare(invitation.scopeId) },
            modifier = Modifier.testTag(Ids.FOLDER_SHARE_ACCEPT_BUTTON),
        ) { Text(stringResource(R.string.common_accept)) }
        TextButton(
            onClick = { actions.onDeclineGroupShare(invitation.scopeId) },
            modifier = Modifier.testTag(Ids.FOLDER_SHARE_DECLINE_BUTTON),
        ) { Text(stringResource(R.string.common_decline)) }
    }
}

/**
 * A shared set this device holds the machinery for — an ordinary, read-only
 * `folder-row`: no seat config, no name (v1 sets are nameless — the short
 * scope id stands in), and no leave button (severance is the authority's mint,
 * not a self-scoped drop — `account-data-taxonomy.md` § The recipient-set
 * scheme). Its `folder-shared-badge` carries the same two readings an M2 row's
 * does. The twin of apple's `GroupScopeRow` and linux's `build_group_scope_row`.
 */
@Composable
private fun GroupScopeRow(scope: GroupScope) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.FOLDER_ROW)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().padding(12.dp),
        ) {
            Text(
                stringResourceFmt(R.string.folders_offline_share_set, scope.shortId),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.weight(1f),
            )
            Text(
                scope.sharedBy?.let { stringResourceFmt(R.string.devices_shared_by, it) }
                    ?: stringResourceFmt(R.string.devices_shared_badge, scope.memberCount.toString()),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.testTag(Ids.FOLDER_SHARED_BADGE),
            )
        }
    }
}

/**
 * The per-set conflict-policy picker (`folder-conflict-policy-select`, indexed,
 * every owner row) — file-sync.md § Conflicts. Values + labels come from the
 * shared `conflictPolicyOptions()` catalog (the same source as
 * `sync-default-conflict-policy-select` below and every app's picker) — never
 * hand-rolled (folders.md § Where logic lives). Absent (no `conflict_policy` sent) renders
 * as the column default, `"auto"`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ConflictPolicySelect(
    currentPolicy: String?,
    onSelect: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val options = remember { com.fauna.ffi.conflictPolicyOptions() }
    val current = currentPolicy ?: "auto"
    val currentLabel = options.firstOrNull { it.value == current }
        ?.let { localized(it.label) ?: it.label.key }
        ?: current
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = !expanded }) {
        OutlinedTextField(
            value = currentLabel,
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(R.string.devices_conflict_policy)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.FOLDER_CONFLICT_POLICY_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(localized(option.label) ?: option.label.key) },
                    onClick = {
                        expanded = false
                        onSelect(option.value)
                    },
                )
            }
        }
    }
}

/**
 * The nest place's content residency (`folder-nest-residency-select`, EVERY
 * row) — folders re-model phase 5, file-sync.md § Content residency. Applies
 * ON CHANGE like [ConflictPolicySelect], never staged like [NestPlaceSection]'s
 * batch: its own `folders.update` field, deliberately outside that save.
 *
 * `onSelect` receives the raw picked value, unvalidated — `"metadata_only"` is
 * the caller's cue to arm `folder-residency-confirm` rather than commit; this
 * composable holds no confirm state of its own, mirroring how
 * [ConflictPolicySelect] holds no write logic either. Unlike the GTK apps,
 * Compose repaints this select off `currentResidency` itself on every
 * recomposition, so a cancelled confirm needs no manual "put it back" — nothing
 * was ever written.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ResidencySelect(
    currentResidency: String?,
    onSelect: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val options = remember { com.fauna.ffi.residencyOptions() }
    val current = com.fauna.ffi.normalizeResidency(currentResidency ?: "")
    val currentLabel = options.firstOrNull { it.value == current }
        ?.let { localized(it.label) ?: it.label.key }
        ?: current
    Column {
        ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = !expanded }) {
            OutlinedTextField(
                value = currentLabel,
                onValueChange = {},
                readOnly = true,
                label = { Text(stringResource(R.string.devices_folder_residency)) },
                trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
                modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.FOLDER_NEST_RESIDENCY_SELECT),
            )
            ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                options.forEach { option ->
                    DropdownMenuItem(
                        text = { Text(localized(option.label) ?: option.label.key) },
                        onClick = {
                            expanded = false
                            onSelect(option.value)
                        },
                    )
                }
            }
        }
        // Keyed on the same NORMALIZED value the select paints, so copy and
        // control cannot disagree.
        Text(
            text = localized(com.fauna.ffi.residencyHint(current)) ?: "",
            style = MaterialTheme.typography.bodySmall,
        )
    }
}

/**
 * The per-set "paywall to tier" picker (`folder-paywall-tier-select`, indexed,
 * website-enabled rows only) — folders.md § Web paywall / monetization.md § Pillar 2.
 * Model values are the creator's own tier NAMES plus an empty-string sentinel for
 * the "Not paywalled (public)" placeholder, offered only while the set is still
 * public (v1 is set-only, ratified 2026-07-13 — no clear affordance, so picking
 * the placeholder is not a write). No tiers ⇒ nothing to paywall to: the select
 * is disabled with a "create a tier first" hint (mirrors the webdav "set up mail
 * first" gate above).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PaywallTierSelect(
    currentTier: String?,
    ownTiers: List<String>,
    onSelect: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val hasTiers = ownTiers.isNotEmpty()

    // Model = wire values. The empty sentinel (placeholder) rides only while public.
    // A tier the set is already paywalled to always stays present even if it was
    // since removed from the tier list, so the row still shows its state.
    val values = remember(currentTier, ownTiers) {
        buildList {
            if (currentTier == null) add("")
            addAll(ownTiers)
            if (currentTier != null && currentTier !in ownTiers) add(currentTier)
        }
    }
    val currentLabel = if (currentTier == null) {
        stringResource(R.string.devices_paywall_tier_none)
    } else {
        currentTier
    }

    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { if (hasTiers) expanded = !expanded }) {
        OutlinedTextField(
            value = currentLabel,
            onValueChange = {},
            readOnly = true,
            enabled = hasTiers,
            label = { Text(stringResource(R.string.devices_paywall_tier)) },
            supportingText = {
                Text(
                    stringResource(
                        if (hasTiers) R.string.devices_paywall_tier_hint else R.string.devices_paywall_tier_needs_tier
                    )
                )
            },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.FOLDER_PAYWALL_TIER_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            values.forEach { value ->
                DropdownMenuItem(
                    text = { Text(value.ifEmpty { stringResource(R.string.devices_paywall_tier_none) }) },
                    onClick = {
                        expanded = false
                        // The empty placeholder is not a write — v1 is set-only, there
                        // is no "clear the paywall" path yet.
                        if (value.isNotEmpty()) onSelect(value)
                    },
                )
            }
        }
    }
}

/**
 * The page-level "Sync defaults" section — today one control, the global default
 * conflict policy for NEW folders (`sync-default-conflict-policy-select`;
 * file-sync.md § Conflicts, policy; user-approved home 2026-07-11). Persisted in
 * `fauna.state.sync-prefs`; the wizard stamps the value onto creates. Existing sets
 * are untouched — each set's own `folder-conflict-policy-select` stays
 * authoritative. Same shared-catalog picker shape as [ConflictPolicySelect]; absent
 * (no preference) renders as `"auto"`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SyncDefaultsSection(currentPolicy: String?, actions: FoldersActions) {
    var expanded by remember { mutableStateOf(false) }
    val options = remember { com.fauna.ffi.conflictPolicyOptions() }
    val current = currentPolicy ?: "auto"
    val currentLabel = options.firstOrNull { it.value == current }
        ?.let { localized(it.label) ?: it.label.key }
        ?: current

    SectionHeader(stringResource(R.string.devices_sync_defaults))
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = !expanded }) {
        OutlinedTextField(
            value = currentLabel,
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(R.string.devices_default_conflict_policy)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.SYNC_DEFAULT_CONFLICT_POLICY_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(localized(option.label) ?: option.label.key) },
                    onClick = {
                        expanded = false
                        actions.onSetDefaultConflictPolicy(option.value)
                    },
                )
            }
        }
    }
}

// ── Folder creation wizard ─────────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FolderWizardDialog(
    wizard: FolderWizardSnapshot,
    actions: WizardActions,
) {
    AlertDialog(
        onDismissRequest = actions.onCancel,
        title = { Text(stringResource(R.string.devices_wizard_new_folder)) },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                when (wizard.step) {
                    FolderWizardStep.NAME -> WizardName(wizard, actions)
                    FolderWizardStep.DEVICES -> WizardDevicePlaces(wizard, actions)
                    FolderWizardStep.REVIEW -> WizardReview(wizard)
                    FolderWizardStep.DONE -> {}
                }
            }
        },
        confirmButton = {
            if (wizard.step == FolderWizardStep.REVIEW) {
                Button(
                    onClick = actions.onCreate,
                    enabled = wizard.review.createEnabled,
                    modifier = Modifier.testTag(Ids.WIZARD_CREATE_BUTTON),
                ) { Text(stringResource(R.string.devices_wizard_create)) }
            } else {
                Button(
                    onClick = actions.onNext,
                    enabled = wizardContinueEnabled(wizard),
                    modifier = Modifier.testTag(Ids.WIZARD_NEXT_BUTTON),
                ) { Text(stringResource(R.string.devices_wizard_next)) }
            }
        },
        dismissButton = {
            if (wizard.step == FolderWizardStep.NAME) {
                TextButton(onClick = actions.onCancel) {
                    Text(stringResource(R.string.common_cancel))
                }
            } else {
                TextButton(
                    onClick = actions.onBack,
                    modifier = Modifier.testTag(Ids.WIZARD_BACK_BUTTON),
                ) { Text(stringResource(R.string.devices_wizard_back)) }
            }
        },
    )
}

private fun wizardContinueEnabled(wizard: FolderWizardSnapshot): Boolean = when (wizard.step) {
    FolderWizardStep.NAME -> wizard.name.continueEnabled
    FolderWizardStep.DEVICES -> wizard.devicePlaces.continueEnabled
    else -> true
}

// ── Step 1: name ────────────────────────────────────────────────────────────
//
// A folder has NO TYPE (folders re-model phase 2 slice e): the three
// `wizard-mode-*` buttons and `wizard-mode-backup-warning` are retired from
// ui.yaml, so this step is the name and nothing else. What a device does is its
// place flags (step 2); what the nest keeps is the nest place's snapshot policy,
// edited on any folder's expanded row (`folder-nest-*`).
//
// ⚠ Retiring `wizard-mode-web` leaves website-folder creation unreachable until
// phase 4 mints the website toggle. That was surfaced to the user and accepted
// 2026-08-15 (the feature has no users); do NOT re-add a mode control to close
// it — phase 4 owns the fix (`ui/folders.md` § Modes).
@Composable
private fun WizardName(wizard: FolderWizardSnapshot, actions: WizardActions) {
    OutlinedTextField(
        value = wizard.name.name,
        onValueChange = actions.onSetName,
        label = { Text(stringResource(R.string.devices_wizard_name_placeholder)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.WIZARD_NAME_INPUT),
    )
    // The disabled-Next explainer: `continueEnabled` gates on a non-empty name,
    // and a greyed-out Next with no on-screen reason was a live-user "unknowable"
    // report (tui and linux carry the same line).
    if (!wizard.name.continueEnabled) {
        Text(
            stringResource(R.string.devices_wizard_name_required),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

// ── Step 2: per-device enrollment + place flags ─────────────────────────────
//
// The `wizard-device-role` Source/Sync/Backup/Mirror picker is RETIRED (phase 2
// slice e): a live user called those labels "a completely incomprehensible list
// of things" (2026-08-05), and the design's answer is not better role nouns but
// three checkboxes that each say what they do.

/**
 * Which of a seat's three flags a checkbox owns: element id, label, explainer,
 * and how to read/replace the field. The flag *meanings* live once, in
 * `fauna_protocol::folders::PlaceFlags`; this only picks the field — the same
 * split linux's `PlaceFlagKind` makes.
 */
private enum class PlaceFlagBox(
    val testId: String,
    val labelRes: Int,
    val descRes: Int,
) {
    ORIGINATES(
        "wizard-device-originates",
        R.string.devices_wizard_place_originates,
        R.string.devices_wizard_place_originates_desc,
    ),
    ACCEPTS(
        "wizard-device-accepts",
        R.string.devices_wizard_place_accepts,
        R.string.devices_wizard_place_accepts_desc,
    ),
    APPLIES_DELETES(
        "wizard-device-applies-deletes",
        R.string.devices_wizard_place_applies_deletes,
        R.string.devices_wizard_place_applies_deletes_desc,
    ),
    ;

    fun read(d: WizardDevice): Boolean = when (this) {
        ORIGINATES -> d.originates
        ACCEPTS -> d.accepts
        APPLIES_DELETES -> d.appliesDeletes
    }

    /**
     * The seat's flag triple with this one replaced — the shape `setDeviceFlags`
     * takes, which is whole-value like the nest write.
     */
    fun with(d: WizardDevice, on: Boolean): Triple<Boolean, Boolean, Boolean> = when (this) {
        ORIGINATES -> Triple(on, d.accepts, d.appliesDeletes)
        ACCEPTS -> Triple(d.originates, on, d.appliesDeletes)
        APPLIES_DELETES -> Triple(d.originates, d.accepts, on)
    }
}

@Composable
private fun WizardDevicePlaces(wizard: FolderWizardSnapshot, actions: WizardActions) {
    if (wizard.devicePlaces.devices.isEmpty()) {
        EmptyHint(stringResource(R.string.devices_wizard_no_devices_available))
        return
    }
    Text(stringResource(R.string.devices_wizard_select_devices_roles))
    wizard.devicePlaces.devices.forEachIndexed { index, device ->
        Column {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Checkbox(
                    checked = device.selected,
                    onCheckedChange = { actions.onToggleDevice(index) },
                    modifier = Modifier.testTag(Ids.WIZARD_DEVICE_CHECK),
                )
                Text(device.label, modifier = Modifier.weight(1f))
            }
            // The three flag boxes render for EVERY seat, enrolled or not — matching
            // tui (the lead app), linux, web and apple. Each carries its own one-line
            // explainer: the pattern the retired role picker used per seat, now per
            // box, because the flags are the thing being explained.
            PlaceFlagBox.entries.forEach { box ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(
                        checked = box.read(device),
                        onCheckedChange = { on ->
                            val (originates, accepts, appliesDeletes) = box.with(device, on)
                            actions.onSetFlags(index, originates, accepts, appliesDeletes)
                        },
                        modifier = Modifier.testTag(box.testId),
                    )
                    Text(stringResource(box.labelRes), modifier = Modifier.weight(1f))
                }
                Text(
                    stringResource(box.descRes),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

// ── Step 3: read-only review ────────────────────────────────────────────────
//
// The scan-frequency step that used to precede it retired with phase 5 of the
// folders re-model (`file-sync.md` § Config, the phase-5 block): the cadence is
// a constant, not a choice. Retention left in slice e — it is the nest place's
// policy, editable on ANY folder's expanded row (`folder-nest-*`).
@Composable
private fun WizardReview(wizard: FolderWizardSnapshot) {
    val review = wizard.review
    LabeledRow(stringResource(R.string.devices_wizard_review_name), review.name)
    // No mode line, no retention line, no cadence line: a folder has no type
    // (phase 2 slice e), retention is the nest place's policy edited on the row
    // rather than chosen at create, and the scan cadence is a constant (phase 5).
    Text(
        stringResource(R.string.devices_wizard_review_devices) + " (${review.enrolled.size})",
        style = MaterialTheme.typography.titleSmall,
    )
    // The enrolled list names devices, not roles — the role picker is retired, and
    // a seat's place is now three flags, which a review line cannot summarize
    // honestly in one noun (linux's build_review carries the same reasoning).
    if (review.enrolled.isEmpty()) {
        Text(
            stringResource(R.string.devices_wizard_review_no_devices),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    } else {
        review.enrolled.forEach { device -> Text(device.label) }
    }
}

@Composable
private fun LabeledRow(label: String, value: String) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(label, style = MaterialTheme.typography.bodyMedium)
        Text(value, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

// ── Photo backup (`photo-backup-controls`, ui.yaml `used_in: [folders]`) ────
//
// The "Photo Library" set's config, re-homed here 2026-07-19 from the
// retired standalone `settings/photo-backup` route (media.md § Photo-backup
// reframe; folders.md § Photo backup) — MediaStore is android's ingress twin of
// apple's PhotoKit, and this section mirrors apple's shared FaunaKit
// `PhotoBackupControlsView` shape exactly (priority #1: enable toggle gates an
// always-on/disabled wifi-only toggle + status + sync-now). The engine already
// resolves its target set through the shared `photo_library_set` resolver — this is purely that set's config UI, no data
// migration.

/** `photo-backup-controls` gestures. No-op defaults so the Compose test can seed any subset. */
data class PhotoBackupActions(
    val onSetAutoBackup: (Boolean) -> Unit = {},
    val onSyncNow: () -> Unit = {},
)

/**
 * The FFI/Hilt-free `photo-backup-controls` rendering — injected state/actions so
 * it's Robolectric-testable (mirrors [FoldersContent]'s purity). [PhotoBackupSection]
 * below wires the real [PhotoBackupVM] + the Android permission launcher.
 */
@Composable
fun PhotoBackupControlsContent(
    hasPermission: Boolean,
    autoBackup: Boolean,
    isSyncing: Boolean,
    syncedCount: Int,
    pendingCount: Int,
    lastError: String?,
    actions: PhotoBackupActions,
) {
    SectionHeader(stringResource(R.string.folders_photo_library_section))
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(stringResource(R.string.photo_backup_back_up_photos), style = MaterialTheme.typography.bodyMedium)
        Switch(
            checked = autoBackup,
            onCheckedChange = actions.onSetAutoBackup,
            modifier = Modifier.testTag(Ids.PHOTO_BACKUP_ENABLE_TOGGLE),
        )
    }
    if (autoBackup && !hasPermission) {
        Text(
            stringResource(R.string.photo_backup_photo_access_required),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
        )
    }
    if (autoBackup && hasPermission) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(stringResource(R.string.photo_backup_wifi_only), style = MaterialTheme.typography.bodyMedium)
            // Hardcoded on + disabled — the real gate is NetworkMonitor.shouldSyncPhotos()
            // (folders.md § Photo backup); mirrors apple's identical placeholder toggle.
            Switch(
                checked = true,
                onCheckedChange = {},
                enabled = false,
                modifier = Modifier.testTag(Ids.PHOTO_BACKUP_WIFI_ONLY_TOGGLE),
            )
        }
        Text(
            stringResource(R.string.photo_backup_auto_upload_desc),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(stringResource(R.string.photo_backup_backed_up), style = MaterialTheme.typography.bodyMedium)
            Text(
                stringResourceFmt(R.string.photo_backup_photos_count, syncedCount),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (pendingCount > 0) {
            Text(
                stringResourceFmt(R.string.photo_backup_pending_count, pendingCount),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Row(
            Modifier.fillMaxWidth().testTag(Ids.PHOTO_BACKUP_LAST_SYNC_TEXT),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(stringResource(R.string.photo_backup_last_backup), style = MaterialTheme.typography.bodyMedium)
            // Not yet wired to a real last-completed-sync timestamp — preserved as-is
            // from the retired settings/photo-backup screen (out of scope here).
            Text("—", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = actions.onSyncNow,
                enabled = !isSyncing,
                modifier = Modifier.testTag(Ids.PHOTO_BACKUP_SYNC_NOW_BUTTON),
            ) { Text(stringResource(R.string.photo_backup_sync_now)) }
            if (isSyncing) {
                CircularProgressIndicator(
                    modifier = Modifier.size(16.dp).testTag(Ids.PHOTO_BACKUP_SYNC_PROGRESS),
                    strokeWidth = 2.dp,
                )
                Text(stringResource(R.string.photo_backup_backup_in_progress), style = MaterialTheme.typography.bodySmall)
            }
        }
        lastError?.let {
            Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
        }
    }
    HorizontalDivider()
}

/**
 * Stateful wrapper: wires [PhotoBackupVM] + the Android media-permission launcher
 * around the pure [PhotoBackupControlsContent]. Turning the enable toggle on without
 * a granted permission launches the system request instead of flipping the
 * preference directly (mirrors apple's `PHPhotoLibrary.requestAuthorization` on
 * toggle-on); a grant persists `autoBackup = true` via [PhotoBackupVM.setAutoBackup].
 */
@Composable
fun PhotoBackupSection(vm: PhotoBackupVM = hiltViewModel()) {
    val context = LocalContext.current
    val syncedCount by vm.syncedCount.collectAsState(initial = 0)
    val isSyncing by vm.isSyncing.collectAsState()
    val uploadedCount by vm.uploadedCount.collectAsState()
    val totalScanned by vm.totalScanned.collectAsState()
    val lastError by vm.lastError.collectAsState()
    val autoBackup by vm.autoBackup.collectAsState()

    val requiredPermissions = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        arrayOf(Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_MEDIA_VIDEO)
    } else {
        arrayOf(Manifest.permission.READ_EXTERNAL_STORAGE)
    }
    var hasPermission by remember {
        mutableStateOf(
            requiredPermissions.all { perm ->
                ContextCompat.checkSelfPermission(context, perm) == PackageManager.PERMISSION_GRANTED
            }
        )
    }
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { results ->
        val granted = results.values.all { it }
        hasPermission = granted
        if (granted) vm.setAutoBackup(true)
    }

    PhotoBackupControlsContent(
        hasPermission = hasPermission,
        autoBackup = autoBackup,
        isSyncing = isSyncing,
        syncedCount = syncedCount,
        pendingCount = (totalScanned - uploadedCount).coerceAtLeast(0),
        lastError = lastError,
        actions = PhotoBackupActions(
            onSetAutoBackup = { enabled ->
                if (enabled && !hasPermission) {
                    permissionLauncher.launch(requiredPermissions)
                } else {
                    vm.setAutoBackup(enabled)
                }
            },
            onSyncNow = {
                val intent = Intent(context, SyncService::class.java)
                ContextCompat.startForegroundService(context, intent)
            },
        ),
    )
}

// ── Shared bits ──────────────────────────────────────────────────────────────

@Composable
private fun SectionHeader(text: String, modifier: Modifier = Modifier) {
    Text(
        text,
        style = MaterialTheme.typography.titleMedium,
        modifier = modifier.padding(vertical = 8.dp),
    )
}

@Composable
private fun EmptyHint(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}
