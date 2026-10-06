package com.fauna.app.ui.screen.folders

import android.content.Context
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.ui.util.LocalConnectionState
import com.fauna.ffi.FfiConnectionState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import com.fauna.ffi.FfiFolderActorMember
import com.fauna.ffi.FfiFolderDevice
import com.fauna.ffi.FfiPendingShare
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_devices_machine.ConflictCandidateSummary
import uniffi.fauna_devices_machine.ConflictSummary
import uniffi.fauna_devices_machine.DeviceSummary
import uniffi.fauna_devices_machine.DevicesSnapshot
import uniffi.fauna_devices_machine.FolderSummary
import uniffi.fauna_folders_machine.DevicePlacesSnapshot
import uniffi.fauna_folders_machine.FolderWizardSnapshot
import uniffi.fauna_folders_machine.FolderWizardStep
import uniffi.fauna_folders_machine.NameSnapshot
import uniffi.fauna_folders_machine.NestPlaceEdit
import uniffi.fauna_folders_machine.VersionRetentionEdit
import uniffi.fauna_folders_machine.NestSnapshotsOption
import uniffi.fauna_folders_machine.ReviewSnapshot
import uniffi.fauna_folders_machine.SubmitPhase
import uniffi.fauna_folders_machine.WizardDevice

/**
 * Compose-level coverage for the stateless [FoldersContent] (Settings → Folders,
 * `docs/goal/ui/folders.md`) — the folder list, in-place per-set config (incl.
 * `folder-conflict-policy-select`), the create wizard, and the conflict surface,
 * after the 2026-06-28 sync/folder UI unification. Renders with a seeded snapshot
 * + injected option catalogs (so the Content is FFI-free) — no Hilt, no VM, no FFI
 * native calls — exercising the canonical ui.yaml test IDs + gesture callbacks on the
 * JVM. (Android E2E `test_folders.py --client android` is the standing gate once the
 * host emulator lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FoldersContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    // The three-state `folder-nest-snapshots-select` catalog the FFI
    // `nest_snapshots_options()` returns, as plain data. The default state leads —
    // it is where every folder rests and where a knob returns (backup-restore.md § 8b).
    private val nestSnapshotOptions = listOf(
        NestSnapshotsOption("default", LocalizedText("devices.nest_snapshots_default_label", emptyMap())),
        NestSnapshotsOption("on", LocalizedText("devices.nest_snapshots_on_label", emptyMap())),
        NestSnapshotsOption("off", LocalizedText("devices.nest_snapshots_off_label", emptyMap())),
    )

    private fun folder(
        name: String,
        mlsGroupId: String? = null,
        webdavEnabled: Boolean = false,
        role: String? = null,
        // "writer" ⇒ the member may write (multi-writer Phase 1); else reader. On
        // android it buys no rendered affordance — see
        // memberFolderRowRendersNoBindingAffordanceEvenForAWriter.
        access: String? = null,
        ownerDisplay: String = "", // "" = the caller's own rows (value-formatting.md § Account display label)
        conflictPolicy: String? = null,
        webPaywallTier: String? = null,
        homeNestUrl: String? = null,
        // The nest place's three knobs. `null` is UNSET — "nothing authoritative
        // said" — not `false`/`0`, and it is where every folder rests
        // (`backup-restore.md` § 8b), so it is the right fixture default.
        retentionPolicy: String? = null,
        nestSnapshots: Boolean? = null,
        nestSnapshotQuietSecs: Long? = null,
        // The version-retention bounds pair, flattened; `0u` = that bound unset,
        // both zero = keep everything — the resting state, the right fixture
        // default (file-versions.md § Retention ruling 1).
        versionRetentionMaxVersions: UInt = 0u,
        versionRetentionMaxAgeDays: UInt = 0u,
        // Phase 5's content-residency field (file-sync.md § Content residency).
        // "" is the wire's own spelling of the default (full) residency,
        // matching every OTHER fixture default's rest-state convention.
        residency: String = "",
        // The folder's audience wire value; "private" is where every fixture
        // folder rests. `public` is what makes a writer grant reach beyond the set.
        audience: String = "private",
        // The website toggle — what the paywall row keys on.
        websiteEnabled: Boolean = false,
    ) = FolderSummary(
        id = 1L,
        name = name,
        retentionPolicy = retentionPolicy,
        cachedSnapshotCount = 2L,
        cachedTotalBytes = 0L,
        cachedLastSnapshotAt = null,
        includePaths = null,
        excludePaths = null,
        mlsGroupId = mlsGroupId,
        role = role,
        access = access,
        ownerHandle = null,
        ownerDisplay = ownerDisplay,
        webdavEnabled = webdavEnabled,
        conflictPolicy = conflictPolicy,
        webPaywallTier = webPaywallTier,
        homeNestUrl = homeNestUrl,
        nestSnapshots = nestSnapshots,
        nestSnapshotQuietSecs = nestSnapshotQuietSecs,
        versionRetentionMaxVersions = versionRetentionMaxVersions,
        versionRetentionMaxAgeDays = versionRetentionMaxAgeDays,
        // Phase 4 (folders re-model): the audience tri-state + website toggle.
        // "private" is where every fixture folder rests, matching the
        // transcribe's pre-phase-4 derivation for an unbound set.
        audience = audience,
        websiteEnabled = websiteEnabled,
        residency = residency,
    )

    private fun member(
        handle: String,
        actorId: String = "ab".repeat(32),
        role: String = "member",
        access: String? = "reader",
        byteCap: Long? = null,
    ) =
        // display mirrors `account_display_label`: the handle when present
        // (every caller here passes one), else the canonical short_id.
        FfiFolderActorMember(
            actorId = actorId,
            handle = handle,
            role = role,
            display = handle,
            access = access,
            byteCap = byteCap,
            bytesUsed = null,
            remote = false,
        )

    private fun deviceActivity(
        label: String,
        changeCount: Long,
        deviceId: String = "aa".repeat(32),
    ) = FfiFolderDevice(
        deviceId = deviceId,
        label = label,
        lastChangeAt = 0L,
        changeCount = changeCount,
    )

    private fun destinationPlace(
        destinationId: String,
        label: String,
        attached: Boolean,
        folderSet: String? = null,
    ) = com.fauna.ffi.FfiFolderDestinationPlace(
        destinationId = destinationId,
        label = label,
        attached = attached,
        folderSet = folderSet,
    )

    private fun pendingShare(
        inboxId: Long = 1L,
        sharedByDisplay: String = "alice@fauna.social",
        setName: String? = "photos",
    ) = FfiPendingShare(
        inboxId = inboxId,
        sharedBy = "ab".repeat(32),
        sharedByHandle = null,
        sharedByDisplay = sharedByDisplay,
        groupId = "cd".repeat(32),
        channelId = "ef".repeat(32),
        setName = setName,
    )

    private fun conflict(
        resolvedAt: Long? = null,
        resolution: String? = null,
        winningManifestHash: String? = null,
        hasOtherVersion: Boolean = false,
        fileInfo: String = "photos: a/b.jpg",
    ) = ConflictSummary(
        id = 5L,
        folder = "photos",
        deviceId = "cd".repeat(32),
        path = "a/b.jpg",
        conflictType = "content",
        details = null,
        createdAt = 0L,
        candidates = listOf(
            ConflictCandidateSummary("ff".repeat(32), "cd".repeat(32), 10L, 0L, null),
        ),
        resolvedAt = resolvedAt,
        resolution = resolution,
        winningManifestHash = winningManifestHash,
        hasOtherVersion = hasOtherVersion,
        fileInfo = fileInfo,
    )

    /**
     * A seat as the three place-flag checkboxes see it (phase 2 slice e) — the
     * three flags ARE the seat's place; the record carries no role.
     */
    private fun wizardDevice(
        label: String,
        selected: Boolean = true,
        originates: Boolean = true,
        accepts: Boolean = true,
        appliesDeletes: Boolean = true,
        deviceId: String = "ab".repeat(32),
    ) = WizardDevice(
        deviceId = deviceId,
        label = label,
        selected = selected,
        originates = originates,
        accepts = accepts,
        appliesDeletes = appliesDeletes,
    )

    private fun wizard(
        step: FolderWizardStep,
        name: String = "",
        devices: List<WizardDevice> = emptyList(),
    ) = FolderWizardSnapshot(
        step = step,
        name = NameSnapshot(
            name = name,
            continueEnabled = name.isNotEmpty(),
        ),
        devicePlaces = DevicePlacesSnapshot(devices = devices, continueEnabled = true),
        review = ReviewSnapshot(
            name = name,
            retention = null,
            enrolled = emptyList(),
            createEnabled = true,
            phase = SubmitPhase.IDLE,
            created = false,
            failedMembers = emptyList(),
            error = null,
        ),
    )

    private fun snapshot(
        folders: List<FolderSummary> = listOf(folder("photos")),
        conflicts: List<ConflictSummary> = emptyList(),
        wizard: FolderWizardSnapshot? = null,
        error: LocalizedText? = null,
    ) = DevicesSnapshot(
        devices = emptyList(),
        folders = folders,
        followed = emptyList(),
        // `None` = unknown, the documented hedge on the website toggle's
        // tri-state hint — a sibling field-add this fixture had not caught up
        // with (android is not CI-gated, so it landed red).
        websiteAddressEnabled = null,
        conflicts = conflicts,
        wizard = wizard,
        error = error,
        // Sibling field-adds (the fleet-removal door): no unaccounted members,
        // and no own fleet id, so no own fingerprint either.
        members = emptyList(),
        ownFleetId = null,
        ownFingerprint = null,
        // Sibling field-add: this device's own p2p participation, unknown.
        ownP2pParticipation = null,
    )

    private val context: Context = ApplicationProvider.getApplicationContext()

    private fun render(
        snapshot: DevicesSnapshot? = snapshot(),
        actions: FoldersActions = FoldersActions(),
        wizardActions: WizardActions = WizardActions(),
        folderActors: Map<String, List<FfiFolderActorMember>> = emptyMap(),
        folderDeviceActivity: Map<String, List<FfiFolderDevice>> = emptyMap(),
        folderDestinationPlaces: Map<String, List<com.fauna.ffi.FfiFolderDestinationPlace>> = emptyMap(),
        // Defaults to "mail is set up" (the actor holds an MSEK) — the ordinary case
        // the other cases exercise. `FoldersContent`'s own default is the opposite
        // (false = fail-safe: don't offer a control that cannot succeed); the
        // MSEK-less rendering is asserted explicitly by the needs-mail cases below.
        canServeWebdav: Boolean = true,
        // Defaults to "the actor has a tier" (the ordinary paywall-select case);
        // the no-tiers disabled rendering is asserted explicitly below.
        ownTiers: List<String> = listOf("gold"),
        defaultConflictPolicy: String? = null,
        pendingShares: List<FfiPendingShare> = emptyList(),
        conflictBadgeLabels: Map<Long, LocalizedText> = emptyMap(),
        nestSnapshotsOptions: List<NestSnapshotsOption> = nestSnapshotOptions,
        nestPlaceEdits: Map<String, NestPlaceEdit> = emptyMap(),
        versionRetentionEdits: Map<String, VersionRetentionEdit> = emptyMap(),
    ) {
        composeTestRule.setContent {
            FoldersContent(
                snapshot,
                actions,
                wizardActions,
                folderActors = folderActors,
                folderDeviceActivity = folderDeviceActivity,
                folderDestinationPlaces = folderDestinationPlaces,
                canServeWebdav = canServeWebdav,
                ownTiers = ownTiers,
                defaultConflictPolicy = defaultConflictPolicy,
                pendingShares = pendingShares,
                conflictBadgeLabels = conflictBadgeLabels,
                nestSnapshotsOptions = nestSnapshotsOptions,
                nestPlaceEdits = nestPlaceEdits,
                versionRetentionEdits = versionRetentionEdits,
            )
        }
    }

    @Test
    fun folderRowAndAddButtonRender() {
        var opened = false
        render(actions = FoldersActions(onOpenWizard = { opened = true }))
        composeTestRule.onNodeWithTag("folder-row").assertExists()
        composeTestRule.onNodeWithTag("folder-add-button").performScrollTo().performClick()
        assertTrue(opened)
    }

    @Test
    fun ownerRowPaintsNoFrequencySelect() {
        // Phase 5 retired the per-row scan-frequency editor (`file-sync.md` § Config,
        // the phase-5 block): the cadence is a constant, not a row value.
        render(snapshot(folders = listOf(folder("photos"))))
        composeTestRule.onNodeWithTag("folder-row").assertExists()
        composeTestRule.onNodeWithTag("folder-frequency-select").assertDoesNotExist()
        composeTestRule.onAllNodesWithText("15 min").assertCountEquals(0)
    }

    @Test
    fun folderExpandRevealsSelectiveSyncAndSaveFiresWithRawText() {
        // onSavePaths now takes the RAW field text (not a pre-split list) — parsing
        // (parsePathsField) moved to the VM boundary so Content stays FFI-free; an
        // untouched field is empty text, not a pre-split empty list.
        var saved: Triple<String, String, String>? = null
        render(actions = FoldersActions(onSavePaths = { name, inc, exc -> saved = Triple(name, inc, exc) }))
        // Selective-sync controls hidden until the row expands.
        composeTestRule.onNodeWithTag("folder-include-paths").assertDoesNotExist()
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-include-paths").assertExists()
        composeTestRule.onNodeWithTag("folder-exclude-paths").assertExists()
        composeTestRule.onNodeWithTag("folder-save-paths").performScrollTo().performClick()
        assertEquals(Triple("photos", "", ""), saved)
    }

    @Test
    fun deleteFolderConfirmDialogFires() {
        var deleted: String? = null
        render(actions = FoldersActions(onDeleteFolder = { deleted = it }))
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-delete-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("folder-delete-confirm").performClick()
        assertEquals("photos", deleted)
    }

    @Test
    fun conflictSurfaceHiddenWhenNoConflicts() {
        // Default snapshot carries no conflicts → no resolve button.
        render()
        composeTestRule.onNodeWithTag("conflict-resolve-button").assertDoesNotExist()
    }

    // ── Conflict review row (folders.md § Conflicts) — badge from the injected
    // `conflictBadgeLabels` map, `conflict-file-info` = the precomputed `fileInfo`
    // field, `conflict-resolve-button` gated on `hasOtherVersion`, an unresolved row
    // renders "awaiting device" with no button ─────────────────────────────────

    @Test
    fun conflictBadgeRendersTheInjectedLocalizedLabelNotTheRawType() {
        render(
            snapshot(conflicts = listOf(conflict(resolvedAt = 1L, resolution = "merged"))),
            conflictBadgeLabels = mapOf(5L to LocalizedText("devices.conflicts.resolved_merged", emptyMap())),
        )
        composeTestRule.onNodeWithTag("conflict-type-badge").assertTextEquals("Merged")
    }

    @Test
    fun conflictFileInfoRendersThePrecomputedField() {
        render(
            snapshot(
                conflicts = listOf(
                    conflict(resolvedAt = 1L, resolution = "merged", fileInfo = "photos: a/b.jpg → ffffffff"),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("conflict-file-info").assertTextEquals("photos: a/b.jpg → ffffffff")
    }

    @Test
    fun resolveButtonHiddenOnUnresolvedRowShowsAwaitingDeviceInstead() {
        render(snapshot(conflicts = listOf(conflict(resolvedAt = null, winningManifestHash = null))))
        composeTestRule.onNodeWithTag("conflict-resolve-button").assertDoesNotExist()
        composeTestRule.onNodeWithText("Awaiting device").assertExists()
    }

    @Test
    fun resolveButtonHiddenOnResolvedRowWithNoOtherVersion() {
        // Resolved but no retained non-winning candidate (candidate-free (mark-only) resolve) —
        // nothing to re-point at; no button, no "awaiting device" caption either.
        render(
            snapshot(
                conflicts = listOf(
                    conflict(resolvedAt = 1L, winningManifestHash = "ff".repeat(32), hasOtherVersion = false),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("conflict-resolve-button").assertDoesNotExist()
        composeTestRule.onNodeWithText("Awaiting device").assertDoesNotExist()
    }

    @Test
    fun resolveButtonShownAndFiresUseOtherVersionOnRepointableRow() {
        var used: Long? = null
        render(
            snapshot(
                conflicts = listOf(
                    conflict(resolvedAt = 1L, winningManifestHash = "ff".repeat(32), hasOtherVersion = true),
                ),
            ),
            actions = FoldersActions(onUseOtherVersion = { id -> used = id }),
        )
        composeTestRule.onNodeWithTag("conflict-resolve-button").assertTextEquals("Use the other version")
        composeTestRule.onNodeWithTag("conflict-resolve-button").performClick()
        assertEquals(5L, used)
    }

    // ── Wizard step 1 — the name, and NOTHING ELSE (folders re-model phase 2
    // slice e: a folder has no type, so `wizard-mode-*` are retired ids) ────────

    @Test
    fun wizardNameStepRendersNameOnlyAndNextFires() {
        var next = false
        render(
            snapshot(wizard = wizard(FolderWizardStep.NAME, name = "vacation")),
            wizardActions = WizardActions(onNext = { next = true }),
        )
        composeTestRule.onNodeWithTag("wizard-name-input").assertExists()
        // The seven retirements are ui.yaml-level: painting one is the defect this
        // asserts against, not merely dead UI (`ui/folders.md` § Element IDs).
        composeTestRule.onNodeWithTag("wizard-mode-sync").assertDoesNotExist()
        composeTestRule.onNodeWithTag("wizard-mode-backup").assertDoesNotExist()
        composeTestRule.onNodeWithTag("wizard-mode-web").assertDoesNotExist()
        composeTestRule.onNodeWithTag("wizard-next-button").performClick()
        assertTrue(next)
    }

    @Test
    fun wizardNameStepExplainsWhyNextIsDisabledWhileTheNameIsEmpty() {
        // A greyed-out Next with no on-screen reason was a live-user "unknowable"
        // report; tui and linux carry the same line.
        render(snapshot(wizard = wizard(FolderWizardStep.NAME, name = "")))
        composeTestRule.onNodeWithText("Enter a name to continue.").assertExists()
    }

    @Test
    fun wizardNameStepWithdrawsTheHintOnceTheNameIsValid() {
        render(snapshot(wizard = wizard(FolderWizardStep.NAME, name = "vacation")))
        composeTestRule.onNodeWithText("Enter a name to continue.").assertDoesNotExist()
    }

    // ── Wizard step 2 — the three place-flag checkboxes replace the retired
    // Source/Sync/Backup/Mirror picker (`wizard-device-role`) ───────────────────

    @Test
    fun wizardDeviceStepRendersTheThreePlaceFlagsForAnEnrolledSeat() {
        render(
            snapshot(
                wizard = wizard(
                    FolderWizardStep.DEVICES,
                    devices = listOf(wizardDevice("laptop")),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("wizard-device-check").assertExists()
        composeTestRule.onNodeWithTag("wizard-device-originates").assertExists().assertIsOn()
        composeTestRule.onNodeWithTag("wizard-device-accepts").assertExists().assertIsOn()
        composeTestRule.onNodeWithTag("wizard-device-applies-deletes").assertExists().assertIsOn()
        composeTestRule.onNodeWithTag("wizard-device-role").assertDoesNotExist()
    }

    @Test
    fun wizardDeviceStepRendersTheFlagsForAnUnEnrolledSeatToo() {
        // The three flag boxes render for EVERY seat, enrolled or not — matching
        // tui (the lead app), linux, web and apple. Ticking a box does not
        // auto-enroll the seat (`set_device_flags` only writes the flag triple),
        // so this is safe: the boxes describe what the seat WOULD do if enrolled.
        render(
            snapshot(
                wizard = wizard(
                    FolderWizardStep.DEVICES,
                    devices = listOf(wizardDevice("laptop", selected = false)),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("wizard-device-check").assertExists()
        composeTestRule.onNodeWithTag("wizard-device-originates").assertExists().assertIsOn()
        composeTestRule.onNodeWithTag("wizard-device-accepts").assertExists().assertIsOn()
        composeTestRule.onNodeWithTag("wizard-device-applies-deletes").assertExists().assertIsOn()
    }

    @Test
    fun wizardDeviceStepReflectsTheArchiveSeatAndSendsTheWholeTripleOnAToggle() {
        // The archive seat — accepts but never deletes — is the flag point the
        // retired `backup` role named, and the one a user reaches by clearing ONE
        // box. The gesture is whole-value: the two untouched flags must ride along,
        // or the machine reads them as cleared.
        var sent: List<Any>? = null
        render(
            snapshot(
                wizard = wizard(
                    FolderWizardStep.DEVICES,
                    devices = listOf(wizardDevice("archive-box", appliesDeletes = false)),
                ),
            ),
            wizardActions = WizardActions(
                onSetFlags = { i, o, a, d -> sent = listOf(i, o, a, d) },
            ),
        )
        composeTestRule.onNodeWithTag("wizard-device-applies-deletes").assertIsOff()
        // performScrollTo first: the wizard body is a scrolling Column, and a click
        // on a node below the fold is silently a no-op under Robolectric.
        composeTestRule.onNodeWithTag("wizard-device-originates").performScrollTo().performClick()
        assertEquals(listOf(0, false, true, false), sent)
    }

    @Test
    fun wizardDeviceStepExplainsEachFlagInline() {
        // Each box carries its own one-line explainer — the pattern the retired role
        // picker used per seat, now per box, because the flags are what needs
        // explaining ("a completely incomprehensible list of things", 2026-08-05).
        render(
            snapshot(
                wizard = wizard(
                    FolderWizardStep.DEVICES,
                    devices = listOf(wizardDevice("laptop")),
                ),
            ),
        )
        composeTestRule
            .onNodeWithText("Files you add or edit on this device are sent to the rest of the folder.")
            .assertExists()
        composeTestRule
            .onNodeWithText("Changes made on your other devices land on this one.")
            .assertExists()
    }

    // ── Wizard step 3 is Review — the frequency step retired in phase 5 ────────

    @Test
    fun wizardReviewPaintsNeitherTheRetiredRetentionBoxesNorACadenceOption() {
        // Retention is the nest place's per-folder policy (slice e), editable on ANY
        // folder's expanded row — not a create-time question; and the scan cadence
        // is a constant, not a choice (phase 5), so no step offers one.
        render(snapshot(wizard = wizard(FolderWizardStep.REVIEW, name = "vacation")))
        composeTestRule.onNodeWithTag("wizard-retention-snapshots").assertDoesNotExist()
        composeTestRule.onNodeWithTag("wizard-retention-days").assertDoesNotExist()
        composeTestRule.onNodeWithTag("wizard-frequency-option").assertDoesNotExist()
        composeTestRule.onNodeWithTag("wizard-create-button").assertExists()
    }

    @Test
    fun wizardReviewStepShowsCreateButton() {
        var created = false
        render(
            snapshot(wizard = wizard(FolderWizardStep.REVIEW, name = "vacation")),
            wizardActions = WizardActions(onCreate = { created = true }),
        )
        composeTestRule.onNodeWithTag("wizard-create-button").performClick()
        assertTrue(created)
    }

    // ── The nest place's snapshot policy (`folder-nest-*`) — backup-restore.md
    // § 8b. Renders on ANY folder (a folder has no type) ──────────────────────

    /** Expand a row so its body (where the editor lives) is composed. */
    private fun expandFirstFolderRow() {
        composeTestRule.onNodeWithText("Selective Sync").performScrollTo().performClick()
    }

    @Test
    fun nestPlaceEditorRendersOnAnyFolderNotJustABackupOne() {
        render(snapshot(folders = listOf(folder("photos"))))
        expandFirstFolderRow()
        composeTestRule.onNodeWithTag("folder-nest-snapshots-select").assertExists()
        composeTestRule.onNodeWithTag("folder-nest-quiet-input").assertExists()
        composeTestRule.onNodeWithTag("folder-nest-retention-snapshots").assertExists()
        composeTestRule.onNodeWithTag("folder-nest-retention-days").assertExists()
        composeTestRule.onNodeWithTag("folder-version-retention-count").assertExists()
        composeTestRule.onNodeWithTag("folder-version-retention-days").assertExists()
        composeTestRule.onNodeWithTag("folder-nest-save-button").assertExists()
    }

    @Test
    fun versionRetentionEditorSeedsFromTheSharedPrefill() {
        // The version-retention SIBLING pair — its own prefill rule
        // set (`versionRetentionEditFromBounds`), injected as plain data exactly
        // like nestPlaceEdits above: a zero bound renders BLANK, never "0".
        render(
            snapshot(folders = listOf(folder("photos"))),
            versionRetentionEdits = mapOf(
                "photos" to VersionRetentionEdit(count = "3", days = "30"),
            ),
        )
        expandFirstFolderRow()
        composeTestRule.onNodeWithTag("folder-version-retention-count")
            .performScrollTo()
            .assertTextContains("3")
        composeTestRule.onNodeWithTag("folder-version-retention-days")
            .performScrollTo()
            .assertTextContains("30")
    }

    @Test
    fun versionRetentionEditorRidesTheSameSaveButtonAsTheNestPlaceKnobs() {
        // The version-retention pair is a SIBLING family, sent whole on the SAME
        // folder-nest-save-button click — never folded into the snapshot retention.
        var sent: List<String>? = null
        render(
            snapshot(folders = listOf(folder("photos"))),
            actions = FoldersActions(
                onSaveNestPlace = { n, s, q, rs, rd, vc, vd -> sent = listOf(n, s, q, rs, rd, vc, vd) },
            ),
            nestPlaceEdits = mapOf(
                "photos" to NestPlaceEdit(
                    snapshots = "default",
                    quietSecs = "",
                    retentionSnapshots = "",
                    retentionDays = "",
                ),
            ),
        )
        expandFirstFolderRow()
        composeTestRule.onNodeWithTag("folder-version-retention-count")
            .performScrollTo()
            .performTextInput("5")
        composeTestRule.onNodeWithTag("folder-version-retention-days")
            .performScrollTo()
            .performTextInput("60")
        composeTestRule.onNodeWithTag("folder-nest-save-button").performScrollTo().performClick()
        assertEquals(listOf("photos", "default", "", "", "", "5", "60"), sent)
    }

    @Test
    fun nestPlaceEditorSeedsFromTheSharedPrefill() {
        // The prefill is the shared rule set (`nestPlaceEditFromRow`), injected as
        // plain data — this render never re-derives it. `off` is the discriminating
        // seed: its label ("Don't keep snapshots") is the one option text that is NOT
        // also the select's own field label, so an unseeded control cannot pass this.
        render(
            snapshot(folders = listOf(folder("photos", nestSnapshots = false, nestSnapshotQuietSecs = 90L))),
            nestPlaceEdits = mapOf(
                "photos" to NestPlaceEdit(
                    snapshots = "off",
                    quietSecs = "90",
                    retentionSnapshots = "",
                    retentionDays = "",
                ),
            ),
        )
        expandFirstFolderRow()
        composeTestRule.onNodeWithTag("folder-nest-snapshots-select")
            .performScrollTo()
            .assertTextContains("Don't keep snapshots")
        composeTestRule.onNodeWithTag("folder-nest-quiet-input")
            .performScrollTo()
            .assertTextContains("90")
    }

    @Test
    fun nestPlaceEditorRendersAnUnsetKnobBlankRatherThanAsAChosenValue() {
        // The blank contract, asserted where it is observable: a knob the prefill
        // left blank must SAVE as blank. Rendering a `0` for an unset retention bound
        // would turn "nothing chosen" into a bound the owner appears to have picked,
        // and the two spellings would then drift on the next save.
        var sent: List<String>? = null
        render(
            snapshot(folders = listOf(folder("photos"))),
            actions = FoldersActions(
                onSaveNestPlace = { n, s, q, rs, rd, vc, vd -> sent = listOf(n, s, q, rs, rd, vc, vd) },
            ),
            nestPlaceEdits = mapOf(
                "photos" to NestPlaceEdit(
                    snapshots = "default",
                    quietSecs = "",
                    retentionSnapshots = "",
                    retentionDays = "",
                ),
            ),
        )
        expandFirstFolderRow()
        composeTestRule.onNodeWithTag("folder-nest-save-button").performScrollTo().performClick()
        assertEquals(listOf("photos", "default", "", "", "", "", ""), sent)
    }

    @Test
    fun nestPlaceSaveSendsAllKnobsRawIncludingTheEmptyOnes() {
        // The policy applies WHOLE: a knob omitted from the write clears back to
        // unset, so an emptied box must ride out as an empty string rather than be
        // dropped. Parsing is the VM's (`nestPlaceWrite`/`versionRetentionWrite`),
        // never this render's.
        var sent: List<String>? = null
        render(
            snapshot(folders = listOf(folder("photos"))),
            actions = FoldersActions(
                onSaveNestPlace = { n, s, q, rs, rd, vc, vd -> sent = listOf(n, s, q, rs, rd, vc, vd) },
            ),
            nestPlaceEdits = mapOf(
                "photos" to NestPlaceEdit(
                    snapshots = "default",
                    quietSecs = "",
                    retentionSnapshots = "7",
                    retentionDays = "",
                ),
            ),
        )
        expandFirstFolderRow()
        composeTestRule.onNodeWithTag("folder-nest-save-button").performScrollTo().performClick()
        assertEquals(listOf("photos", "default", "", "7", "", "", ""), sent)
    }

    @Test
    fun nestPlaceEditorStagesLocallyAndOnlyCommitsOnSave() {
        // Every knob is staged and committed together — an apply-on-change knob here
        // would have to send its three siblings with it, committing half-typed values
        // the user had not saved.
        var sent: List<String>? = null
        render(
            snapshot(folders = listOf(folder("photos"))),
            actions = FoldersActions(
                onSaveNestPlace = { n, s, q, rs, rd, vc, vd -> sent = listOf(n, s, q, rs, rd, vc, vd) },
            ),
            nestPlaceEdits = mapOf(
                "photos" to NestPlaceEdit(
                    snapshots = "default",
                    quietSecs = "",
                    retentionSnapshots = "",
                    retentionDays = "",
                ),
            ),
        )
        expandFirstFolderRow()
        composeTestRule.onNodeWithTag("folder-nest-quiet-input").performScrollTo().performTextInput("120")
        assertNull(sent)
        composeTestRule.onNodeWithTag("folder-nest-save-button").performScrollTo().performClick()
        assertEquals(listOf("photos", "default", "120", "", "", "", ""), sent)
    }

    @Test
    fun nestPlaceEditorSaysThatAnEmptyBoxIsAChoice() {
        // Not decoration: the only on-screen statement that emptying a box is a real
        // choice rather than a no-op.
        render(snapshot(folders = listOf(folder("photos"))))
        expandFirstFolderRow()
        composeTestRule
            .onNodeWithText(
                "Leave a box empty to use the default. Empty is a choice — saving applies every box together.",
            )
            .performScrollTo()
            .assertExists()
    }

    // ── WebDAV per-set serve toggle (`folder-webdav-toggle`) — webdav-server.md
    // § Independent enablement point 2, slice 6b-3 ─────────────────────────────

    @Test
    fun webdavToggleRendersForSyncModeSetAndFires() {
        var served: Triple<String, String?, Boolean>? = null
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = "aa".repeat(32)))),
            actions = FoldersActions(onServeWebdav = { n, g, e -> served = Triple(n, g, e) }),
        )
        composeTestRule.onNodeWithTag("folder-webdav-toggle").assertExists().assertIsOff()
        composeTestRule.onNodeWithTag("folder-webdav-toggle").performClick()
        assertEquals(Triple("photos", "aa".repeat(32), true), served)
    }

    @Test
    fun webdavToggleReflectsPersistedEnabledState() {
        render(snapshot(folders = listOf(folder("photos", webdavEnabled = true))))
        composeTestRule.onNodeWithTag("folder-webdav-toggle").assertIsOn()
    }

    @Test
    fun webdavToggleShownForEveryOwnerFolder() {
        // A folder has no type (webdav-server.md § What the namespace is): the
        // former sync-type gate retired with the mode.
        render(snapshot(folders = listOf(folder("photos"))))
        composeTestRule.onNodeWithTag("folder-webdav-toggle").assertExists()
    }

    // ── 6b-2(c): disable-with-hint while the actor holds no MSEK ───────────────
    // Serving seals the WebdavKeysBlob under the mail encryption key, so an actor
    // who has not set up mail cannot serve. `serve_set` flips the nest flag BEFORE
    // re-provisioning the blob, so a click would commit the flag and only then fail
    // NoMsek — hence disabled, not merely error-on-click.

    @Test
    fun webdavToggleIsDisabledWhenTheActorHasNoMsek() {
        render(
            snapshot(folders = listOf(folder("photos"))),
            canServeWebdav = false,
        )
        composeTestRule.onNodeWithTag("folder-webdav-toggle").assertExists().assertIsNotEnabled()
    }

    @Test
    fun webdavToggleDoesNotFireWhenTheActorHasNoMsek() {
        var served: Triple<String, String?, Boolean>? = null
        render(
            snapshot(folders = listOf(folder("photos"))),
            actions = FoldersActions(onServeWebdav = { n, g, e -> served = Triple(n, g, e) }),
            canServeWebdav = false,
        )
        composeTestRule.onNodeWithTag("folder-webdav-toggle").performClick()
        assertNull("a disabled toggle must not reach serve_set", served)
    }

    @Test
    fun webdavHintExplainsWhyTheToggleIsDisabled() {
        render(
            snapshot(folders = listOf(folder("photos"))),
            canServeWebdav = false,
        )
        composeTestRule
            .onNodeWithText(
                context.getString(R.string.devices_serve_webdav_needs_mail),
            )
            .assertExists()
    }

    @Test
    fun webdavToggleIsEnabledWhenTheActorHasAnMsek() {
        render(
            snapshot(folders = listOf(folder("photos"))),
            canServeWebdav = true,
        )
        composeTestRule.onNodeWithTag("folder-webdav-toggle").assertExists().assertIsEnabled()
    }

    // ── Per-set conflict policy (`folder-conflict-policy-select`, every
    // owner row) — file-sync.md § Conflicts. Options/labels come from the shared
    // `conflictPolicyOptions()`/`conflictPolicyLabel()` catalog (real FFI calls;
    // `just android-host-test`'s host-JNA wiring makes them work under Robolectric,
    // same as `FeedCreateDialogTest`'s `ruleTypeOptions()`) ───────────────────────

    @Test
    fun conflictPolicySelectRendersForSyncModeAndFires() {
        var set: Pair<String, String>? = null
        render(
            snapshot(folders = listOf(folder("photos", conflictPolicy = "auto"))),
            actions = FoldersActions(onSetConflictPolicy = { n, p -> set = n to p }),
        )
        composeTestRule.onNodeWithTag("folder-conflict-policy-select").assertExists()
        composeTestRule.onNodeWithTag("folder-conflict-policy-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Latest edit wins").performClick()
        assertEquals("photos" to "latest_wins_always", set)
    }

    @Test
    fun conflictPolicySelectDefaultsToAutoWhenAbsent() {
        // A row with no conflictPolicy (null) renders the column default. The
        // always-present Sync-defaults section (also defaulting to "auto" here)
        // shows the same label, so this asserts at least one match rather than
        // exactly one (onNodeWithText requires unambiguous single-match).
        render(snapshot(folders = listOf(folder("photos", conflictPolicy = null))))
        composeTestRule.onAllNodesWithText("Auto (merge text, else latest wins)")[0].assertExists()
    }

    @Test
    fun conflictPolicySelectShownForEveryOwnerFolder() {
        // A folder has no type (ui/folders.md § Conflicts): every owner row.
        render(snapshot(folders = listOf(folder("photos"))))
        composeTestRule.onNodeWithTag("folder-conflict-policy-select").assertExists()
    }

    // ── The nest place's content residency (`folder-nest-residency-select` /
    // `folder-residency-confirm`, EVERY row) — folders re-model phase 5, file-sync.md § Content residency.
    // Applies ON CHANGE, unlike NestPlaceSection's batched save. Options/labels
    // from the shared `residencyOptions()`/`residencyLabel()` catalog (real FFI
    // calls under Robolectric, same host-JNA wiring as the conflict-policy
    // tests above) ───────────────────────────────────────────────────────────

    @Test
    fun residencySelectDefaultsToFullWhenAbsent() {
        render(snapshot(folders = listOf(folder("photos", residency = ""))))
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithText("Full — the nest keeps this folder's content").assertExists()
    }

    @Test
    fun residencySelectPickingFullCommitsDirectly() {
        // A folder already Metadata-only, flipped back to Full — the
        // non-destructive direction commits on change, exactly like the
        // audience select's non-public picks.
        var set: Pair<String, String>? = null
        render(
            snapshot(folders = listOf(folder("photos", residency = "metadata_only"))),
            actions = FoldersActions(onSetResidency = { n, r -> set = n to r }),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-nest-residency-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Full — the nest keeps this folder's content").performClick()
        assertEquals("photos" to "full", set)
        composeTestRule.onNodeWithTag("folder-residency-confirm").assertDoesNotExist()
    }

    @Test
    fun residencySelectPickingMetadataOnlyArmsTheConfirmAndWritesNothingUntilAnswered() {
        // ⚠ The load-bearing half of the residency gate: picking Metadata-only
        // must NOT reach onSetResidency until the destructive confirm is
        // answered — the nest deletes its copy of the folder's content on that
        // write. Mirrors deleteFolderConfirmDialogFires' shape.
        var set: Pair<String, String>? = null
        render(
            snapshot(folders = listOf(folder("photos", residency = ""))),
            actions = FoldersActions(onSetResidency = { n, r -> set = n to r }),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-nest-residency-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Metadata only — content stays on my devices").performClick()
        assertNull(set)
        composeTestRule.onNodeWithTag("folder-residency-confirm").performClick()
        assertEquals("photos" to "metadata_only", set)
    }

    // ── Per-set "paywall to tier" (`folder-paywall-tier-select`, website-enabled
    // rows only) — folders.md § Web paywall / monetization.md § Pillar 2. v1 is
    // SET-ONLY; the empty placeholder is not a write ────────────────────────────

    @Test
    fun paywallSelectRendersForWebsiteEnabledAndFires() {
        var paywalled: Triple<String, String?, String>? = null
        render(
            snapshot(folders = listOf(folder("gallery", websiteEnabled = true, mlsGroupId = "aa".repeat(32)))),
            actions = FoldersActions(onPaywallSet = { n, g, t -> paywalled = Triple(n, g, t) }),
            ownTiers = listOf("gold", "silver"),
        )
        composeTestRule.onNodeWithTag("folder-paywall-tier-select").assertExists().assertIsEnabled()
        composeTestRule.onNodeWithTag("folder-paywall-tier-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("gold").performClick()
        assertEquals(Triple("gallery", "aa".repeat(32), "gold"), paywalled)
    }

    @Test
    fun paywallSelectShowsNotPaywalledPlaceholderWhenPublic() {
        render(
            snapshot(folders = listOf(folder("gallery", websiteEnabled = true, webPaywallTier = null))),
            ownTiers = listOf("gold"),
        )
        composeTestRule.onNodeWithText("Not paywalled (public)").assertExists()
    }

    @Test
    fun paywallSelectShowsCurrentTierWhenAlreadyPaywalled() {
        render(
            snapshot(folders = listOf(folder("gallery", websiteEnabled = true, webPaywallTier = "gold"))),
            ownTiers = listOf("gold"),
        )
        composeTestRule.onAllNodesWithText("gold")[0].assertExists()
    }

    @Test
    fun paywallSelectingThePlaceholderDoesNotFire() {
        // v1 is set-only — no clear affordance; picking the placeholder is a no-op.
        // The anchor (already "Not paywalled (public)") and the menu's placeholder
        // item share the same text once expanded — onLast() targets the menu item,
        // matching the AdminUsersContentTest precedent for this exact collision.
        var fired = false
        render(
            snapshot(folders = listOf(folder("gallery", websiteEnabled = true, webPaywallTier = null))),
            actions = FoldersActions(onPaywallSet = { _, _, _ -> fired = true }),
            ownTiers = listOf("gold"),
        )
        composeTestRule.onNodeWithTag("folder-paywall-tier-select").performScrollTo().performClick()
        composeTestRule.onAllNodesWithText("Not paywalled (public)").onLast().performClick()
        assertTrue(!fired)
    }

    @Test
    fun paywallSelectHiddenWhenTheWebsiteToggleIsOff() {
        render(snapshot(folders = listOf(folder("photos"))))
        composeTestRule.onNodeWithTag("folder-paywall-tier-select").assertDoesNotExist()
    }

    @Test
    fun paywallSelectDisabledWhenTheActorHasNoTiers() {
        render(
            snapshot(folders = listOf(folder("gallery", websiteEnabled = true))),
            ownTiers = emptyList(),
        )
        composeTestRule.onNodeWithTag("folder-paywall-tier-select").assertExists().assertIsNotEnabled()
        composeTestRule
            .onNodeWithText(context.getString(R.string.devices_paywall_tier_needs_tier))
            .assertExists()
    }

    // ── Page-level "Sync defaults" (`sync-default-conflict-policy-select`) —
    // file-sync.md § Conflicts, policy; global default for NEW sets only ────────

    @Test
    fun syncDefaultsSectionRendersAndFires() {
        var saved: String? = null
        render(actions = FoldersActions(onSetDefaultConflictPolicy = { saved = it }))
        composeTestRule.onNodeWithTag("sync-default-conflict-policy-select").assertExists()
        composeTestRule.onNodeWithTag("sync-default-conflict-policy-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Latest edit wins").performClick()
        assertEquals("latest_wins_always", saved)
    }

    @Test
    fun syncDefaultsSectionDefaultsToAutoWhenNoPreference() {
        render(defaultConflictPolicy = null)
        composeTestRule.onAllNodesWithText("Auto (merge text, else latest wins)")[0].assertExists()
    }

    // ── Cross-user sharing (owner side) — folders.md § Sharing ──────────────

    @Test
    fun sharedSetLoadsRosterAndRendersBadge() {
        // A shared set (mls_group_id present) eager-loads its roster (even collapsed)
        // and shows the "Shared · N" badge once ≥1 member is surfaced.
        var loaded: String? = null
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = "aa".repeat(32)))),
            actions = FoldersActions(onLoadMembers = { loaded = it }),
            folderActors = mapOf("photos" to listOf(member("alice@fauna.social"))),
        )
        assertEquals("photos", loaded)
        composeTestRule.onNodeWithTag("folder-shared-badge").assertExists()
        composeTestRule.onAllNodesWithText("Shared · 1")[0].assertExists()
    }

    @Test
    fun ownerOnlySetShowsNoBadgeAndDoesNotLoad() {
        // An owner-only set (mls_group_id null) neither loads a roster nor shows a badge.
        var loaded = false
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = null))),
            actions = FoldersActions(onLoadMembers = { loaded = true }),
        )
        assertTrue(!loaded)
        composeTestRule.onNodeWithTag("folder-shared-badge").assertDoesNotExist()
    }

    // ── Per-set device activity (`folder-device-activity-item`/-label/-count) —
    // the ordinary sync change signal, file-sync.md § Implementation status
    // today. Loaded on expand (`onFolderExpandedChanged`); the push-driven
    // LIVE-UPDATE half (re-fetch on `fauna.sync.changed`) is a `DevicesVM`
    // concern, covered by `DevicesVMTest` — this Content is FFI/VM-free and only
    // renders whatever `folderDeviceActivity` map it's handed. ────────────────

    @Test
    fun expandingARowFiresOnFolderExpandedChangedAndCollapsingFiresFalse() {
        val changes = mutableListOf<Pair<String, Boolean>>()
        render(actions = FoldersActions(onFolderExpandedChanged = { n, _, e -> changes.add(n to e) }))
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithText("Cancel").performClick()
        // `onFolderExpandedChanged` fires from `LaunchedEffect(expanded, folder.name)`
        // (FoldersScreen.kt), not from the click handler directly — a SECOND
        // `performClick()` back-to-back with no settling between them isn't
        // reliably caught by that click's own implicit wait-for-idle here: the
        // collapse click simultaneously disposes a whole subtree (paths fields,
        // device-activity/shared-with sections) while relaunching this effect,
        // and Robolectric's Compose idling needs an explicit extra pass to drain
        // both. Same idiom as PostImageC2paBadgeTest / AtprotoSettingsContentTest
        // (LaunchedEffect-driven state needs `waitForIdle()` before asserting,
        // beyond what setContent()/performClick() already do internally).
        composeTestRule.waitForIdle()
        // The initial (collapsed) composition also fires once with `false` — a
        // harmless no-op on the VM side (removing an absent entry from a set).
        assertEquals(listOf("photos" to false, "photos" to true, "photos" to false), changes)
    }

    @Test
    fun expandedRowRendersDeviceActivityFromTheInjectedMap() {
        render(
            folderDeviceActivity = mapOf(
                "photos" to listOf(deviceActivity("laptop", changeCount = 3)),
            ),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-device-activity-item").assertExists()
        composeTestRule.onNodeWithTag("folder-device-activity-label").assertTextEquals("laptop")
        composeTestRule.onNodeWithTag("folder-device-activity-count").assertTextEquals("3")
    }

    @Test
    fun expandedRowWithNoDeviceActivityShowsTheEmptyHintNotAnError() {
        // A freshly created set (or one whose read hasn't landed yet) shows "no
        // activity yet", never a page error — same degrade-to-empty shape as the
        // "Shared with" roster's "Not shared with anyone yet." hint.
        render()
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-device-activity-item").assertDoesNotExist()
        composeTestRule.onNodeWithText("No recorded activity yet.").assertExists()
    }

    @Test
    fun expandedRowRendersMultipleDeviceActivityRowsScopedToTheirOwnSet() {
        // A read for a DIFFERENT set must never paint onto this row (mirrors
        // tui's `a_device_activity_read_for_another_set_is_not_painted`).
        render(
            snapshot(folders = listOf(folder("photos"), folder("docs"))),
            folderDeviceActivity = mapOf(
                "photos" to listOf(
                    deviceActivity("laptop", changeCount = 3, deviceId = "aa".repeat(32)),
                    deviceActivity("phone", changeCount = 1, deviceId = "bb".repeat(32)),
                ),
                "docs" to listOf(deviceActivity("tablet", changeCount = 9, deviceId = "cc".repeat(32))),
            ),
        )
        // Both rows share the "Selective Sync" text — expand the first (photos).
        composeTestRule.onAllNodesWithText("Selective Sync")[0].performClick()
        composeTestRule.onAllNodesWithTag("folder-device-activity-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("folder-device-activity-label")[0].assertTextEquals("laptop")
        composeTestRule.onAllNodesWithTag("folder-device-activity-count")[0].assertTextEquals("3")
        composeTestRule.onAllNodesWithTag("folder-device-activity-label")[1].assertTextEquals("phone")
        composeTestRule.onAllNodesWithTag("folder-device-activity-count")[1].assertTextEquals("1")
    }

    // ── Destination places (`folder-destination-row`/-detach-button/-attach-select/
    // -attach-button) — backup-destinations.md § Ordinary-folder coverage. Hidden
    // entirely while no destination is enrolled at all (an affordance that cannot
    // work must not paint); the push-driven half is a `DevicesVM` concern, covered
    // by `DevicesVMDestinationPlacesTest` — this Content only renders whatever
    // `folderDestinationPlaces` map it's handed. ─────────────────────────────────

    @Test
    fun noEnrolledDestinationsPaintsNothingAtAll() {
        // The whole section — including its title — must not paint: an affordance
        // that cannot work (nothing enrolled) is not decoration.
        render()
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-destination-row").assertDoesNotExist()
        composeTestRule.onNodeWithText("Destination places").assertDoesNotExist()
    }

    @Test
    fun expandedRowRendersAttachedDestinationAndDetachFires() {
        var detached: Triple<String, Long, String>? = null
        render(
            actions = FoldersActions(
                onDetachDestination = { n, id, place -> detached = Triple(n, id, place.destinationId) },
            ),
            folderDestinationPlaces = mapOf(
                "photos" to listOf(destinationPlace("dest-1", "Offsite", attached = true, folderSet = "__folder/aa/1")),
            ),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-destination-row").assertExists()
        composeTestRule.onNodeWithText("Offsite").assertExists()
        composeTestRule.onNodeWithTag("folder-destination-attach-select").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-destination-detach-button").performScrollTo().performClick()
        assertEquals(Triple("photos", 1L, "dest-1"), detached)
    }

    @Test
    fun expandedRowRendersAttachSelectAndAttachFires() {
        var attached: Triple<String, Long, String>? = null
        render(
            actions = FoldersActions(
                onAttachDestination = { n, id, destId -> attached = Triple(n, id, destId) },
            ),
            folderDestinationPlaces = mapOf(
                "photos" to listOf(destinationPlace("dest-1", "Offsite", attached = false)),
            ),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-destination-row").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-destination-attach-select").assertExists()
        composeTestRule.onNodeWithTag("folder-destination-attach-button").performScrollTo().performClick()
        assertEquals(Triple("photos", 1L, "dest-1"), attached)
    }

    @Test
    fun attachSelectOffersOnlyUnattachedDestinationsAndPickingOneChangesTheSelection() {
        var attached: String? = null
        render(
            actions = FoldersActions(onAttachDestination = { _, _, destId -> attached = destId }),
            folderDestinationPlaces = mapOf(
                "photos" to listOf(
                    destinationPlace("dest-1", "Offsite", attached = true, folderSet = "__folder/aa/1"),
                    destinationPlace("dest-2", "Backup Box", attached = false),
                ),
            ),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        // Only the unattached destination is a pick candidate — the select
        // defaults to it, so a bare attach click already targets it.
        composeTestRule.onNodeWithTag("folder-destination-attach-select").performScrollTo().performClick()
        composeTestRule.onAllNodesWithText("Backup Box").onLast().performClick()
        composeTestRule.onNodeWithTag("folder-destination-attach-button").performScrollTo().performClick()
        assertEquals("dest-2", attached)
    }

    @Test
    fun expandedRowScopesDestinationPlacesToTheirOwnSet() {
        // A read for a DIFFERENT set must never paint onto this row (mirrors the
        // device-activity scoping test above).
        render(
            snapshot(folders = listOf(folder("photos"), folder("docs"))),
            folderDestinationPlaces = mapOf(
                "photos" to listOf(destinationPlace("dest-1", "Offsite", attached = true, folderSet = "__folder/aa/1")),
                "docs" to listOf(destinationPlace("dest-2", "Backup Box", attached = true, folderSet = "__folder/aa/2")),
            ),
        )
        composeTestRule.onAllNodesWithText("Selective Sync")[0].performClick()
        composeTestRule.onAllNodesWithTag("folder-destination-row").assertCountEquals(1)
        composeTestRule.onNodeWithText("Offsite").assertExists()
        composeTestRule.onNodeWithText("Backup Box").assertDoesNotExist()
    }

    @Test
    fun expandedSharedSetRendersMemberRowAndRemoveFires() {
        var removed: Triple<String, String, String>? = null
        val memberId = "cd".repeat(32)
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = "aa".repeat(32)))),
            actions = FoldersActions(onRemoveMember = { n, m, g -> removed = Triple(n, m, g) }),
            folderActors = mapOf("photos" to listOf(member("bob@fauna.social", actorId = memberId))),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-member-item").assertExists()
        composeTestRule.onNodeWithTag("folder-member-handle").assertExists()
        // Status is client-derived and always "Active" (no nest status field).
        composeTestRule.onNodeWithTag("folder-member-status").assertTextEquals("Active")
        composeTestRule.onNodeWithTag("folder-member-remove-button").performScrollTo().performClick()
        assertEquals(Triple("photos", memberId, "aa".repeat(32)), removed)
    }

    @Test
    fun memberRowRoleSelectDefaultsToReaderAndChangeFiresSetMemberAccess() {
        var setAccess: kotlin.collections.List<Any?>? = null
        val memberId = "cd".repeat(32)
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = "aa".repeat(32)))),
            actions = FoldersActions(onSetMemberAccess = { n, m, a, c -> setAccess = listOf(n, m, a, c) }),
            folderActors = mapOf("photos" to listOf(member("bob@fauna.social", actorId = memberId, access = "reader"))),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-member-role-select").assertExists()
        composeTestRule.onNodeWithTag("folder-writer-uncapped-warning").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-member-role-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Writer").performClick()
        assertEquals(listOf("photos", memberId, "writer", null), setAccess)
        composeTestRule.onNodeWithTag("folder-writer-uncapped-warning").assertExists()
    }

    @Test
    fun memberRowCapInputCommitsOnImeDoneWithCurrentAccess() {
        var setAccess: kotlin.collections.List<Any?>? = null
        val memberId = "cd".repeat(32)
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = "aa".repeat(32)))),
            actions = FoldersActions(onSetMemberAccess = { n, m, a, c -> setAccess = listOf(n, m, a, c) }),
            folderActors = mapOf(
                "photos" to listOf(member("bob@fauna.social", actorId = memberId, access = "writer", byteCap = null)),
            ),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-writer-uncapped-warning").assertExists()
        composeTestRule.onNodeWithTag("folder-member-cap-input").performScrollTo().performTextInput("5000")
        composeTestRule.onNodeWithTag("folder-member-cap-input").performImeAction()
        assertEquals(listOf("photos", memberId, "writer", 5000L), setAccess)
        composeTestRule.onNodeWithTag("folder-writer-uncapped-warning").assertDoesNotExist()
    }

    // ── The published-folder writer warning (folders.md § Sharing) ──
    // Reach, not the website toggle: `public` or paywalled. State-based, so the
    // member row and the share sheet both paint it and it stacks with the
    // uncapped warning rather than replacing it. The reach/access matrix is pinned
    // once, in `fauna-folders-machine`; these pin that android asks it and paints
    // the answer.

    @Test
    fun aWriterRowOnAPublicFolderWarnsEvenWhenCapped() {
        val memberId = "cd".repeat(32)
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = "aa".repeat(32), audience = "public"))),
            folderActors = mapOf(
                "photos" to listOf(member("bob@fauna.social", actorId = memberId, access = "writer", byteCap = 4096L)),
            ),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-writer-published-warning").assertExists()
        composeTestRule.onNodeWithText("This folder is public, so this member can change what anyone can see.")
            .assertExists()
        // A byte cap bounds the owner's quota, not what the writer can publish.
        composeTestRule.onNodeWithTag("folder-writer-uncapped-warning").assertDoesNotExist()
    }

    @Test
    fun aPaywalledFolderNamesSubscribersNotAnyone() {
        render(
            snapshot(
                folders = listOf(
                    folder("photos", mlsGroupId = "aa".repeat(32), audience = "shared", webPaywallTier = "gold"),
                ),
            ),
            folderActors = mapOf("photos" to listOf(member("bob@fauna.social", access = "writer"))),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithText("This folder is sold to subscribers, so this member can change what they see.")
            .assertExists()
    }

    @Test
    fun aReaderRowOnAPublicFolderCarriesNoPublishedWarning() {
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = "aa".repeat(32), audience = "public"))),
            folderActors = mapOf("photos" to listOf(member("bob@fauna.social", access = "reader"))),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-member-role-select").assertExists()
        composeTestRule.onNodeWithTag("folder-writer-published-warning").assertDoesNotExist()
    }

    @Test
    fun aWriterRowOnAFolderOnlyItsMembersCanReadCarriesNoPublishedWarning() {
        render(
            snapshot(folders = listOf(folder("notes", mlsGroupId = "bb".repeat(32), audience = "shared"))),
            folderActors = mapOf("notes" to listOf(member("carol@fauna.social", access = "writer"))),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-member-role-select").assertExists()
        composeTestRule.onNodeWithTag("folder-writer-published-warning").assertDoesNotExist()
    }

    @Test
    fun promotingAReaderOnAPublicFolderShowsThePublishedWarning() {
        val memberId = "cd".repeat(32)
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = "aa".repeat(32), audience = "public"))),
            actions = FoldersActions(onSetMemberAccess = { _, _, _, _ -> }),
            folderActors = mapOf("photos" to listOf(member("bob@fauna.social", actorId = memberId, access = "reader"))),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-writer-published-warning").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-member-role-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Writer").performClick()
        composeTestRule.onNodeWithTag("folder-writer-published-warning").assertExists()
    }

    @Test
    fun shareSheetWarnsThatAWriterOnAPublicFolderChangesWhatOutsidersSee() {
        render(snapshot(folders = listOf(folder("photos", mlsGroupId = null, audience = "public"))))
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-share-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("folder-writer-published-warning").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-share-role-select").performClick()
        composeTestRule.onNodeWithText("Writer").performClick()
        composeTestRule.onNodeWithTag("folder-writer-published-warning").assertExists()
        // Stacked with — not replacing — the quota warning.
        composeTestRule.onNodeWithTag("folder-writer-uncapped-warning").assertExists()
    }

    @Test
    fun shareSheetOnAnUnpublishedFolderShowsNoPublishedWarningForAWriter() {
        render(snapshot(folders = listOf(folder("photos", mlsGroupId = null))))
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-share-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("folder-share-role-select").performClick()
        composeTestRule.onNodeWithText("Writer").performClick()
        composeTestRule.onNodeWithTag("folder-writer-uncapped-warning").assertExists()
        composeTestRule.onNodeWithTag("folder-writer-published-warning").assertDoesNotExist()
    }

    @Test
    fun expandedOwnerOnlySetShowsShareAffordanceAndEmptyHint() {
        // The "Shared with" section + Share… affordance render on ANY expanded row,
        // so an owner-only set can start sharing; the roster shows the empty hint.
        render(snapshot(folders = listOf(folder("photos", mlsGroupId = null))))
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-share-button").performScrollTo().assertExists()
        composeTestRule.onNodeWithText("Not shared with anyone yet.").assertExists()
    }

    @Test
    fun shareButtonOpensPickerAndConfirmFires() {
        var shared: Triple<String, String, String?>? = null
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = null))),
            actions = FoldersActions(onShareSet = { name, input, access, cb -> shared = Triple(name, input, access); cb(true) }),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-share-button").performScrollTo().performClick()
        // The reused recipient-picker IDs (no new picker IDs — priority #2).
        composeTestRule.onNodeWithTag("recipient-picker-input").performTextInput("alice@fauna.social")
        composeTestRule.onNodeWithTag("folder-share-confirm").performClick()
        // Default (Reader, no selection made) grants null access — the reader default.
        assertEquals(Triple("photos", "alice@fauna.social", null), shared)
    }

    @Test
    fun shareSheetRoleSelectDefaultsToReaderAndWriterShowsWarning() {
        render(snapshot(folders = listOf(folder("photos", mlsGroupId = null))))
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-share-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("folder-share-role-select").assertExists()
        composeTestRule.onNodeWithTag("folder-writer-uncapped-warning").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-share-role-select").performClick()
        composeTestRule.onNodeWithText("Writer").performClick()
        composeTestRule.onNodeWithTag("folder-writer-uncapped-warning").assertExists()
    }

    @Test
    fun shareButtonWithWriterGrantPassesWriterAccess() {
        var grantedAccess: String? = "unset"
        render(
            snapshot(folders = listOf(folder("photos", mlsGroupId = null))),
            actions = FoldersActions(onShareSet = { _, _, access, cb -> grantedAccess = access; cb(true) }),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-share-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("folder-share-role-select").performClick()
        composeTestRule.onNodeWithText("Writer").performClick()
        composeTestRule.onNodeWithTag("recipient-picker-input").performTextInput("alice@fauna.social")
        composeTestRule.onNodeWithTag("folder-share-confirm").performClick()
        assertEquals("writer", grantedAccess)
    }

    // ── Cross-user sharing (recipient side) — folders.md § Sharing, Recipient
    // side: the pending-knock "Shared with you" area + the shared-with-me row ───

    @Test
    fun pendingSharesSectionHiddenWhenEmpty() {
        render(pendingShares = emptyList())
        composeTestRule.onNodeWithTag("folder-pending-share").assertDoesNotExist()
    }

    @Test
    fun pendingShareRendersAndAcceptFires() {
        var accepted: Long? = null
        render(
            pendingShares = listOf(pendingShare(inboxId = 42L, sharedByDisplay = "alice@fauna.social")),
            actions = FoldersActions(onAcceptShare = { id -> accepted = id }),
        )
        composeTestRule.onNodeWithTag("folder-pending-share").assertExists()
        composeTestRule.onNodeWithText("Shared by alice@fauna.social").assertExists()
        composeTestRule.onNodeWithTag("folder-share-accept-button").performClick()
        assertEquals(42L, accepted)
    }

    @Test
    fun pendingShareDeclineFires() {
        var declined: Long? = null
        render(
            pendingShares = listOf(pendingShare(inboxId = 42L)),
            actions = FoldersActions(onDeclineShare = { id -> declined = id }),
        )
        composeTestRule.onNodeWithTag("folder-share-decline-button").performClick()
        assertEquals(42L, declined)
    }

    @Test
    fun pendingShareFallsBackToUnknownWhenSharedByDisplayIsEmpty() {
        // Empty `sharedByDisplay` = a fully unstamped cross-nest share (folders.md
        // § Sharing — Recipient gate: cross-nest stays handle-less by design).
        render(pendingShares = listOf(pendingShare(sharedByDisplay = "")))
        composeTestRule.onNodeWithText("Shared by Unknown").assertExists()
    }

    @Test
    fun memberFolderRowRendersReadOnlyWithBadgeAndLeaveFires() {
        var leftGroupId: String? = null
        render(
            snapshot(
                folders = listOf(
                    folder(
                        "team-notes",
                        mlsGroupId = "aa".repeat(32),
                        role = "member",
                        ownerDisplay = "bob@fauna.social",
                    ),
                ),
            ),
            actions = FoldersActions(onLeaveFolder = { g -> leftGroupId = g }),
        )
        composeTestRule.onNodeWithTag("folder-row").assertExists()
        composeTestRule.onNodeWithTag("folder-shared-badge").assertTextEquals("Shared by bob@fauna.social")
        // Read-only: no owner affordances reach a member row.
        composeTestRule.onNodeWithTag("folder-share-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-delete-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-frequency-select").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-leave-button").performClick()
        assertEquals("aa".repeat(32), leftGroupId)
    }

    /**
     * The android half of the multi-writer writer-binding fan-out, pinned as a
     * DECLARED ABSENCE rather than left as an untested gap (folders.md § Sharing,
     * ruled 2026-08-15): a `writer`-access member row renders EXACTLY what a
     * reader's does, because the only affordance the grant buys is location
     * binding and android has none — `folder-location-*` is desktop
     * `platform_elements`. The desktop/tui twin asserting the opposite (the
     * binding form DOES appear for a writer) is
     * `tests/e2e-unified/tests/test_folders.py::test_writer_member_binds_location`.
     *
     * Guards the shape a future session is most likely to reach for: quietly
     * wiring `WatchedDirectoryManager`'s SAF trees onto a writer row, which would
     * promise read-write sync android's manual one-shot scan cannot honour.
     */
    @Test
    fun memberFolderRowRendersNoBindingAffordanceEvenForAWriter() {
        render(
            snapshot(
                folders = listOf(
                    folder(
                        "team-notes",
                        mlsGroupId = "aa".repeat(32),
                        role = "member",
                        access = "writer",
                        ownerDisplay = "bob@fauna.social",
                    ),
                ),
            ),
        )
        // Non-vacuity: the writer's row really rendered, and is still a member row.
        composeTestRule.onNodeWithTag("folder-row").assertExists()
        composeTestRule.onNodeWithTag("folder-shared-badge").assertTextEquals("Shared by bob@fauna.social")
        composeTestRule.onNodeWithTag("folder-leave-button").assertExists()
        // The whole desktop binding family stays absent on a writer row.
        composeTestRule.onNodeWithTag("folder-location-list").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-location-add-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-location-path-input").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-location-row").assertDoesNotExist()
        // And the row still grows no owner affordances from the grant.
        composeTestRule.onNodeWithTag("folder-share-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-delete-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("folder-frequency-select").assertDoesNotExist()
    }

    @Test
    fun ownerRowNeverRendersALeaveButton() {
        render(snapshot(folders = listOf(folder("photos", role = "owner"))))
        composeTestRule.onNodeWithTag("folder-leave-button").assertDoesNotExist()
    }
    // ── W4 (account-data-plane.md § Workstreams) phase 4's offline gate on this page (batch 10) ───────────────────
    //
    // The gate suite proper lives in `ui/util/OfflineGateTest`; these four cases
    // live HERE because the fixtures they need (`snapshot`, `folder`,
    // `destinationPlace`) already do, and duplicating a thirty-field folder
    // fixture into the gate file would create exactly the maintenance twin this
    // project keeps eliminating. They are part of the same red-verify: run both
    // classes together and grade the split across them.
    //
    // `LocalConnectionState` is provided directly rather than through the gate
    // file's own `render` helper, for the same reason — one composition local,
    // no reach into another test class's private harness.

    private fun renderOffline(
        state: FfiConnectionState?,
        snapshot: DevicesSnapshot? = snapshot(folders = listOf(folder("photos"))),
        folderDestinationPlaces: Map<String, List<com.fauna.ffi.FfiFolderDestinationPlace>> = emptyMap(),
        canServeWebdav: Boolean = true,
    ) {
        composeTestRule.setContent {
            CompositionLocalProvider(LocalConnectionState provides state) {
                FoldersContent(
                    snapshot,
                    FoldersActions(),
                    WizardActions(),
                    folderActors = emptyMap(),
                    folderDeviceActivity = emptyMap(),
                    folderDestinationPlaces = folderDestinationPlaces,
                    canServeWebdav = canServeWebdav,
                    ownTiers = listOf("gold"),
                    nestSnapshotsOptions = nestSnapshotOptions,
                )
            }
        }
    }

    @Test
    fun theWebdavServeToggleGatesWithNoNest() {
        // ⚠ Graded from the ORACLE's gesture, not the Rust issuer's name: the
        // issuer is `reconcile_webdav_keys_blob`, which reads like a background
        // job, but tui's gesture is `ToggleFolderWebdav` and this is that toggle.
        // Serving seals the blob, so the toggle IS the commit.
        renderOffline(FfiConnectionState.DISCONNECTED)
        composeTestRule.onNodeWithTag("folder-webdav-toggle")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theWebdavServeToggleStaysLiveWhenConnected() {
        renderOffline(FfiConnectionState.CONNECTED)
        composeTestRule.onNodeWithTag("folder-webdav-toggle")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theWebdavServeToggleStaysDeadWithoutAnMsekEvenConnected() {
        // Converse — must SURVIVE the unplug. `canServeWebdav` is the page's own
        // predicate (no MSEK ⇒ `serve_set` would flip the nest flag and only
        // then fail `NoMsek`), handed INTO the gate rather than re-tested beside
        // it. A gate that replaced it would offer a click that cannot succeed.
        renderOffline(FfiConnectionState.CONNECTED, canServeWebdav = false)
        composeTestRule.onNodeWithTag("folder-webdav-toggle")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theDestinationAttachAndDetachGate_whileThePickerStaysLive() {
        renderOffline(
            FfiConnectionState.DISCONNECTED,
            folderDestinationPlaces = mapOf(
                "photos" to listOf(
                    destinationPlace("dest-1", "Offsite", attached = true, folderSet = "__folder/aa/1"),
                    destinationPlace("dest-2", "Backup Box", attached = false),
                ),
            ),
        )
        composeTestRule.onNodeWithText("Selective Sync").performClick()
        composeTestRule.onNodeWithTag("folder-destination-attach-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("folder-destination-detach-button")
            .performScrollTo().assertIsNotEnabled()
        // The picker is a pure buffer — the live sibling beside the two dead
        // commits, so a blanket grey cannot pass this case.
        composeTestRule.onNodeWithTag("folder-destination-attach-select")
            .performScrollTo().assertIsEnabled()
    }
}
