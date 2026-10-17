package com.fauna.app.core

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiAftermathLeg
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import uniffi.fauna_core.LocalizedText
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.verify
import org.robolectric.annotation.Config

/**
 * [SuccessionAftermathHost] — the sink's `config_stage_settled` hook
 * (`succession-aftermath.md` § Propagation: "behind the aftermath's raise"):
 * it re-reads the one inherited-filter-marks cache and tells every live roster
 * subscriber, and a leg's already-resolved line is logged without an app-side
 * re-mapping (`null` is a real answer, not an error).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class SuccessionAftermathHostTest {

    @Test
    fun `the settled hook re-reads the filter marks and tells the roster subscribers`() = runBlocking<Unit> {
        val marks = mock(InheritedFilterMarks::class.java)
        val host = SuccessionAftermathHost(marks, mock(AftermathProgressStore::class.java))
        val heard = launch(kotlinx.coroutines.Dispatchers.Unconfined) { host.configStageSettled.first() }

        host.onConfigStageSettled()

        withTimeout(5_000) { heard.join() }
        verify(marks).reload()
    }

    @Test
    fun `the sink's settled hook reaches the host from the FFI runtime`() = runBlocking<Unit> {
        val marks = mock(InheritedFilterMarks::class.java)
        val host = SuccessionAftermathHost(marks, mock(AftermathProgressStore::class.java))
        val heard = launch(kotlinx.coroutines.Dispatchers.Unconfined) { host.configStageSettled.first() }

        host.sink().configStageSettled()

        withTimeout(5_000) { heard.join() }
        verify(marks).reload()
    }

    @Test
    fun `the sink parks each leg's line and a later null clears it`() {
        val store = AftermathProgressStore(mock(AccountStores::class.java))
        val sink = SuccessionAftermathHost(mock(InheritedFilterMarks::class.java), store).sink()
        val line = LocalizedText("settings.recovery_kit.mail_burn_done", emptyMap())

        sink.progress(FfiAftermathLeg.MAIL_BURN, line)
        assertEquals(line, store.progress.value.mailBurn)
        assertNull(store.progress.value.backupRegrant)

        sink.progress(FfiAftermathLeg.MAIL_BURN, null)
        assertNull(store.progress.value.mailBurn)
    }

    @Test
    fun `an account switch drops the parked lines`() {
        val accountStores = mock(AccountStores::class.java)
        val store = AftermathProgressStore(accountStores)
        // The recorded lambda itself (a captor NPEs on registerCloser's non-null
        // param — see FamilyNotifyStoreTest).
        val registerCall = org.mockito.Mockito.mockingDetails(accountStores).invocations
            .single { it.method.name == "registerCloser" }
        @Suppress("UNCHECKED_CAST")
        val closer = registerCall.arguments[1] as () -> Unit
        store.record(FfiAftermathLeg.GRANT_REMINT, LocalizedText("k", emptyMap()))

        closer.invoke()

        assertNull(store.progress.value.grantRemint)
    }

    @Test
    fun `a leg with nothing to report is not an error`() {
        val sink = SuccessionAftermathHost(mock(InheritedFilterMarks::class.java), mock(AftermathProgressStore::class.java)).sink()
        // `null` is the shared projection's real "this leg owes the user nothing".
        sink.progress(com.fauna.ffi.FfiAftermathLeg.values().first(), null)
    }
}
