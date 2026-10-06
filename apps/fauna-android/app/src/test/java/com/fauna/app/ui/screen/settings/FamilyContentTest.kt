package com.fauna.app.ui.screen.settings

import android.content.Context
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.UndenyDecide
import com.fauna.app.ui.util.getStringFmt
import com.fauna.ffi.FfiContentPolicy
import com.fauna.ffi.FfiFamilyAgeBand
import com.fauna.ffi.FfiFamilyApprovalEntry
import com.fauna.ffi.FfiFamilyBlockedPeer
import com.fauna.ffi.FfiFamilyContentNotice
import com.fauna.ffi.FfiFamilyGuardianInfo
import com.fauna.ffi.FfiFamilyIncomingTransfer
import com.fauna.ffi.FfiFamilyPendingTransfer
import com.fauna.ffi.FfiFamilyStatus
import com.fauna.ffi.FfiFamilyWardDevice
import com.fauna.ffi.FfiFamilyWardInfo
import com.fauna.ffi.FfiReachPolicy
import com.fauna.ffi.FfiScreenTimePolicy
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [FamilyContent] (family-safety.md
 * § App surface). Renders with seeded state — no Hilt, no VM — so the
 * guardian / supervised / incoming-transfer sections, the fail-closed policy
 * render, and the typed approval-row display rule are exercised on the JVM.
 * The policy editor/summary DO call the real shared-Rust reach-policy
 * catalog (`unknownSenderOptions`/`feedSourcesLabel`/`reachPolicySummary`)
 * over UniFFI — run via `just android-host-test`, which wires JNA at the
 * host `libfauna_ffi.so` (a bare `:app:testDebugUnitTest` dies at class-init
 * without it). (Android E2E `test_family.py --client android` is the
 * standing gate once the host emulator lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FamilyContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val ctx get() = ApplicationProvider.getApplicationContext<Context>()

    private fun policy(
        contactApproval: Boolean = false,
        unknownSenderMail: String = "allow",
        federationContact: Boolean = true,
        feedSources: String = "allow",
        contentPolicy: FfiContentPolicy? = null,
        contentNotify: Boolean? = null,
        screenTime: FfiScreenTimePolicy? = null,
        unknownPeerDm: String? = null,
    ) = FfiReachPolicy(
        contactApproval = contactApproval,
        unknownSenderMail = unknownSenderMail,
        federationContact = federationContact,
        feedSources = feedSources,
        // contentPolicy / contentNotify / screenTime default null (unset /
        // "leave unchanged"); the Slice C/E editor arms pass a real value
        // (family-safety.md § Content policy / § Screen time).
        contentPolicy = contentPolicy,
        screenTime = screenTime,
        contentNotify = contentNotify,
        // unknownPeerDm: `Option<String>` on the wire, absent = "leave
        // unchanged" (family-safety.md § The bridge-DM gate) — a test opts in
        // to exercise a stored value; the render-default coverage below seeds
        // this at its own `null` explicitly for clarity.
        unknownPeerDm = unknownPeerDm,
    )

    private fun ward(
        handle: String,
        actorId: ByteArray = ByteArray(32) { 1 },
        policy: FfiReachPolicy = policy(),
        pendingTransfer: FfiFamilyPendingTransfer? = null,
        devices: List<FfiFamilyWardDevice> = emptyList(),
        contentNotices: List<FfiFamilyContentNotice> = emptyList(),
        usageTodayMinutes: UInt? = null,
        ageBand: FfiFamilyAgeBand? = null,
        blockedDmPeers: List<FfiFamilyBlockedPeer> = emptyList(),
    ) = FfiFamilyWardInfo(
        actorId = actorId,
        handle = handle,
        policy = policy,
        pendingTransfer = pendingTransfer,
        // family-safety.md § Screen time — the ward's cross-device usage total
        // for their current local day; null unless a test opts in.
        contentNotices = contentNotices,
        usageTodayMinutes = usageTodayMinutes,
        devices = devices,
        // family-safety.md § The account age band — `null` is a band-less
        // admission, which renders nothing; the age-band cases below opt in.
        ageBand = ageBand,
        // family-safety.md § The bridge-DM gate → *The un-deny surface* — the
        // guardian's denied peers; empty unless a test opts in.
        blockedDmPeers = blockedDmPeers,
    )

    private fun approval(
        kind: String,
        peerAddress: String = "",
        summary: String = "",
        supervisedActorId: ByteArray = ByteArray(32) { 1 },
        peerActorId: ByteArray = ByteArray(32) { 2 },
        messageId: ByteArray = ByteArray(16) { 3 },
    ) = FfiFamilyApprovalEntry(
        supervisedActorId = supervisedActorId,
        supervisedHandle = "ward",
        kind = kind,
        peerActorId = peerActorId,
        peerAddress = peerAddress,
        messageId = messageId,
        summary = summary,
        // The peer's nest-joined handle (v1.x) — none of this file's coverage
        // exercises the approval row's peer-handle rendering.
        peerHandle = "",
        // A `feed_source` item's key (v1.x § Feed-source approvals); empty for
        // every other kind.
        bridgeId = "",
        operation = "",
        target = "",
        createdAt = 0,
    )

    private fun status(
        supervisedBy: FfiFamilyGuardianInfo? = null,
        policy: FfiReachPolicy? = null,
        wards: List<FfiFamilyWardInfo> = emptyList(),
        incomingTransfers: List<FfiFamilyIncomingTransfer> = emptyList(),
        usageTodayMinutes: UInt? = null,
        ageBand: FfiFamilyAgeBand? = null,
    ) = FfiFamilyStatus(
        supervisedBy = supervisedBy,
        policy = policy,
        wards = wards,
        incomingTransfers = incomingTransfers,
        usageTodayMinutes = usageTodayMinutes,
        // The caller's own pending contact asks (v1.x) — none of this file's
        // coverage exercises the child-initiated contact-request surface.
        contactRequests = emptyList(),
        // Likewise the ward's own feed-source asks (v1.x § Feed-source approvals).
        feedRequests = emptyList(),
        // The caller's OWN band (§ The account age band); `null` covers both
        // no-row and band-less admission — every case but the age-band ones.
        ageBand = ageBand,
        // The reply's gated supervision fold feeds the enforcement stores;
        // FamilyContent renders from the raw reply, so no case here sets it.
        supervision = null,
    )

    private fun render(
        status: FfiFamilyStatus?,
        approvals: List<FfiFamilyApprovalEntry> = emptyList(),
        selectedWardActorId: ByteArray? = status?.wards?.firstOrNull()?.actorId,
        onSelectWard: (ByteArray) -> Unit = {},
        onSavePolicy: (ByteArray, FfiReachPolicy) -> Unit = { _, _ -> },
        onSaveError: (String) -> Unit = {},
        onMarkDevice: (ByteArray, String, Boolean) -> Unit = { _, _, _ -> },
        onAllowBlockedPeer: (UndenyDecide) -> Unit = {},
        onDecideApproval: (FfiFamilyApprovalEntry, Boolean) -> Unit = { _, _ -> },
        onAddContact: (ByteArray, ByteArray) -> Unit = { _, _ -> },
        onGraduate: (ByteArray) -> Unit = {},
        onProposeTransfer: (ByteArray, ByteArray) -> Unit = { _, _ -> },
        onCancelTransfer: (ByteArray) -> Unit = {},
        onAcceptIncomingTransfer: (ByteArray) -> Unit = {},
        onDeclineIncomingTransfer: (ByteArray) -> Unit = {},
    ) {
        composeTestRule.setContent {
            FamilyContent(
                status = status,
                approvals = approvals,
                selectedWardActorId = selectedWardActorId,
                onBack = {},
                onSelectWard = onSelectWard,
                onSavePolicy = onSavePolicy,
                onSaveError = onSaveError,
                onMarkDevice = onMarkDevice,
                onAllowBlockedPeer = onAllowBlockedPeer,
                onDecideApproval = onDecideApproval,
                onAddContact = onAddContact,
                onGraduate = onGraduate,
                onProposeTransfer = onProposeTransfer,
                onCancelTransfer = onCancelTransfer,
                onAcceptIncomingTransfer = onAcceptIncomingTransfer,
                onDeclineIncomingTransfer = onDeclineIncomingTransfer,
            )
        }
    }

    @Test
    fun rendersHeadingWhileLoading() {
        render(status = null)
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("family-heading").assertExists()
    }

    @Test
    fun supervisedSectionRendersGuardianHandleAndPolicySummary() {
        val guardian = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 5 }, handle = "Guardian1")
        render(
            status = status(
                supervisedBy = guardian,
                policy = policy(contactApproval = true, unknownSenderMail = "reject", federationContact = false, feedSources = "block"),
                wards = emptyList(),
                incomingTransfers = emptyList(),
                usageTodayMinutes = null,
            ),
        )
        composeTestRule.onNodeWithTag("family-guardian-handle")
            .assertTextEquals(ctx.getStringFmt(R.string.family_guardian_label, "Guardian1"))
        composeTestRule.onNodeWithTag("family-policy-summary").assertExists()
    }

    // ── Age-band readouts (family-safety.md § App surface → *Age-band surfaces*):
    // the real shared `ageBandLine` over UniFFI, absent — never placeholdered —
    // when there is no band.

    @Test
    fun supervisedSectionShowsTheOwnAgeBandSummary() {
        val guardian = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 5 }, handle = "Guardian1")
        render(
            status = status(
                supervisedBy = guardian,
                policy = policy(),
                ageBand = FfiFamilyAgeBand(band = "13-15", provenance = "guardian-asserted"),
            ),
        )
        composeTestRule.onNodeWithTag("family-age-band-summary")
            .assertTextEquals("Your age band: 13–15 · set by guardian, set at admission")
    }

    @Test
    fun supervisedSectionHasNoAgeBandSummaryWithoutABand() {
        val guardian = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 5 }, handle = "Guardian1")
        render(status = status(supervisedBy = guardian, policy = policy()))
        composeTestRule.onNodeWithTag("family-age-band-summary").assertDoesNotExist()
    }

    @Test
    fun wardRowShowsItsAgeBandOnlyWhenItHasOne() {
        val banded = ward(
            "Ward1",
            actorId = ByteArray(32) { 1 },
            ageBand = FfiFamilyAgeBand(band = "u13", provenance = "attested-android"),
        )
        val bandless = ward("Ward2", actorId = ByteArray(32) { 2 })
        render(status = status(wards = listOf(banded, bandless)))
        // One readout for two rows: the band-less ward paints nothing.
        val lines = composeTestRule.onAllNodesWithTag("family-ward-age-band", useUnmergedTree = true)
        assertEquals(1, lines.fetchSemanticsNodes().size)
        lines[0].assertTextEquals("Age band: Under 13 · verified on Android")
    }

    @Test
    fun supervisedSectionAbsentWhenNotSupervised() {
        render(status = status(supervisedBy = null, policy = null, wards = emptyList(), incomingTransfers = emptyList(), usageTodayMinutes = null))
        composeTestRule.onNodeWithTag("family-guardian-handle").assertDoesNotExist()
        composeTestRule.onNodeWithTag("family-policy-summary").assertDoesNotExist()
    }

    @Test
    fun guardianSectionRendersWardRowsAndPolicyEditorForSelected() {
        val w1 = ward("Ward1", actorId = ByteArray(32) { 1 })
        val w2 = ward("Ward2", actorId = ByteArray(32) { 2 })
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w1, w2), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w1.actorId,
        )
        assertEquals(2, composeTestRule.onAllNodesWithTag("family-ward-item").fetchSemanticsNodes().size)
        composeTestRule.onNodeWithTag("family-policy-contact-approval-toggle").assertExists()
        composeTestRule.onNodeWithTag("family-policy-unknown-sender-select").assertExists()
        composeTestRule.onNodeWithTag("family-policy-federation-toggle").assertExists()
        composeTestRule.onNodeWithTag("family-policy-feed-sources-select").assertExists()
        composeTestRule.onNodeWithTag("family-policy-unknown-peer-dm-select").assertExists()
        composeTestRule.onNodeWithTag("family-policy-save-button").assertExists()
    }

    @Test
    fun wardRowClickSelectsWard() {
        val w1 = ward("Ward1", actorId = ByteArray(32) { 1 })
        val w2 = ward("Ward2", actorId = ByteArray(32) { 2 })
        var selected: ByteArray? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w1, w2), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w1.actorId,
            onSelectWard = { selected = it },
        )
        composeTestRule.onAllNodesWithTag("family-ward-item")[1].performScrollTo().performClick()
        assertEquals(w2.actorId, selected)
    }

    @Test
    fun wardRowRendersContentNoticesWhenNonEmpty() {
        val w = ward("Ward1", contentNotices = listOf(FfiFamilyContentNotice(category = "nsfw", count = 3u)))
        render(status = status(wards = listOf(w)), selectedWardActorId = w.actorId)
        // useUnmergedTree: the ward row is clickable, which merges its
        // descendants' semantics into the row's own node.
        composeTestRule.onNodeWithTag("family-ward-content-notices", useUnmergedTree = true).assertExists()
    }

    @Test
    fun wardRowOmitsContentNoticesWhenEmpty() {
        val w = ward("Ward1", contentNotices = emptyList())
        render(status = status(wards = listOf(w)), selectedWardActorId = w.actorId)
        composeTestRule.onNodeWithTag("family-ward-content-notices", useUnmergedTree = true).assertDoesNotExist()
    }

    @Test
    fun policySaveFiresWithEditedValues() {
        val w = ward("Ward1", policy = policy(contactApproval = false, unknownSenderMail = "allow", federationContact = true, feedSources = "allow"))
        var saved: FfiReachPolicy? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onSavePolicy = { _, p -> saved = p },
        )
        composeTestRule.onNodeWithTag("family-policy-contact-approval-toggle").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("family-policy-save-button").performScrollTo().performClick()
        assertEquals(true, saved?.contactApproval)
        assertEquals("allow", saved?.unknownSenderMail)
    }

    // ── Fail-closed knob render (load-bearing — family-safety.md § Guardian ──
    // policy): an unparseable wire value normalizes to the strictest option,
    // never the permissive one. windows shipped the "defaults to allow" bug
    // this guards against.

    @Test
    fun unparseableUnknownSenderRendersFailClosedToHold() {
        val w = ward("Ward1", policy = policy(unknownSenderMail = "some_future_v2_value"))
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-unknown-sender-select")
            .assert(hasText(ctx.getString(R.string.family_value_hold)))
    }

    @Test
    fun unparseableFeedSourcesRendersFailClosedToBlock() {
        val w = ward("Ward1", policy = policy(feedSources = "some_future_v2_value"))
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-feed-sources-select")
            .assert(hasText(ctx.getString(R.string.family_value_block)))
    }

    @Test
    fun validKnobValuesRenderTheirOwnLabel() {
        val w = ward("Ward1", policy = policy(unknownSenderMail = "reject", feedSources = "allow"))
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-unknown-sender-select")
            .assert(hasText(ctx.getString(R.string.family_value_reject)))
        composeTestRule.onNodeWithTag("family-policy-feed-sources-select")
            .assert(hasText(ctx.getString(R.string.family_value_allow)))
    }

    // ── The bridge-DM gate (family-safety.md § The bridge-DM gate): the ──
    // mirror image of the render rule above — an ABSENT `unknown_peer_dm`
    // renders the `allow` DEFAULT, never the fail-closed `hold` (the nest
    // omits a knob sitting at its default). A PRESENT-but-unparseable value
    // still fails closed like every other knob. The knob rides `policy.update`
    // only once the guardian has touched the select (`Option<String>` on the
    // wire, absent = leave unchanged).

    @Test
    fun absentUnknownPeerDmRendersAllowDefault() {
        val w = ward("Ward1", policy = policy(unknownPeerDm = null))
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-unknown-peer-dm-select")
            .assert(hasText(ctx.getString(R.string.family_value_allow)))
    }

    @Test
    fun unparseableUnknownPeerDmRendersFailClosedToHold() {
        val w = ward("Ward1", policy = policy(unknownPeerDm = "some_future_v2_value"))
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-unknown-peer-dm-select")
            .assert(hasText(ctx.getString(R.string.family_value_hold)))
    }

    @Test
    fun presentUnknownPeerDmRendersItsOwnLabel() {
        val w = ward("Ward1", policy = policy(unknownPeerDm = "hold"))
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-unknown-peer-dm-select")
            .assert(hasText(ctx.getString(R.string.family_value_hold)))
    }

    @Test
    fun policySaveOmitsUntouchedUnknownPeerDm() {
        // An unrelated edit (contactApproval) must not echo the rendered
        // unknown-peer-dm value back to the nest — an untouched select sends
        // `null` (leave unchanged), never the `hold` this build renders it as.
        val w = ward("Ward1", policy = policy(unknownPeerDm = "hold"))
        var saved: FfiReachPolicy? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onSavePolicy = { _, p -> saved = p },
        )
        composeTestRule.onNodeWithTag("family-policy-contact-approval-toggle").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("family-policy-save-button").performScrollTo().performClick()
        assertNull(saved?.unknownPeerDm)
    }

    @Test
    fun policySaveIncludesTouchedUnknownPeerDm() {
        val w = ward("Ward1", policy = policy(unknownPeerDm = null))
        var saved: FfiReachPolicy? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onSavePolicy = { _, p -> saved = p },
        )
        composeTestRule.onNodeWithTag("family-policy-unknown-peer-dm-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText(ctx.getString(R.string.family_value_hold)).performClick()
        composeTestRule.onNodeWithTag("family-policy-save-button").performScrollTo().performClick()
        assertEquals("hold", saved?.unknownPeerDm)
    }

    // ── Content policy (Slice C, family-safety.md § Content policy): the four ──
    // per-category floors + the Notify toggle, over the shared content-floor
    // catalog. Same fail-closed label-round-trip rule as the reach knobs above.

    @Test
    fun guardianEditorRendersContentPolicyControls() {
        val w = ward("Ward1")
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-content-nsfw-select").assertExists()
        composeTestRule.onNodeWithTag("family-policy-content-spam-select").assertExists()
        composeTestRule.onNodeWithTag("family-policy-content-phishing-select").assertExists()
        composeTestRule.onNodeWithTag("family-policy-content-commercial-select").assertExists()
        composeTestRule.onNodeWithTag("family-policy-content-notify-toggle").assertExists()
    }

    @Test
    fun unsetContentFloorSeedsInherit() {
        // A ward with no content policy (`null`) seeds every floor to `inherit`
        // ("Use my settings"), the ward's-own-preferences-decide default.
        val w = ward("Ward1", policy = policy(contentPolicy = null))
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-content-nsfw-select")
            .assert(hasText(ctx.getString(R.string.family_value_inherit)))
    }

    @Test
    fun unparseableContentFloorRendersFailClosedToBlock() {
        // A floor value this build cannot name (a newer nest) renders `block`,
        // this knob's strict option, never the permissive `inherit`.
        val w = ward("Ward1", policy = policy(contentPolicy = FfiContentPolicy("some_future_v2", "inherit", "collapse", "inherit")))
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-content-nsfw-select")
            .assert(hasText(ctx.getString(R.string.family_value_block)))
        composeTestRule.onNodeWithTag("family-policy-content-phishing-select")
            .assert(hasText(ctx.getString(R.string.family_value_collapse)))
    }

    @Test
    fun policySaveIncludesAssembledContentPolicy() {
        // An untouched Save round-trips the seeded content floors + notify as a
        // present (replace-semantics) policy, not the prior "leave unchanged" null.
        val w = ward(
            "Ward1",
            policy = policy(
                contentPolicy = FfiContentPolicy("block", "collapse", "inherit", "inherit"),
                contentNotify = true,
            ),
        )
        var saved: FfiReachPolicy? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onSavePolicy = { _, p -> saved = p },
        )
        composeTestRule.onNodeWithTag("family-policy-save-button").performScrollTo().performClick()
        assertEquals("block", saved?.contentPolicy?.nsfw)
        assertEquals("collapse", saved?.contentPolicy?.spam)
        assertEquals("inherit", saved?.contentPolicy?.phishing)
        assertEquals(true, saved?.contentNotify)
    }

    // ── Screen time (Slice E, family-safety.md § Screen time): the guardian's ──
    // three policy inputs + the per-ward/ward-own usage readouts, over the
    // shared parseTimeOfDay/parseDailyMinutes/formatTimeOfDay/usageTodayLine.

    @Test
    fun guardianEditorRendersScreenTimeControls() {
        val w = ward("Ward1")
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-screen-window-start-input").assertExists()
        composeTestRule.onNodeWithTag("family-policy-screen-window-end-input").assertExists()
        composeTestRule.onNodeWithTag("family-policy-screen-daily-minutes-input").assertExists()
    }

    @Test
    fun screenTimeInputsPrefillFromStoredPolicy() {
        // The stored minutes render back through the same shared formatter the
        // parse side inverts, so what a guardian sees is exactly what they
        // could retype (mirrors linux's screen_window_start.set_text).
        val w = ward(
            "Ward1",
            policy = policy(screenTime = FfiScreenTimePolicy(windowStart = 540u, windowEnd = 1260u, dailyMinutes = 120u)),
        )
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-policy-screen-window-start-input").assert(hasText("09:00"))
        composeTestRule.onNodeWithTag("family-policy-screen-window-end-input").assert(hasText("21:00"))
        composeTestRule.onNodeWithTag("family-policy-screen-daily-minutes-input").assert(hasText("120"))
    }

    @Test
    fun policySaveIncludesParsedScreenTimePolicy() {
        val w = ward("Ward1")
        var saved: FfiReachPolicy? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onSavePolicy = { _, p -> saved = p },
        )
        composeTestRule.onNodeWithTag("family-policy-screen-window-start-input").performTextInput("09:00")
        composeTestRule.onNodeWithTag("family-policy-screen-window-end-input").performTextInput("21:00")
        composeTestRule.onNodeWithTag("family-policy-screen-daily-minutes-input").performTextInput("120")
        composeTestRule.onNodeWithTag("family-policy-save-button").performScrollTo().performClick()
        assertEquals(540, saved?.screenTime?.windowStart?.toInt())
        assertEquals(1260, saved?.screenTime?.windowEnd?.toInt())
        assertEquals(120, saved?.screenTime?.dailyMinutes?.toInt())
    }

    @Test
    fun invalidScreenTimeInputRefusesSaveAndSurfacesTheReason() {
        // Fallible ONLY because of screen time: an unparseable guardian input
        // must never leave the client — the save is refused locally and the
        // reason surfaces via onSaveError, exactly as onSavePolicy would have
        // fired had the input been valid (mirrors linux's editor_policy Result).
        val w = ward("Ward1")
        var saved: FfiReachPolicy? = null
        var error: String? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onSavePolicy = { _, p -> saved = p },
            onSaveError = { error = it },
        )
        composeTestRule.onNodeWithTag("family-policy-screen-window-start-input").performTextInput("24:00")
        composeTestRule.onNodeWithTag("family-policy-save-button").performScrollTo().performClick()
        assertNull(saved)
        assertEquals(true, error?.isNotEmpty())
    }

    @Test
    fun wardRowRendersUsageTodayWhenBudgetSet() {
        val w = ward("Ward1", policy = policy(screenTime = FfiScreenTimePolicy(null, null, 120u)), usageTodayMinutes = 30u)
        render(status = status(wards = listOf(w)), selectedWardActorId = w.actorId)
        composeTestRule.onNodeWithTag("family-ward-usage-today", useUnmergedTree = true)
            .assert(hasText("120", substring = true))
    }

    @Test
    fun wardRowOmitsUsageTodayWhenNoBudget() {
        val w = ward("Ward1", usageTodayMinutes = null)
        render(status = status(wards = listOf(w)), selectedWardActorId = w.actorId)
        composeTestRule.onNodeWithTag("family-ward-usage-today", useUnmergedTree = true).assertDoesNotExist()
    }

    @Test
    fun policySummaryFoldsInUsageTodayLineWhenBudgetSet() {
        // The ward's own read-only summary must show the SAME number the
        // guardian sees — family-safety.md's transparency rule, made
        // structural by both surfaces resolving through usageTodayLine.
        val guardian = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 5 }, handle = "Guardian1")
        render(
            status = status(
                supervisedBy = guardian,
                policy = policy(screenTime = FfiScreenTimePolicy(null, null, 120u)),
                wards = emptyList(),
                incomingTransfers = emptyList(),
                usageTodayMinutes = 15u,
            ),
        )
        composeTestRule.onNodeWithTag("family-policy-summary").assert(hasText("15", substring = true))
    }

    @Test
    fun policySummaryOmitsUsageTodayLineWhenNoBudget() {
        val guardian = FfiFamilyGuardianInfo(actorId = ByteArray(32) { 5 }, handle = "Guardian1")
        render(
            status = status(
                supervisedBy = guardian,
                policy = policy(),
                wards = emptyList(),
                incomingTransfers = emptyList(),
                usageTodayMinutes = null,
            ),
        )
        composeTestRule.onNodeWithText(ctx.getString(R.string.family_ward_usage_today_label), substring = true)
            .assertDoesNotExist()
    }

    // ── Approval row display rule (family-safety.md § Don't surface content) ──

    @Test
    fun mailHoldApprovalRowShowsPeerAddressNotEmptySummary() {
        val entry = approval(kind = "mail_hold", peerAddress = "attacker@example.com", summary = "")
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(ward("Ward1")), incomingTransfers = emptyList(), usageTodayMinutes = null),
            approvals = listOf(entry),
        )
        composeTestRule.onNode(
            hasTestTag("family-approval-item") and hasAnyDescendant(hasText("attacker@example.com")),
        ).assertExists()
    }

    @Test
    fun mailHoldApprovalRowWithNoAddressShowsNoSenderLabel() {
        val entry = approval(kind = "mail_hold", peerAddress = "", summary = "")
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(ward("Ward1")), incomingTransfers = emptyList(), usageTodayMinutes = null),
            approvals = listOf(entry),
        )
        composeTestRule.onNode(
            hasTestTag("family-approval-item") and hasAnyDescendant(hasText(ctx.getString(R.string.family_approval_no_sender))),
        ).assertExists()
    }

    @Test
    fun contactApprovalRowShowsSummaryIgnoringEmptyPeerAddress() {
        val entry = approval(kind = "contact", peerAddress = "", summary = "wants to connect")
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(ward("Ward1")), incomingTransfers = emptyList(), usageTodayMinutes = null),
            approvals = listOf(entry),
        )
        composeTestRule.onNode(
            hasTestTag("family-approval-item") and hasAnyDescendant(hasText("wants to connect")),
        ).assertExists()
    }

    @Test
    fun approveButtonFiresDecideWithApproveTrue() {
        val entry = approval(kind = "contact", summary = "hi")
        var decided: Pair<FfiFamilyApprovalEntry, Boolean>? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(ward("Ward1")), incomingTransfers = emptyList(), usageTodayMinutes = null),
            approvals = listOf(entry),
            onDecideApproval = { e, approve -> decided = e to approve },
        )
        composeTestRule.onNodeWithTag("family-approval-approve-button").performScrollTo().performClick()
        assertEquals(true, decided?.second)
    }

    // ── Device mark (Slice F, guardian half — family-safety.md § Full ──
    // visibility for young children): one `family-device-mark-item` row per
    // device of the SELECTED ward, each carrying a `family-device-mark-toggle`.

    @Test
    fun deviceMarkSectionRendersOneRowPerWardDevice() {
        val w = ward(
            "Ward1",
            devices = listOf(
                FfiFamilyWardDevice(deviceId = "d1", label = "Alice's Pixel", guardianMarked = false),
                FfiFamilyWardDevice(deviceId = "d2", label = "Alice's iPad", guardianMarked = true),
            ),
        )
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        assertEquals(2, composeTestRule.onAllNodesWithTag("family-device-mark-item").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("family-device-mark-toggle").fetchSemanticsNodes().size)
    }

    @Test
    fun deviceMarkSectionShowsNoDevicesTextWhenWardHasNone() {
        val w = ward("Ward1", devices = emptyList())
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-device-mark-item").assertDoesNotExist()
        composeTestRule.onNodeWithText(ctx.getString(R.string.family_no_ward_devices)).assertExists()
    }

    // The toggle must be a CHILD of its own `family-device-mark-item` — the
    // e2e reads it `scope="family-device-mark-item[i]"`. A flat paint would
    // return nothing for the marked device AND nothing for the unmarked one,
    // a false pass on the negative half (exactly how tui's ward-side badge
    // shipped broken, fixed) — assert the structural nesting, not
    // just presence.
    @Test
    fun deviceMarkToggleIsScopedInsideItsOwnRow() {
        val w = ward(
            "Ward1",
            devices = listOf(FfiFamilyWardDevice(deviceId = "d1", label = "Alice's Pixel", guardianMarked = false)),
        )
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNode(
            hasTestTag("family-device-mark-item") and hasAnyDescendant(hasTestTag("family-device-mark-toggle")),
        ).assertExists()
    }

    @Test
    fun deviceMarkToggleRendersCurrentGuardianMarkedState() {
        val w = ward(
            "Ward1",
            devices = listOf(
                FfiFamilyWardDevice(deviceId = "d1", label = "Alice's Pixel", guardianMarked = false),
                FfiFamilyWardDevice(deviceId = "d2", label = "Alice's iPad", guardianMarked = true),
            ),
        )
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        val toggles = composeTestRule.onAllNodesWithTag("family-device-mark-toggle")
        toggles[0].assertIsOff()
        toggles[1].assertIsOn()
    }

    @Test
    fun deviceMarkToggleFiresWithWardActorIdDeviceIdAndFlippedState() {
        val w = ward(
            "Ward1",
            devices = listOf(FfiFamilyWardDevice(deviceId = "d1", label = "Alice's Pixel", guardianMarked = false)),
        )
        var marked: Triple<ByteArray, String, Boolean>? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onMarkDevice = { ward, deviceId, m -> marked = Triple(ward, deviceId, m) },
        )
        composeTestRule.onNodeWithTag("family-device-mark-toggle").performScrollTo().performClick()
        assertEquals(w.actorId, marked?.first)
        assertEquals("d1", marked?.second)
        assertEquals(true, marked?.third)
    }

    // ── The un-deny surface (family-safety.md § The bridge-DM gate → *The ──
    // un-deny surface*): one `family-blocked-peer-item` per denied peer of the
    // SELECTED ward, its allow button inside it. Mirrors tui's `family.rs` arms
    // and linux's `fill_blocked_peers` tests.

    @Test
    fun aWardWithNothingDeniedStatesItAndRendersNoRows() {
        val w = ward("Ward1")
        render(status = status(wards = listOf(w)), selectedWardActorId = w.actorId)
        composeTestRule.onNodeWithTag("family-blocked-peer-item").assertDoesNotExist()
        composeTestRule.onNodeWithText(ctx.getString(R.string.family_no_blocked_peers)).assertExists()
    }

    @Test
    fun eachDeniedPeerGetsARowWithItsAllowButtonInsideIt() {
        val w = ward(
            "Ward1",
            blockedDmPeers = listOf(
                FfiFamilyBlockedPeer(bridgeId = "nostr", peerId = "npub1aaa"),
                FfiFamilyBlockedPeer(bridgeId = "nostr", peerId = "npub1bbb"),
            ),
        )
        render(status = status(wards = listOf(w)), selectedWardActorId = w.actorId)
        assertEquals(2, composeTestRule.onAllNodesWithTag("family-blocked-peer-item").fetchSemanticsNodes().size)
        // Every denial is reversible from within its OWN row (the e2e reads the
        // button scoped to `family-blocked-peer-item[i]`), and the row's text
        // is the peer id.
        for (peerId in listOf("npub1aaa", "npub1bbb")) {
            composeTestRule.onNode(
                hasTestTag("family-blocked-peer-item") and
                    hasAnyDescendant(hasText(peerId)) and
                    hasAnyDescendant(hasTestTag("family-blocked-peer-allow-button")),
            ).assertExists()
        }
        composeTestRule.onNodeWithText(ctx.getString(R.string.family_no_blocked_peers)).assertDoesNotExist()
    }

    /**
     * ⚠ Rule (f), the assertion this surface exists to get right: each allow
     * button un-denies ITS OWN row's `(bridge_id, peer_id)`, as a `dm_hold`
     * approve for THIS ward. Pointing every button at row 0 would un-deny the
     * wrong person while the surface still looked correct — so the rows here
     * differ in BOTH keys, and each press is checked against its own row.
     */
    @Test
    fun eachAllowButtonUndeniesItsOwnRowsPeer() {
        val wardId = ByteArray(32) { 7 }
        val w = ward(
            "Ward1",
            actorId = wardId,
            blockedDmPeers = listOf(
                FfiFamilyBlockedPeer(bridgeId = "nostr", peerId = "npub1aaa"),
                FfiFamilyBlockedPeer(bridgeId = "activitypub", peerId = "@bob@example.test"),
            ),
        )
        val allowed = mutableListOf<UndenyDecide>()
        render(
            status = status(wards = listOf(w)),
            selectedWardActorId = w.actorId,
            onAllowBlockedPeer = { allowed += it },
        )
        val buttons = composeTestRule.onAllNodesWithTag("family-blocked-peer-allow-button")
        buttons[1].performScrollTo().performClick()
        buttons[0].performScrollTo().performClick()
        assertEquals(
            listOf(
                UndenyDecide(wardId, FfiFamilyBlockedPeer(bridgeId = "activitypub", peerId = "@bob@example.test")),
                UndenyDecide(wardId, FfiFamilyBlockedPeer(bridgeId = "nostr", peerId = "npub1aaa")),
            ),
            allowed,
        )
    }

    // ── Contact-add ──

    @Test
    fun contactAddWithValidHexFiresCallback() {
        val w = ward("Ward1")
        var added: Pair<ByteArray, ByteArray>? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onAddContact = { supervised, peer -> added = supervised to peer },
        )
        composeTestRule.onNodeWithTag("family-contact-add-input").performTextInput("aabbcc")
        composeTestRule.onNodeWithTag("family-contact-add-button").performScrollTo().performClick()
        assertEquals(listOf<Byte>(0xaa.toByte(), 0xbb.toByte(), 0xcc.toByte()), added?.second?.toList())
    }

    @Test
    fun contactAddWithInvalidHexShowsErrorAndDoesNotFire() {
        val w = ward("Ward1")
        var added: Pair<ByteArray, ByteArray>? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onAddContact = { supervised, peer -> added = supervised to peer },
        )
        composeTestRule.onNodeWithTag("family-contact-add-input").performTextInput("not hex zz")
        composeTestRule.onNodeWithTag("family-contact-add-button").performScrollTo().performClick()
        assertNull(added)
        composeTestRule.onNodeWithText(ctx.getString(R.string.family_contact_add_invalid_actor_id)).assertExists()
    }

    // ── Transfer ──

    @Test
    fun transferInputShownWhenNoPendingProposal() {
        val w = ward("Ward1", pendingTransfer = null)
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-transfer-input").assertExists()
        composeTestRule.onNodeWithTag("family-transfer-button").assertExists()
        composeTestRule.onNodeWithTag("family-transfer-pending").assertDoesNotExist()
    }

    @Test
    fun transferPendingShownInsteadOfInput() {
        val pending = FfiFamilyPendingTransfer(
            proposedGuardianActorId = ByteArray(32) { 7 },
            proposedGuardianHandle = "NewGuardian",
            createdAt = 0,
        )
        val w = ward("Ward1", pendingTransfer = pending)
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-transfer-input").assertDoesNotExist()
        composeTestRule.onNodeWithTag("family-transfer-pending")
            .assertTextEquals(ctx.getStringFmt(R.string.family_transfer_pending, "NewGuardian"))
        composeTestRule.onNodeWithTag("family-transfer-cancel-button").assertExists()
    }

    @Test
    fun cancelTransferFiresWithWardActorId() {
        val pending = FfiFamilyPendingTransfer(
            proposedGuardianActorId = ByteArray(32) { 7 },
            proposedGuardianHandle = "NewGuardian",
            createdAt = 0,
        )
        val w = ward("Ward1", pendingTransfer = pending)
        var cancelled: ByteArray? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onCancelTransfer = { cancelled = it },
        )
        composeTestRule.onNodeWithTag("family-transfer-cancel-button").performScrollTo().performClick()
        assertEquals(w.actorId, cancelled)
    }

    // ── Graduate (reveal-then-confirm) ──

    @Test
    fun graduateRevealsConfirmDialogWithWardHandle() {
        val w = ward("Ward1")
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
        )
        composeTestRule.onNodeWithTag("family-graduate-confirm-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("family-graduate-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("family-graduate-confirm-button")
            .assertTextEquals(ctx.getStringFmt(R.string.family_graduate_confirm_button, "Ward1"))
    }

    @Test
    fun graduateConfirmFiresWithWardActorId() {
        val w = ward("Ward1")
        var graduated: ByteArray? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = listOf(w), incomingTransfers = emptyList(), usageTodayMinutes = null),
            selectedWardActorId = w.actorId,
            onGraduate = { graduated = it },
        )
        composeTestRule.onNodeWithTag("family-graduate-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("family-graduate-confirm-button").performClick()
        assertEquals(w.actorId, graduated)
        composeTestRule.onNodeWithTag("family-graduate-confirm-button").assertDoesNotExist()
    }

    // ── Incoming-transfer prompt (any user can be a proposed guardian) ──

    @Test
    fun incomingTransferPromptRendersAndAcceptFires() {
        val incoming = FfiFamilyIncomingTransfer(
            supervisedActorId = ByteArray(32) { 8 },
            supervisedHandle = "Ward9",
            guardianHandle = "CurrentGuardian",
            createdAt = 0,
        )
        var accepted: ByteArray? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = emptyList(), incomingTransfers = listOf(incoming), usageTodayMinutes = null),
            onAcceptIncomingTransfer = { accepted = it },
        )
        composeTestRule.onNodeWithTag("family-incoming-transfer-item").assertExists()
        composeTestRule.onNodeWithTag("family-incoming-transfer-accept-button").performScrollTo().performClick()
        assertEquals(incoming.supervisedActorId, accepted)
    }

    @Test
    fun incomingTransferDeclineFiresWithSupervisedActorId() {
        val incoming = FfiFamilyIncomingTransfer(
            supervisedActorId = ByteArray(32) { 8 },
            supervisedHandle = "Ward9",
            guardianHandle = "CurrentGuardian",
            createdAt = 0,
        )
        var declined: ByteArray? = null
        render(
            status = status(supervisedBy = null, policy = null, wards = emptyList(), incomingTransfers = listOf(incoming), usageTodayMinutes = null),
            onDeclineIncomingTransfer = { declined = it },
        )
        composeTestRule.onNodeWithTag("family-incoming-transfer-decline-button").performScrollTo().performClick()
        assertEquals(incoming.supervisedActorId, declined)
    }
}
