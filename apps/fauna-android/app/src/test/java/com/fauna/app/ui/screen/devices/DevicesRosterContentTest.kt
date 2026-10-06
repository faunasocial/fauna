package com.fauna.app.ui.screen.devices

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.ui.currentClipboardText
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_capabilities.CustodyHolderRowView
import uniffi.fauna_client_capabilities.CustodyReceiptRowView
import uniffi.fauna_client_capabilities.CustodyReceiptStateView
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_devices_machine.DeviceFolderRole
import uniffi.fauna_devices_machine.DeviceSummary
import uniffi.fauna_devices_machine.DevicesSnapshot

/**
 * Compose-level coverage for the stateless [DevicesRosterContent] (Settings →
 * Devices, `docs/goal/ui/devices.md`) — the roster slice of the shared
 * [DevicesSnapshot] after the 2026-06-28 sync/folder UI unification. Renders with
 * a seeded snapshot — no Hilt, no VM, no FFI native calls — so the roster's
 * canonical ui.yaml test IDs and the remove gesture are exercised on the JVM.
 * (The folder / conflict / wizard slices moved to FoldersContentTest.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class DevicesRosterContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    // guardianMarked defaults false — family-safety.md § Full visibility for
    // young children (Slice F); none of this file's existing coverage
    // exercises the guardian-mark badge.
    private fun device(
        label: String,
        online: Boolean = true,
        guardianMarked: Boolean = false,
        deviceId: String = "ab".repeat(32),
        folders: List<DeviceFolderRole> = emptyList(),
    ) = DeviceSummary(
        deviceId = deviceId,
        label = label,
        capabilities = "read,write",
        registeredAt = 0L,
        lastSeenAt = 0L,
        online = online,
        guardianMarked = guardianMarked,
        folders = folders,
        principal = null,
        // Sibling field-adds (the device's p2p participation): unknown, and no
        // pending off-request.
        p2pParticipation = null,
        p2pOffRequested = false,
    )

    private fun snapshot(
        devices: List<DeviceSummary> = listOf(device("Phone")),
        error: LocalizedText? = null,
    ) = DevicesSnapshot(
        devices = devices,
        folders = emptyList(),
        followed = emptyList(),
        // `None` = unknown, the documented hedge on the website toggle's
        // tri-state hint — a sibling field-add this fixture had not caught up
        // with (android is not CI-gated, so it landed red).
        websiteAddressEnabled = null,
        conflicts = emptyList(),
        wizard = null,
        error = error,
        // Sibling field-adds (the fleet-removal door): no unaccounted members,
        // and no own fleet id, so no own fingerprint either.
        members = emptyList(),
        ownFleetId = null,
        ownFingerprint = null,
        // Sibling field-add: this device's own p2p participation, unknown.
        ownP2pParticipation = null,
    )

    // ── T16 custody facet fixtures (devices.md § Custody facet, piece 2) ──
    //
    // The boundary rows come off `fauna_client_capabilities::custody_view`, which
    // already folded both shared label decisions in — so these fixtures carry the
    // labels as data and the render tests below never re-derive which receipt
    // state reads which way.

    private fun receipt(
        statusKey: String = "devices.custody_receipt_fresh",
        attestedAtSecs: Long? = 1_700_000_000L,
        degraded: Boolean = false,
    ) = CustodyReceiptRowView(
        statusLabel = LocalizedText(statusKey, emptyMap()),
        attestedAtSecs = attestedAtSecs,
        heldBytesLabel = LocalizedText("devices.custody_held_bytes", emptyMap()),
        held = LocalizedText("", emptyMap()),
        cap = LocalizedText("", emptyMap()),
        degraded = degraded,
        heldBytes = 1024uL,
        attestedCap = 4096uL,
    )

    private fun custody(
        grantId: ByteArray = ByteArray(16) { 1 },
        host: ByteArray = ByteArray(32) { 2 },
        custodianKey: ByteArray? = ByteArray(32) { 3 },
        custodianNestUrl: String? = null,
        pending: Boolean = false,
        receiptState: CustodyReceiptStateView = CustodyReceiptStateView.FRESH,
        receipt: CustodyReceiptRowView = receipt(),
    ) = CustodyHolderRowView(
        grantId = grantId,
        host = host,
        custodianKey = custodianKey,
        custodianNestUrl = custodianNestUrl,
        scopes = null,
        lastsUntil = null,
        liveness = null,
        receiptState = receiptState,
        receipt = receipt,
        pending = pending,
    )

    private fun render(
        snapshot: DevicesSnapshot? = snapshot(),
        localDeviceId: String? = null,
        actorId: String? = "cd".repeat(32),
        onRemoveDevice: (Int) -> Unit = {},
        custodyRows: List<CustodyHolderRowView> = emptyList(),
        onRevokeCustody: (ByteArray, ByteArray?) -> Unit = { _, _ -> },
        receiptStatusLine: (CustodyReceiptRowView) -> String = { "STATUS:${it.statusLabel.key}" },
        heldBytesLine: (CustodyReceiptRowView) -> String = { "HELD:${it.heldBytes}" },
    ) {
        composeTestRule.setContent {
            DevicesRosterContent(
                snapshot = snapshot,
                localDeviceId = localDeviceId,
                actorId = actorId,
                onRemoveDevice = onRemoveDevice,
                statusLabel = { online -> if (online) "Online" else "Offline" },
                placeLabel = { o, a, d -> "PLACE:$o/$a/$d" },
                custodyRows = custodyRows,
                onRevokeCustody = onRevokeCustody,
                receiptStatusLine = receiptStatusLine,
                heldBytesLine = heldBytesLine,
            )
        }
    }

    @Test
    fun deviceCardRendersWithCanonicalIds() {
        render(snapshot(devices = listOf(device("Phone"), device("Laptop"))))
        assertEquals(2, composeTestRule.onAllNodesWithTag("device-card").fetchSemanticsNodes().size)
        composeTestRule.onAllNodesWithTag("device-name")[0].assertExists()
        composeTestRule.onAllNodesWithTag("device-status")[0].assertExists()
        composeTestRule.onAllNodesWithTag("peer-actor-id-copy-btn")[0].assertExists()
        composeTestRule.onAllNodesWithTag("device-remove-button")[0].assertExists()
    }

    // `peer-actor-id-copy-btn` is page-level (ui.yaml `indexed: false`,
    // `devices.md` § Layout & flow point 2) — exactly one instance regardless
    // of roster size.
    @Test
    fun actorIdCopyButtonIsSingleInstanceRegardlessOfRosterSize() {
        render(snapshot(devices = listOf(device("Phone"), device("Laptop"), device("Tablet"))))
        assertEquals(
            1,
            composeTestRule.onAllNodesWithTag("peer-actor-id-copy-btn").fetchSemanticsNodes().size,
        )
    }

    // Rendered even with an empty roster — pairing the FIRST device is exactly
    // when copying this client's own actor ID is needed.
    @Test
    fun actorIdCopyButtonRendersWithAnEmptyRoster() {
        render(snapshot(devices = emptyList()))
        composeTestRule.onNodeWithTag("peer-actor-id-copy-btn").assertExists()
    }

    @Test
    fun guardianMarkBadgeRendersOnlyOnMarkedRows() {
        render(snapshot(devices = listOf(device("Phone", guardianMarked = true), device("Laptop"))))
        assertEquals(1, composeTestRule.onAllNodesWithTag("device-guardian-mark-badge").fetchSemanticsNodes().size)
    }

    @Test
    fun guardianMarkBadgeAbsentWhenUnmarked() {
        render(snapshot(devices = listOf(device("Phone"))))
        composeTestRule.onNodeWithTag("device-guardian-mark-badge").assertDoesNotExist()
    }

    // `device-this-mark-badge` (`devices.md` § This-device marker) — a pure
    // client-side match against the app's own locally-stored device id.
    @Test
    fun thisDeviceBadgeRendersOnlyOnTheMatchingRow() {
        render(
            snapshot(
                devices = listOf(
                    device("Phone", deviceId = "aa".repeat(32)),
                    device("This laptop", deviceId = "bb".repeat(32)),
                ),
            ),
            localDeviceId = "bb".repeat(32),
        )
        assertEquals(1, composeTestRule.onAllNodesWithTag("device-this-mark-badge").fetchSemanticsNodes().size)
    }

    @Test
    fun thisDeviceBadgeAbsentWhenLocalDeviceIdIsNull() {
        render(snapshot(devices = listOf(device("Phone"))), localDeviceId = null)
        composeTestRule.onNodeWithTag("device-this-mark-badge").assertDoesNotExist()
    }

    // The two badges are independent: a guardian marking their own enrolled
    // device legitimately carries both.
    @Test
    fun aDeviceCanCarryBothBadges() {
        render(
            snapshot(devices = listOf(device("Guardian's own device", guardianMarked = true))),
            localDeviceId = "ab".repeat(32),
        )
        composeTestRule.onNodeWithTag("device-guardian-mark-badge").assertExists()
        composeTestRule.onNodeWithTag("device-this-mark-badge").assertExists()
    }

    @Test
    fun removeDeviceConfirmDialogFiresAtIndex() {
        var removed = -1
        render(
            snapshot(devices = listOf(device("Phone"))),
            onRemoveDevice = { removed = it },
        )
        composeTestRule.onNodeWithTag("device-remove-button").performClick()
        // Confirm in the dialog.
        composeTestRule.onAllNodesWithText("Delete")
            .filterToOne(hasAnyAncestor(isDialog())).performClick()
        assertEquals(0, removed)
    }

    // `peer-actor-id-copy-btn` copies the CLIENT'S OWN actor ID, not any
    // listed device's id — the divergence this fix closes (`devices.md` §
    // Layout & flow point 2).
    @Test
    fun actorIdCopyButtonCopiesTheClientsOwnActorIdNotADeviceId() {
        render(actorId = "cd".repeat(32))
        composeTestRule.onNodeWithTag("peer-actor-id-copy-btn").performClick()
        assertEquals("cd".repeat(32), currentClipboardText())
    }

    // `device-folder-role-badge` (`devices.md` § Element table) — one chip per
    // folder the device carries; absent when the device carries none.
    @Test
    fun folderRoleBadgeAbsentWhenDeviceCarriesNoSets() {
        render(snapshot(devices = listOf(device("Phone"))))
        composeTestRule.onNodeWithTag("device-folder-role-badge").assertDoesNotExist()
    }

    @Test
    fun folderRoleBadgeRendersOneChipPerSet() {
        render(
            snapshot(
                devices = listOf(
                    device(
                        "Backup box",
                        folders = listOf(
                            DeviceFolderRole(name = "photos", originates = true, accepts = false, appliesDeletes = false),
                            DeviceFolderRole(name = "docs", originates = true, accepts = true, appliesDeletes = true),
                            DeviceFolderRole(name = "archive", originates = false, accepts = true, appliesDeletes = false),
                        ),
                    ),
                ),
            ),
        )
        assertEquals(
            3,
            composeTestRule.onAllNodesWithTag("device-folder-role-badge").fetchSemanticsNodes().size,
        )
    }

    // The chip text must go through the injected `placeLabel`, fed the seat's
    // three flags in order — mirrors `statusLabel`'s own contract.
    @Test
    fun folderRoleBadgeTextResolvesThroughTheInjectedLabel() {
        composeTestRule.setContent {
            DevicesRosterContent(
                snapshot = snapshot(
                    devices = listOf(
                        device(
                            "Phone",
                            folders = listOf(
                                DeviceFolderRole(name = "photos", originates = false, accepts = true, appliesDeletes = true),
                            ),
                        ),
                    ),
                ),
                localDeviceId = null,
                actorId = "cd".repeat(32),
                onRemoveDevice = {},
                statusLabel = { online -> if (online) "Online" else "Offline" },
                placeLabel = { o, a, d -> "PLACE:$o/$a/$d" },
            )
        }
        composeTestRule.onNodeWithTag("device-folder-role-badge").assertTextEquals("PLACE:false/true/true")
    }


    // ── T16 custody facet, owner side (devices.md § Custody facet, piece 2) ──

    // An account with no custodians says nothing here: a titled-but-empty
    // section reads as a feature that failed to load. Same hide-when-empty rule
    // linux's `build_custody_section` applies.
    @Test
    fun custodySectionIsAbsentWithNoCustodians() {
        render(custodyRows = emptyList())
        composeTestRule.onNodeWithTag("custody-holder-card").assertDoesNotExist()
    }

    @Test
    fun custodyCardRendersItsCanonicalFamily() {
        render(custodyRows = listOf(custody()))
        composeTestRule.onNodeWithTag("custody-holder-card").assertExists()
        composeTestRule.onNodeWithTag("custody-holder-name").assertExists()
        composeTestRule.onNodeWithTag("custody-holder-receipt-status").assertExists()
        composeTestRule.onNodeWithTag("custody-holder-held-bytes").assertExists()
        composeTestRule.onNodeWithTag("custody-holder-revoke-button").assertExists()
    }

    // The nest-custodian identity fact (ruled 2026-08-17): a custody whose accept
    // bound the host's NEST belongs to the Nests page's `nest-trust-custody-*`
    // family, and `custodian_nest_url` is the marker that says so. One custody
    // never renders in both places — dropping this filter would show the owner
    // the same custodian twice, on two different pages.
    @Test
    fun nestAnchoredCustodyDoesNotRenderOnTheDevicesPage() {
        render(
            custodyRows = listOf(
                custody(custodianNestUrl = "https://friend-nest.example/"),
                custody(grantId = ByteArray(16) { 9 }),
            ),
        )
        assertEquals(
            1,
            composeTestRule.onAllNodesWithTag("custody-holder-card").fetchSemanticsNodes().size,
        )
    }

    // A custody whose ceremony is still pending has minted nothing to revoke and
    // bound no holder to name, so the control is not offered — and the honest-bound
    // note is withheld with it, because there is no revocation for it to bound yet.
    @Test
    fun pendingCustodyDisablesRevokeAndWithholdsTheBoundNote() {
        render(custodyRows = listOf(custody(pending = true, custodianKey = null)))
        composeTestRule.onNodeWithTag("custody-holder-revoke-button").assertIsNotEnabled()
        composeTestRule.onNodeWithText(
            "Copies already held stay held",
            substring = true,
        ).assertDoesNotExist()
    }

    @Test
    fun settledCustodyOffersRevokeWithTheHonestBoundNote() {
        render(custodyRows = listOf(custody()))
        composeTestRule.onNodeWithTag("custody-holder-revoke-button").assertIsEnabled()
        composeTestRule.onNodeWithText(
            "Copies already held stay held",
            substring = true,
        ).assertExists()
    }

    // The gesture carries the row's GRANT ID and accept-bound custodian key —
    // never a row index. A refold re-orders rows, so an index captured at paint
    // time can address a different custody by the time the act runs: this fires
    // the SECOND card and asserts the second row's own identifiers arrive.
    @Test
    fun revokeCarriesTheRowsGrantIdAndBoundHolder() {
        var grant: ByteArray? = null
        var holder: ByteArray? = null
        render(
            custodyRows = listOf(
                custody(grantId = ByteArray(16) { 1 }, custodianKey = ByteArray(32) { 7 }),
                custody(grantId = ByteArray(16) { 2 }, custodianKey = ByteArray(32) { 8 }),
            ),
            onRevokeCustody = { g, h -> grant = g; holder = h },
        )
        composeTestRule.onAllNodesWithTag("custody-holder-revoke-button")[1]
            .performScrollTo().performClick()
        assertEquals(2, grant!![0].toInt())
        assertEquals(8, holder!![0].toInt())
    }

    // Both lines come from the injected resolvers, which resolve the SHARED label
    // decisions (`custody_receipt_status_display` / `custody_held_bytes_display`)
    // already folded into the row — this Content must never assemble either
    // string, or the seven apps drift on which state reads which way.
    @Test
    fun custodyLinesGoThroughTheInjectedResolvers() {
        render(
            custodyRows = listOf(custody()),
            receiptStatusLine = { "RESOLVED-STATUS" },
            heldBytesLine = { "RESOLVED-HELD" },
        )
        composeTestRule.onNodeWithTag("custody-holder-receipt-status")
            .assertTextEquals("RESOLVED-STATUS")
        composeTestRule.onNodeWithTag("custody-holder-held-bytes")
            .assertTextEquals("RESOLVED-HELD")
    }

    // The A7 three-state honesty rule: fresh / stale / no-receipt-yet are three
    // different lines that never collapse and never go empty — a stale custodian
    // must read as degraded redundancy the owner can see, not as an absent row.
    @Test
    fun theThreeReceiptStatesRenderThreeDistinctNonEmptyLines() {
        render(
            custodyRows = listOf(
                custody(
                    grantId = ByteArray(16) { 1 },
                    receiptState = CustodyReceiptStateView.FRESH,
                    receipt = receipt(statusKey = "devices.custody_receipt_fresh"),
                ),
                custody(
                    grantId = ByteArray(16) { 2 },
                    receiptState = CustodyReceiptStateView.STALE,
                    receipt = receipt(statusKey = "devices.custody_receipt_stale"),
                ),
                custody(
                    grantId = ByteArray(16) { 3 },
                    receiptState = CustodyReceiptStateView.NO_RECEIPT_YET,
                    receipt = receipt(
                        statusKey = "devices.custody_receipt_none",
                        attestedAtSecs = null,
                    ),
                ),
            ),
        )
        // Three rows, three status lines. The default resolver keys off the row's
        // own shared label, so a render that collapsed two states — or blanked the
        // receipt-less one — fails one of these three assertions rather than
        // passing on a shared "" the way an is-not-empty check alone would.
        val statuses = composeTestRule.onAllNodesWithTag("custody-holder-receipt-status")
        assertEquals(3, statuses.fetchSemanticsNodes().size)
        statuses[0].assertTextEquals("STATUS:devices.custody_receipt_fresh")
        statuses[1].assertTextEquals("STATUS:devices.custody_receipt_stale")
        statuses[2].assertTextEquals("STATUS:devices.custody_receipt_none")
    }
}
