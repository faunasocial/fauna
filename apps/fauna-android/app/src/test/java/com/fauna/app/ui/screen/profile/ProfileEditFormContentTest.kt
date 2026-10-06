package com.fauna.app.ui.screen.profile

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.ffi.FfiProfileLink
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [ProfileEditFormContent] (the profile
 * text-only edit form, profile.md § Where logic lives → Profile publish/edit):
 * the form landmark + fields, the repeatable link rows (add / fill / remove), and
 * that Save hands back the edited display_name / bio / links and Cancel fires.
 * Renders with seeded state — no Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfileEditFormContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        initialDisplayName: String = "",
        initialBio: String = "",
        initialLinks: List<FfiProfileLink> = emptyList(),
        saving: Boolean = false,
        avatarUploading: Boolean = false,
        bannerUploading: Boolean = false,
        onSave: (String, String, List<FfiProfileLink>) -> Unit = { _, _, _ -> },
        onCancel: () -> Unit = {},
        onPickAvatar: () -> Unit = {},
        onClearAvatar: () -> Unit = {},
        onPickBanner: () -> Unit = {},
        onClearBanner: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            // Mirror ProfileScreen's verticalScroll so off-screen controls
            // (the Save row below a long link list) are scroll-reachable.
            Column(Modifier.verticalScroll(rememberScrollState())) {
                ProfileEditFormContent(
                    initialDisplayName = initialDisplayName,
                    initialBio = initialBio,
                    initialLinks = initialLinks,
                    saving = saving,
                    avatarUploading = avatarUploading,
                    bannerUploading = bannerUploading,
                    onSave = onSave,
                    onCancel = onCancel,
                    onPickAvatar = onPickAvatar,
                    onClearAvatar = onClearAvatar,
                    onPickBanner = onPickBanner,
                    onClearBanner = onClearBanner,
                )
            }
        }
    }

    @Test
    fun form_rendersFieldsAndButtons() {
        render()
        composeTestRule.onNodeWithTag("profile-edit-form").assertExists()
        composeTestRule.onNodeWithTag("profile-edit-display-name").assertExists()
        composeTestRule.onNodeWithTag("profile-edit-bio").assertExists()
        composeTestRule.onNodeWithTag("profile-edit-link-list").assertExists()
        composeTestRule.onNodeWithTag("profile-edit-link-add-button").assertIsDisplayed()
        composeTestRule.onNodeWithTag("profile-edit-avatar").assertIsDisplayed()
        composeTestRule.onNodeWithTag("profile-edit-avatar-remove-button").assertIsDisplayed()
        composeTestRule.onNodeWithTag("profile-edit-banner").assertIsDisplayed()
        composeTestRule.onNodeWithTag("profile-edit-banner-remove-button").assertIsDisplayed()
        composeTestRule.onNodeWithTag("profile-edit-save-button").assertExists()
        composeTestRule.onNodeWithTag("profile-edit-cancel-button").assertExists()
    }

    @Test
    fun pickAvatarAndBanner_firesTheirCallbacks() {
        var avatarPicked = false
        var bannerPicked = false
        render(onPickAvatar = { avatarPicked = true }, onPickBanner = { bannerPicked = true })
        composeTestRule.onNodeWithTag("profile-edit-avatar").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("profile-edit-banner").performScrollTo().performClick()
        assert(avatarPicked)
        assert(bannerPicked)
    }

    @Test
    fun removeAvatarAndBanner_firesTheirCallbacks() {
        var avatarCleared = false
        var bannerCleared = false
        render(onClearAvatar = { avatarCleared = true }, onClearBanner = { bannerCleared = true })
        composeTestRule.onNodeWithTag("profile-edit-avatar-remove-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("profile-edit-banner-remove-button").performScrollTo().performClick()
        assert(avatarCleared)
        assert(bannerCleared)
    }

    @Test
    fun pickerButtons_disabledWhileUploading() {
        render(avatarUploading = true, bannerUploading = true)
        composeTestRule.onNodeWithTag("profile-edit-avatar").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("profile-edit-banner").assertIsNotEnabled()
        // Remove buttons stay usable — clearing doesn't depend on the upload.
        composeTestRule.onNodeWithTag("profile-edit-avatar-remove-button").assertIsEnabled()
        composeTestRule.onNodeWithTag("profile-edit-banner-remove-button").assertIsEnabled()
    }

    @Test
    fun editAndSave_handsBackDisplayNameAndBio() {
        var savedName: String? = null
        var savedBio: String? = null
        render(onSave = { name, bio, _ -> savedName = name; savedBio = bio })
        composeTestRule.onNodeWithTag("profile-edit-display-name")
            .performScrollTo().performTextInput("Ada Lovelace")
        composeTestRule.onNodeWithTag("profile-edit-bio")
            .performScrollTo().performTextInput("Mathematician")
        composeTestRule.onNodeWithTag("profile-edit-save-button")
            .performScrollTo().performClick()
        assertEquals("Ada Lovelace", savedName)
        assertEquals("Mathematician", savedBio)
    }

    @Test
    fun seededDisplayName_isEditable() {
        var savedName: String? = null
        render(initialDisplayName = "old", onSave = { name, _, _ -> savedName = name })
        composeTestRule.onNodeWithTag("profile-edit-display-name").assertTextContains("old")
        composeTestRule.onNodeWithTag("profile-edit-save-button")
            .performScrollTo().performClick()
        assertEquals("old", savedName)
    }

    @Test
    fun addLink_revealsAnEditableRow() {
        var savedLinks: List<FfiProfileLink> = emptyList()
        render(onSave = { _, _, links -> savedLinks = links })
        composeTestRule.onNodeWithTag("profile-edit-link-add-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("profile-edit-link-label").performScrollTo().performTextInput("blog")
        composeTestRule.onNodeWithTag("profile-edit-link-url").performScrollTo().performTextInput("example.com")
        composeTestRule.onNodeWithTag("profile-edit-save-button").performScrollTo().performClick()
        assertEquals(1, savedLinks.size)
        assertEquals("blog", savedLinks[0].label)
        assertEquals("example.com", savedLinks[0].uri)
    }

    @Test
    fun seededLink_canBeRemoved() {
        var savedLinks: List<FfiProfileLink>? = null
        render(
            initialLinks = listOf(FfiProfileLink(label = "site", uri = "example.org")),
            onSave = { _, _, links -> savedLinks = links },
        )
        composeTestRule.onNodeWithTag("profile-edit-link-remove-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("profile-edit-link-label").assertDoesNotExist()
        composeTestRule.onNodeWithTag("profile-edit-save-button").performScrollTo().performClick()
        assertEquals(emptyList<FfiProfileLink>(), savedLinks)
    }

    @Test
    fun cancel_fires() {
        var cancelled = false
        render(onCancel = { cancelled = true })
        composeTestRule.onNodeWithTag("profile-edit-cancel-button").performScrollTo().performClick()
        assert(cancelled)
    }
}
