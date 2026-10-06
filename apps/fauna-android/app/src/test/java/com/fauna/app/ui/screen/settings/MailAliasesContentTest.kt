package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.AliasKind
import uniffi.fauna_client_mail_settings.AliasView
import uniffi.fauna_client_mail_settings.ImportAliasOutcomeView
import uniffi.fauna_client_mail_settings.ImportAliasStatusView
import uniffi.fauna_client_mail_settings.ImportResultView

/**
 * Compose-level coverage for the stateless [MailAliasesContent] (the
 * `mail-aliases` page, mail-aliases.md): the add/edit sheet, the
 * generate-disposable shortcut, and the indexed alias list with its per-row
 * controls. Renders with seeded state — no Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MailAliasesContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun alias(
        id: String = "aa",
        kind: AliasKind = AliasKind.EXACT,
        address: String = "bob@example.com",
        label: String = "Shopping",
        disabled: Boolean = false,
        isCanonical: Boolean = false,
        hitCount: ULong = 3u,
        lastHitAtMs: Long? = null,
    ) = AliasView(
        aliasIdHex = id,
        localDomain = "example.com",
        kind = kind,
        pattern = "bob",
        address = address,
        label = label,
        disabled = disabled,
        isCanonical = isCanonical,
        hitCount = hitCount,
        lastHitAtMs = lastHitAtMs,
        spamThresholdOverride = null,
        rateLimitPerHour = null,
        rateLimitPerDay = null,
        usesRemaining = null,
        expiresAtMs = null,
    )

    private fun render(
        aliases: List<AliasView> = emptyList(),
        defaultDomain: String? = "example.com",
        hydrated: Boolean = true,
        working: Boolean = false,
        // Pure stub for the FFI-backed kind badge (shared `alias_kind_badge`).
        kindLabel: (AliasKind) -> String = { it.name },
        // Pure stub for the FFI-backed hit-count text (shared `alias_hits_label`);
        // mirrors its "N hits" / "N hits · last <date>" shape so the row's hits-cell
        // wiring is exercised without the native path.
        hitsLabel: (AliasView) -> String = { a ->
            "${a.hitCount} hits" + (a.lastHitAtMs?.let { " · last today" } ?: "")
        },
        // FFI-free stubs for the shared parse_count / parse_count_i64 alias-override
        // validators (mirror the pre-lift local toUIntOrNull / toLongOrNull semantics).
        parseCount: (String) -> UInt? = { it.toUIntOrNull() },
        parseCountI64: (String) -> Long? = { it.toLongOrNull() },
        onCreate: (AliasKind, String, String, UInt?, Long?) -> Unit = { _, _, _, _, _ -> },
        onGenerateDisposable: () -> Unit = {},
        onUpdate: (String, String, String, UInt?, Long?) -> Unit = { _, _, _, _, _ -> },
        onRevoke: (String) -> Unit = {},
        onEnable: (String) -> Unit = {},
        onDelete: (String) -> Unit = {},
        lastImportResult: ImportResultView? = null,
        onImport: (List<String>) -> Unit = {},
    ) {
        composeTestRule.setContent {
            MailAliasesContent(
                aliases = aliases,
                defaultDomain = defaultDomain,
                hydrated = hydrated,
                working = working,
                lastImportResult = lastImportResult,
                kindLabel = kindLabel,
                hitsLabel = hitsLabel,
                parseCount = parseCount,
                parseCountI64 = parseCountI64,
                onBack = {},
                onCreate = onCreate,
                onGenerateDisposable = onGenerateDisposable,
                onGenerateWithParams = { _, _, _ -> },
                onUpdate = onUpdate,
                onRevoke = onRevoke,
                onEnable = onEnable,
                onDelete = onDelete,
                onImport = onImport,
            )
        }
    }

    @Test
    fun rendersHeadingAndAddControls() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("mail-aliases-add-button").assertExists()
        composeTestRule.onNodeWithTag("mail-aliases-generate-disposable-button").assertExists()
    }

    @Test
    fun addControlsDisabledWithoutDefaultDomain() {
        render(defaultDomain = null)
        composeTestRule.onNodeWithTag("mail-aliases-add-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-aliases-generate-disposable-button").assertIsNotEnabled()
    }

    @Test
    fun aliasRowsRenderWithFields() {
        render(aliases = listOf(alias(id = "aa"), alias(id = "bb", address = "bob-work@example.com")))
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-aliases-list-item").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-aliases-list-item-pattern").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-aliases-list-item-edit-button").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-aliases-list-item-revoke-button").fetchSemanticsNodes().size)
    }

    @Test
    fun hitsCellRendersSharedLabelNotBareCount() {
        // The row's hit-count cell renders the shared `alias_hits_label` text
        // ("N hits" / "N hits · last <date>"), not the bare number — mail-aliases.md
        // § Hit count. Two rows: one without a last-hit (count-only branch), one with.
        render(
            aliases = listOf(
                alias(id = "aa", hitCount = 3u, lastHitAtMs = null),
                alias(id = "bb", hitCount = 7u, lastHitAtMs = 1_700_000_000_000L),
            ),
        )
        val hits = composeTestRule.onAllNodesWithTag("mail-aliases-list-item-hits")
        assertEquals(2, hits.fetchSemanticsNodes().size)
        hits[0].assertTextEquals("3 hits")
        hits[1].assertTextEquals("7 hits · last today")
    }

    @Test
    fun addSheetOpensAndCreates() {
        var created: List<Any?>? = null
        render(onCreate = { kind, pattern, label, spam, rate -> created = listOf(kind, pattern, label, spam, rate) })
        composeTestRule.onNodeWithTag("mail-aliases-add-button").performClick()
        composeTestRule.onNodeWithTag("mail-aliases-add-sheet-pattern-input").performTextInput("alice")
        composeTestRule.onNodeWithTag("mail-aliases-add-sheet-submit-button").performScrollTo().performClick()
        assertEquals(listOf<Any?>(AliasKind.EXACT, "alice", "", null, null), created)
    }

    @Test
    fun generateDisposableFires() {
        var fired = false
        render(onGenerateDisposable = { fired = true })
        composeTestRule.onNodeWithTag("mail-aliases-generate-disposable-button").performClick()
        assertEquals(true, fired)
    }

    @Test
    fun revokeFiresWithAliasId() {
        var revoked: String? = null
        render(aliases = listOf(alias(id = "deadbeef")), onRevoke = { revoked = it })
        composeTestRule.onNodeWithTag("mail-aliases-list-item-revoke-button").performScrollTo().performClick()
        assertEquals("deadbeef", revoked)
    }

    @Test
    fun canonicalRowIsReadOnly() {
        // One canonical (`<handle>@<domain>`) row + one ordinary row. The canonical
        // row must omit every mutating control (toggle / edit / revoke / overflow
        // delete) and show the "Primary address" marker — mail-aliases.md § Aliases UX.
        render(
            aliases = listOf(
                alias(id = "canon", address = "bob@example.com", isCanonical = true),
                alias(id = "extra", address = "bob-work@example.com"),
            ),
        )
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-aliases-list-item").fetchSemanticsNodes().size)
        // Only the one ordinary row carries the mutating controls.
        assertEquals(1, composeTestRule.onAllNodesWithTag("mail-aliases-list-item-disabled-toggle").fetchSemanticsNodes().size)
        assertEquals(1, composeTestRule.onAllNodesWithTag("mail-aliases-list-item-edit-button").fetchSemanticsNodes().size)
        assertEquals(1, composeTestRule.onAllNodesWithTag("mail-aliases-list-item-revoke-button").fetchSemanticsNodes().size)
        assertEquals(1, composeTestRule.onAllNodesWithTag("mail-aliases-list-item-overflow-menu").fetchSemanticsNodes().size)
        // The canonical row shows the read-only primary-address marker.
        composeTestRule.onNodeWithText("Primary address").assertExists()
    }

    @Test
    fun disabledToggleReEnablesViaOnEnable() {
        // The "Active" toggle is two-way: flipping a disabled alias back ON dispatches
        // Enable (no longer a one-way trap) — mail-aliases.md:156.
        var enabled: String? = null
        render(aliases = listOf(alias(id = "off", disabled = true)), onEnable = { enabled = it })
        composeTestRule.onNodeWithTag("mail-aliases-list-item-disabled-toggle").performScrollTo().performClick()
        assertEquals("off", enabled)
    }

    @Test
    fun emptyListShowsPlaceholder() {
        render(aliases = emptyList())
        assertNull(null)
        assertEquals(0, composeTestRule.onAllNodesWithTag("mail-aliases-list-item").fetchSemanticsNodes().size)
        // hydrated (the default) + genuinely empty -> the resolved-empty claim.
        composeTestRule.onNodeWithText("No aliases yet").assertExists()
    }

    @Test
    fun unhydratedShowsLoadingNotEmptyClaim() {
        // Un-hydrated first paint must not claim "No aliases yet" — the page
        // does not know that yet (`ui/README.md` rule 5).
        render(aliases = emptyList(), hydrated = false)
        composeTestRule.onNodeWithText("Loading your aliases…").assertExists()
        composeTestRule.onNodeWithText("No aliases yet").assertDoesNotExist()
    }

    @Test
    fun importSheetOpensAndSubmits() {
        var imported: List<String>? = null
        render(onImport = { imported = it })
        composeTestRule.onNodeWithTag("mail-aliases-import-button").performClick()
        composeTestRule.onNodeWithTag("mail-aliases-import-textarea").performScrollTo().performTextInput("a@example.com\nb@example.com")
        composeTestRule.onNodeWithTag("mail-aliases-import-submit-button").performScrollTo().performClick()
        assertEquals(listOf("a@example.com", "b@example.com"), imported)
    }

    @Test
    fun importResultRendersCountsAndInvalidReasons() {
        // mail-aliases.md:199-201 requires the per-invalid-line reason be rendered
        // alongside the counts, not just tallied.
        val result = ImportResultView(
            created = 1u,
            skippedDuplicate = 1u,
            invalid = 1u,
            outcomes = listOf(
                ImportAliasOutcomeView(0u, "fresh@example.com", ImportAliasStatusView.CREATED, null),
                ImportAliasOutcomeView(1u, "dup@example.com", ImportAliasStatusView.SKIPPED_DUPLICATE, "already exists"),
                ImportAliasOutcomeView(2u, "not-an-address", ImportAliasStatusView.INVALID, "malformed address"),
            ),
        )
        render(lastImportResult = result)
        composeTestRule.onNodeWithTag("mail-aliases-import-button").performClick()
        // The result stays locally hidden until a submit in *this* open session
        // (mirrors linux's clear-on-reopen — a stale result can't be mistaken for
        // this paste's outcome); an empty submit still flips it visible here since
        // the stateless Content test can't model the VM's async republish.
        composeTestRule.onNodeWithTag("mail-aliases-import-submit-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-aliases-import-result")
            .performScrollTo()
            .assertTextEquals("1 created · 1 already existed · 1 invalid\nnot-an-address — malformed address")
    }
}
