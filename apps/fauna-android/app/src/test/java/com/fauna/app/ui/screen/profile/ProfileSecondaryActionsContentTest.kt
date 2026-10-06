package com.fauna.app.ui.screen.profile

import android.content.Context
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.ContactAskRender
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [ProfileSecondaryActionsRow] (the
 * OTHER-profile secondary relationship actions, `profile.md` § Layout & flow): the
 * `profile-start-dm-button` (nav glue) + the `profile-block-button` Block ⇄ Unblock
 * toggle whose label flips on the viewed actor's `contact_status`. Renders with
 * seeded state — no Hilt, no VM, no FFI native calls (mirrors the linux toggle).
 * Also the `profile-request-contact-button` knock and the ward's guardian-ask
 * pair beside it (render half; the rules live in `core/WardAsksTest`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfileSecondaryActionsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun allThreeActionsRender() {
        composeTestRule.setContent {
            ProfileSecondaryActionsRow(
                blocked = false,
                blockWorking = false,
                onStartDm = {},
                onToggleBlock = {},
                blockLabel = { isBlocked -> if (isBlocked) "Unblock" else "Block" },
            )
        }

        composeTestRule.onNodeWithTag("profile-start-dm-button").assertExists()
        composeTestRule.onNodeWithTag("profile-block-button").assertExists()
        // The knock — routed by the shared `knockRecipientNestUrl` (profile.md
        // § Where logic lives → *Request contact routing*).
        composeTestRule.onNodeWithTag("profile-request-contact-button").assertExists()
    }

    @Test
    fun blockLabelReadsBlock_whenNotBlocked() {
        composeTestRule.setContent {
            ProfileSecondaryActionsRow(
                blocked = false,
                blockWorking = false,
                onStartDm = {},
                onToggleBlock = {},
                blockLabel = { isBlocked -> if (isBlocked) "Unblock" else "Block" },
            )
        }
        composeTestRule.onNodeWithTag("profile-block-button").assertTextEquals("Block")
    }

    @Test
    fun blockLabelFlipsToUnblock_whenBlocked() {
        composeTestRule.setContent {
            ProfileSecondaryActionsRow(
                blocked = true,
                blockWorking = false,
                onStartDm = {},
                onToggleBlock = {},
                blockLabel = { isBlocked -> if (isBlocked) "Unblock" else "Block" },
            )
        }
        composeTestRule.onNodeWithTag("profile-block-button").assertTextEquals("Unblock")
    }

    @Test
    fun toggleDisabledWhileWorking() {
        composeTestRule.setContent {
            ProfileSecondaryActionsRow(
                blocked = false,
                blockWorking = true,
                onStartDm = {},
                onToggleBlock = {},
                blockLabel = { isBlocked -> if (isBlocked) "Unblock" else "Block" },
            )
        }
        composeTestRule.onNodeWithTag("profile-block-button").assertIsNotEnabled()
    }

    @Test
    fun tapsFireTheCallbacks() {
        var startDms = 0
        var toggles = 0
        composeTestRule.setContent {
            ProfileSecondaryActionsRow(
                blocked = false,
                blockWorking = false,
                onStartDm = { startDms++ },
                onToggleBlock = { toggles++ },
                blockLabel = { isBlocked -> if (isBlocked) "Unblock" else "Block" },
            )
        }

        composeTestRule.onNodeWithTag("profile-start-dm-button").performClick()
        composeTestRule.onNodeWithTag("profile-block-button").performClick()

        assertEquals(1, startDms)
        assertEquals(1, toggles)
    }

    // ── profile-request-contact-button + the ward's guardian-ask pair ────────
    // (family-safety.md § Child-initiated contact requests → *App affordance*;
    // tui's `profile/mod.rs` arms, render half — the rules themselves are
    // pinned in `core/WardAsksTest`).

    private fun renderKnock(
        sent: Boolean = false,
        inFlight: Boolean = false,
        askRender: ContactAskRender? = null,
        onRequest: () -> Unit = {},
        onAsk: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            ProfileSecondaryActionsRow(
                blocked = false,
                blockWorking = false,
                onStartDm = {},
                onToggleBlock = {},
                blockLabel = { "Block" },
                requestContactSent = sent,
                requestContactInFlight = inFlight,
                onRequestContact = onRequest,
                askRender = askRender,
                onAskGuardian = onAsk,
            )
        }
    }

    private val ctx get() = ApplicationProvider.getApplicationContext<Context>()

    @Test
    fun requestContactKnocksAndNothingElsePaintsBeforeARefusal() {
        var requests = 0
        renderKnock(onRequest = { requests++ })
        composeTestRule.onNodeWithTag("profile-request-contact-button")
            .assertTextEquals(ctx.getString(R.string.profile_request_contact))
            .assertIsEnabled()
            .performClick()
        assertEquals(1, requests)
        composeTestRule.onNodeWithTag("contact-request-guardian-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("contact-request-pending").assertDoesNotExist()
    }

    @Test
    fun aSentKnockReadsRequestSentAndStaysDisabled() {
        renderKnock(sent = true)
        composeTestRule.onNodeWithTag("profile-request-contact-button")
            .assertTextEquals(ctx.getString(R.string.profile_request_contact_sent))
            .assertIsNotEnabled()
    }

    @Test
    fun aRefusalOffersTheAskButton() {
        var asks = 0
        renderKnock(askRender = ContactAskRender.ASK, onAsk = { asks++ })
        composeTestRule.onNodeWithTag("contact-request-guardian-button").performClick()
        assertEquals(1, asks)
        composeTestRule.onNodeWithTag("contact-request-pending").assertDoesNotExist()
        // The knock stays retryable beside it.
        composeTestRule.onNodeWithTag("profile-request-contact-button").assertIsEnabled()
    }

    @Test
    fun aPendingAskShowsThePendingLabelInsteadOfTheButton() {
        renderKnock(askRender = ContactAskRender.PENDING)
        composeTestRule.onNodeWithTag("contact-request-pending")
            .assertTextEquals(ctx.getString(R.string.contacts_contact_request_pending))
        composeTestRule.onNodeWithTag("contact-request-guardian-button").assertDoesNotExist()
    }
}
