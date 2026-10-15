package com.fauna.app.ui.viewmodel

import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * The profile SELF header's fallback is the SHARED display handle
 * (`fauna_client_accounts::display_handle`): `<handle>@<current primary>`,
 * never the bare handle — `mail-primary-domain-rename.md` § Display-handle
 * interaction. Before the lift android showed `secureStorage.handle` bare,
 * snapshotted once per VM.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfileSelfDisplayHandleTest {

    @Test
    fun theHeaderShowsTheHandleOnTheCurrentPrimary() {
        assertEquals("bob@fauna.test", selfDisplayHandle("bob", "fauna.test"))
        // A rename moves the registry's domain; the header follows it.
        assertEquals("bob@new.test", selfDisplayHandle("bob", "new.test"))
    }

    @Test
    fun theHeaderIsNeverTheBareHandle() {
        assertNull(selfDisplayHandle("bob", null))
        assertNull(selfDisplayHandle(null, null))
    }
}
