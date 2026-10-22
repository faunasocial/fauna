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
import com.fauna.app.ui.viewmodel.CustodyGesture
import uniffi.fauna_client_capabilities.CustodyHeldRowView
import uniffi.fauna_client_capabilities.CustodyHolderRowView
import uniffi.fauna_client_capabilities.CustodyMintCandidateView
import uniffi.fauna_client_capabilities.CustodyOfferRowView
import uniffi.fauna_client_capabilities.CustodyScopesView
import uniffi.fauna_client_capabilities.CustodyReceiptRowView
import uniffi.fauna_client_capabilities.CustodyReceiptStateView
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_devices_machine.DeviceFolderRole
import uniffi.fauna_devices_machine.DeviceSummary
import uniffi.fauna_devices_machine.DevicesSnapshot
import uniffi.fauna_devices_machine.FleetMemberSummary

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
        principal: String? = null,
    ) = DeviceSummary(
        deviceId = deviceId,
        label = label,
        capabilities = "read,write",
        registeredAt = 0L,
        lastSeenAt = 0L,
        online = online,
        guardianMarked = guardianMarked,
        folders = folders,
        principal = principal,
        // Sibling field-adds (the device's p2p participation): unknown, and no
        // pending off-request.
        p2pParticipation = null,
        p2pOffRequested = false,
    )

    private fun snapshot(
        devices: List<DeviceSummary> = listOf(device("Phone")),
        error: LocalizedText? = null,
        members: List<FleetMemberSummary> = emptyList(),
        ownFingerprint: String? = null,
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
        // The fleet-removal door's reads: unaccounted members, and this device's
        // own fingerprint (the fleet id itself is not rendered).
        members = members,
        ownFleetId = null,
        ownFingerprint = ownFingerprint,
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

    private fun held(
        grantId: ByteArray = ByteArray(16) { 4 },
        owner: ByteArray = ByteArray(32) { 5 },
        scopes: CustodyScopesView? = CustodyScopesView(wholeAccount = true, scopes = emptyList()),
        // An unknown key resolves to itself, so the seed text is the key.
        budgetDraft: String = "8 GB",
        stopped: Boolean = false,
        custodianNestUrl: String? = null,
    ) = CustodyHeldRowView(
        grantId = grantId,
        owner = owner,
        scopes = scopes,
        retainedBytesCap = 8uL * 1024uL * 1024uL * 1024uL,
        budgetDraft = LocalizedText(budgetDraft, emptyMap()),
        receipt = receipt(),
        stopped = stopped,
        custodianNestUrl = custodianNestUrl,
    )

    private fun offer(
        grantId: ByteArray = ByteArray(16) { 6 },
        nestCanHold: Boolean = false,
    ) = CustodyOfferRowView(
        grantId = grantId,
        owner = ByteArray(32) { 9 },
        scopes = CustodyScopesView(wholeAccount = true, scopes = emptyList()),
        offeredAtSecs = 1_700_000_000L,
        nestCanHold = nestCanHold,
    )

    private fun candidate(n: Int, label: String) = CustodyMintCandidateView(
        host = ByteArray(32) { n.toByte() },
        channelHex = "chan$n",
        label = label,
    )

    private fun hex(bytes: ByteArray) = bytes.joinToString("") { "%02x".format(it) }

    private fun render(
        snapshot: DevicesSnapshot? = snapshot(),
        localDeviceId: String? = null,
        actorId: String? = "cd".repeat(32),
        onRemoveDevice: (Int) -> Unit = {},
        keylessPrincipals: Set<String> = emptySet(),
        custodyRows: List<CustodyHolderRowView> = emptyList(),
        custodyHeld: List<CustodyHeldRowView> = emptyList(),
        custodyOffers: List<CustodyOfferRowView> = emptyList(),
        custodyOfferTargets: Set<String> = emptySet(),
        mintCandidates: List<CustodyMintCandidateView>? = null,
        onCustodyGesture: (CustodyGesture) -> Unit = {},
        receiptStatusLine: (CustodyReceiptRowView) -> String = { "STATUS:${it.statusLabel.key}" },
        heldBytesLine: (CustodyReceiptRowView) -> String = { "HELD:${it.heldBytes}" },
        memberRemovePending: String? = null,
        onMemberArm: (String) -> Unit = {},
        onMemberCancel: () -> Unit = {},
        onMemberConfirm: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            DevicesRosterContent(
                snapshot = snapshot,
                localDeviceId = localDeviceId,
                actorId = actorId,
                onRemoveDevice = onRemoveDevice,
                statusLabel = { online -> if (online) "Online" else "Offline" },
                placeLabel = { o, a, d -> "PLACE:$o/$a/$d" },
                keylessPrincipals = keylessPrincipals,
                custodyRows = custodyRows,
                custodyHeld = custodyHeld,
                custodyOffers = custodyOffers,
                custodyOfferTargets = custodyOfferTargets,
                mintCandidates = mintCandidates,
                onCustodyGesture = onCustodyGesture,
                receiptStatusLine = receiptStatusLine,
                heldBytesLine = heldBytesLine,
                memberRemovePending = memberRemovePending,
                onMemberArm = onMemberArm,
                onMemberCancel = onMemberCancel,
                onMemberConfirm = onMemberConfirm,
                memberEnrolledAt = { ms -> "WHEN:$ms" },
            )
        }
    }

    private fun member(n: Int) = FleetMemberSummary(
        deviceId = "%02x".format(n).repeat(32),
        fingerprint = "fp$n",
        enrolledAtMs = 1_700_000_000_000L + n,
    )

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
            "Copies already held stay where they are",
            substring = true,
        ).assertDoesNotExist()
    }

    @Test
    fun settledCustodyOffersRevokeWithTheHonestBoundNote() {
        render(custodyRows = listOf(custody()))
        composeTestRule.onNodeWithTag("custody-holder-revoke-button").assertIsEnabled()
        composeTestRule.onNodeWithText(
            "Copies already held stay where they are",
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
            onCustodyGesture = { g ->
                if (g is CustodyGesture.Revoke) { grant = g.grantId; holder = g.holder }
            },
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

    // ── Piece 1: the keyless-posture marker ──────────────────────────────────

    // The badge rides exactly the rows the shared `keyless_posture` answer
    // names, by enrolled principal — never a principal-less row.
    @Test
    fun keylessPostureBadgeRendersOnlyOnRowsTheSharedRuleMarks() {
        render(
            snapshot(
                devices = listOf(
                    device("Kiosk", deviceId = "01".repeat(32), principal = "aa"),
                    device("Phone", deviceId = "02".repeat(32), principal = "bb"),
                    device("Legacy", deviceId = "03".repeat(32), principal = null),
                ),
            ),
            keylessPrincipals = setOf("aa"),
        )
        val cards = composeTestRule.onAllNodesWithTag("device-card")
        cards[0].assert(hasAnyDescendant(hasTestTag("device-keyless-posture-badge")))
        cards[1].assert(!hasAnyDescendant(hasTestTag("device-keyless-posture-badge")))
        cards[2].assert(!hasAnyDescendant(hasTestTag("device-keyless-posture-badge")))
        assertEquals(
            1,
            composeTestRule.onAllNodesWithTag("device-keyless-posture-badge")
                .fetchSemanticsNodes().size,
        )
        composeTestRule.onNodeWithTag("device-keyless-posture-badge")
            .assertTextEquals("Relay only — holds no keys")
    }

    // ── Piece 3: held-for-others cards ───────────────────────────────────────

    @Test
    fun heldSectionIsAbsentWithNothingHeld() {
        render()
        composeTestRule.onAllNodesWithTag("custody-held-card").assertCountEquals(0)
    }

    @Test
    fun heldCardRendersItsCanonicalFamilyAndSeedsTheBudget() {
        render(custodyHeld = listOf(held()))
        for (id in listOf(
            "custody-held-card", "custody-held-owner", "custody-held-scope",
            "custody-held-bytes", "custody-held-stop-button", "custody-held-remove-button",
        )) {
            composeTestRule.onNodeWithTag(id).assertExists()
        }
        composeTestRule.onNodeWithTag("custody-held-scope")
            .assertTextEquals("Their account's sealed data — unreadable on this device, apart from what they published to everyone")
        composeTestRule.onNodeWithTag("custody-held-bytes").assertTextEquals("HELD:1024")
        composeTestRule.onNodeWithTag("custody-held-budget-input").assertTextContains("8 GB")
    }

    /** A NEST-form hold carries the owner's restore address — the accept's URL
     *  verbatim; a device-form hold carries none (`devices.md` § Custody facet,
     *  `custody-held-restore-address`). */
    @Test
    fun onlyANestFormHoldShowsTheOwnersRestoreAddress() {
        render(
            custodyHeld = listOf(
                held(custodianNestUrl = "https://my-nest.example"),
                held(grantId = ByteArray(16) { 8 }),
            ),
        )
        composeTestRule.onAllNodesWithTag("custody-held-card").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("custody-held-restore-address").assertCountEquals(1)
        composeTestRule.onNodeWithTag("custody-held-restore-address")
            .assertTextContains("https://my-nest.example", substring = true)
    }

    @Test
    fun aNamedScopeSetRendersItsScopes() {
        render(
            custodyHeld = listOf(
                held(scopes = CustodyScopesView(wholeAccount = false, scopes = listOf("feed", "files"))),
            ),
        )
        composeTestRule.onNodeWithTag("custody-held-scope").assertTextEquals("feed, files")
    }

    // The budget commits the TYPED text on the keyboard's Done — the VM parses it
    // with the shared parser; this Content never does.
    @Test
    fun theBudgetCommitsTheTypedTextOnDone() {
        val seen = mutableListOf<CustodyGesture>()
        render(custodyHeld = listOf(held(grantId = ByteArray(16) { 4 })), onCustodyGesture = { seen += it })
        val input = composeTestRule.onNodeWithTag("custody-held-budget-input")
        input.performScrollTo().performTextReplacement("3072 MB")
        assertEquals(0, seen.size)
        input.performImeAction()
        val g = seen.single() as CustodyGesture.SetBudget
        assertEquals("3072 MB", g.typed)
        assertEquals(4, g.grantId[0].toInt())
    }

    // The e2e driver commits with a hardware Enter (`drivers/android.py`), which a
    // single-line field turns into its IME action — pinned so the two never drift.
    @OptIn(ExperimentalTestApi::class)
    @Test
    fun aHardwareEnterCommitsTheBudgetToo() {
        val seen = mutableListOf<CustodyGesture>()
        render(custodyHeld = listOf(held()), onCustodyGesture = { seen += it })
        val input = composeTestRule.onNodeWithTag("custody-held-budget-input")
        input.performScrollTo().performTextReplacement("1 GB")
        input.performClick()
        input.performKeyInput { pressKey(androidx.compose.ui.input.key.Key.Enter) }
        assertEquals("1 GB", (seen.single() as CustodyGesture.SetBudget).typed)
    }

    @Test
    fun stopAndRemoveCarryTheRowsGrantId() {
        val seen = mutableListOf<CustodyGesture>()
        render(
            custodyHeld = listOf(held(grantId = ByteArray(16) { 1 }), held(grantId = ByteArray(16) { 2 })),
            onCustodyGesture = { seen += it },
        )
        composeTestRule.onAllNodesWithTag("custody-held-stop-button")[1].performScrollTo().performClick()
        composeTestRule.onAllNodesWithTag("custody-held-remove-button")[0].performScrollTo().performClick()
        assertEquals(2, (seen[0] as CustodyGesture.Stop).grantId[0].toInt())
        assertEquals(1, (seen[1] as CustodyGesture.Remove).grantId[0].toInt())
    }

    // Stopping pauses and keeps the bytes: the control says so and disables,
    // while remove — the reclaim — stays available.
    @Test
    fun aStoppedHoldDisablesStopAndKeepsRemove() {
        render(custodyHeld = listOf(held(stopped = true)))
        composeTestRule.onNodeWithTag("custody-held-stop-button")
            .assertIsNotEnabled()
            .assertTextEquals("Stopped — stored bytes remain until removed")
        composeTestRule.onNodeWithTag("custody-held-remove-button").assertIsEnabled()
    }

    // ── Piece 3: the consent card ────────────────────────────────────────────

    @Test
    fun anOfferRendersTheFloorCopyBeforeAcceptAndNoTargetSelectWithoutTheAnswer() {
        // `nestCanHold` alone never renders the select — only the shared
        // `custody_offer_shows_target_select` answer does.
        render(custodyOffers = listOf(offer(nestCanHold = true)))
        composeTestRule.onNodeWithTag("custody-offer-card").assertExists()
        composeTestRule.onNodeWithTag("custody-offer-floor-note").assertTextEquals(
            "This device would store sealed data it cannot read, apart from what they have published " +
                "to everyone. Of the rest it would see only the shape: which scopes exist, how much is " +
                "stored, and when it changes — never the content.",
        )
        composeTestRule.onAllNodesWithTag("custody-offer-target-select").assertCountEquals(0)
    }

    @Test
    fun acceptWithoutTheSelectBindsThisDevice() {
        val seen = mutableListOf<CustodyGesture>()
        render(custodyOffers = listOf(offer()), onCustodyGesture = { seen += it })
        composeTestRule.onNodeWithTag("custody-offer-accept-button").performScrollTo().performClick()
        val g = seen.single() as CustodyGesture.Accept
        assertEquals(false, g.onNest)
        assertEquals(6, g.grantId[0].toInt())
    }

    @Test
    fun theTargetSelectRendersWhereTheSharedAnswerSaysAndAcceptBindsWhatItPicks() {
        val seen = mutableListOf<CustodyGesture>()
        val o = offer(grantId = ByteArray(16) { 6 })
        render(
            custodyOffers = listOf(o),
            custodyOfferTargets = setOf(hex(o.grantId)),
            onCustodyGesture = { seen += it },
        )
        composeTestRule.onNodeWithTag("custody-offer-target-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("My nest").performClick()
        composeTestRule.onNodeWithTag("custody-offer-accept-button").performScrollTo().performClick()
        assertEquals(true, (seen.single() as CustodyGesture.Accept).onNest)
    }

    @Test
    fun declineCarriesTheOffersGrantId() {
        val seen = mutableListOf<CustodyGesture>()
        render(custodyOffers = listOf(offer(grantId = ByteArray(16) { 3 })), onCustodyGesture = { seen += it })
        composeTestRule.onNodeWithTag("custody-offer-decline-button").performScrollTo().performClick()
        assertEquals(3, (seen.single() as CustodyGesture.Decline).grantId[0].toInt())
    }

    // ── The mint ─────────────────────────────────────────────────────────────

    @Test
    fun theMintButtonAlwaysRendersAndOpensTheFlow() {
        val seen = mutableListOf<CustodyGesture>()
        render(onCustodyGesture = { seen += it })
        composeTestRule.onAllNodesWithTag("custody-mint-host-select").assertCountEquals(0)
        composeTestRule.onNodeWithTag("custody-mint-button").performScrollTo().performClick()
        assertEquals(CustodyGesture.MintOpen, seen.single())
    }

    @Test
    fun anOpenFlowConfirmsOnlyOnceAHostIsChosenAndPassesTheRowBack() {
        val seen = mutableListOf<CustodyGesture>()
        render(
            mintCandidates = listOf(candidate(1, "Alice"), candidate(2, "Bob")),
            onCustodyGesture = { seen += it },
        )
        composeTestRule.onNodeWithTag("custody-mint-floor-note").assertExists()
        composeTestRule.onNodeWithTag("custody-mint-confirm-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("custody-mint-host-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Bob").performClick()
        composeTestRule.onNodeWithTag("custody-mint-confirm-button").assertIsEnabled()
            .performScrollTo().performClick()
        val g = seen.single() as CustodyGesture.Mint
        assertEquals(2, g.host[0].toInt())
        assertEquals("chan2", g.channelHex)
    }

    @Test
    fun cancelClosesTheFlow() {
        val seen = mutableListOf<CustodyGesture>()
        render(mintCandidates = listOf(candidate(1, "Alice")), onCustodyGesture = { seen += it })
        composeTestRule.onNodeWithTag("custody-mint-cancel-button").performScrollTo().performClick()
        assertEquals(CustodyGesture.MintCancel, seen.single())
    }

    // ── Members without a matching entry (devices.md § Members without a matching entry) ──

    // The whole group — title, note, cards — is absent while `members` is empty.
    @Test
    fun memberGroupRendersNothingWhenThereAreNoMembers() {
        render(snapshot(members = emptyList()))
        composeTestRule.onNodeWithTag("device-member-note").assertDoesNotExist()
        composeTestRule.onNodeWithTag("device-member-card").assertDoesNotExist()
    }

    @Test
    fun memberCardShowsTheSharedFingerprintAndEnrolledAt() {
        render(snapshot(members = listOf(member(1), member(2))))
        composeTestRule.onNodeWithTag("device-member-note").assertExists()
        assertEquals(2, composeTestRule.onAllNodesWithTag("device-member-card").fetchSemanticsNodes().size)
        composeTestRule.onNodeWithText("Device fp1").assertExists()
        composeTestRule.onNodeWithText("Device fp2").assertExists()
        composeTestRule.onNodeWithText("Says it signed in WHEN:1700000000001").assertExists()
        assertEquals(2, composeTestRule.onAllNodesWithTag("device-member-remove-button").fetchSemanticsNodes().size)
        composeTestRule.onNodeWithTag("device-member-remove-confirm-button").assertDoesNotExist()
    }

    // The remove button arms THAT card by its fleet id.
    @Test
    fun removeButtonArmsTheCardByItsFleetId() {
        val armed = mutableListOf<String>()
        render(snapshot(members = listOf(member(1), member(2))), onMemberArm = { armed += it })
        composeTestRule.onAllNodesWithTag("device-member-remove-button")[1].performScrollTo().performClick()
        assertEquals(listOf(member(2).deviceId), armed)
    }

    // Only the armed card swaps its button for the confirm/cancel pair, and the
    // arm follows the id, not the position: the same pending id still lands on
    // its card after a refresh inserts another member ahead of it.
    @Test
    fun onlyTheArmedCardPaintsConfirmAndCancelAndTheArmFollowsTheId() {
        val confirmed = mutableListOf<String>()
        var cancelled = 0
        render(
            snapshot(members = listOf(member(3), member(1), member(2))),
            memberRemovePending = member(2).deviceId,
            onMemberConfirm = { confirmed += it },
            onMemberCancel = { cancelled++ },
        )
        assertEquals(1, composeTestRule.onAllNodesWithTag("device-member-remove-confirm-button").fetchSemanticsNodes().size)
        assertEquals(1, composeTestRule.onAllNodesWithTag("device-member-remove-cancel-button").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("device-member-remove-button").fetchSemanticsNodes().size)
        composeTestRule.onNodeWithTag("device-member-remove-cancel-button").performScrollTo().performClick()
        assertEquals(1, cancelled)
        composeTestRule.onNodeWithTag("device-member-remove-confirm-button").performScrollTo().performClick()
        assertEquals(listOf(member(2).deviceId), confirmed)
    }

    // `device-own-fingerprint` rides the this-device row only, and only while the
    // snapshot carries one.
    @Test
    fun ownFingerprintRendersOnTheMarkedRowOnly() {
        render(
            snapshot(
                devices = listOf(
                    device("Phone", deviceId = "aa".repeat(32)),
                    device("This laptop", deviceId = "bb".repeat(32)),
                ),
                ownFingerprint = "ownfp",
            ),
            localDeviceId = "bb".repeat(32),
        )
        assertEquals(1, composeTestRule.onAllNodesWithTag("device-own-fingerprint").fetchSemanticsNodes().size)
        composeTestRule.onNodeWithText("Key ownfp").assertExists()
    }

    @Test
    fun ownFingerprintAbsentWhenSnapshotCarriesNone() {
        render(
            snapshot(devices = listOf(device("This laptop", deviceId = "bb".repeat(32)))),
            localDeviceId = "bb".repeat(32),
        )
        composeTestRule.onNodeWithTag("device-own-fingerprint").assertDoesNotExist()
    }
}
