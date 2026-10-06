package com.fauna.app.ui.screen.backups

import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.ffi.FfiBackupDestinationStatus
import com.fauna.ffi.FfiBackupDestinationView
import com.fauna.ffi.FfiDestinationAuditRow
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [BackupDestinationsContent] (the
 * backups-page destination management section, backups.md § Manage backup
 * destinations): the always-present add button, the add/edit form reveal, the
 * indexed status rows with their per-row controls, and the remove-confirm reveal.
 * Renders with seeded state — no Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class BackupDestinationsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun dest(
        id: String = "id-a",
        url: String = "https://backup.example.com",
        name: String? = "Aunt's nest",
        // The custodian columns the read projection grew with the client-device
        // destination kind (backups.md § Third destination kind). A nest row —
        // which is what every case here seeds — carries the serde default and
        // neither optional column.
        kind: String = "nest",
        custodianDeviceId: String? = null,
        capacityCapBytes: ULong? = null,
    ) = FfiBackupDestinationView(
        destinationId = id,
        destinationNestUrl = url,
        displayName = name,
        kind = kind,
        custodianDeviceId = custodianDeviceId,
        capacityCapBytes = capacityCapBytes,
        unattested = false,
    )

    private fun status(
        id: String = "id-a",
        lastUpload: ULong? = null,
        backlog: UInt = 0u,
        // The two custodian columns the status projection grew with the
        // client-device kind. `heldBytes = null` is "never checked in" (which
        // reads as *nothing held yet*, not *0 held*); `capState` is the nest's
        // own verdict, never inferred from the numbers.
        heldBytes: ULong? = null,
        capState: String? = null,
        // The client-device self-audit pair the projection grew for the audit
        // surface's client-device arm (backups.md § Audit-alert surface).
        // `auditState = null` is "not yet audited" — never a failure and
        // never a pass.
        auditState: String? = null,
        lastAuditPassedAt: ULong? = null,
    ) = FfiBackupDestinationStatus(
        destinationId = id,
        lastUploadTime = lastUpload,
        backlogCount = backlog,
        heldBytes = heldBytes,
        capState = capState,
        auditState = auditState,
        lastAuditPassedAt = lastAuditPassedAt,
    )

    /** A client-device row — the third destination kind, on this very device. */
    private fun custodian(
        id: String = "id-mine",
        name: String? = "This phone",
        cap: ULong? = null,
    ) = dest(
        id = id,
        url = "",
        name = name,
        kind = CLIENT_DEVICE,
        custodianDeviceId = "dev-a",
        capacityCapBytes = cap,
    )

    private fun auditRow(
        id: String = "id-a",
        lastPassedAt: ULong? = null,
    ) = FfiDestinationAuditRow(
        destinationId = id,
        lastPassedAt = lastPassedAt,
        alertReason = null,
        alertReasons = emptyList(),
    )

    private fun render(
        destinations: List<FfiBackupDestinationView> = emptyList(),
        statuses: Map<String, FfiBackupDestinationStatus> = emptyMap(),
        working: Boolean = false,
        onAdd: (String, String) -> Unit = { _, _ -> },
        onEdit: (String, String, String) -> Unit = { _, _, _ -> },
        onRemove: (String, Boolean) -> Unit = { _, _ -> },
        // FFI-free stand-ins for the shared backupLastUploadLabel/backupBacklogLabel
        // resolve the Screen normally injects (BackupDestinationsSection.kt) — mirror
        // their never-vs-real / absent-baseline shape without touching FFI.
        lastUploadText: (FfiBackupDestinationStatus?) -> String =
            { status -> if (status?.lastUploadTime == null) "never" else status.lastUploadTime.toString() },
        backlogText: (FfiBackupDestinationStatus?) -> String =
            { status -> "${status?.backlogCount ?: 0u} queued" },
        auditRows: Map<String, FfiDestinationAuditRow> = emptyMap(),
        // FFI-free stand-in for the shared backupLastAuditLabel resolve, the audit
        // twin of lastUploadText above.
        lastAuditText: (FfiDestinationAuditRow?) -> String =
            { row -> if (row?.lastPassedAt == null) "never" else row.lastPassedAt.toString() },
        // FFI-free stand-in for the shared backupSelfAuditLabel resolve — the
        // client-device twin of lastAuditText, off the status row's own
        // audit_state/last_audit_passed_at rather than an audit row.
        selfAuditText: (FfiBackupDestinationStatus?) -> String =
            { status -> if (status?.lastAuditPassedAt == null) "self-not-yet" else "self-${status.lastAuditPassedAt}" },
        // ── The client-device kind's seams ──
        // Each is an FFI-free stand-in for what BackupDestinationsSection wires
        // from shared Rust — the harness cannot load the native library. They
        // mirror the shared shape; they are never a second implementation of the
        // rule, which is exactly why the kind-swap and cap-refusal assertions
        // below drive the PRODUCTION composable rather than these.
        onEnrollCustodian: (String, ULong?) -> Unit = { _, _ -> },
        kindOptions: List<DestinationKindOption> = KIND_OPTIONS,
        kindBadgeText: (FfiBackupDestinationView) -> String =
            { d -> if (d.kind == CLIENT_DEVICE) "This device" else "Another nest" },
        usageText: (FfiBackupDestinationView, FfiBackupDestinationStatus?) -> String =
            { _, s -> if (s?.capState == "reached") "full" else "${s?.heldBytes ?: 0u} held" },
        isClientDevice: (FfiBackupDestinationView) -> Boolean = { it.kind == CLIENT_DEVICE },
        soleClientDestinations: Boolean = false,
        // The shared parse_byte_size's contract: a readable size, else null —
        // and null is a refusal, never a default.
        parseCapacity: (String) -> ULong? = { typed -> typed.removeSuffix(" GB").toULongOrNull() },
        onError: (String) -> Unit = {},
        // ── Reclaim this device's copy ──
        // A VERDICT the caller has already reached with the shared
        // `custodian_store_is_orphaned` — never a byte count this composable is
        // meant to interpret. `null` is "do not paint the row".
        orphanedStoreBytes: ULong? = null,
        orphanedStoreText: (ULong) -> String = { held -> "holding $held" },
        onReclaimStore: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            // Scrollable, because production mounts this section as a header item
            // inside the snapshot-list LazyColumn — a bare Column here would put
            // the form's buttons permanently below Robolectric's viewport, where
            // performClick() silently no-ops and assertIsDisplayed() fails. The
            // kind select made the add form tall enough for that to start biting.
            // Same idiom as ProfileOffersContentTest.
            Column(Modifier.verticalScroll(rememberScrollState())) {
            BackupDestinationsContent(
                destinations = destinations,
                statuses = statuses,
                working = working,
                onAdd = onAdd,
                onEdit = onEdit,
                onRemove = onRemove,
                lastUploadText = lastUploadText,
                backlogText = backlogText,
                auditRows = auditRows,
                lastAuditText = lastAuditText,
                selfAuditText = selfAuditText,
                onEnrollCustodian = onEnrollCustodian,
                kindOptions = kindOptions,
                kindBadgeText = kindBadgeText,
                usageText = usageText,
                isClientDevice = isClientDevice,
                soleClientDestinations = soleClientDestinations,
                parseCapacity = parseCapacity,
                onError = onError,
                orphanedStoreBytes = orphanedStoreBytes,
                orphanedStoreText = orphanedStoreText,
                onReclaimStore = onReclaimStore,
            )
            }
        }
    }

    /**
     * Scroll a node into Robolectric's viewport before touching it. Everything
     * inside the add form now sits far enough down that an un-scrolled
     * `performClick()` no-ops and the test reads as a product bug.
     */
    private fun node(tag: String) =
        composeTestRule.onNodeWithTag(tag).performScrollTo()

    /** Switch `backup-destination-kind-select` to a kind (the form must be open). */
    private fun selectKind(label: String) {
        node("backup-destination-kind-select").performClick()
        composeTestRule.onNodeWithText(label).performClick()
    }

    @Test
    fun addButton_alwaysPresent_evenWhenEmpty() {
        render(destinations = emptyList())
        composeTestRule.onNodeWithTag("backup-destination-add-button").assertIsDisplayed()
        // No status rows when no destinations are configured.
        composeTestRule.onAllNodesWithTag("backup-destination-status-row").assertCountEquals(0)
    }

    @Test
    fun destinations_renderIndexedStatusRows() {
        render(destinations = listOf(dest(id = "id-a"), dest(id = "id-b", name = null)))
        composeTestRule.onAllNodesWithTag("backup-destination-status-row").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("backup-destination-last-upload-time").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("backup-destination-backlog-count").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("backup-destination-last-audit-time").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("backup-destination-edit-button").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("backup-destination-remove-button").assertCountEquals(2)
    }

    @Test
    fun statusRow_rendersAuditBaseline_whenNeverAudited() {
        // No audit-row entry for the destination (never audited this process) ⇒
        // "never", the same honest-fresh-enrollment baseline as a brand-new
        // destination (backups.md § Audit-alert surface).
        render(destinations = listOf(dest(id = "id-a")))
        composeTestRule.onNodeWithTag("backup-destination-last-audit-time")
            .assertTextContains("never", substring = true)
    }

    @Test
    fun statusRow_rendersAuditPass_byDestinationId() {
        // A destination that has passed maps its audit row onto the same row by
        // destination_id, distinct from statusRow_rendersLiveUploadAndBacklog_byDestinationId
        // above (last-upload is the SOURCE NEST's report; last-audit is this
        // client's own independent check).
        render(
            destinations = listOf(dest(id = "id-a")),
            auditRows = mapOf("id-a" to auditRow(id = "id-a", lastPassedAt = 1_700_000_000uL)),
            lastAuditText = { "2 hours ago" },
        )
        composeTestRule.onNodeWithTag("backup-destination-last-audit-time")
            .assertTextContains("2 hours ago", substring = true)
    }

    @Test
    fun statusRow_rendersBaseline_whenNoLiveStatus() {
        // No status entry for the destination ⇒ the not-yet-backed-up baseline
        // ("never" / "0 queued"), uniform with linux before the first read.
        render(destinations = listOf(dest(id = "id-a")))
        composeTestRule.onNodeWithTag("backup-destination-last-upload-time")
            .assertTextContains("never", substring = true)
        composeTestRule.onNodeWithTag("backup-destination-backlog-count")
            .assertTextContains("0", substring = true)
    }

    @Test
    fun statusRow_rendersLiveUploadAndBacklog_byDestinationId() {
        // A live status with a timestamp + backlog maps onto the row by
        // destination_id: last-upload through the injected relative-time formatter
        // (FFI off the test), backlog as "N queued".
        render(
            destinations = listOf(dest(id = "id-a")),
            statuses = mapOf("id-a" to status(id = "id-a", lastUpload = 1_700_000_000uL, backlog = 5u)),
            lastUploadText = { "5 minutes ago" },
        )
        composeTestRule.onNodeWithTag("backup-destination-last-upload-time")
            .assertTextContains("5 minutes ago", substring = true)
        composeTestRule.onNodeWithTag("backup-destination-backlog-count")
            .assertTextContains("5 queued", substring = true)
    }

    @Test
    fun addButton_opensForm() {
        render()
        node("backup-destination-add-button").performClick()
        node("backup-destination-add-modal").assertIsDisplayed()
        node("backup-destination-url-input").assertIsDisplayed()
        node("backup-destination-name-input").assertIsDisplayed()
        node("backup-destination-add-cancel-button").assertIsDisplayed()
    }

    @Test
    fun addForm_confirm_firesOnAddWithTypedUrl() {
        var added: Pair<String, String>? = null
        render(onAdd = { url, name -> added = url to name })
        node("backup-destination-add-button").performClick()
        node("backup-destination-url-input")
            .performTextInput("https://new.example.com")
        node("backup-destination-name-input")
            .performTextInput("New box")
        node("backup-destination-add-confirm-button").performClick()
        assertEquals("https://new.example.com" to "New box", added)
    }

    @Test
    fun editButton_opensFormPrefilledWithUrl() {
        render(destinations = listOf(dest(id = "id-a", url = "https://backup.example.com")))
        node("backup-destination-edit-button").performClick()
        node("backup-destination-add-modal").assertIsDisplayed()
        node("backup-destination-url-input")
            .assert(hasText("https://backup.example.com"))
    }

    @Test
    fun removeButton_opensConfirm_andFiresOnRemove() {
        var removed: String? = null
        render(destinations = listOf(dest(id = "id-a")), onRemove = { id, _ -> removed = id })
        node("backup-destination-remove-button").performClick()
        node("backup-destination-remove-confirm-modal").assertIsDisplayed()
        node("backup-destination-remove-confirm-button").performClick()
        assertEquals("id-a", removed)
    }

    // ── The client-device destination kind (backups.md § Third destination kind) ──

    @Test
    fun statusRow_paintsKindBadge_onEveryRow_andUsageOnClientDeviceRowsOnly() {
        // The badge is per-row and unconditional — it is the visible half of "a
        // client custodian never silently satisfies *you have an off-site
        // backup*", so a nest row must say so too, not just be unlabelled.
        // Usage is client-device rows ONLY (ui.yaml): a nest row has no cap and
        // no held-bytes report, so the element is ABSENT rather than empty.
        render(destinations = listOf(dest(id = "id-a"), custodian(id = "id-mine")))
        composeTestRule.onAllNodesWithTag("backup-destination-kind-badge").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("backup-destination-usage").assertCountEquals(1)
    }

    // ── the client-device arm of the audit surface ────────────────────────
    //
    // A custodian has no address, so the owner-side loop can never sample it:
    // its audit answer arrives on the STATUS ROW as the device's own verdict
    // (backups.md § Audit-alert surface → *The client-device arm*).

    @Test
    fun statusRow_dispatchesToSelfAuditText_onAClientDeviceRow() {
        // The nest row keeps lastAuditText; the custodian row switches to
        // selfAuditText — a dispatch by kind, never a re-derived guess.
        render(
            destinations = listOf(dest(id = "id-a"), custodian(id = "id-mine")),
            statuses = mapOf("id-mine" to status(id = "id-mine", lastAuditPassedAt = 1_700_000_000uL)),
            lastAuditText = { "owner-side" },
            selfAuditText = { status -> if (status?.lastAuditPassedAt == null) "self-not-yet" else "self-checked" },
        )
        val cells = composeTestRule.onAllNodesWithTag("backup-destination-last-audit-time")
        cells.assertCountEquals(2)
        cells[0].assertTextContains("owner-side", substring = true)
        cells[1].assertTextContains("self-checked", substring = true)
    }

    @Test
    fun statusRow_selfAuditReadsAbsenceAsNotYet_whenTheCustodianHasNotCheckedIn() {
        // A custodian that has never self-audited must render ABSENCE, never
        // a verdict — reading silence as a pass would render an unverified
        // copy as verified.
        render(
            destinations = listOf(custodian(id = "id-mine")),
            statuses = mapOf("id-mine" to status(id = "id-mine", auditState = null)),
        )
        composeTestRule.onNodeWithTag("backup-destination-last-audit-time")
            .assertTextContains("self-not-yet", substring = true)
    }

    @Test
    fun usageRow_readsCapReachedFromTheStatus_notFromTheNumbers() {
        // The load-bearing property of the whole usage render, mirrored from the
        // shared label's own mutation-verified rule: a pull pass that STOPS at
        // its cap ends *below* the cap, so a row whose held bytes sit under the
        // cap can still be cap-reached. Anything re-deriving the verdict from
        // `held >= cap` renders a silently-stopped backup as healthy.
        render(
            destinations = listOf(custodian(id = "id-mine", cap = 50uL)),
            statuses = mapOf(
                "id-mine" to status(id = "id-mine", heldBytes = 40uL, capState = "reached"),
            ),
        )
        node("backup-destination-usage")
            .assertTextContains("full", substring = true)
    }

    // The warning is not derived in the render: the predicate is shared, because
    // both its arms are policy answers (empty list ⇒ NOT sole-client; an
    // unrecognised kind counts as NOT a client device). These two pin that the
    // render obeys it in both directions rather than re-deciding — split in two
    // because the Compose rule allows only one setContent per test.

    @Test
    fun soleClientWarning_paintsWhenThePredicateHolds() {
        render(destinations = listOf(custodian()), soleClientDestinations = true)
        node("backup-sole-client-destination-warning").assertIsDisplayed()
    }

    @Test
    fun soleClientWarning_isAbsentWhenThePredicateDoesNot() {
        render(destinations = listOf(custodian(), dest(id = "id-a")), soleClientDestinations = false)
        composeTestRule.onAllNodesWithTag("backup-sole-client-destination-warning")
            .assertCountEquals(0)
    }

    @Test
    fun kindSelect_swapsUrlForCapacity_becauseACustodianHasNoAddress() {
        // The ratified shape is a SWAP, not a disabled-but-present URL box: a
        // custodian has no address at all, so painting an empty URL field would
        // invite the user to type one nothing could ever use.
        render()
        node("backup-destination-add-button").performClick()
        node("backup-destination-url-input").assertIsDisplayed()
        composeTestRule.onAllNodesWithTag("backup-destination-capacity-input").assertCountEquals(0)

        selectKind("This device")
        composeTestRule.onAllNodesWithTag("backup-destination-url-input").assertCountEquals(0)
        node("backup-destination-capacity-input").assertIsDisplayed()
    }

    @Test
    fun kindSelect_isDisabledWhileEditing_becauseTheKindIsNotAnEditableProperty() {
        // Painted disabled rather than hidden (backups.md § Create / edit /
        // remove): re-pointing a live row would keep a `destination_id` whose
        // registry row and grants describe the other kind.
        render(destinations = listOf(dest(id = "id-a")))
        node("backup-destination-edit-button").performClick()
        node("backup-destination-kind-select").assertIsNotEnabled()
    }

    @Test
    fun custodianConfirm_withBlankCapacity_enrollsUncapped() {
        // A blank cap is a REAL choice — `null` is uncapped — not a missing
        // value to substitute a default for.
        var enrolled: Pair<String, ULong?>? = null
        var enrollCalls = 0
        render(onEnrollCustodian = { name, cap -> enrolled = name to cap; enrollCalls++ })
        node("backup-destination-add-button").performClick()
        selectKind("This device")
        node("backup-destination-name-input").performTextInput("My phone")
        node("backup-destination-add-confirm-button").performClick()
        assertEquals("My phone" to null, enrolled)
        assertEquals(1, enrollCalls)
    }

    @Test
    fun custodianConfirm_withUnreadableCapacity_refuses_ratherThanGuessingOne() {
        // An unreadable cap must enroll NOTHING and report to this page's ONE
        // error surface (the shell banner — the form deliberately tags no second
        // `error-message` node of its own). Substituting a default here is how a
        // device's disk fills.
        var enrollCalls = 0
        var errors = 0
        render(onEnrollCustodian = { _, _ -> enrollCalls++ }, onError = { errors++ })
        node("backup-destination-add-button").performClick()
        selectKind("This device")
        node("backup-destination-capacity-input")
            .performTextInput("as much as you like")
        node("backup-destination-add-confirm-button").performClick()
        assertEquals(0, enrollCalls)
        assertEquals(1, errors)
    }

    @Test
    fun custodianConfirm_isEnabledWithNoUrl_becauseTheKindHasNone() {
        // The nest kind's non-blank-URL gate must not apply to a form that never
        // shows a URL box, or the confirm button is disabled forever.
        var enrollCalls = 0
        render(onEnrollCustodian = { _, _ -> enrollCalls++ })
        node("backup-destination-add-button").performClick()
        node("backup-destination-add-confirm-button").assertIsNotEnabled()
        selectKind("This device")
        node("backup-destination-add-confirm-button").assertIsEnabled()
        node("backup-destination-capacity-input").performTextInput("50 GB")
        node("backup-destination-add-confirm-button").performClick()
        assertEquals(1, enrollCalls)
    }

    // ── Reclaim this device's copy (backups.md § Manage backup destinations →
    // *Reclaim this device's copy*; ids user-approved 2026-08-13) ──

    /**
     * The row paints on the caller's verdict and on nothing else.
     *
     * The absent case is the load-bearing half: this row carries a button that
     * destroys the owner's only offline copy, so a build that painted it
     * whenever bytes happened to be on disk would offer that deletion to a
     * device whose destination rows are still sitting on the same page.
     */
    @Test
    fun orphanedStoreRow_paintsOnlyOnTheVerdict() {
        render(orphanedStoreBytes = null)
        composeTestRule.onAllNodesWithTag("backup-orphaned-store-row").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("backup-destination-reclaim-button").assertCountEquals(0)
    }

    @Test
    fun orphanedStoreRow_carriesTheSharedSentenceAndTheReclaimButton() {
        render(orphanedStoreBytes = 4096uL, orphanedStoreText = { held -> "holding $held bytes" })
        node("backup-orphaned-store-row").assertIsDisplayed()
        composeTestRule.onNodeWithText("holding 4096 bytes").assertIsDisplayed()
        node("backup-destination-reclaim-button").assertIsDisplayed()
    }

    /**
     * Reclaim goes through a confirm, and the confirm is PLAIN — no re-type
     * friction bar, unlike the snapshot-restore confirm on this same page. The
     * store is re-buildable from a fresh pull on re-enrollment, so a typed
     * confirmation would overstate what is lost; the modal exists because
     * reclaiming ends this device's standalone restore.
     */
    @Test
    fun reclaimButton_opensAPlainConfirm_andFiresOnConfirm() {
        var reclaimed = 0
        render(orphanedStoreBytes = 4096uL, onReclaimStore = { reclaimed++ })

        node("backup-destination-reclaim-button").performClick()
        node("backup-reclaim-confirm-modal").assertIsDisplayed()
        composeTestRule.onAllNodesWithTag("restore-confirm-input").assertCountEquals(0)
        assertEquals(0, reclaimed)

        node("backup-reclaim-confirm-button").performClick()
        assertEquals(1, reclaimed)
    }

    /** Cancelling frees nothing and closes the modal. */
    @Test
    fun reclaimConfirm_cancelFreesNothing() {
        var reclaimed = 0
        render(orphanedStoreBytes = 4096uL, onReclaimStore = { reclaimed++ })
        node("backup-destination-reclaim-button").performClick()
        node("backup-reclaim-cancel-button").performClick()
        assertEquals(0, reclaimed)
        composeTestRule.onAllNodesWithTag("backup-reclaim-confirm-modal").assertCountEquals(0)
    }

    /**
     * The remove dialog's opt-in is offered on client-device rows ONLY.
     *
     * A nest destination holds its copy somewhere else entirely, so "also delete
     * this device's copy" is meaningless there — and a checkbox that appeared
     * anyway would invite a user to tick it and then silently do nothing.
     */
    @Test
    fun removeReclaimCheckbox_isOfferedOnClientDeviceRowsOnly() {
        render(destinations = listOf(dest(id = "id-a", kind = "nest")))
        node("backup-destination-remove-button").performClick()
        node("backup-destination-remove-confirm-modal").assertIsDisplayed()
        composeTestRule.onAllNodesWithTag("backup-destination-remove-reclaim-checkbox")
            .assertCountEquals(0)
    }

    /**
     * The opt-in defaults to OFF, and only a tick sends it.
     *
     * This is the whole reason the affordance exists: § 3c-ii deliberately KEEPS
     * the local sealed store across a removal, because it is the owner's only
     * offline copy. A pre-ticked box would quietly invert that ratified default
     * for every user who does not read the line.
     */
    @Test
    fun removeReclaimCheckbox_startsUntickedAndOnlyATickOptsIn() {
        var removed: Pair<String, Boolean>? = null
        render(
            destinations = listOf(dest(id = "id-a", kind = CLIENT_DEVICE)),
            onRemove = { id, reclaim -> removed = id to reclaim },
        )
        node("backup-destination-remove-button").performClick()
        node("backup-destination-remove-reclaim-checkbox").assertIsDisplayed()
        node("backup-destination-remove-confirm-button").performClick()
        assertEquals("id-a" to false, removed)
    }

    @Test
    fun removeReclaimCheckbox_tickedCarriesTheOptIn() {
        var removed: Pair<String, Boolean>? = null
        render(
            destinations = listOf(dest(id = "id-a", kind = CLIENT_DEVICE)),
            onRemove = { id, reclaim -> removed = id to reclaim },
        )
        node("backup-destination-remove-button").performClick()
        node("backup-destination-remove-reclaim-checkbox").performClick()
        node("backup-destination-remove-confirm-button").performClick()
        assertEquals("id-a" to true, removed)
    }

    private companion object {
        const val CLIENT_DEVICE = "client-device"

        /**
         * The shared catalog's shape, in its paint order (nest first — it is the
         * kind that actually satisfies "off-site"). `isClientDevice` is resolved
         * in production by comparing against the exported
         * `destinationKindClientDevice()`; seeding it directly here is what keeps
         * the harness free of FFI.
         */
        val KIND_OPTIONS = listOf(
            DestinationKindOption("nest", "Another nest", isClientDevice = false),
            DestinationKindOption(CLIENT_DEVICE, "This device", isClientDevice = true),
        )
    }
}
