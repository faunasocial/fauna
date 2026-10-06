package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for [AccountSettingsTopBar] — `ui/settings.md` §
 * Sub-page heading conformance. `AccountSettingsScreen` itself has no
 * stateless `*Content` twin (unlike [PrivacySettingsContent]/
 * [MutedWordsContent]/[EncryptionSettingsContent]): it is 300+ lines
 * entangled with Hilt, biometric re-auth, file export and account switching,
 * and extracting all of that is out of proportion to a missing heading id —
 * so only the top bar was pulled out, and this is what this test covers.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AccountSettingsTopBarTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun rendersPageHeading() {
        composeTestRule.setContent { AccountSettingsTopBar(onBack = {}) }
        composeTestRule.onNodeWithTag("page-heading").assertExists()
    }
}
