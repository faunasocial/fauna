package com.fauna.app.ui.viewmodel

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.InheritedFilterMarks
import com.fauna.app.core.SecureStorage
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiSpamPreferences
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config
import uniffi.fauna_core.Possibility

/**
 * `save-spam-prefs` is offered only while a threshold differs from what the
 * nest confirmed (`settings.md`, the spam-moderation row; `ui/README.md`
 * § Offer only what is possible): blocked while the read is out, blocked
 * `common.nothing_to_save` while both sliders hold the saved pair, live once one
 * moves, and a blocked press is refused on the page's error line. tui's
 * `save_spam_prefs_is_not_offered_until_a_threshold_changes` twin. Needs the
 * host `libfauna_ffi.so` (the answer is the shared predicate).
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class PrivacySettingsVMSaveSpamTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val context = ApplicationProvider.getApplicationContext<android.app.Application>()

    private fun make(): PrivacySettingsVM {
        val api = mock(ApiClient::class.java)
        val store = InheritedFilterMarks(api, mock(AccountStores::class.java))
        return PrivacySettingsVM(api, mock(SecureStorage::class.java), store, context)
    }

    @Test
    fun saveIsOfferedOnlyWhileAThresholdChanged() {
        val vm = make()
        assertTrue("blocked before the read", vm.saveSpamAnswer.value is Possibility.Blocked)

        vm.applySpamRead(FfiSpamPreferences(700u, 400u))
        val unchanged = vm.saveSpamAnswer.value
        assertTrue(unchanged is Possibility.Blocked)

        // A blocked press is refused out loud, never sent.
        vm.saveSpamPreferences()
        assertEquals(context.getString(R.string.common_nothing_to_save), vm.spamPrefsError.value)

        vm.setSpamThreshold(0.8f)
        assertEquals(Possibility.Possible, vm.saveSpamAnswer.value)

        vm.setSpamThreshold(0.7f)
        assertTrue(vm.saveSpamAnswer.value is Possibility.Blocked)
    }

    @Test
    fun aFailedReadSaysSoAndOffersNoSave() {
        val vm = make()
        vm.applySpamRead(null)
        assertTrue(vm.saveSpamAnswer.value is Possibility.Blocked)
    }
}
