package com.fauna.app.ui.screen.settings

import android.content.Context
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.HexUtil
import com.fauna.app.ui.currentClipboardText
import com.fauna.app.ui.util.getStringFmt
import com.fauna.ffi.FfiAdminInviteCode
import com.fauna.ffi.FfiAdminInviteRequest
import com.fauna.ffi.FfiAdminUser
import androidx.compose.ui.semantics.SemanticsProperties
import com.fauna.ffi.FfiAdminUserRowControls
import com.fauna.ffi.FfiAgeBandOption
import com.fauna.ffi.FfiRegistrationMode
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the stateless [AdminUsersContent] (the admin-users
 * hub, admin.md § Users). Renders with seeded state — no Hilt, no VM, no FFI
 * native calls (the `FfiAdmin*` records are pure data classes) — so the three
 * sections, their canonical ui.yaml test IDs, and the section callbacks are
 * exercised on the JVM. (Android E2E `test_admin_users_hub.py --client android`
 * is the standing gate once the host emulator lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminUsersContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val tiers = listOf("free", "personal", "community")

    private fun user(
        label: String,
        tier: String,
        mailServing: Boolean = true,
        handle: String? = null,
    ) = FfiAdminUser(
        actorId = ByteArray(32) { 1 },
        tier = tier,
        label = label,
        handle = handle,
        suspended = false,
        createdAt = 0,
        inboxBytesUsed = 0,
        storageBytesUsed = 0,
        eviction = null,
        mailServingEnabled = mailServing,
        isAdmin = false,
    )

    private fun code(value: String, ageBand: String? = null) = FfiAdminInviteCode(
        code = value, tier = "free", usesLeft = 3, createdAt = 0, ageBand = ageBand,
    )

    private fun request(
        handle: String,
        ageBand: String? = null,
        ageBandProvenance: String? = null,
    ) = FfiAdminInviteRequest(
        id = 7,
        actorId = ByteArray(32) { 2 },
        handle = handle,
        message = "let me in",
        status = "pending",
        // is_pending is derived from status in shared Rust; these
        // fixtures are pending requests.
        isPending = true,
        createdAt = 0,
        decidedAt = null,
        decidedBy = null,
        denialReason = null,
        ageBand = ageBand,
        ageBandProvenance = ageBandProvenance,
    )

    // FFI-free twins of the shared age-band catalog (`fauna_client_admin::
    // age_band_options` / `claimed_age_band_option`, `fauna_protocol::age::
    // {age_claim_label, age_band_label}`) over the real `family.age_band.*` keys,
    // so `localized`/`localizedNested` resolve the same R.strings the real
    // screen does. The rules themselves are unit-tested in shared Rust; these
    // only exercise the screen's render-from-the-shared-answer.
    private val bandKeys = linkedMapOf(
        "not-set" to "family.age_band.not_set",
        "u13" to "family.age_band.u13",
        "13-15" to "family.age_band.teen_13_15",
        "16-17" to "family.age_band.teen_16_17",
        "18+" to "family.age_band.adult",
    )
    private val ageBandOptionsStub: () -> List<FfiAgeBandOption> = {
        bandKeys.map { (value, key) -> FfiAgeBandOption(value, LocalizedText(key, emptyMap())) }
    }
    private val claimedAgeBandOptionStub: (String?) -> String = { claimed ->
        claimed?.takeIf { it != "not-set" && it in bandKeys } ?: "not-set"
    }
    private val ageClaimLabelStub: (String?, String?) -> LocalizedText = { band, provenance ->
        val bandKey = band?.let { bandKeys[it] }
        if (bandKey == null) {
            LocalizedText("family.age_band.claim_none", emptyMap())
        } else {
            LocalizedText(
                "family.age_band.claim_line",
                mapOf("band" to bandKey, "provenance" to "family.age_band.provenance_${provenance?.replace('-', '_')}"),
            )
        }
    }
    private val ageBandLabelStub: (String) -> LocalizedText? = { band ->
        bandKeys[band]?.takeIf { band != "not-set" }?.let { LocalizedText(it, emptyMap()) }
    }

    private fun render(
        users: List<FfiAdminUser> = listOf(user("Alice", "free")),
        // Defaults to [users] — before `allUsers` existed the guardian pickers
        // drew from the same list the Users-section table did, so this keeps
        // every pre-existing case exercising the guardian picker unchanged;
        // a case needing the two lists to diverge (e.g. a guardian off the
        // current Users page) passes its own.
        allUsers: List<FfiAdminUser> = users,
        userTotal: Long = users.size.toLong(),
        userOffset: Long = 0L,
        codes: List<FfiAdminInviteCode> = listOf(code("ABC123")),
        requests: List<FfiAdminInviteRequest> = listOf(request("bob")),
        mintedCode: String? = null,
        actionError: String? = null,
        // Section 2 — Registration. Defaults: a KNOWN posture (`closed`, the
        // fresh-nest default per admin.rs) with no ceiling, so the existing
        // (pre-Registration) tests below keep rendering the editable branch
        // without having to pass these explicitly.
        registrationMode: FfiRegistrationMode? = FfiRegistrationMode.CLOSED,
        unknownRegistrationMode: String? = null,
        maxFreeUsers: String = "",
        ageVerificationRequired: Boolean = false,
        onSetUserTier: (FfiAdminUser, String) -> Unit = { _, _ -> },
        onCreateCode: (String, Long, ByteArray?, String?) -> Unit = { _, _, _, _ -> },
        onDeleteCode: (String) -> Unit = {},
        onApprove: (FfiAdminInviteRequest, String, ByteArray?, String?) -> Unit = { _, _, _, _ -> },
        onDeny: (FfiAdminInviteRequest, String?) -> Unit = { _, _ -> },
        onEvictUser: (FfiAdminUser) -> Unit = {},
        onSuspendUser: (FfiAdminUser) -> Unit = {},
        onCancelEviction: (FfiAdminUser) -> Unit = {},
        // The admin-roster grant/revoke instruments (2026-08-16):
        // `admin-users-make-admin-button` / `-remove-admin-button`, gated per row by
        // `rowControls`'s `makeAdmin`/`removeAdmin`.
        onMakeAdmin: (FfiAdminUser) -> Unit = {},
        onRemoveAdmin: (FfiAdminUser) -> Unit = {},
        onNextPage: () -> Unit = {},
        onPrevPage: () -> Unit = {},
        onSaveRegistration: (FfiRegistrationMode, String, Boolean) -> Unit = { _, _, _ -> },
        onAdmitUser: (String, String, String) -> Unit = { _, _, _ -> },
        // FFI-free stub for the shared `admin_user_row_controls` decision — the
        // real screen injects `com.fauna.ffi.adminUserRowControls`, which Robolectric
        // can't load. Default = an active user (evict + suspend, no restore); tests
        // that need another arm override it. The rule itself is unit-tested in
        // `fauna-client-admin`; this only exercises the screen's render-from-controls.
        rowControls: (FfiAdminUser) -> FfiAdminUserRowControls = {
            FfiAdminUserRowControls(`suspend` = true, evict = true, restore = false, makeAdmin = true, removeAdmin = false)
        },
        // FFI-free stub matching `fauna_client_admin::admin_picker_option`'s
        // handle-else-full-hex fallback — keeps the harness off the native path
        // (the real screen injects `com.fauna.ffi.adminPickerOption`).
        guardianOption: (FfiAdminUser) -> String = {
            it.handle?.takeIf { h -> h.isNotEmpty() } ?: HexUtil.bytesToHex(it.actorId)
        },
    ) {
        composeTestRule.setContent {
            AdminUsersContent(
                pendingRequests = requests,
                inviteCodes = codes,
                users = users,
                allUsers = allUsers,
                userTotal = userTotal,
                userOffset = userOffset,
                tiers = tiers,
                mintedCode = mintedCode,
                actionError = actionError,
                registrationMode = registrationMode,
                unknownRegistrationMode = unknownRegistrationMode,
                maxFreeUsers = maxFreeUsers,
                ageVerificationRequired = ageVerificationRequired,
                onBack = {},
                onSetUserTier = onSetUserTier,
                onCreateCode = onCreateCode,
                onDeleteCode = onDeleteCode,
                onApprove = onApprove,
                onDeny = onDeny,
                onEvictUser = onEvictUser,
                onSuspendUser = onSuspendUser,
                onCancelEviction = onCancelEviction,
                onMakeAdmin = onMakeAdmin,
                onRemoveAdmin = onRemoveAdmin,
                onNextPage = onNextPage,
                onPrevPage = onPrevPage,
                onSaveRegistration = onSaveRegistration,
                onAdmitUser = onAdmitUser,
                // FFI-free stub matching `hex_full` output — keeps the harness
                // off the native path (the real screen injects `com.fauna.ffi.hexFull`).
                hexFull = { HexUtil.bytesToHex(it) },
                // FFI-free stub returning the same `LocalizedText` key the shared
                // `mail_serving_status_label` yields, so `resolveLocalized` renders
                // the identical R.string (real screen injects `com.fauna.ffi.mailServingStatusLabel`).
                mailServingStatusLabel = {
                    LocalizedText(
                        if (it) "admin.users_page.serving_here" else "admin.users_page.serving_disabled",
                        emptyMap(),
                    )
                },
                rowControls = rowControls,
                guardianOption = guardianOption,
                // FFI-free stubs matching `fauna_core::format::{total,current}_page`'s
                // exact arithmetic (real screen injects `com.fauna.ffi.{totalPages,currentPage}`).
                totalPages = { total, pageSize -> maxOf(1L, (total + pageSize - 1) / pageSize) },
                currentPage = { offset, pageSize -> offset / pageSize + 1 },
                ageBandOptions = ageBandOptionsStub,
                ageBandNotSet = "not-set",
                claimedAgeBandOption = claimedAgeBandOptionStub,
                ageClaimLabel = ageClaimLabelStub,
                ageBandLabel = ageBandLabelStub,
            )
        }
    }

    @Test
    fun rendersFiveSectionsAndHeading() {
        render()
        composeTestRule.onNodeWithTag("admin-users-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-users-requests-section").assertExists()
        composeTestRule.onNodeWithTag("admin-users-registration-section").assertExists()
        composeTestRule.onNodeWithTag("admin-users-admit-section").assertExists()
        composeTestRule.onNodeWithTag("admin-users-invite-section").assertExists()
        composeTestRule.onNodeWithTag("admin-users-list-section").assertExists()
    }

    // ── Section: Admit (direct admission, public-mode.md § Registration &
    // Identity) — the third account-creation path, `admin-users-admit-*`.

    @Test
    fun admitSectionRendersActorHandleTierAndButton() {
        render()
        composeTestRule.onNodeWithTag("admin-users-admit-section").assertExists()
        composeTestRule.onNodeWithTag("admin-users-admit-actor-input").assertExists()
        composeTestRule.onNodeWithTag("admin-users-admit-handle-input").assertExists()
        composeTestRule.onNodeWithTag("admin-users-admit-tier-select").assertExists()
        composeTestRule.onNodeWithTag("admin-users-admit-button").assertExists()
    }

    @Test
    fun admitButtonFiresWithTypedActorHandleAndTier() {
        var admitted: Triple<String, String, String>? = null
        val actorHex = "a".repeat(64)
        render(onAdmitUser = { actor, handle, tier -> admitted = Triple(actor, handle, tier) })
        composeTestRule.onNodeWithTag("admin-users-admit-actor-input")
            .performScrollTo().performTextInput(actorHex)
        composeTestRule.onNodeWithTag("admin-users-admit-handle-input")
            .performScrollTo().performTextInput("newperson")
        composeTestRule.onNodeWithTag("admin-users-admit-button")
            .performScrollTo().performClick()
        // Default tier is the first option — the screen passes through
        // whatever was typed; format validation is a VM concern.
        assertEquals(Triple(actorHex, "newperson", "free"), admitted)
    }

    @Test
    fun admitFormStaysPopulatedAfterSubmit() {
        // tui/linux's own idiom: nothing here clears the drafts on submit —
        // success feedback is the new row in the Users-section refetch.
        val actorHex = "b".repeat(64)
        render(onAdmitUser = { _, _, _ -> })
        composeTestRule.onNodeWithTag("admin-users-admit-actor-input")
            .performScrollTo().performTextInput(actorHex)
        composeTestRule.onNodeWithTag("admin-users-admit-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-users-admit-actor-input").assertTextContains(actorHex)
    }

    // ── Section 2 — Registration (admin.md § 2 Users → Section 2) ────────────
    // public-mode.md § Registration Modes: `registrationMode == null` must
    // render READ-ONLY (no picker, no Save) — never coerce an unrecognized
    // posture to a guessed variant and offer to overwrite the nest's real one.

    @Test
    fun registrationSectionRendersReadOnlyMessageWhenModeUnknown() {
        val ctx = ApplicationProvider.getApplicationContext<Context>()
        render(registrationMode = null, unknownRegistrationMode = "some_future_mode")
        composeTestRule.onNodeWithTag("admin-users-registration-section").assertExists()
        composeTestRule.onNodeWithTag("admin-users-registration-mode-select").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-users-max-free-users-input").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-users-registration-save-button").assertDoesNotExist()
        composeTestRule.onNodeWithText(
            ctx.getString(R.string.admin_users_page_registration_mode_unknown)
                .replace("{mode}", "some_future_mode")
        ).assertExists()
    }

    // ── Roster controls (admin.md § Admin continuity and succession, instrument 1)
    // — `admin-users-make-admin-button` / `-remove-admin-button`. Added 2026-08-16:
    // the android leg shipped both buttons with NO coverage at all, because its
    // proof was `:app:compileDebugKotlin`, which builds `src/main` only and never
    // compiled this file against the grown signature.

    @Test
    fun makeAdminButtonRendersForAPromotableUserAndFires() {
        var promoted: FfiAdminUser? = null
        render(
            users = listOf(user("Alice", "free")),
            onMakeAdmin = { promoted = it },
        )
        // The default `rowControls` fixture is makeAdmin=true, removeAdmin=false — assert
        // the remove button is absent too, or this case can't tell a real gate from a
        // render that always shows Make Admin (both look identical from this fixture alone).
        composeTestRule.onNodeWithTag("admin-users-remove-admin-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-users-make-admin-button")
            .performScrollTo().performClick()
        assertEquals("Alice", promoted?.label)
    }

    @Test
    fun removeAdminButtonRendersForAnAdminAndFires() {
        var demoted: FfiAdminUser? = null
        render(
            users = listOf(user("Alice", "free")),
            onRemoveAdmin = { demoted = it },
            // The shared `admin_user_row_controls` decision is what gates each
            // button; an admin row offers remove, not make.
            rowControls = {
                FfiAdminUserRowControls(
                    `suspend` = true, evict = true, restore = false,
                    makeAdmin = false, removeAdmin = true,
                )
            },
        )
        composeTestRule.onNodeWithTag("admin-users-make-admin-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-users-remove-admin-button")
            .performScrollTo().performClick()
        assertEquals("Alice", demoted?.label)
    }

    @Test
    fun rosterControlsAreAbsentWhenTheSharedDecisionOffersNeither() {
        // Not cosmetic: the render must take its answer from `rowControls`, not
        // paint both buttons and hope the handler refuses. A row the shared rule
        // says is neither promotable nor demotable carries no roster control.
        render(
            rowControls = {
                FfiAdminUserRowControls(
                    `suspend` = true, evict = true, restore = false,
                    makeAdmin = false, removeAdmin = false,
                )
            },
        )
        composeTestRule.onNodeWithTag("admin-users-make-admin-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-users-remove-admin-button").assertDoesNotExist()
    }

    @Test
    fun registrationSectionRendersPickerAndSeedsCurrentPostureWhenModeKnown() {
        val ctx = ApplicationProvider.getApplicationContext<Context>()
        render(registrationMode = FfiRegistrationMode.OPEN, maxFreeUsers = "5")
        composeTestRule.onNodeWithTag("admin-users-registration-mode-select")
            .assertTextContains(ctx.getString(R.string.admin_users_page_registration_mode_open))
        composeTestRule.onNodeWithTag("admin-users-max-free-users-input")
            .assertTextContains("5")
        composeTestRule.onNodeWithTag("admin-users-registration-save-button").assertExists()
    }

    @Test
    fun registrationSavePersistsThePickedModeNotTheSeededOne() {
        // Change the picker away from the seeded posture, type a cap, then
        // save — the callback must carry what the admin PICKED (invite_required
        // + "3"), not the closed/blank seed, proving the picker + field feed
        // Save rather than the section re-submitting its initial state.
        var saved: Pair<FfiRegistrationMode, String>? = null
        render(
            registrationMode = FfiRegistrationMode.CLOSED,
            maxFreeUsers = "",
            onSaveRegistration = { mode, cap, _ -> saved = mode to cap },
        )
        composeTestRule.onNodeWithTag("admin-users-registration-mode-select")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-users-registration-mode-select-option-invite_required")
            .performClick()
        composeTestRule.onNodeWithTag("admin-users-max-free-users-input")
            .performScrollTo().performTextInput("3")
        composeTestRule.onNodeWithTag("admin-users-registration-save-button")
            .performScrollTo().performClick()
        assertEquals(FfiRegistrationMode.INVITE_REQUIRED to "3", saved)
    }

    @Test
    fun maxFreeUsersInputFiltersNonDigitCharacters() {
        render(registrationMode = FfiRegistrationMode.OPEN, maxFreeUsers = "")
        composeTestRule.onNodeWithTag("admin-users-max-free-users-input").performTextInput("4x2")
        composeTestRule.onNodeWithTag("admin-users-max-free-users-input").assertTextContains("42")
    }

    @Test
    fun usersSectionRendersRowsAndCount() {
        render(users = listOf(user("Alice", "free"), user("Bob", "personal")))
        assertEquals(2, composeTestRule.onAllNodesWithTag("user-row").fetchSemanticsNodes().size)
        composeTestRule.onNodeWithTag("user-count-text").assertExists()
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-users-tier-select").fetchSemanticsNodes().size)
    }

    // ── Pagination (admin.md § Users — limit/offset on `fauna.admin.users.list`) ─

    @Test
    fun paginationControlsRender() {
        render()
        composeTestRule.onNodeWithTag("admin-users-pagination").assertExists()
        composeTestRule.onNodeWithTag("admin-users-prev-page").assertExists()
        composeTestRule.onNodeWithTag("admin-users-next-page").assertExists()
    }

    @Test
    fun pageIndicatorShowsCurrentOverTotalPages() {
        val ctx = ApplicationProvider.getApplicationContext<Context>()
        // offset 100, page size 50 (AdminUsersVM.PAGE_SIZE) -> page 3; total 120 items -> 3 pages.
        render(userTotal = 120, userOffset = 100)
        val expected = ctx.getStringFmt(R.string.admin_users_page_page_indicator, "3", "3")
        composeTestRule.onNodeWithText(expected).assertExists()
    }

    @Test
    fun nextPageButtonFiresCallback() {
        var fired = false
        render(userTotal = 120, userOffset = 0, onNextPage = { fired = true })
        composeTestRule.onNodeWithTag("admin-users-next-page").performScrollTo().performClick()
        assertEquals(true, fired)
    }

    @Test
    fun prevPageButtonFiresCallback() {
        var fired = false
        render(userTotal = 120, userOffset = 50, onPrevPage = { fired = true })
        composeTestRule.onNodeWithTag("admin-users-prev-page").performScrollTo().performClick()
        assertEquals(true, fired)
    }

    @Test
    fun prevPageButtonDisabledAtFirstPage() {
        render(userTotal = 120, userOffset = 0)
        composeTestRule.onNodeWithTag("admin-users-prev-page").assertIsNotEnabled()
    }

    @Test
    fun nextPageButtonDisabledAtLastPage() {
        // 120 items, page size 50 -> pages [0,50), [50,100), [100,120) — offset
        // 100 is the last page (offset + PAGE_SIZE = 150 is NOT < 120).
        render(userTotal = 120, userOffset = 100)
        composeTestRule.onNodeWithTag("admin-users-next-page").assertIsNotEnabled()
    }

    @Test
    fun nextAndPrevPageButtonsEnabledMidRange() {
        render(userTotal = 120, userOffset = 50)
        composeTestRule.onNodeWithTag("admin-users-prev-page").assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-users-next-page").assertIsEnabled()
    }

    @Test
    fun userRowRendersReadOnlyServingStatus() {
        val ctx = ApplicationProvider.getApplicationContext<Context>()
        // Default-on user shows "Serving here"; a disabled user shows "Not
        // serving". The indicator is read-only (admin.md § Users — no control).
        render(users = listOf(user("Alice", "free", mailServing = true), user("Bob", "free", mailServing = false)))
        val statuses = composeTestRule.onAllNodesWithTag("admin-users-mail-serving-status")
        assertEquals(2, statuses.fetchSemanticsNodes().size)
        statuses[0].assertTextEquals(ctx.getString(R.string.admin_users_page_serving_here))
        statuses[1].assertTextEquals(ctx.getString(R.string.admin_users_page_serving_disabled))
    }

    // ── Cut-off ladder (admin.md § 2 Users → Cutting a user off) ─────────────
    // The screen renders whichever of evict / suspend / cancel-eviction the shared
    // `admin_user_row_controls` decision returns; these pin that render-from-
    // controls contract (the decision itself is unit-tested in fauna-client-admin).

    @Test
    fun activeUserRowOffersEvictAndSuspendNotRestore() {
        render(
            users = listOf(user("Alice", "free")),
            rowControls = { FfiAdminUserRowControls(`suspend` = true, evict = true, restore = false, makeAdmin = true, removeAdmin = false) },
        )
        composeTestRule.onNodeWithTag("admin-users-evict-button").assertExists()
        composeTestRule.onNodeWithTag("admin-users-suspend-button").assertExists()
        composeTestRule.onNodeWithTag("admin-users-cancel-eviction-button").assertDoesNotExist()
    }

    @Test
    fun suspendedUserRowOffersRestoreOnly() {
        render(
            users = listOf(user("Alice", "free")),
            rowControls = { FfiAdminUserRowControls(`suspend` = false, evict = false, restore = true, makeAdmin = true, removeAdmin = false) },
        )
        composeTestRule.onNodeWithTag("admin-users-cancel-eviction-button").assertExists()
        composeTestRule.onNodeWithTag("admin-users-evict-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-users-suspend-button").assertDoesNotExist()
    }

    @Test
    fun adminUserRowOffersNoCutOffControls() {
        // An admin can be neither suspended nor evicted (fauna.admin.conflict), so
        // the row withholds all three rather than render a button that always errors.
        render(
            users = listOf(user("Root", "free")),
            rowControls = { FfiAdminUserRowControls(`suspend` = false, evict = false, restore = false, makeAdmin = false, removeAdmin = true) },
        )
        composeTestRule.onNodeWithTag("admin-users-evict-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-users-suspend-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-users-cancel-eviction-button").assertDoesNotExist()
    }

    @Test
    fun suspendButtonFiresCallbackWithUser() {
        var suspended: FfiAdminUser? = null
        render(
            users = listOf(user("Alice", "free")),
            onSuspendUser = { suspended = it },
        )
        composeTestRule.onNodeWithTag("admin-users-suspend-button").performScrollTo().performClick()
        assertEquals("Alice", suspended?.label)
    }

    @Test
    fun inviteCodeRendersAndDeleteFires() {
        var deleted: String? = null
        render(onDeleteCode = { deleted = it })
        composeTestRule.onNodeWithTag("invite-code-item").assertExists()
        composeTestRule.onNodeWithTag("invite-code-value", useUnmergedTree = true)
            .assertTextEquals("ABC123")
        composeTestRule.onNodeWithTag("admin-settings-invite-delete-button")
            .performScrollTo().performClick()
        assertEquals("ABC123", deleted)
    }

    @Test
    fun mintFlowRevealsFormAndConfirms() {
        var created: Triple<String, Long, ByteArray?>? = null
        render(onCreateCode = { tier, uses, guardian, _ -> created = Triple(tier, uses, guardian) })
        // Form hidden until Create is pressed.
        composeTestRule.onNodeWithTag("admin-settings-invite-create-form").assertDoesNotExist()
        composeTestRule.onNodeWithTag("create-invite-code-btn").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-settings-invite-create-form").assertExists()
        composeTestRule.onNodeWithTag("admin-settings-tier-select").assertExists()
        composeTestRule.onNodeWithTag("admin-settings-max-uses-input").assertExists()
        composeTestRule.onNodeWithTag("admin-users-invite-guardian-select").assertExists()
        composeTestRule.onNodeWithTag("create-invite-confirm-btn").performScrollTo().performClick()
        // Default tier is the first option; default max-uses is 1; default
        // guardian is "none" (family-safety.md § App surface).
        assertEquals(Triple("free", 1L, null), created)
    }

    @Test
    fun mintFlowWithGuardianSelectedFiresGuardianActorId() {
        var created: Triple<String, Long, ByteArray?>? = null
        val guardianActorId = ByteArray(32) { 9 }
        render(
            users = listOf(
                user("Alice", "free"),
                user("Guardy", "personal", handle = "guardy99").copy(actorId = guardianActorId),
            ),
            onCreateCode = { tier, uses, guardian, _ -> created = Triple(tier, uses, guardian) },
        )
        composeTestRule.onNodeWithTag("create-invite-code-btn").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-users-invite-guardian-select").performScrollTo().performClick()
        // The dropdown offers the HANDLE, not the label (admin.md § 2 → *What
        // identifies a user in an admin picker*) — "Guardy" the label still
        // renders in the Users section row below, but "guardy99" only appears
        // in the dropdown's own popup item, so there's no ambiguity to resolve.
        composeTestRule.onNodeWithText("guardy99").performClick()
        composeTestRule.onNodeWithTag("create-invite-confirm-btn").performScrollTo().performClick()
        assertEquals("free", created?.first)
        assertEquals(guardianActorId, created?.third)
    }

    /**
     * The guardian pickers' source is `allUsers` — every account on the nest
     * (`fauna_client_admin::users_list_all`) — kept separate from the
     * paginated `users` page the Users-section table renders (admin.md § 2 →
     * *Which accounts a picker offers*). A guardian off the current page must
     * still be offered: seeds `users` with only Alice (the on-screen page)
     * and `allUsers` with Alice + a guardian who is NOT on that page.
     */
    @Test
    fun guardianPickerOffersAnAccountNotOnTheCurrentUsersPage() {
        var created: Triple<String, Long, ByteArray?>? = null
        val guardianActorId = ByteArray(32) { 9 }
        render(
            users = listOf(user("Alice", "free")),
            allUsers = listOf(
                user("Alice", "free"),
                user("Guardy", "personal", handle = "guardy99").copy(actorId = guardianActorId),
            ),
            onCreateCode = { tier, uses, guardian, _ -> created = Triple(tier, uses, guardian) },
        )
        composeTestRule.onNodeWithTag("create-invite-code-btn").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-users-invite-guardian-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("guardy99").performClick()
        composeTestRule.onNodeWithTag("create-invite-confirm-btn").performScrollTo().performClick()
        assertEquals(guardianActorId, created?.third)
    }

    @Test
    fun pendingRequestRowRendersAndApproveFiresAtSelectedTier() {
        var approved: Triple<Long, String, ByteArray?>? = null
        render(onApprove = { req, tier, guardian, _ -> approved = Triple(req.id, tier, guardian) })
        composeTestRule.onNodeWithTag("invite-request-row-handle", useUnmergedTree = true)
            .assertTextEquals("bob")
        composeTestRule.onNodeWithTag("invite-request-row-tier-select").assertExists()
        composeTestRule.onNodeWithTag("invite-request-row-guardian-select").assertExists()
        composeTestRule.onNodeWithTag("invite-request-row-approve-button").performScrollTo().performClick()
        // Approve admits at the row's selected tier (defaults to the first) with
        // no guardian (defaults to "none").
        assertEquals(Triple(7L, "free", null), approved)
    }

    @Test
    fun copyButtonShownOnlyAfterMint() {
        render(mintedCode = null)
        composeTestRule.onNodeWithTag("admin-users-invite-code-copy-btn").assertDoesNotExist()
    }

    @Test
    fun copyButtonCopiesMintedCode() {
        render(mintedCode = "MINT99")
        composeTestRule.onNodeWithTag("admin-users-invite-code-copy-btn")
            .performScrollTo().performClick()
        // The shared CopyButton writes straight to the clipboard.
        assertEquals("MINT99", currentClipboardText())
    }

    // ── Age-band surfaces (family-safety.md § App surface → *Age-band surfaces*) ─

    private fun hasToken(token: String) =
        SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, token)

    private val guardianActorId = ByteArray(32) { 9 }
    private val guardian = user("Guardy", "personal", handle = "guardy99").copy(actorId = guardianActorId)

    private fun pickGuardian(selectId: String) {
        composeTestRule.onNodeWithTag(selectId).performScrollTo().performClick()
        composeTestRule.onNodeWithText("guardy99").performClick()
    }

    private fun pickBand(selectId: String, label: String) {
        composeTestRule.onNodeWithTag(selectId).performScrollTo().performClick()
        composeTestRule.onNodeWithText(label).performClick()
    }

    @Test
    fun inviteBandSelectIsGatedOnTheGuardianAndTheMintCarriesThePickedBand() {
        var created: Pair<ByteArray?, String?>? = null
        render(
            users = listOf(user("Alice", "free"), guardian),
            onCreateCode = { _, _, g, band -> created = g to band },
        )
        composeTestRule.onNodeWithTag("create-invite-code-btn").performScrollTo().performClick()
        val select = composeTestRule.onNodeWithTag("admin-users-invite-age-band-select")
        select.assert(hasToken("not-set")).assertIsNotEnabled()
        pickGuardian("admin-users-invite-guardian-select")
        select.assertIsEnabled()
        pickBand("admin-users-invite-age-band-select", "Under 13")
        select.assert(hasToken("u13"))
        composeTestRule.onNodeWithTag("create-invite-confirm-btn").performScrollTo().performClick()
        assertEquals(guardianActorId, created?.first)
        assertEquals("u13", created?.second)
    }

    @Test
    fun clearingTheInviteGuardianResetsTheBandToNotSet() {
        var created: Pair<ByteArray?, String?>? = null
        render(
            users = listOf(guardian),
            // No request row, whose own guardian picker would paint a second "None".
            requests = emptyList(),
            onCreateCode = { _, _, g, band -> created = g to band },
        )
        composeTestRule.onNodeWithTag("create-invite-code-btn").performScrollTo().performClick()
        pickGuardian("admin-users-invite-guardian-select")
        pickBand("admin-users-invite-age-band-select", "Under 13")
        // Back to "None": the band must not survive without its guardian.
        composeTestRule.onNodeWithTag("admin-users-invite-guardian-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText(
            ApplicationProvider.getApplicationContext<Context>().getString(R.string.admin_users_page_guardian_none)
        ).performClick()
        composeTestRule.onNodeWithTag("admin-users-invite-age-band-select")
            .assert(hasToken("not-set")).assertIsNotEnabled()
        composeTestRule.onNodeWithTag("create-invite-confirm-btn").performScrollTo().performClick()
        // *Not set* is no band on the wire, never the sentinel token.
        assertEquals(null to null, created?.let { it.first to it.second })
    }

    @Test
    fun inviteCodeRowEchoesTheMintedBand() {
        render(codes = listOf(code("BAND13", ageBand = "u13"), code("PLAIN")))
        val rows = composeTestRule.onAllNodesWithTag("invite-code-item")
        rows[0].assertTextContains("Under 13", substring = true)
        rows[1].assertTextContains("PLAIN", substring = true)
            .assert(!hasText("Under 13", substring = true))
    }

    @Test
    fun requestRowShowsTheClaimAndSeedsTheBandFromIt() {
        var approved: Pair<ByteArray?, String?>? = null
        render(
            users = listOf(guardian),
            requests = listOf(request("kid", ageBand = "13-15", ageBandProvenance = "attested-android")),
            onApprove = { _, _, g, band -> approved = g to band },
        )
        composeTestRule.onNodeWithTag("invite-request-row-age-claim", useUnmergedTree = true)
            .assertTextEquals("Age 13–15 · verified on Android")
        val select = composeTestRule.onNodeWithTag("invite-request-row-age-band-select")
        // Seeded from the claim, but un-pickable until a guardian is chosen.
        select.assert(hasToken("13-15")).assertIsNotEnabled()
        pickGuardian("invite-request-row-guardian-select")
        pickBand("invite-request-row-age-band-select", "16–17")
        composeTestRule.onNodeWithTag("invite-request-row-approve-button").performScrollTo().performClick()
        assertEquals(guardianActorId, approved?.first)
        assertEquals("16-17", approved?.second)
    }

    @Test
    fun requestRowWithoutAClaimSaysSoAndStartsAtNotSet() {
        render(requests = listOf(request("adult")))
        composeTestRule.onNodeWithTag("invite-request-row-age-claim", useUnmergedTree = true)
            .assertTextEquals("No app age verification")
        composeTestRule.onNodeWithTag("invite-request-row-age-band-select").assert(hasToken("not-set"))
    }

    @Test
    fun ageVerificationToggleSeedsFromTheNestAndRidesTheSectionSave() {
        var saved: Boolean? = null
        render(
            ageVerificationRequired = false,
            onSaveRegistration = { _, _, required -> saved = required },
        )
        val toggle = composeTestRule.onNodeWithTag("admin-users-registration-age-verification-toggle")
        toggle.assert(hasToken("off")).assertIsOff()
        toggle.performScrollTo().performClick()
        toggle.assert(hasToken("on"))
        composeTestRule.onNodeWithTag("admin-users-registration-save-button").performScrollTo().performClick()
        assertEquals(true, saved)
    }

    @Test
    fun ageVerificationToggleReadsOnWhenTheNestRequiresIt() {
        render(ageVerificationRequired = true)
        composeTestRule.onNodeWithTag("admin-users-registration-age-verification-toggle")
            .assert(hasToken("on")).assertIsOn()
    }

    @Test
    fun actionErrorSurfacesWhenSet() {
        render(actionError = "boom")
        composeTestRule.onNodeWithTag("admin-users-action-error").assertExists()
    }
}
