package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiProfileDisplay
import com.fauna.ffi.FfiProfileEditBase
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails

/**
 * `profile-edit-audience-toggle`'s staging (`profile.md` § Where logic lives →
 * *The remote read*): untouched leaves the stored audience alone (`null` reaches
 * `build_edited_profile_with_images`), a press stages the new value, and a press
 * back onto the base is untouched again — tui's `audience_base` / `audience_edit`.
 */
@ExperimentalCoroutinesApi
class ProfileEditVMAudienceTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun api(): ApiClient = mock(ApiClient::class.java) { inv ->
        if (inv.method.name == "buildEditedProfileWithImages") ByteArray(0) else null
    }

    /** The `audienceEveryone` argument of the one save the VM issued. */
    private fun savedAudience(api: ApiClient): Boolean? =
        mockingDetails(api).invocations
            .single { it.method.name == "buildEditedProfileWithImages" }
            .arguments[7] as Boolean?

    private fun saveAfter(presses: Int): Pair<ProfileEditVM, Boolean?> {
        val api = api()
        val vm = ProfileEditVM(api)
        vm.open("11".repeat(32))
        repeat(presses) { vm.toggleAudience() }
        vm.save("name", "bio", emptyList()) {}
        return vm to savedAudience(api)
    }

    @Test
    fun anUntouchedToggleSavesNothingAboutTheAudience() {
        val (vm, audience) = saveAfter(0)
        assertEquals(null, audience)
        assertFalse(vm.audienceEveryone.value)
    }

    @Test
    fun aPressStagesTheOppositeOfTheBase() {
        val (vm, audience) = saveAfter(1)
        assertEquals(true, audience)
        assertTrue(vm.audienceEveryone.value)
    }

    @Test
    fun aPressBackOntoTheBaseIsUntouchedAgain() {
        val (vm, audience) = saveAfter(2)
        assertEquals(null, audience)
        assertFalse(vm.audienceEveryone.value)
    }

    /**
     * The owner's last identity-signed word (`profile.md` § Where logic lives →
     * *Profile publish/edit* → *Edit surface*): the form projects its base
     * through the own form's resolver, handing it the row served beside the
     * body, so over an external app's edit the toggle opens on the audience
     * the owner last signed; an untouched save passes that row on and stages
     * nothing, so the shared builder signs it again.
     */
    @Test
    fun theOwnersOwnWordOpensTheToggleAndRidesTheSave() {
        val body = byteArrayOf(1)
        val direct = byteArrayOf(2)
        val api = mock(ApiClient::class.java) { inv ->
            when (inv.method.name) {
                "loadProfileEditBase" -> FfiProfileEditBase(body = body, lastDirectBody = direct)
                "decodeOwnProfileDisplay" ->
                    if (inv.arguments[0] === body && inv.arguments[1] === direct) {
                        FfiProfileDisplay(
                            displayName = "Ada",
                            bio = "edited elsewhere",
                            links = emptyList(),
                            avatarHashHex = null,
                            bannerHashHex = null,
                            audienceEveryone = true,
                        )
                    } else {
                        null
                    }
                "buildEditedProfileWithImages" -> ByteArray(0)
                else -> null
            }
        }
        val vm = ProfileEditVM(api)
        vm.open("11".repeat(32))
        assertTrue("the toggle opens on what the save will sign", vm.audienceEveryone.value)

        vm.save("Ada", "edited elsewhere", emptyList()) {}
        val save = mockingDetails(api).invocations
            .single { it.method.name == "buildEditedProfileWithImages" }
        assertTrue("the base rides the save", save.arguments[0] === body)
        assertTrue("the row rides the save", save.arguments[1] === direct)
        assertEquals("an untouched toggle stages nothing", null, save.arguments[7] as Boolean?)
    }
}
