package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [EncryptionSettingsContent] (the
 * `Encryption` Settings sub-page — `ui/settings.md` § Sub-page heading
 * conformance). Renders with seeded state — no Hilt, no VM, no FFI native
 * calls (`mlsManager.getKeyPackageCount()`/`ensureKeypackages` live in the VM
 * and are injected here as plain values).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class EncryptionSettingsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        keyPackageCount: Int? = 20,
        isPublishing: Boolean = false,
        errorMessage: String? = null,
        onRefreshKeys: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            EncryptionSettingsContent(
                keyPackageCount = keyPackageCount,
                isPublishing = isPublishing,
                errorMessage = errorMessage,
                onBack = {},
                onRefreshKeys = onRefreshKeys,
            )
        }
    }

    @Test
    fun rendersPageHeading() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
    }

    @Test
    fun rendersKeyPackageCount() {
        render(keyPackageCount = 7)
        composeTestRule.onNodeWithText("7").assertExists()
    }

    @Test
    fun refreshKeysButtonFiresCallback() {
        var fired = false
        render(onRefreshKeys = { fired = true })
        composeTestRule.onNodeWithText("Refresh Keys").performClick()
        assert(fired)
    }

    @Test
    fun errorMessageRendersWhenPresent() {
        render(errorMessage = "boom")
        composeTestRule.onNodeWithText("boom").assertExists()
    }
}
