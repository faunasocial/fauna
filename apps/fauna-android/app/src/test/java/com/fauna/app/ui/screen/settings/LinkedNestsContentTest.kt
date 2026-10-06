package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.ui.util.formatNamed
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_capabilities.CustodyHolderRowView
import uniffi.fauna_client_capabilities.CustodyReceiptRowView
import uniffi.fauna_client_capabilities.CustodyReceiptStateView
import uniffi.fauna_client_pair.ForwardQueueStatus
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_client_pair.LinkedNestRow
import uniffi.fauna_client_pair.LinkedNestStatus
import uniffi.fauna_client_pair.LinkedNestsSnapshot
import uniffi.fauna_client_pair.TrustBackupKind
import uniffi.fauna_client_pair.TrustBackupRow
import uniffi.fauna_client_pair.TrustBackupStatus
import uniffi.fauna_client_pair.TrustEventKind
import uniffi.fauna_client_pair.TrustFolder
import uniffi.fauna_client_pair.TrustGenerationRow
import uniffi.fauna_client_pair.TrustGenerationStatus
import uniffi.fauna_client_pair.TrustGrantDuration
import uniffi.fauna_client_pair.TrustGrantRow
import uniffi.fauna_client_pair.TrustHistoryRow
import uniffi.fauna_client_pair.TrustLens
import uniffi.fauna_client_pair.TrustLiveness
import uniffi.fauna_client_pair.TrustMintOption
import uniffi.fauna_client_pair.TrustMintUseCase
import uniffi.fauna_client_pair.TrustRestoreOutcome
import uniffi.fauna_client_pair.TrustScope

/**
 * Compose-level coverage for the stateless [LinkedNestsContent] (the "Nests"
 * settings page, docs/goal/ui/nests.md § Trust facet): the home-row-first nest
 * list, the identity line (pairings only — no unlink/caps/expiry on the home
 * row), and the net-new nest-trust facet (per-row Now/History lens, grant list
 * + per-grant renew/revoke, history list, the `nest-trust-empty` state, and the
 * home-row backup trust rows with their two separately-routed revokes).
 * Renders with hand-built `LinkedNestsSnapshot`/`LinkedNestRow` fixtures — no
 * Hilt, no VM, no FFI native calls (the `shortId` formatter is injected as a
 * stub, mirroring `AdminNestContentTest` / `RestoreHistoryContentTest`). The
 * cross-app `test_nest_trust.py` / `test_linked_nests.py --client android`
 * are the standing gate once the host emulator lands.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], qualifiers = "w400dp-h2400dp")
class LinkedNestsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val ctx get() = ApplicationProvider.getApplicationContext<android.content.Context>()

    /** The localized "Serve paywalled posts — ‹tier›" option label the scope
     *  select renders (same formatNamed path the shell uses). */
    private fun paywalledLabel(tier: String) =
        formatNamed(ctx.getString(R.string.nests_mint_option_paywalled), listOf(tier))

    /** A per-tier paywalled-posts mint option with exactly one holder candidate
     *  (the production shape: web-serve derives one holder → no holder select). */
    private fun paywalledOption(tier: String = "gold", holder: String = "web-serve") = TrustMintOption(
        useCase = TrustMintUseCase.PAYWALLED_POSTS,
        tier = tier,
        scope = listOf(TrustScope(`class` = "content.read", kind = "post", tier = tier)),
        holderCandidates = listOf(holder),
    )

    private fun homeRow(
        trustGrants: List<TrustGrantRow> = emptyList(),
        trustHistory: List<TrustHistoryRow> = emptyList(),
        lens: TrustLens = TrustLens.NOW,
        mintOptions: List<TrustMintOption> = emptyList(),
        trustBackups: List<TrustBackupRow> = emptyList(),
        trustGenerations: List<TrustGenerationRow> = emptyList(),
        blessed: Boolean = false,
        mintDefaultDuration: TrustGrantDuration = TrustGrantDuration.ONE_OFF,
    ) = LinkedNestRow(
        nestId = "aa".repeat(32),
        capabilities = emptyList(),
        expiresAt = null,
        createdAt = 0L,
        label = null,
        nestUrl = null,
        isHome = true,
        trustGrants = trustGrants,
        trustHistory = trustHistory,
        lens = lens,
        availableHolders = emptyList(),
        mintOptions = mintOptions,
        blessed = blessed,
        mintDefaultDuration = mintDefaultDuration,
        trustBackups = trustBackups,
        trustGenerations = trustGenerations,
    )

    private fun pairingRow(
        capabilities: List<String> = listOf("mls_pull", "namespace_sync"),
        expiresAt: Long? = null,
    ) = LinkedNestRow(
        nestId = "bb".repeat(32),
        capabilities = capabilities,
        expiresAt = expiresAt,
        createdAt = 0L,
        label = "My other nest",
        nestUrl = null,
        isHome = false,
        trustGrants = emptyList(),
        trustHistory = emptyList(),
        lens = TrustLens.NOW,
        availableHolders = emptyList(),
        mintOptions = emptyList(), // android mint UI is a follow-on lift (linux leads)
        mintDefaultDuration = TrustGrantDuration.ONE_OFF,
        // Both backup grants empower the SOURCE nest, so a pairing row never
        // carries one (nests.md § Trust facet — backup rows).
        trustBackups = emptyList(),
        // Same reasoning: recovery is addressed to the OWNER's backup
        // destinations, which hang off the home row, never off a pairing.
        trustGenerations = emptyList(),
    )

    private fun grant(
        grantId: ByteArray = byteArrayOf(1, 2, 3),
        liveness: TrustLiveness = TrustLiveness.ACTIVE,
        unattested: Boolean = false,
    ) = TrustGrantRow(
        grantId = grantId,
        holder = byteArrayOf(9, 9, 9),
        scope = listOf(TrustScope(`class` = "content.read", kind = "mail", tier = null)),
        lastsUntil = 4_102_444_800L, // 2100-01-01, far future — status assertions don't depend on "now"
        liveness = liveness,
        // The post-succession review mark (succession-aftermath.md § Adjudicating
        // what the aftermath carries across). Defaults false — android does not
        // render the pair yet; the parity lift is tracked.
        unattested = unattested,
    )

    /** The owner→source-nest seal grant row: no destination, and no `since` —
     *  that grant carries no timestamp on the wire (`nests.md:67`). */
    private fun sealBackup(status: TrustBackupStatus = TrustBackupStatus.ACTIVE) = TrustBackupRow(
        kind = TrustBackupKind.SEAL,
        status = status,
        destinationId = "",
        destinationLabel = "",
        since = null,
    )

    /** One destination's writer-grant row — the row whose revoke is spoken to the
     *  DESTINATION, which is what keeps it operable with the source nest hostile. */
    private fun writerBackup(
        destinationId: String = "d1",
        destinationLabel: String = "Aunt's nest",
        status: TrustBackupStatus = TrustBackupStatus.ACTIVE,
        since: Long? = 1_700_000_000L,
    ) = TrustBackupRow(
        kind = TrustBackupKind.WRITER,
        status = status,
        destinationId = destinationId,
        destinationLabel = destinationLabel,
        since = since,
    )

    private companion object {
        /** The folder a web-serve paywall grant covers, as the shared row names it. */
        val PREMIUM = TrustFolder.Named(name = "premium")
    }

    private fun historyRow(kind: TrustEventKind = TrustEventKind.MINT) = TrustHistoryRow(
        grantId = byteArrayOf(1, 2, 3),
        holder = byteArrayOf(9, 9, 9),
        kind = kind,
        scope = listOf(TrustScope(`class` = "content.read", kind = "mail", tier = null)),
        windowStart = 0L,
        windowEnd = 4_102_444_800L,
        at = 0L,
    )

    private fun snapshot(
        home: LinkedNestRow? = null,
        pairings: List<LinkedNestRow> = emptyList(),
        status: LinkedNestStatus = LinkedNestStatus.IDLE,
        error: String? = null,
        restoreOutcome: TrustRestoreOutcome? = null,
        forwardQueue: ForwardQueueStatus? = null,
    ) = LinkedNestsSnapshot(
        home = home,
        pairings = pairings,
        status = status,
        error = error,
        restoreOutcome = restoreOutcome,
        forwardQueue = forwardQueue,
    )

    private fun render(
        snap: LinkedNestsSnapshot,
        onLink: (String) -> Unit = {},
        onUnlink: (String) -> Unit = {},
        onSetLens: (String, TrustLens) -> Unit = { _, _ -> },
        onRenew: (ByteArray) -> Unit = {},
        onRevoke: (ByteArray) -> Unit = {},
        onMint: (String, String, List<TrustScope>, TrustGrantDuration) -> Unit = { _, _, _, _ -> },
        onSetBlessed: (String, Boolean) -> Unit = { _, _ -> },
        onRevokeBackupSeal: () -> Unit = {},
        onRevokeBackupWriter: (String) -> Unit = {},
        onRestoreGeneration: (String, String, String, String) -> Unit = { _, _, _, _ -> },
        onRetryForwards: () -> Unit = {},
        onDiscardForwards: () -> Unit = {},
        custodyNestRows: List<CustodyHolderRowView> = emptyList(),
        escrowHolders: Set<String> = emptySet(),
        onRevokeCustody: (ByteArray, ByteArray?) -> Unit = { _, _ -> },
    ) {
        composeTestRule.setContent {
            LinkedNestsContent(
                snapshot = snap,
                onBack = {},
                onLink = onLink,
                onUnlink = onUnlink,
                onSetLens = onSetLens,
                onRenew = onRenew,
                onRevoke = onRevoke,
                onMint = onMint,
                onSetBlessed = onSetBlessed,
                durationOptions = { listOf(TrustGrantDuration.ONE_OFF, TrustGrantDuration.STANDARD) },
                durationLabel = { LocalizedText("DUR:${it.name}", emptyMap()) },
                onRevokeBackupSeal = onRevokeBackupSeal,
                onRevokeBackupWriter = onRevokeBackupWriter,
                onRestoreGeneration = onRestoreGeneration,
                onRetryForwards = onRetryForwards,
                onDiscardForwards = onDiscardForwards,
                shortId = { it.take(8) },
                custodyNestRows = custodyNestRows,
                escrowHolders = escrowHolders,
                onRevokeCustody = onRevokeCustody,
                receiptStatusLine = { "STATUS:${it.statusLabel.key}" },
                heldBytesLine = { "HELD:${it.heldBytes}" },
            )
        }
    }

    @Test
    fun rendersPageChromeAndAddForm() {
        render(snapshot())
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("nests-add-button").assertExists()
        composeTestRule.onNodeWithTag("nests-add-input").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nests-add-button").performClick()
        composeTestRule.onNodeWithTag("nests-add-input").assertExists()
        composeTestRule.onNodeWithTag("nests-add-submit-button").assertExists()
        composeTestRule.onNodeWithTag("nests-add-cancel-button").assertExists()
    }

    @Test
    fun addSubmitFiresLinkWithEnteredValue() {
        var linked: String? = null
        render(snapshot(), onLink = { linked = it })
        composeTestRule.onNodeWithTag("nests-add-button").performClick()
        composeTestRule.onNodeWithTag("nests-add-input").performTextInput("https://example.nest")
        composeTestRule.onNodeWithTag("nests-add-submit-button").performClick()
        assertEquals("https://example.nest", linked)
    }

    // ── Forward queue (nests.md § Forward queue) ─────────────────────────

    @Test
    fun anUnreportedForwardQueuePaintsNoBlock() {
        render(snapshot(home = homeRow()))
        composeTestRule.onNodeWithTag("nests-forward-queue").assertDoesNotExist()
    }

    @Test
    fun anEmptyForwardQueuePaintsNoBlock() {
        render(snapshot(home = homeRow(), forwardQueue = ForwardQueueStatus(0uL, 0uL, "stale")))
        composeTestRule.onNodeWithTag("nests-forward-queue").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nests-forward-retry-button").assertDoesNotExist()
    }

    @Test
    fun aWaitingQueuePaintsCountStuckHintAndActions() {
        var retried = 0
        var discarded = 0
        render(
            snapshot(home = homeRow(), forwardQueue = ForwardQueueStatus(3uL, 1uL, null)),
            onRetryForwards = { retried++ },
            onDiscardForwards = { discarded++ },
        )
        val summary = composeTestRule.onNodeWithTag("nests-forward-queue")
            .fetchSemanticsNode().config[androidx.compose.ui.semantics.SemanticsProperties.Text]
            .joinToString("") { it.text }
        assertTrue(summary, summary.startsWith("3 ") && summary.contains("1 of them"))
        // No reason line until a send has failed — absent, never empty.
        composeTestRule.onNodeWithTag("nests-forward-queue-reason").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nests-forward-retry-button").performClick()
        composeTestRule.onNodeWithTag("nests-forward-discard-button").performClick()
        assertEquals(1, retried)
        assertEquals(1, discarded)
    }

    /** The reason is relay-chosen text (`private-mode.md` § Post Forwarding).
     *  The shared projection strips its control characters (pinned in
     *  `fauna-client-pair`); what the shell owes is that markup stays text —
     *  a plain `Text(String)`, so `<b>x</b>` is shown literally. */
    @Test
    fun aHostileReasonRendersInert() {
        render(
            snapshot(
                home = homeRow(),
                forwardQueue = ForwardQueueStatus(1uL, 0uL, "[31m<b>x</b>forbidden"),
            ),
        )
        val reason = composeTestRule.onNodeWithTag("nests-forward-queue-reason")
            .fetchSemanticsNode().config[androidx.compose.ui.semantics.SemanticsProperties.Text]
            .joinToString("") { it.text }
        assertTrue(reason, reason.endsWith("[31m<b>x</b>forbidden"))
    }

    @Test
    fun emptyStateShowsWhenNoRows() {
        render(snapshot())
        composeTestRule.onNodeWithTag("nests-item").assertDoesNotExist()
    }

    @Test
    fun homeRowRendersFirstWithNoUnlinkCapsOrExpiry() {
        render(snapshot(home = homeRow(), pairings = listOf(pairingRow())))
        composeTestRule.onAllNodesWithTag("nests-item").assertCountEquals(2)
        // The home row (first) carries no unlink/capabilities/expiry.
        composeTestRule.onAllNodesWithTag("nests-item-unlink-button").assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("nests-item-capabilities").assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("nests-item-expiry").assertCountEquals(1)
    }

    @Test
    fun unlinkButtonFiresWithRowNestId() {
        var unlinked: String? = null
        val pairing = pairingRow()
        render(snapshot(pairings = listOf(pairing)), onUnlink = { unlinked = it })
        composeTestRule.onNodeWithTag("nests-item-unlink-button").performClick()
        assertEquals(pairing.nestId, unlinked)
    }

    @Test
    fun trustFacetShowsEmptyStateWhenNoGrants() {
        render(snapshot(home = homeRow(trustGrants = emptyList())))
        composeTestRule.onNodeWithTag("nest-trust-view-now").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-view-history").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-empty").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-grant-list").assertDoesNotExist()
    }

    @Test
    fun trustFacetRendersGrantItemFields() {
        render(snapshot(home = homeRow(trustGrants = listOf(grant()))))
        composeTestRule.onNodeWithTag("nest-trust-grant-list").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-grant-item").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-grant-scope").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-grant-lasts-until").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-grant-status").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-grant-bound-note").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-grant-renew").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-grant-revoke").assertExists()
    }

    @Test
    fun renewAndRevokeFireWithGrantId() {
        var renewed: ByteArray? = null
        var revoked: ByteArray? = null
        val g = grant(grantId = byteArrayOf(7, 8, 9))
        render(
            snapshot(home = homeRow(trustGrants = listOf(g))),
            onRenew = { renewed = it },
            onRevoke = { revoked = it },
        )
        composeTestRule.onNodeWithTag("nest-trust-grant-renew").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("nest-trust-grant-revoke").performScrollTo().performClick()
        assertEquals(listOf<Byte>(7, 8, 9), renewed?.toList())
        assertEquals(listOf<Byte>(7, 8, 9), revoked?.toList())
    }

    @Test
    fun lensButtonsFireSetLensWithRowNestId() {
        val home = homeRow()
        var got: Pair<String, TrustLens>? = null
        render(snapshot(home = home), onSetLens = { id, lens -> got = id to lens })
        composeTestRule.onNodeWithTag("nest-trust-view-history").performClick()
        assertEquals(home.nestId to TrustLens.HISTORY, got)
    }

    @Test
    fun historyLensRendersHistoryList() {
        render(
            snapshot(
                home = homeRow(
                    trustHistory = listOf(historyRow(TrustEventKind.MINT), historyRow(TrustEventKind.REVOKE)),
                    lens = TrustLens.HISTORY,
                ),
            ),
        )
        composeTestRule.onNodeWithTag("nest-trust-history-list").assertExists()
        composeTestRule.onAllNodesWithTag("nest-trust-history-item").assertCountEquals(2)
        composeTestRule.onNodeWithTag("nest-trust-grant-list").assertDoesNotExist()
    }

    /** The web-serve paywall grant names its folder — on the grant row and on
     *  its History `Revoke`, which carries no scope (nests.md § Trust facet —
     *  grants; the folder resolved in shared Rust, the label from the shared
     *  `grantScopeLabels`). tui's twin is
     *  `a_paywall_grant_and_its_revoke_name_the_folder`. */
    @Test
    fun aPaywallGrantNamesTheFolder() {
        val folderRead = listOf(TrustScope(`class` = "content.read", kind = "folder", tier = null))
        render(
            snapshot(
                home = homeRow(
                    trustGrants = listOf(grant().copy(scope = folderRead, folder = PREMIUM)),
                ),
            ),
        )
        val scopeText = composeTestRule.onNodeWithTag("nest-trust-grant-scope")
            .fetchSemanticsNode().config[androidx.compose.ui.semantics.SemanticsProperties.Text]
            .joinToString("") { it.text }
        assertEquals("Trusted to read: Your folder \"premium\"", scopeText)
    }

    /** The paywall grant's History `Revoke` carries no scope; it is named by
     *  the folder its grant covered. */
    @Test
    fun aPaywallGrantsRevokeNamesTheFolder() {
        val revoke = historyRow(TrustEventKind.REVOKE).copy(scope = emptyList(), folder = PREMIUM)
        render(snapshot(home = homeRow(trustHistory = listOf(revoke), lens = TrustLens.HISTORY)))
        val line = composeTestRule.onNodeWithTag("nest-trust-history-item")
            .fetchSemanticsNode().config[androidx.compose.ui.semantics.SemanticsProperties.Text]
            .joinToString("") { it.text }
        assertTrue(line, line.startsWith("Trust revoked: Your folder \"premium\" · "))
    }

    // ── Backup trust rows (nests.md § Trust facet — backup rows, ratified
    //    2026-07-24; linux + web lead) ──

    @Test
    fun backupRowsRenderAllLeavesForBothKinds() {
        render(snapshot(home = homeRow(trustBackups = listOf(sealBackup(), writerBackup()))))
        composeTestRule.onAllNodesWithTag("nest-trust-backup-item").assertCountEquals(2)
        // Every row carries the same leaf set regardless of kind — the seal row's
        // `since` is empty, not absent (`nests.md:67`).
        composeTestRule.onAllNodesWithTag("nest-trust-backup-scope").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("nest-trust-backup-since").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("nest-trust-backup-status").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("nest-trust-backup-bound-note").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("nest-trust-backup-revoke").assertCountEquals(2)
    }

    @Test
    fun sealRowRendersEmptySinceAndWriterRowNamesItsDestination() {
        render(snapshot(home = homeRow(trustBackups = listOf(sealBackup(), writerBackup()))))
        // Seal row: scope copy is the standing seal wording, `since` blank.
        composeTestRule.onAllNodesWithTag("nest-trust-backup-scope")[0]
            .assertTextEquals(ctx.getString(R.string.nests_backup_scope_seal))
        composeTestRule.onAllNodesWithTag("nest-trust-backup-since")[0].assertTextEquals("")
        // Writer row: scope copy names WHICH destination, `since` is populated.
        composeTestRule.onAllNodesWithTag("nest-trust-backup-scope")[1].assertTextEquals(
            formatNamed(ctx.getString(R.string.nests_backup_scope_writer), listOf("Aunt's nest")),
        )
        composeTestRule.onAllNodesWithTag("nest-trust-backup-since")[1]
            .assertTextContains(ctx.getString(R.string.nests_backup_since), substring = true)
    }

    @Test
    fun backupRowSuppressesTrustEmptyWithZeroContentGrants() {
        // `nest-trust-empty` claims the nest is trusted with NOTHING, so a backup
        // row suppresses it even with no content grants (`nests.md:99`) — rendering
        // "not trusted to read anything" above "Backs up your messages for you"
        // states the opposite of the row beneath it.
        render(snapshot(home = homeRow(trustGrants = emptyList(), trustBackups = listOf(sealBackup()))))
        composeTestRule.onNodeWithTag("nest-trust-empty").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nest-trust-backup-item").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-grant-list").assertDoesNotExist()
    }

    @Test
    fun unreachableStaysDistinctFromMissing() {
        // Collapsing these would let a flaky network read as a revoked backup.
        render(
            snapshot(
                home = homeRow(
                    trustBackups = listOf(
                        writerBackup(destinationId = "d1", status = TrustBackupStatus.UNREACHABLE),
                        writerBackup(destinationId = "d2", status = TrustBackupStatus.MISSING),
                    ),
                ),
            ),
        )
        composeTestRule.onAllNodesWithTag("nest-trust-backup-status")[0]
            .assertTextEquals(ctx.getString(R.string.nests_backup_status_unreachable))
        composeTestRule.onAllNodesWithTag("nest-trust-backup-status")[1]
            .assertTextEquals(ctx.getString(R.string.nests_backup_status_missing))
    }

    @Test
    fun backupRevokeFiresSealAndWriterActionsSeparately() {
        var sealRevoked = false
        var writerRevoked: String? = null
        render(
            snapshot(home = homeRow(trustBackups = listOf(sealBackup(), writerBackup(destinationId = "d7")))),
            onRevokeBackupSeal = { sealRevoked = true },
            onRevokeBackupWriter = { writerRevoked = it },
        )
        composeTestRule.onAllNodesWithTag("nest-trust-backup-revoke")[0].performScrollTo().performClick()
        composeTestRule.onAllNodesWithTag("nest-trust-backup-revoke")[1].performScrollTo().performClick()
        assertTrue(sealRevoked)
        // The writer press names its destination — that id is what routes the
        // revoke to the DESTINATION's own connection rather than the source nest.
        assertEquals("d7", writerRevoked)
    }

    // ── Mint flow (nests.md § Mint, ratified 2026-07-13; web + linux lead) ──

    @Test
    fun mintButtonAbsentWhenCatalogEmpty() {
        // Empty catalog ⇒ nothing derivable / no discoverable holder ⇒ no picker.
        render(snapshot(home = homeRow(mintOptions = emptyList())))
        composeTestRule.onNodeWithTag("nest-trust-grant-mint-button").assertDoesNotExist()
    }

    @Test
    fun mintButtonPresentWhenCatalogNonEmptyAndFormHiddenUntilClicked() {
        render(snapshot(home = homeRow(mintOptions = listOf(paywalledOption()))))
        composeTestRule.onNodeWithTag("nest-trust-grant-mint-button").assertExists()
        // The scope select isn't in the tree until the mint button reveals it.
        composeTestRule.onNodeWithTag("nest-trust-mint-scope-select").assertDoesNotExist()
    }

    @Test
    fun mintFlowSelectsScopeAndConfirmsWithDerivedHolder() {
        var mintedHolder: String? = null
        var mintedScope: List<TrustScope>? = null
        render(
            snapshot(home = homeRow(mintOptions = listOf(paywalledOption(tier = "gold", holder = "web-serve")))),
            onMint = { _, holder, scope, _ -> mintedHolder = holder; mintedScope = scope },
        )
        // Reveal the form, open the scope select, pick the per-tier paywalled option.
        composeTestRule.onNodeWithTag("nest-trust-grant-mint-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("nest-trust-mint-scope-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText(paywalledLabel("gold")).performClick()
        // Single holder candidate ⇒ the holder is derived, no holder select renders.
        composeTestRule.onNodeWithTag("nest-trust-mint-holder-select").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nest-trust-mint-confirm-button").performScrollTo().performClick()
        // Mint fires with the derived holder + the option's tier-scoped content.
        assertEquals("web-serve", mintedHolder)
        assertEquals(1, mintedScope?.size)
        assertEquals("post", mintedScope?.first()?.kind)
        assertEquals("gold", mintedScope?.first()?.tier)
    }

    // ── Retained backup generations (nests.md § Trust facet — generation
    //    recovery, ratified 2026-07-29; linux + tui lead) ──

    private fun generationRow(
        status: TrustGenerationStatus = TrustGenerationStatus.LISTED,
        path: String? = "/Mail/2026",
        destinationId: String = "dest-1",
        destinationLabel: String = "Recovery",
    ) = TrustGenerationRow(
        status = status,
        destinationId = destinationId,
        destinationLabel = destinationLabel,
        folderName = "__mail",
        path = path,
        pathHash = "aa11",
        manifestHash = "mm22",
        sizeBytes = 2048,
        supersededAt = 1_700_000_000L,
        expiresAt = 1_702_592_000L,
    )

    @Test
    fun generationRowRendersAllFieldsForANormalGeneration() {
        render(snapshot(home = homeRow(trustGenerations = listOf(generationRow()))))
        composeTestRule.onNodeWithTag("nest-trust-generation-item").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-generation-path").assertTextEquals(
            formatNamed(ctx.getString(R.string.nests_generation_path), listOf("/Mail/2026")),
        )
        composeTestRule.onNodeWithTag("nest-trust-generation-superseded").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-generation-expires").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-generation-size").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-generation-status")
            .assertTextEquals(ctx.getString(R.string.nests_generation_status_listed))
        composeTestRule.onNodeWithTag("nest-trust-generation-restore").assertExists()
    }

    // Invariant 2 (nests.md:144): a path-less row (a sealed-path or
    // rogue-source custody row) must still render — using the hash, never
    // hidden or skipped, since the rows a rogue source produced are exactly
    // the ones a user needs to see.
    @Test
    fun pathLessGenerationRowStillRendersUsingTheHashNotHidden() {
        render(snapshot(home = homeRow(trustGenerations = listOf(generationRow(path = null)))))
        composeTestRule.onNodeWithTag("nest-trust-generation-item").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-generation-path").assertTextEquals(
            formatNamed(ctx.getString(R.string.nests_generation_path_unknown), listOf("aa11")),
        )
    }

    // Invariant 1 (nests.md:143): a failed/unreachable read is not an empty
    // one — the row itself renders `status: unreachable` with NO restore
    // affordance. Exercised via the row's OWN status, not an empty list.
    @Test
    fun unreachableGenerationRowRendersNoRestoreAffordance() {
        render(
            snapshot(
                home = homeRow(
                    trustGenerations = listOf(
                        generationRow(status = TrustGenerationStatus.UNREACHABLE, path = null),
                    ),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("nest-trust-generation-item").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-generation-restore").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nest-trust-generation-status")
            .assertTextEquals(ctx.getString(R.string.nests_generation_status_unreachable))
        // The identity leaf names the DESTINATION that went dark, not a
        // generation that does not exist.
        composeTestRule.onNodeWithTag("nest-trust-generation-path").assertTextEquals("Recovery")
        // The value leaves render EMPTY, never a fake zero/size that would
        // read as fact.
        composeTestRule.onNodeWithTag("nest-trust-generation-superseded").assertTextEquals("")
        composeTestRule.onNodeWithTag("nest-trust-generation-expires").assertTextEquals("")
        composeTestRule.onNodeWithTag("nest-trust-generation-size").assertTextEquals("")
    }

    @Test
    fun restoreFiresWithTheRowsAddressTripleNeverAnIndex() {
        var restored: List<String>? = null
        render(
            snapshot(home = homeRow(trustGenerations = listOf(generationRow(destinationId = "d9")))),
            onRestoreGeneration = { d, f, p, m -> restored = listOf(d, f, p, m) },
        )
        composeTestRule.onNodeWithTag("nest-trust-generation-restore").performScrollTo().performClick()
        assertEquals(listOf("d9", "__mail", "aa11", "mm22"), restored)
    }

    // ── Restore-outcome notice (`nest-trust-generation-notice`, ratified
    //    2026-07-29) ──

    @Test
    fun generationNoticeRendersEmptyByDefault() {
        // Registered whenever the home row's Now lens renders, EMPTY until a
        // restore resolves — even with no generation rows present at all.
        render(snapshot(home = homeRow()))
        composeTestRule.onNodeWithTag("nest-trust-generation-notice").assertTextEquals("")
    }

    @Test
    fun generationNoticeRendersTheRestoredOutcome() {
        render(snapshot(home = homeRow(), restoreOutcome = TrustRestoreOutcome.RESTORED))
        composeTestRule.onNodeWithTag("nest-trust-generation-notice")
            .assertTextEquals(ctx.getString(R.string.nests_generation_restored))
    }

    @Test
    fun generationNoticeRendersPastRecoveryWindowAsAProductStateNeverAFailure() {
        render(snapshot(home = homeRow(), restoreOutcome = TrustRestoreOutcome.PAST_RECOVERY_WINDOW))
        val text = ctx.getString(R.string.nests_generation_past_window)
        composeTestRule.onNodeWithTag("nest-trust-generation-notice").assertTextEquals(text)
        assertTrue(
            "past-the-window is a product state, never a failure message (nests.md:145)",
            !text.lowercase().contains("failed"),
        )
    }

    // ── Custodian nests + the escrow-holder badge (nests.md § Trust facet —
    // custody rows; participants.md § The participant model → Roles) ──

    private fun custodyNest(pending: Boolean = false) = CustodyHolderRowView(
        grantId = ByteArray(16) { 7 },
        host = ByteArray(32) { 0xB2.toByte() },
        custodianKey = if (pending) null else ByteArray(32) { 3 },
        custodianNestUrl = "wss://friend.example",
        scopes = null,
        lastsUntil = null,
        liveness = null,
        receiptState = CustodyReceiptStateView.NO_RECEIPT_YET,
        receipt = CustodyReceiptRowView(
            statusLabel = LocalizedText("devices.custody_receipt_none", emptyMap()),
            attestedAtSecs = null,
            heldBytesLabel = LocalizedText("devices.custody_held_bytes", emptyMap()),
            held = LocalizedText("", emptyMap()),
            cap = LocalizedText("", emptyMap()),
            degraded = false,
            heldBytes = 0uL,
            attestedCap = 0uL,
        ),
        pending = pending,
    )

    /** A nest-anchored custody renders as its own `nests-item` after the linked
     *  rows, carrying the full `nest-trust-custody-*` family; its revoke names
     *  the grant id + custodian key, never an index (tui's
     *  `push_custody_nest_item`). */
    @Test
    fun aCustodianNestRendersTheCustodyFamilyAndRevokesByGrant() {
        var revoked: Pair<ByteArray, ByteArray?>? = null
        render(
            snapshot(home = homeRow()),
            custodyNestRows = listOf(custodyNest()),
            onRevokeCustody = { g, h -> revoked = g to h },
        )
        composeTestRule.onAllNodesWithTag("nests-item").assertCountEquals(2)
        composeTestRule.onNodeWithTag("nest-trust-custody-item").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-custody-scope")
            .assertTextEquals(ctx.getString(R.string.devices_custody_holder_scope))
        composeTestRule.onNodeWithTag("nest-trust-custody-receipt-status")
            .assertTextEquals("STATUS:devices.custody_receipt_none")
        composeTestRule.onNodeWithTag("nest-trust-custody-held-bytes").assertTextEquals("HELD:0")
        composeTestRule.onNodeWithTag("nest-trust-custody-revoke-button")
            .performScrollTo().assertIsEnabled().performClick()
        assertTrue(revoked!!.first.contentEquals(ByteArray(16) { 7 }))
        assertTrue(revoked!!.second!!.contentEquals(ByteArray(32) { 3 }))
    }

    /** A pending ceremony has minted nothing to revoke — the control is off. */
    @Test
    fun aPendingCustodianNestCannotBeRevoked() {
        render(snapshot(), custodyNestRows = listOf(custodyNest(pending = true)))
        composeTestRule.onNodeWithTag("nests-item").assertExists()
        composeTestRule.onNodeWithTag("nest-trust-custody-revoke-button")
            .performScrollTo().assertIsNotEnabled()
    }

    /** The escrow-holder badge renders on exactly the row whose identity holds
     *  escrow — derived, never nest-asserted. */
    @Test
    fun escrowHolderBadgeRendersOnTheMatchingRowOnly() {
        render(snapshot(home = homeRow()), escrowHolders = setOf("aa".repeat(32)))
        composeTestRule.onNodeWithTag("participant-escrow-holder-badge")
            .assertTextEquals(ctx.getString(R.string.nests_escrow_holder_badge))
    }

    @Test
    fun noEscrowBadgeWhenTheRowHoldsNone() {
        render(snapshot(home = homeRow()), escrowHolders = setOf("bb".repeat(32)))
        composeTestRule.onNodeWithTag("participant-escrow-holder-badge").assertDoesNotExist()
    }

    // ── Duration picker + blessing toggle (nests.md § Expiry / renewal →
    //    Duration and blessing; tui leads) ──

    /** The duration select pre-selects the row's default and sends the pick. */
    @Test
    fun mintSendsThePickedDurationDefaultingToTheRows() {
        var minted: TrustGrantDuration? = null
        render(
            snapshot(home = homeRow(mintOptions = listOf(paywalledOption(tier = "gold")))),
            onMint = { _, _, _, d -> minted = d },
        )
        composeTestRule.onNodeWithTag("nest-trust-grant-mint-button").performScrollTo().performClick()
        // The key text is unresolved in the harness, so it reads back verbatim.
        composeTestRule.onNodeWithTag("nest-trust-mint-duration-select")
            .assertTextContains("DUR:ONE_OFF")
        composeTestRule.onNodeWithTag("nest-trust-mint-scope-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText(paywalledLabel("gold")).performClick()
        composeTestRule.onNodeWithTag("nest-trust-mint-duration-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("DUR:STANDARD").performClick()
        composeTestRule.onNodeWithTag("nest-trust-mint-confirm-button").performScrollTo().performClick()
        assertEquals(TrustGrantDuration.STANDARD, minted)
    }

    /** A blessed nest's form opens on the standing window. */
    @Test
    fun aBlessedNestsMintDefaultsToTheStandingWindow() {
        render(
            snapshot(home = homeRow(
                mintOptions = listOf(paywalledOption()),
                blessed = true,
                mintDefaultDuration = TrustGrantDuration.STANDARD,
            )),
        )
        composeTestRule.onNodeWithTag("nest-trust-grant-mint-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("nest-trust-mint-duration-select")
            .assertTextContains("DUR:STANDARD")
    }

    /** The home row's blessing toggle mirrors `blessed` and dispatches the flip. */
    @Test
    fun blessedToggleMirrorsTheRowAndDispatchesTheFlip() {
        var set: Pair<String, Boolean>? = null
        render(snapshot(home = homeRow(blessed = false)), onSetBlessed = { n, b -> set = n to b })
        composeTestRule.onNodeWithTag("nest-trust-blessed-toggle").performScrollTo().assertIsOff()
            .performClick()
        assertEquals("aa".repeat(32) to true, set)
    }
}
